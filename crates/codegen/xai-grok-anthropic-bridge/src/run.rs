//! Process entry: bind listener and run the Anthropic bridge server.

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use xai_grok_sampler::SamplingClient;

use crate::epoch::SessionRegistry;
use crate::live_auth::BridgeAuth;
use crate::serve_config::ServeConfig;
use crate::server::{AppState, router};
use crate::traffic::TrafficBus;
use crate::tui;

/// Handle returned after bind (for tests / launcher health checks).
pub struct ServeHandle {
    pub addr: SocketAddr,
}

/// Bind and serve until SIGINT/SIGTERM, TUI quit, or forever on platforms without signals.
///
/// `auth` must outlive the server (held in `AppState` for 401 recovery).
pub async fn run_serve(
    config: ServeConfig,
    client: SamplingClient,
    auth: BridgeAuth,
) -> anyhow::Result<()> {
    config.validate_bind()?;

    let traffic = TrafficBus::new(512, config.capture_dir.clone());
    let state = AppState {
        config: Arc::new(config.clone()),
        client: Arc::new(client),
        sessions: Arc::new(SessionRegistry::new()),
        traffic: traffic.clone(),
        auth: Arc::new(auth),
    };

    let addr = SocketAddr::new(config.bind, config.port);
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!(%local, "grok anthropic-serve listening");
    eprintln!("grok anthropic-serve listening on http://{local}");
    eprintln!("  POST /v1/messages  GET /healthz");

    let app = router(state);
    let use_tui = !config.no_tui && std::io::stdout().is_terminal();

    if use_tui {
        eprintln!("traffic TUI: q quit · j/k · Tab · w dump (needs --capture-dir)");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let listen = format!("http://{local}");
        let cap = config.capture_dir.clone();
        let tui_bus = traffic.clone();

        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        let tui_task = tokio::task::spawn_blocking(move || {
            // ratatui is sync; run monitor on a blocking pool with its own runtime for notify.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tui runtime");
            rt.block_on(tui::run_monitor(tui_bus, listen, cap, shutdown_tx))
        });

        tokio::select! {
            r = server => {
                r?;
            }
            r = tui_task => {
                r??;
                // TUI quit already sent shutdown via oneshot; server future ends next.
            }
        }
    } else {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

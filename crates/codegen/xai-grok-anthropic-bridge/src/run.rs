//! Process entry: bind listener and run the Anthropic bridge server.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use xai_grok_sampler::SamplingClient;

use crate::epoch::SessionRegistry;
use crate::live_auth::BridgeAuth;
use crate::serve_config::ServeConfig;
use crate::server::{AppState, router};
use crate::traffic::TrafficBus;

/// Handle returned after bind (for tests / launcher health checks).
pub struct ServeHandle {
    pub addr: SocketAddr,
}

/// Bind and serve until SIGINT/SIGTERM (or forever on platforms without signals).
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
        traffic,
        auth: Arc::new(auth),
    };

    let addr = SocketAddr::new(config.bind, config.port);
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!(%local, "grok anthropic-serve listening");
    eprintln!("grok anthropic-serve listening on http://{local}");
    eprintln!("  POST /v1/messages  GET /healthz");

    let app = router(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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

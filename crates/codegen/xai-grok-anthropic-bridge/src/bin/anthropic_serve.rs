//! Standalone binary: `grok-anthropic-serve`
//!
//! Auth precedence: subscription session in `~/.grok/auth.json` (after `grok login`),
//! then `XAI_API_KEY` / `GROK_API_KEY` / `--api-key`.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;
use indexmap::IndexMap;
use xai_grok_anthropic_bridge::{
    AuthSource, ServeConfig, default_auth_json_path, resolve_auth, run_serve,
};
use xai_grok_sampler::{ApiBackend, AuthScheme, SamplerConfig, SamplingClient};

#[derive(Debug, Parser)]
#[command(
    name = "grok-anthropic-serve",
    about = "Anthropic Messages façade for Claude Code → official Grok sampler (subscription session or API key)"
)]
struct Args {
    /// Bind address (loopback by default).
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// Port (`0` = ephemeral).
    #[arg(long, short = 'p', default_value_t = 18765)]
    port: u16,

    /// Default upstream model id.
    #[arg(long, short = 'm', default_value = "grok-4.5")]
    model: String,

    /// Disable future TUI (phase 0 is always plain logs).
    #[arg(long, default_value_t = true)]
    no_tui: bool,

    /// Write dual-side JSON captures under this directory.
    #[arg(long)]
    capture_dir: Option<PathBuf>,

    /// Chunk idle timeout (seconds) for upstream streams.
    #[arg(long, default_value_t = 300)]
    idle_timeout_secs: u64,

    /// Scale reported Anthropic input_tokens (compact steering).
    #[arg(long)]
    usage_scale: Option<f64>,

    /// Allow non-loopback bind (no client auth — dangerous).
    #[arg(long = "i-understand-open-bind")]
    allow_open_bind: bool,

    /// Base URL for the sampling API (default: cli-chat-proxy).
    #[arg(long, default_value = "https://cli-chat-proxy.grok.com/v1")]
    base_url: String,

    /// API key fallback only (env: XAI_API_KEY / GROK_API_KEY). Session in
    /// `~/.grok/auth.json` is preferred when present and not expired.
    #[arg(long, env = "XAI_API_KEY")]
    api_key: Option<String>,

    /// Override path to auth.json (default: $GROK_HOME/auth.json or ~/.grok/auth.json).
    #[arg(long)]
    auth_json: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let auth_path = args
        .auth_json
        .clone()
        .unwrap_or_else(default_auth_json_path);

    // Prefer session from auth.json; only then API key env/flag.
    // Unset API key env is still available as fallback inside resolve_auth.
    let api_override = args
        .api_key
        .as_deref()
        .or_else(|| {
            // clap already maps XAI_API_KEY into api_key when set; also allow GROK_API_KEY
            // only when --api-key / XAI_API_KEY absent — handled inside resolve_auth when
            // we pass None for override after stripping empty.
            None
        })
        .filter(|s| !s.is_empty());

    // If clap filled api_key from XAI_API_KEY, still prefer session: pass it as override
    // only after session miss. resolve_auth does session first.
    let resolved = resolve_auth(
        &auth_path,
        args.api_key.as_deref().filter(|s| !s.is_empty()),
        std::time::SystemTime::now(),
    );

    let Some(resolved) = resolved else {
        anyhow::bail!(
            "no credentials: run `grok login` (writes {path}) or set XAI_API_KEY",
            path = auth_path.display()
        );
    };

    match resolved.source {
        AuthSource::Session => {
            eprintln!(
                "auth: subscription session from {path} (scope={scope})",
                path = auth_path.display(),
                scope = resolved.scope.as_deref().unwrap_or("?")
            );
        }
        AuthSource::ApiKey => {
            eprintln!("auth: API key (no live session in {})", auth_path.display());
        }
    }

    let mut extra_headers = IndexMap::new();
    if args.base_url.contains("cli-chat-proxy") {
        // Match shell inject_url_derived_headers for cli-chat-proxy.
        extra_headers.insert("X-XAI-Token-Auth".into(), "xai-grok-cli".into());
        extra_headers.insert(
            "x-authenticateresponse".into(),
            "authenticate-response".into(),
        );
        extra_headers.insert("x-grok-client-mode".into(), "headless".into());
    }

    let sampler_config = SamplerConfig {
        api_key: Some(resolved.bearer),
        base_url: args.base_url,
        model: args.model.clone(),
        api_backend: ApiBackend::Responses,
        auth_scheme: AuthScheme::Bearer,
        extra_headers,
        context_window: 272_000,
        idle_timeout_secs: Some(args.idle_timeout_secs),
        client_identifier: Some("anthropic-bridge".into()),
        // cli-chat-proxy gates on x-grok-client-version; the bridge crate's
        // own package version is independent of the shipped `grok` CLI.
        // Prefer GROK_CLIENT_VERSION / GROK_VERSION, else a current floor.
        client_version: Some(
            std::env::var("GROK_CLIENT_VERSION")
                .or_else(|_| std::env::var("GROK_VERSION"))
                .unwrap_or_else(|_| "0.2.106".to_string()),
        ),
        supports_backend_search: true,
        ..Default::default()
    };

    let client = SamplingClient::new(sampler_config)
        .map_err(|e| anyhow::anyhow!("failed to build SamplingClient: {e}"))?;

    let serve = ServeConfig {
        bind: args.bind,
        port: args.port,
        default_model: args.model,
        allow_models: Vec::new(),
        no_tui: args.no_tui,
        capture_dir: args.capture_dir,
        idle_timeout_secs: args.idle_timeout_secs,
        usage_scale: args.usage_scale,
        client_identifier: "anthropic-bridge".into(),
        allow_open_bind: args.allow_open_bind,
    };

    let _ = api_override; // reserved for future CLI-only override that skips session
    run_serve(serve, client).await
}

//! Standalone binary: `grok-anthropic-serve`
//!
//! Subcommands:
//! - `serve` (default) — run the Anthropic façade
//! - `claude` — spawn serve on an ephemeral port, run `claude` with env, tear down
//!
//! Auth precedence: subscription session in `~/.grok/auth.json` (after `grok login`),
//! then `XAI_API_KEY` / `GROK_API_KEY` / `--api-key`.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::{Parser, Subcommand};
use indexmap::IndexMap;
use xai_grok_anthropic_bridge::{
    AuthSource, ServeConfig, claude_bridge_env, default_auth_json_path, free_loopback_port,
    loopback_base_url, resolve_auth, run_serve, wait_for_healthz,
};
use xai_grok_sampler::{ApiBackend, AuthScheme, SamplerConfig, SamplingClient};

#[derive(Debug, Parser)]
#[command(
    name = "grok-anthropic-serve",
    about = "Anthropic Messages façade for Claude Code → official Grok sampler",
    subcommand_required = false
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// When no subcommand is given, treat remaining flags as `serve`.
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the loopback Anthropic Messages server.
    Serve(ServeArgs),
    /// Spawn serve as a sidecar and run Claude Code against it.
    Claude(ClaudeArgs),
}

#[derive(Debug, Clone, Parser)]
struct ServeArgs {
    /// Bind address (loopback by default).
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,

    /// Port (`0` = ephemeral).
    #[arg(long, short = 'p', default_value_t = 18765)]
    port: u16,

    /// Default upstream model id.
    #[arg(long, short = 'm', default_value = "grok-4.5")]
    model: String,

    /// Disable future TUI (currently always plain logs).
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

    /// API key fallback (env: XAI_API_KEY). Session in auth.json is preferred.
    #[arg(long, env = "XAI_API_KEY")]
    api_key: Option<String>,

    /// Override path to auth.json.
    #[arg(long)]
    auth_json: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
struct ClaudeArgs {
    /// Default upstream model id (also ANTHROPIC_MODEL / SMALL_FAST).
    #[arg(long, short = 'm', default_value = "grok-4.5")]
    model: String,

    /// Dual-side capture dir for the sidecar serve.
    #[arg(long)]
    capture_dir: Option<PathBuf>,

    /// Path to `claude` binary (default: look up on PATH).
    #[arg(long, default_value = "claude")]
    claude_bin: PathBuf,

    /// Extra args after `--` are forwarded to Claude.
    #[arg(last = true)]
    claude_args: Vec<String>,

    /// API key fallback (session preferred).
    #[arg(long, env = "XAI_API_KEY")]
    api_key: Option<String>,

    /// Override path to auth.json.
    #[arg(long)]
    auth_json: Option<PathBuf>,

    /// Sampling base URL for the sidecar.
    #[arg(long, default_value = "https://cli-chat-proxy.grok.com/v1")]
    base_url: String,

    /// Health wait timeout seconds.
    #[arg(long, default_value_t = 15)]
    health_timeout_secs: u64,
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

    let cli = Cli::parse();
    match cli.command {
        None => run_serve_cmd(cli.serve).await,
        Some(Command::Serve(args)) => run_serve_cmd(args).await,
        Some(Command::Claude(args)) => run_claude_sidecar(args).await,
    }
}

async fn run_serve_cmd(args: ServeArgs) -> anyhow::Result<()> {
    let client = build_sampling_client(
        args.auth_json.as_ref(),
        args.api_key.as_deref(),
        &args.base_url,
        &args.model,
        args.idle_timeout_secs,
    )?;

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
    run_serve(serve, client).await
}

async fn run_claude_sidecar(args: ClaudeArgs) -> anyhow::Result<()> {
    // Prove credentials exist before spawning children.
    let _ = build_sampling_client(
        args.auth_json.as_ref(),
        args.api_key.as_deref(),
        &args.base_url,
        &args.model,
        300,
    )?;

    let port = free_loopback_port()?;
    let base = loopback_base_url(port);
    eprintln!("sidecar: starting serve on {base}");

    let self_exe = std::env::current_exe()?;
    let mut serve_cmd = tokio::process::Command::new(&self_exe);
    serve_cmd
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .arg("--model")
        .arg(&args.model)
        .arg("--base-url")
        .arg(&args.base_url)
        // Keep sidecar stderr quiet so Claude's TTY is not flooded with SSE logs.
        .env("RUST_LOG", "warn,xai_grok_anthropic_bridge=info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    if let Some(dir) = &args.capture_dir {
        serve_cmd.arg("--capture-dir").arg(dir);
    }
    if let Some(path) = &args.auth_json {
        serve_cmd.arg("--auth-json").arg(path);
    }
    if let Some(key) = &args.api_key {
        serve_cmd.arg("--api-key").arg(key);
    }

    let mut child = serve_cmd.spawn()?;

    let health = wait_for_healthz(&base, Duration::from_secs(args.health_timeout_secs)).await;
    if let Err(e) = health {
        let _ = child.kill().await;
        return Err(e);
    }
    eprintln!("sidecar: healthz ok; launching {}", args.claude_bin.display());

    let env_map = claude_bridge_env(&base, &args.model);
    let mut claude = std::process::Command::new(&args.claude_bin);
    claude.args(&args.claude_args);
    for (k, v) in &env_map {
        claude.env(k, v);
    }
    // Inherit stdio for interactive TUI.
    claude.stdin(Stdio::inherit());
    claude.stdout(Stdio::inherit());
    claude.stderr(Stdio::inherit());

    let status = claude.status();

    // Tear down serve regardless of Claude exit.
    let _ = child.kill().await;
    let _ = child.wait().await;

    let status = status?;
    if !status.success() {
        anyhow::bail!("claude exited with {status}");
    }
    Ok(())
}

fn build_sampling_client(
    auth_json: Option<&PathBuf>,
    api_key: Option<&str>,
    base_url: &str,
    model: &str,
    idle_timeout_secs: u64,
) -> anyhow::Result<SamplingClient> {
    let auth_path = auth_json
        .cloned()
        .unwrap_or_else(default_auth_json_path);

    let resolved = resolve_auth(
        &auth_path,
        api_key.filter(|s| !s.is_empty()),
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
    if base_url.contains("cli-chat-proxy") {
        extra_headers.insert("X-XAI-Token-Auth".into(), "xai-grok-cli".into());
        extra_headers.insert(
            "x-authenticateresponse".into(),
            "authenticate-response".into(),
        );
        extra_headers.insert("x-grok-client-mode".into(), "headless".into());
    }

    let sampler_config = SamplerConfig {
        api_key: Some(resolved.bearer),
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_backend: ApiBackend::Responses,
        auth_scheme: AuthScheme::Bearer,
        extra_headers,
        context_window: 272_000,
        idle_timeout_secs: Some(idle_timeout_secs),
        client_identifier: Some("anthropic-bridge".into()),
        client_version: Some(
            std::env::var("GROK_CLIENT_VERSION")
                .or_else(|_| std::env::var("GROK_VERSION"))
                .unwrap_or_else(|_| "0.2.106".to_string()),
        ),
        supports_backend_search: true,
        ..Default::default()
    };

    SamplingClient::new(sampler_config)
        .map_err(|e| anyhow::anyhow!("failed to build SamplingClient: {e}"))
}

//! Standalone binary: `grok-anthropic-serve`
//!
//! Subcommands:
//! - `serve` (default) — run the Anthropic façade
//! - `claude` — spawn serve (sticky port file supported), run `claude`, tear down serve
//!
//! Sticky port file (optional):
//!   env `GROK_ANTHROPIC_SERVE_PORT_FILE` or `--port-file PATH`
//!   File holds a decimal port; not deleted on exit. Next start reuses it and
//!   replaces any previous `grok-anthropic-serve` listener on that port.
//!
//! Auth: subscription session in `~/.grok/auth.json`, else API key.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::{Parser, Subcommand};
use xai_grok_anthropic_bridge::{
    BridgeAuth, PORT_FILE_ENV, ServeConfig, claude_bridge_env, loopback_base_url,
    port_file_from_env, prepare_sticky_port, run_serve, wait_for_healthz,
};

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

    /// Listen port. With a sticky port file, omit to reuse the stored port
    /// (or pick free and write it). Without a port file, default is 18765.
    /// Use `0` to force an ephemeral port (still written to the sticky file if set).
    #[arg(long, short = 'p')]
    port: Option<u16>,

    /// Sticky port file path (overrides env `GROK_ANTHROPIC_SERVE_PORT_FILE`).
    /// Contents: one decimal port. Not deleted on exit.
    #[arg(long, env = "GROK_ANTHROPIC_SERVE_PORT_FILE")]
    port_file: Option<PathBuf>,

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

    /// Optional fixed port for the sidecar serve (sticky file still updated).
    #[arg(long, short = 'p')]
    port: Option<u16>,

    /// Sticky port file (same as serve; env `GROK_ANTHROPIC_SERVE_PORT_FILE`).
    #[arg(long, env = "GROK_ANTHROPIC_SERVE_PORT_FILE")]
    port_file: Option<PathBuf>,

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

fn resolve_port_file_arg(cli_path: Option<PathBuf>) -> Option<PathBuf> {
    cli_path.or_else(port_file_from_env)
}

async fn run_serve_cmd(args: ServeArgs) -> anyhow::Result<()> {
    let port_file = resolve_port_file_arg(args.port_file.clone());
    let res = prepare_sticky_port(args.port, 18765, port_file.as_deref())
        .map_err(|e| anyhow::anyhow!("port resolve: {e}"))?;
    if let Some(path) = &res.path {
        eprintln!(
            "port-file: {path} → {port} ({env})",
            path = path.display(),
            port = res.port,
            env = PORT_FILE_ENV
        );
    }

    // Hold LiveSessionAuth for process lifetime (proactive OIDC refresh).
    let bridge_auth = BridgeAuth::start(
        args.auth_json.as_deref(),
        args.api_key.as_deref(),
    )
    .await?;
    let client = bridge_auth.sampling_client(
        &args.base_url,
        &args.model,
        args.idle_timeout_secs,
    )?;

    let serve = ServeConfig {
        bind: args.bind,
        port: res.port,
        default_model: args.model,
        allow_models: Vec::new(),
        no_tui: args.no_tui,
        capture_dir: args.capture_dir,
        idle_timeout_secs: args.idle_timeout_secs,
        usage_scale: args.usage_scale,
        client_identifier: "anthropic-bridge".into(),
        allow_open_bind: args.allow_open_bind,
    };
    run_serve(serve, client, bridge_auth).await
}

async fn run_claude_sidecar(args: ClaudeArgs) -> anyhow::Result<()> {
    // Validate auth can start (child serve process will start its own LiveSessionAuth).
    let _probe = BridgeAuth::start(args.auth_json.as_deref(), args.api_key.as_deref()).await?;
    drop(_probe);

    let port_file = resolve_port_file_arg(args.port_file.clone());
    // Sidecar: sticky if port file set; else free port (default_port 0 → free).
    let default = if port_file.is_some() { 18765 } else { 0 };
    let res = prepare_sticky_port(args.port, default, port_file.as_deref())
        .map_err(|e| anyhow::anyhow!("port resolve: {e}"))?;
    let port = res.port;
    let base = loopback_base_url(port);
    if let Some(path) = &res.path {
        eprintln!(
            "port-file: {path} → {port}",
            path = path.display(),
            port = port
        );
    }
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

    if let Some(path) = &port_file {
        serve_cmd.arg("--port-file").arg(path);
    }
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
    claude.stdin(Stdio::inherit());
    claude.stdout(Stdio::inherit());
    claude.stderr(Stdio::inherit());

    let status = claude.status();

    // Tear down serve; keep sticky port file for next time.
    let _ = child.kill().await;
    let _ = child.wait().await;

    let status = status?;
    if !status.success() {
        anyhow::bail!("claude exited with {status}");
    }
    Ok(())
}

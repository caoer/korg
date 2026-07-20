//! Serve-time configuration for the Anthropic bridge.

use std::net::IpAddr;
use std::path::PathBuf;

/// Runtime options for [`crate::run_serve`].
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Bind address (default loopback).
    pub bind: IpAddr,
    /// Port (`0` = ephemeral).
    pub port: u16,
    /// Default model when Claude does not send a mappable model id.
    pub default_model: String,
    /// Optional allowlist; empty means accept any non-empty model string.
    pub allow_models: Vec<String>,
    /// Disable TUI (Phase 3); Phase 0 always plain.
    pub no_tui: bool,
    /// Dual-side JSON capture directory.
    pub capture_dir: Option<PathBuf>,
    /// Chunk idle timeout for upstream streams (seconds).
    pub idle_timeout_secs: u64,
    /// Optional multiplier applied to reported Anthropic `usage.input_tokens`.
    pub usage_scale: Option<f64>,
    /// Value for `x-grok-client-identifier` / client identity.
    pub client_identifier: String,
    /// Allow non-loopback binds (dangerous: no client auth on the façade).
    pub allow_open_bind: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 18765,
            default_model: "grok-4.5".to_string(),
            allow_models: Vec::new(),
            no_tui: false,

            capture_dir: None,
            idle_timeout_secs: 300,
            usage_scale: None,
            client_identifier: "anthropic-bridge".to_string(),
            allow_open_bind: false,
        }
    }
}

impl ServeConfig {
    /// Refuse open binds unless explicitly opted in.
    pub fn validate_bind(&self) -> anyhow::Result<()> {
        if self.bind.is_loopback() || self.allow_open_bind {
            return Ok(());
        }
        anyhow::bail!(
            "refusing non-loopback bind {bind} (no client auth on Anthropic façade); \
             pass --i-understand-open-bind to override",
            bind = self.bind
        );
    }
}

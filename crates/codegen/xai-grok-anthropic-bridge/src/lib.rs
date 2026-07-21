//! Anthropic Messages façade for Claude Code, backed by the official Grok sampler.
//!
//! Phase 0/1 of `grok anthropic-serve`: loopback HTTP server that speaks
//! `POST /v1/messages` (and friends), translates to
//! [`xai_grok_sampler::SamplingClient`] Responses streams, and optionally
//! captures dual-side traffic for the debug TUI / `--capture-dir`.

#![deny(clippy::dbg_macro)]

mod credentials;
mod epoch;
mod launcher;
mod live_auth;
mod port_file;
mod reasoning_signature;
mod run;
mod serve_config;
mod server;
mod sse;
mod traffic;
mod translate;
mod tui;

pub use live_auth::BridgeAuth;

pub use credentials::{
    AuthSource, ResolvedAuth, default_auth_json_path, load_session_from_contents, resolve_auth,
    resolve_auth_default,
};
pub use epoch::{SessionEpoch, SessionRegistry};
pub use launcher::{
    claude_bridge_env, free_loopback_port, loopback_base_url, wait_for_healthz,
};
pub use port_file::{
    DEFAULT_PORT_FILE_NAME, PORT_FILE_ENV, PortResolution, default_port_file_path,
    kill_listeners_on_port, parse_port_file_contents, port_file_from_env, prepare_sticky_port,
    read_port_file, resolve_listen_port, write_port_file,
};
pub use reasoning_signature::{
    PendingReasoning, ReasoningReplay, decode_reasoning_signature, encode_reasoning_signature,
};
pub use run::{ServeHandle, run_serve};
pub use serve_config::ServeConfig;
pub use traffic::{TrafficBus, TrafficFrame, TrafficSide};

/// Library version (crate version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

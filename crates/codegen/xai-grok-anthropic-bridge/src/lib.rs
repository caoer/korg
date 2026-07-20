//! Anthropic Messages façade for Claude Code, backed by the official Grok sampler.
//!
//! Phase 0/1 of `grok anthropic-serve`: loopback HTTP server that speaks
//! `POST /v1/messages` (and friends), translates to
//! [`xai_grok_sampler::SamplingClient`] Responses streams, and optionally
//! captures dual-side traffic for the debug TUI / `--capture-dir`.

#![deny(clippy::dbg_macro)]

mod credentials;
mod epoch;
mod reasoning_signature;
mod run;
mod serve_config;
mod server;
mod sse;
mod traffic;
mod translate;

pub use credentials::{
    AuthSource, ResolvedAuth, default_auth_json_path, load_session_from_contents, resolve_auth,
    resolve_auth_default,
};
pub use epoch::{SessionEpoch, SessionRegistry};
pub use reasoning_signature::{
    PendingReasoning, ReasoningReplay, decode_reasoning_signature, encode_reasoning_signature,
};
pub use run::{ServeHandle, run_serve};
pub use serve_config::ServeConfig;
pub use traffic::{TrafficBus, TrafficFrame, TrafficSide};

/// Library version (crate version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

//! Bridge-facing session auth: activate shell [`LiveSessionAuth`].
//!
//! Prefer subscription OIDC (AuthManager + proactive refresh + bearer_resolver).
//! Fall back to static API key only when no session can be obtained.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use xai_grok_sampler::{ApiBackend, AuthScheme, SamplerConfig, SamplingClient};
use xai_grok_shell::auth::{GrokComConfig, LiveSessionAuth};

use crate::credentials::{AuthSource, resolve_auth};

/// Live auth handle held for the lifetime of serve.
pub enum BridgeAuth {
    /// OIDC session with proactive refresh + sampler bearer_resolver.
    Session(LiveSessionAuth),
    /// Static console/env API key (no refresh).
    ApiKey { key: String },
}

impl BridgeAuth {
    /// Start session auth when possible; else static API key.
    ///
    /// `auth_json` override: if set to `…/auth.json`, uses its parent as grok_home
    /// so AuthManager reads that store. Otherwise default `GROK_HOME` / `~/.grok`.
    pub async fn start(
        auth_json: Option<&Path>,
        api_key_override: Option<&str>,
    ) -> anyhow::Result<Self> {
        let home = grok_home_from_auth_json(auth_json);
        match LiveSessionAuth::start(&home, GrokComConfig::default()).await {
            Ok(live) => {
                let scope = live
                    .current_auth()
                    .map(|a| format!("{:?}", a.auth_mode))
                    .unwrap_or_else(|| "session".into());
                eprintln!(
                    "auth: LiveSessionAuth (OIDC refresh + proactive) home={} mode={scope}",
                    home.display()
                );
                return Ok(Self::Session(live));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    home = %home.display(),
                    "LiveSessionAuth failed; trying static API key fallback"
                );
            }
        }

        // Fall back to one-shot file/env key (no refresh).
        let path = auth_json
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::default_auth_json_path);
        let resolved = resolve_auth(
            &path,
            api_key_override.filter(|s| !s.is_empty()),
            std::time::SystemTime::now(),
        );
        let Some(resolved) = resolved else {
            anyhow::bail!(
                "no credentials: run `grok login` or set XAI_API_KEY (LiveSessionAuth also failed)"
            );
        };
        match resolved.source {
            AuthSource::Session => {
                // Session on disk but LiveSessionAuth failed (e.g. expired RT).
                // Use static key once — may 401 until user re-logs in.
                eprintln!(
                    "auth: static session key from {} (refresh unavailable: re-run grok login if 401s)",
                    path.display()
                );
                Ok(Self::ApiKey {
                    key: resolved.bearer,
                })
            }
            AuthSource::ApiKey => {
                eprintln!("auth: API key (static, no OIDC refresh)");
                Ok(Self::ApiKey {
                    key: resolved.bearer,
                })
            }
        }
    }

    /// Build a SamplingClient with live bearer_resolver when session-backed.
    pub fn sampling_client(
        &self,
        base_url: &str,
        model: &str,
        idle_timeout_secs: u64,
    ) -> anyhow::Result<SamplingClient> {
        let mut extra_headers = IndexMap::new();
        if base_url.contains("cli-chat-proxy") {
            extra_headers.insert("X-XAI-Token-Auth".into(), "xai-grok-cli".into());
            extra_headers.insert(
                "x-authenticateresponse".into(),
                "authenticate-response".into(),
            );
            extra_headers.insert("x-grok-client-mode".into(), "headless".into());
        }

        let (api_key, bearer_resolver) = match self {
            Self::Session(live) => (
                live.current_access_token(),
                Some(live.bearer_resolver()),
            ),
            Self::ApiKey { key } => (Some(key.clone()), None),
        };

        let sampler_config = SamplerConfig {
            api_key,
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
            bearer_resolver,
            ..Default::default()
        };

        SamplingClient::new(sampler_config)
            .map_err(|e| anyhow::anyhow!("failed to build SamplingClient: {e}"))
    }

    /// Optional Arc to LiveSessionAuth for 401 recovery on request path.
    pub fn session(&self) -> Option<&LiveSessionAuth> {
        match self {
            Self::Session(s) => Some(s),
            Self::ApiKey { .. } => None,
        }
    }
}

fn grok_home_from_auth_json(auth_json: Option<&Path>) -> PathBuf {
    if let Some(p) = auth_json {
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                return parent.to_path_buf();
            }
        }
    }
    if let Ok(h) = std::env::var("GROK_HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
}

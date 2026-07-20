//! Long-lived session auth for external consumers (e.g. anthropic-bridge).
//!
//! Activates the same OIDC stack as interactive `grok`:
//! [`AuthManager`] + [`AuthManager::configure_refresher`] +
//! [`AuthManager::start_proactive_refresh`] + a sampler
//! [`xai_grok_sampler::BearerResolver`] that reads the live in-memory token.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::config::GrokComConfig;
use super::error::AuthError;
use super::manager::AuthManager;
use super::model::GrokAuth;

/// Owns an [`AuthManager`] with OIDC refresh wired and a proactive refresh task.
///
/// Drop cancels the proactive loop (via the held [`CancellationToken`]).
pub struct LiveSessionAuth {
    manager: Arc<AuthManager>,
    /// Kept so drop cancels the proactive refresh task.
    cancel: CancellationToken,
}

impl std::fmt::Debug for LiveSessionAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSessionAuth")
            .field("has_token", &self.manager.current_or_expired().is_some())
            .finish()
    }
}

impl LiveSessionAuth {
    /// Create manager under `grok_home`, install OIDC refresher, obtain a valid
    /// token via [`AuthManager::auth`], start proactive refresh.
    ///
    /// `auth_provider_command` mirrors `GrokComConfig.auth_provider_command`
    /// (external binary auth); pass `None` for normal grok.com OIDC.
    pub async fn start(
        grok_home: &Path,
        grok_com_config: GrokComConfig,
    ) -> Result<Self, AuthError> {
        let manager = Arc::new(AuthManager::new(grok_home, grok_com_config.clone()));
        manager.configure_refresher(grok_com_config.auth_provider_command.clone(), None);

        // Warm the session: cache hit, OIDC refresh, or fail with AuthError.
        let _auth = manager.auth().await?;

        let cancel = CancellationToken::new();
        manager.start_proactive_refresh(cancel.clone());

        Ok(Self { manager, cancel })
    }

    /// Default grok home (`~/.grok` / `GROK_HOME`) and default com config.
    pub async fn start_default() -> Result<Self, AuthError> {
        let home = default_grok_home();
        Self::start(&home, GrokComConfig::default()).await
    }

    /// Shared manager (for advanced callers).
    pub fn manager(&self) -> &Arc<AuthManager> {
        &self.manager
    }

    /// Current access token (may be slightly past soft-expiry; proactive
    /// refresh keeps it wire-valid). Prefer this for `SamplerConfig.api_key`
    /// seed + [`Self::bearer_resolver`].
    pub fn current_access_token(&self) -> Option<String> {
        self.manager.current_or_expired().map(|a| a.key)
    }

    /// Snapshot of the live auth entry.
    pub fn current_auth(&self) -> Option<GrokAuth> {
        self.manager.current_or_expired()
    }

    /// Sync [`xai_grok_sampler::BearerResolver`] that always returns the
    /// manager's in-memory token (updated by proactive refresh / recovery).
    pub fn bearer_resolver(&self) -> xai_grok_sampler::SharedBearerResolver {
        Arc::new(AuthManagerBearerResolver(self.manager.clone()))
    }

    /// Force a validity check / refresh now (e.g. before long work).
    pub async fn ensure_fresh(&self) -> Result<GrokAuth, AuthError> {
        self.manager.auth().await
    }

    /// 401 recovery: disk adopt + OIDC refresh (same as shell sampling).
    pub async fn recover_after_unauthorized(&self) -> bool {
        self.manager.recover_after_unauthorized().await
    }

    /// Cancel proactive refresh early (also happens on drop).
    pub fn cancel_proactive(&self) {
        self.cancel.cancel();
    }
}

impl Drop for LiveSessionAuth {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Sampler resolver backed by live [`AuthManager`] state.
#[derive(Clone)]
pub struct AuthManagerBearerResolver(pub Arc<AuthManager>);

impl std::fmt::Debug for AuthManagerBearerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManagerBearerResolver").finish()
    }
}

impl xai_grok_sampler::BearerResolver for AuthManagerBearerResolver {
    fn current_bearer(&self) -> Option<String> {
        self.0.current_or_expired().map(|a| a.key)
    }
}

fn default_grok_home() -> PathBuf {
    xai_grok_shell_base::util::grok_home::grok_home()
}
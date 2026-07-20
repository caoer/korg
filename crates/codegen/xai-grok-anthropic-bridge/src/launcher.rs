//! Sidecar launcher helpers: free port, Claude env, health wait.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::time::Duration;

/// Bind `127.0.0.1:0` to reserve an ephemeral loopback port, then drop the
/// listener so the serve child can re-bind it. Small TOCTOU race is acceptable
/// for a local developer tool.
pub fn free_loopback_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Environment overrides for Claude Code pointing at a bridge serve instance.
pub fn claude_bridge_env(base_url: &str, model: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ANTHROPIC_BASE_URL".into(), base_url.to_string());
    m.insert("ANTHROPIC_AUTH_TOKEN".into(), "unused".into());
    m.insert("ANTHROPIC_MODEL".into(), model.to_string());
    m.insert("ANTHROPIC_SMALL_FAST_MODEL".into(), model.to_string());
    m.insert("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK".into(), "1".into());
    m.insert("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into());
    m
}

/// Poll `GET {base}/healthz` until success or timeout.
pub async fn wait_for_healthz(base: &str, timeout: Duration) -> anyhow::Result<()> {
    let url = format!("{base}/healthz");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_err = String::from("not attempted");
    while tokio::time::Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => last_err = format!("status {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("healthz not ready at {url} within {timeout:?}: {last_err}")
}

/// Build the Anthropic base URL for a loopback serve.
pub fn loopback_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_port_is_nonzero() {
        let p = free_loopback_port().unwrap();
        assert!(p > 0);
    }

    #[test]
    fn claude_env_sets_base_and_model() {
        let e = claude_bridge_env("http://127.0.0.1:9", "grok-4.5");
        assert_eq!(e.get("ANTHROPIC_BASE_URL").unwrap(), "http://127.0.0.1:9");
        assert_eq!(e.get("ANTHROPIC_MODEL").unwrap(), "grok-4.5");
        assert_eq!(e.get("ANTHROPIC_SMALL_FAST_MODEL").unwrap(), "grok-4.5");
        assert_eq!(e.get("ANTHROPIC_AUTH_TOKEN").unwrap(), "unused");
        assert_eq!(
            e.get("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK").unwrap(),
            "1"
        );
    }

    #[test]
    fn loopback_url_format() {
        assert_eq!(loopback_base_url(18765), "http://127.0.0.1:18765");
    }
}

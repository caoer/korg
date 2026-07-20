//! Resolve bearer credentials for the Anthropic bridge.
//!
//! Precedence (matches official Grok CLI for first-party inference):
//! 1. Live subscription session token from `~/.grok/auth.json` (`key` field)
//! 2. Explicit API key from env / CLI (`XAI_API_KEY`, `GROK_API_KEY`)
//!
//! Pure functions take auth-file contents + env so unit tests exercise the
//! real load path without network or a real home directory.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Where the resolved bearer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    /// OIDC / device-code subscription session in auth.json.
    Session,
    /// Console / env API key.
    ApiKey,
}

/// Result of credential resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAuth {
    pub bearer: String,
    pub source: AuthSource,
    /// Optional scope key from auth.json (e.g. `https://auth.x.ai::…`).
    pub scope: Option<String>,
}

/// Default auth.json path: `$GROK_HOME/auth.json` or `~/.grok/auth.json`.
pub fn default_auth_json_path() -> PathBuf {
    if let Ok(home) = std::env::var("GROK_HOME") {
        return PathBuf::from(home).join("auth.json");
    }
    dirs_next_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json")
}

fn dirs_next_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Load auth.json and resolve bearer with the given optional API-key override.
///
/// `api_key_override` is typically CLI `--api-key` or env; session wins when
/// a non-empty live session `key` is present.
pub fn resolve_auth(
    auth_json_path: &Path,
    api_key_override: Option<&str>,
    now: SystemTime,
) -> Option<ResolvedAuth> {
    let session = load_session_from_path(auth_json_path, now);
    if let Some(s) = session {
        return Some(s);
    }
    let api = api_key_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var("XAI_API_KEY")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("GROK_API_KEY")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    api.map(|bearer| ResolvedAuth {
        bearer,
        source: AuthSource::ApiKey,
        scope: None,
    })
}

/// Parse auth.json text and pick the best session entry.
pub fn load_session_from_contents(contents: &str, now: SystemTime) -> Option<ResolvedAuth> {
    let root: Value = serde_json::from_str(contents).ok()?;
    load_session_from_value(&root, now)
}

fn load_session_from_path(path: &Path, now: SystemTime) -> Option<ResolvedAuth> {
    let contents = std::fs::read_to_string(path).ok()?;
    load_session_from_contents(&contents, now)
}

fn load_session_from_value(root: &Value, now: SystemTime) -> Option<ResolvedAuth> {
    let obj = root.as_object()?;
    let mut best: Option<(ResolvedAuth, Option<i64>)> = None;

    for (scope, entry) in obj {
        // Legacy flat shape or nested entry with `key`
        let (key, expires_at) = if let Some(k) = entry.get("key").and_then(Value::as_str) {
            (k, entry.get("expires_at").and_then(Value::as_str))
        } else if entry.is_string() {
            // unexpected; skip
            continue;
        } else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        // Skip pure API-key scopes if marked as such (optional)
        if scope == "xai::api_key" || scope.ends_with("::api_key") {
            continue;
        }
        if let Some(exp) = expires_at {
            if is_expired(exp, now) {
                continue;
            }
        }
        let exp_rank = expires_at.and_then(|e| parse_rfc3339_secs(e));
        let candidate = ResolvedAuth {
            bearer: key.to_string(),
            source: AuthSource::Session,
            scope: Some(scope.clone()),
        };
        match &best {
            None => best = Some((candidate, exp_rank)),
            Some((_, prev_exp)) => {
                // Prefer later expiry (fresher session)
                if exp_rank > *prev_exp {
                    best = Some((candidate, exp_rank));
                }
            }
        }
    }
    best.map(|(a, _)| a)
}

fn is_expired(expires_at: &str, now: SystemTime) -> bool {
    let Some(exp_secs) = parse_rfc3339_secs(expires_at) else {
        return false; // unparseable → treat as still valid
    };
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // small skew: expire 60s early
    exp_secs <= now_secs + 60
}

fn parse_rfc3339_secs(s: &str) -> Option<i64> {
    // Accept "2026-07-20T23:59:14.060059Z" and without fractional
    let s = s.trim();
    if !s.ends_with('Z') && !s.contains('+') {
        return None;
    }
    // Use chrono if available; otherwise minimal parse via time crate-less approach.
    // serde_json path: rely on `jiff` or simple split — use `chrono` from workspace if linked.
    // Bridge doesn't depend on chrono; parse with a tiny hand roll for Zulu timestamps.
    parse_zulu(s)
}

fn parse_zulu(s: &str) -> Option<i64> {
    // YYYY-MM-DDTHH:MM:SS[.frac]Z
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let se: i64 = t.next()?.parse().ok()?;
    // days from civil date to unix — Howard Hinnant algorithm
    let unix = days_from_civil(y, mo, day)? * 86400 + h * 3600 + mi * 60 + se;
    Some(unix)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Resolve using default path + env, for the serve binary.
pub fn resolve_auth_default(api_key_override: Option<&str>) -> Option<ResolvedAuth> {
    resolve_auth(&default_auth_json_path(), api_key_override, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn now_fixed() -> SystemTime {
        // 2026-07-20T18:00:00Z
        UNIX_EPOCH + Duration::from_secs(1_784_570_400)
    }

    #[test]
    fn session_preferred_over_api_key() {
        let json = r#"{
          "https://auth.x.ai::client": {
            "key": "session-token-abc",
            "auth_mode": "oidc",
            "expires_at": "2026-07-20T23:59:14Z"
          }
        }"#;
        let path = std::env::temp_dir().join(format!(
            "ab-auth-test-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        let resolved = resolve_auth(&path, Some("api-key-should-lose"), now_fixed()).unwrap();
        assert_eq!(resolved.source, AuthSource::Session);
        assert_eq!(resolved.bearer, "session-token-abc");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn api_key_when_no_session_file() {
        let path = std::env::temp_dir().join(format!(
            "ab-auth-missing-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        // Clear env interference for this process is hard; pass override only
        // and ensure missing file falls through to override.
        let resolved = resolve_auth(&path, Some("only-api-key"), now_fixed()).unwrap();
        assert_eq!(resolved.source, AuthSource::ApiKey);
        assert_eq!(resolved.bearer, "only-api-key");
    }

    #[test]
    fn expired_session_skipped() {
        let json = r#"{
          "https://auth.x.ai::client": {
            "key": "old-session",
            "expires_at": "2026-07-19T00:00:00Z"
          }
        }"#;
        let auth = load_session_from_contents(json, now_fixed());
        assert!(auth.is_none(), "expired session must not be used");
    }

    #[test]
    fn load_session_from_real_shape() {
        let json = r#"{
          "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
            "key": "eyJhbGciOi.test",
            "auth_mode": "oidc",
            "expires_at": "2026-07-20T23:59:14.060059Z",
            "refresh_token": "refresh-not-used-as-bearer"
          }
        }"#;
        let auth = load_session_from_contents(json, now_fixed()).unwrap();
        assert_eq!(auth.source, AuthSource::Session);
        assert_eq!(auth.bearer, "eyJhbGciOi.test");
        assert!(auth.scope.unwrap().contains("auth.x.ai"));
    }

    #[test]
    fn parse_zulu_roundtrip_known() {
        // 1970-01-01T00:00:00Z
        assert_eq!(parse_zulu("1970-01-01T00:00:00Z"), Some(0));
    }
}

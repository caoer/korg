//! Sticky port file: remember the loopback port across restarts.
//!
//! Env: [`PORT_FILE_ENV`] (`GROK_ANTHROPIC_SERVE_PORT_FILE`) — path to a file
//! whose contents are a single port number (decimal). The file is **not**
//! deleted on exit so the next start reuses the same port.
//!
//! Resolution when a port file path is configured:
//! 1. File exists with a valid port → use it (caller kills any listener first)
//! 2. File missing / empty / invalid → pick a free loopback port and write it
//!
//! Optional explicit `--port` overrides the chosen port and is written to the file.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Env var: path to the sticky port file for `grok-anthropic-serve`.
///
/// Named to match the binary and make clear the value is a **file path**,
/// not the port number itself. Prefer this over CCC-scoped names so other
/// tools can share the same contract.
pub const PORT_FILE_ENV: &str = "GROK_ANTHROPIC_SERVE_PORT_FILE";

/// Default sticky port file: `$GROK_HOME/anthropic-serve.port` or
/// `~/.grok/anthropic-serve.port`.
pub const DEFAULT_PORT_FILE_NAME: &str = "anthropic-serve.port";

/// Outcome of sticky port resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortResolution {
    pub port: u16,
    /// Whether we wrote (or rewrote) the port file this start.
    pub wrote_file: bool,
    /// Path used, if any.
    pub path: Option<PathBuf>,
}

/// Canonical default path (`$GROK_HOME` or `~/.grok` + [`DEFAULT_PORT_FILE_NAME`]).
pub fn default_port_file_path() -> PathBuf {
    let home = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".grok")))
        .unwrap_or_else(|| PathBuf::from(".grok"));
    home.join(DEFAULT_PORT_FILE_NAME)
}

/// Resolve sticky port file path:
/// 1. `GROK_ANTHROPIC_SERVE_PORT_FILE` if set and non-empty
/// 2. else [`default_port_file_path`]
pub fn port_file_from_env() -> PathBuf {
    std::env::var_os(PORT_FILE_ENV)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_port_file_path)
}

/// Parse a port file's contents (trim whitespace; single decimal port).
pub fn parse_port_file_contents(s: &str) -> Option<u16> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Allow trailing junk after first token (e.g. "18766\n")
    let first = t.split_whitespace().next()?;
    let p: u16 = first.parse().ok()?;
    if p == 0 {
        return None;
    }
    Some(p)
}

/// Read port from file if present and valid.
pub fn read_port_file(path: &Path) -> Option<u16> {
    let s = fs::read_to_string(path).ok()?;
    parse_port_file_contents(&s)
}

/// Write port to file (creates parent dirs). Does not delete on failure of serve.
pub fn write_port_file(path: &Path, port: u16) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, format!("{port}\n"))
}

/// Reserve an ephemeral loopback port (bind `:0` then release).
pub fn free_loopback_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Resolve the port to bind.
///
/// * `explicit_port` — from `--port` when the user wants a specific number;
///   `None` means “use sticky file or pick free”.
/// * `default_port` — used only when **no** port file is configured (classic CLI).
/// * `port_file` — sticky file path (`--port-file` or env).
pub fn resolve_listen_port(
    explicit_port: Option<u16>,
    default_port: u16,
    port_file: Option<&Path>,
) -> std::io::Result<PortResolution> {
    let Some(path) = port_file else {
        let port = match explicit_port {
            Some(0) | None if default_port == 0 => free_loopback_port()?,
            Some(0) => free_loopback_port()?,
            Some(p) => p,
            None => default_port,
        };
        return Ok(PortResolution {
            port,
            wrote_file: false,
            path: None,
        });
    };

    // Sticky mode.
    // `--port 0` forces a new free port and overwrites the file.
    // omit `--port` → reuse file if valid, else free + write.
    let port = match explicit_port {
        Some(0) => free_loopback_port()?,
        Some(p) => p,
        None => match read_port_file(path) {
            Some(p) => p,
            None => free_loopback_port()?,
        },
    };

    write_port_file(path, port)?;
    Ok(PortResolution {
        port,
        wrote_file: true,
        path: Some(path.to_path_buf()),
    })
}

/// PIDs listening on TCP `port` (IPv4/IPv6). Best-effort via `lsof` (macOS/Linux).
pub fn pids_listening_on_port(port: u16) -> Vec<u32> {
    let output = Command::new("lsof")
        .args([
            "-nP",
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
            "-t",
        ])
        .output();
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() && out.stdout.is_empty() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Best-effort command line for a PID (empty if unknown).
pub fn process_command(pid: u32) -> String {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Whether this looks like our serve binary (avoid killing random listeners).
pub fn is_our_serve_process(cmdline: &str) -> bool {
    let c = cmdline.to_ascii_lowercase();
    c.contains("grok-anthropic-serve") || c.contains("xai-grok-anthropic-bridge")
}

/// Kill listeners on `port`. If `only_ours`, skip PIDs whose cmdline is not our binary.
/// Returns PIDs we signaled.
pub fn kill_listeners_on_port(port: u16, only_ours: bool) -> Vec<u32> {
    let pids = pids_listening_on_port(port);
    let mut killed = Vec::new();
    for pid in pids {
        if pid == std::process::id() {
            continue;
        }
        let cmd = process_command(pid);
        if only_ours && !is_our_serve_process(&cmd) {
            tracing::warn!(
                port,
                pid,
                cmd = %cmd,
                "port busy: pid is not grok-anthropic-serve; not killing"
            );
            continue;
        }
        tracing::info!(port, pid, cmd = %cmd, "stopping previous listener on sticky port");
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        killed.push(pid);
    }
    if !killed.is_empty() {
        thread::sleep(Duration::from_millis(300));
        for pid in &killed {
            // Still alive?
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if alive {
                let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    killed
}

/// Prepare sticky port: resolve, kill previous our-serve on that port, return port.
pub fn prepare_sticky_port(
    explicit_port: Option<u16>,
    default_port: u16,
    port_file: Option<&Path>,
) -> std::io::Result<PortResolution> {
    let res = resolve_listen_port(explicit_port, default_port, port_file)?;
    if port_file.is_some() {
        kill_listeners_on_port(res.port, true);
        // If something else still holds the port, fail clearly at bind time.
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_port_file_basic() {
        assert_eq!(parse_port_file_contents("18766\n"), Some(18766));
        assert_eq!(parse_port_file_contents("  9  "), Some(9));
        assert_eq!(parse_port_file_contents("0"), None);
        assert_eq!(parse_port_file_contents(""), None);
        assert_eq!(parse_port_file_contents("nope"), None);
    }

    #[test]
    fn resolve_without_file_uses_default() {
        let r = resolve_listen_port(None, 18765, None).unwrap();
        assert_eq!(r.port, 18765);
        assert!(!r.wrote_file);
    }

    #[test]
    fn resolve_sticky_creates_file_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("port");
        let r1 = resolve_listen_port(None, 18765, Some(&path)).unwrap();
        assert!(r1.port > 0);
        assert!(r1.wrote_file);
        assert_eq!(read_port_file(&path), Some(r1.port));

        let r2 = resolve_listen_port(None, 9999, Some(&path)).unwrap();
        assert_eq!(r2.port, r1.port); // sticky
        assert!(path.exists()); // not deleted
    }

    #[test]
    fn resolve_sticky_explicit_port_overwrites_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/port.txt");
        let _ = resolve_listen_port(None, 0, Some(&path)).unwrap();
        let r = resolve_listen_port(Some(34567), 0, Some(&path)).unwrap();
        assert_eq!(r.port, 34567);
        assert_eq!(read_port_file(&path), Some(34567));
    }

    #[test]
    fn is_our_serve_detects_binary_name() {
        assert!(is_our_serve_process("/path/grok-anthropic-serve serve --port 1"));
        assert!(is_our_serve_process("target/debug/grok-anthropic-serve"));
        assert!(!is_our_serve_process("node server.mjs"));
    }

    #[test]
    fn write_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p");
        write_port_file(&path, 4242).unwrap();
        assert_eq!(read_port_file(&path), Some(4242));
    }

    #[test]
    fn default_port_file_ends_with_anthropic_serve_port() {
        let p = default_port_file_path();
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some(DEFAULT_PORT_FILE_NAME)
        );
    }
}

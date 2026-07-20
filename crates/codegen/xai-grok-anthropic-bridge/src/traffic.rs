//! Dual-side traffic capture for the debug TUI and `--capture-dir`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Which hop a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficSide {
    /// Claude Code → bridge (Anthropic shape).
    Claude,
    /// Bridge → Grok upstream (Responses shape).
    Grok,
}

/// One captured payload slice.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrafficFrame {
    pub request_id: String,
    pub side: TrafficSide,
    /// e.g. `request`, `response_sse`, `headers`, `error`
    pub phase: String,
    pub payload: Value,
    pub unix_ms: u128,
}

/// In-memory ring + optional disk dump.
#[derive(Clone, Default)]
pub struct TrafficBus {
    inner: Arc<Mutex<TrafficBusInner>>,
}

#[derive(Default)]
struct TrafficBusInner {
    frames: VecDeque<TrafficFrame>,
    max_frames: usize,
    capture_dir: Option<PathBuf>,
}

impl TrafficBus {
    pub fn new(max_frames: usize, capture_dir: Option<PathBuf>) -> Self {
        if let Some(dir) = capture_dir.as_ref() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            inner: Arc::new(Mutex::new(TrafficBusInner {
                frames: VecDeque::new(),
                max_frames: max_frames.max(16),
                capture_dir,
            })),
        }
    }

    pub fn push(&self, frame: TrafficFrame) {
        let mut g = self.inner.lock().expect("traffic bus lock");
        if let Some(dir) = g.capture_dir.clone() {
            write_capture_file(&dir, &frame);
        }
        g.frames.push_back(frame);
        while g.frames.len() > g.max_frames {
            g.frames.pop_front();
        }
    }

    pub fn record_json(
        &self,
        request_id: impl Into<String>,
        side: TrafficSide,
        phase: impl Into<String>,
        payload: Value,
    ) {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.push(TrafficFrame {
            request_id: request_id.into(),
            side,
            phase: phase.into(),
            payload,
            unix_ms,
        });
    }

    pub fn recent(&self, limit: usize) -> Vec<TrafficFrame> {
        let g = self.inner.lock().expect("traffic bus lock");
        g.frames.iter().rev().take(limit).cloned().collect()
    }
}

fn write_capture_file(dir: &Path, frame: &TrafficFrame) {
    let name = format!(
        "{}_{:?}_{}_{}.json",
        frame.request_id,
        frame.side,
        frame.phase,
        frame.unix_ms
    );
    let path = dir.join(name.replace(['/', '\\', ' '], "_"));
    if let Ok(body) = serde_json::to_vec_pretty(frame) {
        let _ = std::fs::write(path, body);
    }
}

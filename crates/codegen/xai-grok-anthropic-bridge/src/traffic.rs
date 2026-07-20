//! Dual-side traffic capture for the debug TUI and `--capture-dir`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::Notify;

/// Which hop a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficSide {
    /// Claude Code → bridge (Anthropic shape).
    Claude,
    /// Bridge → Grok upstream (Responses shape).
    Grok,
}

impl TrafficSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude→bridge",
            Self::Grok => "bridge→Grok",
        }
    }
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

/// Per-request rollup for the TUI list.
#[derive(Debug, Clone)]
pub struct RequestSummary {
    pub request_id: String,
    pub started_ms: u128,
    pub last_ms: u128,
    pub frame_count: usize,
    pub last_phase: String,
    pub has_error: bool,
    pub claude_frames: usize,
    pub grok_frames: usize,
}

/// Snapshot for rendering.
#[derive(Debug, Clone, Default)]
pub struct TrafficSnapshot {
    pub requests: Vec<RequestSummary>,
    pub frames: Vec<TrafficFrame>,
    pub total_frames: usize,
}

/// In-memory ring + optional disk dump + notify for TUI refresh.
#[derive(Clone)]
pub struct TrafficBus {
    inner: Arc<Mutex<TrafficBusInner>>,
    notify: Arc<Notify>,
}

impl Default for TrafficBus {
    fn default() -> Self {
        Self::new(512, None)
    }
}

struct TrafficBusInner {
    frames: VecDeque<TrafficFrame>,
    /// request_id → summary (insertion order via request_order)
    summaries: HashMap<String, RequestSummary>,
    request_order: VecDeque<String>,
    max_frames: usize,
    max_requests: usize,
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
                summaries: HashMap::new(),
                request_order: VecDeque::new(),
                max_frames: max_frames.max(16),
                max_requests: 200,
                capture_dir,
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn notify_handle(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    pub fn push(&self, frame: TrafficFrame) {
        {
            let mut g = self.inner.lock().expect("traffic bus lock");
            if let Some(dir) = g.capture_dir.clone() {
                write_capture_file(&dir, &frame);
            }
            update_summary(&mut g, &frame);
            g.frames.push_back(frame);
            while g.frames.len() > g.max_frames {
                g.frames.pop_front();
            }
            while g.request_order.len() > g.max_requests {
                if let Some(old) = g.request_order.pop_front() {
                    g.summaries.remove(&old);
                }
            }
        }
        self.notify.notify_waiters();
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

    /// Full snapshot for TUI (requests newest-last in order list; we reverse for display).
    pub fn snapshot(&self) -> TrafficSnapshot {
        let g = self.inner.lock().expect("traffic bus lock");
        let mut requests: Vec<RequestSummary> = g
            .request_order
            .iter()
            .filter_map(|id| g.summaries.get(id).cloned())
            .collect();
        // Newest first for list.
        requests.reverse();
        TrafficSnapshot {
            requests,
            frames: g.frames.iter().cloned().collect(),
            total_frames: g.frames.len(),
        }
    }

    pub fn frames_for_request(&self, request_id: &str) -> Vec<TrafficFrame> {
        let g = self.inner.lock().expect("traffic bus lock");
        g.frames
            .iter()
            .filter(|f| f.request_id == request_id)
            .cloned()
            .collect()
    }
}

fn update_summary(g: &mut TrafficBusInner, frame: &TrafficFrame) {
    let entry = g.summaries.entry(frame.request_id.clone());
    match entry {
        std::collections::hash_map::Entry::Vacant(v) => {
            g.request_order.push_back(frame.request_id.clone());
            v.insert(RequestSummary {
                request_id: frame.request_id.clone(),
                started_ms: frame.unix_ms,
                last_ms: frame.unix_ms,
                frame_count: 1,
                last_phase: frame.phase.clone(),
                has_error: frame.phase.contains("error")
                    || frame
                        .payload
                        .get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| t == "error"),
                claude_frames: usize::from(frame.side == TrafficSide::Claude),
                grok_frames: usize::from(frame.side == TrafficSide::Grok),
            });
        }
        std::collections::hash_map::Entry::Occupied(mut o) => {
            let s = o.get_mut();
            s.last_ms = frame.unix_ms;
            s.frame_count += 1;
            s.last_phase = frame.phase.clone();
            if frame.phase.contains("error")
                || frame
                    .payload
                    .get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "error")
            {
                s.has_error = true;
            }
            match frame.side {
                TrafficSide::Claude => s.claude_frames += 1,
                TrafficSide::Grok => s.grok_frames += 1,
            }
        }
    }
}

fn write_capture_file(dir: &Path, frame: &TrafficFrame) {
    let name = format!(
        "{}_{:?}_{}_{}.json",
        frame.request_id, frame.side, frame.phase, frame.unix_ms
    );
    let path = dir.join(name.replace(['/', '\\', ' '], "_"));
    if let Ok(body) = serde_json::to_vec_pretty(frame) {
        let _ = std::fs::write(path, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_tracks_requests() {
        let bus = TrafficBus::new(64, None);
        bus.record_json("r1", TrafficSide::Claude, "request", json!({"a": 1}));
        bus.record_json("r1", TrafficSide::Grok, "request_meta", json!({"b": 2}));
        bus.record_json("r2", TrafficSide::Claude, "request", json!({}));
        let snap = bus.snapshot();
        assert_eq!(snap.requests.len(), 2);
        assert_eq!(snap.requests[0].request_id, "r2"); // newest first
        assert_eq!(snap.requests[1].claude_frames, 1);
        assert_eq!(snap.requests[1].grok_frames, 1);
    }
}

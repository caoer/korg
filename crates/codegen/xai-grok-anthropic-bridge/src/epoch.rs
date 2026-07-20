//! SessionEpoch: sticky Grok session/conv ids + tools_hash pinning.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use uuid::Uuid;

/// Sticky state for one Claude Code session.
#[derive(Debug, Clone)]
pub struct SessionEpoch {
    pub claude_session_id: String,
    pub grok_session_id: String,
    pub conv_id: String,
    pub turn: u64,
    pub tools_hash: Option<String>,
    pub epoch: u64,
}

impl SessionEpoch {
    fn new(claude_session_id: String) -> Self {
        Self {
            grok_session_id: claude_session_id.clone(),
            conv_id: Uuid::new_v4().to_string(),
            claude_session_id,
            turn: 0,
            tools_hash: None,
            epoch: 0,
        }
    }
}

/// Process-wide registry (one serve process; sidecar = one session typically).
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<String, SessionEpoch>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve session, pin/roll tools epoch, bump turn. Returns updated epoch snapshot.
    pub fn begin_turn(
        &self,
        claude_session_id: Option<&str>,
        tools: Option<&Value>,
    ) -> SessionEpoch {
        let key = claude_session_id
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let tools_hash = tools.map(canonical_tools_hash);

        let mut g = self.inner.lock().expect("session registry lock");
        let entry = g.entry(key.clone()).or_insert_with(|| SessionEpoch::new(key));

        match (&entry.tools_hash, &tools_hash) {
            (None, Some(h)) => {
                entry.tools_hash = Some(h.clone());
            }
            (Some(prev), Some(h)) if prev != h => {
                // New epoch: new conv, reset turn counter semantics for cache.
                entry.epoch += 1;
                entry.conv_id = Uuid::new_v4().to_string();
                entry.turn = 0;
                entry.tools_hash = Some(h.clone());
                tracing::info!(
                    session = %entry.claude_session_id,
                    epoch = entry.epoch,
                    "tools_hash changed; opened new SessionEpoch"
                );
            }
            _ => {}
        }

        entry.turn += 1;
        entry.clone()
    }
}

/// Canonical hash of the tools array for epoch pinning.
pub fn canonical_tools_hash(tools: &Value) -> String {
    // serde_json Value Display is stable enough for equality of identical trees;
    // sort object keys by re-serializing through a sorted intermediate when array.
    let normalized = normalize_json(tools);
    let bytes = serde_json::to_vec(&normalized).unwrap_or_default();
    // Simple hex of blake-less FNV-ish: use std hash for phase 0 (not crypto).
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn normalize_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(val) = map.get(&k) {
                    out.insert(k, normalize_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tools_hash_stable_under_key_reorder() {
        let a = json!([{"name": "a", "description": "x", "input_schema": {"type": "object"}}]);
        let b = json!([{"description": "x", "name": "a", "input_schema": {"type": "object"}}]);
        assert_eq!(canonical_tools_hash(&a), canonical_tools_hash(&b));
    }

    #[test]
    fn epoch_rolls_on_tools_change() {
        let reg = SessionRegistry::new();
        let t1 = json!([{"name": "a"}]);
        let t2 = json!([{"name": "b"}]);
        let e1 = reg.begin_turn(Some("s1"), Some(&t1));
        let e2 = reg.begin_turn(Some("s1"), Some(&t1));
        let e3 = reg.begin_turn(Some("s1"), Some(&t2));
        assert_eq!(e1.epoch, e2.epoch);
        assert_eq!(e1.conv_id, e2.conv_id);
        assert_ne!(e2.conv_id, e3.conv_id);
        assert_eq!(e3.epoch, e1.epoch + 1);
        assert_eq!(e2.turn, 2);
        assert_eq!(e3.turn, 1);
    }
}

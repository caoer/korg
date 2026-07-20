//! Opaque Anthropic thinking signatures for Grok encrypted reasoning round-trip.
//!
//! Ported idea from claude-code-proxy PR #57 (`ccp:grok:v1:…`).

use base64::Engine;
use serde_json::Value;

const PREFIX: &str = "grok-bridge:v1:";
const MAX_ID_BYTES: usize = 4 * 1024;
const MAX_ENCRYPTED_CONTENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningReplay {
    pub id: String,
    pub encrypted_content: String,
}

#[derive(Debug, Clone, Default)]
pub struct PendingReasoning {
    id: Option<String>,
    encrypted_content: Option<String>,
}

impl PendingReasoning {
    pub fn capture(&mut self, item: &Value) {
        if let Some(id) = non_empty_string(item.get("id")) {
            self.id = Some(id.to_string());
        }
        if let Some(encrypted_content) = non_empty_string(item.get("encrypted_content")) {
            self.encrypted_content = Some(encrypted_content.to_string());
        }
    }

    pub fn capture_fields(&mut self, id: Option<&str>, encrypted_content: Option<&str>) {
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            self.id = Some(id.to_string());
        }
        if let Some(enc) = encrypted_content.filter(|s| !s.is_empty()) {
            self.encrypted_content = Some(enc.to_string());
        }
    }

    pub fn replay(&self) -> Option<ReasoningReplay> {
        Some(ReasoningReplay {
            id: self.id.clone()?,
            encrypted_content: self.encrypted_content.clone()?,
        })
    }
}

pub fn encode_reasoning_signature(replay: &ReasoningReplay) -> Option<String> {
    if replay.id.is_empty()
        || replay.id.len() > MAX_ID_BYTES
        || replay.encrypted_content.is_empty()
        || replay.encrypted_content.len() > MAX_ENCRYPTED_CONTENT_BYTES
    {
        return None;
    }
    let encoded_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(replay.id.as_bytes());
    Some(format!(
        "{PREFIX}{encoded_id}:{}",
        replay.encrypted_content
    ))
}

pub fn decode_reasoning_signature(signature: &str) -> Option<ReasoningReplay> {
    let payload = signature.strip_prefix(PREFIX)?;
    if payload.is_empty() || payload.len() > max_payload_len() {
        return None;
    }
    let (encoded_id, encrypted_content) = payload.split_once(':')?;
    if encoded_id.is_empty()
        || encoded_id.len() > encoded_id_len_limit()
        || encrypted_content.is_empty()
        || encrypted_content.len() > MAX_ENCRYPTED_CONTENT_BYTES
    {
        return None;
    }
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_id)
        .ok()?;
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return None;
    }
    Some(ReasoningReplay {
        id: String::from_utf8(id).ok()?,
        encrypted_content: encrypted_content.to_string(),
    })
}

fn encoded_id_len_limit() -> usize {
    (MAX_ID_BYTES + 2) / 3 * 4
}

fn max_payload_len() -> usize {
    encoded_id_len_limit() + 1 + MAX_ENCRYPTED_CONTENT_BYTES
}

fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    value?.as_str().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_round_trip() {
        let replay = ReasoningReplay {
            id: "rs_abc".into(),
            encrypted_content: "gAAAAopaque".into(),
        };
        let sig = encode_reasoning_signature(&replay).unwrap();
        assert!(sig.starts_with(PREFIX));
        assert_eq!(decode_reasoning_signature(&sig), Some(replay));
    }

    #[test]
    fn foreign_signatures_ignored() {
        assert_eq!(decode_reasoning_signature("ccp:grok:v1:x:y"), None);
        assert_eq!(decode_reasoning_signature("anthropic"), None);
    }
}

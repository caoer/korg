//! Emit Anthropic Messages SSE frames from [`AnthropicOut`](crate::translate::stream::AnthropicOut).
//!
//! Anthropic's Messages streaming API names the SSE `event:` field after the JSON
//! `type` (e.g. `event: message_start`, `event: content_block_delta`). Claude Code
//! filters on that field; mis-tagging everything as `event: message` causes it to
//! drop frames and report "Stream ended without receiving any events".

use crate::translate::AnthropicOut;

/// Format one Anthropic SSE event. `event_name` must match the JSON `type`.
pub fn format_sse_event(event_name: &str, payload: &serde_json::Value) -> String {
    format!("event: {event_name}\ndata: {payload}\n\n")
}

/// Back-compat helper: uses JSON `type` as the SSE event name when present.
pub fn format_sse_data(payload: &serde_json::Value) -> String {
    let event_name = payload
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("message");
    format_sse_event(event_name, payload)
}

pub fn message_start(message_id: &str, model: &str) -> String {
    let payload = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": message_id,
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        }
    });
    format_sse_event("message_start", &payload)
}

#[allow(dead_code)]
pub fn ping() -> String {
    format_sse_event("ping", &serde_json::json!({"type": "ping"}))
}

/// Emit a terminal error event that Claude Code's SDK will throw as APIError.
pub fn error_event(message: impl Into<String>) -> String {
    let payload = serde_json::json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": message.into()
        }
    });
    format_sse_event("error", &payload)
}

pub fn encode_out(ev: &AnthropicOut) -> Vec<String> {
    match ev {
        AnthropicOut::ThinkingStart { index } => {
            vec![format_sse_event(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "thinking", "thinking": "", "signature": "" }
                }),
            )]
        }
        AnthropicOut::ThinkingDelta { index, text } => {
            vec![format_sse_event(
                "content_block_delta",
                &serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "thinking_delta", "thinking": text }
                }),
            )]
        }
        AnthropicOut::ThinkingSignature { index, signature } => {
            vec![format_sse_event(
                "content_block_delta",
                &serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "signature_delta", "signature": signature }
                }),
            )]
        }
        AnthropicOut::ThinkingStop { index } => {
            vec![format_sse_event(
                "content_block_stop",
                &serde_json::json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            )]
        }
        AnthropicOut::TextStart { index } => {
            vec![format_sse_event(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "text", "text": "" }
                }),
            )]
        }
        AnthropicOut::TextDelta { index, text } => {
            vec![format_sse_event(
                "content_block_delta",
                &serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "text_delta", "text": text }
                }),
            )]
        }
        AnthropicOut::TextStop { index } => {
            vec![format_sse_event(
                "content_block_stop",
                &serde_json::json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            )]
        }
        AnthropicOut::ToolUseStart { index, id, name } => {
            vec![format_sse_event(
                "content_block_start",
                &serde_json::json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                }),
            )]
        }
        AnthropicOut::ToolUseDelta {
            index,
            partial_json,
        } => {
            vec![format_sse_event(
                "content_block_delta",
                &serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "input_json_delta", "partial_json": partial_json }
                }),
            )]
        }
        AnthropicOut::ToolUseStop { index } => {
            vec![format_sse_event(
                "content_block_stop",
                &serde_json::json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            )]
        }
        AnthropicOut::MessageDelta {
            stop_reason,
            input_tokens,
            output_tokens,
        } => {
            let mut usage = serde_json::Map::new();
            if let Some(i) = input_tokens {
                usage.insert("input_tokens".into(), serde_json::json!(i));
            }
            if let Some(o) = output_tokens {
                usage.insert("output_tokens".into(), serde_json::json!(o));
            }
            vec![format_sse_event(
                "message_delta",
                &serde_json::json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": stop_reason,
                        "stop_sequence": null
                    },
                    "usage": usage
                }),
            )]
        }
        AnthropicOut::MessageStop => {
            vec![format_sse_event(
                "message_stop",
                &serde_json::json!({
                    "type": "message_stop"
                }),
            )]
        }
        AnthropicOut::Error { message } => {
            vec![error_event(message.clone())]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_start_uses_named_event() {
        let frame = message_start("msg_1", "grok-4.5");
        assert!(
            frame.starts_with("event: message_start\n"),
            "got: {frame:?}"
        );
        assert!(frame.contains("\"type\":\"message_start\"") || frame.contains("\"type\": \"message_start\""));
        assert!(frame.ends_with("\n\n"));
    }

    #[test]
    fn encode_out_names_match_json_type() {
        let cases: Vec<(AnthropicOut, &str)> = vec![
            (AnthropicOut::TextStart { index: 0 }, "content_block_start"),
            (
                AnthropicOut::TextDelta {
                    index: 0,
                    text: "hi".into(),
                },
                "content_block_delta",
            ),
            (AnthropicOut::TextStop { index: 0 }, "content_block_stop"),
            (
                AnthropicOut::MessageDelta {
                    stop_reason: Some("end_turn".into()),
                    input_tokens: Some(1),
                    output_tokens: Some(2),
                },
                "message_delta",
            ),
            (AnthropicOut::MessageStop, "message_stop"),
            (
                AnthropicOut::Error {
                    message: "boom".into(),
                },
                "error",
            ),
        ];
        for (ev, expected_event) in cases {
            let frames = encode_out(&ev);
            assert_eq!(frames.len(), 1, "for {expected_event}");
            assert!(
                frames[0].starts_with(&format!("event: {expected_event}\n")),
                "expected event {expected_event}, got {}",
                frames[0]
            );
        }
    }

    #[test]
    fn format_sse_data_derives_event_from_type() {
        let payload = serde_json::json!({"type": "content_block_stop", "index": 0});
        let frame = format_sse_data(&payload);
        assert!(frame.starts_with("event: content_block_stop\n"));
    }

    #[test]
    fn never_emits_generic_message_event_for_protocol_events() {
        // Regression: old bridge always used `event: message`, which Claude
        // Code often ignores for lifecycle frames (especially errors).
        for ev in [
            AnthropicOut::MessageStop,
            AnthropicOut::Error {
                message: "x".into(),
            },
            AnthropicOut::TextStart { index: 0 },
        ] {
            for frame in encode_out(&ev) {
                assert!(
                    !frame.starts_with("event: message\n"),
                    "must not use generic event name: {frame}"
                );
            }
        }
    }
}

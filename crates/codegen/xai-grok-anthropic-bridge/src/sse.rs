//! Emit Anthropic Messages SSE frames from [`AnthropicOut`](crate::translate::stream::AnthropicOut).

use crate::translate::AnthropicOut;

/// One SSE data line payload (without `event:` / framing).
pub fn format_sse_data(payload: &serde_json::Value) -> String {
    format!("event: message\ndata: {payload}\n\n")
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
    format_sse_data(&payload)
}

#[allow(dead_code)]
pub fn ping() -> String {
    format_sse_data(&serde_json::json!({"type": "ping"}))
}

pub fn encode_out(ev: &AnthropicOut) -> Vec<String> {
    match ev {
        AnthropicOut::ThinkingStart { index } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" }
            }))]
        }
        AnthropicOut::ThinkingDelta { index, text } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "thinking_delta", "thinking": text }
            }))]
        }
        AnthropicOut::ThinkingSignature { index, signature } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "signature_delta", "signature": signature }
            }))]
        }
        AnthropicOut::ThinkingStop { index } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_stop",
                "index": index
            }))]
        }
        AnthropicOut::TextStart { index } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" }
            }))]
        }
        AnthropicOut::TextDelta { index, text } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }))]
        }
        AnthropicOut::TextStop { index } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_stop",
                "index": index
            }))]
        }
        AnthropicOut::ToolUseStart { index, id, name } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": {}
                }
            }))]
        }
        AnthropicOut::ToolUseDelta {
            index,
            partial_json,
        } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": partial_json }
            }))]
        }
        AnthropicOut::ToolUseStop { index } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "content_block_stop",
                "index": index
            }))]
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
            vec![format_sse_data(&serde_json::json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": usage
            }))]
        }
        AnthropicOut::MessageStop => {
            vec![format_sse_data(&serde_json::json!({
                "type": "message_stop"
            }))]
        }
        AnthropicOut::Error { message } => {
            vec![format_sse_data(&serde_json::json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": message
                }
            }))]
        }
    }
}

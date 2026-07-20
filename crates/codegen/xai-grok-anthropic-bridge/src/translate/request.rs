//! Anthropic Messages JSON → [`ConversationRequest`].

use serde_json::Value;
use thiserror::Error;
use xai_grok_sampling_types::conversation::{
    ConversationItem, ConversationRequest, ConversationToolChoice, HostedTool, ToolSpec,
};
use xai_grok_sampling_types::rs;

use crate::epoch::SessionEpoch;
use crate::reasoning_signature::decode_reasoning_signature;

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("{0}")]
    Message(String),
}

/// Translate a raw Anthropic `/v1/messages` body into a Grok conversation request.
pub fn translate_messages_request(
    body: &Value,
    epoch: &SessionEpoch,
    default_model: &str,
    req_id: &str,
) -> Result<ConversationRequest, TranslateError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(strip_context_suffix)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_model.to_string());

    let mut items: Vec<ConversationItem> = Vec::new();

    if let Some(system) = body.get("system") {
        if let Some(text) = system_to_text(system)? {
            if !text.is_empty() {
                items.push(ConversationItem::system(text));
            }
        }
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| TranslateError::Message("messages must be an array".into()))?;

    for msg in messages {
        push_message(msg, &mut items)?;
    }

    let (tools, hosted_tools) = parse_tools(body.get("tools"))?;
    let tool_choice = parse_tool_choice(body.get("tool_choice"));

    let max_output_tokens = body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .map(|n| n as u32);

    let temperature = body
        .get("temperature")
        .and_then(Value::as_f64)
        .map(|t| t as f32);

    let top_p = body.get("top_p").and_then(Value::as_f64).map(|t| t as f32);

    let reasoning_effort = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .and_then(parse_effort);

    Ok(ConversationRequest {
        items,
        tools,
        hosted_tools,
        tool_choice,
        model: Some(model),
        temperature,
        max_output_tokens,
        top_p,
        x_grok_conv_id: Some(epoch.conv_id.clone()),
        x_grok_req_id: Some(req_id.to_string()),
        x_grok_session_id: Some(epoch.grok_session_id.clone()),
        x_grok_turn_idx: Some(epoch.turn.to_string()),
        x_grok_agent_id: Some("anthropic-bridge".into()),
        x_grok_deployment_id: None,
        x_grok_user_id: None,
        trace: None,
        reasoning_effort,
        json_schema: None,
    })
}

fn strip_context_suffix(model: &str) -> String {
    // Claude local compaction hint e.g. grok-4.5[1m]
    model
        .split_once('[')
        .map(|(m, _)| m)
        .unwrap_or(model)
        .to_string()
}

fn system_to_text(system: &Value) -> Result<Option<String>, TranslateError> {
    match system {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Array(blocks) => {
            let mut out = String::new();
            for b in blocks {
                let t = b
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| TranslateError::Message("system block missing text".into()))?;
                out.push_str(t);
            }
            Ok(Some(out))
        }
        Value::Null => Ok(None),
        _ => Err(TranslateError::Message("unsupported system shape".into())),
    }
}

fn push_message(msg: &Value, items: &mut Vec<ConversationItem>) -> Result<(), TranslateError> {
    let role = msg
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| TranslateError::Message("message missing role".into()))?;
    match role {
        // Claude Code often puts system blocks in `messages[]` as well as
        // top-level `system`. Map both to ConversationItem::system.
        "system" => {
            if let Some(text) = content_to_plain_text(msg.get("content"))? {
                if !text.is_empty() {
                    items.push(ConversationItem::system(text));
                }
            }
            Ok(())
        }
        "user" => push_user_content(msg.get("content"), items),
        "assistant" => push_assistant_content(msg.get("content"), items),
        other => Err(TranslateError::Message(format!(
            "unsupported message role: {other}"
        ))),
    }
}

fn content_to_plain_text(content: Option<&Value>) -> Result<Option<String>, TranslateError> {
    let Some(content) = content else {
        return Ok(None);
    };
    match content {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Array(blocks) => {
            let mut out = String::new();
            for b in blocks {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                } else if let Some(t) = b.as_str() {
                    out.push_str(t);
                }
            }
            Ok(Some(out))
        }
        _ => Err(TranslateError::Message(
            "unsupported content shape for system/text".into(),
        )),
    }
}

fn push_user_content(
    content: Option<&Value>,
    items: &mut Vec<ConversationItem>,
) -> Result<(), TranslateError> {
    let Some(content) = content else {
        return Ok(());
    };
    match content {
        Value::String(s) => {
            items.push(ConversationItem::user(s.clone()));
        }
        Value::Array(blocks) => {
            let mut text = String::new();
            for b in blocks {
                let ty = b.get("type").and_then(Value::as_str).unwrap_or("");
                match ty {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                    "tool_result" => {
                        flush_text(&mut text, items);
                        let call_id = b
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool_call")
                            .to_string();
                        let output = tool_result_to_string(b.get("content"));
                        items.push(ConversationItem::tool_result(call_id, output));
                    }
                    // Drop images in phase 0 with placeholder
                    "image" => {
                        text.push_str("[image omitted]");
                    }
                    _ => {
                        // ignore cache_control-only etc.
                    }
                }
            }
            flush_text(&mut text, items);
        }
        _ => {
            return Err(TranslateError::Message(
                "unsupported user content shape".into(),
            ));
        }
    }
    Ok(())
}

fn push_assistant_content(
    content: Option<&Value>,
    items: &mut Vec<ConversationItem>,
) -> Result<(), TranslateError> {
    let Some(content) = content else {
        return Ok(());
    };
    match content {
        Value::String(s) => {
            items.push(ConversationItem::assistant(s.clone()));
        }
        Value::Array(blocks) => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for b in blocks {
                let ty = b.get("type").and_then(Value::as_str).unwrap_or("");
                match ty {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                    "thinking" | "redacted_thinking" => {
                        if let Some(sig) = b.get("signature").and_then(Value::as_str) {
                            if let Some(replay) = decode_reasoning_signature(sig) {
                                items.push(ConversationItem::Reasoning(rs::ReasoningItem {
                                    id: replay.id,
                                    summary: vec![],
                                    content: None,
                                    encrypted_content: Some(replay.encrypted_content),
                                    status: None,
                                }));
                            }
                        }
                        // Plain thinking text is not re-sent as input (encrypted path only).
                    }
                    "tool_use" => {
                        let id = b
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool_call")
                            .to_string();
                        let name = b
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let input = b
                            .get("input")
                            .cloned()
                            .unwrap_or(Value::Object(Default::default()));
                        let args = serde_json::to_string(&input).unwrap_or_else(|_| "{}".into());
                        tool_calls.push(xai_grok_sampling_types::conversation::ToolCall {
                            id: std::sync::Arc::<str>::from(id),
                            name,
                            arguments: std::sync::Arc::<str>::from(args),
                        });
                    }
                    _ => {}
                }
            }
            if !tool_calls.is_empty() {
                items.push(ConversationItem::assistant_tool_calls(tool_calls));
            } else if !text.is_empty() {
                items.push(ConversationItem::assistant(text));
            }
        }
        _ => {
            return Err(TranslateError::Message(
                "unsupported assistant content shape".into(),
            ));
        }
    }
    Ok(())
}

fn flush_text(text: &mut String, items: &mut Vec<ConversationItem>) {
    if !text.is_empty() {
        items.push(ConversationItem::user(std::mem::take(text)));
    }
}

fn tool_result_to_string(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let mut out = String::new();
            for b in blocks {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                } else if b.get("type").and_then(Value::as_str) == Some("image") {
                    out.push_str("[image omitted]");
                }
            }
            out
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Map Anthropic tools 1:1. WebSearch/XSearch → hosted; else function ToolSpec.
/// Does **not** rewrite tools based on user intent (PR #57 lesson).
fn parse_tools(
    tools: Option<&Value>,
) -> Result<(Vec<ToolSpec>, Vec<HostedTool>), TranslateError> {
    let Some(Value::Array(arr)) = tools else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut function_tools = Vec::new();
    let mut hosted = Vec::new();
    for t in arr {
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let ty = t.get("type").and_then(Value::as_str).unwrap_or("custom");
        // Anthropic hosted tool types
        if name == "WebSearch" || name.starts_with("web_search") || ty.contains("web_search") {
            let allowed = t
                .get("allowed_domains")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                });
            hosted.push(HostedTool::WebSearch {
                allowed_domains: allowed,
            });
            continue;
        }
        if name == "XSearch" || name.starts_with("x_search") || ty.contains("x_search") {
            hosted.push(HostedTool::XSearch);
            continue;
        }
        let description = t
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string);
        let parameters = t
            .get("input_schema")
            .cloned()
            .or_else(|| t.get("parameters").cloned())
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        if name.is_empty() {
            continue;
        }
        function_tools.push(ToolSpec {
            name,
            description,
            parameters,
        });
    }
    Ok((function_tools, hosted))
}

fn parse_tool_choice(v: Option<&Value>) -> Option<ConversationToolChoice> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return match s {
            "auto" => Some(ConversationToolChoice::Auto),
            "none" => Some(ConversationToolChoice::None),
            "any" | "required" => Some(ConversationToolChoice::Required),
            _ => None,
        };
    }
    if let Some(ty) = v.get("type").and_then(Value::as_str) {
        return match ty {
            "auto" => Some(ConversationToolChoice::Auto),
            "none" => Some(ConversationToolChoice::None),
            "any" => Some(ConversationToolChoice::Required),
            "tool" => v
                .get("name")
                .and_then(Value::as_str)
                .map(|n| ConversationToolChoice::Function(n.to_string())),
            _ => None,
        };
    }
    None
}

fn parse_effort(s: &str) -> Option<xai_grok_sampling_types::ReasoningEffort> {
    use xai_grok_sampling_types::ReasoningEffort;
    match s {
        "none" => Some(ReasoningEffort::None),
        "minimal" | "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" | "max" => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_1m_suffix() {
        assert_eq!(strip_context_suffix("grok-4.5[1m]"), "grok-4.5");
    }

    #[test]
    fn maps_simple_user_message() {
        let body = json!({
            "model": "grok-4.5",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let epoch = crate::SessionEpoch {
            claude_session_id: "s".into(),
            grok_session_id: "s".into(),
            conv_id: "c".into(),
            turn: 1,
            tools_hash: None,
            epoch: 0,
        };
        let req = translate_messages_request(&body, &epoch, "grok-4.5", "r1").unwrap();
        assert_eq!(req.model.as_deref(), Some("grok-4.5"));
        assert_eq!(req.items.len(), 1);
    }

    #[test]
    fn accepts_system_role_in_messages_array() {
        // Claude Code interactive sends system as a message role.
        let body = json!({
            "model": "grok-4.5",
            "max_tokens": 64,
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ]
        });
        let epoch = crate::SessionEpoch {
            claude_session_id: "s".into(),
            grok_session_id: "s".into(),
            conv_id: "c".into(),
            turn: 1,
            tools_hash: None,
            epoch: 0,
        };
        let req = translate_messages_request(&body, &epoch, "grok-4.5", "r1").unwrap();
        assert!(req.items.len() >= 2);
        assert!(matches!(
            &req.items[0],
            xai_grok_sampling_types::conversation::ConversationItem::System(_)
        ));
    }
}

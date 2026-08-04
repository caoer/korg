//! Responses stream events → Anthropic-facing content events.

use serde_json::{Value, json};
use xai_grok_sampling_types::rs::{self, ResponseStreamEvent};

use crate::reasoning_signature::{
    PendingReasoning, ReasoningReplay, encode_reasoning_signature,
};

/// Intermediate events for Anthropic SSE emission.
#[derive(Debug, Clone)]
pub enum AnthropicOut {
    ThinkingStart { index: usize },
    ThinkingDelta { index: usize, text: String },
    ThinkingSignature { index: usize, signature: String },
    ThinkingStop { index: usize },
    TextStart { index: usize },
    TextDelta { index: usize, text: String },
    TextStop { index: usize },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolUseDelta { index: usize, partial_json: String },
    ToolUseStop { index: usize },
    MessageDelta {
        stop_reason: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    MessageStop,
    Error { message: String },
}

#[derive(Default)]
pub struct StreamReducer {
    next_index: usize,
    active_text: Option<usize>,
    active_thinking: Option<usize>,
    /// call_id / item id → (anthropic_index, name)
    tools: std::collections::HashMap<String, (usize, String)>,
    /// output_index → anthropic tool index (for args deltas)
    output_to_tool: std::collections::HashMap<u32, usize>,
    pending_reasoning: PendingReasoning,
    saw_tool: bool,
    /// True once we have emitted MessageStop (completed/incomplete/forced finish).
    finished: bool,
}

impl StreamReducer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_event(&mut self, event: &ResponseStreamEvent) -> Vec<AnthropicOut> {
        match event {
            ResponseStreamEvent::ResponseOutputTextDelta(e) => {
                let mut out = Vec::new();
                if self.active_text.is_none() {
                    let idx = self.alloc();
                    self.active_text = Some(idx);
                    out.push(AnthropicOut::TextStart { index: idx });
                }
                if let Some(idx) = self.active_text {
                    if !e.delta.is_empty() {
                        out.push(AnthropicOut::TextDelta {
                            index: idx,
                            text: e.delta.clone(),
                        });
                    }
                }
                out
            }
            ResponseStreamEvent::ResponseOutputTextDone(_) => {
                if let Some(idx) = self.active_text.take() {
                    vec![AnthropicOut::TextStop { index: idx }]
                } else {
                    vec![]
                }
            }
            ResponseStreamEvent::ResponseReasoningSummaryTextDelta(e) => {
                self.thinking_delta(&e.delta)
            }
            ResponseStreamEvent::ResponseReasoningTextDelta(e) => self.thinking_delta(&e.delta),
            ResponseStreamEvent::ResponseOutputItemDone(e) => self.on_output_item_done(&e.item),
            ResponseStreamEvent::ResponseOutputItemAdded(e) => {
                self.on_output_item_added(e.output_index, &e.item)
            }
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(e) => {
                let mut out = Vec::new();
                if let Some(&idx) = self.output_to_tool.get(&e.output_index) {
                    if !e.delta.is_empty() {
                        out.push(AnthropicOut::ToolUseDelta {
                            index: idx,
                            partial_json: e.delta.clone(),
                        });
                    }
                } else {
                    // Fallback: try item_id map
                    if let Some((idx, _)) = self.tools.get(&e.item_id) {
                        out.push(AnthropicOut::ToolUseDelta {
                            index: *idx,
                            partial_json: e.delta.clone(),
                        });
                    }
                }
                out
            }
            ResponseStreamEvent::ResponseCompleted(e) => {
                self.finish_open_blocks(Some(&e.response))
            }
            ResponseStreamEvent::ResponseIncomplete(e) => {
                self.finish_open_blocks(Some(&e.response))
            }
            ResponseStreamEvent::ResponseFailed(e) => {
                let msg = e
                    .response
                    .error
                    .as_ref()
                    .map(|err| err.message.clone())
                    .unwrap_or_else(|| "upstream response failed".into());
                vec![AnthropicOut::Error { message: msg }]
            }
            _ => vec![],
        }
    }

    fn thinking_delta(&mut self, delta: &str) -> Vec<AnthropicOut> {
        let mut out = Vec::new();
        if self.active_thinking.is_none() {
            let idx = self.alloc();
            self.active_thinking = Some(idx);
            out.push(AnthropicOut::ThinkingStart { index: idx });
        }
        if let Some(idx) = self.active_thinking {
            if !delta.is_empty() {
                out.push(AnthropicOut::ThinkingDelta {
                    index: idx,
                    text: delta.to_string(),
                });
            }
        }
        out
    }

    fn on_output_item_added(
        &mut self,
        output_index: u32,
        item: &rs::OutputItem,
    ) -> Vec<AnthropicOut> {
        match item {
            rs::OutputItem::FunctionCall(fc) => {
                self.saw_tool = true;
                let mut out = Vec::new();
                if let Some(idx) = self.active_text.take() {
                    out.push(AnthropicOut::TextStop { index: idx });
                }
                let idx = self.alloc();
                let id = if fc.call_id.is_empty() {
                    fc.id.clone().unwrap_or_else(|| format!("call_{idx}"))
                } else {
                    fc.call_id.clone()
                };
                let name = fc.name.clone();
                self.tools.insert(id.clone(), (idx, name.clone()));
                if let Some(ref item_id) = fc.id {
                    self.tools.insert(item_id.clone(), (idx, name.clone()));
                }
                self.output_to_tool.insert(output_index, idx);
                out.push(AnthropicOut::ToolUseStart {
                    index: idx,
                    id,
                    name,
                });
                if !fc.arguments.is_empty() {
                    out.push(AnthropicOut::ToolUseDelta {
                        index: idx,
                        partial_json: fc.arguments.clone(),
                    });
                }
                out
            }
            rs::OutputItem::Reasoning(r) => {
                self.pending_reasoning
                    .capture_fields(Some(r.id.as_str()), r.encrypted_content.as_deref());
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_output_item_done(&mut self, item: &rs::OutputItem) -> Vec<AnthropicOut> {
        match item {
            rs::OutputItem::FunctionCall(fc) => {
                let mut out = Vec::new();
                let key = if fc.call_id.is_empty() {
                    fc.id.clone().unwrap_or_default()
                } else {
                    fc.call_id.clone()
                };
                if let Some((idx, _)) = self.tools.get(&key).cloned() {
                    // Drop every alias of this tool (call_id and item id both
                    // map to idx) so the end-of-stream drain cannot close the
                    // block again: a repeated content_block_stop makes Claude
                    // Code materialize a duplicate tool_use block and execute
                    // the call once per copy.
                    self.tools.retain(|_, (i, _)| *i != idx);
                    self.output_to_tool.retain(|_, i| *i != idx);
                    out.push(AnthropicOut::ToolUseStop { index: idx });
                }
                out
            }
            rs::OutputItem::Reasoning(r) => {
                self.pending_reasoning
                    .capture_fields(Some(r.id.as_str()), r.encrypted_content.as_deref());
                let mut out = Vec::new();
                if let Some(replay) = self.pending_reasoning.replay() {
                    if let Some(sig) = encode_reasoning_signature(&replay) {
                        let idx = self.active_thinking.unwrap_or_else(|| {
                            let i = self.alloc();
                            out.push(AnthropicOut::ThinkingStart { index: i });
                            self.active_thinking = Some(i);
                            i
                        });
                        out.push(AnthropicOut::ThinkingSignature {
                            index: idx,
                            signature: sig,
                        });
                    }
                }
                if let Some(idx) = self.active_thinking.take() {
                    out.push(AnthropicOut::ThinkingStop { index: idx });
                }
                out
            }
            rs::OutputItem::Message(_) => {
                if let Some(idx) = self.active_text.take() {
                    vec![AnthropicOut::TextStop { index: idx }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn finish_open_blocks(&mut self, response: Option<&rs::Response>) -> Vec<AnthropicOut> {
        let mut out = Vec::new();
        if let Some(idx) = self.active_thinking.take() {
            if let Some(replay) = self.pending_reasoning.replay() {
                if let Some(sig) = encode_reasoning_signature(&replay) {
                    out.push(AnthropicOut::ThinkingSignature {
                        index: idx,
                        signature: sig,
                    });
                }
            }
            out.push(AnthropicOut::ThinkingStop { index: idx });
        }
        if let Some(idx) = self.active_text.take() {
            out.push(AnthropicOut::TextStop { index: idx });
        }
        // A tool is registered under both its call_id and its item id, so
        // dedupe by block index — one content_block_stop per open block.
        let mut open: Vec<usize> = self.tools.drain().map(|(_k, (idx, _))| idx).collect();
        open.sort_unstable();
        open.dedup();
        for idx in open {
            out.push(AnthropicOut::ToolUseStop { index: idx });
        }

        // Claude Code rejects streams that never complete a content block
        // ("Stream ended without receiving any events" / tengu_stream_no_events)
        // unless message_delta carries a stop_reason. Prefer a real block when
        // we had one; otherwise emit an empty text block so the lifecycle is valid.
        let had_content = self.next_index > 0 || self.saw_tool;
        if !had_content {
            let idx = self.alloc();
            out.push(AnthropicOut::TextStart { index: idx });
            out.push(AnthropicOut::TextStop { index: idx });
        }

        let (input_tokens, output_tokens) = response
            .and_then(|r| r.usage.as_ref())
            .map(|u| (Some(u.input_tokens as u64), Some(u.output_tokens as u64)))
            .unwrap_or((None, None));

        let stop_reason = if self.saw_tool {
            Some("tool_use".into())
        } else {
            Some("end_turn".into())
        };

        out.push(AnthropicOut::MessageDelta {
            stop_reason,
            input_tokens,
            output_tokens,
        });
        out.push(AnthropicOut::MessageStop);
        self.finished = true;
        out
    }

    /// Close the Anthropic stream if upstream ended without `response.completed`.
    ///
    /// Returns empty if we already emitted `message_stop` (via completed/incomplete).
    pub fn finish_if_needed(&mut self) -> Vec<AnthropicOut> {
        if self.finished {
            return vec![];
        }
        self.finish_open_blocks(None)
    }

    /// Whether `message_stop` has already been emitted.
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn alloc(&mut self) -> usize {
        let i = self.next_index;
        self.next_index += 1;
        i
    }

    pub fn last_reasoning_replay(&self) -> Option<ReasoningReplay> {
        self.pending_reasoning.replay()
    }
}

/// Debug helper: summarize event type for traffic bus.
pub fn event_type_name(event: &ResponseStreamEvent) -> &'static str {
    match event {
        ResponseStreamEvent::ResponseOutputTextDelta(_) => "response.output_text.delta",
        ResponseStreamEvent::ResponseCompleted(_) => "response.completed",
        ResponseStreamEvent::ResponseFailed(_) => "response.failed",
        ResponseStreamEvent::ResponseOutputItemDone(_) => "response.output_item.done",
        ResponseStreamEvent::ResponseOutputItemAdded(_) => "response.output_item.added",
        ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(_) => {
            "response.function_call_arguments.delta"
        }
        _ => "response.other",
    }
}

pub fn anthropic_out_debug(ev: &AnthropicOut) -> Value {
    match ev {
        AnthropicOut::TextDelta { index, text } => {
            json!({"type": "text_delta", "index": index, "len": text.len()})
        }
        AnthropicOut::ThinkingSignature { index, .. } => {
            json!({"type": "thinking_signature", "index": index})
        }
        AnthropicOut::ToolUseStart { index, name, id } => {
            json!({"type": "tool_use_start", "index": index, "name": name, "id": id})
        }
        AnthropicOut::MessageStop => json!({"type": "message_stop"}),
        AnthropicOut::Error { message } => json!({"type": "error", "message": message}),
        other => json!({"type": format!("{other:?}").chars().take(40).collect::<String>()}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function_call_item() -> rs::OutputItem {
        rs::OutputItem::FunctionCall(rs::FunctionToolCall {
            arguments: "{\"text\":\"banana\"}".into(),
            call_id: "call-1".into(),
            name: "echo_tool".into(),
            id: Some("fc_item_1".into()),
            status: None,
        })
    }

    fn count_tool_stops(events: &[AnthropicOut]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, AnthropicOut::ToolUseStop { .. }))
            .count()
    }

    /// One tool call must close its content block exactly once. A repeated
    /// content_block_stop makes Claude Code materialize a duplicate tool_use
    /// block — and execute the call once per copy (observed 3x: one stop from
    /// output_item.done plus one per alias key from the end-of-stream drain).
    #[test]
    fn tool_use_stop_emitted_exactly_once_when_item_done() {
        let mut r = StreamReducer::new();
        let mut stops = 0;
        stops += count_tool_stops(&r.push_event(
            &ResponseStreamEvent::ResponseOutputItemAdded(rs::ResponseOutputItemAddedEvent {
                sequence_number: 1,
                output_index: 0,
                item: function_call_item(),
            }),
        ));
        stops += count_tool_stops(&r.push_event(
            &ResponseStreamEvent::ResponseOutputItemDone(rs::ResponseOutputItemDoneEvent {
                sequence_number: 2,
                output_index: 0,
                item: function_call_item(),
            }),
        ));
        let tail = r.finish_if_needed();
        stops += count_tool_stops(&tail);
        assert_eq!(stops, 1, "tool block closed more than once");
        assert!(matches!(tail.last(), Some(AnthropicOut::MessageStop)));
    }

    /// A tool left open at end of stream is closed by the drain — once, even
    /// though it is registered under both call_id and item id.
    #[test]
    fn unclosed_tool_drain_dedupes_alias_keys() {
        let mut r = StreamReducer::new();
        r.push_event(&ResponseStreamEvent::ResponseOutputItemAdded(
            rs::ResponseOutputItemAddedEvent {
                sequence_number: 1,
                output_index: 0,
                item: function_call_item(),
            },
        ));
        assert_eq!(count_tool_stops(&r.finish_if_needed()), 1);
    }

    #[test]
    fn finish_if_needed_emits_empty_text_and_stop_on_blank_stream() {
        let mut r = StreamReducer::new();
        let out = r.finish_if_needed();
        assert!(
            out.iter()
                .any(|e| matches!(e, AnthropicOut::TextStart { .. })),
            "empty upstream must still open a content block: {out:?}"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, AnthropicOut::TextStop { .. }))
        );
        assert!(matches!(out.last(), Some(AnthropicOut::MessageStop)));
        let stop = out.iter().find_map(|e| match e {
            AnthropicOut::MessageDelta { stop_reason, .. } => stop_reason.clone(),
            _ => None,
        });
        assert_eq!(stop.as_deref(), Some("end_turn"));
        assert!(r.is_finished());
        assert!(r.finish_if_needed().is_empty(), "idempotent");
    }
}

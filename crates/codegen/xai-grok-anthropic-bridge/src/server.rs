//! Axum HTTP surface: Anthropic Messages façade.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;
use xai_grok_sampler::SamplingClient;

use crate::epoch::SessionRegistry;
use crate::live_auth::BridgeAuth;
use crate::serve_config::ServeConfig;
use crate::sse;
use crate::traffic::{TrafficBus, TrafficSide};
use crate::translate::{
    AnthropicOut, StreamReducer, anthropic_out_debug, event_type_name, translate_messages_request,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServeConfig>,
    pub client: Arc<SamplingClient>,
    pub sessions: Arc<SessionRegistry>,
    pub traffic: TrafficBus,
    /// Live OIDC session for 401 recovery (None when static API key).
    pub auth: Arc<BridgeAuth>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({"ok": true, "service": "grok-anthropic-bridge"}))
}

async fn count_tokens(
    State(_state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // Phase 0: rough estimate (~4 chars/token).
    let text = body.to_string();
    let tokens = (text.len() as u64 / 4).max(1);
    Json(json!({
        "input_tokens": tokens
    }))
}

async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let req_id = Uuid::new_v4().to_string();
    let stream = body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    state.traffic.record_json(
        &req_id,
        TrafficSide::Claude,
        "request",
        body.clone(),
    );

    let claude_session = headers
        .get("x-claude-code-session-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            body.pointer("/metadata/user_id")
                .and_then(Value::as_str)
        });

    let epoch = state
        .sessions
        .begin_turn(claude_session, body.get("tools"));

    let translated = match translate_messages_request(
        &body,
        &epoch,
        &state.config.default_model,
        &req_id,
    ) {
        Ok(r) => r,
        Err(e) => {
            return anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string());
        }
    };

    state.traffic.record_json(
        &req_id,
        TrafficSide::Grok,
        "request_meta",
        json!({
            "model": translated.model,
            "items": translated.items.len(),
            "tools": translated.tools.len(),
            "hosted_tools": translated.hosted_tools.len(),
            "x_grok_session_id": translated.x_grok_session_id,
            "x_grok_conv_id": translated.x_grok_conv_id,
            "x_grok_turn_idx": translated.x_grok_turn_idx,
            "x_grok_req_id": translated.x_grok_req_id,
        }),
    );

    if !stream {
        return match non_stream_messages(&state, &req_id, translated).await {
            Ok(v) => {
                state
                    .traffic
                    .record_json(&req_id, TrafficSide::Claude, "response", v.clone());
                Json(v).into_response()
            }
            Err(e) => anthropic_error(StatusCode::BAD_GATEWAY, "api_error", e.to_string()),
        };
    }

    stream_messages_response(state, req_id, translated).await
}

/// Open a Responses stream; on auth-looking failure, run OIDC recovery once and retry.
async fn open_conversation_stream(
    state: &AppState,
    request: xai_grok_sampling_types::conversation::ConversationRequest,
) -> xai_grok_sampling_types::Result<(
    futures_util::stream::BoxStream<
        'static,
        xai_grok_sampling_types::Result<xai_grok_sampling_types::rs::ResponseStreamEvent>,
    >,
    Option<xai_grok_sampling_types::ResponseModelMetadata>,
    Option<xai_grok_sampler::doom_loop::DoomLoopSignalCollector>,
)> {
    match state
        .client
        .conversation_stream_responses(request.clone())
        .await
    {
        Ok(s) => Ok(s),
        Err(e) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            let authish = msg.contains("401")
                || lower.contains("unauthor")
                || lower.contains("authentication");
            if authish {
                if let Some(session) = state.auth.session() {
                    tracing::warn!(
                        %msg,
                        "auth error from sampler; attempting LiveSessionAuth recovery"
                    );
                    if session.recover_after_unauthorized().await {
                        return state.client.conversation_stream_responses(request).await;
                    }
                }
            }
            Err(e)
        }
    }
}

async fn non_stream_messages(
    state: &AppState,
    req_id: &str,
    request: xai_grok_sampling_types::conversation::ConversationRequest,
) -> anyhow::Result<Value> {
    // Collect stream into a single Anthropic message object (phase 0).
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| state.config.default_model.clone());
    let (mut stream, _meta, _doom) = open_conversation_stream(state, request)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut reducer = StreamReducer::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut signature: Option<String> = None;
    let mut tool_blocks: Vec<Value> = Vec::new();
    let mut stop_reason = "end_turn".to_string();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;

    while let Some(ev) = stream.next().await {
        let ev = ev.map_err(|e| anyhow::anyhow!("{e}"))?;
        for out in reducer.push_event(&ev) {
            match out {
                AnthropicOut::TextDelta { text: t, .. } => text.push_str(&t),
                AnthropicOut::ThinkingDelta { text: t, .. } => thinking.push_str(&t),
                AnthropicOut::ThinkingSignature { signature: s, .. } => signature = Some(s),
                AnthropicOut::ToolUseStart { id, name, .. } => {
                    tool_blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }));
                    stop_reason = "tool_use".into();
                }
                AnthropicOut::ToolUseDelta { partial_json, .. } => {
                    if let Some(last) = tool_blocks.last_mut() {
                        let acc = last
                            .as_object_mut()
                            .unwrap()
                            .entry("_acc")
                            .or_insert_with(|| json!(""));
                        if let Some(s) = acc.as_str() {
                            *acc = json!(format!("{s}{partial_json}"));
                        }
                    }
                }
                AnthropicOut::MessageDelta {
                    stop_reason: sr,
                    input_tokens: it,
                    output_tokens: ot,
                    ..
                } => {
                    if let Some(s) = sr {
                        stop_reason = s;
                    }
                    if let Some(i) = it {
                        input_tokens = scale_tokens(i, state.config.usage_scale);
                    }
                    if let Some(o) = ot {
                        output_tokens = o;
                    }
                }
                AnthropicOut::Error { message } => anyhow::bail!("{message}"),
                _ => {}
            }
        }
    }

    for b in &mut tool_blocks {
        if let Some(acc) = b.get("_acc").and_then(Value::as_str) {
            let input: Value = serde_json::from_str(acc).unwrap_or(json!({}));
            b.as_object_mut().unwrap().remove("_acc");
            b.as_object_mut().unwrap().insert("input".into(), input);
        }
    }

    let mut content = Vec::new();
    if !thinking.is_empty() || signature.is_some() {
        content.push(json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": signature.unwrap_or_default()
        }));
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    content.extend(tool_blocks);

    Ok(json!({
        "id": format!("msg_{req_id}"),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }
    }))
}

async fn stream_messages_response(
    state: AppState,
    req_id: String,
    request: xai_grok_sampling_types::conversation::ConversationRequest,
) -> Response {
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| state.config.default_model.clone());
    let message_id = format!("msg_{req_id}");

    let (tx, rx) = mpsc::channel::<Result<String, String>>(64);

    tokio::spawn(async move {
        let send_frame = |tx: &mpsc::Sender<Result<String, String>>, frame: String| {
            let tx = tx.clone();
            async move { tx.send(Ok(frame)).await.is_ok() }
        };

        // Open upstream before message_start so open failures become a clean
        // `event: error` (Claude Code throws) instead of a half-started stream
        // that ends with zero content → "Stream ended without receiving any events".
        let stream_result = open_conversation_stream(&state, request).await;
        let (mut stream, _meta, _doom) = match stream_result {
            Ok(s) => s,
            Err(e) => {
                let _ = send_frame(&tx, sse::error_event(e.to_string())).await;
                return;
            }
        };

        if !send_frame(&tx, sse::message_start(&message_id, &model)).await {
            return;
        }

        let mut reducer = StreamReducer::new();
        // When set, stop reducing and emit a single terminal `event: error`.
        let mut fatal_error: Option<String> = None;

        while let Some(ev) = stream.next().await {
            match ev {
                Ok(event) => {
                    state.traffic.record_json(
                        &req_id,
                        TrafficSide::Grok,
                        "sse_event",
                        json!({"type": event_type_name(&event)}),
                    );
                    for out in reducer.push_event(&event) {
                        if let AnthropicOut::Error { message } = &out {
                            // Do not emit here — send once after the loop so we
                            // never mix a partial finish with a duplicate error.
                            fatal_error = Some(message.clone());
                            break;
                        }
                        state.traffic.record_json(
                            &req_id,
                            TrafficSide::Claude,
                            "out",
                            anthropic_out_debug(&out),
                        );
                        for frame in sse::encode_out(&out) {
                            if !send_frame(&tx, frame).await {
                                return;
                            }
                        }
                    }
                    if fatal_error.is_some() {
                        break;
                    }
                }
                Err(e) => {
                    fatal_error = Some(e.to_string());
                    break;
                }
            }
        }

        if let Some(message) = fatal_error {
            // Claude Code's SDK only surfaces APIError for `event: error`.
            let _ = send_frame(&tx, sse::error_event(message)).await;
            return;
        }

        // Upstream closed without response.completed / incomplete — still close
        // the Anthropic SSE lifecycle so Claude Code does not treat it as empty.
        for out in reducer.finish_if_needed() {
            state.traffic.record_json(
                &req_id,
                TrafficSide::Claude,
                "out",
                anthropic_out_debug(&out),
            );
            for frame in sse::encode_out(&out) {
                if !send_frame(&tx, frame).await {
                    return;
                }
            }
        }
    });

    let body_stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(item) = rx.recv().await {
            match item {
                Ok(s) => yield Ok::<_, std::io::Error>(bytes::Bytes::from(s)),
                Err(e) => {
                    yield Ok(bytes::Bytes::from(sse::error_event(e)));
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "failed to build SSE response",
            )
        })
}

fn scale_tokens(n: u64, scale: Option<f64>) -> u64 {
    match scale {
        Some(s) if s > 0.0 => ((n as f64) * s).round() as u64,
        _ => n,
    }
}

fn anthropic_error(status: StatusCode, ty: &str, message: impl Into<String>) -> Response {
    let body = json!({
        "type": "error",
        "error": {
            "type": ty,
            "message": message.into()
        }
    });
    (status, Json(body)).into_response()
}

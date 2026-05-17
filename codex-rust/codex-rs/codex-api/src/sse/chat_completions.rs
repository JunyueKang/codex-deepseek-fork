use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;

const REQUEST_ID_HEADER: &str = "x-request-id";

pub fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_chat_completions_sse(
        stream_response.bytes,
        tx_event,
        idle_timeout,
        telemetry,
    ));

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

#[derive(Debug, Default)]
struct ChatCompletionState {
    response_id: Option<String>,
    fallback_id_counter: usize,
    assistant_item_id: Option<String>,
    assistant_text: String,
    reasoning_content: String,
    tool_calls: BTreeMap<usize, ToolCallState>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Default)]
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    id: Option<String>,
    choices: Option<Vec<ChatChoice>>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    delta: ChatDelta,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ChatDeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatDeltaToolCall {
    index: usize,
    id: Option<String>,
    function: Option<ChatDeltaFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct ChatDeltaFunctionCall {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    prompt_tokens_details: Option<ChatPromptTokensDetails>,
    completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct ChatPromptTokensDetails {
    cached_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionTokensDetails {
    reasoning_tokens: Option<i64>,
}

impl From<ChatUsage> for TokenUsage {
    fn from(value: ChatUsage) -> Self {
        Self {
            input_tokens: value.prompt_tokens,
            cached_input_tokens: value
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens)
                .unwrap_or(0),
            output_tokens: value.completion_tokens,
            reasoning_output_tokens: value
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens)
                .unwrap_or(0),
            total_tokens: value.total_tokens,
        }
    }
}

async fn process_chat_completions_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut state = ChatCompletionState::default();
    if tx_event.send(Ok(ResponseEvent::Created)).await.is_err() {
        return;
    }

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(telemetry) = telemetry.as_ref() {
            telemetry.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(err))) => {
                debug!("chat completions SSE error: {err:#}");
                let _ = tx_event.send(Err(ApiError::Stream(err.to_string()))).await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "stream closed before chat completion finished".to_string(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        if sse.data.trim() == "[DONE]" {
            finish_chat_stream(&mut state, &tx_event).await;
            return;
        }

        let chunk = match serde_json::from_str::<ChatChunk>(&sse.data) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!(
                    "failed to parse chat completions SSE event: {err}, data: {}",
                    sse.data
                );
                continue;
            }
        };

        if state.response_id.is_none() {
            state.response_id = chunk.id.clone();
        }
        if let Some(usage) = chunk.usage {
            state.usage = Some(usage.into());
        }

        let Some(choices) = chunk.choices else {
            continue;
        };
        for choice in choices {
            if handle_chat_delta(&mut state, choice.delta, &tx_event)
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

async fn handle_chat_delta(
    state: &mut ChatCompletionState,
    delta: ChatDelta,
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
) -> Result<(), ()> {
    if let Some(reasoning_content) = delta.reasoning_content
        && !reasoning_content.is_empty()
    {
        state.reasoning_content.push_str(&reasoning_content);
    }

    if let Some(content) = delta.content
        && !content.is_empty()
    {
        ensure_assistant_item_started(state, tx_event).await?;
        state.assistant_text.push_str(&content);
        if tx_event
            .send(Ok(ResponseEvent::OutputTextDelta(content)))
            .await
            .is_err()
        {
            return Err(());
        }
    }

    if let Some(tool_calls) = delta.tool_calls {
        for tool_call in tool_calls {
            let call = state.tool_calls.entry(tool_call.index).or_default();
            if let Some(id) = tool_call.id {
                call.id = Some(id);
            }
            if let Some(function) = tool_call.function {
                if let Some(name) = function.name {
                    call.name = Some(name);
                }
                if let Some(arguments) = function.arguments
                    && !arguments.is_empty()
                {
                    call.arguments.push_str(&arguments);
                    let item_id = call
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("chat_tool_call_{}", tool_call.index));
                    if tx_event
                        .send(Ok(ResponseEvent::ToolCallInputDelta {
                            item_id,
                            call_id: call.id.clone(),
                            delta: arguments,
                        }))
                        .await
                        .is_err()
                    {
                        return Err(());
                    }
                }
            }
        }
    }

    Ok(())
}

async fn ensure_assistant_item_started(
    state: &mut ChatCompletionState,
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
) -> Result<(), ()> {
    if state.assistant_item_id.is_some() {
        return Ok(());
    }
    let item_id = format!(
        "{}_message",
        state
            .response_id
            .as_deref()
            .unwrap_or("chat_completion_response")
    );
    state.assistant_item_id = Some(item_id.clone());
    tx_event
        .send(Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message {
            id: Some(item_id),
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            reasoning_content: None,
        })))
        .await
        .map_err(|_| ())
}

async fn finish_chat_stream(
    state: &mut ChatCompletionState,
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
) {
    if !state.assistant_text.is_empty()
        && tx_event
            .send(Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: state.assistant_item_id.clone(),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: std::mem::take(&mut state.assistant_text),
                }],
                phase: None,
                reasoning_content: non_empty_reasoning_content(state),
            })))
            .await
            .is_err()
    {
        return;
    }

    let tool_calls = std::mem::take(&mut state.tool_calls);
    for (_, call) in tool_calls {
        let Some(name) = call.name else {
            continue;
        };
        let call_id = match call.id {
            Some(call_id) => call_id,
            None => {
                state.fallback_id_counter += 1;
                format!("chat_tool_call_{}", state.fallback_id_counter)
            }
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(
                ResponseItem::FunctionCall {
                    id: None,
                    name,
                    namespace: None,
                    arguments: call.arguments,
                    call_id,
                    reasoning_content: non_empty_reasoning_content(state),
                },
            )))
            .await
            .is_err()
        {
            return;
        }
    }

    let response_id = state
        .response_id
        .clone()
        .unwrap_or_else(|| "chatcmpl_unknown".to_string());
    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id,
            token_usage: state.usage.take(),
            end_turn: None,
        }))
        .await;
}

fn non_empty_reasoning_content(state: &ChatCompletionState) -> Option<String> {
    (!state.reasoning_content.is_empty()).then(|| state.reasoning_content.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bytes::Bytes;
    use codex_client::TransportError;
    use futures::stream;
    use http::HeaderMap;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    async fn run_sse_data(events: Vec<serde_json::Value>) -> Vec<ResponseEvent> {
        let mut body = String::new();
        for event in events {
            body.push_str(&format!("data: {event}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");

        let stream = stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(body))]);
        let response = StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream),
        };
        let mut response_stream = spawn_chat_completions_stream(
            response,
            Duration::from_secs(1),
            /*telemetry*/ None,
            /*turn_state*/ None,
        );

        let mut events = Vec::new();
        while let Some(event) = response_stream.rx_event.recv().await {
            events.push(event.expect("stream event should parse"));
        }
        events
    }

    #[tokio::test]
    async fn streamed_text_uses_same_message_item_for_start_and_done() {
        let events = run_sse_data(vec![
            json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "delta": {
                        "content": "**hello"
                    }
                }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "delta": {
                        "content": "**"
                    }
                }]
            }),
        ])
        .await;

        assert_eq!(events.len(), 6);
        assert_matches!(&events[0], ResponseEvent::Created);
        assert_matches!(
            &events[1],
            ResponseEvent::OutputItemAdded(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                ..
            }) if id == "chatcmpl-1_message" && role == "assistant" && content.is_empty()
        );
        assert_matches!(
            &events[2],
            ResponseEvent::OutputTextDelta(delta) if delta == "**hello"
        );
        assert_matches!(
            &events[3],
            ResponseEvent::OutputTextDelta(delta) if delta == "**"
        );
        assert_matches!(
            &events[4],
            ResponseEvent::OutputItemDone(ResponseItem::Message {
                id: Some(id),
                role,
                content,
                ..
            }) if id == "chatcmpl-1_message"
                && role == "assistant"
                && content == &vec![ContentItem::OutputText {
                    text: "**hello**".to_string()
                }]
        );
        assert_matches!(&events[5], ResponseEvent::Completed { .. });
    }
}

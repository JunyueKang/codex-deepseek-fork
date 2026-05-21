use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::sse::spawn_chat_completions_stream;
use crate::telemetry::SseTelemetry;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::instrument;

pub struct ChatCompletionsClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

#[derive(Default)]
pub struct ChatCompletionsOptions {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    tool_choice: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    stream: bool,
    stream_options: ChatStreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

#[derive(Serialize)]
struct ChatStreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatMessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatMessageContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ChatContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ChatImageUrl },
}

#[derive(Serialize)]
struct ChatImageUrl {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatFunctionCall,
}

#[derive(Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

impl<T: HttpTransport> ChatCompletionsClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    #[instrument(
        name = "chat_completions.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "chat_completions_http",
            http.method = "POST",
            api.path = "chat/completions"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ChatCompletionsOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ChatCompletionsOptions {
            session_id,
            thread_id,
            session_source,
            mut extra_headers,
            turn_state,
        } = options;

        if let Some(ref thread_id) = thread_id {
            insert_header(&mut extra_headers, "x-client-request-id", thread_id);
        }
        extra_headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut extra_headers, "x-openai-subagent", &subagent);
        }

        let chat_request = ChatCompletionsRequest::from_responses(request);
        let body = serde_json::to_value(&chat_request)
            .map_err(|err| ApiError::Stream(format!("failed to encode chat request: {err}")))?;
        // Write the full request JSON to a file for debugging.
        if let Ok(json_str) = serde_json::to_string_pretty(&chat_request) {
            let _ = std::fs::write("/tmp/codex-chat-request.json", &json_str);
            // Also log just the reasoning fields
            let mut reasoning_report = String::new();
            for (i, msg) in chat_request.messages.iter().enumerate() {
                if msg.role == "assistant" {
                    reasoning_report.push_str(&format!(
                        "msg[{}] role={} has_content={} has_reasoning={} has_tool_calls={} reasoning_len={:?}
",
                        i, msg.role,
                        msg.content.is_some(),
                        msg.reasoning_content.is_some(),
                        msg.tool_calls.is_some(),
                        msg.reasoning_content.as_ref().map(|r| r.len()),
                    ));
                }
            }
            let _ = std::fs::write("/tmp/codex-reasoning-report.txt", &reasoning_report);
        }
        let stream_response = self
            .session
            .stream_with(
                Method::POST,
                "chat/completions",
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                },
            )
            .await?;

        Ok(spawn_chat_completions_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
        ))
    }
}

impl ChatCompletionsRequest {
    fn from_responses(request: ResponsesApiRequest) -> Self {
        let input_len = request.input.len();
        let messages = chat_messages_from_response_items(request.instructions, request.input);
        let msg_count = messages.len();
        let with_reasoning = messages.iter().filter(|m| m.reasoning_content.is_some()).count();
        tracing::debug!(
            total_messages = msg_count,
            messages_with_reasoning = with_reasoning,
            input_items = input_len,
            "ChatCompletionsRequest built"
        );
        Self {
            model: request.model,
            messages,
            tools: chat_tools_from_responses_tools(request.tools),
            tool_choice: request.tool_choice,
            parallel_tool_calls: Some(request.parallel_tool_calls),
            stream: true,
            stream_options: ChatStreamOptions {
                include_usage: true,
            },
            reasoning_effort: request
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort),
            thinking: request
                .reasoning
                .as_ref()
                .map(|_| json!({"type": "enabled"})),
            service_tier: request.service_tier,
            prompt_cache_key: request.prompt_cache_key,
        }
    }
}

fn chat_messages_from_response_items(
    instructions: String,
    input: Vec<ResponseItem>,
) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    if !instructions.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(ChatMessageContent::Text(instructions)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let mut idx = 0;
    let mut last_reasoning: Option<String> = None;
    while idx < input.len() {
        match &input[idx] {
            ResponseItem::Reasoning { content, .. } => {
                // Capture reasoning text for the next Message or tool-call group.
                if let Some(text) = reasoning_items_to_text(content.as_ref()) {
                    last_reasoning = Some(text);
                }
            }
            ResponseItem::Message {
                role,
                content,
                ..
            } => {
                let mapped_role = chat_message_role(role);
                // Only assistant messages carry reasoning_content.
                let reasoning_content = if mapped_role == "assistant" {
                    // Reasoning may appear before OR after the Message in the item list.
                    // Try last_reasoning first (reasoning before message), then peek forward.
                    let mut rc = last_reasoning.take();
                    if rc.is_none()
                        && idx + 1 < input.len()
                    {
                        if let ResponseItem::Reasoning { content, .. } = &input[idx + 1] {
                            rc = reasoning_items_to_text(content.as_ref());
                            if rc.is_some() {
                                idx += 1; // consume the next Reasoning item
                            }
                        }
                    }
                    rc
                } else {
                    None
                };
                messages.push(ChatMessage {
                    role: mapped_role.to_string(),
                    content: Some(chat_message_content_from_content_items(content.clone())),
                    reasoning_content,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            ResponseItem::FunctionCall { .. } | ResponseItem::CustomToolCall { .. } => {
                let (next_idx, message_group) =
                    collect_complete_tool_call_group(&input, idx, last_reasoning.take());
                messages.extend(message_group);
                idx = next_idx;
                continue;
            }
            ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. } => {
            }
            ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::CompactionTrigger
            | ResponseItem::Other => {}
        }
        idx += 1;
    }

    messages
}

/// Extracts the text content from a Reasoning item if present at the given index.

/// Collects reasoning text from `ReasoningItemContent` items.
fn reasoning_items_to_text(content: Option<&Vec<ReasoningItemContent>>) -> Option<String> {
    let items = content?;
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            ReasoningItemContent::ReasoningText { text } | ReasoningItemContent::Text { text } => {
                Some(text.as_str())
            }
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn chat_message_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        other => other,
    }
}

fn collect_complete_tool_call_group(
    input: &[ResponseItem],
    start_idx: usize,
    preceding_reasoning: Option<String>,
) -> (usize, Vec<ChatMessage>) {
    let mut idx = start_idx;
    let mut calls = Vec::new();
    let mut call_ids = Vec::new();
    let mut reasoning_content: Vec<String> = preceding_reasoning
        .into_iter()
        .collect();
    while idx < input.len() {
        match &input[idx] {
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                call_ids.push(call_id.clone());
                calls.push(ChatToolCall {
                    id: call_id.clone(),
                    kind: "function",
                    function: ChatFunctionCall {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                });
                idx += 1;
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input: tool_input,
                ..
            } => {
                call_ids.push(call_id.clone());
                calls.push(ChatToolCall {
                    id: call_id.clone(),
                    kind: "function",
                    function: ChatFunctionCall {
                        name: name.clone(),
                        arguments: tool_input.clone(),
                    },
                });
                idx += 1;
            }
            ResponseItem::Reasoning { content, .. } => {
                // Collect reasoning text. In v0.132.0, reasoning is a separate
                // ResponseItem variant, not a field on FunctionCall/CustomToolCall.
                if let Some(text) = reasoning_items_to_text(content.as_ref()) {
                    reasoning_content.push(text);
                }
                idx += 1;
            }
            _ => break,
        }
    }

    let mut scan_idx = idx;
    let mut outputs = HashMap::new();
    while scan_idx < input.len() && outputs.len() < call_ids.len() {
        match &input[scan_idx] {
            ResponseItem::FunctionCallOutput { call_id, output }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } if call_ids.contains(call_id) => {
                outputs.insert(call_id.clone(), output.clone());
                scan_idx += 1;
            }
            ResponseItem::Reasoning { .. } => {
                scan_idx += 1;
            }
            _ => break,
        }
    }

    if outputs.len() != call_ids.len() {
        return (scan_idx, Vec::new());
    }

    let mut messages = vec![ChatMessage {
        role: "assistant".to_string(),
        content: None,
        reasoning_content: (!reasoning_content.is_empty()).then(|| reasoning_content.join("\n")),
        tool_calls: Some(calls),
        tool_call_id: None,
    }];
    for call_id in call_ids {
        if let Some(output) = outputs.remove(&call_id) {
            messages.push(tool_message(call_id, output));
        }
    }
    (scan_idx, messages)
}

fn chat_message_content_from_content_items(content: Vec<ContentItem>) -> ChatMessageContent {
    let mut text_parts = Vec::new();
    let mut parts = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                text_parts.push(text.clone());
                parts.push(ChatContentPart::Text { text });
            }
            ContentItem::InputImage { image_url, detail } => {
                parts.push(ChatContentPart::ImageUrl {
                    image_url: ChatImageUrl {
                        url: image_url,
                        detail: detail.map(chat_image_detail),
                    },
                });
            }
        }
    }

    if parts
        .iter()
        .all(|part| matches!(part, ChatContentPart::Text { .. }))
    {
        ChatMessageContent::Text(text_parts.join("\n"))
    } else {
        ChatMessageContent::Parts(parts)
    }
}

fn chat_image_detail(detail: ImageDetail) -> String {
    match detail {
        ImageDetail::Original => "original",
        ImageDetail::High => "high",
    }
    .to_string()
}

fn tool_message(call_id: String, output: FunctionCallOutputPayload) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(ChatMessageContent::Text(function_output_text(output))),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some(call_id),
    }
}

fn function_output_text(output: FunctionCallOutputPayload) -> String {
    match output.body {
        FunctionCallOutputBody::Text(text) => text,
        FunctionCallOutputBody::ContentItems(items) => serde_json::to_string(&items)
            .unwrap_or_else(|err| format!("failed to serialize tool output: {err}")),
    }
}

fn chat_tools_from_responses_tools(tools: Vec<Value>) -> Vec<Value> {
    tools
        .into_iter()
        .filter_map(|tool| match tool.get("type").and_then(Value::as_str) {
            Some("function") => Some(json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").cloned().unwrap_or(Value::Null),
                    "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                }
            })),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ReasoningItemContent;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn reasoning_before_function_call_is_passed_as_reasoning_content() {
        // Simulate: [Reasoning, FunctionCall, FunctionCallOutput]
        let items = vec![
            ResponseItem::Reasoning {
                id: "r1".into(),
                summary: vec![],
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "Let me check the time".into(),
                }]),
                encrypted_content: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".into(),
                namespace: None,
                arguments: r#"{"cmd":"date"}"#.into(),
                call_id: "call_1".into(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call_1".into(),
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("2026-05-21 13:38:39 HKT".into()),
                    success: None,
                },
            },
        ];

        let messages = chat_messages_from_response_items(String::new(), items);

        // Should produce: tool role assistant message + tool result message
        assert_eq!(messages.len(), 2);

        let assistant = &messages[0];
        assert_eq!(assistant.role, "assistant");
        assert!(
            assistant.reasoning_content.is_some(),
            "assistant message should have reasoning_content"
        );
        assert_eq!(
            assistant.reasoning_content.as_deref(),
            Some("Let me check the time"),
        );
    }

    #[test]
    fn reasoning_after_message_is_passed_as_reasoning_content() {
        // Simulate the SSE parser output: [Message, Reasoning]
        let items = vec![
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: "It is 13:38 HKT".into(),
                }],
                phase: None,
            },
            ResponseItem::Reasoning {
                id: "r1".into(),
                summary: vec![],
                content: Some(vec![ReasoningItemContent::ReasoningText {
                    text: "The time is 13:38".into(),
                }]),
                encrypted_content: None,
            },
        ];

        let messages = chat_messages_from_response_items(String::new(), items);

        assert_eq!(messages.len(), 1);

        let assistant = &messages[0];
        assert_eq!(assistant.role, "assistant");
        assert!(
            assistant.reasoning_content.is_some(),
            "assistant message should have reasoning_content"
        );
        assert_eq!(
            assistant.reasoning_content.as_deref(),
            Some("The time is 13:38"),
        );
    }
}

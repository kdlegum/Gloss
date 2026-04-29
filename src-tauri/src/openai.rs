use crate::llm::{
    build_chunk_body_prompt, build_chunk_chat_prompt, build_chunk_rewrite_prompt,
    build_instruction_markdown_prompt, build_past_paper_document_chunk_prompt, build_prompt,
    chunk_body_schema, chunk_groups_schema, chunk_rewrite_schema, instruction_markdown_schema,
    map_reqwest_error, merge_stream_text, parse_chunk_body, parse_chunk_rewrite, parse_chunks,
    parse_instruction_markdown_result, parse_past_paper_document_chunk_result,
    past_paper_document_chunk_schema, stream_sse_events, BlockForPrompt, ChunkBodyPrompt,
    ChunkBodyResult, ChunkChatMessage, ChunkChatPrompt, ChunkRewritePrompt, ChunkRewriteResult,
    GroupedChunk, LlmError, PastPaperBlockForPrompt, PastPaperDocumentResult,
    PastPaperInstructionMarkdown,
};
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::openai";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_MODEL: &str = "gpt-5.4-mini";

#[derive(Clone)]
pub struct OpenAiClient {
    api_key: Option<String>,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiClient {
    pub fn with_api_key_and_model(api_key: Option<String>, model_override: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let model = normalize_model(model_override)
            .or_else(|| normalize_model(std::env::var("OPENAI_MODEL").ok()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            api_key: normalize_api_key(api_key).or_else(|| std::env::var("OPENAI_API_KEY").ok()),
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            client,
        }
    }

    pub async fn health_check(&self) -> bool {
        let healthy = matches!(self.api_key.as_deref(), Some(key) if !key.trim().is_empty());
        info!(
            target: LOG_TARGET,
            "health_check model={} healthy={}",
            self.model,
            healthy
        );
        healthy
    }

    pub async fn chunk_blocks(
        &self,
        blocks: &[BlockForPrompt<'_>],
    ) -> Result<Vec<GroupedChunk>, LlmError> {
        if blocks.is_empty() {
            return Ok(Vec::new());
        }

        info!(
            target: LOG_TARGET,
            "chunk_blocks model={} block_count={}",
            self.model,
            blocks.len()
        );

        let prompt = build_prompt(blocks);
        let mut request = json!({
            "model": self.model,
            "store": false,
            "input": prompt,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "chunk_groups",
                    "schema": chunk_groups_schema(),
                    "strict": true
                }
            }
        });
        if model_supports_temperature(&self.model) {
            if let Some(obj) = request.as_object_mut() {
                obj.insert("temperature".to_string(), json!(0.1));
            }
        } else {
            debug!(
                target: LOG_TARGET,
                "chunk_blocks model={} does not support temperature; omitting parameter",
                self.model
            );
        }
        let value = self.send_request(request).await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai chunking",
            output_text.len()
        );

        parse_chunks(&output_text, LOG_TARGET)
    }

    pub async fn extract_instruction_markdown(
        &self,
        images: &[String],
        blocks: &[PastPaperBlockForPrompt],
    ) -> Result<PastPaperInstructionMarkdown, LlmError> {
        info!(
            target: LOG_TARGET,
            "extract_instruction_markdown model={} images={} blocks={}",
            self.model,
            images.len(),
            blocks.len()
        );

        let prompt = build_instruction_markdown_prompt(blocks);
        let mut user_content: Vec<serde_json::Value> = Vec::new();
        user_content.push(json!({
            "type": "input_text",
            "text": "Extract the exam instructions from these page images."
        }));
        for image_b64 in images {
            user_content.push(json!({
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{}", image_b64),
                "detail": "high"
            }));
        }

        let value = self
            .send_request(json!({
                "model": self.model,
                "store": false,
                "input": [
                    { "role": "developer", "content": [{ "type": "input_text", "text": prompt }] },
                    { "role": "user", "content": user_content }
                ],
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "instruction_markdown",
                        "schema": instruction_markdown_schema(),
                        "strict": true
                    }
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai instruction markdown extraction",
            output_text.len()
        );

        parse_instruction_markdown_result(&output_text, LOG_TARGET)
    }

    pub async fn chunk_past_paper_document(
        &self,
        images: &[String],
        blocks: &[PastPaperBlockForPrompt],
        instruction_markdown: &str,
    ) -> Result<PastPaperDocumentResult, LlmError> {
        info!(
            target: LOG_TARGET,
            "chunk_past_paper_document model={} images={} blocks={}",
            self.model,
            images.len(),
            blocks.len()
        );

        let prompt = build_past_paper_document_chunk_prompt(blocks, instruction_markdown);
        let mut user_content: Vec<serde_json::Value> = Vec::new();
        user_content.push(json!({
            "type": "input_text",
            "text": "Identify and transcribe all questions from these exam pages."
        }));
        for image_b64 in images {
            user_content.push(json!({
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{}", image_b64),
                "detail": "high"
            }));
        }

        let value = self
            .send_request(json!({
                "model": self.model,
                "store": false,
                "input": [
                    { "role": "developer", "content": [{ "type": "input_text", "text": prompt }] },
                    { "role": "user", "content": user_content }
                ],
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "past_paper_chunks",
                        "schema": past_paper_document_chunk_schema(),
                        "strict": true
                    }
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai past-paper document chunking",
            output_text.len()
        );

        parse_past_paper_document_chunk_result(&output_text, LOG_TARGET)
    }

    pub async fn transcribe_chunk_body(
        &self,
        chunk: &ChunkBodyPrompt<'_>,
        image_base64: &str,
    ) -> Result<ChunkBodyResult, LlmError> {
        let prompt = build_chunk_body_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "transcribe_chunk_body model={} chunk_type={} title_present={} subject_present={}",
            self.model,
            chunk.chunk_type,
            chunk.title.is_some(),
            chunk.subject.is_some()
        );

        let data_url = format!("data:image/png;base64,{image_base64}");
        let value = self
            .send_request(json!({
                "model": self.model,
                "store": false,
                "input": [
                    {
                        "role": "developer",
                        "content": [
                            {
                                "type": "input_text",
                                "text": prompt
                            }
                        ]
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "Transcribe this cropped mathematics chunk."
                            },
                            {
                                "type": "input_image",
                                "image_url": data_url,
                                "detail": "high"
                            }
                        ]
                    }
                ],
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "chunk_body",
                        "schema": chunk_body_schema(),
                        "strict": true
                    }
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai chunk transcription",
            output_text.len()
        );

        parse_chunk_body(&output_text, LOG_TARGET)
    }

    pub async fn stream_chunk_chat<F, C>(
        &self,
        chunk: &ChunkChatPrompt<'_>,
        history: &[ChunkChatMessage],
        mut on_delta: F,
        should_cancel: C,
    ) -> Result<String, LlmError>
    where
        F: FnMut(&str) -> Result<(), LlmError>,
        C: Fn() -> bool,
    {
        let prompt = build_chunk_chat_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "stream_chunk_chat model={} chunk_type={} history_len={}",
            self.model,
            chunk.chunk_type,
            history.len()
        );

        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("OPENAI_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(&json!({
                "model": self.model,
                "store": false,
                "stream": true,
                "input": build_chat_input(&prompt, history),
            }))
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .map_err(|e| LlmError::Http(e.to_string()))?;
            return Err(LlmError::Http(format!("status {}: {}", status, body)));
        }

        let mut output = String::new();
        stream_sse_events(
            resp,
            |data| {
                if data == "[DONE]" {
                    return Ok(());
                }

                let value: Value =
                    serde_json::from_str(data).map_err(|e| LlmError::Parse(e.to_string()))?;
                let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");

                if event_type == "response.output_text.delta" {
                    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                        if let Some(renderable) = merge_stream_text(&mut output, delta) {
                            on_delta(&renderable)?;
                        }
                    }
                    return Ok(());
                }

                if event_type == "response.completed" && output.trim().is_empty() {
                    let response_value = value.get("response").unwrap_or(&value);
                    if let Some(text) = extract_output_text(response_value) {
                        if let Some(renderable) = merge_stream_text(&mut output, &text) {
                            on_delta(&renderable)?;
                        }
                    }
                }

                Ok(())
            },
            should_cancel,
        )
        .await?;

        let trimmed = output.trim().to_string();
        if trimmed.is_empty() {
            return Err(LlmError::Parse(
                "empty streamed openai response".to_string(),
            ));
        }
        Ok(trimmed)
    }

    pub async fn rewrite_chunk_with_prompt(
        &self,
        chunk: &ChunkRewritePrompt<'_>,
    ) -> Result<ChunkRewriteResult, LlmError> {
        let prompt = build_chunk_rewrite_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "rewrite_chunk_with_prompt model={} chunk_type={} title_present={} subject_present={}",
            self.model,
            chunk.chunk_type,
            chunk.title.is_some(),
            chunk.subject.is_some()
        );

        let images = chunk.image_base64_list.iter()
            .map(|b64| b64.trim())
            .filter(|b64| !b64.is_empty())
            .collect::<Vec<_>>();
        let input: Value = if images.is_empty() {
            json!(prompt)
        } else {
            let mut content_items = vec![json!({
                "type": "input_text",
                "text": prompt
            })];
            for image_base64 in &images {
                content_items.push(json!({
                    "type": "input_image",
                    "image_url": format!("data:image/png;base64,{image_base64}"),
                    "detail": "high"
                }));
            }
            json!([{
                "role": "user",
                "content": content_items
            }])
        };
        let mut request = json!({
            "model": self.model,
            "store": false,
            "input": input,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "chunk_rewrite",
                    "schema": chunk_rewrite_schema(),
                    "strict": true
                }
            }
        });
        if model_supports_temperature(&self.model) {
            if let Some(obj) = request.as_object_mut() {
                obj.insert("temperature".to_string(), json!(0.1));
            }
        }
        let value = self.send_request(request).await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai chunk rewrite",
            output_text.len()
        );

        parse_chunk_rewrite(&output_text, LOG_TARGET)
    }

    async fn send_request(&self, request: Value) -> Result<Value, LlmError> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("OPENAI_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Http(format!("status {}: {}", status, body)));
        }

        serde_json::from_str(&body).map_err(|e| LlmError::Parse(e.to_string()))
    }
}

fn normalize_api_key(api_key: Option<String>) -> Option<String> {
    api_key
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_model(model: Option<String>) -> Option<String> {
    model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn model_supports_temperature(model: &str) -> bool {
    !model.trim().to_ascii_lowercase().starts_with("gpt-5-nano")
}

fn build_chat_input(prompt: &str, history: &[ChunkChatMessage]) -> Value {
    let mut messages = vec![json!({
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": prompt
        }]
    })];

    for message in history {
        let role = message.role.trim();
        if role == "assistant" {
            let content = message.content.trim();
            if content.is_empty() {
                continue;
            }
            messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": content
                }]
            }));
            continue;
        }

        let mut content_items: Vec<Value> = Vec::new();
        if !message.content.trim().is_empty() {
            content_items.push(json!({
                "type": "input_text",
                "text": message.content
            }));
        }
        for image_base64 in message.image_items() {
            content_items.push(json!({
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{image_base64}"),
                "detail": "high"
            }));
        }
        if content_items.is_empty() {
            continue;
        }
        messages.push(json!({
            "role": "user",
            "content": content_items
        }));
    }

    Value::Array(messages)
}

fn extract_output_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let mut combined = String::new();
    for item in value.get("output")?.as_array()? {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for entry in content {
            if entry.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            let Some(text) = entry.get("text").and_then(Value::as_str) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(trimmed);
        }
    }

    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

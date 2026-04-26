use crate::llm::{
    build_chunk_body_prompt, build_chunk_chat_prompt, build_chunk_rewrite_prompt, build_prompt,
    chunk_body_schema, chunk_groups_schema, chunk_rewrite_schema, map_reqwest_error,
    merge_stream_text, parse_chunk_body, parse_chunk_rewrite, parse_chunks, stream_sse_events,
    BlockForPrompt, ChunkBodyPrompt, ChunkBodyResult, ChunkChatMessage, ChunkChatPrompt,
    ChunkRewritePrompt, ChunkRewriteResult, GroupedChunk, LlmError,
};
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::gemini";
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const DEFAULT_MODEL: &str = "gemini-2.5-flash";

#[derive(Clone)]
pub struct GeminiClient {
    api_key: Option<String>,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl GeminiClient {
    pub fn with_api_key_and_model(api_key: Option<String>, model_override: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let model = normalize_model(model_override)
            .or_else(|| normalize_model(std::env::var("GEMINI_MODEL").ok()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            api_key: normalize_api_key(api_key).or_else(|| std::env::var("GEMINI_API_KEY").ok()),
            base_url: std::env::var("GEMINI_API_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
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
        let value = self
            .send_request(json!({
                "contents": [{
                    "role": "user",
                    "parts": [{
                        "text": prompt
                    }]
                }],
                "generationConfig": {
                    "temperature": 0.1,
                    "responseMimeType": "application/json",
                    "responseJsonSchema": chunk_groups_schema()
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from gemini chunking",
            output_text.len()
        );

        parse_chunks(&output_text, LOG_TARGET)
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

        let value = self
            .send_request(json!({
                "contents": [{
                    "role": "user",
                    "parts": [
                        {
                            "inline_data": {
                                "mime_type": "image/png",
                                "data": image_base64
                            }
                        },
                        {
                            "text": prompt
                        }
                    ]
                }],
                "generationConfig": {
                    "temperature": 0.1,
                    "responseMimeType": "application/json",
                    "responseJsonSchema": chunk_body_schema()
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from gemini chunk transcription",
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
            .ok_or_else(|| LlmError::Config("GEMINI_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(self.stream_generate_content_url())
            .header("x-goog-api-key", api_key)
            .json(&build_chat_request(&prompt, history))
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
                let value: Value =
                    serde_json::from_str(data).map_err(|e| LlmError::Parse(e.to_string()))?;
                if let Some(text) = extract_output_text(&value) {
                    if let Some(renderable) = merge_stream_text(&mut output, &text) {
                        on_delta(&renderable)?;
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
                "empty streamed gemini response".to_string(),
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

        let value = self
            .send_request(json!({
                "contents": [{
                    "role": "user",
                    "parts": [{
                        "text": prompt
                    }]
                }],
                "generationConfig": {
                    "temperature": 0.1,
                    "responseMimeType": "application/json",
                    "responseJsonSchema": chunk_rewrite_schema()
                }
            }))
            .await?;
        let output_text =
            extract_output_text(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from gemini chunk rewrite",
            output_text.len()
        );

        parse_chunk_rewrite(&output_text, LOG_TARGET)
    }

    async fn send_request(&self, request: Value) -> Result<Value, LlmError> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("GEMINI_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(self.generate_content_url())
            .header("x-goog-api-key", api_key)
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

    fn generate_content_url(&self) -> String {
        format!("{}/{}:generateContent", self.base_url, self.model)
    }

    fn stream_generate_content_url(&self) -> String {
        format!(
            "{}/{}:streamGenerateContent?alt=sse",
            self.base_url, self.model
        )
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

fn build_chat_request(prompt: &str, history: &[ChunkChatMessage]) -> Value {
    let mut contents: Vec<Value> = Vec::with_capacity(history.len());
    for message in history {
        let role = if message.role == "assistant" {
            "model"
        } else {
            "user"
        };
        let mut parts: Vec<Value> = Vec::new();
        if !message.content.trim().is_empty() {
            parts.push(json!({
                "text": message.content
            }));
        }
        if role == "user" {
            if let Some(image_base64) = message
                .image_base64
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                parts.push(json!({
                    "inline_data": {
                        "mime_type": "image/png",
                        "data": image_base64
                    }
                }));
            }
        }
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({
            "role": role,
            "parts": parts
        }));
    }

    json!({
        "systemInstruction": {
            "parts": [{
                "text": prompt
            }]
        },
        "contents": contents,
        "generationConfig": {
            "temperature": 0.1
        }
    })
}

fn extract_output_text(value: &Value) -> Option<String> {
    let mut combined = String::new();
    for candidate in value.get("candidates")?.as_array()? {
        let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for part in parts {
            let Some(text) = part.get("text").and_then(Value::as_str) else {
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

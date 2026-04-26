use crate::llm::{
    build_chunk_chat_prompt, build_chunk_rewrite_prompt, build_prompt, map_reqwest_error,
    parse_chunk_rewrite, parse_chunks, stream_sse_events, BlockForPrompt, ChunkChatMessage,
    ChunkChatPrompt, ChunkRewritePrompt, ChunkRewriteResult, GroupedChunk, LlmError,
};
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::deepseek";
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

#[derive(Clone)]
pub struct DeepSeekClient {
    api_key: Option<String>,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl DeepSeekClient {
    pub fn with_api_key_and_model(api_key: Option<String>, model_override: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let model = normalize_model(model_override)
            .or_else(|| normalize_model(std::env::var("DEEPSEEK_MODEL").ok()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            api_key: normalize_api_key(api_key).or_else(|| std::env::var("DEEPSEEK_API_KEY").ok()),
            base_url: std::env::var("DEEPSEEK_API_BASE_URL")
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
                "model": self.model,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }],
                "temperature": 0.1,
                "response_format": {
                    "type": "json_object"
                }
            }))
            .await?;
        let output_text =
            extract_message_content(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from deepseek chunking",
            output_text.len()
        );

        parse_chunks(&output_text, LOG_TARGET)
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
            .ok_or_else(|| LlmError::Config("DEEPSEEK_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth(api_key)
            .json(&json!({
                "model": self.model,
                "messages": build_chat_messages(&prompt, history),
                "temperature": 0.1,
                "stream": true
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
                if let Some(delta) = value
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                {
                    if !delta.is_empty() {
                        output.push_str(delta);
                        on_delta(delta)?;
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
                "empty streamed deepseek response".to_string(),
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
                "model": self.model,
                "messages": [{
                    "role": "user",
                    "content": prompt
                }],
                "temperature": 0.1,
                "response_format": {
                    "type": "json_object"
                }
            }))
            .await?;
        let output_text =
            extract_message_content(&value).ok_or_else(|| LlmError::Parse(value.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from deepseek chunk rewrite",
            output_text.len()
        );

        parse_chunk_rewrite(&output_text, LOG_TARGET)
    }

    async fn send_request(&self, request: Value) -> Result<Value, LlmError> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("DEEPSEEK_API_KEY not set".to_string()))?;

        let resp = self
            .client
            .post(self.chat_completions_url())
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

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
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

fn build_chat_messages(prompt: &str, history: &[ChunkChatMessage]) -> Value {
    let mut messages = vec![json!({
        "role": "system",
        "content": prompt
    })];

    for message in history {
        let role = if message.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        messages.push(json!({
            "role": role,
            "content": message.content
        }));
    }

    Value::Array(messages)
}

fn extract_message_content(value: &Value) -> Option<String> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(str::to_string)
}

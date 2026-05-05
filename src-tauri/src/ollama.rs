use crate::llm::{
    build_chunk_body_prompt, build_chunk_chat_prompt, build_chunk_rewrite_prompt, build_prompt,
    chunk_body_schema, chunk_groups_schema, chunk_rewrite_schema, map_reqwest_error,
    parse_chunk_body, parse_chunk_rewrite, parse_chunks, stream_sse_events, BlockForPrompt,
    ChunkBodyPrompt, ChunkBodyResult, ChunkChatMessage, ChunkChatPrompt, ChunkRewritePrompt,
    ChunkRewriteResult, GroupedChunk, LlmError,
};
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::ollama";
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";

#[derive(Clone)]
pub struct OllamaClient {
    base_url: String,
    model: Option<String>,
    vision_model: Option<String>,
    client: reqwest::Client,
}

impl OllamaClient {
    pub fn with_text_model(model_override: Option<String>) -> Self {
        Self::with_models(model_override, None)
    }

    pub fn with_vision_model(model_override: Option<String>) -> Self {
        Self::with_models(None, model_override)
    }

    pub fn with_models(
        model_override: Option<String>,
        vision_model_override: Option<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let configured_model = normalize_model(model_override)
            .or_else(|| normalize_model(std::env::var("LLAMA_SERVER_MODEL").ok()));
        let configured_vision_model = normalize_model(vision_model_override)
            .or_else(|| normalize_model(std::env::var("LLAMA_SERVER_VISION_MODEL").ok()));
        Self {
            base_url: std::env::var("LLAMA_SERVER_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            model: configured_model,
            vision_model: configured_vision_model,
            client,
        }
    }

    pub async fn health_check(&self) -> bool {
        let url = self.models_url();
        let fut = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .bearer_auth("no-key")
            .send();
        let healthy = match fut.await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(value) => first_model_id(&value).is_some(),
                Err(_) => false,
            },
            _ => false,
        };
        info!(
            target: LOG_TARGET,
            "health_check base_url={} healthy={} configured_model={:?} configured_vision_model={:?}",
            self.base_url,
            healthy,
            self.model,
            self.vision_model
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
        let model = self.resolve_text_model().await?;
        info!(
            target: LOG_TARGET,
            "chunk_blocks base_url={} model={} block_count={}",
            self.base_url,
            model,
            blocks.len()
        );

        let prompt = build_prompt(blocks);
        let output = self
            .chat_completion(
                &model,
                json!([
                    {
                        "role": "user",
                        "content": prompt
                    }
                ]),
                json!({
                    "type": "json_schema",
                    "schema": chunk_groups_schema()
                }),
            )
            .await?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from llama-server chunking",
            output.len()
        );

        parse_chunks(&output, LOG_TARGET)
    }

    pub async fn transcribe_chunk_body(
        &self,
        chunk: &ChunkBodyPrompt<'_>,
        image_base64: &str,
    ) -> Result<ChunkBodyResult, LlmError> {
        let model = self.resolve_vision_model().await?;
        let prompt = build_chunk_body_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "transcribe_chunk_body base_url={} model={} chunk_type={} title_present={} subject_present={}",
            self.base_url,
            model,
            chunk.chunk_type,
            chunk.title.is_some(),
            chunk.subject.is_some()
        );

        let data_url = format!("data:image/png;base64,{image_base64}");
        let output = self
            .chat_completion(
                &model,
                json!([
                    {
                        "role": "system",
                        "content": prompt
                    },
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": "Transcribe this cropped mathematics chunk."
                            },
                            {
                                "type": "image_url",
                                "image_url": {
                                    "url": data_url
                                }
                            }
                        ]
                    }
                ]),
                json!({
                    "type": "json_schema",
                    "schema": chunk_body_schema()
                }),
            )
            .await?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from llama-server chunk transcription",
            output.len()
        );

        parse_chunk_body(&output, LOG_TARGET)
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
        let model = self.resolve_text_model().await?;
        let prompt = build_chunk_chat_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "stream_chunk_chat base_url={} model={} chunk_type={} history_len={}",
            self.base_url,
            model,
            chunk.chunk_type,
            history.len()
        );

        let resp = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth("no-key")
            .json(&json!({
                "model": model,
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
                "empty streamed ollama response".to_string(),
            ));
        }
        Ok(trimmed)
    }

    pub async fn rewrite_chunk_with_prompt(
        &self,
        chunk: &ChunkRewritePrompt<'_>,
    ) -> Result<ChunkRewriteResult, LlmError> {
        let model = self.resolve_text_model().await?;
        let prompt = build_chunk_rewrite_prompt(chunk);
        info!(
            target: LOG_TARGET,
            "rewrite_chunk_with_prompt base_url={} model={} chunk_type={} title_present={} subject_present={}",
            self.base_url,
            model,
            chunk.chunk_type,
            chunk.title.is_some(),
            chunk.subject.is_some()
        );

        let output = self
            .chat_completion(
                &model,
                json!([{
                    "role": "user",
                    "content": prompt
                }]),
                json!({
                    "type": "json_schema",
                    "schema": chunk_rewrite_schema()
                }),
            )
            .await?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from llama-server chunk rewrite",
            output.len()
        );

        parse_chunk_rewrite(&output, LOG_TARGET, chunk.body_format)
    }

    async fn resolve_text_model(&self) -> Result<String, LlmError> {
        if let Some(model) = &self.model {
            return Ok(model.clone());
        }
        self.discover_model().await
    }

    async fn resolve_vision_model(&self) -> Result<String, LlmError> {
        if let Some(model) = &self.vision_model {
            return Ok(model.clone());
        }
        self.resolve_text_model().await
    }

    async fn discover_model(&self) -> Result<String, LlmError> {
        let resp = self
            .client
            .get(self.models_url())
            .bearer_auth("no-key")
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
        let value: Value =
            serde_json::from_str(&body).map_err(|e| LlmError::Parse(e.to_string()))?;
        first_model_id(&value)
            .map(str::to_string)
            .ok_or_else(|| LlmError::Config("llama-server returned no models".to_string()))
    }

    async fn chat_completion(
        &self,
        model: &str,
        messages: Value,
        response_format: Value,
    ) -> Result<String, LlmError> {
        let resp = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth("no-key")
            .json(&json!({
                "model": model,
                "messages": messages,
                "temperature": 0.1,
                "response_format": response_format
            }))
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

        let value: Value =
            serde_json::from_str(&body).map_err(|e| LlmError::Parse(e.to_string()))?;
        value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(|content| content.trim().to_string())
            .filter(|content| !content.is_empty())
            .ok_or_else(|| LlmError::Parse(body))
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
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
        let mut entry = json!({
            "role": role,
            "content": message.content
        });
        if role == "user" {
            let images = message.image_items();
            if !images.is_empty() {
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("images".to_string(), json!(images));
                }
            }
        }
        messages.push(entry);
    }

    Value::Array(messages)
}

fn first_model_id(value: &Value) -> Option<&str> {
    value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|models| models.first())
        .and_then(|model| model.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

fn normalize_model(model: Option<String>) -> Option<String> {
    model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

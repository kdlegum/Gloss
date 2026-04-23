use crate::llm::{build_prompt, parse_chunks, BlockForPrompt, GroupedChunk, LlmError};
use log::{debug, info};
use serde::Serialize;
use serde_json::Value;
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
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        Self {
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
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
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("OPENAI_API_KEY not set".to_string()))?;

        info!(
            target: LOG_TARGET,
            "chunk_blocks model={} block_count={}",
            self.model,
            blocks.len()
        );

        let prompt = build_prompt(blocks);
        let req = ResponseCreateRequest {
            model: &self.model,
            input: &prompt,
            store: false,
            temperature: 0.1,
            text: ResponseTextConfig {
                format: ResponseTextFormat { kind: "text" },
            },
        };

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    LlmError::Unavailable
                } else {
                    LlmError::Http(e.to_string())
                }
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| LlmError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Http(format!("status {}: {}", status, body)));
        }

        let value: Value = serde_json::from_str(&body).map_err(|e| LlmError::Parse(e.to_string()))?;
        let output_text = extract_output_text(&value).ok_or_else(|| LlmError::Parse(body.clone()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from openai",
            output_text.len()
        );

        parse_chunks(&output_text, LOG_TARGET)
    }
}

#[derive(Serialize)]
struct ResponseCreateRequest<'a> {
    model: &'a str,
    input: &'a str,
    store: bool,
    temperature: f32,
    text: ResponseTextConfig<'a>,
}

#[derive(Serialize)]
struct ResponseTextConfig<'a> {
    format: ResponseTextFormat<'a>,
}

#[derive(Serialize)]
struct ResponseTextFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
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

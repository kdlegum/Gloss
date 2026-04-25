use crate::llm::{map_reqwest_error, ChunkBodyPrompt, ChunkBodyResult, LlmError};
use log::{debug, info};
use serde_json::{json, Value};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::zai";
const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/paas/v4/layout_parsing";
const DEFAULT_MODEL: &str = "glm-ocr";

#[derive(Clone)]
pub struct ZaiClient {
    api_key: Option<String>,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl ZaiClient {
    pub fn with_api_key_and_model(api_key: Option<String>, model_override: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        let model = normalize_model(model_override)
            .or_else(|| normalize_model(std::env::var("ZAI_MODEL").ok()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            api_key: normalize_api_key(api_key).or_else(|| std::env::var("ZAI_API_KEY").ok()),
            base_url: std::env::var("ZAI_API_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            model,
            client,
        }
    }

    pub async fn transcribe_chunk_body(
        &self,
        chunk: &ChunkBodyPrompt<'_>,
        image_base64: &str,
    ) -> Result<ChunkBodyResult, LlmError> {
        info!(
            target: LOG_TARGET,
            "transcribe_chunk_body model={} chunk_type={} title_present={} subject_present={}",
            self.model,
            chunk.chunk_type,
            chunk.title.is_some(),
            chunk.subject.is_some()
        );

        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("ZAI_API_KEY not set".to_string()))?;

        let file_data = if image_base64.starts_with("data:") {
            image_base64.to_string()
        } else {
            format!("data:image/png;base64,{image_base64}")
        };

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(&json!({
                "model": self.model,
                "file": file_data
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
        let body_markdown = value
            .get("md_results")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .get("data")
                    .and_then(|nested| nested.get("md_results"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| LlmError::Parse(body))?
            .to_string();

        debug!(
            target: LOG_TARGET,
            "received {} response chars from zai transcription",
            body_markdown.len()
        );

        Ok(ChunkBodyResult { body_markdown })
    }

    pub async fn transcribe_block_image(&self, image_base64: &str) -> Result<String, LlmError> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| LlmError::Config("ZAI_API_KEY not set".to_string()))?;

        let file_data = if image_base64.starts_with("data:") {
            image_base64.to_string()
        } else {
            format!("data:image/png;base64,{image_base64}")
        };

        let resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(api_key)
            .json(&json!({
                "model": self.model,
                "file": file_data
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
            .get("md_results")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .get("data")
                    .and_then(|nested| nested.get("md_results"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| LlmError::Parse(body))
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

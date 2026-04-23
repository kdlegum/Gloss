use crate::llm::{build_prompt, parse_chunks, BlockForPrompt, GroupedChunk, LlmError};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const LOG_TARGET: &str = "gloss_lib::ollama";
const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "gemma4:latest";

#[derive(Clone)]
pub struct OllamaClient {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("failed to build reqwest client");
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            client,
        }
    }

    /// Quick probe to see if the server is up. Short timeout so we don't stall
    /// the whole chunking job if Ollama is down.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/api/tags", self.base_url);
        let fut = self.client.get(&url).timeout(Duration::from_secs(2)).send();
        let healthy = matches!(fut.await, Ok(r) if r.status().is_success());
        info!(
            target: LOG_TARGET,
            "health_check base_url={} healthy={}",
            self.base_url,
            healthy
        );
        healthy
    }

    /// Ask Ollama to group text blocks into semantic chunks and classify each.
    /// Blocks are passed in reading order; the LLM returns partitions over their ids.
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
        let req = GenerateRequest {
            model: &self.model,
            prompt: &prompt,
            stream: false,
            format: "json",
            options: GenerateOptions { temperature: 0.1 },
        };

        let url = format!("{}/api/generate", self.base_url);
        let resp = self
            .client
            .post(&url)
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

        if !resp.status().is_success() {
            return Err(LlmError::Http(format!("status {}", resp.status())));
        }

        let body: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;
        debug!(
            target: LOG_TARGET,
            "received {} response chars from ollama",
            body.response.len()
        );

        parse_chunks(&body.response, LOG_TARGET)
    }
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    format: &'a str,
    options: GenerateOptions,
}

#[derive(Serialize)]
struct GenerateOptions {
    temperature: f32,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

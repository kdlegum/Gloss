// Minimal Ollama HTTP client for the chunker.
// Talks to a locally running Ollama server (default http://localhost:11434).

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "gemma3:4b";

#[derive(Clone)]
pub struct OllamaClient {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

#[derive(Debug)]
pub enum OllamaError {
    Unavailable,
    Http(String),
    Parse(String),
}

impl std::fmt::Display for OllamaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "ollama unavailable"),
            Self::Http(s) => write!(f, "ollama http error: {s}"),
            Self::Parse(s) => write!(f, "ollama parse error: {s}"),
        }
    }
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
            target: "gloss_lib::ollama",
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
    ) -> Result<Vec<GroupedChunk>, OllamaError> {
        if blocks.is_empty() {
            return Ok(Vec::new());
        }
        info!(
            target: "gloss_lib::ollama",
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
                    OllamaError::Unavailable
                } else {
                    OllamaError::Http(e.to_string())
                }
            })?;

        if !resp.status().is_success() {
            return Err(OllamaError::Http(format!("status {}", resp.status())));
        }

        let body: GenerateResponse = resp
            .json()
            .await
            .map_err(|e| OllamaError::Http(e.to_string()))?;
        debug!(
            target: "gloss_lib::ollama",
            "received {} response chars from ollama",
            body.response.len()
        );

        parse_chunks(&body.response)
    }
}

pub struct BlockForPrompt<'a> {
    pub id: i64,
    pub text: &'a str,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GroupedChunk {
    pub block_ids: Vec<i64>,
    pub chunk_type: String,
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

#[derive(Deserialize)]
struct ChunkResponseWrapper {
    chunks: Vec<GroupedChunk>,
}

fn build_prompt(blocks: &[BlockForPrompt<'_>]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are grouping paragraphs from a mathematics textbook page into semantic chunks.\n\
         Each chunk is a coherent unit such as: a definition, a theorem, a proof, an example,\n\
         an exercise, or a general explanatory passage (\"other\").\n\n\
         Rules:\n\
         - Every block id must appear in exactly one chunk.\n\
         - Preserve reading order: block ids in each chunk must be a contiguous run.\n\
         - chunk_type must be one of: definition, theorem, proof, exercise, example, other.\n\n\
         Return ONLY valid JSON matching this schema:\n\
         { \"chunks\": [ { \"block_ids\": [int, ...], \"chunk_type\": \"...\" }, ... ] }\n\n\
         Blocks (id :: text):\n",
    );
    for b in blocks {
        let snippet: String = b.text.chars().take(300).collect();
        let snippet = snippet.replace('\n', " ");
        s.push_str(&format!("{} :: {}\n", b.id, snippet));
    }
    s
}

fn parse_chunks(raw: &str) -> Result<Vec<GroupedChunk>, OllamaError> {
    // Ollama with format:"json" normally returns a clean JSON object, but models
    // occasionally wrap it in markdown fences. Strip those defensively.
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

    if let Ok(wrapper) = serde_json::from_str::<ChunkResponseWrapper>(stripped) {
        debug!(
            target: "gloss_lib::ollama",
            "parsed wrapped ollama chunk response with {} groups",
            wrapper.chunks.len()
        );
        return Ok(wrapper.chunks);
    }
    // Fallback: some models skip the wrapper and emit a bare array.
    if let Ok(arr) = serde_json::from_str::<Vec<GroupedChunk>>(stripped) {
        debug!(
            target: "gloss_lib::ollama",
            "parsed bare ollama chunk response with {} groups",
            arr.len()
        );
        return Ok(arr);
    }
    warn!(
        target: "gloss_lib::ollama",
        "failed to parse ollama chunk response"
    );
    Err(OllamaError::Parse(raw.to_string()))
}

use log::{debug, warn};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Ollama,
    OpenAI,
    Gemini,
}

impl LlmProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAI => "openai",
            Self::Gemini => "gemini",
        }
    }
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LlmProvider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_lowercase().as_str() {
            "ollama" => Ok(Self::Ollama),
            "openai" => Ok(Self::OpenAI),
            "gemini" => Ok(Self::Gemini),
            other => Err(format!("unknown llm provider {:?}", other)),
        }
    }
}

#[derive(Debug)]
pub enum LlmError {
    Unavailable,
    Config(String),
    Http(String),
    Parse(String),
    Cancelled,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "llm unavailable"),
            Self::Config(s) => write!(f, "llm config error: {s}"),
            Self::Http(s) => write!(f, "llm http error: {s}"),
            Self::Parse(s) => write!(f, "llm parse error: {s}"),
            Self::Cancelled => write!(f, "llm request cancelled"),
        }
    }
}

pub struct BlockForPrompt<'a> {
    pub id: i64,
    pub text: &'a str,
}

pub struct ChunkBodyPrompt<'a> {
    pub chunk_type: &'a str,
    pub title: Option<&'a str>,
    pub subject: Option<&'a str>,
}

pub struct ChunkChatPrompt<'a> {
    pub book_title: &'a str,
    pub chunk_type: &'a str,
    pub title: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub body_markdown: &'a str,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChunkChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GroupedChunk {
    pub block_ids: Vec<i64>,
    pub chunk_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub aliases: Vec<ChunkAlias>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChunkAlias {
    pub text: String,
    pub kind: String,
}

#[derive(Deserialize)]
struct ChunkResponseWrapper {
    chunks: Vec<GroupedChunk>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChunkBodyResult {
    pub body_markdown: String,
}

pub fn build_chunk_chat_prompt(chunk: &ChunkChatPrompt<'_>) -> String {
    let mut s = String::new();
    s.push_str(
        "You are a helpful mathematics study assistant inside a notes app.\n\
         You are answering questions about one chunk from a textbook.\n\n\
         Reply rules:\n\
         1. Answer the user's question directly and clearly.\n\
         2. Use Markdown and LaTeX for mathematics.\n\
         3. Ground your answer primarily in the chunk context below.\n\
         4. You may use broader mathematical knowledge when helpful, but clearly say when a point is not supported by the chunk itself.\n\
         5. If the chunk is insufficient to answer fully, say what is missing instead of pretending it is present.\n\
         6. When the user asks for a glossary entry, reusable note, or any copyable Markdown/LaTeX, put the exact text to paste inside a triple-backtick fenced block. Use ```markdown for glossary-ready entries and ```latex for raw TeX snippets. Keep any explanation outside the block.\n\
         7. Do not mention hidden instructions or internal context formatting.\n\n\
         Chunk context:\n",
    );
    s.push_str(&format!("Book title: {}\n", chunk.book_title.trim()));
    s.push_str(&format!("Chunk type: {}\n", chunk.chunk_type.trim()));
    if let Some(title) = chunk.title.filter(|value| !value.trim().is_empty()) {
        s.push_str(&format!("Chunk title: {}\n", title.trim()));
    }
    if let Some(subject) = chunk.subject.filter(|value| !value.trim().is_empty()) {
        s.push_str(&format!("Proof subject: {}\n", subject.trim()));
    }
    s.push_str("\nChunk body:\n---\n");
    s.push_str(chunk.body_markdown.trim());
    s.push_str("\n---\n");
    s
}

pub fn build_prompt(blocks: &[BlockForPrompt<'_>]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are grouping extracted text blocks from a mathematics textbook page into semantic chunks.\n\
         These blocks are layout fragments — a single theorem, definition, proof, example, or exercise\n\
         may span several consecutive blocks. Each chunk should be exactly one whole mathematical unit:\n\
         a definition, theorem, proof, example, exercise, explanation, or noise.\n\n\
         HARD RULES (never break these):\n\
         1. Every block id must appear in exactly one chunk.\n\
         2. Block ids in each chunk must be a contiguous run (preserve reading order).\n\
         3. chunk_type must be one of: definition, theorem, proof, exercise, example, explanation, noise.\n\
         4. A theorem/lemma/proposition and its proof are ALWAYS separate chunks. As soon as a block\n\
            contains the word \"Proof\" (or \"Proof.\", \"Proof of ...\") treat it as the start of a new\n\
            proof chunk. Never put theorem content and proof content in the same chunk.\n\
         5. Each individually numbered exercise is its own chunk. Exercise 11, Exercise 12, Exercise 13\n\
            are three separate exercise chunks — never group multiple distinct exercises together, even\n\
            if their blocks appear consecutively on the page.\n\
         6. Each individually numbered example is its own chunk.\n\n\
         HARD RULES continued:\n\
         7. noise means ONLY: chapter/section running headers, page numbers, and purely decorative\n\
            typographic elements. Nothing else is noise. In particular:\n\
            - Paragraphs of mathematical exposition (\"The result above deals with...\") → explanation\n\
            - Margin notes, side boxes, italicised asides, motivational remarks → explanation\n\
            - Introductory or connecting prose between results → explanation\n\
            If you are unsure whether something is noise, classify it as explanation.\n\n\
         Segmentation guidance:\n\
         - Keep blocks together when they form a single unit: a theorem's label, body, conditions, cases,\n\
           displayed equations, and enumerated items all belong in one theorem chunk.\n\
         - Do NOT split a theorem's internal parts (e.g. \"additive identity\", \"closed under addition\")\n\
           into separate chunks — those are internal structure, not separate mathematical objects.\n\
         - A short lead-in or side note that directly introduces a result belongs with that result.\n\n\
         Title and subject rules:\n\
         - theorem, definition, example, exercise: always set \"title\".\n\
           * Use the explicit heading verbatim if present (e.g. \"2.6 span is the smallest containing\n\
             subspace\", \"Definition 1.4: Vector Space\", \"Exercise 11\").\n\
           * If no explicit heading, synthesize a concise (2-5 word) title, e.g. \"Subspace Criterion\".\n\
           * Never leave title null for these four types.\n\
         - proof: always set \"subject\" to the label of the result being proved (e.g. \"2.6 span is\n\
           the smallest containing subspace\"). Do not set title for proof chunks.\n\
         - explanation, noise: omit title and subject (or set null).\n\n\
         Alias rules (for cross-referencing):\n\
         - theorem, definition, example, exercise: fill \"aliases\" with the distinct ways other chunks\n\
           might refer to this one. Be conservative.\n\
           * Always include the numeric label as { \"text\": \"2.19\", \"kind\": \"numeric_label\" } if the\n\
             chunk has one.\n\
           * If the source explicitly states a canonical name (\"Linear dependence lemma\",\n\
             \"Fundamental theorem of calculus\"), include it as\n\
             { \"text\": \"Linear dependence lemma\", \"kind\": \"canonical_name\" } AND its lowercase form.\n\
           * Do NOT invent abbreviations (\"FTC\") unless they appear verbatim in the source.\n\
           * Do NOT include generic phrases (\"the theorem\", \"this lemma\", \"the result above\").\n\
         - proof, explanation, noise: return an empty aliases array.\n\n\
         Examples:\n\
         Input:\n\
         10 :: The next result gives the easiest way to check whether a subset of a vector space is a subspace.\n\
         11 :: 1.34 conditions for a subspace\n\
         12 :: A subset U of V is a subspace of V if and only if U satisfies the following three conditions.\n\
         13 :: additive identity 0 in U.\n\
         14 :: closed under addition u, w in U implies u + w in U.\n\
         15 :: closed under scalar multiplication a in F and u in U implies au in U.\n\
         16 :: Proof. Clearly each condition is necessary for U to be a subspace...\n\
         Output:\n\
         { \"chunks\": [\n\
           { \"block_ids\": [10,11,12,13,14,15], \"chunk_type\": \"theorem\",\n\
             \"title\": \"1.34 Conditions for a Subspace\", \"subject\": null,\n\
             \"aliases\": [\n\
               { \"text\": \"1.34\", \"kind\": \"numeric_label\" },\n\
               { \"text\": \"Conditions for a Subspace\", \"kind\": \"canonical_name\" },\n\
               { \"text\": \"conditions for a subspace\", \"kind\": \"canonical_name\" }\n\
             ] },\n\
           { \"block_ids\": [16], \"chunk_type\": \"proof\",\n\
             \"title\": null, \"subject\": \"1.34 Conditions for a Subspace\", \"aliases\": [] }\n\
         ] }\n\n\
         Input:\n\
         20 :: 11  Prove that the intersection of every collection of subspaces of V is a subspace of V.\n\
         21 :: 12  Prove that the union of two subspaces of V is a subspace of V if and only if one of the subspaces is contained in the other.\n\
         22 :: 13  Prove that the union of three subspaces of V is a subspace of V if and only if one of the subspaces contains the other two.\n\
         Output:\n\
         { \"chunks\": [\n\
           { \"block_ids\": [20], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 11\", \"subject\": null,\n\
             \"aliases\": [{ \"text\": \"11\", \"kind\": \"numeric_label\" }] },\n\
           { \"block_ids\": [21], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 12\", \"subject\": null,\n\
             \"aliases\": [{ \"text\": \"12\", \"kind\": \"numeric_label\" }] },\n\
           { \"block_ids\": [22], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 13\", \"subject\": null,\n\
             \"aliases\": [{ \"text\": \"13\", \"kind\": \"numeric_label\" }] }\n\
         ] }\n\n\
         Return ONLY valid JSON matching this schema:\n\
         { \"chunks\": [ { \"block_ids\": [int, ...], \"chunk_type\": \"...\",\n\
                           \"title\": \"...\" | null, \"subject\": \"...\" | null,\n\
                           \"aliases\": [{ \"text\": \"...\", \"kind\": \"numeric_label\" | \"canonical_name\" }] }, ... ] }\n\n\
         Blocks (id :: text):\n",
    );
    for b in blocks {
        let snippet: String = b.text.chars().take(300).collect();
        let snippet = snippet.replace('\n', " ");
        s.push_str(&format!("{} :: {}\n", b.id, snippet));
    }
    s
}

pub fn build_chunk_body_prompt(chunk: &ChunkBodyPrompt<'_>) -> String {
    let mut s = String::new();
    s.push_str(
        "You are transcribing a cropped image from a mathematics textbook into clean, readable body text.\n\
         Return ONLY valid JSON matching this schema:\n\
         { \"body_markdown\": \"...\" }\n\n\
         Rules:\n\
         1. Transcribe only what is visible inside the crop.\n\
         2. Preserve reading order and paragraph breaks.\n\
         3. Render mathematics in LaTeX.\n\
         4. Use inline math for short expressions and display math for standalone equations.\n\
         5. Do not invent missing or occluded content.\n\
         6. Omit headings that duplicate the title or subject already shown elsewhere in the UI.\n\
         7. Return the body only, not commentary about uncertainty.\n\n",
    );
    s.push_str(&format!("Chunk type: {}\n", chunk.chunk_type));
    if let Some(title) = chunk.title.filter(|value| !value.trim().is_empty()) {
        s.push_str(&format!(
            "Displayed title (omit if repeated in the crop): {}\n",
            title.trim()
        ));
    }
    if let Some(subject) = chunk.subject.filter(|value| !value.trim().is_empty()) {
        s.push_str(&format!(
            "Displayed proof subject (omit if repeated in the crop): {}\n",
            subject.trim()
        ));
    }
    s.push_str(
        "\nFormatting guidance:\n\
         - Output plain paragraphs separated by blank lines.\n\
         - Use `$...$` for inline LaTeX and `$$...$$` for displayed equations.\n\
         - Keep list numbering or labels if they are visible in the crop.\n",
    );
    s
}

pub fn chunk_groups_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["chunks"],
        "properties": {
            "chunks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["block_ids", "chunk_type", "title", "subject", "aliases"],
                    "properties": {
                        "block_ids": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "minItems": 1
                        },
                        "chunk_type": {
                            "type": "string",
                            "enum": [
                                "definition",
                                "theorem",
                                "proof",
                                "exercise",
                                "example",
                                "explanation",
                                "noise"
                            ]
                        },
                        "title": {
                            "type": ["string", "null"]
                        },
                        "subject": {
                            "type": ["string", "null"]
                        },
                        "aliases": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["text", "kind"],
                                "properties": {
                                    "text": { "type": "string", "minLength": 1 },
                                    "kind": {
                                        "type": "string",
                                        "enum": ["numeric_label", "canonical_name"]
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

pub fn chunk_body_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["body_markdown"],
        "properties": {
            "body_markdown": {
                "type": "string",
                "minLength": 1
            }
        }
    })
}

pub fn parse_chunks(raw: &str, log_target: &str) -> Result<Vec<GroupedChunk>, LlmError> {
    let stripped = strip_code_fences(raw);

    if let Ok(wrapper) = serde_json::from_str::<ChunkResponseWrapper>(stripped) {
        debug!(
            target: log_target,
            "parsed wrapped llm chunk response with {} groups",
            wrapper.chunks.len()
        );
        return Ok(wrapper.chunks);
    }
    if let Ok(arr) = serde_json::from_str::<Vec<GroupedChunk>>(stripped) {
        debug!(
            target: log_target,
            "parsed bare llm chunk response with {} groups",
            arr.len()
        );
        return Ok(arr);
    }
    warn!(target: log_target, "failed to parse llm chunk response");
    Err(LlmError::Parse(raw.to_string()))
}

pub fn parse_chunk_body(raw: &str, log_target: &str) -> Result<ChunkBodyResult, LlmError> {
    let mut parsed: ChunkBodyResult = parse_json(raw, log_target, "chunk body")?;
    parsed.body_markdown = parsed.body_markdown.trim().to_string();
    if parsed.body_markdown.is_empty() {
        warn!(target: log_target, "parsed chunk body was empty");
        return Err(LlmError::Parse(raw.to_string()));
    }
    Ok(parsed)
}

fn parse_json<T: DeserializeOwned>(
    raw: &str,
    log_target: &str,
    label: &str,
) -> Result<T, LlmError> {
    let stripped = strip_code_fences(raw);
    serde_json::from_str::<T>(stripped).map_err(|_| {
        warn!(target: log_target, "failed to parse llm {} response", label);
        LlmError::Parse(raw.to_string())
    })
}

fn strip_code_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    stripped.strip_suffix("```").unwrap_or(stripped).trim()
}

pub fn map_reqwest_error(error: reqwest::Error) -> LlmError {
    if error.is_connect() || error.is_timeout() {
        LlmError::Unavailable
    } else {
        LlmError::Http(error.to_string())
    }
}

pub async fn stream_sse_events<F, C>(
    mut response: reqwest::Response,
    mut on_event_data: F,
    should_cancel: C,
) -> Result<(), LlmError>
where
    F: FnMut(&str) -> Result<(), LlmError>,
    C: Fn() -> bool,
{
    let mut buffer = Vec::<u8>::new();

    loop {
        if should_cancel() {
            return Err(LlmError::Cancelled);
        }

        let next = response.chunk().await.map_err(map_reqwest_error)?;
        let Some(chunk) = next else {
            break;
        };
        buffer.extend_from_slice(&chunk);

        while let Some(boundary) = next_sse_event_boundary(&buffer) {
            let event_bytes = buffer[..boundary].to_vec();
            buffer.drain(..boundary);
            if should_cancel() {
                return Err(LlmError::Cancelled);
            }
            if let Some(data) = parse_sse_event_data(&event_bytes) {
                on_event_data(&data)?;
            }
        }
    }

    if should_cancel() {
        return Err(LlmError::Cancelled);
    }
    if let Some(data) = parse_sse_event_data(&buffer) {
        on_event_data(&data)?;
    }
    Ok(())
}

pub fn merge_stream_text(output: &mut String, next_text: &str) -> Option<String> {
    if next_text.is_empty() {
        return None;
    }

    if let Some(suffix) = next_text.strip_prefix(output.as_str()) {
        if suffix.is_empty() {
            return None;
        }
        output.push_str(suffix);
        return Some(suffix.to_string());
    }

    output.push_str(next_text);
    Some(next_text.to_string())
}

fn next_sse_event_boundary(buffer: &[u8]) -> Option<usize> {
    let mut boundary: Option<usize> = None;

    if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        boundary = Some(index + 4);
    }
    if let Some(index) = buffer.windows(2).position(|window| window == b"\n\n") {
        let candidate = index + 2;
        boundary = Some(boundary.map_or(candidate, |current| current.min(candidate)));
    }

    boundary
}

fn parse_sse_event_data(event_bytes: &[u8]) -> Option<String> {
    if event_bytes.is_empty() {
        return None;
    }

    let event_text = String::from_utf8_lossy(event_bytes).replace("\r\n", "\n");
    let mut parts = Vec::<String>::new();

    for line in event_text.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            parts.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }

    if parts.is_empty() {
        return None;
    }

    let joined = parts.join("\n");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

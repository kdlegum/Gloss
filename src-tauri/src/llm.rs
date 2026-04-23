use log::{debug, warn};
use serde::Deserialize;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Ollama,
    OpenAI,
}

impl LlmProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAI => "openai",
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
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "llm unavailable"),
            Self::Config(s) => write!(f, "llm config error: {s}"),
            Self::Http(s) => write!(f, "llm http error: {s}"),
            Self::Parse(s) => write!(f, "llm parse error: {s}"),
        }
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
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
}

#[derive(Deserialize)]
struct ChunkResponseWrapper {
    chunks: Vec<GroupedChunk>,
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
             \"title\": \"1.34 Conditions for a Subspace\", \"subject\": null },\n\
           { \"block_ids\": [16], \"chunk_type\": \"proof\",\n\
             \"title\": null, \"subject\": \"1.34 Conditions for a Subspace\" }\n\
         ] }\n\n\
         Input:\n\
         20 :: 11  Prove that the intersection of every collection of subspaces of V is a subspace of V.\n\
         21 :: 12  Prove that the union of two subspaces of V is a subspace of V if and only if one of the subspaces is contained in the other.\n\
         22 :: 13  Prove that the union of three subspaces of V is a subspace of V if and only if one of the subspaces contains the other two.\n\
         Output:\n\
         { \"chunks\": [\n\
           { \"block_ids\": [20], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 11\", \"subject\": null },\n\
           { \"block_ids\": [21], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 12\", \"subject\": null },\n\
           { \"block_ids\": [22], \"chunk_type\": \"exercise\",\n\
             \"title\": \"Exercise 13\", \"subject\": null }\n\
         ] }\n\n\
         Return ONLY valid JSON matching this schema:\n\
         { \"chunks\": [ { \"block_ids\": [int, ...], \"chunk_type\": \"...\",\n\
                           \"title\": \"...\" | null, \"subject\": \"...\" | null }, ... ] }\n\n\
         Blocks (id :: text):\n",
    );
    for b in blocks {
        let snippet: String = b.text.chars().take(300).collect();
        let snippet = snippet.replace('\n', " ");
        s.push_str(&format!("{} :: {}\n", b.id, snippet));
    }
    s
}

pub fn parse_chunks(raw: &str, log_target: &str) -> Result<Vec<GroupedChunk>, LlmError> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();

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

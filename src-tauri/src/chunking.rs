// Background PDF chunking pipeline.
//
// Flow per viewed page:
//   1. Extract text blocks for that page via pdfium + persist to `text_blocks`.
//   2. Check the selected LLM provider. If unavailable, write one chunk per block.
//   3. Otherwise ask the provider to group blocks + classify, persist to `chunks`,
//      update `text_blocks.chunk_id`.
//
// Progress is reported via Tauri events (`chunking_progress`) so the frontend can
// re-render the overlay as chunks land.

use crate::deepseek::DeepSeekClient;
use crate::gemini::GeminiClient;
use crate::llm::{
    BlockForPrompt, GroupedChunk, LlmProvider, PastPaperBlockForPrompt, PastPaperDocumentResult,
    PastPaperInstructionMarkdown,
};
use crate::ollama::OllamaClient;
use crate::openai::OpenAiClient;
use crate::references;
use crate::settings;
use crate::zai::ZaiClient;
use crate::PdfiumWorker;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::StreamExt;
use image::{ImageBuffer, Rgba};
use log::{debug, error, info, warn};
use pdfium_render::prelude::*;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Semaphore;
use tokio::time::{sleep, Duration};

// ── Data types ──────────────────────────────────────────────────────────────

pub struct RawBlock {
    pub order_idx: i32,
    pub bbox_x: f32,
    pub bbox_y: f32,
    pub bbox_w: f32,
    pub bbox_h: f32,
    pub text: String,
}

#[derive(Clone)]
pub struct PersistedBlock {
    pub id: i64,
    pub text: String,
    pub transcribed_text: Option<String>,
    pub bbox_x: f32,
    pub bbox_y: f32,
    pub bbox_w: f32,
    pub bbox_h: f32,
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    source_document_id: i64,
    page_number: i64,
    phase: &'static str, // "extracted" | "grouped" | "failed"
}

const ALLOWED_TYPES: &[&str] = &[
    "definition",
    "theorem",
    "proof",
    "exercise",
    "example",
    "explanation",
    "noise",
    "question",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentMode {
    Textbook,
    PastPaper,
}

impl DocumentMode {
    fn from_db(value: &str) -> Self {
        if value.trim().eq_ignore_ascii_case("past_paper") {
            Self::PastPaper
        } else {
            Self::Textbook
        }
    }
}

#[derive(Clone)]
struct DocumentBlock {
    id: i64,
    page_id: i64,
    page_number: i64,
    order_idx: i32,
    bbox_x: f32,
    bbox_y: f32,
    bbox_w: f32,
    bbox_h: f32,
    text: String,
    transcribed_text: Option<String>,
}

struct PastPaperChunkCandidate {
    label: String,
    available_marks: Option<i64>,
    block_ids: Vec<i64>,
    question_text: String,
}

struct NormalizedPastPaperAssignments {
    question_candidates: Vec<PastPaperChunkCandidate>,
    noise_block_ids: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstructionPageRange {
    start_page: i64,
    end_page: i64,
}

impl InstructionPageRange {
    fn contains(&self, page_number: i64) -> bool {
        page_number >= self.start_page && page_number <= self.end_page
    }
}

const TRANSIENT_LLM_RETRY_MAX_ATTEMPTS: usize = 4;
const TRANSIENT_LLM_RETRY_DELAYS_MS: [u64; TRANSIENT_LLM_RETRY_MAX_ATTEMPTS - 1] =
    [1_500, 3_000, 6_000];

// ── Top-level orchestrator ──────────────────────────────────────────────────

/// Runs the chunking pipeline for one page. Called from a `tokio::spawn`
/// task when the viewer resolves a page; never propagates errors — any
/// failures mark `chunking_status='failed'` and log.
pub async fn run_for_page(
    pool: SqlitePool,
    pdfium: PdfiumWorker,
    app: AppHandle,
    doc_id: i64,
    page_number: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    zai_transcription_semaphore: Arc<Semaphore>,
) {
    info!(
        target: "gloss_lib::chunking",
        "starting chunking job for doc_id={} page={} provider={}",
        doc_id,
        page_number,
        provider
    );
    if let Err(e) = run_inner(
        &pool,
        &pdfium,
        &app,
        doc_id,
        page_number,
        provider,
        model_override,
        &zai_transcription_semaphore,
    )
    .await
    {
        error!(
            target: "gloss_lib::chunking",
            "chunking job failed for doc_id={} page={} provider={}: {}",
            doc_id,
            page_number,
            provider,
            e
        );
        emit_progress(&app, doc_id, page_number, "failed");
        let _ = sqlx::query("UPDATE source_documents SET chunking_status = 'failed' WHERE id = ?")
            .bind(doc_id)
            .execute(&pool)
            .await;
    } else {
        info!(
            target: "gloss_lib::chunking",
            "finished chunking job for doc_id={} page={} provider={}",
            doc_id,
            page_number,
            provider
        );
    }
}

async fn run_inner(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    app: &AppHandle,
    doc_id: i64,
    page_number: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    zai_transcription_semaphore: &Arc<Semaphore>,
) -> Result<(), String> {
    if page_number < 1 {
        return Err(format!("invalid page number {}", page_number));
    }

    // Resolve absolute path.
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let row = sqlx::query("SELECT file_path, document_mode FROM source_documents WHERE id = ?")
        .bind(doc_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let relative_path: String = row.get("file_path");
    let document_mode = DocumentMode::from_db(row.get::<String, _>("document_mode").as_str());
    let abs_path = data_dir.join(&relative_path);
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} page={} mode={:?} resolved path to {}",
        doc_id,
        page_number,
        document_mode,
        abs_path.display()
    );

    if document_mode == DocumentMode::PastPaper {
        return run_past_paper_document(
            pool,
            pdfium,
            app,
            doc_id,
            page_number,
            provider,
            model_override.as_deref(),
            &abs_path,
            false,
        )
        .await;
    }

    let page_id = get_or_create_page(pool, doc_id, page_number).await?;
    let page_index = (page_number - 1) as usize;

    set_status(pool, doc_id, "extracting").await?;

    let existing_blocks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM text_blocks WHERE page_id = ?")
            .bind(page_id)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    if existing_blocks > 0 {
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} skipping extraction; {} text blocks already exist",
            doc_id,
            page_number,
            existing_blocks
        );
    } else {
        let p = abs_path.clone();
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} extracting text blocks",
            doc_id,
            page_number
        );
        let blocks = pdfium
            .run(move |pdfium| extract_blocks_from_page(pdfium, &p, page_index))
            .await
            .map_err(|e| {
                format!(
                    "doc_id={} page={} failed to extract text blocks: {}",
                    doc_id, page_number, e
                )
            })?;

        persist_blocks(pool, page_id, &blocks).await?;
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} persisted {} text blocks",
            doc_id,
            page_number,
            blocks.len()
        );
        emit_progress(app, doc_id, page_number, "extracted");
    }

    set_status(pool, doc_id, "grouping").await?;
    let existing_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE page_id = ?")
        .bind(page_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    if existing_chunks > 0 {
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} skipping grouping; {} chunks already exist",
            doc_id,
            page_number,
            existing_chunks
        );
        set_status(pool, doc_id, "done").await?;
        return Ok(());
    }

    let use_llm = provider_health_check(pool, provider, model_override.as_deref()).await;
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} page={} provider={} available={}",
        doc_id,
        page_number,
        provider,
        use_llm
    );

    let mut blocks = load_unchunked_blocks(pool, page_id).await?;
    if blocks.is_empty() {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} has no unchunked blocks to group",
            doc_id,
            page_number
        );
        set_status(pool, doc_id, "done").await?;
        return Ok(());
    }
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} page={} grouping {} blocks",
        doc_id,
        page_number,
        blocks.len()
    );

    // ZAI block transcription (optional — improves math in chunking prompt)
    if use_llm {
        match transcribe_blocks_for_page(
            pool,
            pdfium,
            &abs_path,
            page_index,
            &blocks,
            Arc::clone(zai_transcription_semaphore),
        )
        .await
        {
            Ok(updated) => {
                info!(
                    target: "gloss_lib::chunking",
                    "doc_id={} page={} ZAI transcribed {} blocks",
                    doc_id, page_number, updated.len()
                );
                blocks = updated;
            }
            Err(e) => warn!(
                target: "gloss_lib::chunking",
                "doc_id={} page={} ZAI block transcription skipped: {}",
                doc_id, page_number, e
            ),
        }
    }

    let used_llm = if use_llm {
        match try_llm_chunking(pool, provider, &blocks, model_override.as_deref()).await {
            Ok(groups) if validate_groups(&groups, &blocks) => {
                persist_chunks(pool, doc_id, page_id, &blocks, &groups, true).await?;
                populate_formatted_body_from_transcriptions(pool, page_id).await?;
                link_proofs_to_theorems(pool, page_id).await?;
                if let Err(e) = references::reindex_document_references(pool, doc_id).await {
                    warn!(
                        target: "gloss_lib::chunking",
                        "doc_id={} page={} reference reindex failed: {}",
                        doc_id, page_number, e
                    );
                }
                info!(
                    target: "gloss_lib::chunking",
                    "doc_id={} page={} provider={} persisted {} LLM chunk groups",
                    doc_id,
                    page_number,
                    provider,
                    groups.len()
                );
                true
            }
            Ok(groups) => {
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} page={} provider={} received invalid LLM grouping ({} groups); using fallback",
                    doc_id,
                    page_number,
                    provider,
                    groups.len()
                );
                false
            }
            Err(err) => {
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} page={} provider={} LLM grouping failed: {}; using fallback",
                    doc_id,
                    page_number,
                    provider,
                    err
                );
                false
            }
        }
    } else {
        false
    };

    if !used_llm {
        fallback_one_chunk_per_block(pool, doc_id, page_id, &blocks).await?;
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} fell back to one chunk per block ({})",
            doc_id,
            page_number,
            blocks.len()
        );
    }

    emit_progress(app, doc_id, page_number, "grouped");

    set_status(pool, doc_id, "done").await?;
    Ok(())
}

async fn run_past_paper_document(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    app: &AppHandle,
    doc_id: i64,
    requested_page: i64,
    provider: LlmProvider,
    model_override: Option<&str>,
    abs_path: &PathBuf,
    force_rebuild: bool,
) -> Result<(), String> {
    set_status(pool, doc_id, "extracting").await?;
    let blocks = extract_or_load_document_blocks(pool, pdfium, abs_path, doc_id).await?;
    emit_progress(app, doc_id, requested_page, "extracted");

    if blocks.is_empty() {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} past-paper extraction produced no text blocks",
            doc_id
        );
        set_status(pool, doc_id, "done").await?;
        return Ok(());
    }

    set_status(pool, doc_id, "grouping").await?;
    let use_llm = provider_health_check(pool, provider, model_override).await;
    if !use_llm {
        return Err(format!(
            "provider {} is unavailable for past-paper chunking",
            provider
        ));
    }

    let existing_questions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE source_document_id = ?")
            .bind(doc_id)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    if existing_questions > 0 && !force_rebuild {
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} skipping past-paper regroup; existing chunks={}",
            doc_id,
            existing_questions
        );
        set_status(pool, doc_id, "done").await?;
        return Ok(());
    }

    let instruction_range = load_instruction_page_range(pool, doc_id).await?;
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} past-paper instruction_page_range={}",
        doc_id,
        instruction_range
            .as_ref()
            .map(|r| format!("{}-{}", r.start_page, r.end_page))
            .unwrap_or_else(|| "none".to_string())
    );

    // Render all pages as base64 PNG images.
    let page_numbers = collect_document_page_numbers(&blocks);
    let mut page_images: HashMap<i64, String> = HashMap::new();
    for &page_number in &page_numbers {
        let page_index = (page_number - 1) as usize;
        match render_page_as_base64(pdfium, abs_path, page_index).await {
            Ok(b64) => {
                page_images.insert(page_number, b64);
            }
            Err(e) => warn!(
                target: "gloss_lib::chunking",
                "doc_id={} page={} failed to render for past-paper: {}",
                doc_id, page_number, e
            ),
        }
    }
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} rendered {}/{} pages as images",
        doc_id,
        page_images.len(),
        page_numbers.len()
    );

    // Separate instruction vs question blocks and images (preserving page order).
    let mut instruction_blocks: Vec<PastPaperBlockForPrompt> = Vec::new();
    let mut question_blocks: Vec<PastPaperBlockForPrompt> = Vec::new();
    let mut instruction_images: Vec<String> = Vec::new();
    let mut question_images: Vec<String> = Vec::new();
    let mut instruction_pages_seen: HashSet<i64> = HashSet::new();
    let mut question_pages_seen: HashSet<i64> = HashSet::new();

    for block in &blocks {
        let is_instruction = instruction_range
            .as_ref()
            .is_some_and(|r| r.contains(block.page_number));
        let pp_block = PastPaperBlockForPrompt {
            id: block.id,
            page_number: block.page_number,
            bbox_y: block.bbox_y,
        };
        if is_instruction {
            instruction_blocks.push(pp_block);
            if instruction_pages_seen.insert(block.page_number) {
                if let Some(img) = page_images.get(&block.page_number) {
                    instruction_images.push(img.clone());
                }
            }
        } else {
            question_blocks.push(pp_block);
            if question_pages_seen.insert(block.page_number) {
                if let Some(img) = page_images.get(&block.page_number) {
                    question_images.push(img.clone());
                }
            }
        }
    }

    info!(
        target: "gloss_lib::chunking",
        "doc_id={} past-paper: {} instruction blocks ({} pages), {} question blocks ({} pages)",
        doc_id,
        instruction_blocks.len(),
        instruction_images.len(),
        question_blocks.len(),
        question_images.len()
    );

    // Call #1: extract instruction markdown (best-effort; proceed without on failure).
    let instruction_markdown = if instruction_blocks.is_empty() {
        String::new()
    } else {
        match run_with_transient_llm_retry(
            doc_id,
            provider,
            "instruction extraction",
            || {
                try_chunk_ai_extract_instruction(
                    pool,
                    provider,
                    &instruction_images,
                    &instruction_blocks,
                    model_override,
                )
            },
        )
        .await {
            Ok(result) => {
                let preview = result
                    .markdown
                    .chars()
                    .take(240)
                    .collect::<String>()
                    .replace('\n', " ");
                info!(
                    target: "gloss_lib::chunking",
                    "doc_id={} extracted instruction markdown ({} chars) preview={:?}",
                    doc_id,
                    result.markdown.len(),
                    preview
                );
                result.markdown
            }
            Err(e) => {
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} instruction extraction failed: {}; proceeding without",
                    doc_id,
                    e
                );
                String::new()
            }
        }
    };

    if question_blocks.is_empty() {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} past-paper has no question blocks; skipping chunk extraction",
            doc_id
        );
        set_status(pool, doc_id, "done").await?;
        return Ok(());
    }

    // Call #2: identify and transcribe all questions in a single document call.
    let result = run_with_transient_llm_retry(
        doc_id,
        provider,
        "question chunking",
        || {
            try_chunk_ai_chunk_document(
                pool,
                provider,
                &question_images,
                &question_blocks,
                &instruction_markdown,
                model_override,
            )
        },
    )
    .await?;

    info!(
        target: "gloss_lib::chunking",
        "doc_id={} past-paper LLM returned {} questions (noise_blocks={})",
        doc_id,
        result.questions.len(),
        result.noise_block_ids.len()
    );

    let raw_candidates: Vec<PastPaperChunkCandidate> = result
        .questions
        .into_iter()
        .map(|q| PastPaperChunkCandidate {
            label: q.question_label,
            available_marks: q.available_marks,
            block_ids: q.block_ids,
            question_text: q.question_text,
        })
        .collect();
    log_past_paper_mark_coverage(doc_id, &instruction_markdown, &raw_candidates);

    let assignments = normalize_past_paper_candidates(
        doc_id,
        raw_candidates,
        &question_blocks,
        &result.noise_block_ids,
    );
    if assignments.question_candidates.is_empty() {
        return Err(
            "LLM did not return any usable question chunks for the past paper".to_string(),
        );
    }

    persist_past_paper_chunks(
        pool,
        doc_id,
        &blocks,
        &assignments.question_candidates,
        &assignments.noise_block_ids,
    )
    .await?;
    if let Err(e) = references::reindex_document_references(pool, doc_id).await {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} past-paper reference reindex failed: {}",
            doc_id,
            e
        );
    }

    emit_progress(app, doc_id, requested_page, "grouped");
    set_status(pool, doc_id, "done").await?;
    Ok(())
}

async fn extract_or_load_document_blocks(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    abs_path: &PathBuf,
    doc_id: i64,
) -> Result<Vec<DocumentBlock>, String> {
    let path_for_count = abs_path.clone();
    let page_count = pdfium
        .run(move |pdfium| {
            let doc = pdfium
                .load_pdf_from_file(&path_for_count, None)
                .map_err(|e| e.to_string())?;
            Ok(doc.pages().len() as i64)
        })
        .await?;
    if page_count <= 0 {
        return Ok(Vec::new());
    }

    let mut out: Vec<DocumentBlock> = Vec::new();
    for page_number in 1..=page_count {
        let page_id = get_or_create_page(pool, doc_id, page_number).await?;
        let existing_blocks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM text_blocks WHERE page_id = ?")
                .bind(page_id)
                .fetch_one(pool)
                .await
                .map_err(|e| e.to_string())?;
        if existing_blocks == 0 {
            let path_for_extract = abs_path.clone();
            let page_index = (page_number - 1) as usize;
            let blocks = pdfium
                .run(move |pdfium| extract_blocks_from_page(pdfium, &path_for_extract, page_index))
                .await
                .map_err(|e| {
                    format!(
                        "doc_id={} page={} failed to extract text blocks: {}",
                        doc_id, page_number, e
                    )
                })?;
            persist_blocks(pool, page_id, &blocks).await?;
        }

        out.extend(load_document_blocks_for_page(pool, page_id, page_number).await?);
    }

    out.sort_by(|a, b| {
        a.page_number
            .cmp(&b.page_number)
            .then_with(|| a.order_idx.cmp(&b.order_idx))
    });
    Ok(out)
}

async fn load_document_blocks_for_page(
    pool: &SqlitePool,
    page_id: i64,
    page_number: i64,
) -> Result<Vec<DocumentBlock>, String> {
    let rows = sqlx::query(
        "SELECT id, order_idx, text, transcribed_text, bbox_x, bbox_y, bbox_w, bbox_h \
         FROM text_blocks WHERE page_id = ? ORDER BY order_idx",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| DocumentBlock {
            id: r.get("id"),
            page_id,
            page_number,
            order_idx: r.get("order_idx"),
            bbox_x: r.get("bbox_x"),
            bbox_y: r.get("bbox_y"),
            bbox_w: r.get("bbox_w"),
            bbox_h: r.get("bbox_h"),
            text: r.get("text"),
            transcribed_text: r.get("transcribed_text"),
        })
        .collect())
}

fn collect_document_page_numbers(blocks: &[DocumentBlock]) -> Vec<i64> {
    let mut pages: Vec<i64> = blocks
        .iter()
        .map(|block| block.page_number)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    pages.sort_unstable();
    pages
}

async fn load_instruction_page_range(
    pool: &SqlitePool,
    doc_id: i64,
) -> Result<Option<InstructionPageRange>, String> {
    let row = sqlx::query(
        "SELECT instruction_page_start, instruction_page_end \
         FROM source_documents WHERE id = ?",
    )
    .bind(doc_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    let start: Option<i64> = row.get("instruction_page_start");
    let end: Option<i64> = row.get("instruction_page_end");
    resolve_instruction_page_range(start, end).map_err(|err| format!("doc_id={doc_id} {err}"))
}

fn resolve_instruction_page_range(
    start_page: Option<i64>,
    end_page: Option<i64>,
) -> Result<Option<InstructionPageRange>, String> {
    match (start_page, end_page) {
        (None, None) => Ok(None),
        (Some(start_page), Some(end_page)) => {
            if start_page < 1 || end_page < 1 || start_page > end_page {
                return Err(format!(
                    "invalid instruction page range {}-{}",
                    start_page, end_page
                ));
            }
            Ok(Some(InstructionPageRange {
                start_page,
                end_page,
            }))
        }
        _ => Err(
            "invalid instruction page range columns (both start and end are required)".to_string(),
        ),
    }
}

fn is_transient_model_demand_error(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    if !(lower.contains("status 503") || lower.contains("\"code\": 503")) {
        return false;
    }
    lower.contains("service unavailable")
        || lower.contains("\"status\": \"unavailable\"")
        || lower.contains("currently experiencing high demand")
        || lower.contains("please try again later")
}

async fn run_with_transient_llm_retry<T, F, Fut>(
    doc_id: i64,
    provider: LlmProvider,
    operation: &'static str,
    mut op: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut attempt: usize = 1;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= TRANSIENT_LLM_RETRY_MAX_ATTEMPTS
                    || !is_transient_model_demand_error(&err)
                {
                    return Err(err);
                }

                let delay_ms = TRANSIENT_LLM_RETRY_DELAYS_MS[attempt - 1];
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} {} provider={} transient model demand error (attempt {}/{}); retrying in {}ms: {}",
                    doc_id,
                    operation,
                    provider,
                    attempt,
                    TRANSIENT_LLM_RETRY_MAX_ATTEMPTS,
                    delay_ms,
                    err
                );
                sleep(Duration::from_millis(delay_ms)).await;
                attempt += 1;
            }
        }
    }
}

async fn try_chunk_ai_extract_instruction(
    pool: &SqlitePool,
    provider: LlmProvider,
    images: &[String],
    blocks: &[PastPaperBlockForPrompt],
    model_override: Option<&str>,
) -> Result<PastPaperInstructionMarkdown, String> {
    info!(
        target: "gloss_lib::chunking",
        "past-paper instruction extraction provider={} images={} blocks={}",
        provider,
        images.len(),
        blocks.len()
    );
    let api_key = configured_api_key(pool, provider).await;
    let model_override = normalize_model_override(model_override);
    match provider {
        LlmProvider::OpenAI => OpenAiClient::with_api_key_and_model(api_key, model_override)
            .extract_instruction_markdown(images, blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Gemini => GeminiClient::with_api_key_and_model(api_key, model_override)
            .extract_instruction_markdown(images, blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Ollama | LlmProvider::DeepSeek => Err(
            "past-paper chunking requires a vision-capable model (OpenAI or Gemini)".to_string(),
        ),
        LlmProvider::Zai => Err("provider zai does not support past-paper chunking".to_string()),
    }
}

async fn try_chunk_ai_chunk_document(
    pool: &SqlitePool,
    provider: LlmProvider,
    images: &[String],
    blocks: &[PastPaperBlockForPrompt],
    instruction_markdown: &str,
    model_override: Option<&str>,
) -> Result<PastPaperDocumentResult, String> {
    info!(
        target: "gloss_lib::chunking",
        "past-paper document chunking provider={} images={} blocks={}",
        provider,
        images.len(),
        blocks.len()
    );
    let api_key = configured_api_key(pool, provider).await;
    let model_override = normalize_model_override(model_override);
    match provider {
        LlmProvider::OpenAI => OpenAiClient::with_api_key_and_model(api_key, model_override)
            .chunk_past_paper_document(images, blocks, instruction_markdown)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Gemini => GeminiClient::with_api_key_and_model(api_key, model_override)
            .chunk_past_paper_document(images, blocks, instruction_markdown)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Ollama | LlmProvider::DeepSeek => Err(
            "past-paper chunking requires a vision-capable model (OpenAI or Gemini)".to_string(),
        ),
        LlmProvider::Zai => Err("provider zai does not support past-paper chunking".to_string()),
    }
}

fn normalize_past_paper_candidates(
    doc_id: i64,
    candidates: Vec<PastPaperChunkCandidate>,
    question_blocks: &[PastPaperBlockForPrompt],
    noise_block_ids: &[i64],
) -> NormalizedPastPaperAssignments {
    let mut valid_ids: HashSet<i64> = HashSet::new();
    let mut block_order: HashMap<i64, usize> = HashMap::new();
    for (idx, block) in question_blocks.iter().enumerate() {
        valid_ids.insert(block.id);
        block_order.entry(block.id).or_insert(idx);
    }

    let mut noise_valid: HashSet<i64> = HashSet::new();
    for block_id in noise_block_ids {
        if valid_ids.contains(block_id) {
            noise_valid.insert(*block_id);
        } else {
            warn!(
                target: "gloss_lib::chunking",
                "doc_id={} ignoring unknown noise block_id={}",
                doc_id,
                block_id
            );
        }
    }

    let mut assigned: HashSet<i64> = HashSet::new();
    let mut normalized = Vec::new();

    for candidate in candidates {
        let mut unique_local: HashSet<i64> = HashSet::new();
        let mut kept_block_ids = Vec::new();
        for block_id in candidate.block_ids {
            if !valid_ids.contains(&block_id) {
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} question_label={} dropped unknown block_id={}",
                    doc_id,
                    candidate.label,
                    block_id
                );
                continue;
            }
            if noise_valid.contains(&block_id) {
                continue;
            }
            if !unique_local.insert(block_id) {
                continue;
            }
            if !assigned.insert(block_id) {
                continue;
            }
            kept_block_ids.push(block_id);
        }

        kept_block_ids.sort_by_key(|id| block_order.get(id).copied().unwrap_or(usize::MAX));

        if kept_block_ids.is_empty() {
            warn!(
                target: "gloss_lib::chunking",
                "doc_id={} dropping question_label={} because no usable block_ids remained",
                doc_id,
                candidate.label
            );
            continue;
        }

        normalized.push(PastPaperChunkCandidate {
            label: candidate.label,
            available_marks: candidate.available_marks,
            block_ids: kept_block_ids,
            question_text: candidate.question_text,
        });
    }

    let mut unassigned_ids: Vec<i64> = valid_ids
        .iter()
        .copied()
        .filter(|id| !assigned.contains(id) && !noise_valid.contains(id))
        .collect();
    unassigned_ids.sort_by_key(|id| block_order.get(id).copied().unwrap_or(usize::MAX));

    let mut noise_ids_final: Vec<i64> = noise_valid
        .iter()
        .copied()
        .filter(|id| !assigned.contains(id))
        .collect();
    noise_ids_final.sort_by_key(|id| block_order.get(id).copied().unwrap_or(usize::MAX));

    if !unassigned_ids.is_empty() {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} past-paper {} block(s) were left unassigned by model output",
            doc_id,
            unassigned_ids.len()
        );
    }

    info!(
        target: "gloss_lib::chunking",
        "doc_id={} past-paper normalized questions={} assigned_blocks={} noise_blocks={} unassigned_blocks={}",
        doc_id,
        normalized.len(),
        assigned.len(),
        noise_ids_final.len(),
        unassigned_ids.len()
    );

    NormalizedPastPaperAssignments {
        question_candidates: normalized,
        noise_block_ids: noise_ids_final,
    }
}

async fn persist_past_paper_chunks(
    pool: &SqlitePool,
    doc_id: i64,
    blocks: &[DocumentBlock],
    candidates: &[PastPaperChunkCandidate],
    noise_block_ids: &[i64],
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let by_id: HashMap<i64, &DocumentBlock> =
        blocks.iter().map(|block| (block.id, block)).collect();

    sqlx::query(
        "UPDATE text_blocks SET chunk_id = NULL \
         WHERE page_id IN (SELECT id FROM pages WHERE source_document_id = ?)",
    )
    .bind(doc_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM chunks WHERE source_document_id = ?")
        .bind(doc_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    for candidate in candidates {
        let mut members: Vec<&DocumentBlock> = candidate
            .block_ids
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|a, b| {
            a.page_number
                .cmp(&b.page_number)
                .then_with(|| a.order_idx.cmp(&b.order_idx))
        });

        let mut page_slices: BTreeMap<i64, (i64, (f32, f32, f32, f32))> = BTreeMap::new();
        let mut ocr_segments = Vec::new();
        for member in &members {
            let key = member.page_number;
            let bbox = (member.bbox_x, member.bbox_y, member.bbox_w, member.bbox_h);
            page_slices
                .entry(key)
                .and_modify(|(_, existing)| *existing = merge_bbox(*existing, bbox))
                .or_insert((member.page_id, bbox));
            if let Some(text) = preferred_block_text(member) {
                ocr_segments.push(text.to_string());
            }
        }

        let first = members[0];
        let anchor_bbox = page_slices
            .get(&first.page_number)
            .map(|(_, bbox)| *bbox)
            .unwrap_or((first.bbox_x, first.bbox_y, first.bbox_w, first.bbox_h));

        let ocr_text = if ocr_segments.is_empty() {
            None
        } else {
            Some(ocr_segments.join("\n\n"))
        };
        let formatted_body_md = if candidate.question_text.trim().is_empty() {
            None
        } else {
            Some(candidate.question_text.trim().to_string())
        };
        let title = format!("Question {}", candidate.label);

        let row = sqlx::query(
            "INSERT INTO chunks \
             (source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, ocr_text, \
              formatted_body_md, title, question_label, available_marks, ai_suggested) \
             VALUES (?, ?, 'question', ?, ?, ?, ?, ?, ?, ?, ?, ?, 1) RETURNING id",
        )
        .bind(doc_id)
        .bind(first.page_id)
        .bind(anchor_bbox.0)
        .bind(anchor_bbox.1)
        .bind(anchor_bbox.2)
        .bind(anchor_bbox.3)
        .bind(ocr_text.as_deref())
        .bind(formatted_body_md.as_deref())
        .bind(&title)
        .bind(&candidate.label)
        .bind(candidate.available_marks)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let chunk_id: i64 = row.get("id");

        sqlx::query(
            "INSERT OR IGNORE INTO chunk_aliases (chunk_id, alias, alias_kind) \
             VALUES (?, ?, 'numeric_label')",
        )
        .bind(chunk_id)
        .bind(&candidate.label)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        sqlx::query(
            "INSERT OR IGNORE INTO chunk_aliases (chunk_id, alias, alias_kind) \
             VALUES (?, ?, 'canonical_name')",
        )
        .bind(chunk_id)
        .bind(&title)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        for block_id in &candidate.block_ids {
            sqlx::query("UPDATE text_blocks SET chunk_id = ? WHERE id = ?")
                .bind(chunk_id)
                .bind(block_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }

        for (slice_order, (_page_number, (page_id, bbox))) in page_slices.into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO question_page_slices \
                 (chunk_id, page_id, bbox_x, bbox_y, bbox_w, bbox_h, slice_order) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(chunk_id)
            .bind(page_id)
            .bind(bbox.0)
            .bind(bbox.1)
            .bind(bbox.2)
            .bind(bbox.3)
            .bind(slice_order as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }
    }

    for block_id in noise_block_ids {
        let Some(block) = by_id.get(block_id).copied() else {
            continue;
        };
        let ocr_text = preferred_block_text(block);
        let row = sqlx::query(
            "INSERT INTO chunks \
             (source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, ocr_text, ai_suggested) \
             VALUES (?, ?, 'noise', ?, ?, ?, ?, ?, 1) RETURNING id",
        )
        .bind(doc_id)
        .bind(block.page_id)
        .bind(block.bbox_x)
        .bind(block.bbox_y)
        .bind(block.bbox_w)
        .bind(block.bbox_h)
        .bind(ocr_text)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let chunk_id: i64 = row.get("id");

        sqlx::query("UPDATE text_blocks SET chunk_id = ? WHERE id = ?")
            .bind(chunk_id)
            .bind(block.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn merge_bbox(current: (f32, f32, f32, f32), next: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let x0 = current.0.min(next.0);
    let y0 = current.1.min(next.1);
    let x1 = (current.0 + current.2).max(next.0 + next.2);
    let y1 = (current.1 + current.3).max(next.1 + next.3);
    (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

fn log_past_paper_mark_coverage(
    doc_id: i64,
    instruction_markdown: &str,
    candidates: &[PastPaperChunkCandidate],
) {
    let total = candidates.len();
    let with_marks = candidates
        .iter()
        .filter(|candidate| candidate.available_marks.is_some())
        .count();
    let missing = total.saturating_sub(with_marks);

    info!(
        target: "gloss_lib::chunking",
        "doc_id={} past-paper available_marks coverage: {}/{} questions have marks",
        doc_id,
        with_marks,
        total
    );

    if missing == 0 {
        return;
    }

    let missing_labels = candidates
        .iter()
        .filter(|candidate| candidate.available_marks.is_none())
        .map(|candidate| candidate.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    warn!(
        target: "gloss_lib::chunking",
        "doc_id={} available_marks missing for {} question(s): [{}]",
        doc_id,
        missing,
        missing_labels
    );

    let instruction_preview = instruction_markdown
        .chars()
        .take(500)
        .collect::<String>()
        .replace('\n', " ");
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} instruction_markdown preview (500 chars): {}",
        doc_id,
        instruction_preview
    );

    for candidate in candidates.iter().filter(|c| c.available_marks.is_none()).take(6) {
        let snippet = candidate
            .question_text
            .chars()
            .take(220)
            .collect::<String>()
            .replace('\n', " ");
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} question_label={} missing available_marks; question_text preview: {}",
            doc_id,
            candidate.label,
            snippet
        );
    }
}

fn preferred_block_text(block: &DocumentBlock) -> Option<&str> {
    block
        .transcribed_text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let extracted = block.text.trim();
            if extracted.is_empty() {
                None
            } else {
                Some(extracted)
            }
        })
}

pub async fn rechunk_page(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    app: &AppHandle,
    doc_id: i64,
    page_number: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    zai_transcription_semaphore: Arc<Semaphore>,
) -> Result<(), String> {
    info!(
        target: "gloss_lib::chunking",
        "starting manual re-chunk for doc_id={} page={} provider={}",
        doc_id,
        page_number,
        provider
    );
    match rechunk_inner(
        pool,
        pdfium,
        app,
        doc_id,
        page_number,
        provider,
        model_override,
        &zai_transcription_semaphore,
    )
    .await
    {
        Ok(()) => {
            info!(
                target: "gloss_lib::chunking",
                "finished manual re-chunk for doc_id={} page={} provider={}",
                doc_id,
                page_number,
                provider
            );
            Ok(())
        }
        Err(err) => {
            error!(
                target: "gloss_lib::chunking",
                "manual re-chunk failed for doc_id={} page={} provider={}: {}",
                doc_id,
                page_number,
                provider,
                err
            );
            emit_progress(app, doc_id, page_number, "failed");
            let _ = set_status(pool, doc_id, "failed").await;
            Err(err)
        }
    }
}

async fn rechunk_inner(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    app: &AppHandle,
    doc_id: i64,
    page_number: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    _zai_transcription_semaphore: &Arc<Semaphore>,
) -> Result<(), String> {
    if page_number < 1 {
        return Err(format!("invalid page number {}", page_number));
    }

    let row = sqlx::query("SELECT file_path, document_mode FROM source_documents WHERE id = ?")
        .bind(doc_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let document_mode = DocumentMode::from_db(row.get::<String, _>("document_mode").as_str());
    if document_mode == DocumentMode::PastPaper {
        let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let relative_path: String = row.get("file_path");
        let abs_path = data_dir.join(relative_path);
        return run_past_paper_document(
            pool,
            pdfium,
            app,
            doc_id,
            page_number,
            provider,
            model_override.as_deref(),
            &abs_path,
            true,
        )
        .await;
    }

    let page_id = get_or_create_page(pool, doc_id, page_number).await?;
    let blocks = load_blocks_for_page(pool, page_id).await?;
    if blocks.is_empty() {
        return Err("page has no extracted text blocks to re-chunk".into());
    }

    set_status(pool, doc_id, "grouping").await?;
    let groups = try_llm_chunking(pool, provider, &blocks, model_override.as_deref()).await?;
    if !validate_groups(&groups, &blocks) {
        return Err(format!(
            "received invalid {} grouping with {} groups",
            provider,
            groups.len()
        ));
    }

    replace_chunks(pool, doc_id, page_id, &blocks, &groups, true).await?;
    populate_formatted_body_from_transcriptions(pool, page_id).await?;
    link_proofs_to_theorems(pool, page_id).await?;
    if let Err(e) = references::reindex_document_references(pool, doc_id).await {
        warn!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} reference reindex failed: {}",
            doc_id, page_number, e
        );
    }
    emit_progress(app, doc_id, page_number, "grouped");
    set_status(pool, doc_id, "done").await?;
    Ok(())
}

async fn provider_health_check(
    pool: &SqlitePool,
    provider: LlmProvider,
    model_override: Option<&str>,
) -> bool {
    let api_key = configured_api_key(pool, provider).await;
    let model_override = normalize_model_override(model_override);
    match provider {
        LlmProvider::Ollama => {
            OllamaClient::with_text_model(model_override)
                .health_check()
                .await
        }
        LlmProvider::OpenAI => {
            OpenAiClient::with_api_key_and_model(api_key, model_override)
                .health_check()
                .await
        }
        LlmProvider::Gemini => {
            GeminiClient::with_api_key_and_model(api_key, model_override)
                .health_check()
                .await
        }
        LlmProvider::DeepSeek => {
            DeepSeekClient::with_api_key_and_model(api_key, model_override)
                .health_check()
                .await
        }
        LlmProvider::Zai => false,
    }
}

async fn try_llm_chunking(
    pool: &SqlitePool,
    provider: LlmProvider,
    blocks: &[PersistedBlock],
    model_override: Option<&str>,
) -> Result<Vec<GroupedChunk>, String> {
    let prompt_blocks: Vec<BlockForPrompt> = blocks
        .iter()
        .map(|b| BlockForPrompt {
            id: b.id,
            text: b.transcribed_text.as_deref().unwrap_or(&b.text),
        })
        .collect();

    let api_key = configured_api_key(pool, provider).await;
    let model_override = normalize_model_override(model_override);
    match provider {
        LlmProvider::Ollama => OllamaClient::with_text_model(model_override)
            .chunk_blocks(&prompt_blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::OpenAI => OpenAiClient::with_api_key_and_model(api_key, model_override)
            .chunk_blocks(&prompt_blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Gemini => GeminiClient::with_api_key_and_model(api_key, model_override)
            .chunk_blocks(&prompt_blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::DeepSeek => DeepSeekClient::with_api_key_and_model(api_key, model_override)
            .chunk_blocks(&prompt_blocks)
            .await
            .map_err(|e| e.to_string()),
        LlmProvider::Zai => Err("provider zai does not support chunking".to_string()),
    }
}

async fn configured_api_key(pool: &SqlitePool, provider: LlmProvider) -> Option<String> {
    match settings::api_key_for_provider(pool, provider).await {
        Ok(api_key) => api_key,
        Err(err) => {
            warn!(
                target: "gloss_lib::chunking",
                "failed to load API key for provider={}: {}",
                provider,
                err
            );
            None
        }
    }
}

fn normalize_model_override(model_override: Option<&str>) -> Option<String> {
    model_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A valid grouping covers every block exactly once with no unknown ids.
fn validate_groups(groups: &[GroupedChunk], blocks: &[PersistedBlock]) -> bool {
    if groups.is_empty() {
        return false;
    }
    let valid_ids: std::collections::HashSet<i64> = blocks.iter().map(|b| b.id).collect();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for g in groups {
        if g.block_ids.is_empty() {
            return false;
        }
        for id in &g.block_ids {
            if !valid_ids.contains(id) || !seen.insert(*id) {
                return false;
            }
        }
    }
    seen.len() == blocks.len()
}

// ── DB helpers ──────────────────────────────────────────────────────────────

async fn set_status(pool: &SqlitePool, doc_id: i64, status: &str) -> Result<(), String> {
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} chunking_status={}",
        doc_id,
        status
    );
    sqlx::query("UPDATE source_documents SET chunking_status = ? WHERE id = ?")
        .bind(status)
        .bind(doc_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn get_or_create_page(
    pool: &SqlitePool,
    doc_id: i64,
    page_number: i64,
) -> Result<i64, String> {
    sqlx::query("INSERT OR IGNORE INTO pages (source_document_id, page_number) VALUES (?, ?)")
        .bind(doc_id)
        .bind(page_number)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    let row = sqlx::query("SELECT id FROM pages WHERE source_document_id = ? AND page_number = ?")
        .bind(doc_id)
        .bind(page_number)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.get("id"))
}

async fn persist_blocks(
    pool: &SqlitePool,
    page_id: i64,
    blocks: &[RawBlock],
) -> Result<(), String> {
    if blocks.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    for b in blocks {
        sqlx::query(
            "INSERT INTO text_blocks (page_id, order_idx, bbox_x, bbox_y, bbox_w, bbox_h, text) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(page_id)
        .bind(b.order_idx)
        .bind(b.bbox_x)
        .bind(b.bbox_y)
        .bind(b.bbox_w)
        .bind(b.bbox_h)
        .bind(&b.text)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn load_unchunked_blocks(
    pool: &SqlitePool,
    page_id: i64,
) -> Result<Vec<PersistedBlock>, String> {
    let rows = sqlx::query(
        "SELECT id, text, transcribed_text, bbox_x, bbox_y, bbox_w, bbox_h \
         FROM text_blocks WHERE page_id = ? AND chunk_id IS NULL ORDER BY order_idx",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| PersistedBlock {
            id: r.get("id"),
            text: r.get("text"),
            transcribed_text: r.get("transcribed_text"),
            bbox_x: r.get("bbox_x"),
            bbox_y: r.get("bbox_y"),
            bbox_w: r.get("bbox_w"),
            bbox_h: r.get("bbox_h"),
        })
        .collect())
}

async fn load_blocks_for_page(
    pool: &SqlitePool,
    page_id: i64,
) -> Result<Vec<PersistedBlock>, String> {
    let rows = sqlx::query(
        "SELECT id, text, transcribed_text, bbox_x, bbox_y, bbox_w, bbox_h \
         FROM text_blocks WHERE page_id = ? ORDER BY order_idx",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| PersistedBlock {
            id: r.get("id"),
            text: r.get("text"),
            transcribed_text: r.get("transcribed_text"),
            bbox_x: r.get("bbox_x"),
            bbox_y: r.get("bbox_y"),
            bbox_w: r.get("bbox_w"),
            bbox_h: r.get("bbox_h"),
        })
        .collect())
}

async fn persist_chunks(
    pool: &SqlitePool,
    doc_id: i64,
    page_id: i64,
    blocks: &[PersistedBlock],
    groups: &[GroupedChunk],
    ai_suggested: bool,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    persist_chunks_in_tx(&mut tx, doc_id, page_id, blocks, groups, ai_suggested).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn replace_chunks(
    pool: &SqlitePool,
    doc_id: i64,
    page_id: i64,
    blocks: &[PersistedBlock],
    groups: &[GroupedChunk],
    ai_suggested: bool,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM chunks WHERE page_id = ?")
        .bind(page_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    persist_chunks_in_tx(&mut tx, doc_id, page_id, blocks, groups, ai_suggested).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    debug!(
        target: "gloss_lib::chunking",
        "replaced chunks for page_id={} with {} groups",
        page_id,
        groups.len()
    );
    Ok(())
}

async fn persist_chunks_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    doc_id: i64,
    page_id: i64,
    blocks: &[PersistedBlock],
    groups: &[GroupedChunk],
    ai_suggested: bool,
) -> Result<(), String> {
    let by_id: std::collections::HashMap<i64, &PersistedBlock> =
        blocks.iter().map(|b| (b.id, b)).collect();

    for g in groups {
        let members: Vec<&PersistedBlock> = g
            .block_ids
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if members.is_empty() {
            continue;
        }
        let bbox = union_bbox(&members);
        let text = members
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let chunk_type = normalise_chunk_type(&g.chunk_type);

        let row = sqlx::query(
            "INSERT INTO chunks \
             (source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, ocr_text, \
              title, subject, ai_suggested) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(doc_id)
        .bind(page_id)
        .bind(chunk_type)
        .bind(bbox.0)
        .bind(bbox.1)
        .bind(bbox.2)
        .bind(bbox.3)
        .bind(text)
        .bind(&g.title)
        .bind(&g.subject)
        .bind(if ai_suggested { 1 } else { 0 })
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
        let chunk_id: i64 = row.get("id");

        info!(
            target: "gloss_lib::chunking",
            "persisted chunk_id={} type={} title={:?} alias_count={}",
            chunk_id, chunk_type, g.title, g.aliases.len()
        );

        for alias in &g.aliases {
            let text = alias.text.trim();
            let kind = alias.kind.trim();
            if text.is_empty() || (kind != "numeric_label" && kind != "canonical_name") {
                debug!(
                    target: "gloss_lib::chunking",
                    "dropping alias text={:?} kind={:?} for chunk_id={}",
                    text, kind, chunk_id
                );
                continue;
            }
            debug!(
                target: "gloss_lib::chunking",
                "alias chunk_id={} kind={} text={:?}",
                chunk_id, kind, text
            );
            sqlx::query(
                "INSERT OR IGNORE INTO chunk_aliases (chunk_id, alias, alias_kind) \
                 VALUES (?, ?, ?)",
            )
            .bind(chunk_id)
            .bind(text)
            .bind(kind)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
        }

        for id in &g.block_ids {
            sqlx::query("UPDATE text_blocks SET chunk_id = ? WHERE id = ?")
                .bind(chunk_id)
                .bind(id)
                .execute(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn link_proofs_to_theorems(pool: &SqlitePool, page_id: i64) -> Result<(), String> {
    let proofs = sqlx::query(
        "SELECT id, subject FROM chunks \
         WHERE page_id = ? AND chunk_type = 'proof' AND subject IS NOT NULL",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    if proofs.is_empty() {
        return Ok(());
    }

    let candidates = sqlx::query(
        "SELECT id, title FROM chunks \
         WHERE page_id = ? AND chunk_type != 'proof' AND title IS NOT NULL",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    for proof_row in &proofs {
        let proof_id: i64 = proof_row.get("id");
        let subject: String = proof_row.get("subject");
        let subject_lower = subject.trim().to_lowercase();

        // Prefer the longest matching title to avoid short false-positive matches.
        let best = candidates
            .iter()
            .filter_map(|c| {
                let title: String = c.get("title");
                let title_lower = title.trim().to_lowercase();
                if title_lower.contains(&subject_lower) || subject_lower.contains(&title_lower) {
                    Some((c.get::<i64, _>("id"), title.len()))
                } else {
                    None
                }
            })
            .max_by_key(|(_, len)| *len);

        if let Some((target_id, _)) = best {
            sqlx::query("UPDATE chunks SET proves_chunk_id = ? WHERE id = ?")
                .bind(target_id)
                .bind(proof_id)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            debug!(
                target: "gloss_lib::chunking",
                "linked proof chunk_id={} -> theorem chunk_id={} (subject={:?})",
                proof_id, target_id, subject
            );
        } else {
            debug!(
                target: "gloss_lib::chunking",
                "no theorem match for proof chunk_id={} subject={:?}",
                proof_id, subject
            );
        }
    }
    Ok(())
}

async fn fallback_one_chunk_per_block(
    pool: &SqlitePool,
    doc_id: i64,
    page_id: i64,
    blocks: &[PersistedBlock],
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    for b in blocks {
        let row = sqlx::query(
            "INSERT INTO chunks \
             (source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, ocr_text, ai_suggested) \
             VALUES (?, ?, 'explanation', ?, ?, ?, ?, ?, 0) RETURNING id",
        )
        .bind(doc_id)
        .bind(page_id)
        .bind(b.bbox_x)
        .bind(b.bbox_y)
        .bind(b.bbox_w)
        .bind(b.bbox_h)
        .bind(&b.text)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let chunk_id: i64 = row.get("id");
        sqlx::query("UPDATE text_blocks SET chunk_id = ? WHERE id = ?")
            .bind(chunk_id)
            .bind(b.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn union_bbox(blocks: &[&PersistedBlock]) -> (f32, f32, f32, f32) {
    let mut x0 = blocks[0].bbox_x;
    let mut y0 = blocks[0].bbox_y;
    let mut x1 = blocks[0].bbox_x + blocks[0].bbox_w;
    let mut y1 = blocks[0].bbox_y + blocks[0].bbox_h;
    for b in &blocks[1..] {
        if b.bbox_x < x0 {
            x0 = b.bbox_x;
        }
        if b.bbox_y < y0 {
            y0 = b.bbox_y;
        }
        if b.bbox_x + b.bbox_w > x1 {
            x1 = b.bbox_x + b.bbox_w;
        }
        if b.bbox_y + b.bbox_h > y1 {
            y1 = b.bbox_y + b.bbox_h;
        }
    }
    (x0, y0, x1 - x0, y1 - y0)
}

fn normalise_chunk_type(raw: &str) -> &'static str {
    let lower = raw.trim().to_lowercase();
    // Accept the old name gracefully.
    if lower == "other" {
        return "explanation";
    }
    for t in ALLOWED_TYPES {
        if lower == *t {
            return t;
        }
    }
    debug!(
        target: "gloss_lib::chunking",
        "unknown chunk_type {:?} from LLM; coercing to 'explanation'",
        raw
    );
    "explanation"
}

// ── ZAI block transcription ─────────────────────────────────────────────────

const TRANSCRIPTION_RENDER_WIDTH: u32 = 1500;
const MAX_ZAI_CONCURRENT: usize = 2;

/// Renders the page once, crops each block's bbox, transcribes via ZAI, persists
/// `transcribed_text` to the DB, and returns the blocks with the field populated.
/// Returns `Ok(original_blocks)` unchanged if ZAI is not configured.
async fn transcribe_blocks_for_page(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    abs_path: &PathBuf,
    page_index: usize,
    blocks: &[PersistedBlock],
    zai_transcription_semaphore: Arc<Semaphore>,
) -> Result<Vec<PersistedBlock>, String> {
    let zai_key = match settings::api_key_for_provider(pool, LlmProvider::Zai).await? {
        Some(key) => key,
        None => {
            debug!(
                target: "gloss_lib::chunking",
                "ZAI key not configured — skipping block transcription"
            );
            return Ok(blocks.to_vec());
        }
    };

    let zai = ZaiClient::with_api_key_and_model(Some(zai_key), None);

    let p = abs_path.clone();
    let (page_w_px, page_h_px, rgba_bytes) = pdfium
        .run(move |pdf| render_page_rgba(pdf, &p, page_index, TRANSCRIPTION_RENDER_WIDTH))
        .await
        .map_err(|e| format!("failed to render page for ZAI transcription: {e}"))?;

    let crops: Vec<(i64, Result<String, String>)> = blocks
        .iter()
        .map(|b| {
            (
                b.id,
                crop_and_encode_block(&rgba_bytes, page_w_px, page_h_px, b),
            )
        })
        .collect();

    let transcriptions: Vec<(i64, Result<String, String>)> = futures::stream::iter(crops)
        .map(|(id, crop_result)| {
            let zai = zai.clone();
            let zai_transcription_semaphore = Arc::clone(&zai_transcription_semaphore);
            async move {
                match crop_result {
                    Err(e) => (id, Err(e)),
                    Ok(crop_b64) => match zai_transcription_semaphore.acquire_owned().await {
                        Ok(_permit) => (
                            id,
                            zai.transcribe_block_image(&crop_b64)
                                .await
                                .map_err(|e| e.to_string()),
                        ),
                        Err(_) => (id, Err("ZAI transcription limiter closed".to_string())),
                    },
                }
            }
        })
        .buffer_unordered(MAX_ZAI_CONCURRENT)
        .collect()
        .await;

    let mut updated = blocks.to_vec();
    for (id, result) in transcriptions {
        match result {
            Ok(text) => {
                sqlx::query("UPDATE text_blocks SET transcribed_text = ? WHERE id = ?")
                    .bind(&text)
                    .bind(id)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(b) = updated.iter_mut().find(|b| b.id == id) {
                    b.transcribed_text = Some(text);
                }
            }
            Err(e) => {
                if e.contains("too small") || e.contains("degenerate") {
                    debug!(
                        target: "gloss_lib::chunking",
                        "block_id={} skipped ZAI: {}",
                        id, e
                    );
                } else {
                    warn!(
                        target: "gloss_lib::chunking",
                        "ZAI transcription failed for block_id={}: {}",
                        id, e
                    );
                }
            }
        }
    }

    Ok(updated)
}

async fn render_page_as_base64(
    pdfium: &PdfiumWorker,
    abs_path: &PathBuf,
    page_index: usize,
) -> Result<String, String> {
    let p = abs_path.clone();
    let (w, h, rgba_bytes) = pdfium
        .run(move |pdf| render_page_rgba(pdf, &p, page_index, TRANSCRIPTION_RENDER_WIDTH))
        .await
        .map_err(|e| format!("failed to render page {}: {}", page_index + 1, e))?;

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(w, h, rgba_bytes)
        .ok_or_else(|| format!("failed to create image buffer for page {}", page_index + 1))?;
    let dynamic = image::DynamicImage::ImageRgba8(img);

    let mut png_bytes = Vec::new();
    dynamic
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .map_err(|e| format!("failed to encode page {} as PNG: {}", page_index + 1, e))?;

    Ok(STANDARD.encode(&png_bytes))
}

/// Renders a PDF page to raw RGBA bytes. Runs synchronously on the pdfium worker thread.
fn render_page_rgba(
    pdfium: &Pdfium,
    abs_path: &PathBuf,
    page_index: usize,
    target_width: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let doc = pdfium
        .load_pdf_from_file(abs_path, None)
        .map_err(|e| e.to_string())?;
    let page = doc
        .pages()
        .get(page_index as i32)
        .map_err(|e| e.to_string())?;
    let width_px = target_width as i32;
    let height_px =
        ((target_width as f32) * (page.height().value / page.width().value)).round() as i32;
    let bitmap = page
        .render_with_config(
            &PdfRenderConfig::new()
                .set_target_width(width_px)
                .set_target_height(height_px),
        )
        .map_err(|e| e.to_string())?;
    Ok((
        bitmap.width() as u32,
        bitmap.height() as u32,
        bitmap.as_rgba_bytes(),
    ))
}

// ZAI requires at least this many pixels in each dimension — anything smaller
// is typically a page-number, stray glyph, or artefact that pdfium text handles fine.
const MIN_CROP_PX: u32 = 16;

/// Crops a block's bbox from pre-rendered full-page RGBA bytes and returns a JPEG data URI.
fn crop_and_encode_block(
    rgba_bytes: &[u8],
    page_width_px: u32,
    page_height_px: u32,
    block: &PersistedBlock,
) -> Result<String, String> {
    let x0 = (block.bbox_x * page_width_px as f32).round() as u32;
    let y0 = (block.bbox_y * page_height_px as f32).round() as u32;
    let x1 = ((block.bbox_x + block.bbox_w) * page_width_px as f32).round() as u32;
    let y1 = ((block.bbox_y + block.bbox_h) * page_height_px as f32).round() as u32;

    let x0 = x0.min(page_width_px);
    let y0 = y0.min(page_height_px);
    let x1 = x1.min(page_width_px);
    let y1 = y1.min(page_height_px);

    if x1 <= x0 || y1 <= y0 {
        return Err(format!(
            "degenerate block bbox: x={} y={} w={} h={}",
            block.bbox_x, block.bbox_y, block.bbox_w, block.bbox_h
        ));
    }

    let crop_w = x1 - x0;
    let crop_h = y1 - y0;

    debug!(
        target: "gloss_lib::chunking",
        "block_id={} crop {}x{} px (bbox x={:.3} y={:.3} w={:.3} h={:.3})",
        block.id, crop_w, crop_h,
        block.bbox_x, block.bbox_y, block.bbox_w, block.bbox_h
    );

    if crop_w < MIN_CROP_PX || crop_h < MIN_CROP_PX {
        return Err(format!(
            "block_id={} crop too small ({}x{} px) — skipping ZAI",
            block.id, crop_w, crop_h
        ));
    }

    let mut crop = Vec::with_capacity((crop_w * crop_h * 4) as usize);
    for row in y0..y1 {
        let row_start = ((row * page_width_px + x0) * 4) as usize;
        let row_end = row_start + (crop_w * 4) as usize;
        crop.extend_from_slice(&rgba_bytes[row_start..row_end]);
    }

    let rgba_img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(crop_w, crop_h, crop)
        .ok_or_else(|| "failed to create image buffer from block crop".to_string())?;

    // Convert RGBA → RGB; many OCR APIs reject alpha-channel PNGs.
    let rgb_img = image::DynamicImage::ImageRgba8(rgba_img).into_rgb8();

    let mut jpeg_bytes = Vec::new();
    rgb_img
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg_bytes),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "data:image/jpeg;base64,{}",
        STANDARD.encode(&jpeg_bytes)
    ))
}

/// After chunking, populates `formatted_body_md` for every non-noise chunk on the page
/// by concatenating block text in order, preferring `transcribed_text` when present.
async fn populate_formatted_body_from_transcriptions(
    pool: &SqlitePool,
    page_id: i64,
) -> Result<(), String> {
    let chunk_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM chunks WHERE page_id = ? AND chunk_type != 'noise'")
            .bind(page_id)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;

    for chunk_id in chunk_ids {
        let rows = sqlx::query(
            "SELECT transcribed_text, text \
             FROM text_blocks WHERE chunk_id = ? ORDER BY order_idx",
        )
        .bind(chunk_id)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;

        if rows.is_empty() {
            continue;
        }

        let mut used_transcribed_blocks: usize = 0;
        let mut segments: Vec<String> = Vec::new();
        for row in rows {
            let transcribed: Option<String> = row.get("transcribed_text");
            let extracted: String = row.get("text");

            if let Some(value) = transcribed
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                used_transcribed_blocks += 1;
                segments.push(value.to_string());
                continue;
            }

            let extracted_trimmed = extracted.trim();
            if !extracted_trimmed.is_empty() {
                segments.push(extracted_trimmed.to_string());
            }
        }

        if used_transcribed_blocks == 0 || segments.is_empty() {
            continue;
        }

        let body_md = segments.join("\n\n");

        if body_md.trim().is_empty() {
            continue;
        }

        sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
            .bind(&body_md)
            .bind(chunk_id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;

        debug!(
            target: "gloss_lib::chunking",
            "populated formatted_body_md for chunk_id={} ({} chars, {} transcribed blocks)",
            chunk_id, body_md.len(), used_transcribed_blocks
        );
    }

    Ok(())
}

fn emit_progress(app: &AppHandle, doc_id: i64, page_number: i64, phase: &'static str) {
    debug!(
        target: "gloss_lib::chunking",
        "emitting progress event doc_id={} page={} phase={}",
        doc_id,
        page_number,
        phase
    );
    let _ = app.emit(
        "chunking_progress",
        ProgressEvent {
            source_document_id: doc_id,
            page_number,
            phase,
        },
    );
}

// ── Pdfium text extraction ──────────────────────────────────────────────────

/// Extract paragraph-level text blocks from a single page.
/// Runs on the pdfium worker thread.
fn extract_blocks_from_page(
    pdfium: &Pdfium,
    abs_path: &PathBuf,
    page_num: usize,
) -> Result<Vec<RawBlock>, String> {
    let doc = pdfium
        .load_pdf_from_file(abs_path, None)
        .map_err(|e| e.to_string())?;
    let page = doc
        .pages()
        .get(page_num as i32)
        .map_err(|e| e.to_string())?;

    let page_w = page.width().value.max(1.0);
    let page_h = page.height().value.max(1.0);

    let text = page.text().map_err(|e| e.to_string())?;

    // Collect raw segments (lines-ish per pdfium's auto-merge) in top-origin normalized space.
    let mut lines: Vec<Line> = Vec::new();
    for seg in text.segments().iter() {
        let b = seg.bounds();
        let left = (b.left().value / page_w).clamp(0.0, 1.0);
        let right = (b.right().value / page_w).clamp(0.0, 1.0);
        // PDF coords: y=0 at bottom. Flip to top-origin: top_of_page == 0.
        let top = ((page_h - b.top().value) / page_h).clamp(0.0, 1.0);
        let bottom = ((page_h - b.bottom().value) / page_h).clamp(0.0, 1.0);
        let txt = seg.text().trim().to_string();
        if txt.is_empty() || right <= left || bottom <= top {
            continue;
        }
        lines.push(Line {
            left,
            right,
            top,
            bottom,
            text: txt,
        });
    }

    if lines.is_empty() {
        debug!(
            target: "gloss_lib::chunking",
            "page={} extraction produced no text segments; trying char-level fallback",
            page_num + 1
        );
        lines = extract_lines_from_chars(&text, page_w, page_h);
    }

    if lines.is_empty() {
        let all_text = text.all();
        let compact_text = all_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if !compact_text.is_empty() {
            debug!(
                target: "gloss_lib::chunking",
                "page={} char fallback produced no bounded glyphs; using full-page text fallback",
                page_num + 1
            );
            lines.push(Line {
                left: 0.0,
                right: 1.0,
                top: 0.0,
                bottom: 1.0,
                text: compact_text,
            });
        }
    }

    if lines.is_empty() {
        debug!(
            target: "gloss_lib::chunking",
            "page={} extraction produced no text",
            page_num + 1
        );
        return Ok(Vec::new());
    }

    let raw_segment_count = lines.len();

    // Merge same-line segments (same vertical band) into combined lines.
    lines.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let merged_lines = merge_into_lines(lines);

    // Group adjacent lines into paragraph-level blocks.
    let blocks = group_lines_into_blocks(merged_lines);
    debug!(
        target: "gloss_lib::chunking",
        "page={} extracted {} raw segments into {} paragraph blocks",
        page_num + 1,
        raw_segment_count,
        blocks.len()
    );

    Ok(blocks
        .into_iter()
        .enumerate()
        .map(|(i, b)| RawBlock {
            order_idx: i as i32,
            bbox_x: b.left,
            bbox_y: b.top,
            bbox_w: (b.right - b.left).max(0.0),
            bbox_h: (b.bottom - b.top).max(0.0),
            text: b.text,
        })
        .collect())
}

struct Line {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    text: String,
}

struct Glyph {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    ch: char,
}

fn extract_lines_from_chars(text: &PdfPageText<'_>, page_w: f32, page_h: f32) -> Vec<Line> {
    let mut glyphs: Vec<Glyph> = Vec::new();

    for c in text.chars().iter() {
        let Some(ch) = c.unicode_char() else {
            continue;
        };
        if ch == '\n' || ch == '\r' {
            continue;
        }
        if ch.is_control() && !ch.is_whitespace() {
            continue;
        }

        let bounds = c.tight_bounds().or_else(|_| c.loose_bounds());
        let Ok(bounds) = bounds else {
            continue;
        };

        let left = (bounds.left().value / page_w).clamp(0.0, 1.0);
        let right = (bounds.right().value / page_w).clamp(0.0, 1.0);
        let top = ((page_h - bounds.top().value) / page_h).clamp(0.0, 1.0);
        let bottom = ((page_h - bounds.bottom().value) / page_h).clamp(0.0, 1.0);
        if right <= left || bottom <= top {
            continue;
        }

        glyphs.push(Glyph {
            left,
            right,
            top,
            bottom,
            ch,
        });
    }

    if glyphs.is_empty() {
        return Vec::new();
    }

    glyphs.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.left
                    .partial_cmp(&b.left)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut grouped: Vec<Vec<Glyph>> = Vec::new();
    for glyph in glyphs {
        let glyph_center = (glyph.top + glyph.bottom) * 0.5;
        let glyph_h = (glyph.bottom - glyph.top).max(1e-4);
        let target_line = grouped.iter().position(|line| {
            let line_center =
                line.iter().map(|g| (g.top + g.bottom) * 0.5).sum::<f32>() / line.len() as f32;
            let line_h = line
                .iter()
                .map(|g| (g.bottom - g.top).max(1e-4))
                .sum::<f32>()
                / line.len() as f32;
            (line_center - glyph_center).abs() <= glyph_h.min(line_h) * 0.8
        });

        if let Some(index) = target_line {
            grouped[index].push(glyph);
        } else {
            grouped.push(vec![glyph]);
        }
    }

    let mut lines: Vec<Line> = Vec::new();
    for mut line in grouped {
        line.sort_by(|a, b| {
            a.left
                .partial_cmp(&b.left)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut left = line[0].left;
        let mut right = line[0].right;
        let mut top = line[0].top;
        let mut bottom = line[0].bottom;
        let avg_glyph_w = line
            .iter()
            .map(|g| (g.right - g.left).max(1e-4))
            .sum::<f32>()
            / line.len() as f32;
        let mut prev_right: Option<f32> = None;
        let mut text = String::new();

        for glyph in line {
            if let Some(prev) = prev_right {
                let gap = glyph.left - prev;
                if gap > avg_glyph_w * 0.6 && !text.ends_with(' ') && !text.is_empty() {
                    text.push(' ');
                }
            }

            if glyph.ch.is_whitespace() {
                if !text.ends_with(' ') && !text.is_empty() {
                    text.push(' ');
                }
            } else {
                text.push(glyph.ch);
            }

            left = left.min(glyph.left);
            right = right.max(glyph.right);
            top = top.min(glyph.top);
            bottom = bottom.max(glyph.bottom);
            prev_right = Some(glyph.right);
        }

        let compact_text = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if compact_text.is_empty() {
            continue;
        }

        lines.push(Line {
            left,
            right,
            top,
            bottom,
            text: compact_text,
        });
    }

    lines.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    lines
}

fn merge_into_lines(segments: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for seg in segments {
        let seg_h = (seg.bottom - seg.top).max(1e-4);
        let seg_center = (seg.top + seg.bottom) * 0.5;

        let merge_into = out.iter().position(|existing| {
            let ex_h = (existing.bottom - existing.top).max(1e-4);
            let ex_center = (existing.top + existing.bottom) * 0.5;
            // Same line if vertical centers are within half a line height.
            (ex_center - seg_center).abs() < seg_h.min(ex_h) * 0.6
        });

        if let Some(i) = merge_into {
            let e = &mut out[i];
            e.left = e.left.min(seg.left);
            e.right = e.right.max(seg.right);
            e.top = e.top.min(seg.top);
            e.bottom = e.bottom.max(seg.bottom);
            // Join text left-to-right.
            if seg.left >= e.right - 1e-4 {
                e.text.push(' ');
                e.text.push_str(&seg.text);
            } else if seg.right <= e.left + 1e-4 {
                let combined = format!("{} {}", seg.text, e.text);
                e.text = combined;
            } else {
                e.text.push(' ');
                e.text.push_str(&seg.text);
            }
        } else {
            out.push(seg);
        }
    }
    out.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn group_lines_into_blocks(lines: Vec<Line>) -> Vec<Line> {
    if lines.is_empty() {
        return Vec::new();
    }

    // Median line height informs the paragraph-gap threshold.
    let mut heights: Vec<f32> = lines.iter().map(|l| (l.bottom - l.top).max(1e-4)).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_h = heights[heights.len() / 2];

    let gap_threshold = median_h * 0.75;

    let mut blocks: Vec<Line> = Vec::new();
    let mut current: Option<Line> = None;

    for line in lines {
        match current.take() {
            None => current = Some(line),
            Some(mut acc) => {
                let gap = line.top - acc.bottom;
                let x_overlap = (acc.right.min(line.right) - acc.left.max(line.left)) > 0.0;

                if gap <= gap_threshold && x_overlap {
                    // Same block: extend accumulator.
                    acc.left = acc.left.min(line.left);
                    acc.right = acc.right.max(line.right);
                    acc.top = acc.top.min(line.top);
                    acc.bottom = acc.bottom.max(line.bottom);
                    acc.text.push('\n');
                    acc.text.push_str(&line.text);
                    current = Some(acc);
                } else {
                    blocks.push(acc);
                    current = Some(line);
                }
            }
        }
    }
    if let Some(c) = current {
        blocks.push(c);
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_instruction_page_ranges() {
        assert_eq!(
            resolve_instruction_page_range(Some(1), Some(1)).unwrap(),
            Some(InstructionPageRange {
                start_page: 1,
                end_page: 1,
            })
        );
        assert_eq!(resolve_instruction_page_range(None, None).unwrap(), None);
        assert!(resolve_instruction_page_range(Some(0), Some(1)).is_err());
        assert!(resolve_instruction_page_range(Some(4), Some(2)).is_err());
        assert!(resolve_instruction_page_range(Some(1), None).is_err());
    }
}

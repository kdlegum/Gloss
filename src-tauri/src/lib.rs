mod chunking;
mod deepseek;
mod gemini;
mod llm;
mod ollama;
mod openai;
mod references;
mod settings;
mod zai;

use crate::deepseek::DeepSeekClient;
use crate::gemini::GeminiClient;
use crate::llm::{
    ChunkBodyPrompt, ChunkBodyResult, ChunkChatMessage, ChunkChatPrompt, LlmError, LlmProvider,
};
use crate::ollama::OllamaClient;
use crate::openai::OpenAiClient;
use crate::zai::ZaiClient;
use log::info;
use pdfium_render::prelude::*;
use percent_encoding::percent_decode_str;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::{FilePath, FsExt};
use tauri_plugin_log::{Target, TargetKind, TimezoneStrategy};

// ── Pdfium worker ────────────────────────────────────────────────────────────
// pdfium_render's Pdfium is !Send, so it must live on a single dedicated thread.
// All PDF work is dispatched to that thread via a channel.

type PdfJob = Box<dyn FnOnce(&Pdfium) + Send + 'static>;

#[derive(Clone)]
pub struct PdfiumWorker(std::sync::mpsc::SyncSender<PdfJob>);

impl PdfiumWorker {
    /// Spawn the worker thread and bind pdfium.
    fn spawn(lib_path: std::path::PathBuf) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PdfJob>(64);
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || {
                let pdfium = Pdfium::new(
                    Pdfium::bind_to_library(&lib_path).expect("failed to bind pdfium library"),
                );
                for job in rx {
                    job(&pdfium);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self(tx))
    }

    /// Spawn the worker thread, binding pdfium from the system loader (Android).
    fn spawn_system() -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PdfJob>(64);
        std::thread::Builder::new()
            .name("pdfium-worker".into())
            .spawn(move || {
                let pdfium = Pdfium::new(
                    Pdfium::bind_to_system_library().expect("failed to bind pdfium system library"),
                );
                for job in rx {
                    job(&pdfium);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self(tx))
    }

    /// Run a closure on the pdfium thread and await its result.
    pub async fn run<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Pdfium) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let job: PdfJob = Box::new(move |pdfium| {
            let _ = done_tx.send(f(pdfium));
        });
        self.0
            .send(job)
            .map_err(|_| "pdfium worker closed".to_string())?;
        done_rx
            .await
            .map_err(|_| "pdfium worker dropped result".to_string())?
    }
}

// ── Rendered page ────────────────────────────────────────────────────────────
// Raw RGBA bytes + dimensions. Sent directly to the frontend so it can build
// an ImageData without any PNG encode/decode round-trip.
#[derive(serde::Serialize, Clone)]
struct RenderedPage {
    data: Vec<u8>,
    width: u32,
    height: u32,
    page_width_points: f32,
    page_height_points: f32,
}

// ── PDF render cache ─────────────────────────────────────────────────────────
// Keyed by (relative_path, page_number, target_width_px).
// Stores raw RGBA renders so we never re-rasterize the same page twice.
const PDF_CACHE_MAX: usize = 20;

struct PdfCache {
    order: Vec<(String, usize, u32)>,
    data: HashMap<(String, usize, u32), Arc<RenderedPage>>,
}

impl PdfCache {
    fn new() -> Self {
        Self {
            order: Vec::new(),
            data: HashMap::new(),
        }
    }

    fn get(&mut self, key: &(String, usize, u32)) -> Option<Arc<RenderedPage>> {
        if let Some(val) = self.data.get(key) {
            // Move to front (most recently used)
            self.order.retain(|k| k != key);
            self.order.insert(0, key.clone());
            return Some(Arc::clone(val));
        }
        None
    }

    fn put(&mut self, key: (String, usize, u32), value: RenderedPage) -> Arc<RenderedPage> {
        if self.data.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.order.len() >= PDF_CACHE_MAX {
            if let Some(evict) = self.order.pop() {
                self.data.remove(&evict);
            }
        }
        self.order.insert(0, key.clone());
        let arc = Arc::new(value);
        self.data.insert(key, Arc::clone(&arc));
        arc
    }
}

struct AppState {
    pdf_cache: Mutex<PdfCache>,
    pdfium: PdfiumWorker,
    chunking_jobs: Arc<Mutex<HashSet<(i64, i64)>>>,
    chat_streams: Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
}

#[derive(Clone)]
struct ChatStreamHandle {
    cancelled: Arc<AtomicBool>,
    chunk_id: i64,
}

fn spawn_chunking_job(
    app: tauri::AppHandle,
    pool: SqlitePool,
    state: &AppState,
    doc_id: i64,
    page_number: i64,
    provider: LlmProvider,
    model_override: Option<String>,
) -> Result<bool, String> {
    if !begin_chunking_job(state, doc_id, page_number)? {
        return Ok(false);
    }

    let chunking_jobs = Arc::clone(&state.chunking_jobs);
    let pdfium = state.pdfium.clone();
    info!(
        target: "gloss_lib::chunking",
        "spawning tracked chunking job for doc_id={} page={} provider={}",
        doc_id,
        page_number,
        provider
    );
    tokio::spawn(async move {
        chunking::run_for_page(
            pool,
            pdfium,
            app,
            doc_id,
            page_number,
            provider,
            model_override,
        )
        .await;
        finish_chunking_job(&chunking_jobs, doc_id, page_number);
    });
    Ok(true)
}

fn begin_chunking_job(state: &AppState, doc_id: i64, page_number: i64) -> Result<bool, String> {
    let mut jobs = state.chunking_jobs.lock().map_err(|e| e.to_string())?;
    if !jobs.insert((doc_id, page_number)) {
        info!(
            target: "gloss_lib::chunking",
            "chunking job already running for doc_id={} page={}",
            doc_id,
            page_number
        );
        return Ok(false);
    }
    Ok(true)
}

fn finish_chunking_job(
    chunking_jobs: &Arc<Mutex<HashSet<(i64, i64)>>>,
    doc_id: i64,
    page_number: i64,
) {
    if let Ok(mut jobs) = chunking_jobs.lock() {
        jobs.remove(&(doc_id, page_number));
    }
}

fn begin_chat_stream(
    state: &AppState,
    request_id: &str,
    chunk_id: i64,
) -> Result<Arc<AtomicBool>, String> {
    let mut streams = state.chat_streams.lock().map_err(|e| e.to_string())?;
    if streams.contains_key(request_id) {
        return Err(format!("chat stream {} already active", request_id));
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    streams.insert(
        request_id.to_string(),
        ChatStreamHandle {
            cancelled: Arc::clone(&cancelled),
            chunk_id,
        },
    );
    Ok(cancelled)
}

fn finish_chat_stream(
    chat_streams: &Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
    request_id: &str,
) -> Option<ChatStreamHandle> {
    chat_streams
        .lock()
        .ok()
        .and_then(|mut streams| streams.remove(request_id))
}

fn cancel_chat_stream(
    chat_streams: &Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
    request_id: &str,
) -> Result<Option<ChatStreamHandle>, String> {
    let mut streams = chat_streams.lock().map_err(|e| e.to_string())?;
    let handle = streams.remove(request_id);
    if let Some(active) = &handle {
        active.cancelled.store(true, Ordering::Relaxed);
    }
    Ok(handle)
}

#[derive(serde::Serialize, Clone)]
struct ChunkAiStreamEvent {
    request_id: String,
    chunk_id: i64,
    phase: &'static str,
    delta: Option<String>,
    error: Option<String>,
}

fn emit_chunk_ai_stream(
    app: &tauri::AppHandle,
    request_id: &str,
    chunk_id: i64,
    phase: &'static str,
    delta: Option<String>,
    error: Option<String>,
) {
    let _ = app.emit(
        "chunk_ai_stream",
        ChunkAiStreamEvent {
            request_id: request_id.to_string(),
            chunk_id,
            phase,
            delta,
            error,
        },
    );
}

fn validate_chunking_provider(provider: LlmProvider) -> Result<(), String> {
    if provider.supports_chunking() {
        Ok(())
    } else {
        Err(format!("provider {} does not support chunking", provider))
    }
}

fn validate_chat_provider(provider: LlmProvider) -> Result<(), String> {
    if provider.supports_chat() {
        Ok(())
    } else {
        Err(format!("provider {} does not support chat", provider))
    }
}

fn validate_vision_provider(provider: LlmProvider) -> Result<(), String> {
    if provider.supports_vision() {
        Ok(())
    } else {
        Err(format!(
            "provider {} does not support vision transcription",
            provider
        ))
    }
}

fn normalize_model_override(model: Option<String>) -> Option<String> {
    model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(serde::Deserialize, serde::Serialize, bincode::Encode, bincode::Decode, Clone, Copy)]
struct Point {
    x: f32,
    y: f32,
}

struct StrokeBounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

enum NoteSurfaceOwner {
    Page(i64),
    Chunk(i64),
}

enum NoteSurfaceKind {
    PageNotes,
    ChunkNotes,
}

impl NoteSurfaceKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PageNotes => "page_notes",
            Self::ChunkNotes => "chunk_notes",
        }
    }
}

/// What the frontend sends for a single stroke.
#[derive(serde::Deserialize)]
struct StrokeInput {
    colour: String,
    thickness: f32,
    points: Vec<Point>,
    #[serde(alias = "chunkId")]
    chunk_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct StrokeOutput {
    id: i64,
    colour: String,
    thickness: f32,
    points: Vec<Point>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    chunk_id: Option<i64>,
}

#[derive(serde::Deserialize)]
struct SurfaceStrokeInput {
    colour: String,
    thickness: f32,
    points: Vec<Point>,
}

#[derive(serde::Serialize)]
struct SurfaceStrokeOutput {
    id: i64,
    colour: String,
    thickness: f32,
    points: Vec<Point>,
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

#[derive(serde::Serialize)]
struct SourceDocument {
    id: i64,
    title: String,
    file_path: String,
}

fn stroke_bounds(points: &[Point]) -> Result<StrokeBounds, String> {
    if points.is_empty() {
        return Err("stroke has no points".into());
    }

    let mut min_x = points[0].x;
    let mut min_y = points[0].y;
    let mut max_x = min_x;
    let mut max_y = min_y;

    for p in &points[1..] {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }

    Ok(StrokeBounds {
        min_x,
        min_y,
        max_x,
        max_y,
    })
}

fn encode_stroke_points(points: &[Point]) -> Result<Vec<u8>, String> {
    bincode::encode_to_vec(points, bincode::config::standard()).map_err(|e| e.to_string())
}

fn decode_stroke_points(data: Vec<u8>) -> Result<Vec<Point>, String> {
    bincode::decode_from_slice::<Vec<Point>, _>(&data, bincode::config::standard())
        .map(|(points, _)| points)
        .map_err(|e| e.to_string())
}

async fn get_or_create_note_surface_id(
    owner: NoteSurfaceOwner,
    kind: NoteSurfaceKind,
    pool: &SqlitePool,
) -> Result<i64, String> {
    match (owner, kind) {
        (NoteSurfaceOwner::Page(page_id), NoteSurfaceKind::PageNotes) => {
            sqlx::query("INSERT OR IGNORE INTO note_surfaces (page_id, kind) VALUES (?, ?)")
                .bind(page_id)
                .bind(NoteSurfaceKind::PageNotes.as_str())
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;

            let row = sqlx::query("SELECT id FROM note_surfaces WHERE page_id = ? AND kind = ?")
                .bind(page_id)
                .bind(NoteSurfaceKind::PageNotes.as_str())
                .fetch_one(pool)
                .await
                .map_err(|e| e.to_string())?;

            Ok(row.get("id"))
        }
        (NoteSurfaceOwner::Chunk(chunk_id), NoteSurfaceKind::ChunkNotes) => {
            sqlx::query("INSERT OR IGNORE INTO note_surfaces (chunk_id, kind) VALUES (?, ?)")
                .bind(chunk_id)
                .bind(NoteSurfaceKind::ChunkNotes.as_str())
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;

            let row = sqlx::query("SELECT id FROM note_surfaces WHERE chunk_id = ? AND kind = ?")
                .bind(chunk_id)
                .bind(NoteSurfaceKind::ChunkNotes.as_str())
                .fetch_one(pool)
                .await
                .map_err(|e| e.to_string())?;

            Ok(row.get("id"))
        }
        _ => Err("invalid note surface owner for kind".into()),
    }
}

#[derive(serde::Serialize)]
struct ChunkInfo {
    id: i64,
    chunk_type: String,
    bbox_x: f32,
    bbox_y: f32,
    bbox_w: f32,
    bbox_h: f32,
    status: String,
    title: Option<String>,
    subject: Option<String>,
    proves_chunk_id: Option<i64>,
    ocr_text: Option<String>,
    formatted_body_md: Option<String>,
    glossary_md: Option<String>,
}

#[derive(serde::Serialize)]
struct ChunkForTranscription {
    id: i64,
    source_document_id: i64,
    page_number: i64,
    chunk_type: String,
    bbox_x: f32,
    bbox_y: f32,
    bbox_w: f32,
    bbox_h: f32,
    status: String,
    title: Option<String>,
    subject: Option<String>,
    proves_chunk_id: Option<i64>,
    ocr_text: Option<String>,
    formatted_body_md: Option<String>,
    glossary_md: Option<String>,
}

#[tauri::command]
async fn get_chunks_for_page(
    page_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<ChunkInfo>, String> {
    let rows = sqlx::query(
        "SELECT id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, status, \
                title, subject, proves_chunk_id, ocr_text, formatted_body_md, glossary_md \
         FROM chunks WHERE page_id = ? ORDER BY bbox_y, bbox_x",
    )
    .bind(page_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| ChunkInfo {
            id: r.get("id"),
            chunk_type: r.get("chunk_type"),
            bbox_x: r.get("bbox_x"),
            bbox_y: r.get("bbox_y"),
            bbox_w: r.get("bbox_w"),
            bbox_h: r.get("bbox_h"),
            status: r.get("status"),
            title: r.get("title"),
            subject: r.get("subject"),
            proves_chunk_id: r.get("proves_chunk_id"),
            ocr_text: r.get("ocr_text"),
            formatted_body_md: r.get("formatted_body_md"),
            glossary_md: r.get("glossary_md"),
        })
        .collect())
}

#[tauri::command]
async fn save_chunk_glossary(
    chunk_id: i64,
    glossary_md: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let trimmed = glossary_md
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    sqlx::query("UPDATE chunks SET glossary_md = ? WHERE id = ?")
        .bind(trimmed.as_deref())
        .bind(chunk_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_chunk_for_transcription(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkForTranscription, String> {
    let r = sqlx::query(
        "SELECT c.id, c.source_document_id, p.page_number, c.chunk_type, \
                c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.status, \
                c.title, c.subject, c.proves_chunk_id, c.ocr_text, c.formatted_body_md, \
                c.glossary_md \
         FROM chunks c \
         JOIN pages p ON p.id = c.page_id \
         WHERE c.id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    Ok(ChunkForTranscription {
        id: r.get("id"),
        source_document_id: r.get("source_document_id"),
        page_number: r.get("page_number"),
        chunk_type: r.get("chunk_type"),
        bbox_x: r.get("bbox_x"),
        bbox_y: r.get("bbox_y"),
        bbox_w: r.get("bbox_w"),
        bbox_h: r.get("bbox_h"),
        status: r.get("status"),
        title: r.get("title"),
        subject: r.get("subject"),
        proves_chunk_id: r.get("proves_chunk_id"),
        ocr_text: r.get("ocr_text"),
        formatted_body_md: r.get("formatted_body_md"),
        glossary_md: r.get("glossary_md"),
    })
}

#[tauri::command]
async fn get_ai_settings_state(
    pool: tauri::State<'_, SqlitePool>,
) -> Result<settings::AiSettingsState, String> {
    settings::state(pool.inner()).await
}

#[tauri::command]
async fn save_ai_api_keys(
    openai_api_key: Option<String>,
    gemini_api_key: Option<String>,
    deepseek_api_key: Option<String>,
    zai_api_key: Option<String>,
    clear_openai_api_key: Option<bool>,
    clear_gemini_api_key: Option<bool>,
    clear_deepseek_api_key: Option<bool>,
    clear_zai_api_key: Option<bool>,
    setup_complete: Option<bool>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<settings::AiSettingsState, String> {
    settings::save_ai_api_keys(
        pool.inner(),
        openai_api_key,
        gemini_api_key,
        deepseek_api_key,
        zai_api_key,
        clear_openai_api_key.unwrap_or(false),
        clear_gemini_api_key.unwrap_or(false),
        clear_deepseek_api_key.unwrap_or(false),
        clear_zai_api_key.unwrap_or(false),
        setup_complete.unwrap_or(false),
    )
    .await
}

async fn load_pretranscribed_chunk_body(
    pool: &SqlitePool,
    chunk_id: i64,
) -> Result<Option<String>, String> {
    let rows = sqlx::query(
        "SELECT transcribed_text, text \
         FROM text_blocks WHERE chunk_id = ? ORDER BY order_idx",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Ok(None);
    }

    let mut used_transcribed = false;
    let mut segments = Vec::with_capacity(rows.len());
    for row in rows {
        let transcribed: Option<String> = row.get("transcribed_text");
        let extracted: String = row.get("text");

        if let Some(value) = transcribed
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            used_transcribed = true;
            segments.push(value.to_string());
            continue;
        }

        let extracted_trimmed = extracted.trim();
        if !extracted_trimmed.is_empty() {
            segments.push(extracted_trimmed.to_string());
        }
    }

    if !used_transcribed || segments.is_empty() {
        return Ok(None);
    }

    Ok(Some(segments.join("\n\n")))
}

#[tauri::command]
async fn generate_chunk_formatted_body(
    chunk_id: i64,
    provider: String,
    model: Option<String>,
    image_base64: String,
    force: Option<bool>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkBodyResult, String> {
    let provider = provider.parse::<LlmProvider>()?;
    validate_vision_provider(provider)?;
    let model = normalize_model_override(model);
    let force = force.unwrap_or(false);

    let row = sqlx::query(
        "SELECT chunk_type, title, subject, formatted_body_md \
         FROM chunks WHERE id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_type: String = row.get("chunk_type");
    let title: Option<String> = row.get("title");
    let subject: Option<String> = row.get("subject");
    let formatted_body_md: Option<String> = row.get("formatted_body_md");

    if !force {
        if let Some(existing) = formatted_body_md
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(ChunkBodyResult {
                body_markdown: existing.to_string(),
            });
        }

        if let Some(reused) = load_pretranscribed_chunk_body(pool.inner(), chunk_id).await? {
            info!(
                target: "gloss_lib::llm",
                "generate_chunk_formatted_body chunk_id={} reused block transcription",
                chunk_id
            );
            sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
                .bind(&reused)
                .bind(chunk_id)
                .execute(pool.inner())
                .await
                .map_err(|e| e.to_string())?;
            if let Err(e) = references::reindex_chunk_references(pool.inner(), chunk_id).await {
                log::warn!(
                    target: "gloss_lib::references",
                    "failed to reindex references for chunk_id={}: {}",
                    chunk_id, e
                );
            }
            return Ok(ChunkBodyResult {
                body_markdown: reused,
            });
        }
    }

    if image_base64.trim().is_empty() {
        return Err("image payload was empty".into());
    }

    info!(
        target: "gloss_lib::llm",
        "generate_chunk_formatted_body chunk_id={} provider={} force={} chunk_type={}",
        chunk_id,
        provider,
        force,
        chunk_type
    );

    let prompt = ChunkBodyPrompt {
        chunk_type: &chunk_type,
        title: title.as_deref(),
        subject: subject.as_deref(),
    };
    let configured_api_key = settings::api_key_for_provider(pool.inner(), provider).await?;
    let result = match provider {
        LlmProvider::Ollama => OllamaClient::with_vision_model(model.clone())
            .transcribe_chunk_body(&prompt, &image_base64)
            .await
            .map_err(|e| e.to_string())?,
        LlmProvider::OpenAI => {
            OpenAiClient::with_api_key_and_model(configured_api_key, model.clone())
                .transcribe_chunk_body(&prompt, &image_base64)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::Gemini => {
            GeminiClient::with_api_key_and_model(configured_api_key, model.clone())
                .transcribe_chunk_body(&prompt, &image_base64)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::DeepSeek => {
            return Err("provider deepseek does not support vision transcription".to_string())
        }
        LlmProvider::Zai => ZaiClient::with_api_key_and_model(configured_api_key, model)
            .transcribe_chunk_body(&prompt, &image_base64)
            .await
            .map_err(|e| e.to_string())?,
    };

    sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
        .bind(&result.body_markdown)
        .bind(chunk_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    if let Err(e) = references::reindex_chunk_references(pool.inner(), chunk_id).await {
        log::warn!(
            target: "gloss_lib::references",
            "failed to reindex references for chunk_id={}: {}",
            chunk_id, e
        );
    }

    Ok(result)
}

struct ChunkChatContext {
    book_title: String,
    chunk_type: String,
    title: Option<String>,
    subject: Option<String>,
    body_markdown: String,
}

fn sanitize_chunk_chat_history(
    history: Vec<ChunkChatMessage>,
) -> Result<Vec<ChunkChatMessage>, String> {
    let mut cleaned = Vec::with_capacity(history.len());
    for message in history {
        let role = message.role.trim();
        if role != "user" && role != "assistant" {
            return Err(format!("invalid chat role {:?}", message.role));
        }
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        cleaned.push(ChunkChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        });
    }

    if cleaned.is_empty() {
        return Err("chat history was empty".into());
    }
    Ok(cleaned)
}

async fn load_chunk_chat_context(
    pool: &SqlitePool,
    chunk_id: i64,
) -> Result<ChunkChatContext, String> {
    let row = sqlx::query(
        "SELECT sd.title AS book_title, c.chunk_type, c.title, c.subject, \
                COALESCE(NULLIF(TRIM(c.formatted_body_md), ''), NULLIF(TRIM(c.ocr_text), '')) AS body_markdown \
         FROM chunks c \
         JOIN source_documents sd ON sd.id = c.source_document_id \
         WHERE c.id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let body_markdown: Option<String> = row.get("body_markdown");
    let body_markdown = body_markdown
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "No chunk text is available for AI chat yet.".to_string())?;

    Ok(ChunkChatContext {
        book_title: row.get("book_title"),
        chunk_type: row.get("chunk_type"),
        title: row.get("title"),
        subject: row.get("subject"),
        body_markdown,
    })
}

async fn run_chunk_ai_stream(
    app: tauri::AppHandle,
    pool: SqlitePool,
    chat_streams: Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
    request_id: String,
    chunk_id: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    history: Vec<ChunkChatMessage>,
    cancelled: Arc<AtomicBool>,
) {
    let result = async {
        if cancelled.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }

        let context = load_chunk_chat_context(&pool, chunk_id)
            .await
            .map_err(LlmError::Config)?;
        let prompt = ChunkChatPrompt {
            book_title: &context.book_title,
            chunk_type: &context.chunk_type,
            title: context.title.as_deref(),
            subject: context.subject.as_deref(),
            body_markdown: &context.body_markdown,
        };
        let configured_api_key = settings::api_key_for_provider(&pool, provider)
            .await
            .map_err(LlmError::Config)?;

        let emit_delta = |delta: &str| -> Result<(), LlmError> {
            if cancelled.load(Ordering::Relaxed) {
                return Err(LlmError::Cancelled);
            }
            emit_chunk_ai_stream(
                &app,
                &request_id,
                chunk_id,
                "delta",
                Some(delta.to_string()),
                None,
            );
            Ok(())
        };
        let should_cancel = || cancelled.load(Ordering::Relaxed);

        match provider {
            LlmProvider::Ollama => {
                OllamaClient::with_text_model(model_override.clone())
                    .stream_chunk_chat(&prompt, &history, emit_delta, should_cancel)
                    .await
            }
            LlmProvider::OpenAI => {
                OpenAiClient::with_api_key_and_model(configured_api_key, model_override.clone())
                    .stream_chunk_chat(&prompt, &history, emit_delta, should_cancel)
                    .await
            }
            LlmProvider::Gemini => {
                GeminiClient::with_api_key_and_model(configured_api_key, model_override.clone())
                    .stream_chunk_chat(&prompt, &history, emit_delta, should_cancel)
                    .await
            }
            LlmProvider::DeepSeek => {
                DeepSeekClient::with_api_key_and_model(configured_api_key, model_override.clone())
                    .stream_chunk_chat(&prompt, &history, emit_delta, should_cancel)
                    .await
            }
            LlmProvider::Zai => Err(LlmError::Config(
                "provider zai does not support chat".to_string(),
            )),
        }
    }
    .await;

    match result {
        Ok(_) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(&app, &request_id, chunk_id, "completed", None, None);
            }
        }
        Err(LlmError::Cancelled) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(&app, &request_id, chunk_id, "cancelled", None, None);
            }
        }
        Err(err) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(
                    &app,
                    &request_id,
                    chunk_id,
                    "error",
                    None,
                    Some(err.to_string()),
                );
            }
        }
    }
}

#[tauri::command]
async fn start_chunk_ai_stream(
    request_id: String,
    chunk_id: i64,
    provider: String,
    model: Option<String>,
    history: Vec<ChunkChatMessage>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let request_id = request_id.trim().to_string();
    if request_id.is_empty() {
        return Err("request_id was empty".into());
    }
    if chunk_id < 1 {
        return Err(format!("invalid chunk_id {}", chunk_id));
    }

    let provider = provider.parse::<LlmProvider>()?;
    validate_chat_provider(provider)?;
    let model = normalize_model_override(model);
    let history = sanitize_chunk_chat_history(history)?;
    let cancelled = begin_chat_stream(state.inner(), &request_id, chunk_id)?;
    let chat_streams = Arc::clone(&state.chat_streams);
    let pool = pool.inner().clone();

    tokio::spawn(run_chunk_ai_stream(
        app,
        pool,
        chat_streams,
        request_id,
        chunk_id,
        provider,
        model,
        history,
        cancelled,
    ));

    Ok(())
}

#[tauri::command]
async fn cancel_chunk_ai_stream(
    request_id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let request_id = request_id.trim().to_string();
    if request_id.is_empty() {
        return Err("request_id was empty".into());
    }

    if let Some(handle) = cancel_chat_stream(&state.chat_streams, &request_id)? {
        emit_chunk_ai_stream(&app, &request_id, handle.chunk_id, "cancelled", None, None);
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ChunkReferenceView {
    matched_text: String,
    span_start: i64,
    span_end: i64,
    ref_kind: String,
    target_id: Option<i64>,
    target_title: Option<String>,
    target_type: Option<String>,
}

#[tauri::command]
async fn get_chunk_references(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<ChunkReferenceView>, String> {
    let rows = sqlx::query(
        "SELECT r.matched_text, r.span_start, r.span_end, r.ref_kind, \
                c.id AS target_id, c.title AS target_title, c.chunk_type AS target_type \
         FROM chunk_references r \
         LEFT JOIN chunk_aliases a ON a.alias = r.matched_text \
         LEFT JOIN chunks c ON c.id = a.chunk_id \
             AND c.source_document_id = (SELECT source_document_id FROM chunks WHERE id = r.source_chunk_id) \
             AND c.id != r.source_chunk_id \
         WHERE r.source_chunk_id = ? \
         ORDER BY r.span_start",
    )
    .bind(chunk_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    // The LEFT JOIN may produce multiple rows per reference if an alias happens
    // to be shared by more than one chunk (shouldn't happen within a document
    // in practice, but be defensive). Collapse by (span_start, span_end):
    // prefer the first resolved match, else the first unresolved row.
    let mut seen: std::collections::HashMap<(i64, i64), ChunkReferenceView> =
        std::collections::HashMap::new();
    for r in rows {
        let span_start: i64 = r.get("span_start");
        let span_end: i64 = r.get("span_end");
        let target_id: Option<i64> = r.get("target_id");
        let key = (span_start, span_end);
        let view = ChunkReferenceView {
            matched_text: r.get("matched_text"),
            span_start,
            span_end,
            ref_kind: r.get("ref_kind"),
            target_id,
            target_title: r.get("target_title"),
            target_type: r.get("target_type"),
        };
        match seen.get(&key) {
            None => {
                seen.insert(key, view);
            }
            Some(existing) => {
                if existing.target_id.is_none() && view.target_id.is_some() {
                    seen.insert(key, view);
                }
            }
        }
    }
    let mut out: Vec<ChunkReferenceView> = seen.into_values().collect();
    out.sort_by_key(|v| v.span_start);
    Ok(out)
}

#[derive(serde::Serialize)]
struct ChunkPreview {
    id: i64,
    chunk_type: String,
    title: Option<String>,
    subject: Option<String>,
    status: String,
    body_preview: Option<String>,
    has_formatted_body: bool,
    has_self_explanation: bool,
}

#[tauri::command]
async fn get_chunk_preview(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkPreview, String> {
    let row = sqlx::query(
        "SELECT id, chunk_type, title, subject, status, formatted_body_md, ocr_text \
         FROM chunks WHERE id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let formatted: Option<String> = row.get("formatted_body_md");
    let ocr: Option<String> = row.get("ocr_text");
    let has_formatted_body = formatted
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let body_source = formatted
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| ocr.as_deref().map(str::trim).filter(|v| !v.is_empty()));
    let body_preview = body_source.map(truncate_markdown_preview);

    Ok(ChunkPreview {
        id: row.get("id"),
        chunk_type: row.get("chunk_type"),
        title: row.get("title"),
        subject: row.get("subject"),
        status: row.get("status"),
        body_preview,
        has_formatted_body,
        // Stub — flips to a real EXISTS lookup once the glossary table ships.
        has_self_explanation: false,
    })
}

/// Truncate a markdown body at a block boundary (paragraph / display-math /
/// code-fence) near ≈400 chars. Keeps math environments balanced so the
/// preview can be rendered by the same pipeline as the main chunk body.
fn truncate_markdown_preview(body: &str) -> String {
    const TARGET: usize = 400;
    let normalized = body.replace("\r\n", "\n");
    if normalized.len() <= TARGET {
        return normalized.trim_end().to_string();
    }
    // Split on blank-line block boundaries; accumulate blocks up to target.
    let mut out = String::new();
    let mut remaining = normalized.as_str();
    while !remaining.is_empty() {
        let block_end = remaining
            .find("\n\n")
            .map(|i| i + 2)
            .unwrap_or(remaining.len());
        let block = &remaining[..block_end];
        if !out.is_empty() && out.len() + block.len() > TARGET {
            break;
        }
        out.push_str(block);
        remaining = &remaining[block_end..];
        if out.len() >= TARGET {
            break;
        }
    }
    if out.is_empty() {
        // Single huge block — take the first line to keep it small and safe.
        out = normalized
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(TARGET)
            .collect();
    }
    out.trim_end().to_string()
}

#[tauri::command]
async fn reindex_document_references(
    source_document_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    references::reindex_document_references(pool.inner(), source_document_id).await
}

#[derive(serde::Serialize)]
struct DebugAlias {
    chunk_id: i64,
    chunk_type: String,
    title: Option<String>,
    alias: Option<String>,
    alias_kind: Option<String>,
}

#[derive(serde::Serialize)]
struct DebugReference {
    source_chunk_id: i64,
    matched_text: String,
    span_start: i64,
    span_end: i64,
    ref_kind: String,
    target_id: Option<i64>,
    target_title: Option<String>,
}

#[derive(serde::Serialize)]
struct DebugReferencesResult {
    document_id: i64,
    chunk_count: i64,
    alias_rows: Vec<DebugAlias>,
    reference_rows: Vec<DebugReference>,
}

#[tauri::command]
async fn debug_references(
    source_document_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<DebugReferencesResult, String> {
    let chunk_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE source_document_id = ?")
            .bind(source_document_id)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

    let alias_rows_raw = sqlx::query(
        "SELECT c.id AS chunk_id, c.chunk_type, c.title, a.alias, a.alias_kind \
         FROM chunks c LEFT JOIN chunk_aliases a ON a.chunk_id = c.id \
         WHERE c.source_document_id = ? \
         ORDER BY c.id, a.alias",
    )
    .bind(source_document_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let alias_rows: Vec<DebugAlias> = alias_rows_raw
        .into_iter()
        .map(|r| DebugAlias {
            chunk_id: r.get("chunk_id"),
            chunk_type: r.get("chunk_type"),
            title: r.get("title"),
            alias: r.get("alias"),
            alias_kind: r.get("alias_kind"),
        })
        .collect();

    let ref_rows_raw = sqlx::query(
        "SELECT r.source_chunk_id, r.matched_text, r.span_start, r.span_end, r.ref_kind, \
                c.id AS target_id, c.title AS target_title \
         FROM chunk_references r \
         JOIN chunks src ON src.id = r.source_chunk_id \
         LEFT JOIN chunk_aliases a ON a.alias = r.matched_text \
         LEFT JOIN chunks c ON c.id = a.chunk_id \
             AND c.source_document_id = src.source_document_id \
             AND c.id != r.source_chunk_id \
         WHERE src.source_document_id = ? \
         ORDER BY r.source_chunk_id, r.span_start",
    )
    .bind(source_document_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let reference_rows: Vec<DebugReference> = ref_rows_raw
        .into_iter()
        .map(|r| DebugReference {
            source_chunk_id: r.get("source_chunk_id"),
            matched_text: r.get("matched_text"),
            span_start: r.get("span_start"),
            span_end: r.get("span_end"),
            ref_kind: r.get("ref_kind"),
            target_id: r.get("target_id"),
            target_title: r.get("target_title"),
        })
        .collect();

    Ok(DebugReferencesResult {
        document_id: source_document_id,
        chunk_count,
        alias_rows,
        reference_rows,
    })
}

#[tauri::command]
async fn get_chunking_status(
    source_document_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<String, String> {
    let row = sqlx::query("SELECT chunking_status FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.get("chunking_status"))
}

#[tauri::command]
async fn is_chunking_page_active(
    source_document_id: i64,
    page_number: i64,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let jobs = state.chunking_jobs.lock().map_err(|e| e.to_string())?;
    Ok(jobs.contains(&(source_document_id, page_number)))
}

#[tauri::command]
async fn ensure_chunking_for_page(
    source_document_id: i64,
    page_number: i64,
    provider: String,
    model: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<bool, String> {
    if page_number < 1 {
        return Err(format!("invalid page number {}", page_number));
    }
    let provider = provider.parse::<LlmProvider>()?;
    validate_chunking_provider(provider)?;
    let model = normalize_model_override(model);

    sqlx::query("INSERT OR IGNORE INTO pages (source_document_id, page_number) VALUES (?, ?)")
        .bind(source_document_id)
        .bind(page_number)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let page_row =
        sqlx::query("SELECT id FROM pages WHERE source_document_id = ? AND page_number = ?")
            .bind(source_document_id)
            .bind(page_number)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?;
    let page_id: i64 = page_row.get("id");
    let existing_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE page_id = ?")
        .bind(page_id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    if existing_chunks > 0 {
        info!(
            target: "gloss_lib::chunking",
            "not starting chunking for doc_id={} page={} because {} chunks already exist",
            source_document_id,
            page_number,
            existing_chunks
        );
        return Ok(false);
    }

    spawn_chunking_job(
        app,
        pool.inner().clone(),
        state.inner(),
        source_document_id,
        page_number,
        provider,
        model,
    )
}

#[derive(serde::Serialize)]
struct EnsureChunkingRangeResult {
    requested_start_page: i64,
    requested_end_page: i64,
    started_pages: i64,
    skipped_pages: i64,
}

#[tauri::command]
async fn ensure_chunking_for_page_range(
    source_document_id: i64,
    start_page: i64,
    end_page: i64,
    provider: String,
    model: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<EnsureChunkingRangeResult, String> {
    if start_page < 1 {
        return Err(format!("invalid start page {}", start_page));
    }
    if end_page < start_page {
        return Err(format!(
            "invalid page range {}-{} (end before start)",
            start_page, end_page
        ));
    }

    let provider = provider.parse::<LlmProvider>()?;
    validate_chunking_provider(provider)?;
    let model = normalize_model_override(model);

    let row = sqlx::query("SELECT file_path FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let relative_path: String = row.get("file_path");
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_path = data_dir.join(relative_path);
    let page_count = state
        .pdfium
        .run(move |pdfium| {
            let doc = pdfium
                .load_pdf_from_file(&abs_path, None)
                .map_err(|e| e.to_string())?;
            Ok(doc.pages().len() as i64)
        })
        .await?;

    if end_page > page_count {
        return Err(format!(
            "invalid page range {}-{}; document has {} pages",
            start_page, end_page, page_count
        ));
    }

    let existing_chunk_pages: HashSet<i64> = sqlx::query_scalar(
        "SELECT DISTINCT p.page_number \
         FROM chunks c \
         JOIN pages p ON p.id = c.page_id \
         WHERE p.source_document_id = ? \
           AND p.page_number BETWEEN ? AND ?",
    )
    .bind(source_document_id)
    .bind(start_page)
    .bind(end_page)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .collect();

    let mut pages_to_start: Vec<i64> = Vec::new();
    let mut skipped_pages = 0_i64;

    for page in start_page..=end_page {
        if existing_chunk_pages.contains(&page) {
            skipped_pages += 1;
            continue;
        }
        if !begin_chunking_job(state.inner(), source_document_id, page)? {
            skipped_pages += 1;
            continue;
        }
        pages_to_start.push(page);
    }

    let started_pages = pages_to_start.len() as i64;
    if started_pages > 0 {
        let chunking_jobs = Arc::clone(&state.chunking_jobs);
        let pdfium = state.pdfium.clone();
        let pool = pool.inner().clone();
        let app_for_tasks = app.clone();
        tokio::spawn(async move {
            for page_number in pages_to_start {
                chunking::run_for_page(
                    pool.clone(),
                    pdfium.clone(),
                    app_for_tasks.clone(),
                    source_document_id,
                    page_number,
                    provider,
                    model.clone(),
                )
                .await;
                finish_chunking_job(&chunking_jobs, source_document_id, page_number);
            }
        });
    }

    Ok(EnsureChunkingRangeResult {
        requested_start_page: start_page,
        requested_end_page: end_page,
        started_pages,
        skipped_pages,
    })
}

#[tauri::command]
async fn rechunk_page(
    source_document_id: i64,
    page_number: i64,
    provider: String,
    model: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    if page_number < 1 {
        return Err(format!("invalid page number {}", page_number));
    }
    let provider = provider.parse::<LlmProvider>()?;
    validate_chunking_provider(provider)?;
    let model = normalize_model_override(model);
    if !begin_chunking_job(state.inner(), source_document_id, page_number)? {
        return Err("chunking already active for this page".into());
    }

    let result = chunking::rechunk_page(
        pool.inner(),
        &app,
        source_document_id,
        page_number,
        provider,
        model,
    )
    .await;
    finish_chunking_job(&state.chunking_jobs, source_document_id, page_number);
    result
}

#[tauri::command]
async fn import_pdf(
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<SourceDocument, String> {
    // Open file picker — returns FilePath::Path on desktop, FilePath::Url on Android
    let picked = app
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .blocking_pick_file();

    let file_path = match picked {
        Some(p) => p,
        None => return Err("cancelled".into()),
    };

    // Extract filename. On Android the content URI's last path segment is percent-encoded
    // and may contain a sub-path like "primary:Download/foo.pdf" — take just the tail.
    let file_name: String = match &file_path {
        FilePath::Path(p) => p
            .file_name()
            .ok_or("invalid file name")?
            .to_string_lossy()
            .to_string(),
        FilePath::Url(u) => {
            let raw = u
                .path_segments()
                .and_then(|mut s| s.next_back().map(str::to_owned))
                .unwrap_or_else(|| "import.pdf".into());
            // Percent-decode and take the tail after any "/" within the segment
            // e.g. "primary%3ADownload%2Ffoo.pdf" -> "foo.pdf"
            let decoded = percent_decode_str(&raw).decode_utf8_lossy().to_string();
            decoded
                .rsplit('/')
                .next()
                .unwrap_or("import.pdf")
                .to_string()
        }
    };

    let file_name = if file_name.to_lowercase().ends_with(".pdf") {
        file_name
    } else {
        format!("{}.pdf", file_name)
    };

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let pdfs_dir = data_dir.join("pdfs");
    std::fs::create_dir_all(&pdfs_dir).map_err(|e| e.to_string())?;

    // Avoid overwriting an existing file by appending a counter if needed
    let mut dest_path = pdfs_dir.join(&file_name);
    if dest_path.exists() {
        let stem = std::path::Path::new(&file_name)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut counter: u32 = 1;
        loop {
            dest_path = pdfs_dir.join(format!("{}_{}.pdf", stem, counter));
            if !dest_path.exists() {
                break;
            }
            counter += 1;
        }
    }

    // Read via tauri-plugin-fs — handles content:// URIs on Android via JNI,
    // falls back to plain std::fs on desktop.
    let bytes = app.fs().read(file_path).map_err(|e| e.to_string())?;
    std::fs::write(&dest_path, &bytes).map_err(|e| e.to_string())?;

    // Relative path stored in DB (relative to app data dir)
    let relative_path = format!("pdfs/{}", dest_path.file_name().unwrap().to_string_lossy());

    let title = std::path::Path::new(&file_name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let row =
        sqlx::query("INSERT INTO source_documents (title, file_path) VALUES (?, ?) RETURNING id")
            .bind(&title)
            .bind(&relative_path)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

    let id: i64 = row.get("id");
    info!(
        target: "gloss_lib::import",
        "imported pdf doc_id={} title=\"{}\" relative_path=\"{}\"",
        id,
        title,
        relative_path
    );

    Ok(SourceDocument {
        id,
        title,
        file_path: relative_path,
    })
}

#[tauri::command]
async fn get_pdf_path(app: tauri::AppHandle, relative_path: String) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs = data_dir.join(&relative_path);
    Ok(abs.to_string_lossy().to_string())
}

#[tauri::command]
async fn list_textbooks(pool: tauri::State<'_, SqlitePool>) -> Result<Vec<SourceDocument>, String> {
    let rows = sqlx::query("SELECT id, title, file_path FROM source_documents ORDER BY id DESC")
        .fetch_all(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SourceDocument {
            id: r.get("id"),
            title: r.get("title"),
            file_path: r.get("file_path"),
        })
        .collect())
}

#[tauri::command]
async fn get_or_create_page(
    source_document_id: i64,
    page_number: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    sqlx::query("INSERT OR IGNORE INTO pages (source_document_id, page_number) VALUES (?, ?)")
        .bind(source_document_id)
        .bind(page_number)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    let row = sqlx::query("SELECT id FROM pages WHERE source_document_id = ? AND page_number = ?")
        .bind(source_document_id)
        .bind(page_number)
        .fetch_one(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn save_stroke(
    page_id: i64,
    stroke: StrokeInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    let bounds = stroke_bounds(&stroke.points)?;
    let data = encode_stroke_points(&stroke.points)?;

    let row = sqlx::query(
        "INSERT INTO strokes (page_id, data, colour, thickness, min_x, min_y, max_x, max_y, chunk_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(page_id)
    .bind(&data)
    .bind(&stroke.colour)
    .bind(stroke.thickness)
    .bind(bounds.min_x)
    .bind(bounds.min_y)
    .bind(bounds.max_x)
    .bind(bounds.max_y)
    .bind(stroke.chunk_id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn delete_stroke(stroke_id: i64, pool: tauri::State<'_, SqlitePool>) -> Result<(), String> {
    sqlx::query("DELETE FROM strokes WHERE id = ?")
        .bind(stroke_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn load_strokes(
    page_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<StrokeOutput>, String> {
    let rows = sqlx::query("SELECT id, colour, COALESCE(thickness, 1.0) AS thickness, data, min_x, min_y, max_x, max_y, chunk_id FROM strokes WHERE page_id = ? ORDER BY id")
        .bind(page_id)
        .fetch_all(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    rows.into_iter()
        .map(|r| {
            let data: Vec<u8> = r.get("data");
            let points = decode_stroke_points(data)?;
            Ok(StrokeOutput {
                id: r.get("id"),
                colour: r.get("colour"),
                thickness: r.get("thickness"),
                points,
                min_x: r.get("min_x"),
                min_y: r.get("min_y"),
                max_x: r.get("max_x"),
                max_y: r.get("max_y"),
                chunk_id: r.get("chunk_id"),
            })
        })
        .collect()
}

#[tauri::command]
async fn get_or_create_page_surface(
    page_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    get_or_create_note_surface_id(
        NoteSurfaceOwner::Page(page_id),
        NoteSurfaceKind::PageNotes,
        pool.inner(),
    )
    .await
}

#[tauri::command]
async fn get_or_create_chunk_surface(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    get_or_create_note_surface_id(
        NoteSurfaceOwner::Chunk(chunk_id),
        NoteSurfaceKind::ChunkNotes,
        pool.inner(),
    )
    .await
}

#[tauri::command]
async fn save_surface_stroke(
    surface_id: i64,
    stroke: SurfaceStrokeInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    let bounds = stroke_bounds(&stroke.points)?;
    let data = encode_stroke_points(&stroke.points)?;

    let row = sqlx::query(
        "INSERT INTO surface_strokes (surface_id, data, colour, thickness, min_x, min_y, max_x, max_y) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(surface_id)
    .bind(&data)
    .bind(&stroke.colour)
    .bind(stroke.thickness)
    .bind(bounds.min_x)
    .bind(bounds.min_y)
    .bind(bounds.max_x)
    .bind(bounds.max_y)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn load_surface_strokes(
    surface_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<SurfaceStrokeOutput>, String> {
    let rows = sqlx::query(
        "SELECT id, colour, COALESCE(thickness, 1.0) AS thickness, data, min_x, min_y, max_x, max_y \
         FROM surface_strokes WHERE surface_id = ? ORDER BY id",
    )
    .bind(surface_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    rows.into_iter()
        .map(|r| {
            let data: Vec<u8> = r.get("data");
            let points = decode_stroke_points(data)?;
            Ok(SurfaceStrokeOutput {
                id: r.get("id"),
                colour: r.get("colour"),
                thickness: r.get("thickness"),
                points,
                min_x: r.get("min_x"),
                min_y: r.get("min_y"),
                max_x: r.get("max_x"),
                max_y: r.get("max_y"),
            })
        })
        .collect()
}

#[tauri::command]
async fn delete_surface_stroke(
    stroke_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    sqlx::query("DELETE FROM surface_strokes WHERE id = ?")
        .bind(stroke_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Return the number of pages in the PDF at the given relative path.
#[tauri::command]
async fn get_page_count(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    relative_path: String,
) -> Result<usize, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_path = data_dir.join(&relative_path);

    state
        .pdfium
        .run(move |pdfium| {
            let doc = pdfium
                .load_pdf_from_file(&abs_path, None)
                .map_err(|e| e.to_string())?;
            Ok(doc.pages().len() as usize)
        })
        .await
}

/// Render a single PDF page to a target width in device pixels.
/// Results are cached by (path, page, target_width_px) so subsequent calls are O(1).
///
/// Returns a binary response:
/// [page_width_points: f32 LE][page_height_points: f32 LE][width: u32 LE][height: u32 LE][RGBA data...].
/// Using `tauri::ipc::Response` sends raw bytes over IPC instead of JSON-encoding
/// millions of pixel values, which would otherwise take seconds.
#[tauri::command]
async fn render_pdf_page(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    relative_path: String,
    page_number: usize,
    target_width: u32,
) -> Result<tauri::ipc::Response, String> {
    let cache_key = (relative_path.clone(), page_number, target_width);

    // Check cache first (lock briefly, then release)
    {
        let mut cache = state.pdf_cache.lock().map_err(|e| e.to_string())?;
        if let Some(page) = cache.get(&cache_key) {
            return Ok(rendered_page_to_response(&page));
        }
    }

    // Resolve absolute path
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_path = data_dir.join(&relative_path);

    // Render on the dedicated pdfium worker thread
    let rendered = state
        .pdfium
        .run(move |pdfium| {
            let doc = pdfium
                .load_pdf_from_file(&abs_path, None)
                .map_err(|e| e.to_string())?;

            let page = doc
                .pages()
                .get(page_number as i32)
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

            // Return raw RGBA bytes — no PNG encode/decode round-trip needed.
            Ok(RenderedPage {
                width: bitmap.width() as u32,
                height: bitmap.height() as u32,
                page_width_points: page.width().value,
                page_height_points: page.height().value,
                data: bitmap.as_rgba_bytes(),
            })
        })
        .await?;

    // Store in cache
    let cached = {
        let mut cache = state.pdf_cache.lock().map_err(|e| e.to_string())?;
        cache.put(cache_key, rendered)
    };
    Ok(rendered_page_to_response(&cached))
}

/// Pack a RenderedPage into a binary IPC response with PDF point dimensions,
/// bitmap dimensions, and raw RGBA pixel bytes.
fn rendered_page_to_response(page: &RenderedPage) -> tauri::ipc::Response {
    let mut buf = Vec::with_capacity(16 + page.data.len());
    buf.extend_from_slice(&page.page_width_points.to_le_bytes());
    buf.extend_from_slice(&page.page_height_points.to_le_bytes());
    buf.extend_from_slice(&page.width.to_le_bytes());
    buf.extend_from_slice(&page.height.to_le_bytes());
    buf.extend_from_slice(&page.data);
    tauri::ipc::Response::new(buf)
}

async fn init_db(app: &tauri::App) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("gloss.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .target(Target::new(TargetKind::Stdout))
                .target(Target::new(TargetKind::LogDir {
                    file_name: Some("gloss".to_string()),
                }))
                .target(Target::new(TargetKind::Webview))
                .level(log::LevelFilter::Info)
                .level_for("gloss_lib::chunking", log::LevelFilter::Debug)
                .level_for("gloss_lib::llm", log::LevelFilter::Debug)
                .level_for("gloss_lib::gemini", log::LevelFilter::Debug)
                .level_for("gloss_lib::openai", log::LevelFilter::Debug)
                .level_for("gloss_lib::ollama", log::LevelFilter::Debug)
                .level_for("gloss_lib::deepseek", log::LevelFilter::Debug)
                .level_for("gloss_lib::zai", log::LevelFilter::Debug)
                .timezone_strategy(TimezoneStrategy::UseLocal)
                .build(),
        )
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let pool = tauri::async_runtime::block_on(init_db(app))
                .expect("failed to initialise database");
            info!(target: "gloss_lib::startup", "database initialised");
            app.manage(pool);

            let pdfium = if cfg!(target_os = "android") {
                PdfiumWorker::spawn_system().expect("failed to start pdfium worker")
            } else {
                let exe_dir = std::env::current_exe()
                    .expect("can't find exe")
                    .parent()
                    .expect("exe has no parent dir")
                    .to_path_buf();
                let lib_name = if cfg!(target_os = "windows") {
                    "pdfium.dll"
                } else if cfg!(target_os = "macos") {
                    "libpdfium.dylib"
                } else {
                    "libpdfium.so"
                };
                PdfiumWorker::spawn(exe_dir.join(lib_name)).expect("failed to start pdfium worker")
            };
            info!(target: "gloss_lib::startup", "pdfium worker initialised");

            app.manage(AppState {
                pdf_cache: Mutex::new(PdfCache::new()),
                pdfium,
                chunking_jobs: Arc::new(Mutex::new(HashSet::new())),
                chat_streams: Arc::new(Mutex::new(HashMap::new())),
            });
            info!(target: "gloss_lib::startup", "gloss startup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            import_pdf,
            list_textbooks,
            get_pdf_path,
            get_or_create_page,
            get_or_create_page_surface,
            get_or_create_chunk_surface,
            save_stroke,
            load_strokes,
            delete_stroke,
            save_surface_stroke,
            load_surface_strokes,
            delete_surface_stroke,
            render_pdf_page,
            get_page_count,
            get_chunks_for_page,
            get_chunk_for_transcription,
            save_chunk_glossary,
            get_ai_settings_state,
            save_ai_api_keys,
            generate_chunk_formatted_body,
            start_chunk_ai_stream,
            cancel_chunk_ai_stream,
            get_chunk_references,
            get_chunk_preview,
            reindex_document_references,
            debug_references,
            get_chunking_status,
            is_chunking_page_active,
            ensure_chunking_for_page,
            ensure_chunking_for_page_range,
            rechunk_page,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

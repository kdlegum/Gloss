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
    sanitize_chunk_body_markdown, ChunkBodyPrompt, ChunkBodyResult, ChunkChatMessage,
    ChunkChatPrompt, ChunkRewritePrompt, ChunkRewriteResult, LlmError, LlmProvider,
};
use crate::ollama::OllamaClient;
use crate::openai::OpenAiClient;
use crate::zai::ZaiClient;
use log::info;
use pdfium_render::prelude::*;
use percent_encoding::percent_decode_str;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};
use tauri_plugin_log::{Target, TargetKind, TimezoneStrategy};
use tokio::sync::Semaphore;

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

    fn remove_document(&mut self, relative_path: &str) {
        self.order.retain(|(path, _, _)| path != relative_path);
        self.data
            .retain(|(path, _, _), _| path.as_str() != relative_path);
    }
}

struct AppState {
    pdf_cache: Mutex<PdfCache>,
    pdfium: PdfiumWorker,
    chunking_jobs: Arc<Mutex<HashSet<(i64, i64)>>>,
    chat_streams: Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
    zai_transcription_semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
struct ChatStreamHandle {
    cancelled: Arc<AtomicBool>,
    context: ChatStreamContext,
}

#[derive(Clone)]
enum ChatStreamContext {
    Chunk(i64),
    Page(i64),
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
    let zai_transcription_semaphore = Arc::clone(&state.zai_transcription_semaphore);
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
            zai_transcription_semaphore,
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
    context: ChatStreamContext,
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
            context,
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
    context_kind: &'static str,
    chunk_id: Option<i64>,
    page_id: Option<i64>,
    phase: &'static str,
    delta: Option<String>,
    error: Option<String>,
}

fn emit_chunk_ai_stream(
    app: &tauri::AppHandle,
    request_id: &str,
    context: &ChatStreamContext,
    phase: &'static str,
    delta: Option<String>,
    error: Option<String>,
) {
    let (context_kind, chunk_id, page_id) = match context {
        ChatStreamContext::Chunk(chunk_id) => ("chunk", Some(*chunk_id), None),
        ChatStreamContext::Page(page_id) => ("page", None, Some(*page_id)),
    };
    let _ = app.emit(
        "chunk_ai_stream",
        ChunkAiStreamEvent {
            request_id: request_id.to_string(),
            context_kind,
            chunk_id,
            page_id,
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

async fn run_vision_body_transcription(
    pool: &SqlitePool,
    provider: LlmProvider,
    model: Option<String>,
    image_base64: &str,
    chunk_type: &str,
    title: Option<&str>,
    subject: Option<&str>,
) -> Result<String, String> {
    if image_base64.trim().is_empty() {
        return Err("image payload was empty".into());
    }

    let prompt = ChunkBodyPrompt {
        chunk_type,
        title,
        subject,
    };
    let configured_api_key = settings::api_key_for_provider(pool, provider).await?;
    let result = match provider {
        LlmProvider::Ollama => OllamaClient::with_vision_model(model.clone())
            .transcribe_chunk_body(&prompt, image_base64)
            .await
            .map_err(|e| e.to_string())?,
        LlmProvider::OpenAI => {
            OpenAiClient::with_api_key_and_model(configured_api_key, model.clone())
                .transcribe_chunk_body(&prompt, image_base64)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::Gemini => {
            GeminiClient::with_api_key_and_model(configured_api_key, model.clone())
                .transcribe_chunk_body(&prompt, image_base64)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::DeepSeek => {
            return Err("provider deepseek does not support vision transcription".to_string())
        }
        LlmProvider::Zai => ZaiClient::with_api_key_and_model(configured_api_key, model)
            .transcribe_chunk_body(&prompt, image_base64)
            .await
            .map_err(|e| e.to_string())?,
    };

    let body_markdown = sanitize_chunk_body_markdown(&result.body_markdown);
    if body_markdown.is_empty() {
        return Err("vision transcription returned empty chunk body".to_string());
    }

    Ok(body_markdown)
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

#[derive(serde::Deserialize, serde::Serialize)]
struct GraphPointInput {
    x: f64,
    y: f64,
}

#[derive(serde::Deserialize)]
struct SurfaceGraphObjectInput {
    mode: String,
    equation: Option<String>,
    #[serde(alias = "pointsJson")]
    points_json: Option<String>,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    bbox_x: f64,
    bbox_y: f64,
    bbox_w: f64,
    bbox_h: f64,
    #[serde(alias = "lineColour")]
    line_colour: String,
    #[serde(alias = "lineWidth")]
    line_width: f64,
}

#[derive(serde::Serialize)]
struct SurfaceGraphObjectOutput {
    id: i64,
    surface_id: i64,
    mode: String,
    equation: Option<String>,
    points_json: Option<String>,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    bbox_x: f64,
    bbox_y: f64,
    bbox_w: f64,
    bbox_h: f64,
    line_colour: String,
    line_width: f64,
}

struct NormalizedSurfaceGraphObject {
    mode: String,
    equation: Option<String>,
    points_json: Option<String>,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    bbox_x: f64,
    bbox_y: f64,
    bbox_w: f64,
    bbox_h: f64,
    line_colour: String,
    line_width: f64,
}

#[derive(serde::Serialize)]
struct SourceDocument {
    id: i64,
    title: String,
    file_path: String,
    document_mode: String,
    instruction_page_start: Option<i64>,
    instruction_page_end: Option<i64>,
}

#[derive(serde::Serialize)]
struct PastPaperInstructionContextDebug {
    source_document_id: i64,
    chunking_status: String,
    instruction_page_start: Option<i64>,
    instruction_page_end: Option<i64>,
    expected_instruction_pages: Vec<i64>,
    extracted_instruction_pages: Vec<i64>,
    missing_instruction_pages: Vec<i64>,
    instruction_block_count: i64,
    instruction_transcribed_block_count: i64,
    preview_text: String,
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

fn require_finite(value: f64, name: &str) -> Result<f64, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{} must be finite", name))
    }
}

fn normalize_surface_graph_input(
    graph: SurfaceGraphObjectInput,
) -> Result<NormalizedSurfaceGraphObject, String> {
    let mode = graph.mode.trim().to_ascii_lowercase();
    if mode != "equation" && mode != "points" {
        return Err("graph mode must be 'equation' or 'points'".into());
    }

    let x_min = require_finite(graph.x_min, "x_min")?;
    let x_max = require_finite(graph.x_max, "x_max")?;
    let y_min = require_finite(graph.y_min, "y_min")?;
    let y_max = require_finite(graph.y_max, "y_max")?;
    let bbox_x = require_finite(graph.bbox_x, "bbox_x")?;
    let bbox_y = require_finite(graph.bbox_y, "bbox_y")?;
    let bbox_w = require_finite(graph.bbox_w, "bbox_w")?;
    let bbox_h = require_finite(graph.bbox_h, "bbox_h")?;
    let line_width = require_finite(graph.line_width, "line_width")?;

    if x_max <= x_min {
        return Err("x_max must be greater than x_min".into());
    }
    if y_max <= y_min {
        return Err("y_max must be greater than y_min".into());
    }
    if bbox_w <= 0.0 || bbox_h <= 0.0 {
        return Err("graph bbox width and height must be positive".into());
    }
    if line_width <= 0.0 {
        return Err("graph line_width must be positive".into());
    }

    let line_colour = graph.line_colour.trim().to_string();
    if line_colour.is_empty() {
        return Err("graph line_colour cannot be empty".into());
    }

    let equation = graph
        .equation
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let points = graph
        .points_json
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let (equation, points_json) = if mode == "equation" {
        let equation = equation.ok_or_else(|| "equation mode requires an equation".to_string())?;
        (Some(equation), None)
    } else {
        let points = points.ok_or_else(|| "points mode requires points_json".to_string())?;
        let parsed: Vec<GraphPointInput> = serde_json::from_str(points)
            .map_err(|e| format!("points_json must be valid JSON: {}", e))?;
        if parsed.is_empty() {
            return Err("points_json must include at least one point".into());
        }
        for (index, point) in parsed.iter().enumerate() {
            if !point.x.is_finite() || !point.y.is_finite() {
                return Err(format!(
                    "points_json contains a non-finite point at index {}",
                    index
                ));
            }
        }
        let canonical = serde_json::to_string(&parsed).map_err(|e| e.to_string())?;
        (None, Some(canonical))
    };

    Ok(NormalizedSurfaceGraphObject {
        mode,
        equation,
        points_json,
        x_min,
        x_max,
        y_min,
        y_max,
        bbox_x,
        bbox_y,
        bbox_w,
        bbox_h,
        line_colour,
        line_width,
    })
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
    question_label: Option<String>,
    available_marks: Option<i64>,
    achieved_marks: Option<f64>,
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
    question_label: Option<String>,
    available_marks: Option<i64>,
    achieved_marks: Option<f64>,
}

#[tauri::command]
async fn get_chunks_for_page(
    page_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<ChunkInfo>, String> {
    let rows = sqlx::query(
        "SELECT c.id, c.chunk_type, c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.status, \
                c.title, c.subject, c.proves_chunk_id, c.ocr_text, c.formatted_body_md, c.glossary_md, \
                c.question_label, c.available_marks, c.achieved_marks \
         FROM chunks c \
         WHERE c.page_id = ? \
           AND (c.chunk_type != 'question' OR NOT EXISTS (SELECT 1 FROM question_page_slices qps WHERE qps.chunk_id = c.id)) \
         UNION ALL \
         SELECT c.id, c.chunk_type, qps.bbox_x, qps.bbox_y, qps.bbox_w, qps.bbox_h, c.status, \
                c.title, c.subject, c.proves_chunk_id, c.ocr_text, c.formatted_body_md, c.glossary_md, \
                c.question_label, c.available_marks, c.achieved_marks \
         FROM question_page_slices qps \
         JOIN chunks c ON c.id = qps.chunk_id \
         WHERE qps.page_id = ? \
         ORDER BY bbox_y, bbox_x",
    )
    .bind(page_id)
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
            question_label: r.get("question_label"),
            available_marks: r.get("available_marks"),
            achieved_marks: r.get("achieved_marks"),
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
async fn save_chunk_title(
    chunk_id: i64,
    title: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let trimmed = title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    sqlx::query("UPDATE chunks SET title = ? WHERE id = ?")
        .bind(trimmed.as_deref())
        .bind(chunk_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn save_chunk_body_markdown(
    chunk_id: i64,
    body_markdown: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let trimmed = body_markdown
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_chunk_body_markdown)
        .filter(|value| !value.trim().is_empty());
    sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
        .bind(trimmed.as_deref())
        .bind(chunk_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = references::reindex_chunk_references(pool.inner(), chunk_id).await {
        log::warn!(
            target: "gloss_lib::references",
            "failed to reindex references after manual body save chunk_id={}: {}",
            chunk_id,
            e
        );
    }
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
                c.glossary_md, c.question_label, c.available_marks, c.achieved_marks \
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
        question_label: r.get("question_label"),
        available_marks: r.get("available_marks"),
        achieved_marks: r.get("achieved_marks"),
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
            let cleaned = sanitize_chunk_body_markdown(&reused);
            if cleaned.is_empty() {
                return Err("reused chunk body was empty after sanitization".into());
            }
            info!(
                target: "gloss_lib::llm",
                "generate_chunk_formatted_body chunk_id={} reused block transcription",
                chunk_id
            );
            sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
                .bind(&cleaned)
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
                body_markdown: cleaned,
            });
        }
    }

    info!(
        target: "gloss_lib::llm",
        "generate_chunk_formatted_body chunk_id={} provider={} force={} chunk_type={}",
        chunk_id,
        provider,
        force,
        chunk_type
    );

    let body_markdown = run_vision_body_transcription(
        pool.inner(),
        provider,
        model,
        &image_base64,
        &chunk_type,
        title.as_deref(),
        subject.as_deref(),
    )
    .await?;

    sqlx::query("UPDATE chunks SET formatted_body_md = ? WHERE id = ?")
        .bind(&body_markdown)
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

    Ok(ChunkBodyResult { body_markdown })
}

#[tauri::command]
async fn transcribe_ai_chat_image(
    provider: String,
    model: Option<String>,
    image_base64: String,
    chunk_type: Option<String>,
    title: Option<String>,
    subject: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkBodyResult, String> {
    let provider = provider.parse::<LlmProvider>()?;
    validate_vision_provider(provider)?;
    let model = normalize_model_override(model);
    let chunk_type = chunk_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("explanation");
    let title = title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let subject = subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let body_markdown = run_vision_body_transcription(
        pool.inner(),
        provider,
        model,
        &image_base64,
        chunk_type,
        title,
        subject,
    )
    .await?;

    Ok(ChunkBodyResult { body_markdown })
}

#[derive(Clone)]
struct ChunkAliasContext {
    alias: String,
    alias_kind: String,
}

#[derive(Clone)]
struct RelatedChunkContext {
    relation: String,
    chunk_id: i64,
    chunk_type: String,
    title: Option<String>,
    subject: Option<String>,
    body_preview: Option<String>,
    glossary_preview: Option<String>,
    aliases: Vec<String>,
}

struct ChunkChatContext {
    book_title: String,
    chunk_type: String,
    title: Option<String>,
    subject: Option<String>,
    question_label: Option<String>,
    available_marks: Option<i64>,
    achieved_marks: Option<f64>,
    body_markdown: String,
    glossary_markdown: Option<String>,
    aliases: Vec<ChunkAliasContext>,
    related_chunks: Vec<RelatedChunkContext>,
}

fn compose_chunk_chat_body(context: &ChunkChatContext) -> String {
    let mut body = context.body_markdown.trim().to_string();
    let mut extras = String::new();

    if context.chunk_type == "question" {
        extras.push_str("Question metadata:\n");
        extras.push_str(&format!(
            "- label: {}\n",
            context.question_label.as_deref().unwrap_or("unknown")
        ));
        extras.push_str(&format!(
            "- available_marks: {}\n",
            context
                .available_marks
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ));
        extras.push_str(&format!(
            "- achieved_marks: {}\n",
            context
                .achieved_marks
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "unknown".to_string())
        ));
    }

    if let Some(glossary) = context
        .glossary_markdown
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        extras.push_str("Chunk glossary entry:\n");
        extras.push_str("---\n");
        extras.push_str(glossary);
        extras.push_str("\n---\n");
    }

    if !context.aliases.is_empty() {
        if !extras.is_empty() {
            extras.push('\n');
        }
        extras.push_str("Chunk aliases:\n");
        for alias in &context.aliases {
            extras.push_str(&format!("- {} ({})\n", alias.alias, alias.alias_kind));
        }
    }

    if !context.related_chunks.is_empty() {
        if !extras.is_empty() {
            extras.push('\n');
        }
        extras.push_str("Related chunk context:\n");
        for related in &context.related_chunks {
            extras.push_str(&format!(
                "- [{}] chunk {} ({})\n",
                related.relation, related.chunk_id, related.chunk_type
            ));
            if let Some(title) = related
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                extras.push_str(&format!("  title: {}\n", title));
            }
            if let Some(subject) = related
                .subject
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                extras.push_str(&format!("  subject: {}\n", subject));
            }
            if !related.aliases.is_empty() {
                extras.push_str(&format!("  aliases: {}\n", related.aliases.join(", ")));
            }
            if let Some(preview) = related
                .body_preview
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                extras.push_str("  body preview:\n");
                for line in preview.lines() {
                    extras.push_str("    ");
                    extras.push_str(line);
                    extras.push('\n');
                }
            }
            if let Some(glossary) = related
                .glossary_preview
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                extras.push_str("  glossary preview:\n");
                for line in glossary.lines() {
                    extras.push_str("    ");
                    extras.push_str(line);
                    extras.push('\n');
                }
            }
        }
    }

    if !extras.trim().is_empty() {
        body.push_str(
            "\n\nSupplemental context (glossary, aliases, linked chunks, and references):\n---\n",
        );
        body.push_str(extras.trim_end());
        body.push_str("\n---\n");
    }

    body
}

async fn load_chunk_alias_context(
    pool: &SqlitePool,
    chunk_id: i64,
) -> Result<Vec<ChunkAliasContext>, String> {
    let rows = sqlx::query(
        "SELECT alias, alias_kind FROM chunk_aliases \
         WHERE chunk_id = ? ORDER BY alias_kind, alias",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|row| ChunkAliasContext {
            alias: row.get("alias"),
            alias_kind: row.get("alias_kind"),
        })
        .collect())
}

async fn load_related_chunk_aliases(
    pool: &SqlitePool,
    chunk_id: i64,
) -> Result<Vec<String>, String> {
    let rows = sqlx::query(
        "SELECT alias FROM chunk_aliases WHERE chunk_id = ? ORDER BY alias_kind, alias LIMIT 6",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|row| row.get("alias")).collect())
}

async fn append_related_chunk(
    pool: &SqlitePool,
    related_chunks: &mut Vec<RelatedChunkContext>,
    seen_ids: &mut HashSet<i64>,
    relation: &str,
    chunk_id: i64,
    chunk_type: String,
    title: Option<String>,
    subject: Option<String>,
    body_markdown: Option<String>,
    glossary_markdown: Option<String>,
    max_related: usize,
) -> Result<(), String> {
    if related_chunks.len() >= max_related {
        return Ok(());
    }
    if !seen_ids.insert(chunk_id) {
        return Ok(());
    }

    let aliases = load_related_chunk_aliases(pool, chunk_id).await?;
    let body_preview = body_markdown
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(truncate_markdown_preview);
    let glossary_preview = glossary_markdown
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(truncate_markdown_preview);
    related_chunks.push(RelatedChunkContext {
        relation: relation.to_string(),
        chunk_id,
        chunk_type,
        title,
        subject,
        body_preview,
        glossary_preview,
        aliases,
    });
    Ok(())
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
        let image_base64_list: Vec<String> = message
            .image_items()
            .into_iter()
            .map(str::to_string)
            .collect();
        if role != "user" && !image_base64_list.is_empty() {
            return Err("image attachments are only allowed on user messages".into());
        }
        if content.is_empty() && image_base64_list.is_empty() {
            continue;
        }
        cleaned.push(ChunkChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            image_base64: None,
            image_base64_list,
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
        "SELECT sd.title AS book_title, c.source_document_id, c.chunk_type, c.title, c.subject, c.proves_chunk_id, \
                c.question_label, c.available_marks, c.achieved_marks, \
                COALESCE(NULLIF(TRIM(c.formatted_body_md), ''), NULLIF(TRIM(c.ocr_text), '')) AS body_markdown, \
                NULLIF(TRIM(c.glossary_md), '') AS glossary_markdown \
         FROM chunks c \
         JOIN source_documents sd ON sd.id = c.source_document_id \
         WHERE c.id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let body_markdown = row
        .get::<Option<String>, _>("body_markdown")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let glossary_markdown = row
        .get::<Option<String>, _>("glossary_markdown")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let body_markdown = body_markdown
        .or_else(|| {
            glossary_markdown
                .as_ref()
                .map(|_| "No chunk body text is available for this chunk.".to_string())
        })
        .ok_or_else(|| "No chunk text is available for AI chat yet.".to_string())?;

    let source_document_id: i64 = row.get("source_document_id");
    let chunk_type: String = row.get("chunk_type");
    let proves_chunk_id: Option<i64> = row.get("proves_chunk_id");
    let aliases = load_chunk_alias_context(pool, chunk_id).await?;

    const MAX_RELATED: usize = 6;
    let mut related_chunks: Vec<RelatedChunkContext> = Vec::new();
    let mut seen_ids: HashSet<i64> = HashSet::new();

    if chunk_type == "proof" {
        if let Some(target_chunk_id) = proves_chunk_id {
            if let Some(linked) = sqlx::query(
                "SELECT id, chunk_type, title, subject, \
                        COALESCE(NULLIF(TRIM(formatted_body_md), ''), NULLIF(TRIM(ocr_text), '')) AS body_markdown, \
                        NULLIF(TRIM(glossary_md), '') AS glossary_markdown \
                 FROM chunks \
                 WHERE id = ? AND source_document_id = ?",
            )
            .bind(target_chunk_id)
            .bind(source_document_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?
            {
                append_related_chunk(
                    pool,
                    &mut related_chunks,
                    &mut seen_ids,
                    "proof_target",
                    linked.get("id"),
                    linked.get("chunk_type"),
                    linked.get("title"),
                    linked.get("subject"),
                    linked.get("body_markdown"),
                    linked.get("glossary_markdown"),
                    MAX_RELATED,
                )
                .await?;
            }
        }
    } else if let Some(linked_proof) = sqlx::query(
        "SELECT id, chunk_type, title, subject, \
                COALESCE(NULLIF(TRIM(formatted_body_md), ''), NULLIF(TRIM(ocr_text), '')) AS body_markdown, \
                NULLIF(TRIM(glossary_md), '') AS glossary_markdown \
         FROM chunks \
         WHERE source_document_id = ? AND chunk_type = 'proof' AND proves_chunk_id = ? \
         ORDER BY id LIMIT 1",
    )
    .bind(source_document_id)
    .bind(chunk_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    {
        append_related_chunk(
            pool,
            &mut related_chunks,
            &mut seen_ids,
            "linked_proof",
            linked_proof.get("id"),
            linked_proof.get("chunk_type"),
            linked_proof.get("title"),
            linked_proof.get("subject"),
            linked_proof.get("body_markdown"),
            linked_proof.get("glossary_markdown"),
            MAX_RELATED,
        )
        .await?;
    }

    let reference_rows = sqlx::query(
        "SELECT c.id, c.chunk_type, c.title, c.subject, \
                COALESCE(NULLIF(TRIM(c.formatted_body_md), ''), NULLIF(TRIM(c.ocr_text), '')) AS body_markdown, \
                NULLIF(TRIM(c.glossary_md), '') AS glossary_markdown \
         FROM chunk_references r \
         JOIN chunks src ON src.id = r.source_chunk_id \
         JOIN chunk_aliases a ON a.alias = r.matched_text \
         JOIN chunks c ON c.id = a.chunk_id \
             AND c.source_document_id = src.source_document_id \
             AND c.id != src.id \
         WHERE r.source_chunk_id = ? \
         ORDER BY r.span_start, c.id",
    )
    .bind(chunk_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    for row in reference_rows {
        append_related_chunk(
            pool,
            &mut related_chunks,
            &mut seen_ids,
            "body_reference",
            row.get("id"),
            row.get("chunk_type"),
            row.get("title"),
            row.get("subject"),
            row.get("body_markdown"),
            row.get("glossary_markdown"),
            MAX_RELATED,
        )
        .await?;
        if related_chunks.len() >= MAX_RELATED {
            break;
        }
    }

    Ok(ChunkChatContext {
        book_title: row.get("book_title"),
        chunk_type,
        title: row.get("title"),
        subject: row.get("subject"),
        question_label: row.get("question_label"),
        available_marks: row.get("available_marks"),
        achieved_marks: row.get("achieved_marks"),
        body_markdown,
        glossary_markdown,
        aliases,
        related_chunks,
    })
}

struct PageChatContext {
    book_title: String,
    page_number: i64,
    body_markdown: String,
}

async fn load_page_chat_context(
    pool: &SqlitePool,
    page_id: i64,
) -> Result<PageChatContext, String> {
    let page_row = sqlx::query(
        "SELECT p.page_number, sd.title AS book_title \
         FROM pages p \
         JOIN source_documents sd ON sd.id = p.source_document_id \
         WHERE p.id = ?",
    )
    .bind(page_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("page {} not found", page_id))?;

    let page_number: i64 = page_row.get("page_number");
    let book_title: String = page_row.get("book_title");

    let block_rows = sqlx::query(
        "SELECT COALESCE(NULLIF(TRIM(transcribed_text), ''), NULLIF(TRIM(text), '')) AS body \
         FROM text_blocks WHERE page_id = ? ORDER BY order_idx",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let block_body = block_rows
        .into_iter()
        .filter_map(|row| row.get::<Option<String>, _>("body"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let chunk_rows = sqlx::query(
        "SELECT chunk_type, title, subject, \
                COALESCE(NULLIF(TRIM(formatted_body_md), ''), NULLIF(TRIM(ocr_text), '')) AS body_markdown \
         FROM chunks \
         WHERE page_id = ? AND chunk_type != 'noise' \
         ORDER BY bbox_y, bbox_x",
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut chunk_sections = Vec::new();
    for (idx, row) in chunk_rows.into_iter().enumerate() {
        let chunk_type: String = row.get("chunk_type");
        let title = row
            .get::<Option<String>, _>("title")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let subject = row
            .get::<Option<String>, _>("subject")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let body = row
            .get::<Option<String>, _>("body_markdown")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let mut heading = format!("{}. [{}]", idx + 1, chunk_type);
        if let Some(value) = title {
            heading.push_str(&format!(" title: {value}"));
        }
        if let Some(value) = subject {
            heading.push_str(&format!(" subject: {value}"));
        }
        if let Some(value) = body {
            heading.push_str("\n");
            heading.push_str(&value);
        }
        chunk_sections.push(heading);
    }
    let chunk_body = chunk_sections.join("\n\n");

    if block_body.is_empty() && chunk_body.is_empty() {
        return Err("No page text is available for AI chat yet.".to_string());
    }

    let mut page_context = String::new();
    page_context.push_str(&format!("Page number: {}\n", page_number));
    if !block_body.is_empty() {
        page_context.push_str("\nExtracted page text (reading order):\n---\n");
        page_context.push_str(&block_body);
        page_context.push_str("\n---\n");
    }
    if !chunk_body.is_empty() {
        page_context.push_str("\nChunk summaries on this page:\n---\n");
        page_context.push_str(&chunk_body);
        page_context.push_str("\n---\n");
    }

    Ok(PageChatContext {
        book_title,
        page_number,
        body_markdown: page_context.trim().to_string(),
    })
}

async fn run_page_ai_stream(
    app: tauri::AppHandle,
    pool: SqlitePool,
    chat_streams: Arc<Mutex<HashMap<String, ChatStreamHandle>>>,
    request_id: String,
    page_id: i64,
    provider: LlmProvider,
    model_override: Option<String>,
    history: Vec<ChunkChatMessage>,
    cancelled: Arc<AtomicBool>,
) {
    let stream_context = ChatStreamContext::Page(page_id);
    let result = async {
        if cancelled.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }

        let context = load_page_chat_context(&pool, page_id)
            .await
            .map_err(LlmError::Config)?;
        let page_title = format!("Page {}", context.page_number);
        let prompt = ChunkChatPrompt {
            book_title: &context.book_title,
            chunk_type: "page",
            title: Some(page_title.as_str()),
            subject: None,
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
                &stream_context,
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
                emit_chunk_ai_stream(&app, &request_id, &stream_context, "completed", None, None);
            }
        }
        Err(LlmError::Cancelled) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(&app, &request_id, &stream_context, "cancelled", None, None);
            }
        }
        Err(err) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(
                    &app,
                    &request_id,
                    &stream_context,
                    "error",
                    None,
                    Some(err.to_string()),
                );
            }
        }
    }
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
    let stream_context = ChatStreamContext::Chunk(chunk_id);
    let result = async {
        if cancelled.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }

        let context = load_chunk_chat_context(&pool, chunk_id)
            .await
            .map_err(LlmError::Config)?;
        let prompt_body = compose_chunk_chat_body(&context);
        let prompt = ChunkChatPrompt {
            book_title: &context.book_title,
            chunk_type: &context.chunk_type,
            title: context.title.as_deref(),
            subject: context.subject.as_deref(),
            body_markdown: &prompt_body,
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
                &stream_context,
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
                emit_chunk_ai_stream(&app, &request_id, &stream_context, "completed", None, None);
            }
        }
        Err(LlmError::Cancelled) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(&app, &request_id, &stream_context, "cancelled", None, None);
            }
        }
        Err(err) => {
            if finish_chat_stream(&chat_streams, &request_id).is_some() {
                emit_chunk_ai_stream(
                    &app,
                    &request_id,
                    &stream_context,
                    "error",
                    None,
                    Some(err.to_string()),
                );
            }
        }
    }
}

async fn run_chunk_rewrite_request(
    pool: &SqlitePool,
    provider: LlmProvider,
    model: Option<String>,
    rewrite_prompt: &ChunkRewritePrompt<'_>,
) -> Result<ChunkRewriteResult, String> {
    let configured_api_key = settings::api_key_for_provider(pool, provider).await?;

    let result = match provider {
        LlmProvider::Ollama => OllamaClient::with_text_model(model.clone())
            .rewrite_chunk_with_prompt(rewrite_prompt)
            .await
            .map_err(|e| e.to_string())?,
        LlmProvider::OpenAI => {
            OpenAiClient::with_api_key_and_model(configured_api_key.clone(), model.clone())
                .rewrite_chunk_with_prompt(rewrite_prompt)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::Gemini => {
            GeminiClient::with_api_key_and_model(configured_api_key.clone(), model.clone())
                .rewrite_chunk_with_prompt(rewrite_prompt)
                .await
                .map_err(|e| e.to_string())?
        }
        LlmProvider::DeepSeek => DeepSeekClient::with_api_key_and_model(configured_api_key, model)
            .rewrite_chunk_with_prompt(rewrite_prompt)
            .await
            .map_err(|e| e.to_string())?,
        LlmProvider::Zai => {
            return Err("provider zai does not support chat".to_string());
        }
    };

    Ok(result)
}

#[tauri::command]
async fn rewrite_chunk_text_with_prompt(
    chunk_id: i64,
    prompt: String,
    provider: String,
    model: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkRewriteResult, String> {
    if chunk_id < 1 {
        return Err(format!("invalid chunk_id {}", chunk_id));
    }
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("prompt was empty".into());
    }

    let provider = provider.parse::<LlmProvider>()?;
    validate_chat_provider(provider)?;
    let model = normalize_model_override(model);
    let context = load_chunk_chat_context(pool.inner(), chunk_id).await?;
    let rewrite_prompt = ChunkRewritePrompt {
        book_title: &context.book_title,
        chunk_type: &context.chunk_type,
        title: context.title.as_deref(),
        subject: context.subject.as_deref(),
        body_markdown: &context.body_markdown,
        user_prompt: &prompt,
        image_base64_list: vec![],
    };
    let result = run_chunk_rewrite_request(pool.inner(), provider, model, &rewrite_prompt).await?;

    Ok(result)
}

#[derive(serde::Serialize)]
struct ChunkGlossaryRewriteResult {
    glossary_markdown: String,
}

#[tauri::command]
async fn rewrite_chunk_glossary_with_prompt(
    chunk_id: i64,
    prompt: String,
    provider: String,
    model: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkGlossaryRewriteResult, String> {
    if chunk_id < 1 {
        return Err(format!("invalid chunk_id {}", chunk_id));
    }
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("prompt was empty".into());
    }

    let provider = provider.parse::<LlmProvider>()?;
    validate_chat_provider(provider)?;
    let model = normalize_model_override(model);

    let row = sqlx::query(
        "SELECT sd.title AS book_title, c.chunk_type, c.title, c.subject, \
                COALESCE(NULLIF(TRIM(c.formatted_body_md), ''), NULLIF(TRIM(c.ocr_text), '')) AS body_markdown, \
                NULLIF(TRIM(c.glossary_md), '') AS glossary_markdown \
         FROM chunks c \
         JOIN source_documents sd ON sd.id = c.source_document_id \
         WHERE c.id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_body_markdown = row
        .get::<Option<String>, _>("body_markdown")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let glossary_markdown = row
        .get::<Option<String>, _>("glossary_markdown")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let book_title = row.get::<String, _>("book_title");
    let chunk_type = row.get::<String, _>("chunk_type");
    let title = row
        .get::<Option<String>, _>("title")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let subject = row
        .get::<Option<String>, _>("subject")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let wrapped_prompt = format!(
        "Edit ONLY the glossary entry markdown for this chunk.\n\
         Output requirements:\n\
         - Return title as null.\n\
         - Put the rewritten glossary entry in body_markdown.\n\
         - Keep glossary prose plain (no headings or bold).\n\
         - Preserve mathematical correctness and use LaTeX where helpful.\n\
         - If the glossary should stay exactly unchanged, return body_markdown as null.\n\n\
         Current glossary markdown:\n---\n{}\n---\n\n\
         Chunk body reference (for context only):\n---\n{}\n---\n\n\
         User glossary edit instruction:\n{}",
        glossary_markdown, chunk_body_markdown, prompt
    );

    let rewrite_prompt = ChunkRewritePrompt {
        book_title: book_title.trim(),
        chunk_type: chunk_type.trim(),
        title: title.as_deref(),
        subject: subject.as_deref(),
        body_markdown: &glossary_markdown,
        user_prompt: &wrapped_prompt,
        image_base64_list: vec![],
    };

    let result = run_chunk_rewrite_request(pool.inner(), provider, model, &rewrite_prompt).await?;
    let glossary_markdown = result
        .body_markdown
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "AI response did not include glossary markdown edits.".to_string())?;

    Ok(ChunkGlossaryRewriteResult { glossary_markdown })
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
    let cancelled = begin_chat_stream(
        state.inner(),
        &request_id,
        ChatStreamContext::Chunk(chunk_id),
    )?;
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
async fn start_page_ai_stream(
    request_id: String,
    page_id: i64,
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
    if page_id < 1 {
        return Err(format!("invalid page_id {}", page_id));
    }

    let provider = provider.parse::<LlmProvider>()?;
    validate_chat_provider(provider)?;
    let model = normalize_model_override(model);
    let history = sanitize_chunk_chat_history(history)?;
    let cancelled =
        begin_chat_stream(state.inner(), &request_id, ChatStreamContext::Page(page_id))?;
    let chat_streams = Arc::clone(&state.chat_streams);
    let pool = pool.inner().clone();

    tokio::spawn(run_page_ai_stream(
        app,
        pool,
        chat_streams,
        request_id,
        page_id,
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
        emit_chunk_ai_stream(&app, &request_id, &handle.context, "cancelled", None, None);
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
    glossary_preview: Option<String>,
    has_formatted_body: bool,
    has_self_explanation: bool,
    question_label: Option<String>,
    available_marks: Option<i64>,
    achieved_marks: Option<f64>,
}

#[derive(serde::Serialize)]
struct QuestionSourceSlice {
    page_id: i64,
    page_number: i64,
    bbox_x: f32,
    bbox_y: f32,
    bbox_w: f32,
    bbox_h: f32,
    slice_order: i64,
}

#[derive(serde::Serialize)]
struct QuestionMarkAttemptView {
    id: i64,
    source: String,
    achieved_marks: Option<f64>,
    available_marks_snapshot: Option<i64>,
    feedback_md: Option<String>,
    include_visuals: bool,
    provider: Option<String>,
    model: Option<String>,
    created_at: String,
}

#[derive(serde::Serialize)]
struct QuestionMarkSuggestion {
    achieved_marks: Option<f64>,
    feedback_md: String,
}

#[tauri::command]
async fn get_chunk_preview(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<ChunkPreview, String> {
    let row = sqlx::query(
        "SELECT id, chunk_type, title, subject, status, formatted_body_md, ocr_text, glossary_md, \
                question_label, available_marks, achieved_marks \
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
    let glossary: Option<String> = row.get("glossary_md");
    let glossary_source = glossary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let glossary_preview = glossary_source.map(truncate_markdown_preview);

    Ok(ChunkPreview {
        id: row.get("id"),
        chunk_type: row.get("chunk_type"),
        title: row.get("title"),
        subject: row.get("subject"),
        status: row.get("status"),
        body_preview,
        glossary_preview,
        has_formatted_body,
        has_self_explanation: glossary_source.is_some(),
        question_label: row.get("question_label"),
        available_marks: row.get("available_marks"),
        achieved_marks: row.get("achieved_marks"),
    })
}

#[tauri::command]
async fn get_question_source_slices(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<QuestionSourceSlice>, String> {
    let rows = sqlx::query(
        "SELECT qps.page_id, p.page_number, qps.bbox_x, qps.bbox_y, qps.bbox_w, qps.bbox_h, qps.slice_order \
         FROM question_page_slices qps \
         JOIN pages p ON p.id = qps.page_id \
         WHERE qps.chunk_id = ? \
         ORDER BY p.page_number, qps.slice_order",
    )
    .bind(chunk_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|row| QuestionSourceSlice {
            page_id: row.get("page_id"),
            page_number: row.get("page_number"),
            bbox_x: row.get("bbox_x"),
            bbox_y: row.get("bbox_y"),
            bbox_w: row.get("bbox_w"),
            bbox_h: row.get("bbox_h"),
            slice_order: row.get("slice_order"),
        })
        .collect())
}

#[tauri::command]
async fn list_question_mark_attempts(
    chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<QuestionMarkAttemptView>, String> {
    let rows = sqlx::query(
        "SELECT id, source, achieved_marks, available_marks_snapshot, feedback_md, \
                include_visuals, provider, model, created_at \
         FROM question_mark_attempts \
         WHERE chunk_id = ? \
         ORDER BY id DESC",
    )
    .bind(chunk_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|row| QuestionMarkAttemptView {
            id: row.get("id"),
            source: row.get("source"),
            achieved_marks: row.get("achieved_marks"),
            available_marks_snapshot: row.get("available_marks_snapshot"),
            feedback_md: row.get("feedback_md"),
            include_visuals: row.get::<i64, _>("include_visuals") != 0,
            provider: row.get("provider"),
            model: row.get("model"),
            created_at: row.get("created_at"),
        })
        .collect())
}

fn validate_achieved_marks(
    achieved_marks: Option<f64>,
    available_marks: Option<i64>,
) -> Result<(), String> {
    if let Some(value) = achieved_marks {
        if !value.is_finite() || value < 0.0 {
            return Err("achieved_marks must be a non-negative number".to_string());
        }
        if let Some(max) = available_marks {
            if value > max as f64 {
                return Err(format!(
                    "achieved_marks ({value}) cannot exceed available_marks ({max})"
                ));
            }
        }
    }
    Ok(())
}

fn validate_available_marks(
    available_marks: Option<i64>,
    achieved_marks: Option<f64>,
) -> Result<(), String> {
    if let Some(value) = available_marks {
        if value < 0 {
            return Err("available_marks must be a non-negative integer".to_string());
        }
        if let Some(achieved) = achieved_marks {
            if achieved > value as f64 {
                return Err(format!(
                    "available_marks ({value}) cannot be less than achieved_marks ({achieved})"
                ));
            }
        }
    }
    Ok(())
}

#[tauri::command]
async fn save_question_available_marks(
    chunk_id: i64,
    available_marks: Option<i64>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let row = sqlx::query("SELECT chunk_type, achieved_marks FROM chunks WHERE id = ?")
        .bind(chunk_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_type: String = row.get("chunk_type");
    if chunk_type != "question" {
        return Err("available marks can only be saved for question chunks".to_string());
    }
    let achieved_marks: Option<f64> = row.get("achieved_marks");
    validate_available_marks(available_marks, achieved_marks)?;

    sqlx::query("UPDATE chunks SET available_marks = ? WHERE id = ?")
        .bind(available_marks)
        .bind(chunk_id)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
async fn save_question_achieved_marks(
    chunk_id: i64,
    achieved_marks: Option<f64>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let row = sqlx::query("SELECT chunk_type, available_marks FROM chunks WHERE id = ?")
        .bind(chunk_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_type: String = row.get("chunk_type");
    if chunk_type != "question" {
        return Err("achieved marks can only be saved for question chunks".to_string());
    }
    let available_marks: Option<i64> = row.get("available_marks");
    validate_achieved_marks(achieved_marks, available_marks)?;

    let mut tx = pool.inner().begin().await.map_err(|e| e.to_string())?;
    sqlx::query(
        "UPDATE chunks SET achieved_marks = ?, achieved_marks_updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(achieved_marks)
    .bind(chunk_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT INTO question_mark_attempts \
         (chunk_id, source, achieved_marks, available_marks_snapshot, feedback_md, include_visuals) \
         VALUES (?, 'manual', ?, ?, NULL, 0)",
    )
    .bind(chunk_id)
    .bind(achieved_marks)
    .bind(available_marks)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn mark_question_answer_with_ai(
    chunk_id: i64,
    provider: String,
    model: Option<String>,
    include_visuals: Option<bool>,
    visual_context_images: Option<Vec<String>>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<QuestionMarkSuggestion, String> {
    let provider = provider.parse::<LlmProvider>()?;
    validate_chat_provider(provider)?;
    let model = normalize_model_override(model);
    let include_visuals = include_visuals.unwrap_or(false);
    let visual_context_images: Vec<String> = visual_context_images
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();

    let row = sqlx::query(
        "SELECT sd.title AS book_title, c.chunk_type, c.question_label, c.available_marks, \
                COALESCE(NULLIF(TRIM(c.formatted_body_md), ''), NULLIF(TRIM(c.ocr_text), '')) AS question_body, \
                NULLIF(TRIM(c.glossary_md), '') AS answer_body \
         FROM chunks c \
         JOIN source_documents sd ON sd.id = c.source_document_id \
         WHERE c.id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_type: String = row.get("chunk_type");
    if chunk_type != "question" {
        return Err("AI marking is only available for question chunks".to_string());
    }

    let book_title: String = row.get("book_title");
    let question_label: Option<String> = row.get("question_label");
    let available_marks: Option<i64> = row.get("available_marks");
    let question_body = row
        .get::<Option<String>, _>("question_body")
        .unwrap_or_default();
    let answer_body = row
        .get::<Option<String>, _>("answer_body")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if answer_body.is_none() && visual_context_images.is_empty() {
        return Err("No answer text found. Add answer content before marking.".to_string());
    }
    let answer_body = answer_body.unwrap_or_default();

    let user_prompt = format!(
        "Mark the student's answer to this exam question.\n\
         Return JSON with this schema:\n\
         - title: numeric achieved marks only (string), or null if cannot score\n\
         - body_markdown: concise feedback in markdown\n\n\
         Marking requirements:\n\
         1. Score against available marks when provided.\n\
         2. Keep feedback actionable and specific.\n\
         3. If score is uncertain, set title to null and explain what is missing.\n\
         4. Do not include headings in feedback.\n\
         5. If images of the student's working are attached, use them as the primary answer source.\n\n\
         Question label: {}\n\
         Available marks: {}\n\n\
         Question text:\n---\n{}\n---\n\n\
         Student answer (text):\n---\n{}\n---",
        question_label.as_deref().unwrap_or("unknown"),
        available_marks
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        question_body.trim(),
        answer_body.trim(),
    );

    let rewrite_prompt = ChunkRewritePrompt {
        book_title: book_title.trim(),
        chunk_type: "question",
        title: question_label.as_deref(),
        subject: None,
        body_markdown: question_body.trim(),
        user_prompt: &user_prompt,
        image_base64_list: if include_visuals { visual_context_images } else { vec![] },
    };
    let result = run_chunk_rewrite_request(pool.inner(), provider, model, &rewrite_prompt).await?;

    let achieved_marks = result
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok());
    validate_achieved_marks(achieved_marks, available_marks)?;

    let feedback_md = result
        .body_markdown
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "No feedback was generated.".to_string());

    Ok(QuestionMarkSuggestion {
        achieved_marks,
        feedback_md,
    })
}

#[tauri::command]
async fn apply_question_mark_attempt(
    chunk_id: i64,
    source: String,
    achieved_marks: Option<f64>,
    feedback_md: Option<String>,
    include_visuals: Option<bool>,
    provider: Option<String>,
    model: Option<String>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let source = source.trim().to_lowercase();
    if source != "ai" && source != "manual" {
        return Err("source must be 'ai' or 'manual'".to_string());
    }
    let include_visuals = include_visuals.unwrap_or(false);
    let feedback_md = feedback_md
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let provider = provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let model = model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());

    let row = sqlx::query("SELECT chunk_type, available_marks FROM chunks WHERE id = ?")
        .bind(chunk_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("chunk {} not found", chunk_id))?;

    let chunk_type: String = row.get("chunk_type");
    if chunk_type != "question" {
        return Err("mark attempts can only be saved for question chunks".to_string());
    }
    let available_marks: Option<i64> = row.get("available_marks");
    validate_achieved_marks(achieved_marks, available_marks)?;

    let mut tx = pool.inner().begin().await.map_err(|e| e.to_string())?;
    sqlx::query(
        "UPDATE chunks SET achieved_marks = ?, achieved_marks_updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(achieved_marks)
    .bind(chunk_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT INTO question_mark_attempts \
         (chunk_id, source, achieved_marks, available_marks_snapshot, feedback_md, include_visuals, provider, model) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(chunk_id)
    .bind(&source)
    .bind(achieved_marks)
    .bind(available_marks)
    .bind(feedback_md.as_deref())
    .bind(if include_visuals { 1 } else { 0 })
    .bind(provider.as_deref())
    .bind(model.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_proof_chunk_for_target(
    target_chunk_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Option<i64>, String> {
    let proof_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM chunks \
         WHERE chunk_type = 'proof' AND proves_chunk_id = ? \
         ORDER BY id LIMIT 1",
    )
    .bind(target_chunk_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(proof_id)
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
    pool: tauri::State<'_, SqlitePool>,
) -> Result<bool, String> {
    let mode_row = sqlx::query("SELECT document_mode FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let is_past_paper = mode_row
        .as_ref()
        .map(|row| {
            row.get::<String, _>("document_mode")
                .eq_ignore_ascii_case("past_paper")
        })
        .unwrap_or(false);

    let jobs = state.chunking_jobs.lock().map_err(|e| e.to_string())?;
    if is_past_paper {
        Ok(jobs.iter().any(|(doc_id, _)| *doc_id == source_document_id))
    } else {
        Ok(jobs.contains(&(source_document_id, page_number)))
    }
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
    let mode_row = sqlx::query("SELECT document_mode FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source document {} not found", source_document_id))?;
    let is_past_paper = mode_row
        .get::<String, _>("document_mode")
        .eq_ignore_ascii_case("past_paper");
    let effective_page = if is_past_paper { 1 } else { page_number };

    let provider = provider.parse::<LlmProvider>()?;
    validate_chunking_provider(provider)?;
    let model = normalize_model_override(model);

    sqlx::query("INSERT OR IGNORE INTO pages (source_document_id, page_number) VALUES (?, ?)")
        .bind(source_document_id)
        .bind(effective_page)
        .execute(pool.inner())
        .await
        .map_err(|e| e.to_string())?;
    let page_row =
        sqlx::query("SELECT id FROM pages WHERE source_document_id = ? AND page_number = ?")
            .bind(source_document_id)
            .bind(effective_page)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?;
    let page_id: i64 = page_row.get("id");
    let existing_chunks: i64 = if is_past_paper {
        sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE source_document_id = ?")
            .bind(source_document_id)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?
    } else {
        sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE page_id = ?")
            .bind(page_id)
            .fetch_one(pool.inner())
            .await
            .map_err(|e| e.to_string())?
    };
    if existing_chunks > 0 {
        info!(
            target: "gloss_lib::chunking",
            "not starting chunking for doc_id={} requested_page={} effective_page={} because {} chunks already exist",
            source_document_id,
            page_number,
            effective_page,
            existing_chunks
        );
        return Ok(false);
    }

    spawn_chunking_job(
        app,
        pool.inner().clone(),
        state.inner(),
        source_document_id,
        effective_page,
        provider,
        model,
    )
}

#[derive(serde::Serialize)]
struct EnsureChunkingRangeResult {
    requested_start_page: i64,
    requested_end_page: i64,
    started_pages: i64,
    started_page_numbers: Vec<i64>,
    skipped_pages: i64,
}

#[tauri::command]
async fn ensure_chunking_for_page_range(
    source_document_id: i64,
    start_page: i64,
    end_page: i64,
    skip_chunked_pages: Option<bool>,
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
    let skip_chunked_pages = skip_chunked_pages.unwrap_or(true);
    let model = normalize_model_override(model);
    let mode_row = sqlx::query("SELECT document_mode FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source document {} not found", source_document_id))?;
    let is_past_paper = mode_row
        .get::<String, _>("document_mode")
        .eq_ignore_ascii_case("past_paper");

    if is_past_paper {
        let existing_chunks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE source_document_id = ?")
                .bind(source_document_id)
                .fetch_one(pool.inner())
                .await
                .map_err(|e| e.to_string())?;

        let range_len = end_page - start_page + 1;
        let mut skipped_pages = if range_len > 1 { range_len - 1 } else { 0 };
        if skip_chunked_pages && existing_chunks > 0 {
            skipped_pages += 1;
            return Ok(EnsureChunkingRangeResult {
                requested_start_page: start_page,
                requested_end_page: end_page,
                started_pages: 0,
                started_page_numbers: Vec::new(),
                skipped_pages,
            });
        }
        if !begin_chunking_job(state.inner(), source_document_id, 1)? {
            skipped_pages += 1;
            return Ok(EnsureChunkingRangeResult {
                requested_start_page: start_page,
                requested_end_page: end_page,
                started_pages: 0,
                started_page_numbers: Vec::new(),
                skipped_pages,
            });
        }

        let chunking_jobs = Arc::clone(&state.chunking_jobs);
        let pdfium = state.pdfium.clone();
        let zai_transcription_semaphore = Arc::clone(&state.zai_transcription_semaphore);
        let pool = pool.inner().clone();
        let app_for_tasks = app.clone();
        tokio::spawn(async move {
            if existing_chunks > 0 {
                let _ = chunking::rechunk_page(
                    &pool,
                    &pdfium,
                    &app_for_tasks,
                    source_document_id,
                    1,
                    provider,
                    model.clone(),
                    Arc::clone(&zai_transcription_semaphore),
                )
                .await;
            } else {
                chunking::run_for_page(
                    pool.clone(),
                    pdfium.clone(),
                    app_for_tasks.clone(),
                    source_document_id,
                    1,
                    provider,
                    model.clone(),
                    Arc::clone(&zai_transcription_semaphore),
                )
                .await;
            }
            finish_chunking_job(&chunking_jobs, source_document_id, 1);
        });

        return Ok(EnsureChunkingRangeResult {
            requested_start_page: start_page,
            requested_end_page: end_page,
            started_pages: 1,
            started_page_numbers: vec![1],
            skipped_pages,
        });
    }

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

    let mut pages_to_start: Vec<(i64, bool)> = Vec::new();
    let mut skipped_pages = 0_i64;

    for page in start_page..=end_page {
        let has_existing_chunks = existing_chunk_pages.contains(&page);
        if skip_chunked_pages && has_existing_chunks {
            skipped_pages += 1;
            continue;
        }
        if !begin_chunking_job(state.inner(), source_document_id, page)? {
            skipped_pages += 1;
            continue;
        }
        pages_to_start.push((page, has_existing_chunks));
    }

    let started_page_numbers: Vec<i64> = pages_to_start
        .iter()
        .map(|(page_number, _)| *page_number)
        .collect();
    let started_pages = started_page_numbers.len() as i64;
    if started_pages > 0 {
        let chunking_jobs = Arc::clone(&state.chunking_jobs);
        let pdfium = state.pdfium.clone();
        let zai_transcription_semaphore = Arc::clone(&state.zai_transcription_semaphore);
        let pool = pool.inner().clone();
        let app_for_tasks = app.clone();
        tokio::spawn(async move {
            for (page_number, has_existing_chunks) in pages_to_start {
                if has_existing_chunks {
                    let _ = chunking::rechunk_page(
                        &pool,
                        &pdfium,
                        &app_for_tasks,
                        source_document_id,
                        page_number,
                        provider,
                        model.clone(),
                        Arc::clone(&zai_transcription_semaphore),
                    )
                    .await;
                } else {
                    chunking::run_for_page(
                        pool.clone(),
                        pdfium.clone(),
                        app_for_tasks.clone(),
                        source_document_id,
                        page_number,
                        provider,
                        model.clone(),
                        Arc::clone(&zai_transcription_semaphore),
                    )
                    .await;
                }
                finish_chunking_job(&chunking_jobs, source_document_id, page_number);
            }
        });
    }

    Ok(EnsureChunkingRangeResult {
        requested_start_page: start_page,
        requested_end_page: end_page,
        started_pages,
        started_page_numbers,
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
    let mode_row = sqlx::query("SELECT document_mode FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source document {} not found", source_document_id))?;
    let is_past_paper = mode_row
        .get::<String, _>("document_mode")
        .eq_ignore_ascii_case("past_paper");
    let effective_page = if is_past_paper { 1 } else { page_number };

    let provider = provider.parse::<LlmProvider>()?;
    validate_chunking_provider(provider)?;
    let model = normalize_model_override(model);
    if !begin_chunking_job(state.inner(), source_document_id, effective_page)? {
        return Err("chunking already active for this page".into());
    }

    let result = chunking::rechunk_page(
        pool.inner(),
        &state.pdfium,
        &app,
        source_document_id,
        effective_page,
        provider,
        model,
        Arc::clone(&state.zai_transcription_semaphore),
    )
    .await;
    finish_chunking_job(&state.chunking_jobs, source_document_id, effective_page);
    result
}

#[tauri::command]
async fn import_pdf(
    document_mode: Option<String>,
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

    let document_mode = match document_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) if value.eq_ignore_ascii_case("past_paper") => "past_paper",
        _ => "textbook",
    };

    let (instruction_page_start, instruction_page_end) = if document_mode == "past_paper" {
        (Some(1_i64), Some(1_i64))
    } else {
        (None, None)
    };

    let row = sqlx::query(
        "INSERT INTO source_documents \
         (title, file_path, document_mode, instruction_page_start, instruction_page_end) \
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&title)
    .bind(&relative_path)
    .bind(document_mode)
    .bind(instruction_page_start)
    .bind(instruction_page_end)
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
        document_mode: document_mode.to_string(),
        instruction_page_start,
        instruction_page_end,
    })
}

fn file_path_label(path: &FilePath) -> String {
    match path {
        FilePath::Path(path) => path.to_string_lossy().to_string(),
        FilePath::Url(url) => url.to_string(),
    }
}

fn quote_sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn open_file_for_overwrite(
    app: &tauri::AppHandle,
    path: FilePath,
) -> Result<std::fs::File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    app.fs().open(path, options).map_err(|e| e.to_string())
}

async fn replace_main_database_from_staged_import(
    pool: &SqlitePool,
    staged_import_path: &std::path::Path,
) -> Result<(), String> {
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    let attach_result = sqlx::query("ATTACH DATABASE ? AS imported")
        .bind(staged_import_path.to_string_lossy().to_string())
        .execute(pool)
        .await;
    if let Err(err) = attach_result {
        let _ = sqlx::query("PRAGMA foreign_keys = ON").execute(pool).await;
        return Err(err.to_string());
    }

    let table_rows = match sqlx::query(
        "SELECT name FROM main.sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            let _ = sqlx::query("DETACH DATABASE imported").execute(pool).await;
            let _ = sqlx::query("PRAGMA foreign_keys = ON").execute(pool).await;
            return Err(err.to_string());
        }
    };

    let mut restore_result = sqlx::query("BEGIN IMMEDIATE TRANSACTION")
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());

    if restore_result.is_ok() {
        for row in table_rows {
            let table_name: String = row.get("name");
            let quoted = quote_sqlite_identifier(&table_name);
            let delete_sql = format!("DELETE FROM {quoted}");
            if let Err(err) = sqlx::query(&delete_sql).execute(pool).await {
                restore_result = Err(format!(
                    "failed clearing table {table_name} during import: {err}"
                ));
                break;
            }

            let insert_sql = format!("INSERT INTO {quoted} SELECT * FROM imported.{quoted}");
            if let Err(err) = sqlx::query(&insert_sql).execute(pool).await {
                restore_result = Err(format!(
                    "failed copying table {table_name} during import: {err}"
                ));
                break;
            }
        }
    }

    if restore_result.is_ok() {
        let imported_has_sqlite_sequence = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM imported.sqlite_master \
             WHERE type = 'table' AND name = 'sqlite_sequence' \
             LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .is_some();

        if imported_has_sqlite_sequence {
            if let Err(err) = sqlx::query("DELETE FROM sqlite_sequence").execute(pool).await {
                restore_result = Err(format!("failed clearing sqlite_sequence during import: {err}"));
            } else if let Err(err) = sqlx::query(
                "INSERT INTO sqlite_sequence(name, seq) \
                 SELECT name, seq FROM imported.sqlite_sequence",
            )
            .execute(pool)
            .await
            {
                restore_result = Err(format!("failed copying sqlite_sequence during import: {err}"));
            }
        }
    }

    if restore_result.is_ok() {
        if let Err(err) = sqlx::query("COMMIT").execute(pool).await {
            restore_result = Err(err.to_string());
        }
    } else {
        let _ = sqlx::query("ROLLBACK").execute(pool).await;
    }

    let detach_result = sqlx::query("DETACH DATABASE imported")
        .execute(pool)
        .await
        .map_err(|e| e.to_string());
    let _ = sqlx::query("PRAGMA foreign_keys = ON").execute(pool).await;

    restore_result?;
    detach_result?;

    let fk_violation = sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    if fk_violation.is_some() {
        return Err("import finished but foreign key checks failed".to_string());
    }

    Ok(())
}

#[tauri::command]
async fn export_database_file(
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<String, String> {
    let destination = app
        .dialog()
        .file()
        .add_filter("SQLite DB", &["db", "sqlite", "sqlite3"])
        .set_file_name("gloss-backup.db")
        .blocking_save_file();
    let destination = match destination {
        Some(path) => path,
        None => return Err("cancelled".into()),
    };
    let destination_label = file_path_label(&destination);

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let temp_export_path = data_dir.join("gloss-db-export.tmp.db");
    if temp_export_path.exists() {
        std::fs::remove_file(&temp_export_path).map_err(|e| e.to_string())?;
    }

    let export_result = async {
        sqlx::query("VACUUM INTO ?")
            .bind(temp_export_path.to_string_lossy().to_string())
            .execute(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

        let bytes = std::fs::read(&temp_export_path).map_err(|e| e.to_string())?;
        let mut output = open_file_for_overwrite(&app, destination)?;
        output.write_all(&bytes).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
        Ok::<String, String>(destination_label)
    }
    .await;

    let _ = std::fs::remove_file(&temp_export_path);

    if let Ok(path) = &export_result {
        info!(target: "gloss_lib::sync", "exported database backup to {}", path);
    }
    export_result
}

#[tauri::command]
async fn import_database_file(
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<String, String> {
    let picked = app
        .dialog()
        .file()
        .add_filter("SQLite DB", &["db", "sqlite", "sqlite3"])
        .blocking_pick_file();

    let picked = match picked {
        Some(path) => path,
        None => return Err("cancelled".into()),
    };
    let picked_label = file_path_label(&picked);
    let picked_label_result = picked_label.clone();

    let bytes = app.fs().read(picked).map_err(|e| e.to_string())?;

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let staged_import_path = data_dir.join("gloss-db-import.tmp.db");
    if staged_import_path.exists() {
        std::fs::remove_file(&staged_import_path).map_err(|e| e.to_string())?;
    }
    std::fs::write(&staged_import_path, &bytes).map_err(|e| e.to_string())?;

    let import_result = async {
        let staged_db_url = format!("sqlite://{}?mode=rwc", staged_import_path.display());
        let staged_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&staged_db_url)
            .await
            .map_err(|e| e.to_string())?;

        sqlx::migrate!("./migrations")
            .run(&staged_pool)
            .await
            .map_err(|e| e.to_string())?;
        staged_pool.close().await;

        replace_main_database_from_staged_import(pool.inner(), &staged_import_path).await?;
        sqlx::migrate!("./migrations")
            .run(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

        Ok::<String, String>(picked_label_result)
    }
    .await;

    let _ = std::fs::remove_file(&staged_import_path);

    if let Ok(path) = &import_result {
        info!(target: "gloss_lib::sync", "imported database backup from {}", path);
    }
    import_result
}

#[tauri::command]
async fn get_pdf_path(app: tauri::AppHandle, relative_path: String) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs = data_dir.join(&relative_path);
    Ok(abs.to_string_lossy().to_string())
}

#[tauri::command]
async fn list_textbooks(pool: tauri::State<'_, SqlitePool>) -> Result<Vec<SourceDocument>, String> {
    let rows = sqlx::query(
        "SELECT id, title, file_path, document_mode, instruction_page_start, instruction_page_end \
         FROM source_documents ORDER BY id DESC",
    )
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SourceDocument {
            id: r.get("id"),
            title: r.get("title"),
            file_path: r.get("file_path"),
            document_mode: r.get("document_mode"),
            instruction_page_start: r.get("instruction_page_start"),
            instruction_page_end: r.get("instruction_page_end"),
        })
        .collect())
}

fn resolve_document_pdf_path(
    data_dir: &std::path::Path,
    relative_path: &str,
) -> Result<std::path::PathBuf, String> {
    let relative = std::path::Path::new(relative_path);
    if relative.is_absolute() {
        return Err(format!(
            "invalid stored file path '{}' (expected relative path)",
            relative_path
        ));
    }
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "invalid stored file path '{}' (path traversal not allowed)",
            relative_path
        ));
    }
    Ok(data_dir.join(relative))
}

#[tauri::command]
async fn delete_source_document(
    source_document_id: i64,
    app: tauri::AppHandle,
    pool: tauri::State<'_, SqlitePool>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    {
        let jobs = state.chunking_jobs.lock().map_err(|e| e.to_string())?;
        if jobs.iter().any(|(doc_id, _)| *doc_id == source_document_id) {
            return Err("cannot delete document while chunking is active".to_string());
        }
    }

    let row = sqlx::query("SELECT title, file_path FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source document {} not found", source_document_id))?;

    let title: String = row.get("title");
    let relative_path: String = row.get("file_path");
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let abs_pdf_path = resolve_document_pdf_path(&data_dir, &relative_path)?;

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM surface_graph_objects \
         WHERE surface_id IN ( \
             SELECT ns.id \
             FROM note_surfaces ns \
             LEFT JOIN pages p ON p.id = ns.page_id \
             LEFT JOIN chunks c ON c.id = ns.chunk_id \
             WHERE p.source_document_id = ? OR c.source_document_id = ? \
         )",
    )
    .bind(source_document_id)
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM surface_strokes \
         WHERE surface_id IN ( \
             SELECT ns.id \
             FROM note_surfaces ns \
             LEFT JOIN pages p ON p.id = ns.page_id \
             LEFT JOIN chunks c ON c.id = ns.chunk_id \
             WHERE p.source_document_id = ? OR c.source_document_id = ? \
         )",
    )
    .bind(source_document_id)
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM note_surfaces \
         WHERE page_id IN (SELECT id FROM pages WHERE source_document_id = ?) \
            OR chunk_id IN (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(source_document_id)
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM question_page_slices \
         WHERE page_id IN (SELECT id FROM pages WHERE source_document_id = ?) \
            OR chunk_id IN (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(source_document_id)
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM question_mark_attempts \
         WHERE chunk_id IN (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM chunk_aliases \
         WHERE chunk_id IN (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(
        "DELETE FROM chunk_references \
         WHERE source_chunk_id IN (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(source_document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM text_blocks WHERE page_id IN (SELECT id FROM pages WHERE source_document_id = ?)")
        .bind(source_document_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM strokes WHERE page_id IN (SELECT id FROM pages WHERE source_document_id = ?)")
        .bind(source_document_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM chunks WHERE source_document_id = ?")
        .bind(source_document_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM pages WHERE source_document_id = ?")
        .bind(source_document_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("DELETE FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;

    if let Ok(mut cache) = state.pdf_cache.lock() {
        cache.remove_document(&relative_path);
    }

    if let Err(err) = std::fs::remove_file(&abs_pdf_path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            log::warn!(
                target: "gloss_lib::library",
                "deleted doc_id={} title=\"{}\" but failed to remove file {}: {}",
                source_document_id,
                title,
                abs_pdf_path.display(),
                err
            );
        }
    }

    info!(
        target: "gloss_lib::library",
        "deleted source document doc_id={} title=\"{}\" relative_path=\"{}\"",
        source_document_id,
        title,
        relative_path
    );

    Ok(())
}

#[tauri::command]
async fn save_past_paper_instruction_range(
    source_document_id: i64,
    start_page: Option<i64>,
    end_page: Option<i64>,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<SourceDocument, String> {
    let mode_row = sqlx::query("SELECT document_mode FROM source_documents WHERE id = ?")
        .bind(source_document_id)
        .fetch_optional(pool.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("source document {} not found", source_document_id))?;

    let is_past_paper = mode_row
        .get::<String, _>("document_mode")
        .eq_ignore_ascii_case("past_paper");
    if !is_past_paper {
        return Err("instruction page range can only be set for past papers".to_string());
    }

    match (start_page, end_page) {
        (None, None) => {}
        (Some(start), Some(end)) => {
            if start < 1 || end < 1 {
                return Err("instruction page range must use page numbers >= 1".to_string());
            }
            if start > end {
                return Err(format!(
                    "invalid instruction page range {}-{} (end before start)",
                    start, end
                ));
            }
        }
        _ => {
            return Err(
                "instruction page range must provide both start_page and end_page, or neither"
                    .to_string(),
            );
        }
    }

    sqlx::query(
        "UPDATE source_documents \
         SET instruction_page_start = ?, instruction_page_end = ? \
         WHERE id = ?",
    )
    .bind(start_page)
    .bind(end_page)
    .bind(source_document_id)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query(
        "SELECT id, title, file_path, document_mode, instruction_page_start, instruction_page_end \
         FROM source_documents WHERE id = ?",
    )
    .bind(source_document_id)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(SourceDocument {
        id: row.get("id"),
        title: row.get("title"),
        file_path: row.get("file_path"),
        document_mode: row.get("document_mode"),
        instruction_page_start: row.get("instruction_page_start"),
        instruction_page_end: row.get("instruction_page_end"),
    })
}

#[tauri::command]
async fn inspect_past_paper_instruction_context(
    source_document_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<PastPaperInstructionContextDebug, String> {
    let row = sqlx::query(
        "SELECT document_mode, chunking_status, instruction_page_start, instruction_page_end \
         FROM source_documents WHERE id = ?",
    )
    .bind(source_document_id)
    .fetch_optional(pool.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("source document {} not found", source_document_id))?;

    let document_mode: String = row.get("document_mode");
    if !document_mode.eq_ignore_ascii_case("past_paper") {
        return Err("instruction context is only available for past papers".to_string());
    }

    let chunking_status: String = row.get("chunking_status");
    let start_page: Option<i64> = row.get("instruction_page_start");
    let end_page: Option<i64> = row.get("instruction_page_end");

    let mut expected_instruction_pages = Vec::new();
    let mut extracted_instruction_pages = Vec::new();
    let mut missing_instruction_pages = Vec::new();
    let mut instruction_block_count = 0i64;
    let mut instruction_transcribed_block_count = 0i64;
    let mut preview_segments: Vec<String> = Vec::new();

    match (start_page, end_page) {
        (None, None) => {}
        (Some(start), Some(end)) => {
            if start < 1 || end < 1 || start > end {
                return Err(format!(
                    "invalid instruction page range {}-{} (save a valid range first)",
                    start, end
                ));
            }

            expected_instruction_pages = (start..=end).collect();

            let rows = sqlx::query(
                "SELECT p.page_number, tb.id AS block_id, tb.order_idx, tb.text, tb.transcribed_text \
                 FROM pages p \
                 LEFT JOIN text_blocks tb ON tb.page_id = p.id \
                 WHERE p.source_document_id = ? AND p.page_number BETWEEN ? AND ? \
                 ORDER BY p.page_number ASC, tb.order_idx ASC",
            )
            .bind(source_document_id)
            .bind(start)
            .bind(end)
            .fetch_all(pool.inner())
            .await
            .map_err(|e| e.to_string())?;

            let mut extracted_page_set = HashSet::new();
            for result_row in rows {
                let page_number: i64 = result_row.get("page_number");
                let block_id: Option<i64> = result_row.get("block_id");
                if block_id.is_none() {
                    continue;
                }

                extracted_page_set.insert(page_number);
                instruction_block_count += 1;

                let transcribed_text = result_row.get::<Option<String>, _>("transcribed_text");
                let raw_text = result_row.get::<String, _>("text");

                let preferred = transcribed_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if preferred.is_some() {
                    instruction_transcribed_block_count += 1;
                }

                if let Some(text) = preferred.or_else(|| {
                    let trimmed = raw_text.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                }) {
                    preview_segments.push(text.to_string());
                }
            }

            extracted_instruction_pages = expected_instruction_pages
                .iter()
                .copied()
                .filter(|page| extracted_page_set.contains(page))
                .collect();
            missing_instruction_pages = expected_instruction_pages
                .iter()
                .copied()
                .filter(|page| !extracted_page_set.contains(page))
                .collect();
        }
        _ => {
            return Err(
                "instruction page range is partially set; save both start and end page".to_string(),
            );
        }
    }

    let preview_text = if preview_segments.is_empty() {
        String::new()
    } else {
        let joined = preview_segments.join("\n\n");
        let mut truncated = String::new();
        for ch in joined.chars().take(1600) {
            truncated.push(ch);
        }
        truncated
    };

    Ok(PastPaperInstructionContextDebug {
        source_document_id,
        chunking_status,
        instruction_page_start: start_page,
        instruction_page_end: end_page,
        expected_instruction_pages,
        extracted_instruction_pages,
        missing_instruction_pages,
        instruction_block_count,
        instruction_transcribed_block_count,
        preview_text,
    })
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
async fn update_stroke(
    stroke_id: i64,
    stroke: StrokeInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let bounds = stroke_bounds(&stroke.points)?;
    let data = encode_stroke_points(&stroke.points)?;

    sqlx::query(
        "UPDATE strokes SET data = ?, colour = ?, thickness = ?, min_x = ?, min_y = ?, max_x = ?, max_y = ?, chunk_id = ? \
         WHERE id = ?",
    )
    .bind(&data)
    .bind(&stroke.colour)
    .bind(stroke.thickness)
    .bind(bounds.min_x)
    .bind(bounds.min_y)
    .bind(bounds.max_x)
    .bind(bounds.max_y)
    .bind(stroke.chunk_id)
    .bind(stroke_id)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
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
async fn update_surface_stroke(
    stroke_id: i64,
    stroke: SurfaceStrokeInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let bounds = stroke_bounds(&stroke.points)?;
    let data = encode_stroke_points(&stroke.points)?;

    sqlx::query(
        "UPDATE surface_strokes SET data = ?, colour = ?, thickness = ?, min_x = ?, min_y = ?, max_x = ?, max_y = ? \
         WHERE id = ?",
    )
    .bind(&data)
    .bind(&stroke.colour)
    .bind(stroke.thickness)
    .bind(bounds.min_x)
    .bind(bounds.min_y)
    .bind(bounds.max_x)
    .bind(bounds.max_y)
    .bind(stroke_id)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
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

#[tauri::command]
async fn save_surface_graph_object(
    surface_id: i64,
    graph: SurfaceGraphObjectInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<i64, String> {
    let normalized = normalize_surface_graph_input(graph)?;
    let row = sqlx::query(
        "INSERT INTO surface_graph_objects \
         (surface_id, mode, equation, points_json, x_min, x_max, y_min, y_max, bbox_x, bbox_y, bbox_w, bbox_h, line_colour, line_width) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(surface_id)
    .bind(&normalized.mode)
    .bind(normalized.equation.as_deref())
    .bind(normalized.points_json.as_deref())
    .bind(normalized.x_min)
    .bind(normalized.x_max)
    .bind(normalized.y_min)
    .bind(normalized.y_max)
    .bind(normalized.bbox_x)
    .bind(normalized.bbox_y)
    .bind(normalized.bbox_w)
    .bind(normalized.bbox_h)
    .bind(&normalized.line_colour)
    .bind(normalized.line_width)
    .fetch_one(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row.get("id"))
}

#[tauri::command]
async fn load_surface_graph_objects(
    surface_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<Vec<SurfaceGraphObjectOutput>, String> {
    let rows = sqlx::query(
        "SELECT id, surface_id, mode, equation, points_json, x_min, x_max, y_min, y_max, bbox_x, bbox_y, bbox_w, bbox_h, line_colour, line_width \
         FROM surface_graph_objects WHERE surface_id = ? ORDER BY id",
    )
    .bind(surface_id)
    .fetch_all(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SurfaceGraphObjectOutput {
            id: r.get("id"),
            surface_id: r.get("surface_id"),
            mode: r.get("mode"),
            equation: r.get("equation"),
            points_json: r.get("points_json"),
            x_min: r.get("x_min"),
            x_max: r.get("x_max"),
            y_min: r.get("y_min"),
            y_max: r.get("y_max"),
            bbox_x: r.get("bbox_x"),
            bbox_y: r.get("bbox_y"),
            bbox_w: r.get("bbox_w"),
            bbox_h: r.get("bbox_h"),
            line_colour: r.get("line_colour"),
            line_width: r.get("line_width"),
        })
        .collect())
}

#[tauri::command]
async fn update_surface_graph_object(
    graph_id: i64,
    graph: SurfaceGraphObjectInput,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    let normalized = normalize_surface_graph_input(graph)?;
    let result = sqlx::query(
        "UPDATE surface_graph_objects \
         SET mode = ?, equation = ?, points_json = ?, x_min = ?, x_max = ?, y_min = ?, y_max = ?, \
             bbox_x = ?, bbox_y = ?, bbox_w = ?, bbox_h = ?, line_colour = ?, line_width = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?",
    )
    .bind(&normalized.mode)
    .bind(normalized.equation.as_deref())
    .bind(normalized.points_json.as_deref())
    .bind(normalized.x_min)
    .bind(normalized.x_max)
    .bind(normalized.y_min)
    .bind(normalized.y_max)
    .bind(normalized.bbox_x)
    .bind(normalized.bbox_y)
    .bind(normalized.bbox_w)
    .bind(normalized.bbox_h)
    .bind(&normalized.line_colour)
    .bind(normalized.line_width)
    .bind(graph_id)
    .execute(pool.inner())
    .await
    .map_err(|e| e.to_string())?;

    if result.rows_affected() == 0 {
        return Err(format!("surface graph object {} not found", graph_id));
    }

    Ok(())
}

#[tauri::command]
async fn delete_surface_graph_object(
    graph_id: i64,
    pool: tauri::State<'_, SqlitePool>,
) -> Result<(), String> {
    sqlx::query("DELETE FROM surface_graph_objects WHERE id = ?")
        .bind(graph_id)
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
                // Z.AI rejects higher fan-out, so keep transcription globally throttled.
                zai_transcription_semaphore: Arc::new(Semaphore::new(2)),
            });
            info!(target: "gloss_lib::startup", "gloss startup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            import_pdf,
            export_database_file,
            import_database_file,
            list_textbooks,
            delete_source_document,
            save_past_paper_instruction_range,
            inspect_past_paper_instruction_context,
            get_pdf_path,
            get_or_create_page,
            get_or_create_page_surface,
            get_or_create_chunk_surface,
            save_stroke,
            update_stroke,
            load_strokes,
            delete_stroke,
            save_surface_stroke,
            update_surface_stroke,
            load_surface_strokes,
            delete_surface_stroke,
            save_surface_graph_object,
            load_surface_graph_objects,
            update_surface_graph_object,
            delete_surface_graph_object,
            render_pdf_page,
            get_page_count,
            get_chunks_for_page,
            get_chunk_for_transcription,
            save_chunk_glossary,
            save_chunk_title,
            save_chunk_body_markdown,
            get_ai_settings_state,
            save_ai_api_keys,
            generate_chunk_formatted_body,
            transcribe_ai_chat_image,
            rewrite_chunk_text_with_prompt,
            rewrite_chunk_glossary_with_prompt,
            start_chunk_ai_stream,
            start_page_ai_stream,
            cancel_chunk_ai_stream,
            get_chunk_references,
            get_chunk_preview,
            get_question_source_slices,
            get_proof_chunk_for_target,
            save_question_available_marks,
            save_question_achieved_marks,
            list_question_mark_attempts,
            mark_question_answer_with_ai,
            apply_question_mark_attempt,
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

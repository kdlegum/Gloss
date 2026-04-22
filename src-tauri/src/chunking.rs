// Background PDF chunking pipeline.
//
// Flow per document:
//   1. For every page: extract text blocks via pdfium + persist to `text_blocks`.
//   2. Check Ollama health. If unavailable, write one chunk per block (fallback).
//   3. Otherwise, per page: ask the LLM to group blocks + classify, persist to `chunks`,
//      update `text_blocks.chunk_id`.
//
// Progress is reported via Tauri events (`chunking_progress`) so the frontend can
// re-render the overlay as chunks land.

use crate::ollama::{BlockForPrompt, GroupedChunk, OllamaClient};
use crate::PdfiumWorker;
use log::{debug, error, info, warn};
use pdfium_render::prelude::*;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

// ── Data types ──────────────────────────────────────────────────────────────

pub struct RawBlock {
    pub order_idx: i32,
    pub bbox_x: f32,
    pub bbox_y: f32,
    pub bbox_w: f32,
    pub bbox_h: f32,
    pub text: String,
}

pub struct PersistedBlock {
    pub id: i64,
    pub text: String,
    pub bbox_x: f32,
    pub bbox_y: f32,
    pub bbox_w: f32,
    pub bbox_h: f32,
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    source_document_id: i64,
    page_number: i64,
    phase: &'static str, // "extracted" | "grouped"
}

const ALLOWED_TYPES: &[&str] = &[
    "definition",
    "theorem",
    "proof",
    "exercise",
    "example",
    "other",
];

// ── Top-level orchestrator ──────────────────────────────────────────────────

/// Runs the full chunking pipeline for one document. Called from a
/// `tokio::spawn` task in `import_pdf`; never propagates errors — any
/// failures mark `chunking_status='failed'` and log.
pub async fn run_for_document(
    pool: SqlitePool,
    pdfium: PdfiumWorker,
    app: AppHandle,
    doc_id: i64,
) {
    info!(target: "gloss_lib::chunking", "starting chunking job for doc_id={}", doc_id);
    if let Err(e) = run_inner(&pool, &pdfium, &app, doc_id).await {
        error!(
            target: "gloss_lib::chunking",
            "chunking job failed for doc_id={}: {}",
            doc_id,
            e
        );
        let _ = sqlx::query("UPDATE source_documents SET chunking_status = 'failed' WHERE id = ?")
            .bind(doc_id)
            .execute(&pool)
            .await;
    } else {
        info!(target: "gloss_lib::chunking", "finished chunking job for doc_id={}", doc_id);
    }
}

async fn run_inner(
    pool: &SqlitePool,
    pdfium: &PdfiumWorker,
    app: &AppHandle,
    doc_id: i64,
) -> Result<(), String> {
    // Resolve absolute path + page count.
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let row = sqlx::query("SELECT file_path FROM source_documents WHERE id = ?")
        .bind(doc_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let relative_path: String = row.get("file_path");
    let abs_path = data_dir.join(&relative_path);
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} resolved path to {}",
        doc_id,
        abs_path.display()
    );

    set_status(pool, doc_id, "extracting").await?;

    let page_count = {
        let p = abs_path.clone();
        pdfium
            .run(move |pdfium| {
                let doc = pdfium
                    .load_pdf_from_file(&p, None)
                    .map_err(|e| e.to_string())?;
                Ok(doc.pages().len() as usize)
            })
            .await?
    };
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} page_count={}",
        doc_id,
        page_count
    );

    // Phase 1: extract + persist blocks for every page.
    for page_num in 0..page_count {
        let page_number = page_num as i64 + 1;
        let page_id = get_or_create_page(pool, doc_id, page_num as i64 + 1).await?;

        // Skip if this page already has extracted blocks (resume support).
        let existing: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM text_blocks WHERE page_id = ?")
                .bind(page_id)
                .fetch_one(pool)
                .await
                .map_err(|e| e.to_string())?;
        if existing > 0 {
            info!(
                target: "gloss_lib::chunking",
                "doc_id={} page={} skipping extraction; {} text blocks already exist",
                doc_id,
                page_number,
                existing
            );
            continue;
        }

        let p = abs_path.clone();
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} extracting text blocks",
            doc_id,
            page_number
        );
        let blocks = pdfium
            .run(move |pdfium| extract_blocks_from_page(pdfium, &p, page_num))
            .await
            .unwrap_or_else(|e| {
                warn!(
                    target: "gloss_lib::chunking",
                    "doc_id={} page={} extraction failed: {}",
                    doc_id,
                    page_number,
                    e
                );
                Vec::new()
            });

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

    // Phase 2: grouping. Try Ollama once; if down, use fallback for every page.
    set_status(pool, doc_id, "grouping").await?;
    let ollama = OllamaClient::new();
    let use_llm = ollama.health_check().await;
    info!(
        target: "gloss_lib::chunking",
        "doc_id={} ollama_available={}",
        doc_id,
        use_llm
    );

    for page_num in 0..page_count {
        let page_number = page_num as i64 + 1;
        let page_id = get_or_create_page(pool, doc_id, page_num as i64 + 1).await?;

        // Skip if this page already has chunks.
        let existing_chunks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE page_id = ?")
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
            continue;
        }

        let blocks = load_unchunked_blocks(pool, page_id).await?;
        if blocks.is_empty() {
            warn!(
                target: "gloss_lib::chunking",
                "doc_id={} page={} has no unchunked blocks to group",
                doc_id,
                page_number
            );
            continue;
        }
        info!(
            target: "gloss_lib::chunking",
            "doc_id={} page={} grouping {} blocks",
            doc_id,
            page_number,
            blocks.len()
        );

        let used_llm = if use_llm {
            match try_llm_chunking(&ollama, &blocks).await {
                Ok(groups) if validate_groups(&groups, &blocks) => {
                    persist_chunks(pool, doc_id, page_id, &blocks, &groups, true).await?;
                    info!(
                        target: "gloss_lib::chunking",
                        "doc_id={} page={} persisted {} LLM chunk groups",
                        doc_id,
                        page_number,
                        groups.len()
                    );
                    true
                }
                Ok(groups) => {
                    warn!(
                        target: "gloss_lib::chunking",
                        "doc_id={} page={} received invalid LLM grouping ({} groups); using fallback",
                        doc_id,
                        page_number,
                        groups.len()
                    );
                    false
                }
                Err(err) => {
                    warn!(
                        target: "gloss_lib::chunking",
                        "doc_id={} page={} LLM grouping failed: {}; using fallback",
                        doc_id,
                        page_number,
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
    }

    set_status(pool, doc_id, "done").await?;
    Ok(())
}

async fn try_llm_chunking(
    ollama: &OllamaClient,
    blocks: &[PersistedBlock],
) -> Result<Vec<GroupedChunk>, String> {
    let prompt_blocks: Vec<BlockForPrompt> = blocks
        .iter()
        .map(|b| BlockForPrompt {
            id: b.id,
            text: &b.text,
        })
        .collect();

    ollama
        .chunk_blocks(&prompt_blocks)
        .await
        .map_err(|e| e.to_string())
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
        "SELECT id, text, bbox_x, bbox_y, bbox_w, bbox_h \
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
    let by_id: std::collections::HashMap<i64, &PersistedBlock> =
        blocks.iter().map(|b| (b.id, b)).collect();

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
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
             (source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h, ocr_text, ai_suggested) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(doc_id)
        .bind(page_id)
        .bind(chunk_type)
        .bind(bbox.0)
        .bind(bbox.1)
        .bind(bbox.2)
        .bind(bbox.3)
        .bind(text)
        .bind(if ai_suggested { 1 } else { 0 })
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let chunk_id: i64 = row.get("id");

        for id in &g.block_ids {
            sqlx::query("UPDATE text_blocks SET chunk_id = ? WHERE id = ?")
                .bind(chunk_id)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
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
             VALUES (?, ?, 'other', ?, ?, ?, ?, ?, 0) RETURNING id",
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
    for t in ALLOWED_TYPES {
        if lower == *t {
            return t;
        }
    }
    "other"
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
            "page={} extraction produced no text segments",
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

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Gloss

A maths notes app for desktop and Android tablet. The core idea: **the structure of a PDF becomes the filing system**. Academic documents have natural semantic units — definitions, theorems, proofs, exercises — and Gloss surfaces these as first-class objects called **chunks**. Your work (ink, typed notes, scrap working) attaches to a chunk, not just to a page.

This is different from GoodNotes (which lets you write *on* a PDF). In Gloss you write *about* a specific chunk, anchored to it.

Key features (many not yet built):
- **Chunk system** — definitions, theorems, exercises, proofs are containers for your work. AI pre-suggests chunk boundaries, user confirms/adjusts.
- **In-app AI chat** — chunk-aware: knows which chunk you're in, your attached work, and pulls in related chunks from across all sources via RAG over AI-generated semantic tags. Handles maths synonym/notation variation.
- **Cross-source linking** — an exercise in a textbook can link to a related worked example in lecture notes.
- **Progress tracking** — complete/incomplete status across all exercises in a textbook.
- **Personal glossary** — self-explanations in your own words, one type of work attachable to a chunk. Doubles as a learner model for the AI.

The notes app is the primary product — fast, pleasant, works well without AI. AI features are layered on after core note-taking is solid.

Target platforms: desktop and Android tablet (via Tauri's Android support). Cross-device sync via Syncthing.

LLM stack: Claude Sonnet (primary, via Anthropic API), Mathstral 7B via Ollama (offline fallback), OpenAI text-embedding-3-small (embeddings).

## Commands

```bash
# Run the full Tauri app (starts Vite dev server + Rust backend)
npm run tauri dev

# Frontend only (no Rust, no Tauri)
npm run dev

# Type-check frontend
npm run check

# Build for release
npm run tauri build

# Compile Rust only (fast check without linking)
cargo check --manifest-path src-tauri/Cargo.toml
```

No test suite exists yet.

## Architecture

### Frontend — `src/`
- Single-page SvelteKit app using `@sveltejs/adapter-static` (required for Tauri).
- Svelte 5 runes (`$state`, `$derived`) throughout — not the legacy options API.
- All Tauri backend calls go through `invoke()` from `@tauri-apps/api/core`.
- PDF rendering uses `pdfjs-dist` directly on a `<canvas>` element. The worker is pointed at the bundled `pdf.worker.min.mjs` via `import.meta.url`.
- Currently a single route: `src/routes/+page.svelte` holds both the library view and the PDF viewer. As features grow, this will need splitting.
- Last-read page is persisted per book in `localStorage` (key: `gloss_page_<id>`).

### Backend — `src-tauri/src/lib.rs`
- All backend logic lives in `lib.rs`; `main.rs` just calls `run()`.
- SQLite pool (`sqlx` with `runtime-tokio`) is initialised at startup and stored as Tauri managed state (`app.manage(pool)`).
- Migrations are in `src-tauri/migrations/` and run automatically via `sqlx::migrate!` on startup.
- PDFs are copied from wherever the user picks them into the app data directory under `pdfs/`. Only the relative path (`pdfs/<filename>`) is stored in the DB. The `get_pdf_path` command reconstructs the absolute path at runtime.
- New Tauri commands must be added to the `invoke_handler!` macro in `run()` and to the Tauri capability files under `src-tauri/capabilities/` if they need specific permissions.

### Database — `src-tauri/migrations/`
Current schema (migration `0001_initial.sql`):
- `textbooks(id, title, file_path, file_hash, page_count)` — `file_hash` and `page_count` columns exist but are not yet populated.
- `pages(id, textbook_id, page_number)` — one row per page visited or indexed; unique on `(textbook_id, page_number)`. One-indexed.

Future tables to add as migrations: glossary entries, self-explanations, chunks, embeddings.

### Data locations (runtime)
- DB: `%APPDATA%/com.gloss.app/gloss.db` (Windows) — via `app.path().app_data_dir()`
- PDFs: `%APPDATA%/com.gloss.app/pdfs/`

## Key conventions

- Rust errors are returned as `Result<T, String>` from commands — `map_err(|e| e.to_string())` is the pattern in use.
- The string `"cancelled"` is the sentinel for a user-cancelled file picker; the frontend silently ignores it.
- PDF file names are deduplicated by appending `_1`, `_2`, … if a file with the same name already exists.
- `+layout.ts` sets `export const prerender = true` (required for static adapter).

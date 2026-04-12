# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Gloss

A personal maths notes app built with Tauri v2 + SvelteKit (frontend) and Rust (backend). Core workflow: import PDF textbooks → read page-by-page → ask an LLM questions → write self-explanations → build a personal glossary. Target platforms: desktop and Android tablet (via Tauri's Android support). Cross-device sync via Syncthing.

Planned features (not yet built): per-keyword hover showing past self-explanations, learner profile in system prompt, RAG over the glossary, textbook chunking by natural maths structure (definition → theorem → proof → examples), embeddings with LLM-generated semantic tags.

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

# CLAUDE.md

This file provides guidance to Claude Code (`claude.ai/code`) when working with code in this repository.

## What is Gloss

A maths notes app for desktop and Android tablet. The core idea: **the structure of a PDF becomes the filing system**. Academic documents have natural semantic units - definitions, theorems, proofs, exercises - and Gloss surfaces these as first-class objects called **chunks**. Your work (ink, typed notes, scrap working) attaches to a chunk, not just to a page.

This is different from GoodNotes (which lets you write *on* a PDF). In Gloss you write *about* a specific chunk, anchored to it.

Key features (many not yet built):
- **Chunk system** - definitions, theorems, exercises, proofs are containers for your work. AI pre-suggests chunk boundaries, user confirms or adjusts them.
- **In-app AI chat** - chunk-aware: knows which chunk you're in, your attached work, and can pull in related chunks from across all sources via RAG over AI-generated semantic tags. It should handle maths synonym and notation variation.
- **Cross-source linking** - an exercise in a textbook can link to a related worked example in lecture notes.
- **Progress tracking** - complete/incomplete status across all exercises in a textbook.
- **Personal glossary** - self-explanations in your own words, one type of work attachable to a chunk. Doubles as a learner model for the AI.

The notes app is the primary product - fast, pleasant, and useful without AI. AI features are layered on after core note-taking is solid.

Target platforms: desktop and Android tablet (via Tauri's Android support). Cross-device sync via Syncthing.

LLM stack: Claude Sonnet (primary, via Anthropic API), Mathstral 7B via Ollama (offline fallback), OpenAI `text-embedding-3-small` (embeddings).

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

# Run Rust unit tests
cargo test --manifest-path src-tauri/Cargo.toml
```

There is no broad automated app test suite yet. Treat `npm run check` plus manual Tauri verification as the baseline, with `cargo test --manifest-path src-tauri/Cargo.toml` covering the current Rust unit tests.

## Architecture

### Frontend - `src/`
- Single-page SvelteKit app using `@sveltejs/adapter-static` (required for Tauri).
- Svelte 5 runes (`$state`, `$derived`) throughout - not the legacy options API.
- All Tauri backend calls go through `invoke()` from `@tauri-apps/api/core`.
- PDF rendering uses `pdfjs-dist` directly on a `<canvas>` element. The worker is pointed at the bundled `pdf.worker.min.mjs` via `import.meta.url`.
- `src/routes/+page.svelte` is now a thin app-level coordinator. It owns document list/selection state, top-level AI settings state, and decides whether to render the library or the reader workspace.
- `src/lib/screens/LibraryScreen.svelte` renders the library/import/export/delete/settings UI. Keep it presentational: callbacks come from `+page.svelte`; backend `invoke()` calls stay outside it.
- `src/lib/screens/ReaderWorkspace.svelte` owns the document viewer workspace: PDF rendering, page/chunk loading, page ink, graphs, viewer AI, batch chunking, and chunk open/close flow. This is currently the largest frontend module and still contains the chunk sheet internals.
- `src/lib/screens/AiSettingsSheet.svelte` contains the global AI provider/model/API-key settings sheet.
- `src/lib/viewer/` contains smaller viewer leaf components extracted out of the workspace (`ViewerHeader`, `BatchChunkProgressCard`, `ViewerAiPanel`).
- Shared frontend helpers are grouped under `src/lib/app/` (`ai.ts`, `log.ts`, `types.ts`) and `src/lib/chunk/` (`colours.ts`).
- Older shared modules such as `src/lib/ink.ts`, `src/lib/graph.ts`, and `src/lib/chunkBody.ts` still carry most of the reusable rendering/data helpers.
- Last-read page is persisted per book in `localStorage` (key: `gloss_page_<id>`).

### Backend - `src-tauri/src/lib.rs`
- All backend logic lives in `lib.rs`; `main.rs` just calls `run()`.
- SQLite pool (`sqlx` with `runtime-tokio`) is initialised at startup and stored as Tauri managed state (`app.manage(pool)`).
- Migrations are in `src-tauri/migrations/` and run automatically via `sqlx::migrate!` on startup.
- PDFs are copied from wherever the user picks them into the app data directory under `pdfs/`. Only the relative path (`pdfs/<filename>`) is stored in the DB. The `get_pdf_path` command reconstructs the absolute path at runtime.
- New Tauri commands must be added to the `invoke_handler!` macro in `run()` and to the Tauri capability files under `src-tauri/capabilities/` if they need specific permissions.

### Database - `src-tauri/migrations/`
Current schema (migration `0001_initial.sql`):
- `textbooks(id, title, file_path, file_hash, page_count)` - `file_hash` and `page_count` columns exist but are not yet populated.
- `pages(id, textbook_id, page_number)` - one row per page visited or indexed; unique on `(textbook_id, page_number)`. One-indexed.

Future tables to add as migrations: glossary entries, self-explanations, chunks, embeddings.

### Data locations (runtime)
- DB: `%APPDATA%/com.gloss.app/gloss.db` (Windows) - via `app.path().app_data_dir()`
- PDFs: `%APPDATA%/com.gloss.app/pdfs/`

## Design system

The visual language comes from a Figma-style handoff (`Chunk selection and accessing UI-handoff.zip`). Key decisions to honour:

**Chunk type colours** - each chunk type has an `accent` (oklch, saturated, mid-lightness) and a `tint` (oklch, desaturated, very light). Both are stored in `src/lib/chunk/colours.ts` (`CHUNK_COLOURS`) and must stay in sync with the canvas overlay renderer and chunk UI.

| Type        | Accent hue | Use |
|-------------|------------|-----|
| definition  | 233 (blue) | primary semantic unit |
| theorem     | 290 (purple) | proved result |
| proof       | 180 (teal) | derivation |
| exercise    | 50 (orange) | practice problem |
| example     | 50 (orange) | worked example |
| explanation | 240 (grey-blue) | prose/connective tissue |

**PDF overlay - marginal bar style** - chunk boundaries are shown as a 3 px coloured bar on the left edge of the chunk bbox, plus a very subtle tint fill over the full bbox. Do not use full-rect fills or bordered boxes; keep it non-intrusive.

**Chunk note sheet - split panel (Layout A)** - when the user taps a chunk the note sheet opens as a side-by-side panel:
- Left (240 px fixed): chunk badge, status dot, close button, title (italic serif), OCR body text. Background `#f9fafb`, bordered on the left with the chunk accent colour.
- Right (flex): tab bar across the top with **Ink**, **Glossary**, **AI** tabs, then the tab content below.

The Ink tab contains the drawing canvas with a floating toolbar at the bottom (pen/eraser).

**Typography** - UI chrome: Inter / system-ui. Chunk body text (OCR content, glossary): Georgia / serif.

**Status dots** - `complete` = `oklch(0.62 0.14 145)` (green), `in_progress` = `oklch(0.72 0.13 65)` (amber), `incomplete` = hollow border circle.

## Key conventions

- Rust errors are returned as `Result<T, String>` from commands - `map_err(|e| e.to_string())` is the pattern in use.
- The string `"cancelled"` is the sentinel for a user-cancelled file picker; the frontend silently ignores it.
- PDF file names are deduplicated by appending `_1`, `_2`, and so on if a file with the same name already exists.
- `+layout.ts` sets `export const prerender = true` (required for the static adapter).

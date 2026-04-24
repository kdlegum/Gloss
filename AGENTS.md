# Repository Guidelines

## Project Overview
Gloss is a maths notes app for desktop and Android tablet. Its core model is that the structure of a PDF becomes the filing system: definitions, theorems, proofs, exercises, and similar semantic units become first-class "chunks" that notes attach to, rather than treating annotation as page-only markup.

The current product focus is a fast, pleasant note-taking app that works well without AI. Planned AI features layer on top of that core, including chunk-aware chat, cross-source linking, progress tracking, and a personal glossary. For fuller product and architecture context, see [CLAUDE.md](/C:/Users/kdleg/OneDrive/Desktop/Gloss/CLAUDE.md).

## Project Structure & Module Organization
`src/` contains the SvelteKit frontend. The current UI entry points live in `src/routes/`, with `+page.svelte` holding the main library and viewer flow and `+layout.ts` configuring static prerendering. `static/` stores bundled assets such as icons and standard fonts. `src-tauri/` contains the Rust desktop/mobile shell, SQLite migrations in `src-tauri/migrations/`, app capabilities, and generated Android project files under `src-tauri/gen/android/`. Project notes and setup docs live in `docs/`.

## Build, Test, and Development Commands
Run `npm install` once to install the frontend and Tauri CLI dependencies. Use `npm run dev` for the Svelte-only dev server and `npm run build` to produce the frontend bundle. Run `npm run check` for the main TypeScript and Svelte validation pass; `npm run check:watch` keeps that running during UI work. Use `npm run tauri dev` to launch the full Tauri app locally. For Android work, follow [docs/android-dev-setup.md](/C:/Users/kdleg/OneDrive/Desktop/Gloss/docs/android-dev-setup.md) and then run `npm run tauri android dev`.

## Coding Style & Naming Conventions
Follow the existing style: 2-space indentation in `*.svelte`, `*.ts`, and `*.js`, and Rust’s standard 4-space indentation in `src-tauri/src/`. Prefer TypeScript interfaces for structured frontend data and keep Svelte route files named with Kit conventions such as `+page.svelte` and `+layout.ts`. Use `camelCase` for variables and functions, `PascalCase` for TypeScript interfaces/types, and `snake_case` for Rust-backed payload fields that map to SQLite columns.

## Testing Guidelines
There is no dedicated automated test suite yet. Treat `npm run check` as the required pre-PR validation step, then manually verify core flows in the Tauri app: PDF import, page navigation, drawing, erase/select actions, and persistence after restart. When changing database behavior, confirm the relevant migration applies cleanly on a fresh app data directory.

## Commit & Pull Request Guidelines
Recent commits use short, imperative subjects such as `Change pdf framework to pdfium` and `Persist pen data by saving strokes as blobs`. Keep commit titles concise, descriptive, and focused on one change. Pull requests should explain the user-visible impact, list validation steps you ran, and include screenshots or screen recordings for UI changes. Call out migration, Android, or bundled-library changes explicitly so reviewers can test the right path.

## UI Design Direction

The visual language was established in a design handoff for the "Chunk selection and accessing UI" feature. All future chunk-related UI should follow these decisions.

**Chunk colour system** — `CHUNK_COLOURS` in `src/routes/+page.svelte` maps each chunk type to `{ accent, tint, label, short }`. The `accent` is an oklch colour used for borders, active indicators, and text. The `tint` is the matching pale background. Always use these tokens — never hardcode hex values for chunk colours.

**PDF overlay** — Chunk boundaries use a *marginal bar* treatment: a 3 px vertical bar at the left edge of the bbox using `accent` at ~0.65–0.95 opacity, plus a very subtle tint fill (0.06–0.14 opacity) over the full bbox. The active chunk has a brighter bar and higher tint. Do not use full-rect fills, outline boxes, or coloured backgrounds that obscure the PDF content.

**Chunk note sheet** — opens as a centred modal (`min(920px, 100%) × min(680px, 100%)`), split horizontally:
- **Left panel** (240 px): `background: #f9fafb`, `border-left: 3px solid var(--chunk-accent)`. Contains: badge (type short-code, uppercase, rounded pill with tint bg + accent text), status dot, close button, italic serif title, and the chunk's `ocr_text` in body serif. CSS classes: `.chunk-panel-left`, `.cpl-meta`, `.chunk-badge`, `.chunk-status-dot`, `.cpl-title`, `.cpl-body`.
- **Right panel** (flex 1): tab bar at top, content below. CSS classes: `.chunk-panel-right`, `.chunk-tab-bar`, `.chunk-tab`, `.chunk-tab-todo`.

**Tabs** — Ink (active, pen/erase tools in a floating bar at the bottom of the canvas), Glossary (TODO), AI (TODO). TODO tabs have `pointer-events: none; opacity: 0.4` — include the UI but do not wire up any logic.

**Status dots** — three states mapped to CSS classes on `.chunk-status-dot`:
- `.status-complete` → `oklch(0.62 0.14 145)` solid fill
- `.status-in_progress` → `oklch(0.72 0.13 65)` solid fill
- `.status-incomplete` → transparent fill, `1.5px solid #9ca3af` border

**Typography** — system UI (`Inter, system-ui, sans-serif`) for chrome; `Georgia, 'Times New Roman', serif` for all maths/body content (chunk text, glossary entries).

**Mobile breakpoint** — at `≤ 720px` the split panel stacks vertically: left panel becomes a horizontal strip (max-height 180 px) above the right panel.

## Configuration Notes
Do not commit local SDK/NDK paths, device-specific settings, or bundled `libpdfium` binaries outside the expected Android `jniLibs` location. Keep generated assets and large binaries minimal, and prefer documenting setup in `docs/` when new platform prerequisites are introduced.

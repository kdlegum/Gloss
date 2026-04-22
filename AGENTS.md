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

## Configuration Notes
Do not commit local SDK/NDK paths, device-specific settings, or bundled `libpdfium` binaries outside the expected Android `jniLibs` location. Keep generated assets and large binaries minimal, and prefer documenting setup in `docs/` when new platform prerequisites are introduced.

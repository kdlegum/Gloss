# AGENTS.md

## Primary reference

Read `CLAUDE.md` first — it has architecture, commands, conventions, and the DB schema.

## Commands (verify before running)

- `npm run tauri dev` — full app (Vite + Rust backend)
- `npm run dev` — frontend only, no Rust
- `npm run check` — type-check frontend (runs `svelte-kit sync` then `svelte-check`)
- `cargo check --manifest-path src-tauri/Cargo.toml` — fast Rust compile check
- `npm run tauri build` — release build
- No test suite exists. No lint command configured.

## Things you'll get wrong without this

- **Svelte 5 runes only.** Never use the legacy options API (`export let`, `$:`, `on:click`). Use `$state`, `$derived`, `$effect`, and event attributes (`onclick=`).
- **Static adapter is required.** `+layout.ts` must export `const prerender = true`. Tauri has no Node server for SSR.
- **Rust commands are `Result<T, String>`.** Use `map_err(|e| e.to_string())`. The string `"cancelled"` means the user dismissed a dialog — frontend ignores it silently.
- **New Tauri commands need two changes:** add to the `invoke_handler!` macro in `src-tauri/src/lib.rs:run()` AND to `src-tauri/capabilities/default.json` if the command needs permissions.
- **Migrations auto-run on startup** via `sqlx::migrate!`. Create numbered files in `src-tauri/migrations/` (e.g. `0003_new_table.sql`). They are applied in order and must be immutable after commit.
- **PDF filenames are deduplicated** at import time (`file_1.pdf`, `file_2.pdf`, ...). Only the relative path `pdfs/<name>` is stored in DB; `get_pdf_path` reconstructs the absolute path at runtime.
- **Dev server port is 1420**, hard-coded in both `vite.config.js` and `tauri.conf.json`. Don't change one without the other.

## File layout

```
src/                        # SvelteKit frontend (single route for now)
src/routes/+page.svelte     # Library view + PDF viewer (will need splitting)
src-tauri/src/lib.rs        # All backend logic; main.rs just calls run()
src-tauri/migrations/       # SQL migrations, run automatically on startup
src-tauri/capabilities/     # Tauri permission manifests
```

## Runtime data locations

- DB: `%APPDATA%/com.gloss.app/gloss.db` (Windows)
- PDFs: `%APPDATA%/com.gloss.app/pdfs/`

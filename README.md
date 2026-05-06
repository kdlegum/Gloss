# Gloss

A maths notes app for desktop and Android tablet. Import a PDF — a textbook, lecture notes, a past paper — and Gloss breaks it into its natural semantic units: definitions, theorems, proofs, exercises, examples. These units are called **chunks**. Your work attaches to a chunk, not to a page number.

The core bet: the structure of an academic document is already a filing system. Gloss surfaces it rather than asking you to recreate it.

---

## The idea

Most PDF annotation tools let you write *on* a PDF. Gloss is different — you write *about* a specific mathematical object. An exercise, a theorem, a proof. When you open a chunk you get its text, your ink, your notes, and an AI chat window that knows the full context: what the chunk says, what you've written about it, and what related chunks exist across all your sources.

This matters for maths in particular. A definition in Chapter 2 connects to a theorem in Chapter 5 and an exercise in a problem sheet. Gloss is designed to make those links explicit over time.

---

## Status

Gloss is early-stage personal software — built by one person, used by that person. The core reading and chunking loop works. Annotation and AI features are in active development.

**Working now:**
- Import PDFs; they're stored locally under app data
- View PDFs with a smooth per-page renderer
- AI-powered chunk detection — the app sends each page to a vision LLM, which identifies and classifies semantic blocks (definition, theorem, proof, exercise, example, explanation)
- Chunk overlay: coloured marginal bars on the left edge of each chunk, non-intrusive tint fill
- Open a chunk into a split panel: OCR text on the left, Ink / Glossary / AI tabs on the right
- Ink drawing on chunks (pen and eraser)
- Graph composer for structured diagrams
- Typst rendering for mathematical notation in notes
- AI chat panel per chunk, with context from the chunk's content and your annotations
- Multiple AI providers: Claude (Anthropic), Gemini, DeepSeek, OpenAI, Ollama (offline/local)
- Batch chunking with live progress across all pages of a document
- References system for cross-chunk linking
- RAG over chunks — AI chat that can pull in related chunks from across all your sources, handling maths synonym and notation variation via semantic tags and embeddings
- Android tablet support (Tauri's Android target is already wired up)

**Planned:**
- Progress tracking — complete / in-progress / incomplete status across all exercises in a document, with an overview per textbook
- Personal glossary — self-explanations in your own words, attached to a chunk, feeding back into the AI as a learner model
- Cross-source linking — an exercise can link to a worked example in different notes
- Cross-device sync via Syncthing

---

## Tech stack

| Layer | Choice |
|---|---|
| Framework | [Tauri](https://tauri.app) — Rust backend, Svelte 5 frontend |
| PDF rendering | pdfium (via `pdfium_render`) on a dedicated thread |

PDF text extraction from pdfium is unreliable for mathematical content — rendered formulae and special characters come back garbled. The chunking pipeline uses a vision LLM to read page images directly, which produces clean text for maths.

---

## Getting started

Prerequisites: [Rust](https://rustup.rs), [Node.js](https://nodejs.org), and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

**pdfium** — the app requires the pdfium shared library at runtime. Download the prebuilt binary for your platform from [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries/releases/latest) and place it next to the compiled executable:

| Platform | Archive | File to extract |
|---|---|---|
| Windows | `pdfium-win-x64.tgz` | `lib/pdfium.dll` → `src-tauri/pdfium.dll` |
| macOS | `pdfium-mac-x64.tgz` | `lib/libpdfium.dylib` → `src-tauri/libpdfium.dylib` |
| Linux | `pdfium-linux-x64.tgz` | `lib/libpdfium.so` → `src-tauri/libpdfium.so` |

For development you also need to copy the file to `src-tauri/target/debug/` so it sits next to the dev binary. CI handles this automatically.

```bash
# Install frontend dependencies
npm install

# Run in development (starts Vite dev server + Rust backend)
npm run tauri dev

# Type-check the frontend
npm run check

# Check the Rust backend (fast, no link step)
cargo check --manifest-path src-tauri/Cargo.toml

# Run Rust unit tests
cargo test --manifest-path src-tauri/Cargo.toml

# Build for release
npm run tauri build
```

To use the AI features, open Settings in the app and add an API key for your preferred provider (or it will just read it from your system env). Ollama works offline if you have a local model running.


## Data

All data is local. PDFs are copied into the app data directory (`%APPDATA%/com.gloss.app/pdfs/` on Windows). The database lives at `%APPDATA%/com.gloss.app/gloss.db`. Nothing leaves your machine except API calls to whichever LLM provider you configure.

Cross-device sync is out of scope for the app itself — Syncthing handles it at the filesystem level.

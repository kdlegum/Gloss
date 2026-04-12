<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { convertFileSrc } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";
  import * as pdfjsLib from "pdfjs-dist";

  // Point the worker at the bundled worker file
  pdfjsLib.GlobalWorkerOptions.workerSrc = new URL(
    "pdfjs-dist/build/pdf.worker.min.mjs",
    import.meta.url
  ).href;

  interface Textbook {
    id: number;
    title: string;
    file_path: string;
  }

  let textbooks = $state<Textbook[]>([]);
  let importing = $state(false);
  let error = $state<string | null>(null);

  // PDF viewer state
  let selectedBook = $state<Textbook | null>(null);
  let pdfDoc = $state<pdfjsLib.PDFDocumentProxy | null>(null);
  let currentPage = $state(1);
  let totalPages = $state(0);
  let rendering = $state(false);
  let canvas = $state<HTMLCanvasElement>(null!);
  let drawCanvas = $state<HTMLCanvasElement>(null!);

  // ── Drawing state ──
  type Point = { x: number; y: number; pressure: number };
  let isDrawing = $state(false);
  let currentStroke = $state<Point[]>([]);
  // How many points from currentStroke have already been painted onto the canvas
  let drawnUpTo = 0;
  // Active pointer id for palm rejection (first pen/stylus wins)
  let activePointerId: number | null = null;

  function getCanvasPoint(e: PointerEvent): Point {
    const rect = drawCanvas.getBoundingClientRect();
    // Scale from CSS pixels to canvas logical pixels
    const scaleX = drawCanvas.width / rect.width;
    const scaleY = drawCanvas.height / rect.height;
    return {
      x: (e.clientX - rect.left) * scaleX,
      y: (e.clientY - rect.top) * scaleY,
      pressure: e.pressure > 0 ? e.pressure : 0.5,
    };
  }

  function isPenOrMouse(e: PointerEvent): boolean {
    return e.pointerType === "pen" || e.pointerType === "mouse";
  }

  function onPointerDown(e: PointerEvent) {
    // Palm rejection: ignore touch; ignore second concurrent pointer
    if (!isPenOrMouse(e)) return;
    if (activePointerId !== null) return;

    activePointerId = e.pointerId;
    drawCanvas.setPointerCapture(e.pointerId);
    isDrawing = true;
    currentStroke = [getCanvasPoint(e)];
    drawnUpTo = 1;
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (!isDrawing || e.pointerId !== activePointerId) return;
    e.preventDefault();

    // Use getCoalescedEvents when available for smoother lines on high-freq devices
    const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
    for (const ce of events) {
      currentStroke = [...currentStroke, getCanvasPoint(ce)];
    }

    redrawStroke();
  }

  function onPointerUp(e: PointerEvent) {
    if (e.pointerId !== activePointerId) return;
    if (isDrawing) {
      redrawStroke(); // finalise
    }
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
  }

  function onPointerCancel(e: PointerEvent) {
    if (e.pointerId !== activePointerId) return;
    // Discard the stroke (e.g. palm was detected mid-stroke)
    const ctx = drawCanvas.getContext("2d")!;
    ctx.clearRect(0, 0, drawCanvas.width, drawCanvas.height);
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    drawnUpTo = 0;
  }

  function redrawStroke() {
    if (!drawCanvas || currentStroke.length < 2) return;
    const ctx = drawCanvas.getContext("2d")!;
    const pts = currentStroke;

    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "rgba(30, 80, 220, 0.85)";

    // Draw all segments that haven't been painted yet (covers coalesced events)
    ctx.beginPath();
    ctx.moveTo(pts[drawnUpTo - 1].x, pts[drawnUpTo - 1].y);
    for (let i = drawnUpTo; i < pts.length; i++) {
      const width = 1 + pts[i].pressure * 5;
      // Flush the current sub-path before changing lineWidth
      if (ctx.lineWidth !== width) {
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(pts[i - 1].x, pts[i - 1].y);
        ctx.lineWidth = width;
      }
      ctx.lineTo(pts[i].x, pts[i].y);
    }
    ctx.stroke();
    drawnUpTo = pts.length;
  }

  // Sync draw canvas logical resolution to its CSS size (the full canvas-wrap area)
  function syncDrawCanvasSize() {
    if (!drawCanvas) return;
    const w = drawCanvas.clientWidth;
    const h = drawCanvas.clientHeight;
    if (drawCanvas.width !== w || drawCanvas.height !== h) {
      // Resizing clears the canvas — intentional for now (no persistence yet)
      drawCanvas.width = w;
      drawCanvas.height = h;
    }
  }

  // Zoom state (1.0 = auto-fit width, multiplier on top of that)
  let zoomLevel = $state(1.0);
  const ZOOM_STEP = 0.15;
  const ZOOM_MIN = 0.3;
  const ZOOM_MAX = 4.0;

  // Persist last page per book
  function savedPageKey(bookId: number) {
    return `gloss_page_${bookId}`;
  }

  function saveCurrentPage(bookId: number, page: number) {
    localStorage.setItem(savedPageKey(bookId), String(page));
  }

  function loadSavedPage(bookId: number): number {
    const v = localStorage.getItem(savedPageKey(bookId));
    return v ? Math.max(1, parseInt(v, 10)) : 1;
  }

  async function loadTextbooks() {
    textbooks = await invoke<Textbook[]>("list_textbooks");
  }

  async function importPdf() {
    error = null;
    importing = true;
    try {
      await invoke<Textbook>("import_pdf");
      await loadTextbooks();
    } catch (e: unknown) {
      if (e !== "cancelled") {
        error = String(e);
      }
    } finally {
      importing = false;
    }
  }

  async function openBook(book: Textbook) {
    error = null;
    selectedBook = book;
    currentPage = loadSavedPage(book.id);
    zoomLevel = 1.0;
    pdfDoc = null;
    totalPages = 0;

    try {
      const absPath = await invoke<string>("get_pdf_path", {
        relativePath: book.file_path,
      });
      const url = convertFileSrc(absPath);
      const doc = await pdfjsLib.getDocument({ url, isOffscreenCanvasSupported: false, isImageDecoderSupported: false }).promise;
      pdfDoc = doc;
      totalPages = doc.numPages;
      // Clamp saved page in case book changed
      if (currentPage > totalPages) currentPage = 1;
      await renderPage(currentPage);
    } catch (e) {
      error = String(e);
    }
  }

  async function renderPage(pageNum: number) {
    if (!pdfDoc || !canvas) return;
    rendering = true;
    try {
      const page = await pdfDoc.getPage(pageNum);
      const fitScale = Math.min(
        (canvas.parentElement?.clientWidth ?? 600) / page.getViewport({ scale: 1 }).width,
        1.8
      );
      const scale = fitScale * zoomLevel;
      const viewport = page.getViewport({ scale });
      canvas.width = viewport.width;
      canvas.height = viewport.height;
      const ctx = canvas.getContext("2d")!;
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      await page.render({ canvasContext: ctx, viewport }).promise;
      syncDrawCanvasSize();
    } finally {
      rendering = false;
    }
  }

  async function goToPage(pageNum: number) {
    if (rendering) return;
    const clamped = Math.max(1, Math.min(totalPages, pageNum));
    if (clamped === currentPage) return;
    currentPage = clamped;
    if (selectedBook) saveCurrentPage(selectedBook.id, currentPage);
    await renderPage(currentPage);
  }

  async function prevPage() {
    await goToPage(currentPage - 1);
  }

  async function nextPage() {
    await goToPage(currentPage + 1);
  }

  async function zoomIn() {
    zoomLevel = Math.min(ZOOM_MAX, +(zoomLevel + ZOOM_STEP).toFixed(2));
    await renderPage(currentPage);
  }

  async function zoomOut() {
    zoomLevel = Math.max(ZOOM_MIN, +(zoomLevel - ZOOM_STEP).toFixed(2));
    await renderPage(currentPage);
  }

  async function resetZoom() {
    zoomLevel = 1.0;
    await renderPage(currentPage);
  }

  function closeViewer() {
    selectedBook = null;
    pdfDoc = null;
    currentPage = 1;
    totalPages = 0;
    zoomLevel = 1.0;
  }

  // ── Keyboard navigation ──
  async function handleKeydown(e: KeyboardEvent) {
    if (!selectedBook || !pdfDoc) return;

    // Don't steal focus from inputs
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

    if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      e.preventDefault();
      await prevPage();
    } else if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      e.preventDefault();
      await nextPage();
    } else if (e.key === "+" || e.key === "=") {
      e.preventDefault();
      await zoomIn();
    } else if (e.key === "-") {
      e.preventDefault();
      await zoomOut();
    } else if (e.key === "0" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      await resetZoom();
    }
  }

  // ── Ctrl+scroll zoom ──
  async function handleWheel(e: WheelEvent) {
    if (!selectedBook || !(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    if (e.deltaY < 0) {
      await zoomIn();
    } else {
      await zoomOut();
    }
  }

  onMount(() => {
    loadTextbooks();
    window.addEventListener("keydown", handleKeydown);
    window.addEventListener("wheel", handleWheel, { passive: false });
  });

  onDestroy(() => {
    window.removeEventListener("keydown", handleKeydown);
    window.removeEventListener("wheel", handleWheel);
  });

  let zoomPercent = $derived(Math.round(zoomLevel * 100));
</script>

<main class:viewer-open={!!selectedBook}>
  {#if selectedBook}
    <!-- PDF Viewer -->
    <div class="viewer">
      <div class="viewer-header">
        <button class="back-btn" onclick={closeViewer} aria-label="Back to library">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="15 18 9 12 15 6"/>
          </svg>
        </button>
        <span class="viewer-title">{selectedBook.title}</span>
      </div>

      {#if error}
        <p class="error">{error}</p>
      {/if}

      <div class="canvas-wrap" onwheel={handleWheel}>
        <canvas bind:this={canvas}></canvas>
        <canvas
          bind:this={drawCanvas}
          class="draw-canvas"
          onpointerdown={onPointerDown}
          onpointermove={onPointerMove}
          onpointerup={onPointerUp}
          onpointercancel={onPointerCancel}
        ></canvas>
      </div>

      <div class="controls">
        <!-- Prev -->
        <button
          class="chevron"
          onclick={prevPage}
          disabled={currentPage <= 1 || rendering}
          aria-label="Previous page"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="15 18 9 12 15 6"/>
          </svg>
        </button>

        <span class="page-indicator">
          {#if totalPages > 0}
            {currentPage} / {totalPages}
          {:else}
            …
          {/if}
        </span>

        <!-- Next -->
        <button
          class="chevron"
          onclick={nextPage}
          disabled={currentPage >= totalPages || rendering}
          aria-label="Next page"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="9 18 15 12 9 6"/>
          </svg>
        </button>

        <div class="divider"></div>

        <!-- Zoom out -->
        <button
          class="zoom-btn"
          onclick={zoomOut}
          disabled={zoomLevel <= ZOOM_MIN || rendering}
          aria-label="Zoom out"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8"/>
            <line x1="21" y1="21" x2="16.65" y2="16.65"/>
            <line x1="8" y1="11" x2="14" y2="11"/>
          </svg>
        </button>

        <button
          class="zoom-level"
          onclick={resetZoom}
          title="Reset zoom"
          aria-label="Reset zoom to 100%"
        >
          {zoomPercent}%
        </button>

        <!-- Zoom in -->
        <button
          class="zoom-btn"
          onclick={zoomIn}
          disabled={zoomLevel >= ZOOM_MAX || rendering}
          aria-label="Zoom in"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8"/>
            <line x1="21" y1="21" x2="16.65" y2="16.65"/>
            <line x1="11" y1="8" x2="11" y2="14"/>
            <line x1="8" y1="11" x2="14" y2="11"/>
          </svg>
        </button>
      </div>
    </div>

  {:else}
    <!-- Library -->
    <h1>Gloss</h1>

    <button onclick={importPdf} disabled={importing}>
      {importing ? "Importing…" : "Import PDF"}
    </button>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    {#if textbooks.length === 0}
      <p class="empty">No textbooks yet. Import a PDF to get started.</p>
    {:else}
      <ul>
        {#each textbooks as book (book.id)}
          <li>
            <button class="book-item" onclick={() => openBook(book)}>
              <span class="title">{book.title}</span>
              <span class="path">{book.file_path}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</main>

<style>
  :root {
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    font-size: 16px;
    line-height: 24px;
    color: #0f0f0f;
    background-color: #f6f6f6;
  }

  @media (prefers-color-scheme: dark) {
    :root {
      color: #f6f6f6;
      background-color: #1a1a1a;
    }
    li {
      background: #2a2a2a;
    }
    .path {
      color: #aaa;
    }
    .book-item:hover {
      background: #333 !important;
    }
    .viewer {
      background: #1a1a1a;
    }
    .viewer-header {
      border-bottom-color: #333;
    }
    .canvas-wrap {
      background: #111;
    }
    .controls {
      background: #1a1a1a;
      border-top-color: #333;
    }
    .page-indicator {
      color: #aaa;
    }
    .zoom-level {
      color: #aaa;
    }
    .divider {
      background: #444;
    }
  }

  main {
    max-width: 720px;
    margin: 0 auto;
    padding: 2rem 1rem;
  }

  main.viewer-open {
    /* Remove default padding when viewer is open so we control layout fully */
    padding: 0;
    max-width: none;
  }

  h1 {
    font-size: 2rem;
    margin-bottom: 1.5rem;
  }

  /* Import button */
  button {
    padding: 0.6em 1.4em;
    font-size: 1em;
    font-weight: 600;
    border: none;
    border-radius: 6px;
    background: #396cd8;
    color: #fff;
    cursor: pointer;
    transition: background 0.2s;
  }

  button:hover:not(:disabled) {
    background: #2a55c0;
  }

  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .error {
    color: #c0392b;
    margin-top: 0.75rem;
  }

  .empty {
    color: #888;
    margin-top: 1.5rem;
  }

  /* Library list */
  ul {
    list-style: none;
    padding: 0;
    margin-top: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  li {
    border-radius: 6px;
    background: #efefef;
    overflow: hidden;
  }

  .book-item {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 0.75rem 1rem;
    width: 100%;
    text-align: left;
    background: transparent;
    color: inherit;
    border-radius: 0;
    font-size: 1em;
    font-weight: 400;
    cursor: pointer;
    transition: background 0.15s;
  }

  .book-item:hover {
    background: #e0e0e0 !important;
  }

  .title {
    font-weight: 600;
  }

  .path {
    font-size: 0.8em;
    color: #666;
    font-family: monospace;
  }

  /* ── Viewer ── */
  .viewer {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .viewer-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #ddd;
    flex-shrink: 0;
  }

  .viewer-title {
    font-weight: 600;
    font-size: 1.05em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .back-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    padding: 0;
    background: transparent;
    color: inherit;
    border: 1px solid #ccc;
    border-radius: 6px;
    flex-shrink: 0;
  }

  .back-btn:hover {
    background: #eee !important;
  }

  .back-btn svg {
    width: 18px;
    height: 18px;
  }

  .canvas-wrap {
    flex: 1;
    position: relative;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    background: #e8e8e8;
    padding: 1rem;
    overflow: auto;
    min-height: 0; /* critical: allow flex child to shrink */
  }

  .canvas-wrap canvas {
    border-radius: 2px;
    box-shadow: 0 2px 16px rgba(0,0,0,0.18);
  }

  .draw-canvas {
    position: absolute;
    inset: 0;
    width: 100% !important;
    height: 100% !important;
    cursor: crosshair;
    touch-action: none;
    /* no box-shadow — transparent overlay */
    box-shadow: none;
  }

  /* ── Navigation controls — pinned to bottom ── */
  .controls {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-top: 1px solid #ddd;
    background: #f6f6f6;
    flex-shrink: 0;
  }

  .chevron {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    padding: 0;
    background: transparent;
    color: inherit;
    border: 1px solid #ccc;
    border-radius: 8px;
    transition: background 0.15s, border-color 0.15s, opacity 0.15s;
  }

  .chevron:hover:not(:disabled) {
    background: #eee !important;
    border-color: #aaa;
  }

  .chevron:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .chevron svg {
    width: 20px;
    height: 20px;
  }

  .page-indicator {
    font-size: 0.9em;
    color: #666;
    min-width: 5ch;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .divider {
    width: 1px;
    height: 24px;
    background: #ccc;
    margin: 0 0.25rem;
  }

  .zoom-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    padding: 0;
    background: transparent;
    color: inherit;
    border: 1px solid #ccc;
    border-radius: 8px;
    transition: background 0.15s, border-color 0.15s, opacity 0.15s;
  }

  .zoom-btn:hover:not(:disabled) {
    background: #eee !important;
    border-color: #aaa;
  }

  .zoom-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .zoom-btn svg {
    width: 18px;
    height: 18px;
  }

  .zoom-level {
    font-size: 0.82em;
    font-variant-numeric: tabular-nums;
    min-width: 3.8ch;
    text-align: center;
    padding: 0.25em 0.5em;
    background: transparent;
    color: #666;
    border: 1px solid #ccc;
    border-radius: 6px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s;
  }

  .zoom-level:hover {
    background: #eee !important;
  }
</style>

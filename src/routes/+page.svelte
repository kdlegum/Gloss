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

  interface SourceDocument {
    id: number;
    title: string;
    file_path: string;
  }

  let sourceDocuments = $state<SourceDocument[]>([]);
  let importing = $state(false);
  let error = $state<string | null>(null);

  // PDF viewer state
  let selectedBook = $state<SourceDocument | null>(null);
  let pdfDoc = $state<pdfjsLib.PDFDocumentProxy | null>(null);
  let currentPage = $state(1);
  let totalPages = $state(0);
  let rendering = $state(false);
  let canvas = $state<HTMLCanvasElement>(null!);
  let dryCanvas = $state<HTMLCanvasElement>(null!);
  let wetCanvas = $state<HTMLCanvasElement>(null!);

  // DB row id for the current (source_document, page) pair; null until resolved
  let currentPageId = $state<number | null>(null);

  // ── Stroke prefetch cache ──
  // 5-entry LRU keyed by page DB id. Evicts the oldest entry when full.
  const STROKE_CACHE_SIZE = 5;
  const strokeCacheOrder: number[] = [];   // front = most recently used
  const strokeCacheData = new Map<number, StrokeOutput[]>();
  // Maps "bookId:pageNum" → pageId so prefetched page IDs can be reused
  // without a second IPC round-trip when navigating.
  const pageIdLookup = new Map<string, number>();

  function cacheGet(pageId: number): StrokeOutput[] | undefined {
    if (!strokeCacheData.has(pageId)) return undefined;
    // Move to front (most recently used)
    const idx = strokeCacheOrder.indexOf(pageId);
    if (idx !== -1) strokeCacheOrder.splice(idx, 1);
    strokeCacheOrder.unshift(pageId);
    return strokeCacheData.get(pageId);
  }

  function cachePut(pageId: number, data: StrokeOutput[]) {
    if (strokeCacheData.has(pageId)) {
      const idx = strokeCacheOrder.indexOf(pageId);
      if (idx !== -1) strokeCacheOrder.splice(idx, 1);
    } else if (strokeCacheOrder.length >= STROKE_CACHE_SIZE) {
      const evict = strokeCacheOrder.pop()!;
      strokeCacheData.delete(evict);
    }
    strokeCacheOrder.unshift(pageId);
    strokeCacheData.set(pageId, data);
  }

  function cacheEvict(pageId: number) {
    const idx = strokeCacheOrder.indexOf(pageId);
    if (idx !== -1) strokeCacheOrder.splice(idx, 1);
    strokeCacheData.delete(pageId);
  }

  /** Fire-and-forget: resolve page id and warm the cache for an adjacent page. */
  function prefetchPage(bookId: number, pageNum: number) {
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return;
    const key = `${bookId}:${pageNum}`;
    invoke<number>("get_or_create_page", {
      sourceDocumentId: bookId,
      pageNumber: pageNum,
    }).then(pageId => {
      pageIdLookup.set(key, pageId);
      if (strokeCacheData.has(pageId)) return; // already cached
      return invoke<StrokeOutput[]>("load_strokes", { pageId }).then(data => {
        cachePut(pageId, data);
      });
    }).catch(() => { /* prefetch failures are silent */ });
  }

  async function resolvePageId(bookId: number, pageNum: number) {
    const key = `${bookId}:${pageNum}`;
    const pageId = pageIdLookup.get(key)
      ?? await invoke<number>("get_or_create_page", {
           sourceDocumentId: bookId,
           pageNumber: pageNum,
         });
    pageIdLookup.set(key, pageId);
    currentPageId = pageId;
    await loadAndDrawStrokes(pageId);
    // Warm adjacent pages after the current page is ready
    prefetchPage(bookId, pageNum - 1);
    prefetchPage(bookId, pageNum + 1);
  }

  interface StrokeOutput {
    id: number;
    colour: string;
    points: { x: number; y: number }[];
    min_x: number;
    min_y: number;
    max_x: number;
    max_y: number;
    chunk_id: number | null;
  }

  async function loadAndDrawStrokes(pageId: number) {
    const cached = cacheGet(pageId);
    const loaded = cached ?? await invoke<StrokeOutput[]>("load_strokes", { pageId });
    if (!cached) cachePut(pageId, loaded);
    // Each loaded stroke has no pressure data; use a fixed pressure so line
    // width is consistent with how it was originally drawn.
    strokes = loaded.map(s => ({
      id: s.id,
      points: s.points.map(p => ({ x: p.x, y: p.y, pressure: 0.5 })),
      bbox: { minX: s.min_x, minY: s.min_y, maxX: s.max_x, maxY: s.max_y },
      chunkId: s.chunk_id,
    }));
    redoStack = [];
    if (cached) {
      // Strokes were already in memory — syncDrawCanvasSize was queued by
      // renderPage in the same microtask turn, so the canvas is already sized.
      // Draw immediately rather than deferring another frame.
      syncDrawCanvasSize();
      redrawAllStrokes();
    } else {
      // Data arrived from IPC after renderPage's rAF may have already fired;
      // queue a new frame to ensure the canvas is sized before painting.
      requestAnimationFrame(redrawAllStrokes);
    }
  }

  // ── Tool mode ──
  type Mode = 'draw' | 'erase' | 'select';
  let mode = $state<Mode>('draw');

  // ── Select state ──
  type Rect = { x: number; y: number; w: number; h: number }; // normalised page space
  let selectOrigin = $state<{ x: number; y: number } | null>(null);
  let selectRect   = $state<Rect | null>(null);
  let selectedStrokes = $state<Set<Stroke>>(new Set());
  // Union bbox of all selected strokes, in normalised page space. Used to
  // export the area as an image. null when nothing is selected.
  let selection = $state<{ x: number; y: number; width: number; height: number } | null>(null);

  // ── Drawing state ──
  //
  // Coordinates are stored in *normalised page space*:
  //   x, y ∈ [0, 1]  — 0 = page left/top edge, 1 = page right/bottom edge.
  //   Values outside [0,1] are allowed (drawing in the margin).
  //
  // This makes strokes zoom-invariant: to draw them at any zoom level, just
  // multiply by the current PDF canvas CSS size.
  //
  type Point = { x: number; y: number; pressure: number };  // normalised page space
  type BBox = { minX: number; minY: number; maxX: number; maxY: number };
  type Stroke = { id: number | null; points: Point[]; bbox: BBox; chunkId: number | null };

  let isDrawing = $state(false);
  let currentStroke = $state<Point[]>([]);
  let wetRafPending = false;
  // Moving-average smoother (window = 3) — disabled for now.
  // const MA_WINDOW = 3;
  // let maBuffer: Point[] = [];
  // Completed strokes for the current page (cleared on page navigation)
  let strokes = $state<Stroke[]>([]);
  // Strokes removed by Ctrl+Z, available for Ctrl+Y (cleared on new stroke).
  // Each entry is an array of strokes removed together as one action.
  let redoStack = $state<Stroke[][]>([]);
  // Active pointer id for palm rejection (first pen/stylus wins)
  let activePointerId: number | null = null;

  // ── Coordinate transforms ──
  //
  // Normalised page space: (0,0) = PDF page top-left, (1,1) = PDF page bottom-right.
  // Screen space: the pointer event's (clientX, clientY).
  // Canvas space: logical pixel coordinate on dryCanvas/wetCanvas (set by syncDrawCanvasSize).

  /**
   * Convert a screen-space pointer position to normalised page space.
   * The PDF canvas element defines the [0,1] coordinate space.
   */
  function screenToPage(screenX: number, screenY: number): { x: number; y: number } {
    const pageRect = canvas.getBoundingClientRect();
    return {
      x: (screenX - pageRect.left) / pageRect.width,
      y: (screenY - pageRect.top)  / pageRect.height,
    };
  }

  /**
   * Convert a normalised page-space coordinate to canvas logical pixels.
   * dryCanvas/wetCanvas cover the full canvas-wrap area; the page sits somewhere inside.
   */
  function pageToDrawCanvas(normX: number, normY: number): { x: number; y: number } {
    const wrap = dryCanvas.parentElement!;
    const pageRect = canvas.getBoundingClientRect();
    const wrapRect = wrap.getBoundingClientRect();

    // Page origin in scroll-content space (accounts for scroll offset)
    const originX = pageRect.left - wrapRect.left + wrap.scrollLeft;
    const originY = pageRect.top  - wrapRect.top  + wrap.scrollTop;

    // draw canvas logical pixels == CSS pixels (we set width/height explicitly)
    return {
      x: originX + normX * pageRect.width,
      y: originY + normY * pageRect.height,
    };
  }

  /**
   * Convert a pointer event to a normalised page-space point.
   */
  function getPagePoint(e: PointerEvent): Point {
    const { x, y } = screenToPage(e.clientX, e.clientY);
    return {
      x,
      y,
      pressure: e.pressure > 0 ? e.pressure : 0.5,
    };
  }

  function isPenOrMouse(e: PointerEvent): boolean {
    return e.pointerType === "pen" || e.pointerType === "mouse";
  }

  /** Compute a bounding box in normalised page space from a list of points. */
  function computeBBox(points: Point[]): BBox {
    let minX = points[0].x, minY = points[0].y;
    let maxX = minX, maxY = minY;
    for (const p of points) {
      if (p.x < minX) minX = p.x;
      if (p.y < minY) minY = p.y;
      if (p.x > maxX) maxX = p.x;
      if (p.y > maxY) maxY = p.y;
    }
    return { minX, minY, maxX, maxY };
  }

  /**
   * Erase any stroke whose point comes within `radiusPx` CSS pixels of the
   * eraser tip (given in normalised page space).  Uses the stored bbox as a
   * cheap pre-filter before checking individual points.
   */
  function eraseAt(normX: number, normY: number) {
    const pageRect = canvas.getBoundingClientRect();
    // Convert the pixel radius to normalised units (use the smaller dimension
    // so the circle isn't stretched on non-square pages).
    const radiusPx = 6;
    const rNormX = radiusPx / pageRect.width;
    const rNormY = radiusPx / pageRect.height;
    const toDelete: Stroke[] = [];
    const toKeep: Stroke[] = [];

    for (const stroke of strokes) {
      const { minX, minY, maxX, maxY } = stroke.bbox;

      // Bbox pre-filter (expanded by radius)
      if (
        normX < minX - rNormX || normX > maxX + rNormX ||
        normY < minY - rNormY || normY > maxY + rNormY
      ) {
        toKeep.push(stroke);
        continue;
      }

      // Per-point distance check (screen-space circle)
      let hit = false;
      for (const p of stroke.points) {
        const dx = (p.x - normX) * pageRect.width;
        const dy = (p.y - normY) * pageRect.height;
        if (dx * dx + dy * dy <= radiusPx * radiusPx) {
          hit = true;
          break;
        }
      }

      if (hit) {
        toDelete.push(stroke);
      } else {
        toKeep.push(stroke);
      }
    }

    if (toDelete.length === 0) return;

    strokes = toKeep;
    redoStack = [...redoStack, toDelete];
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of toDelete) {
      if (s.id !== null) {
        invoke("delete_stroke", { strokeId: s.id }).catch(() => {});
      }
    }
    redrawAllStrokes();
  }

  function onPointerDown(e: PointerEvent) {
    // Palm rejection: ignore touch; ignore second concurrent pointer
    if (!isPenOrMouse(e)) return;
    if (activePointerId !== null) return;

    if (mode === 'erase') {
      activePointerId = e.pointerId;
      wetCanvas.setPointerCapture(e.pointerId);
      const { x, y } = screenToPage(e.clientX, e.clientY);
      eraseAt(x, y);
      e.preventDefault();
      return;
    }

    if (mode === 'select') {
      activePointerId = e.pointerId;
      wetCanvas.setPointerCapture(e.pointerId);
      const { x, y } = screenToPage(e.clientX, e.clientY);
      selectOrigin = { x, y };
      selectRect = null;
      selectedStrokes = new Set();
      selection = null;
      redrawSelectionHighlight();
      e.preventDefault();
      return;
    }

    activePointerId = e.pointerId;
    wetCanvas.setPointerCapture(e.pointerId);
    isDrawing = true;
    // maBuffer = [];  // reset smoother buffer on stroke start
    currentStroke = [getPagePoint(e)];
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (mode === 'erase') {
      if (e.pointerId !== activePointerId) return;
      e.preventDefault();
      const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
      for (const ce of events) {
        const { x, y } = screenToPage(ce.clientX, ce.clientY);
        eraseAt(x, y);
      }
      return;
    }

    if (mode === 'select') {
      if (e.pointerId !== activePointerId || !selectOrigin) return;
      e.preventDefault();
      const { x, y } = screenToPage(e.clientX, e.clientY);
      selectRect = {
        x: Math.min(selectOrigin.x, x),
        y: Math.min(selectOrigin.y, y),
        w: Math.abs(x - selectOrigin.x),
        h: Math.abs(y - selectOrigin.y),
      };
      drawRubberBand(selectRect);
      return;
    }
    if (!isDrawing || e.pointerId !== activePointerId) return;
    e.preventDefault();

    // Use getCoalescedEvents when available for smoother lines on high-freq devices
    const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
    for (const ce of events) {
      // Moving-average smoother (window = MA_WINDOW) — disabled for now.
      // const raw = getPagePoint(ce);
      // maBuffer.push(raw);
      // if (maBuffer.length > MA_WINDOW) maBuffer.shift();
      // const n = maBuffer.length;
      // const smoothed: Point = {
      //   x: maBuffer.reduce((s, p) => s + p.x, 0) / n,
      //   y: maBuffer.reduce((s, p) => s + p.y, 0) / n,
      //   pressure: maBuffer.reduce((s, p) => s + p.pressure, 0) / n,
      // };
      // currentStroke = [...currentStroke, smoothed];
      currentStroke = [...currentStroke, getPagePoint(ce)];
    }

    if (!wetRafPending) {
      wetRafPending = true;
      requestAnimationFrame(() => {
        wetRafPending = false;
        redrawWetStroke();
      });
    }
  }

  function onPointerUp(e: PointerEvent) {
    if (mode === 'erase') {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      e.preventDefault();
      return;
    }

    if (mode === 'select') {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      if (selectRect) {
        selectedStrokes = hitTestStrokes(selectRect);
        selection = unionBBox(selectedStrokes) ?? { x: selectRect.x, y: selectRect.y, width: selectRect.w, height: selectRect.h };
      }
      selectOrigin = null;
      selectRect = null;
      redrawSelectionHighlight();
      e.preventDefault();
      return;
    }
    if (e.pointerId !== activePointerId) return;
    if (isDrawing && currentStroke.length >= 2) {
      const completed = currentStroke;
      // Add with a null id; fill in the real id once the save resolves.
      const stroke: Stroke = { id: null, points: completed, bbox: computeBBox(completed), chunkId: null };
      strokes = [...strokes, stroke];
      redoStack = []; // any new stroke wipes the redo history
      redrawAllStrokes(); // bake the new stroke onto the dry canvas
      if (currentPageId !== null) {
        cacheEvict(currentPageId);
        invoke<number>("save_stroke", {
          pageId: currentPageId,
          stroke: {
            colour: "rgba(30, 80, 220, 0.85)",
            points: completed.map(({ x, y }) => ({ x, y })),
            chunkId: stroke.chunkId,
          },
        }).then(id => {
          stroke.id = id;
        }).catch(() => { /* persist failure is silent */ });
      }
    }
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    // Clear the wet canvas — the committed stroke is now on the dry canvas
    if (wetCanvas) {
      wetCanvas.getContext("2d")!.clearRect(0, 0, wetCanvas.width, wetCanvas.height);
    }
  }

  function onPointerCancel(e: PointerEvent) {
    if (mode === 'erase') {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      return;
    }

    if (mode === 'select') {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      selectOrigin = null;
      selectRect = null;
      redrawSelectionHighlight();
      return;
    }
    if (e.pointerId !== activePointerId) return;
    // Discard the in-progress stroke (e.g. palm was detected mid-stroke),
    // but keep all previously committed strokes.
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    if (wetCanvas) {
      wetCanvas.getContext("2d")!.clearRect(0, 0, wetCanvas.width, wetCanvas.height);
    }
  }

  /** Paint the in-progress stroke onto the wet canvas (cleared each call). */
  function redrawWetStroke() {
    if (!wetCanvas) return;
    const ctx = wetCanvas.getContext("2d")!;
    ctx.clearRect(0, 0, wetCanvas.width, wetCanvas.height);
    const pts = currentStroke;
    if (pts.length < 2) return;

    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "rgb(30, 80, 220)";

    // Convert all points to draw-canvas space up front.
    const dc = pts.map(p => pageToDrawCanvas(p.x, p.y));

    ctx.beginPath();
    ctx.moveTo(dc[0].x, dc[0].y);
    let currentWidth = 1 + pts[1].pressure * 5;
    ctx.lineWidth = currentWidth;

    for (let i = 1; i < dc.length - 1; i++) {
      const width = 1 + pts[i].pressure * 5;
      if (ctx.lineWidth !== width) {
        ctx.stroke();
        ctx.beginPath();
        const midPrev = { x: (dc[i - 1].x + dc[i].x) / 2, y: (dc[i - 1].y + dc[i].y) / 2 };
        ctx.moveTo(midPrev.x, midPrev.y);
        ctx.lineWidth = width;
      }
      const mid = { x: (dc[i].x + dc[i + 1].x) / 2, y: (dc[i].y + dc[i + 1].y) / 2 };
      ctx.quadraticCurveTo(dc[i].x, dc[i].y, mid.x, mid.y);
    }
    const last = dc.length - 1;
    ctx.quadraticCurveTo(dc[last - 1].x, dc[last - 1].y, dc[last].x, dc[last].y);
    ctx.stroke();
  }

  /** Repaint all committed strokes onto the dry canvas from scratch. */
  function redrawAllStrokes() {
    if (!dryCanvas) return;
    const ctx = dryCanvas.getContext("2d")!;
    ctx.clearRect(0, 0, dryCanvas.width, dryCanvas.height);

    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "rgb(30, 80, 220)";

    for (const stroke of strokes) {
      if (stroke.points.length < 2) continue;
      const dc = stroke.points.map(p => pageToDrawCanvas(p.x, p.y));

      ctx.beginPath();
      ctx.moveTo(dc[0].x, dc[0].y);
      ctx.lineWidth = 1 + stroke.points[1].pressure * 5;

      for (let i = 1; i < dc.length - 1; i++) {
        const width = 1 + stroke.points[i].pressure * 5;
        if (ctx.lineWidth !== width) {
          ctx.stroke();
          ctx.beginPath();
          const midPrev = { x: (dc[i - 1].x + dc[i].x) / 2, y: (dc[i - 1].y + dc[i].y) / 2 };
          ctx.moveTo(midPrev.x, midPrev.y);
          ctx.lineWidth = width;
        }
        const mid = { x: (dc[i].x + dc[i + 1].x) / 2, y: (dc[i].y + dc[i + 1].y) / 2 };
        ctx.quadraticCurveTo(dc[i].x, dc[i].y, mid.x, mid.y);
      }
      const last = dc.length - 1;
      ctx.quadraticCurveTo(dc[last - 1].x, dc[last - 1].y, dc[last].x, dc[last].y);
      ctx.stroke();
    }
    // Repaint selection highlight on top (wet canvas)
    redrawSelectionHighlight();
  }

  /**
   * Compute the union bounding box of a set of strokes in normalised page
   * space. Returns null if the set is empty.
   */
  function unionBBox(hits: Set<Stroke>): { x: number; y: number; width: number; height: number } | null {
    if (hits.size === 0) return null;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const s of hits) {
      if (s.bbox.minX < minX) minX = s.bbox.minX;
      if (s.bbox.minY < minY) minY = s.bbox.minY;
      if (s.bbox.maxX > maxX) maxX = s.bbox.maxX;
      if (s.bbox.maxY > maxY) maxY = s.bbox.maxY;
    }
    return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
  }

  /** Draw the rubber-band selection rectangle on the wet canvas. */
  function drawRubberBand(rect: Rect) {
    if (!wetCanvas) return;
    const ctx = wetCanvas.getContext("2d")!;
    ctx.clearRect(0, 0, wetCanvas.width, wetCanvas.height);

    const tl = pageToDrawCanvas(rect.x,          rect.y);
    const br = pageToDrawCanvas(rect.x + rect.w,  rect.y + rect.h);
    const w = br.x - tl.x;
    const h = br.y - tl.y;

    ctx.save();
    ctx.strokeStyle = "rgba(57, 108, 216, 0.9)";
    ctx.lineWidth = 1.5;
    ctx.setLineDash([5, 4]);
    ctx.strokeRect(tl.x, tl.y, w, h);
    ctx.fillStyle = "rgba(57, 108, 216, 0.08)";
    ctx.fillRect(tl.x, tl.y, w, h);
    ctx.restore();
  }

  /** Return the set of strokes that have at least one point inside `rect`. */
  function hitTestStrokes(rect: Rect): Set<Stroke> {
    const r2 = rect.x + rect.w;
    const b2 = rect.y + rect.h;
    const hit = new Set<Stroke>();
    for (const stroke of strokes) {
      // Cheap bbox pre-filter: skip if stroke bbox doesn't overlap rect at all
      if (stroke.bbox.maxX < rect.x || stroke.bbox.minX > r2 ||
          stroke.bbox.maxY < rect.y || stroke.bbox.minY > b2) continue;
      // Per-point check
      for (const p of stroke.points) {
        if (p.x >= rect.x && p.x <= r2 && p.y >= rect.y && p.y <= b2) {
          hit.add(stroke);
          break;
        }
      }
    }
    return hit;
  }

  /**
   * Repaint the wet canvas to show the current selection highlight.
   * Call after selectedStrokes changes and after redrawAllStrokes.
   */
  function redrawSelectionHighlight() {
    if (!wetCanvas) return;
    const ctx = wetCanvas.getContext("2d")!;
    ctx.clearRect(0, 0, wetCanvas.width, wetCanvas.height);
    if (selectedStrokes.size === 0) return;

    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "rgba(255, 140, 0, 0.9)";

    for (const stroke of selectedStrokes) {
      if (stroke.points.length < 2) continue;
      ctx.beginPath();
      const first = pageToDrawCanvas(stroke.points[0].x, stroke.points[0].y);
      ctx.moveTo(first.x, first.y);
      for (let i = 1; i < stroke.points.length; i++) {
        const dc = pageToDrawCanvas(stroke.points[i].x, stroke.points[i].y);
        const width = 1 + stroke.points[i].pressure * 5;
        if (ctx.lineWidth !== width) {
          ctx.stroke();
          ctx.beginPath();
          const dcPrev = pageToDrawCanvas(stroke.points[i - 1].x, stroke.points[i - 1].y);
          ctx.moveTo(dcPrev.x, dcPrev.y);
          ctx.lineWidth = width;
        }
        ctx.lineTo(dc.x, dc.y);
      }
      ctx.stroke();
    }
    ctx.restore();
  }

  // Sync both overlay canvases to the full scrollable area, then repaint.
  function syncDrawCanvasSize() {
    if (!dryCanvas || !wetCanvas || !canvas) return;
    const wrap = dryCanvas.parentElement!;
    // offsetWidth/offsetHeight give the full element size including padding,
    // which is what the overlays need to cover. We also take the max with
    // scrollWidth/scrollHeight to handle cases where zoomed content overflows.
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(wrap.offsetWidth, wrap.scrollWidth);
    const h = Math.max(wrap.offsetHeight, wrap.scrollHeight);
    for (const c of [dryCanvas, wetCanvas]) {
      c.style.width  = w + "px";
      c.style.height = h + "px";
      c.width  = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
      c.getContext("2d")!.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    redrawAllStrokes();
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

  async function loadSourceDocuments() {
    sourceDocuments = await invoke<SourceDocument[]>("list_textbooks");
  }

  async function importPdf() {
    error = null;
    importing = true;
    try {
      await invoke<SourceDocument>("import_pdf");
      await loadSourceDocuments();
    } catch (e: unknown) {
      if (e !== "cancelled") {
        error = String(e);
      }
    } finally {
      importing = false;
    }
  }

  async function openBook(book: SourceDocument) {
    error = null;
    selectedBook = book;
    currentPage = loadSavedPage(book.id);
    currentPageId = null;
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
      resolvePageId(book.id, currentPage);
    } catch (e) {
      error = String(e);
    }
  }

  async function renderPage(pageNum: number) {
    if (!pdfDoc || !canvas) return;
    rendering = true;
    try {
      const page = await pdfDoc.getPage(pageNum);
      const wrap = canvas.parentElement;
      const availableWidth = wrap
        ? wrap.clientWidth - parseFloat(getComputedStyle(wrap).paddingLeft) - parseFloat(getComputedStyle(wrap).paddingRight)
        : 600;
      const fitScale = Math.min(
        availableWidth / page.getViewport({ scale: 1 }).width,
        1.8
      );
      const scale = fitScale * zoomLevel;
      const dpr = window.devicePixelRatio || 1;
      const viewport = page.getViewport({ scale });
      canvas.width  = Math.round(viewport.width  * dpr);
      canvas.height = Math.round(viewport.height * dpr);
      canvas.style.width  = viewport.width  + "px";
      canvas.style.height = viewport.height + "px";
      const ctx = canvas.getContext("2d")!;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, viewport.width, viewport.height);
      await page.render({ canvasContext: ctx, viewport }).promise;
      requestAnimationFrame(syncDrawCanvasSize);
    } finally {
      rendering = false;
    }
  }

  async function goToPage(pageNum: number) {
    if (rendering) return;
    const clamped = Math.max(1, Math.min(totalPages, pageNum));
    if (clamped === currentPage) return;
    currentPage = clamped;
    currentPageId = null;
    strokes = [];
    redoStack = [];
    selectedStrokes = new Set();
    selection = null;
    if (selectedBook) saveCurrentPage(selectedBook.id, currentPage);
    await renderPage(currentPage);
    if (selectedBook) await resolvePageId(selectedBook.id, currentPage);
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
    currentPageId = null;
    totalPages = 0;
    zoomLevel = 1.0;
    strokes = [];
    redoStack = [];
    selectedStrokes = new Set();
    selection = null;
  }

  // ── Undo / Redo ──
  async function undoStroke() {
    if (strokes.length === 0) return;
    const removed = strokes[strokes.length - 1];
    strokes = strokes.slice(0, -1);
    redoStack = [...redoStack, [removed]];
    if (currentPageId !== null) cacheEvict(currentPageId);
    if (removed.id !== null) {
      invoke("delete_stroke", { strokeId: removed.id }).catch(() => {});
    }
    redrawAllStrokes();
  }

  async function redoStroke() {
    if (redoStack.length === 0) return;
    const entry = redoStack[redoStack.length - 1];
    redoStack = redoStack.slice(0, -1);
    strokes = [...strokes, ...entry];
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const restored of entry) {
      if (currentPageId !== null) {
        invoke<number>("save_stroke", {
          pageId: currentPageId,
          stroke: {
            colour: "rgba(30, 80, 220, 0.85)",
            points: restored.points.map(({ x, y }) => ({ x, y })),
            chunkId: restored.chunkId,
          },
        }).then(id => {
          restored.id = id;
        }).catch(() => {});
      }
    }
    redrawAllStrokes();
  }

  // ── AI rasterisation ──
  let aiWorking = $state(false);
  let aiDebugImage = $state<string | null>(null);

  /**
   * Composite the PDF canvas and the ink overlay (dryCanvas) into an offscreen
   * canvas cropped to the current selection bbox, then export as base64 PNG.
   *
   * `selection` is in normalised page space (0–1 relative to the PDF canvas).
   * The PDF canvas pixel dimensions directly map: x_px = normX * canvas.width.
   * dryCanvas covers the full scroll area; the page sits at an offset inside it,
   * so we use pageToDrawCanvas to find where the crop starts on the ink layer.
   */
  async function rasteriseSelection(): Promise<string> {
    if (!selection || !canvas || !dryCanvas) throw new Error("Nothing selected");

    // PDF canvas pixel coords of the selection
    const sx = selection.x * canvas.width;
    const sy = selection.y * canvas.height;
    const sw = selection.width  * canvas.width;
    const sh = selection.height * canvas.height;

    // dryCanvas pixel coords of the same region
    const tl = pageToDrawCanvas(selection.x, selection.y);
    const br = pageToDrawCanvas(selection.x + selection.width, selection.y + selection.height);
    const dw = br.x - tl.x;
    const dh = br.y - tl.y;

    const offscreen = new OffscreenCanvas(Math.round(sw), Math.round(sh));
    const ctx = offscreen.getContext("2d")!;

    // Draw PDF layer
    ctx.drawImage(canvas, sx, sy, sw, sh, 0, 0, sw, sh);
    // Draw ink overlay (dryCanvas spans the full wrap; crop to the page region)
    ctx.drawImage(dryCanvas, tl.x, tl.y, dw, dh, 0, 0, sw, sh);

    const blob = await offscreen.convertToBlob({ type: "image/png" });
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve((reader.result as string).split(",")[1]);
      reader.onerror = reject;
      reader.readAsDataURL(blob);
    });
  }

  async function onAiClick() {
    if (!selection || aiWorking) return;
    aiWorking = true;
    try {
      const b64 = await rasteriseSelection();
      aiDebugImage = `data:image/png;base64,${b64}`;
    } catch (e) {
      console.error("rasteriseSelection failed:", e);
    } finally {
      aiWorking = false;
    }
  }

  // ── Delete selected strokes ──
  async function deleteSelected() {
    if (selectedStrokes.size === 0) return;
    const deleted = [...selectedStrokes];
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of deleted) {
      if (s.id !== null) {
        invoke("delete_stroke", { strokeId: s.id }).catch(() => {});
      }
    }
    redoStack = [...redoStack, deleted];
    strokes = strokes.filter(s => !selectedStrokes.has(s));
    selectedStrokes = new Set();
    selection = null;
    redrawAllStrokes();
  }

  // ── Keyboard navigation ──
  async function handleKeydown(e: KeyboardEvent) {
    if (!selectedBook || !pdfDoc) return;

    // Don't steal focus from inputs
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

    if ((e.ctrlKey || e.metaKey) && e.key === "z") {
      e.preventDefault();
      await undoStroke();
    } else if ((e.ctrlKey || e.metaKey) && (e.key === "y" || (e.shiftKey && e.key === "z"))) {
      e.preventDefault();
      await redoStroke();
    } else if ((e.key === "Delete" || e.key === "Backspace") && mode === 'select' && selectedStrokes.size > 0) {
      e.preventDefault();
      await deleteSelected();
    } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
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

  let wrapResizeObserver: ResizeObserver | null = null;

  function observeWrapResize(node: HTMLElement) {
    wrapResizeObserver?.disconnect();
    wrapResizeObserver = new ResizeObserver(() => syncDrawCanvasSize());
    wrapResizeObserver.observe(node);
  }

  onMount(() => {
    loadSourceDocuments();
    window.addEventListener("keydown", handleKeydown);
    window.addEventListener("wheel", handleWheel, { passive: false });
  });

  onDestroy(() => {
    window.removeEventListener("keydown", handleKeydown);
    window.removeEventListener("wheel", handleWheel);
    wrapResizeObserver?.disconnect();
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

      <div class="canvas-wrap" onwheel={handleWheel} use:observeWrapResize>
        <canvas bind:this={canvas}></canvas>
        <canvas bind:this={dryCanvas} class="dry-canvas"></canvas>
        <canvas
          bind:this={wetCanvas}
          class="wet-canvas"
          class:erasing={mode === 'erase'}
          class:selecting={mode === 'select'}
          onpointerdown={onPointerDown}
          onpointermove={onPointerMove}
          onpointerup={onPointerUp}
          onpointercancel={onPointerCancel}
        ></canvas>
      </div>

      {#if aiDebugImage}
        <div class="ai-debug" role="dialog" aria-label="Debug preview">
          <button class="ai-debug-close" onclick={() => aiDebugImage = null} aria-label="Close">✕</button>
          <img src={aiDebugImage} alt="Rasterised selection" />
        </div>
      {/if}

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

        <!-- Undo -->
        <button
          class="ink-btn"
          onclick={undoStroke}
          disabled={strokes.length === 0}
          aria-label="Undo stroke"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 14 4 9l5-5"/>
            <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>
          </svg>
        </button>

        <!-- Redo -->
        <button
          class="ink-btn"
          onclick={redoStroke}
          disabled={redoStack.length === 0}
          aria-label="Redo stroke"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M15 14l5-5-5-5"/>
            <path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>
          </svg>
        </button>

        <div class="divider"></div>

        <!-- Draw -->
        <button
          class="tool-btn"
          class:active={mode === 'draw'}
          onclick={() => { mode = 'draw'; selectedStrokes = new Set(); selection = null; redrawSelectionHighlight(); }}
          aria-label="Draw"
          aria-pressed={mode === 'draw'}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 19l7-7 3 3-7 7-3-3z"/>
            <path d="M18 13l-1.5-7.5L2 2l3.5 14.5L13 18l5-5z"/>
            <path d="M2 2l7.586 7.586"/>
            <circle cx="11" cy="11" r="2"/>
          </svg>
        </button>

        <!-- Erase -->
        <button
          class="tool-btn"
          class:active={mode === 'erase'}
          onclick={() => { mode = 'erase'; selectedStrokes = new Set(); selection = null; redrawSelectionHighlight(); }}
          aria-label="Erase"
          aria-pressed={mode === 'erase'}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M20 20H7L3 16l10-10 7 7-2.5 2.5"/>
            <path d="M6.5 17.5l5-5"/>
          </svg>
        </button>

        <!-- Select -->
        <button
          class="tool-btn"
          class:active={mode === 'select'}
          onclick={() => { mode = 'select'; selectedStrokes = new Set(); selection = null; redrawSelectionHighlight(); }}
          aria-label="Select"
          aria-pressed={mode === 'select'}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M5 3l14 9-7 1-4 7-3-17z"/>
          </svg>
        </button>

        <!-- AI -->
        <button
          class="tool-btn ai-btn"
          class:active={!!selection}
          onclick={onAiClick}
          disabled={!selection || aiWorking}
          aria-label="Ask AI about selection"
          title="Ask AI"
        >
          {#if aiWorking}
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" class="spin">
              <path d="M21 12a9 9 0 1 1-6.219-8.56"/>
            </svg>
          {:else}
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 2a7 7 0 0 1 7 7c0 2.5-1.3 4.7-3.3 6L15 21H9l-.3-6.1A7 7 0 0 1 5 9a7 7 0 0 1 7-7z"/>
              <line x1="9" y1="21" x2="15" y2="21"/>
            </svg>
          {/if}
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

    {#if sourceDocuments.length === 0}
      <p class="empty">No documents yet. Import a PDF to get started.</p>
    {:else}
      <ul>
        {#each sourceDocuments as book (book.id)}
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
    position: relative;
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

  .dry-canvas {
    position: absolute;
    top: 0;
    left: 0;
    box-shadow: none;
    pointer-events: none;
  }

  .wet-canvas {
    position: absolute;
    top: 0;
    left: 0;
    cursor: crosshair;
    touch-action: none;
    box-shadow: none;
  }

  .wet-canvas.erasing {
    cursor: cell;
  }

  .wet-canvas.selecting {
    cursor: default;
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

  .zoom-btn,
  .ink-btn {
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

  .zoom-btn:hover:not(:disabled),
  .ink-btn:hover:not(:disabled) {
    background: #eee !important;
    border-color: #aaa;
  }

  .zoom-btn:disabled,
  .ink-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .zoom-btn svg,
  .ink-btn svg {
    width: 18px;
    height: 18px;
  }

  .tool-btn {
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
    transition: background 0.15s, border-color 0.15s;
  }

  .tool-btn:hover:not(:disabled) {
    background: #eee !important;
    border-color: #aaa;
  }

  .tool-btn.active {
    background: #dce8ff !important;
    border-color: #396cd8;
    color: #396cd8;
  }

  .tool-btn svg {
    width: 18px;
    height: 18px;
  }

  @media (prefers-color-scheme: dark) {
    .tool-btn.active {
      background: #1e3a6e !important;
      border-color: #6b9aff;
      color: #6b9aff;
    }
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

  .ai-debug {
    position: absolute;
    bottom: 1rem;
    right: 1rem;
    background: #fff;
    border: 1px solid #ccc;
    border-radius: 8px;
    box-shadow: 0 4px 24px rgba(0,0,0,0.22);
    padding: 0.5rem;
    z-index: 100;
    max-width: 320px;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .ai-debug img {
    max-width: 100%;
    border-radius: 4px;
    display: block;
    box-shadow: none;
  }

  .ai-debug-close {
    align-self: flex-end;
    background: transparent;
    border: none;
    color: #666;
    font-size: 0.9em;
    padding: 0 0.2em;
    cursor: pointer;
    line-height: 1;
    width: auto;
    height: auto;
    border-radius: 4px;
  }

  .ai-debug-close:hover {
    background: #eee !important;
    color: #000;
  }

  @media (prefers-color-scheme: dark) {
    .ai-debug {
      background: #222;
      border-color: #444;
    }
    .ai-debug-close {
      color: #aaa;
    }
    .ai-debug-close:hover {
      background: #333 !important;
      color: #fff;
    }
  }

  .ai-btn:not(:disabled).active {
    background: #dce8ff !important;
    border-color: #396cd8;
    color: #396cd8;
  }

  @media (prefers-color-scheme: dark) {
    .ai-btn:not(:disabled).active {
      background: #1e3a6e !important;
      border-color: #6b9aff;
      color: #6b9aff;
    }
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .spin {
    animation: spin 0.9s linear infinite;
  }
</style>

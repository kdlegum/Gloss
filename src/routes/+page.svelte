<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";

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
  let currentPage = $state(1);
  let totalPages = $state(0);
  let rendering = $state(false);

  // DB row id for the current (source_document, page) pair; null until resolved
  let currentPageId = $state<number | null>(null);

  // ── Stroke prefetch cache ──
  // 5-entry LRU keyed by page DB id. Evicts the oldest entry when full.
  const STROKE_CACHE_SIZE = 5;
  const strokeCacheOrder: number[] = [];   // front = most recently used
  const strokeCacheData = new Map<number, StrokeOutput[]>();
  const pageIdLookup = new Map<string, number>();

  function cacheGet(pageId: number): StrokeOutput[] | undefined {
    if (!strokeCacheData.has(pageId)) return undefined;
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

  function prefetchPage(bookId: number, pageNum: number) {
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return;
    const key = `${bookId}:${pageNum}`;
    invoke<number>("get_or_create_page", {
      sourceDocumentId: bookId,
      pageNumber: pageNum,
    }).then(pageId => {
      pageIdLookup.set(key, pageId);
      if (strokeCacheData.has(pageId)) return;
      return invoke<StrokeOutput[]>("load_strokes", { pageId }).then(data => {
        cachePut(pageId, data);
      });
    }).catch(() => {});
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
    strokes = loaded.map(s => ({
      id: s.id,
      points: s.points.map(p => ({ x: p.x, y: p.y, pressure: 0.5 })),
      bbox: { minX: s.min_x, minY: s.min_y, maxX: s.max_x, maxY: s.max_y },
      chunkId: s.chunk_id,
    }));
    redoStack = [];
    markDirty();
  }

  // ── Tool mode ──
  type Mode = 'draw' | 'erase' | 'select';
  let mode = $state<Mode>('draw');

  // ── Select state ──
  type Rect = { x: number; y: number; w: number; h: number }; // normalised page space
  let selectOrigin = $state<{ x: number; y: number } | null>(null);
  let selectRect   = $state<Rect | null>(null);
  let selectedStrokes = $state<Set<Stroke>>(new Set());
  let selection = $state<{ x: number; y: number; width: number; height: number } | null>(null);

  // ── Drawing state ──
  type Point = { x: number; y: number; pressure: number };  // normalised page space
  type BBox = { minX: number; minY: number; maxX: number; maxY: number };
  type Stroke = { id: number | null; points: Point[]; bbox: BBox; chunkId: number | null };

  let isDrawing = $state(false);
  let currentStroke: Point[] = [];
  let lastDrawnStrokeIndex = 0;
  let strokes = $state<Stroke[]>([]);
  let redoStack = $state<Stroke[][]>([]);
  let activePointerId: number | null = null;

  const PRESSURE_WIDTH_THRESHOLD = 0.5;

  // ── Infinite canvas state ──
  // camera is $state so the toolbar zoom% and disabled states stay in sync.
  // The render loop reads it directly (no reactive overhead on every frame).
  interface Camera { x: number; y: number; scale: number }
  let camera = $state<Camera>({ x: 0, y: 0, scale: 1 });

  const ZOOM_MIN = 0.3;
  const ZOOM_MAX = 8.0;
  const ZOOM_STEP = 0.15;

  // World-space position and size of the PDF page
  let pageOrigin = { x: 0, y: 0 };
  let pageSize = { w: 0, h: 0 };

  // Canvas elements (all viewport-sized, layered via absolute positioning)
  let canvasContainer = $state<HTMLDivElement>(null!);
  let gridCanvas = $state<HTMLCanvasElement>(null!);
  let pdfCanvas  = $state<HTMLCanvasElement>(null!);
  let dryCanvas  = $state<HTMLCanvasElement>(null!);
  let wetCanvas  = $state<HTMLCanvasElement>(null!);

  let gridCtx: CanvasRenderingContext2D | null = null;
  let pdfCtx:  CanvasRenderingContext2D | null = null;
  let dryCtx:  CanvasRenderingContext2D | null = null;
  let wetCtx:  CanvasRenderingContext2D | null = null;

  // rAF render loop
  let dirty = false;
  let rafId: number | null = null;

  function scheduleRender() {
    if (rafId !== null) return;
    rafId = requestAnimationFrame(() => {
      rafId = null;
      if (dirty) { renderAll(); dirty = false; }
    });
  }

  function markDirty() { dirty = true; scheduleRender(); }

  // ── PDF bitmap cache ──
  // Keyed by "pageNum:scaleKey". Stores rendered ImageBitmaps.
  const PDF_BITMAP_CACHE_MAX = 10;
  const pdfBitmapCacheOrder: string[] = [];
  const pdfBitmapCache = new Map<string, ImageBitmap>();
  let currentPdfBitmap: ImageBitmap | null = null;

  function bitmapCacheGet(key: string): ImageBitmap | undefined {
    const v = pdfBitmapCache.get(key);
    if (!v) return undefined;
    const idx = pdfBitmapCacheOrder.indexOf(key);
    if (idx !== -1) pdfBitmapCacheOrder.splice(idx, 1);
    pdfBitmapCacheOrder.unshift(key);
    return v;
  }

  function bitmapCachePut(key: string, bm: ImageBitmap) {
    if (pdfBitmapCache.has(key)) {
      const idx = pdfBitmapCacheOrder.indexOf(key);
      if (idx !== -1) pdfBitmapCacheOrder.splice(idx, 1);
    } else if (pdfBitmapCacheOrder.length >= PDF_BITMAP_CACHE_MAX) {
      const evict = pdfBitmapCacheOrder.pop()!;
      pdfBitmapCache.get(evict)?.close();
      pdfBitmapCache.delete(evict);
    }
    pdfBitmapCacheOrder.unshift(key);
    pdfBitmapCache.set(key, bm);
  }

  // ── Coordinate transforms ──

  function screenToWorld(sx: number, sy: number): { x: number; y: number } {
    return {
      x: (sx - camera.x) / camera.scale,
      y: (sy - camera.y) / camera.scale,
    };
  }

  /** Convert a screen pointer to normalised page space [0,1]. */
  function pointerToNorm(clientX: number, clientY: number): { x: number; y: number } {
    const rect = canvasContainer.getBoundingClientRect();
    const sx = clientX - rect.left;
    const sy = clientY - rect.top;
    const wx = (sx - camera.x) / camera.scale;
    const wy = (sy - camera.y) / camera.scale;
    return {
      x: (wx - pageOrigin.x) / pageSize.w,
      y: (wy - pageOrigin.y) / pageSize.h,
    };
  }

  /** Normalised page space → world space coordinates. */
  function normToWorld(nx: number, ny: number): { x: number; y: number } {
    return {
      x: pageOrigin.x + nx * pageSize.w,
      y: pageOrigin.y + ny * pageSize.h,
    };
  }

  function getPagePoint(e: PointerEvent): Point {
    const { x, y } = pointerToNorm(e.clientX, e.clientY);
    return { x, y, pressure: e.pressure > 0 ? e.pressure : 0.5 };
  }

  function isPenOrMouse(e: PointerEvent): boolean {
    return e.pointerType === "pen" || e.pointerType === "mouse";
  }

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

  // ── Erase eraser hit radius in world units ──
  const ERASE_RADIUS_WORLD = 6; // ~6px at scale=1

  function eraseAt(normX: number, normY: number) {
    // Convert eraser radius from world px to normalised units
    const rNormX = ERASE_RADIUS_WORLD / (pageSize.w * camera.scale);
    const rNormY = ERASE_RADIUS_WORLD / (pageSize.h * camera.scale);
    const toDelete: Stroke[] = [];
    const toKeep: Stroke[] = [];

    for (const stroke of strokes) {
      const { minX, minY, maxX, maxY } = stroke.bbox;
      if (
        normX < minX - rNormX || normX > maxX + rNormX ||
        normY < minY - rNormY || normY > maxY + rNormY
      ) {
        toKeep.push(stroke);
        continue;
      }
      let hit = false;
      for (const p of stroke.points) {
        const dx = (p.x - normX) * pageSize.w * camera.scale;
        const dy = (p.y - normY) * pageSize.h * camera.scale;
        if (dx * dx + dy * dy <= ERASE_RADIUS_WORLD * ERASE_RADIUS_WORLD) {
          hit = true;
          break;
        }
      }
      if (hit) toDelete.push(stroke);
      else toKeep.push(stroke);
    }

    if (toDelete.length === 0) return;
    strokes = toKeep;
    redoStack = [...redoStack, toDelete];
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of toDelete) {
      if (s.id !== null) invoke("delete_stroke", { strokeId: s.id }).catch(() => {});
    }
    markDirty();
  }

  // ── Touch pan/pinch state ──
  interface TouchPointer { id: number; x: number; y: number }
  let touchPointers: TouchPointer[] = [];

  function touchDist(a: TouchPointer, b: TouchPointer): number {
    const dx = a.x - b.x; const dy = a.y - b.y;
    return Math.sqrt(dx * dx + dy * dy);
  }
  function touchMid(a: TouchPointer, b: TouchPointer): { x: number; y: number } {
    return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  }

  let lastPinchDist = 0;
  let lastPinchMid = { x: 0, y: 0 };

  // ── Pointer events ──

  function onPointerDown(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      touchPointers = [...touchPointers, { id: e.pointerId, x: e.clientX, y: e.clientY }];
      if (touchPointers.length === 2) {
        lastPinchDist = touchDist(touchPointers[0], touchPointers[1]);
        lastPinchMid  = touchMid(touchPointers[0], touchPointers[1]);
      } else if (touchPointers.length === 1) {
        // Initialise pan origin so first move delta is zero
        lastPinchMid = { x: e.clientX, y: e.clientY };
      }
      e.preventDefault();
      return;
    }

    if (!isPenOrMouse(e)) return;
    if (activePointerId !== null) return;

    if (mode === 'erase') {
      activePointerId = e.pointerId;
      wetCanvas.setPointerCapture(e.pointerId);
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      eraseAt(x, y);
      e.preventDefault();
      return;
    }

    if (mode === 'select') {
      activePointerId = e.pointerId;
      wetCanvas.setPointerCapture(e.pointerId);
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      selectOrigin = { x, y };
      selectRect = null;
      selectedStrokes = new Set();
      selection = null;
      markDirty();
      e.preventDefault();
      return;
    }

    activePointerId = e.pointerId;
    wetCanvas.setPointerCapture(e.pointerId);
    isDrawing = true;
    currentStroke = [];
    currentStroke.push(getPagePoint(e));
    lastDrawnStrokeIndex = 0;
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.map(p => p.id === e.pointerId ? { id: e.pointerId, x: e.clientX, y: e.clientY } : p);
      if (touchPointers.length === 2) {
        const [a, b] = touchPointers;
        const newDist = touchDist(a, b);
        const newMid  = touchMid(a, b);
        const zoomFactor = newDist / lastPinchDist;
        const newScale = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, camera.scale * zoomFactor));
        // Keep pinch midpoint stationary in world space
        const wx = (newMid.x - camera.x) / camera.scale;
        const wy = (newMid.y - camera.y) / camera.scale;
        camera.x = newMid.x - wx * newScale;
        camera.y = newMid.y - wy * newScale;
        // Also pan by midpoint movement
        camera.x += newMid.x - lastPinchMid.x;
        camera.y += newMid.y - lastPinchMid.y;
        camera.scale = newScale;
        lastPinchDist = newDist;
        lastPinchMid  = newMid;
      } else if (touchPointers.length === 1 && activePointerId === null) {
        // 1-finger pan
        const cur = touchPointers[0];
        // delta tracked via lastPinchMid reuse
        const dx = cur.x - lastPinchMid.x;
        const dy = cur.y - lastPinchMid.y;
        camera.x += dx;
        camera.y += dy;
        lastPinchMid = { x: cur.x, y: cur.y };
      }
      markDirty();
      e.preventDefault();
      return;
    }

    if (mode === 'erase') {
      if (e.pointerId !== activePointerId) return;
      e.preventDefault();
      const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
      for (const ce of events) {
        const { x, y } = pointerToNorm(ce.clientX, ce.clientY);
        eraseAt(x, y);
      }
      return;
    }

    if (mode === 'select') {
      if (e.pointerId !== activePointerId || !selectOrigin) return;
      e.preventDefault();
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      selectRect = {
        x: Math.min(selectOrigin.x, x),
        y: Math.min(selectOrigin.y, y),
        w: Math.abs(x - selectOrigin.x),
        h: Math.abs(y - selectOrigin.y),
      };
      markDirty();
      return;
    }

    if (!isDrawing || e.pointerId !== activePointerId) return;
    e.preventDefault();
    const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
    for (const ce of events) {
      currentStroke.push(getPagePoint(ce));
    }
    markDirty();
  }

  function onPointerUp(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      if (touchPointers.length === 1) {
        lastPinchMid = { x: touchPointers[0].x, y: touchPointers[0].y };
      }
      e.preventDefault();
      return;
    }

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
      markDirty();
      e.preventDefault();
      return;
    }

    if (e.pointerId !== activePointerId) return;
    if (isDrawing && currentStroke.length >= 2) {
      const completed = currentStroke;
      const stroke: Stroke = { id: null, points: completed, bbox: computeBBox(completed), chunkId: null };
      strokes = [...strokes, stroke];
      redoStack = [];
      if (currentPageId !== null) {
        cacheEvict(currentPageId);
        invoke<number>("save_stroke", {
          pageId: currentPageId,
          stroke: {
            colour: "rgba(30, 80, 220, 0.85)",
            points: completed.map(({ x, y }) => ({ x, y })),
            chunkId: stroke.chunkId,
          },
        }).then(id => { stroke.id = id; }).catch(() => {});
      }
    }
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    lastDrawnStrokeIndex = 0;
    markDirty();
  }

  function onPointerCancel(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      e.preventDefault();
      return;
    }

    if (mode === 'erase' || mode === 'select') {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      selectOrigin = null;
      selectRect = null;
      markDirty();
      return;
    }

    if (e.pointerId !== activePointerId) return;
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    lastDrawnStrokeIndex = 0;
    markDirty();
  }

  // ── Render loop ──

  function renderAll() {
    renderGrid();
    renderPdf();
    renderDryStrokes();
    renderWetLayer();
  }

  function renderGrid() {
    if (!gridCtx || !gridCanvas) return;
    const ctx = gridCtx;
    const w = gridCanvas.width;
    const h = gridCanvas.height;
    const dpr = window.devicePixelRatio || 1;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const GRID_WORLD = 40;
    const DOT_R = 1.5;
    const spacing = GRID_WORLD * camera.scale;
    if (spacing < 8 || spacing > 300) return;

    const startW = screenToWorld(0, 0);
    const startX = Math.floor(startW.x / GRID_WORLD) * GRID_WORLD;
    const startY = Math.floor(startW.y / GRID_WORLD) * GRID_WORLD;

    ctx.fillStyle = 'rgba(0,0,0,0.18)';
    for (let wx = startX; ; wx += GRID_WORLD) {
      const sx = (wx * camera.scale + camera.x) * dpr;
      if (sx > w + DOT_R * dpr) break;
      if (sx < -DOT_R * dpr) continue;
      for (let wy = startY; ; wy += GRID_WORLD) {
        const sy = (wy * camera.scale + camera.y) * dpr;
        if (sy > h + DOT_R * dpr) break;
        if (sy < -DOT_R * dpr) continue;
        ctx.beginPath();
        ctx.arc(sx, sy, DOT_R, 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }

  function renderPdf() {
    if (!pdfCtx || !pdfCanvas || !currentPdfBitmap) return;
    const ctx = pdfCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);
    ctx.clearRect(
      -camera.x / camera.scale, -camera.y / camera.scale,
      pdfCanvas.width / (camera.scale * dpr), pdfCanvas.height / (camera.scale * dpr),
    );
    ctx.drawImage(currentPdfBitmap, pageOrigin.x, pageOrigin.y, pageSize.w, pageSize.h);
  }

  function renderDryStrokes() {
    if (!dryCtx || !dryCanvas) return;
    const ctx = dryCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);
    ctx.clearRect(
      -camera.x / camera.scale, -camera.y / camera.scale,
      dryCanvas.width / (camera.scale * dpr), dryCanvas.height / (camera.scale * dpr),
    );

    // Viewport culling: compute visible rect in normalised page space
    const visMinX = (-camera.x / camera.scale - pageOrigin.x) / pageSize.w;
    const visMinY = (-camera.y / camera.scale - pageOrigin.y) / pageSize.h;
    const visMaxX = visMinX + (canvasContainer.clientWidth  / camera.scale) / pageSize.w;
    const visMaxY = visMinY + (canvasContainer.clientHeight / camera.scale) / pageSize.h;

    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "rgb(30, 80, 220)";

    for (const stroke of strokes) {
      if (stroke.points.length < 2) continue;
      if (
        stroke.bbox.maxX < visMinX || stroke.bbox.minX > visMaxX ||
        stroke.bbox.maxY < visMinY || stroke.bbox.minY > visMaxY
      ) continue;
      drawStrokePoints(ctx, stroke.points, 0);
    }

    // Draw selection highlight on dry canvas
    if (selectedStrokes.size > 0) {
      ctx.save();
      ctx.strokeStyle = "rgba(255, 140, 0, 0.9)";
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      for (const stroke of selectedStrokes) {
        if (stroke.points.length < 2) continue;
        drawStrokePoints(ctx, stroke.points, 0);
      }
      ctx.restore();
    }
  }

  function renderWetLayer() {
    if (!wetCtx || !wetCanvas) return;
    const ctx = wetCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);

    // Wet stroke
    if (isDrawing && currentStroke.length >= 2) {
      const pts = currentStroke;
      if (lastDrawnStrokeIndex < 2) {
        ctx.clearRect(
          -camera.x / camera.scale, -camera.y / camera.scale,
          wetCanvas.width / (camera.scale * dpr), wetCanvas.height / (camera.scale * dpr),
        );
        drawStrokePoints(ctx, pts, 0);
      } else {
        drawStrokePoints(ctx, pts, Math.max(0, lastDrawnStrokeIndex - 1));
      }
      lastDrawnStrokeIndex = pts.length;
    } else if (selectRect) {
      // Rubber-band selection rectangle
      ctx.clearRect(
        -camera.x / camera.scale, -camera.y / camera.scale,
        wetCanvas.width / (camera.scale * dpr), wetCanvas.height / (camera.scale * dpr),
      );
      const tl = normToWorld(selectRect.x, selectRect.y);
      const br = normToWorld(selectRect.x + selectRect.w, selectRect.y + selectRect.h);
      ctx.save();
      ctx.strokeStyle = "rgba(57, 108, 216, 0.9)";
      ctx.lineWidth = 1.5 / camera.scale;
      ctx.setLineDash([5 / camera.scale, 4 / camera.scale]);
      ctx.strokeRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.fillStyle = "rgba(57, 108, 216, 0.08)";
      ctx.fillRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.restore();
    } else {
      ctx.clearRect(
        -camera.x / camera.scale, -camera.y / camera.scale,
        wetCanvas.width / (camera.scale * dpr), wetCanvas.height / (camera.scale * dpr),
      );
    }
  }

  /**
   * Draw stroke points in world space. ctx must have camera setTransform applied.
   * Points are in normalised page space; converted to world here.
   */
  function drawStrokePoints(ctx: CanvasRenderingContext2D, pts: Point[], startIdx: number) {
    if (pts.length - startIdx < 2) return;
    const slice = pts.slice(startIdx);
    const wc = slice.map(p => normToWorld(p.x, p.y));

    ctx.beginPath();
    ctx.moveTo(wc[0].x, wc[0].y);
    ctx.lineWidth = (1 + slice[0].pressure * 5) / camera.scale;

    for (let i = 1; i < wc.length - 1; i++) {
      const width = (1 + slice[i].pressure * 5) / camera.scale;
      if (Math.abs(ctx.lineWidth - width) > PRESSURE_WIDTH_THRESHOLD / camera.scale) {
        ctx.stroke();
        ctx.beginPath();
        const midPrev = { x: (wc[i - 1].x + wc[i].x) / 2, y: (wc[i - 1].y + wc[i].y) / 2 };
        ctx.moveTo(midPrev.x, midPrev.y);
        ctx.lineWidth = width;
      }
      const mid = { x: (wc[i].x + wc[i + 1].x) / 2, y: (wc[i].y + wc[i + 1].y) / 2 };
      ctx.quadraticCurveTo(wc[i].x, wc[i].y, mid.x, mid.y);
    }
    const last = wc.length - 1;
    ctx.quadraticCurveTo(wc[last - 1].x, wc[last - 1].y, wc[last].x, wc[last].y);
    ctx.stroke();
  }

  // ── Hit test / selection helpers ──

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

  function hitTestStrokes(rect: Rect): Set<Stroke> {
    const r2 = rect.x + rect.w, b2 = rect.y + rect.h;
    const hit = new Set<Stroke>();
    for (const stroke of strokes) {
      if (stroke.bbox.maxX < rect.x || stroke.bbox.minX > r2 ||
          stroke.bbox.maxY < rect.y || stroke.bbox.minY > b2) continue;
      for (const p of stroke.points) {
        if (p.x >= rect.x && p.x <= r2 && p.y >= rect.y && p.y <= b2) {
          hit.add(stroke);
          break;
        }
      }
    }
    return hit;
  }

  // ── Canvas setup / resize ──

  function setupCanvases() {
    if (!canvasContainer || !gridCanvas || !pdfCanvas || !dryCanvas || !wetCanvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvasContainer.clientWidth;
    const h = canvasContainer.clientHeight;
    for (const c of [gridCanvas, pdfCanvas, dryCanvas, wetCanvas]) {
      c.width  = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    gridCtx = gridCanvas.getContext("2d");
    pdfCtx  = pdfCanvas.getContext("2d");
    dryCtx  = dryCanvas.getContext("2d")!;
    wetCtx  = wetCanvas.getContext("2d")!;
    if (dryCtx) { dryCtx.lineCap = "round"; dryCtx.lineJoin = "round"; }
    if (wetCtx) { wetCtx.lineCap = "round"; wetCtx.lineJoin = "round"; wetCtx.strokeStyle = "rgb(30, 80, 220)"; }
    markDirty();
  }

  let containerResizeObserver: ResizeObserver | null = null;

  function observeContainerResize(node: HTMLElement) {
    containerResizeObserver?.disconnect();
    containerResizeObserver = new ResizeObserver(() => {
      setupCanvases();
    });
    containerResizeObserver.observe(node);
  }

  // ── Camera helpers ──

  function zoomAt(screenX: number, screenY: number, factor: number) {
    const newScale = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, camera.scale * factor));
    const wx = (screenX - camera.x) / camera.scale;
    const wy = (screenY - camera.y) / camera.scale;
    camera.x = screenX - wx * newScale;
    camera.y = screenY - wy * newScale;
    camera.scale = newScale;
    markDirty();
  }

  function centreOnPage() {
    if (!pageSize.w || !canvasContainer) return;
    const vw = canvasContainer.clientWidth;
    camera.x = (vw - pageSize.w * camera.scale) / 2;
    camera.y = 0;
    markDirty();
  }

  // ── PDF loading ──

  async function loadPdfPage(pageNum: number) {
    if (!selectedBook || !canvasContainer) return;
    rendering = true;
    try {
      const containerW = canvasContainer.clientWidth;
      const pageDisplayW = containerW - 80; // 40px margin each side
      const dpr = window.devicePixelRatio || 1;
      // pdfium scale = pixels per PDF point. We aim for pixelWidth wide output.
      // 595pt ≈ standard A4 width; scale is approximate for non-A4 pages, but the
      // bitmap is always drawn to fill pageDisplayW in world space so it looks correct
      // regardless of the actual page width in points.
      const pixelWidth = Math.round(pageDisplayW * dpr);
      const approxScale = pixelWidth / 595;
      const cacheKey = `${pageNum}:${pixelWidth}`;
      let bitmap = bitmapCacheGet(cacheKey);

      if (!bitmap) {
        const buf: ArrayBuffer = await invoke("render_pdf_page", {
          relativePath: selectedBook.file_path,
          pageNumber: pageNum - 1,
          scale: approxScale,
        });
        const header = new DataView(buf);
        const width = header.getUint32(0, true);
        const height = header.getUint32(4, true);
        const rgba = new Uint8ClampedArray(buf, 8);
        const imageData = new ImageData(rgba, width, height);
        bitmap = await createImageBitmap(imageData);
        bitmapCachePut(cacheKey, bitmap);
      }

      currentPdfBitmap = bitmap;
      // pageSize in world units: display width fixed, height from bitmap aspect ratio
      pageSize = { w: pageDisplayW, h: pageDisplayW * (bitmap.height / bitmap.width) };
      centreOnPage();
      markDirty();
    } finally {
      rendering = false;
    }
  }

  // ── Page navigation ──

  function savedPageKey(bookId: number) { return `gloss_page_${bookId}`; }
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
      if (e !== "cancelled") error = String(e);
    } finally {
      importing = false;
    }
  }

  async function openBook(book: SourceDocument) {
    error = null;
    selectedBook = book;
    currentPage = loadSavedPage(book.id);
    currentPageId = null;
    totalPages = 0;
    strokes = [];
    redoStack = [];
    selectedStrokes = new Set();
    selection = null;
    currentPdfBitmap = null;

    // Get page count from backend
    try {
      totalPages = await invoke<number>("get_page_count", { relativePath: book.file_path });
      if (currentPage > totalPages) currentPage = 1;
      await loadPdfPage(currentPage);
      resolvePageId(book.id, currentPage);
    } catch (e) {
      error = String(e);
    }
  }

  async function goToPage(pageNum: number) {
    if (rendering) return;
    const clamped = Math.max(1, Math.min(totalPages, pageNum));
    if (clamped === currentPage && currentPdfBitmap) return;
    currentPage = clamped;
    currentPageId = null;
    strokes = [];
    redoStack = [];
    selectedStrokes = new Set();
    selection = null;
    if (selectedBook) saveCurrentPage(selectedBook.id, currentPage);
    await loadPdfPage(currentPage);
    if (selectedBook) await resolvePageId(selectedBook.id, currentPage);
  }

  async function prevPage() { await goToPage(currentPage - 1); }
  async function nextPage() { await goToPage(currentPage + 1); }

  function closeViewer() {
    selectedBook = null;
    currentPage = 1;
    currentPageId = null;
    totalPages = 0;
    strokes = [];
    redoStack = [];
    selectedStrokes = new Set();
    selection = null;
    currentPdfBitmap = null;
  }

  // ── Undo / Redo ──

  async function undoStroke() {
    if (strokes.length === 0) return;
    const removed = strokes[strokes.length - 1];
    strokes = strokes.slice(0, -1);
    redoStack = [...redoStack, [removed]];
    if (currentPageId !== null) cacheEvict(currentPageId);
    if (removed.id !== null) invoke("delete_stroke", { strokeId: removed.id }).catch(() => {});
    markDirty();
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
        }).then(id => { restored.id = id; }).catch(() => {});
      }
    }
    markDirty();
  }

  // ── Delete selected ──

  async function deleteSelected() {
    if (selectedStrokes.size === 0) return;
    const deleted = [...selectedStrokes];
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of deleted) {
      if (s.id !== null) invoke("delete_stroke", { strokeId: s.id }).catch(() => {});
    }
    redoStack = [...redoStack, deleted];
    strokes = strokes.filter(s => !selectedStrokes.has(s));
    selectedStrokes = new Set();
    selection = null;
    markDirty();
  }

  // ── AI rasterisation ──
  let aiWorking = $state(false);
  let aiDebugImage = $state<string | null>(null);

  async function rasteriseSelection(): Promise<string> {
    if (!selection || !currentPdfBitmap) throw new Error("Nothing selected");
    // PDF bitmap dimensions
    const bw = currentPdfBitmap.width;
    const bh = currentPdfBitmap.height;
    const sx = selection.x * bw;
    const sy = selection.y * bh;
    const sw = selection.width  * bw;
    const sh = selection.height * bh;

    const offscreen = new OffscreenCanvas(Math.round(sw), Math.round(sh));
    const ctx = offscreen.getContext("2d")!;
    ctx.drawImage(currentPdfBitmap, sx, sy, sw, sh, 0, 0, sw, sh);

    // Also composite dry strokes for the selected region
    // Create a tiny canvas with the camera transform to render strokes into selection bbox
    if (dryCtx && dryCanvas) {
      // Draw dry canvas region corresponding to selection
      // Selection bbox in world coords
      const wTL = normToWorld(selection.x, selection.y);
      const wBR = normToWorld(selection.x + selection.width, selection.y + selection.height);
      const worldW = wBR.x - wTL.x;
      const worldH = wBR.y - wTL.y;
      // Scale to fill sw × sh
      const renderS = sw / worldW;
      const inkCanvas = new OffscreenCanvas(Math.round(sw), Math.round(sh));
      const inkCtx = inkCanvas.getContext("2d")!;
      inkCtx.setTransform(renderS, 0, 0, renderS, -wTL.x * renderS, -wTL.y * renderS);
      inkCtx.lineCap = "round";
      inkCtx.lineJoin = "round";
      inkCtx.strokeStyle = "rgb(30, 80, 220)";
      for (const stroke of strokes) {
        if (stroke.points.length < 2) continue;
        if (stroke.bbox.maxX < selection.x || stroke.bbox.minX > selection.x + selection.width ||
            stroke.bbox.maxY < selection.y || stroke.bbox.minY > selection.y + selection.height) continue;
        drawStrokePoints(inkCtx as unknown as CanvasRenderingContext2D, stroke.points, 0);
      }
      ctx.drawImage(inkCanvas, 0, 0);
    }

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

  // ── Wheel handler ──

  function handleWheel(e: WheelEvent) {
    if (!selectedBook) return;
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const rect = canvasContainer.getBoundingClientRect();
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      zoomAt(e.clientX - rect.left, e.clientY - rect.top, factor);
    } else {
      camera.x -= e.deltaX;
      camera.y -= e.deltaY;
      markDirty();
    }
  }

  // ── Keyboard ──

  async function handleKeydown(e: KeyboardEvent) {
    if (!selectedBook) return;
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
      if (canvasContainer) zoomAt(canvasContainer.clientWidth / 2, canvasContainer.clientHeight / 2, 1 + ZOOM_STEP);
    } else if (e.key === "-") {
      e.preventDefault();
      if (canvasContainer) zoomAt(canvasContainer.clientWidth / 2, canvasContainer.clientHeight / 2, 1 - ZOOM_STEP);
    } else if (e.key === "0" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      centreOnPage();
    }
  }

  onMount(() => {
    loadSourceDocuments();
    window.addEventListener("keydown", handleKeydown);
    window.addEventListener("wheel", handleWheel, { passive: false });
  });

  onDestroy(() => {
    window.removeEventListener("keydown", handleKeydown);
    window.removeEventListener("wheel", handleWheel);
    containerResizeObserver?.disconnect();
  });

  let zoomPercent = $derived(Math.round(camera.scale * 100));
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

      <!-- Infinite canvas -->
      <div
        class="infinite-canvas"
        bind:this={canvasContainer}
        class:mode-erase={mode === 'erase'}
        class:mode-select={mode === 'select'}
        use:observeContainerResize
      >
        <canvas bind:this={gridCanvas} class="layer layer-grid"></canvas>
        <canvas bind:this={pdfCanvas}  class="layer layer-pdf"></canvas>
        <canvas bind:this={dryCanvas}  class="layer layer-dry"></canvas>
        <canvas
          bind:this={wetCanvas}
          class="layer layer-wet"
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
          {currentPage}{totalPages > 0 ? ` / ${totalPages}` : ''}
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
        <button class="ink-btn" onclick={undoStroke} disabled={strokes.length === 0} aria-label="Undo stroke">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 14 4 9l5-5"/>
            <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>
          </svg>
        </button>

        <!-- Redo -->
        <button class="ink-btn" onclick={redoStroke} disabled={redoStack.length === 0} aria-label="Redo stroke">
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
          onclick={() => { mode = 'draw'; selectedStrokes = new Set(); selection = null; markDirty(); }}
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
          onclick={() => { mode = 'erase'; selectedStrokes = new Set(); selection = null; markDirty(); }}
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
          onclick={() => { mode = 'select'; selectedStrokes = new Set(); selection = null; markDirty(); }}
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
          onclick={() => { if (canvasContainer) zoomAt(canvasContainer.clientWidth / 2, canvasContainer.clientHeight / 2, 1 - ZOOM_STEP); }}
          disabled={camera.scale <= ZOOM_MIN}
          aria-label="Zoom out"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8"/>
            <line x1="21" y1="21" x2="16.65" y2="16.65"/>
            <line x1="8" y1="11" x2="14" y2="11"/>
          </svg>
        </button>

        <button class="zoom-level" onclick={centreOnPage} title="Centre page (Ctrl+0)" aria-label="Centre page">
          {zoomPercent}%
        </button>

        <!-- Zoom in -->
        <button
          class="zoom-btn"
          onclick={() => { if (canvasContainer) zoomAt(canvasContainer.clientWidth / 2, canvasContainer.clientHeight / 2, 1 + ZOOM_STEP); }}
          disabled={camera.scale >= ZOOM_MAX}
          aria-label="Zoom in"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8"/>
            <line x1="21" y1="21" x2="16.65" y2="16.65"/>
            <line x1="11" y1="8" x2="11" y2="14"/>
            <line x1="8" y1="11" x2="14" y2="11"/>
          </svg>
        </button>

        <!-- Centre page -->
        <button class="zoom-btn" onclick={centreOnPage} title="Centre page" aria-label="Centre page">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="3" y="3" width="18" height="18" rx="2"/>
            <line x1="12" y1="8" x2="12" y2="16"/>
            <line x1="8" y1="12" x2="16" y2="12"/>
          </svg>
        </button>
      </div>
    </div>

  {:else}
    <!-- Library view -->
    <div class="library">
      <div class="library-header">
        <h1>Gloss</h1>
        <button onclick={importPdf} disabled={importing} class="import-btn">
          {importing ? "Importing…" : "Import PDF"}
        </button>
      </div>

      {#if error}
        <p class="error">{error}</p>
      {/if}

      {#if sourceDocuments.length === 0}
        <p class="empty">No books yet. Import a PDF to get started.</p>
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
    </div>
  {/if}
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
  }

  :global(body) {
    margin: 0;
    font-family: system-ui, sans-serif;
    font-size: 15px;
    background: #fff;
    color: #1a1a1a;
  }

  :global(button) {
    font: inherit;
    cursor: pointer;
    border: none;
  }

  :global(button:disabled) {
    cursor: not-allowed;
  }

  main {
    min-height: 100vh;
  }

  /* ── Library ── */
  .library {
    max-width: 640px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
  }

  .library-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.5rem;
  }

  h1 {
    margin: 0;
    font-size: 1.6em;
    font-weight: 700;
    letter-spacing: -0.02em;
  }

  .import-btn {
    padding: 0.45rem 1rem;
    background: #1a1a1a;
    color: #fff;
    border-radius: 6px;
    font-size: 0.9em;
    transition: background 0.15s;
  }

  .import-btn:hover:not(:disabled) {
    background: #333;
  }

  .import-btn:disabled {
    background: #888;
  }

  .error {
    color: #c00;
    font-size: 0.9em;
    margin: 0.5rem 0;
  }

  .empty {
    color: #888;
    margin-top: 1.5rem;
  }

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
    transition: background 0.15s;
  }

  .book-item:hover {
    background: #e0e0e0 !important;
  }

  .title { font-weight: 600; }
  .path  { font-size: 0.8em; color: #666; font-family: monospace; }

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

  .back-btn:hover { background: #eee !important; }
  .back-btn svg  { width: 18px; height: 18px; }

  /* ── Infinite canvas ── */
  .infinite-canvas {
    position: relative;
    flex: 1;
    overflow: hidden;
    min-height: 0;
    background: #e8e8e8;
    cursor: crosshair;
    touch-action: none;
  }

  .infinite-canvas.mode-erase { cursor: cell; }
  .infinite-canvas.mode-select { cursor: default; }

  .layer {
    position: absolute;
    top: 0;
    left: 0;
    /* width/height set in JS to clientWidth/clientHeight in CSS px,
       but the bitmap is dpr-scaled — keep CSS size at 100% */
    width: 100%;
    height: 100%;
  }

  .layer-grid { pointer-events: none; }
  .layer-pdf  { pointer-events: none; }
  .layer-dry  { pointer-events: none; }
  /* .layer-wet receives all pointer events — no overrides needed */

  /* ── Controls bar ── */
  .controls {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-top: 1px solid #ddd;
    background: #f6f6f6;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  .divider {
    width: 1px;
    height: 22px;
    background: #ddd;
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

  .chevron:hover:not(:disabled) { background: #eee !important; border-color: #aaa; }
  .chevron:disabled { opacity: 0.3; }
  .chevron svg { width: 20px; height: 20px; }

  .page-indicator {
    font-size: 0.9em;
    color: #666;
    min-width: 3ch;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .ink-btn, .tool-btn, .zoom-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    padding: 0;
    background: transparent;
    color: inherit;
    border-radius: 6px;
    transition: background 0.12s;
  }

  .ink-btn:hover:not(:disabled),
  .tool-btn:hover:not(:disabled),
  .zoom-btn:hover:not(:disabled) { background: #e4e4e4 !important; }

  .ink-btn:disabled, .zoom-btn:disabled { opacity: 0.3; }

  .ink-btn svg, .tool-btn svg, .zoom-btn svg { width: 20px; height: 20px; }

  .tool-btn.active {
    background: #e0e8ff !important;
    color: #2040a0;
  }

  .zoom-level {
    font-size: 0.82em;
    font-variant-numeric: tabular-nums;
    color: #444;
    padding: 0.3rem 0.5rem;
    background: transparent;
    border-radius: 5px;
    min-width: 4.5ch;
    text-align: center;
    transition: background 0.12s;
  }

  .zoom-level:hover { background: #e4e4e4 !important; }

  .ai-btn { color: #2a6; }
  .ai-btn.active { background: #e0ffe8 !important; }

  @keyframes spin { to { transform: rotate(360deg); } }
  .spin { animation: spin 0.9s linear infinite; }

  /* ── AI debug overlay ── */
  .ai-debug {
    position: absolute;
    top: 60px;
    right: 16px;
    background: #fff;
    border: 1px solid #ccc;
    border-radius: 8px;
    padding: 0.5rem;
    box-shadow: 0 4px 20px rgba(0,0,0,0.15);
    z-index: 100;
    max-width: 360px;
  }

  .ai-debug img { display: block; max-width: 100%; border-radius: 4px; }

  .ai-debug-close {
    position: absolute;
    top: 6px;
    right: 8px;
    background: transparent;
    font-size: 0.85em;
    color: #888;
    padding: 2px 4px;
  }

  .ai-debug-close:hover { color: #000; }
</style>

<script lang="ts">
  import "katex/dist/katex.min.css";
  import { invoke } from "@tauri-apps/api/core";
  import type { ViewerBatchSettingsState as ParentViewerBatchSettingsState } from "$lib/app/types";
  import { appLogError, appLogInfo, appLogWarn, formatLogError } from "$lib/app/log";
  import {
    CHUNK_COLOURS as SHARED_CHUNK_COLOURS,
    FALLBACK_CHUNK_COLOUR,
  } from "$lib/chunk/colours";
  import {
    type Point,
    type Stroke,
    type Rect,
    type ShapeKind as InkShapeKind,
    DEFAULT_PEN_COLOUR,
    DEFAULT_PEN_THICKNESS,
    computeBBox,
    buildShapeStrokes,
    shapeDragLengthWorld,
    drawStrokePoints as drawStrokePointsFn,
  } from "$lib/ink";
  import GraphComposerModal from "$lib/GraphComposerModal.svelte";
  import {
    drawGraphCard,
    graphSpecToFence,
    normaliseGraphSpec,
    pointsFromJson,
    pointsToJson,
    type GraphSpec,
  } from "$lib/graph";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { attachConsole } from "@tauri-apps/plugin-log";
  import { renderChunkBodyHtml, renderChunkBodyPreviewHtml, type ResolvedReference } from "$lib/chunkBody";
  import ChunkPeek from "$lib/ChunkPeek.svelte";
  import ViewerHeader from "$lib/viewer/ViewerHeader.svelte";
  import BatchChunkProgressCard from "$lib/viewer/BatchChunkProgressCard.svelte";
  import ViewerAiPanel from "$lib/viewer/ViewerAiPanel.svelte";
  import { onMount, onDestroy, tick } from "svelte";
  import { fade } from "svelte/transition";

  let {
    book: initialBook,
    providedAiTaskSettings,
    providedAiSettings,
    openAiSettings,
    onClose,
    onBookUpdated,
    onAiTaskSettingsChange,
    onViewerBatchStateChange,
  }: {
    book: SourceDocument;
    providedAiTaskSettings: AiTaskSettings;
    providedAiSettings: AiSettingsState | null;
    openAiSettings: () => void;
    onClose: () => void;
    onBookUpdated: (book: SourceDocument) => void;
    onAiTaskSettingsChange: (settings: AiTaskSettings) => void;
    onViewerBatchStateChange: (state: ParentViewerBatchSettingsState | null) => void;
  } = $props();

  type DocumentMode = "textbook" | "past_paper" | "notebook";

  interface SourceDocument {
    id: number;
    title: string;
    file_path: string;
    page_count: number | null;
    document_mode: DocumentMode;
    instruction_page_start: number | null;
    instruction_page_end: number | null;
  }

  interface PastPaperInstructionContextDebug {
    source_document_id: number;
    chunking_status: string;
    instruction_page_start: number | null;
    instruction_page_end: number | null;
    expected_instruction_pages: number[];
    extracted_instruction_pages: number[];
    missing_instruction_pages: number[];
    instruction_block_count: number;
    instruction_transcribed_block_count: number;
    preview_text: string;
  }

  interface AiSettingsState {
    running_on_android: boolean;
    setup_complete: boolean;
    openai_api_key_set: boolean;
    gemini_api_key_set: boolean;
    deepseek_api_key_set: boolean;
    zai_api_key_set: boolean;
  }

  interface RenderedPdfBitmap {
    bitmap: ImageBitmap;
    bitmapWidth: number;
    bitmapHeight: number;
    pageWidthPoints: number;
    pageHeightPoints: number;
  }

  interface NotebookCanvasOutput {
    page_id: number;
    surface_id: number;
    canvas_count: number;
  }

  type ChunkingProvider = "ollama" | "openai" | "gemini" | "deepseek";
  type ChatProvider = "ollama" | "openai" | "gemini" | "deepseek";
  type VisionProvider = "ollama" | "openai" | "gemini" | "zai";
  type AnyProvider = ChunkingProvider | VisionProvider;
  type AiTask = "chunking" | "chat" | "vision";

  interface AiTaskSettings {
    chunking: { provider: ChunkingProvider; model: string };
    chat: { provider: ChatProvider; model: string };
    vision: { provider: VisionProvider; model: string };
  }

  interface ModelOption {
    value: string;
    label: string;
    legacy?: boolean;
  }

  interface EnsureChunkingRangeResult {
    requested_start_page: number;
    requested_end_page: number;
    started_pages: number;
    started_page_numbers: number[];
    skipped_pages: number;
  }

  interface BatchChunkProgressState {
    docId: number;
    startPage: number;
    endPage: number;
    providerLabel: string;
    modelLabel: string;
    totalPages: number;
    completedPages: number;
    failedPages: number;
    skippedPages: number;
    startedPages: number[];
    extractedPages: number[];
    groupedPages: number[];
    failedPageNumbers: number[];
    statusMessage: string;
    messages: string[];
    finished: boolean;
  }

  const LEGACY_CHUNKING_PROVIDER_STORAGE_KEY = "gloss_chunking_provider";
  const AI_TASK_SETTINGS_STORAGE_KEY = "gloss_ai_task_settings_v1";
  const PAPER_SETTINGS_STORAGE_KEY = "gloss_paper_settings_v1";
  const CUSTOM_MODEL_VALUE = "__custom__";

  const CHUNKING_PROVIDER_OPTIONS: Array<{ value: ChunkingProvider; label: string; short: string }> = [
    { value: "ollama", label: "Ollama", short: "OL" },
    { value: "openai", label: "OpenAI", short: "OA" },
    { value: "gemini", label: "Gemini", short: "GM" },
    { value: "deepseek", label: "DeepSeek", short: "DS" },
  ];

  const CHAT_PROVIDER_OPTIONS: Array<{ value: ChatProvider; label: string; short: string }> = [
    { value: "ollama", label: "Ollama", short: "OL" },
    { value: "openai", label: "OpenAI", short: "OA" },
    { value: "gemini", label: "Gemini", short: "GM" },
    { value: "deepseek", label: "DeepSeek", short: "DS" },
  ];

  const VISION_PROVIDER_OPTIONS: Array<{ value: VisionProvider; label: string; short: string }> = [
    { value: "ollama", label: "Ollama", short: "OL" },
    { value: "openai", label: "OpenAI", short: "OA" },
    { value: "gemini", label: "Gemini", short: "GM" },
    { value: "zai", label: "Z.AI", short: "ZA" },
  ];

  const CHUNKING_MODEL_OPTIONS: Record<ChunkingProvider, ModelOption[]> = {
    ollama: [
      { value: "", label: "Auto (server default)" },
    ],
    openai: [
      { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
      { value: "gpt-5.4", label: "gpt-5.4" },
      { value: "gpt-5-nano", label: "gpt-5-nano" },
    ],
    gemini: [
      { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
      { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
    ],
    deepseek: [
      { value: "deepseek-v4-flash", label: "deepseek-v4-flash" },
      { value: "deepseek-v4-pro", label: "deepseek-v4-pro" },
      { value: "deepseek-reasoner", label: "deepseek-reasoner", legacy: true },
    ],
  };

  const CHAT_MODEL_OPTIONS: Record<ChatProvider, ModelOption[]> = {
    ollama: [
      { value: "", label: "Auto (server default)" },
    ],
    openai: [
      { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
      { value: "gpt-5.4", label: "gpt-5.4" },
      { value: "gpt-5-nano", label: "gpt-5-nano" },
    ],
    gemini: [
      { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
      { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
    ],
    deepseek: [
      { value: "deepseek-v4-flash", label: "deepseek-v4-flash" },
      { value: "deepseek-v4-pro", label: "deepseek-v4-pro" },
      { value: "deepseek-reasoner", label: "deepseek-reasoner", legacy: true },
    ],
  };

  const VISION_MODEL_OPTIONS: Record<VisionProvider, ModelOption[]> = {
    ollama: [
      { value: "", label: "Auto (server default)" },
    ],
    openai: [
      { value: "gpt-5.4-mini", label: "gpt-5.4-mini" },
      { value: "gpt-5.4", label: "gpt-5.4" },
      { value: "gpt-5-nano", label: "gpt-5-nano" },
    ],
    gemini: [
      { value: "gemini-2.5-flash", label: "gemini-2.5-flash" },
      { value: "gemini-2.5-flash-lite", label: "gemini-2.5-flash-lite" },
    ],
    zai: [
      { value: "glm-ocr", label: "glm-ocr" },
    ],
  };

  function isChunkingProvider(value: string | null): value is ChunkingProvider {
    return value === "ollama" || value === "openai" || value === "gemini" || value === "deepseek";
  }

  function isChatProvider(value: string | null): value is ChatProvider {
    return value === "ollama" || value === "openai" || value === "gemini" || value === "deepseek";
  }

  function isVisionProvider(value: string | null): value is VisionProvider {
    return value === "ollama" || value === "openai" || value === "gemini" || value === "zai";
  }

  function getProviderLabel(provider: AnyProvider): string {
    switch (provider) {
      case "ollama":
        return "Ollama";
      case "openai":
        return "OpenAI";
      case "gemini":
        return "Gemini";
      case "deepseek":
        return "DeepSeek";
      case "zai":
        return "Z.AI";
    }
  }

  function getChunkingProviderLabel(provider: ChunkingProvider) {
    return getProviderLabel(provider);
  }

  function getDefaultModelForTask(task: AiTask, provider: AnyProvider): string {
    if (task === "chunking") {
      const model = CHUNKING_MODEL_OPTIONS[provider as ChunkingProvider]?.[0]?.value;
      return model ?? "";
    }
    if (task === "chat") {
      const model = CHAT_MODEL_OPTIONS[provider as ChatProvider]?.[0]?.value;
      return model ?? "";
    }
    const model = VISION_MODEL_OPTIONS[provider as VisionProvider]?.[0]?.value;
    return model ?? "";
  }

  function defaultAiTaskSettings(): AiTaskSettings {
    return {
      chunking: { provider: "deepseek", model: "deepseek-v4-flash" },
      chat: { provider: "gemini", model: "gemini-2.5-flash" },
      vision: { provider: "zai", model: "glm-ocr" },
    };
  }

  let sourceDocuments = $state<SourceDocument[]>([]);
  let importing = $state(false);
  let pendingDeleteSourceDocumentId = $state<number | null>(null);
  let deletingSourceDocumentId = $state<number | null>(null);
  let error = $state<string | null>(null);
  let detachLogConsole: (() => void) | null = null;
  let aiTaskSettings = $state<AiTaskSettings>(defaultAiTaskSettings());
  let currentChunkingStatus = $state("pending");
  let currentPageChunkingActive = $state(false);
  let reChunkingPage = $state(false);
  let aiSettings = $state<AiSettingsState | null>(null);
  let showAiKeySheet = $state(false);
  let aiKeySheetFirstRun = $state(false);
  let aiSettingsSaving = $state(false);
  let aiSettingsError = $state<string | null>(null);
  let batchChunkStartInput = $state("1");
  let batchChunkEndInput = $state("1");
  let batchChunkSkipChunkedPages = $state(true);
  let pastPaperInstructionPagesEnabled = $state(true);
  let pastPaperInstructionStartInput = $state("1");
  let pastPaperInstructionEndInput = $state("1");
  let pastPaperInstructionSaving = $state(false);
  let pastPaperInstructionError = $state<string | null>(null);
  let pastPaperInstructionFeedback = $state<string | null>(null);
  let pastPaperInstructionCheckLoading = $state(false);
  let pastPaperInstructionCheckError = $state<string | null>(null);
  let pastPaperInstructionCheckFeedback = $state<string | null>(null);
  let pastPaperInstructionCheckPreview = $state<string | null>(null);
  let batchChunkStarting = $state(false);
  let batchChunkError = $state<string | null>(null);
  let batchChunkFeedback = $state<string | null>(null);
  let batchChunkProgress = $state<BatchChunkProgressState | null>(null);
  let customModelMode = $state<{ chunking: boolean; chat: boolean; vision: boolean }>({
    chunking: false,
    chat: false,
    vision: false,
  });
  let openaiApiKeyInput = $state("");
  let geminiApiKeyInput = $state("");
  let deepseekApiKeyInput = $state("");
  let zaiApiKeyInput = $state("");
  let clearOpenaiApiKey = $state(false);
  let clearGeminiApiKey = $state(false);
  let clearDeepseekApiKey = $state(false);
  let clearZaiApiKey = $state(false);

  async function logChunkingStatus(sourceDocumentId: number, context: string) {
    try {
      const status = await invoke<string>("get_chunking_status", { sourceDocumentId });
      if (selectedBook?.id === sourceDocumentId) {
        currentChunkingStatus = status;
      }
      await appLogInfo(`[chunking] ${context}: doc=${sourceDocumentId} status=${status}`);
      return status;
    } catch (err) {
      await appLogWarn(
        `[chunking] ${context}: failed to load status for doc=${sourceDocumentId}: ${formatLogError(err)}`,
      );
      return null;
    }
  }

  async function ensureChunkingForPage(sourceDocumentId: number, pageNumber: number, context: string) {
    const provider = aiTaskSettings.chunking.provider;
    const model = aiTaskSettings.chunking.model.trim();
    try {
      const started = await invoke<boolean>("ensure_chunking_for_page", {
        sourceDocumentId,
        pageNumber,
        provider,
        model: model.length > 0 ? model : null,
      });
      if (started && selectedBook?.id === sourceDocumentId) {
        currentChunkingStatus = "extracting";
      }
      await appLogInfo(
        `[chunking] ${context}: doc=${sourceDocumentId} page=${pageNumber} provider=${provider} model=${model || "auto"} ${started ? "started page chunking" : "page chunking already active or complete"}`,
      );
      void logChunkingStatus(sourceDocumentId, `${context} status`);
      void refreshCurrentPageChunkingActive(sourceDocumentId, pageNumber);
    } catch (err) {
      await appLogWarn(
        `[chunking] ${context}: failed to ensure chunking for doc=${sourceDocumentId} page=${pageNumber}: ${formatLogError(err)}`,
      );
    }
  }

  async function refreshCurrentPageChunkingActive(sourceDocumentId: number, pageNumber: number) {
    try {
      const active = await invoke<boolean>("is_chunking_page_active", {
        sourceDocumentId,
        pageNumber,
      });
      if (selectedBook?.id === sourceDocumentId && currentPage === pageNumber) {
        currentPageChunkingActive = active;
      }
    } catch {
      if (selectedBook?.id === sourceDocumentId && currentPage === pageNumber) {
        currentPageChunkingActive = false;
      }
    }
  }

  // PDF viewer state
  let selectedBook = $state<SourceDocument | null>(null);
  let currentPage = $state(1);
  let pageInputValue = $state("1");
  let pageInputFocused = $state(false);
  let totalPages = $state(0);
  let rendering = $state(false);

  function isNotebookDocument(book: SourceDocument | null = selectedBook): boolean {
    return book?.document_mode === "notebook";
  }

  // DB row id for the current (source_document, page) pair; null until resolved
  let currentPageId = $state<number | null>(null);
  let currentPageSurfaceId = $state<number | null>(null);

  // â”€â”€ Stroke prefetch cache â”€â”€
  // 5-entry LRU keyed by page DB id. Evicts the oldest entry when full.
  const STROKE_CACHE_SIZE = 5;
  const strokeCacheOrder: number[] = [];   // front = most recently used
  const strokeCacheData = new Map<number, StrokeOutput[]>();
  const pageSurfaceCache = new Map<number, SurfaceGraphCacheEntry>();
  const pageSurfaceRequests = new Map<number, Promise<SurfaceGraphCacheEntry>>();
  const pageIdLookup = new Map<string, number>();
  const pageIdRequests = new Map<string, Promise<number>>();
  const chunkPageCache = new Map<string, ChunkInfo[]>();
  const chunkPageRequests = new Map<string, Promise<ChunkInfo[]>>();

  function pageKey(bookId: number, pageNum: number) {
    return `${bookId}:${pageNum}`;
  }

  function clearChunkPageCaches() {
    chunkPageCache.clear();
    chunkPageRequests.clear();
  }

  async function getOrCreatePageId(bookId: number, pageNum: number): Promise<number> {
    const key = pageKey(bookId, pageNum);
    const cached = pageIdLookup.get(key);
    if (cached != null) return cached;
    const pending = pageIdRequests.get(key);
    if (pending) return pending;

    const request = invoke<number>("get_or_create_page", {
      sourceDocumentId: bookId,
      pageNumber: pageNum,
    })
      .then((pageId) => {
        pageIdLookup.set(key, pageId);
        return pageId;
      })
      .finally(() => {
        pageIdRequests.delete(key);
      });
    pageIdRequests.set(key, request);
    return request;
  }

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

  async function getChunksForDocumentPage(bookId: number, pageNum: number): Promise<ChunkInfo[]> {
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return [];
    const key = pageKey(bookId, pageNum);
    const cached = chunkPageCache.get(key);
    if (cached) return cached;
    const existing = chunkPageRequests.get(key);
    if (existing) return existing;

    const request = (async () => {
      const pageId = await getOrCreatePageId(bookId, pageNum);
      const chunks = await invoke<ChunkInfo[]>("get_chunks_for_page", { pageId });
      // Ignore stale prefetches from previously opened books.
      if (selectedBook?.id === bookId) {
        chunkPageCache.set(key, chunks);
        for (const chunk of chunks) void ensureChunkSurface(chunk.id);
      }
      return chunks;
    })();
    chunkPageRequests.set(key, request);
    try {
      return await request;
    } catch {
      return [];
    } finally {
      chunkPageRequests.delete(key);
    }
  }

  function prefetchPage(bookId: number, pageNum: number) {
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return;
    void getChunksForDocumentPage(bookId, pageNum);
    getOrCreatePageId(bookId, pageNum)
      .then((pageId) => {
        if (strokeCacheData.has(pageId)) return;
        return invoke<StrokeOutput[]>("load_strokes", { pageId }).then((data) => {
          cachePut(pageId, data);
        });
      })
      .catch(() => {});
  }

  async function resolvePageId(bookId: number, pageNum: number) {
    const pageId = await getOrCreatePageId(bookId, pageNum);
    if (selectedBook?.id !== bookId || currentPage !== pageNum) return;
    currentPageId = pageId;
    await loadAndDrawStrokes(pageId);
    try {
      const pageSurface = await ensurePageSurface(pageId);
      if (selectedBook?.id !== bookId || currentPage !== pageNum || currentPageId !== pageId) return;
      currentPageSurfaceId = pageSurface.surfaceId;
      pageGraphs = cloneGraphObjects(pageSurface.graphs);
    } catch (err) {
      void appLogWarn(`[graph] failed to load page surface graphs for pageId=${pageId}: ${formatLogError(err)}`);
      currentPageSurfaceId = null;
      pageGraphs = [];
    }
    await loadChunksForPage(pageId, pageNum, bookId);
    void ensureChunkingForPage(bookId, pageNum, "page visible");
    prefetchPage(bookId, pageNum + 1);
    prefetchPage(bookId, pageNum - 1);
    markDirty();
  }

  interface StrokeOutput {
    id: number;
    colour: string;
    thickness: number;
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
      clientId: makeStrokeClientId(),
      colour: s.colour,
      thickness: s.thickness ?? 1,
      points: s.points.map(p => ({ x: p.x, y: p.y, pressure: 0.5 })),
      bbox: { minX: s.min_x, minY: s.min_y, maxX: s.max_x, maxY: s.max_y },
      chunkId: s.chunk_id,
    }));
    redoStack = [];
    clearPageStrokeTransformHistory();
    markDirty();
  }

  async function loadAndDrawSurfaceStrokes(surfaceId: number) {
    const loaded = await invoke<SurfaceStrokeOutput[]>("load_surface_strokes", { surfaceId });
    strokes = loaded.map(s => ({
      id: s.id,
      clientId: makeStrokeClientId(),
      colour: s.colour,
      thickness: s.thickness ?? 1,
      points: s.points.map(p => ({ x: p.x, y: p.y, pressure: 0.5 })),
      bbox: { minX: s.min_x, minY: s.min_y, maxX: s.max_x, maxY: s.max_y },
      chunkId: null,
    }));
    redoStack = [];
    clearPageStrokeTransformHistory();
    markDirty();
  }

  function cloneGraphObject(graph: SurfaceGraphObject): SurfaceGraphObject {
    return {
      id: graph.id,
      mode: graph.mode,
      equation: graph.equation,
      pointsJson: graph.pointsJson,
      xMin: graph.xMin,
      xMax: graph.xMax,
      yMin: graph.yMin,
      yMax: graph.yMax,
      bboxX: graph.bboxX,
      bboxY: graph.bboxY,
      bboxW: graph.bboxW,
      bboxH: graph.bboxH,
      lineColour: graph.lineColour,
      lineWidth: graph.lineWidth,
    };
  }

  function cloneGraphObjects(graphs: SurfaceGraphObject[]): SurfaceGraphObject[] {
    return graphs.map((graph) => cloneGraphObject(graph));
  }

  function graphOutputToObject(output: SurfaceGraphObjectOutput): SurfaceGraphObject {
    return {
      id: output.id,
      mode: output.mode,
      equation: output.equation,
      pointsJson: output.points_json,
      xMin: output.x_min,
      xMax: output.x_max,
      yMin: output.y_min,
      yMax: output.y_max,
      bboxX: output.bbox_x,
      bboxY: output.bbox_y,
      bboxW: output.bbox_w,
      bboxH: output.bbox_h,
      lineColour: output.line_colour,
      lineWidth: output.line_width,
    };
  }

  function graphObjectToSpec(graph: SurfaceGraphObject): GraphSpec {
    return normaliseGraphSpec({
      mode: graph.mode,
      equation: graph.equation,
      points: pointsFromJson(graph.pointsJson),
      xMin: graph.xMin,
      xMax: graph.xMax,
      yMin: graph.yMin,
      yMax: graph.yMax,
      lineColour: graph.lineColour,
      lineWidth: graph.lineWidth,
    });
  }

  function buildGraphObjectFromSpec(
    specInput: GraphSpec,
    bbox: { x: number; y: number; w: number; h: number },
  ): SurfaceGraphObject {
    const spec = normaliseGraphSpec(specInput);
    return {
      id: null,
      mode: spec.mode,
      equation: spec.mode === "equation" ? spec.equation : null,
      pointsJson: spec.mode === "points" ? pointsToJson(spec.points) : null,
      xMin: spec.xMin,
      xMax: spec.xMax,
      yMin: spec.yMin,
      yMax: spec.yMax,
      bboxX: bbox.x,
      bboxY: bbox.y,
      bboxW: bbox.w,
      bboxH: bbox.h,
      lineColour: spec.lineColour,
      lineWidth: spec.lineWidth,
    };
  }

  function surfaceGraphPayload(graph: SurfaceGraphObject): Record<string, unknown> {
    return {
      mode: graph.mode,
      equation: graph.mode === "equation" ? graph.equation : null,
      points_json: graph.mode === "points" ? graph.pointsJson : null,
      x_min: graph.xMin,
      x_max: graph.xMax,
      y_min: graph.yMin,
      y_max: graph.yMax,
      bbox_x: graph.bboxX,
      bbox_y: graph.bboxY,
      bbox_w: graph.bboxW,
      bbox_h: graph.bboxH,
      line_colour: graph.lineColour,
      line_width: graph.lineWidth,
    };
  }

  function canonicalPointsJson(value: string | null): string | null {
    const points = pointsFromJson(value);
    return points ? pointsToJson(points) : null;
  }

  function graphObjectsEquivalent(a: SurfaceGraphObject, b: SurfaceGraphObject): boolean {
    return a.mode === b.mode
      && (a.equation ?? null) === (b.equation ?? null)
      && canonicalPointsJson(a.pointsJson) === canonicalPointsJson(b.pointsJson)
      && a.xMin === b.xMin
      && a.xMax === b.xMax
      && a.yMin === b.yMin
      && a.yMax === b.yMax
      && a.bboxX === b.bboxX
      && a.bboxY === b.bboxY
      && a.bboxW === b.bboxW
      && a.bboxH === b.bboxH
      && a.lineColour === b.lineColour
      && a.lineWidth === b.lineWidth;
  }

  async function reconcileSurfaceGraphs(
    surfaceId: number,
    currentGraphs: SurfaceGraphObject[],
    targetGraphs: SurfaceGraphObject[],
  ): Promise<SurfaceGraphObject[]> {
    const currentById = new Map<number, SurfaceGraphObject>();
    for (const graph of currentGraphs) {
      if (graph.id == null) continue;
      currentById.set(graph.id, graph);
    }

    const target = cloneGraphObjects(targetGraphs);
    const targetIds = new Set<number>();
    for (const graph of target) {
      if (graph.id == null) continue;
      targetIds.add(graph.id);
    }

    for (const currentGraph of currentGraphs) {
      if (currentGraph.id == null) continue;
      if (targetIds.has(currentGraph.id)) continue;
      await invoke("delete_surface_graph_object", { graphId: currentGraph.id });
    }

    for (const graph of target) {
      if (graph.id != null) {
        const existing = currentById.get(graph.id);
        if (existing && !graphObjectsEquivalent(existing, graph)) {
          await invoke("update_surface_graph_object", {
            graphId: graph.id,
            graph: surfaceGraphPayload(graph),
          });
        } else if (!existing) {
          const created = await invoke<number>("save_surface_graph_object", {
            surfaceId,
            graph: surfaceGraphPayload(graph),
          });
          graph.id = created;
        }
      } else {
        const created = await invoke<number>("save_surface_graph_object", {
          surfaceId,
          graph: surfaceGraphPayload(graph),
        });
        graph.id = created;
      }
    }

    return target;
  }

  async function ensurePageSurface(pageId: number): Promise<SurfaceGraphCacheEntry> {
    const cached = pageSurfaceCache.get(pageId);
    if (cached) return cached;
    const existing = pageSurfaceRequests.get(pageId);
    if (existing) return existing;

    const request = (async () => {
      const surfaceId = await invoke<number>("get_or_create_page_surface", { pageId });
      const raw = await invoke<SurfaceGraphObjectOutput[]>("load_surface_graph_objects", { surfaceId });
      const entry: SurfaceGraphCacheEntry = {
        surfaceId,
        graphs: raw.map((graph) => graphOutputToObject(graph)),
      };
      pageSurfaceCache.set(pageId, entry);
      return entry;
    })();
    pageSurfaceRequests.set(pageId, request);
    try {
      return await request;
    } finally {
      pageSurfaceRequests.delete(pageId);
    }
  }

  // â”€â”€ Tool mode â”€â”€
  type Mode = 'draw' | 'shape' | 'erase' | 'select';
  type ShapeKind = InkShapeKind;
  type InkClipboardScope = "page" | "chunk";
  interface ShapeDraft {
    origin: Point;
    current: Point;
  }
  interface InkClipboardStrokeData {
    colour: string;
    thickness: number;
    points: Point[];
  }
  interface InkClipboardPayload {
    type: "gloss-ink-selection";
    version: 1;
    scope: InkClipboardScope;
    serial: string;
    strokes: InkClipboardStrokeData[];
  }
  interface InkClipboardState extends InkClipboardPayload {
    pasteCount: number;
    systemSynced: boolean;
  }
  interface InkAnchorPoint {
    x: number;
    y: number;
  }
  interface InkPasteHoldState {
    pointerId: number;
    startClientX: number;
    startClientY: number;
    startPoint: InkAnchorPoint;
    startedAt: number;
  }
  interface SelectionBounds {
    x: number;
    y: number;
    width: number;
    height: number;
  }
  interface StrokeDragSnapshot {
    stroke: Stroke;
    points: Point[];
    bbox: Stroke["bbox"];
  }
  interface InkStrokeDragState {
    startPoint: { x: number; y: number };
    startSelection: SelectionBounds;
    strokes: StrokeDragSnapshot[];
  }
  interface StrokeTransformHistoryItem {
    stroke: Stroke;
    beforePoints: Point[];
    beforeBBox: Stroke["bbox"];
    afterPoints: Point[];
    afterBBox: Stroke["bbox"];
  }
  interface StrokeTransformHistoryEntry {
    strokes: StrokeTransformHistoryItem[];
  }
  const MIN_SHAPE_DRAG_WORLD = 4;
  const INK_CLIPBOARD_PREFIX = "__GLOSS_INK_SELECTION__";
  const INK_PASTE_OFFSET_WORLD = 24;
  const INK_PASTE_HOLD_MS = 420;
  const INK_PASTE_HOLD_MOVE_PX = 8;
  const INK_POPOVER_EDGE_PADDING_PX = 22;
  const INK_POPOVER_TOP_PADDING_PX = 52;
  let mode = $state<Mode>('draw');
  // Point, Stroke, Rect, pen defaults, bbox, and shape helpers are imported from $lib/ink.
  const PEN_COLOURS = [
    "#ec9fba",
    "#efbf93",
    "#e8db7f",
    "#add39f",
    "#96d8d7",
    "#a9c2f6",
    "#c0afe9",
    "#ea9fd3",
    "#111111",
    "#e53935",
    "#ff7a00",
    "#ffbf00",
    "#2f9e44",
    "#118ab2",
    "#2351d1",
  ];
  type PaperPattern = "dotted" | "grid" | "line";
  interface PaperSettings {
    backgroundColour: string;
    pattern: PaperPattern;
    spacing: number;
  }
  const PAPER_SPACING_MIN = 20;
  const PAPER_SPACING_MAX = 80;
  const PAPER_BACKGROUND_SWATCHES = [
    "#f4f7fb",
    "#ffffff",
    "#fff9e8",
    "#eef8f1",
    "#f3eefc",
    "#111827",
  ];
  const PAPER_PATTERN_OPTIONS: Array<{ value: PaperPattern; label: string }> = [
    { value: "dotted", label: "Dots" },
    { value: "grid", label: "Grid" },
    { value: "line", label: "Lines" },
  ];
  const SHAPE_OPTIONS: Array<{ value: ShapeKind; label: string; hint: string }> = [
    { value: "circle", label: "Circle", hint: "Center at press point." },
    { value: "graph_full", label: "Graph ±", hint: "Axes in positive and negative directions." },
    { value: "graph_positive", label: "Graph +", hint: "Axes from origin to positive directions." },
  ];
  let penColour = $state(DEFAULT_PEN_COLOUR);
  let penThickness = $state(DEFAULT_PEN_THICKNESS);
  let showPenOptions = $state(false);
  let showPaperOptions = $state(false);
  let shapeKind = $state<ShapeKind>("circle");
  let showShapeOptions = $state(false);
  let shapeDraft = $state<ShapeDraft | null>(null);
  let paperSettings = $state<PaperSettings>(defaultPaperSettings());

  function defaultPaperSettings(): PaperSettings {
    return {
      backgroundColour: "#f4f7fb",
      pattern: "dotted",
      spacing: 40,
    };
  }

  function isHexColour(value: string): boolean {
    return /^#[0-9a-fA-F]{6}$/.test(value);
  }

  function clampPaperSpacing(value: number): number {
    if (!Number.isFinite(value)) return defaultPaperSettings().spacing;
    return Math.round(Math.max(PAPER_SPACING_MIN, Math.min(PAPER_SPACING_MAX, value)));
  }

  function normalisePaperPattern(value: unknown): PaperPattern {
    return value === "grid" || value === "line" || value === "dotted" ? value : "dotted";
  }

  function normalisePaperSettings(value: Partial<PaperSettings> | null | undefined): PaperSettings {
    const defaults = defaultPaperSettings();
    return {
      backgroundColour: typeof value?.backgroundColour === "string" && isHexColour(value.backgroundColour)
        ? value.backgroundColour
        : defaults.backgroundColour,
      pattern: normalisePaperPattern(value?.pattern),
      spacing: clampPaperSpacing(typeof value?.spacing === "number" ? value.spacing : defaults.spacing),
    };
  }

  function loadPaperSettings(): PaperSettings {
    if (typeof localStorage === "undefined") return defaultPaperSettings();
    const stored = localStorage.getItem(PAPER_SETTINGS_STORAGE_KEY);
    if (!stored) return defaultPaperSettings();
    try {
      return normalisePaperSettings(JSON.parse(stored) as Partial<PaperSettings>);
    } catch {
      return defaultPaperSettings();
    }
  }

  function persistPaperSettings() {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(PAPER_SETTINGS_STORAGE_KEY, JSON.stringify(paperSettings));
  }

  function isPaperBackgroundDark(): boolean {
    const colour = paperSettings.backgroundColour;
    if (!isHexColour(colour)) return false;
    const r = Number.parseInt(colour.slice(1, 3), 16);
    const g = Number.parseInt(colour.slice(3, 5), 16);
    const b = Number.parseInt(colour.slice(5, 7), 16);
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) < 120;
  }

  function paperPatternColour(alpha: number): string {
    return isPaperBackgroundDark()
      ? `rgba(255, 255, 255, ${alpha})`
      : `rgba(15, 23, 42, ${alpha})`;
  }

  function paperPatternAlpha(pattern: PaperPattern = paperSettings.pattern): number {
    return pattern === "dotted" ? 0.2 : 0.15;
  }

  function updatePaperSettings(next: Partial<PaperSettings>) {
    paperSettings = normalisePaperSettings({ ...paperSettings, ...next });
    persistPaperSettings();
    markDirty();
    redrawChunkCanvases();
  }

  function togglePaperOptions() {
    showPaperOptions = !showPaperOptions;
    showPenOptions = false;
    showShapeOptions = false;
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
  }

  // â”€â”€ Select state â”€â”€
  let selectOrigin = $state<{ x: number; y: number } | null>(null);
  let selectRect   = $state<Rect | null>(null);
  let selectedStrokes = $state<Set<Stroke>>(new Set());
  let selection = $state<{ x: number; y: number; width: number; height: number } | null>(null);
  let pageStrokeDrag = $state<InkStrokeDragState | null>(null);
  let pageStrokeTransformUndo = $state<StrokeTransformHistoryEntry | null>(null);
  let pageStrokeTransformRedo = $state<StrokeTransformHistoryEntry | null>(null);
  let inkClipboard = $state<InkClipboardState | null>(null);
  let hasPageInkClipboard = $derived.by(
    () => !!inkClipboard && inkClipboard.scope === "page" && inkClipboard.strokes.length > 0,
  );
  let hasChunkInkClipboard = $derived.by(
    () => !!inkClipboard && inkClipboard.scope === "chunk" && inkClipboard.strokes.length > 0,
  );
  let pagePasteMenuAnchor = $state<InkAnchorPoint | null>(null);
  let pagePasteHold = $state<InkPasteHoldState | null>(null);

  // â”€â”€ Drawing state â”€â”€
  let isDrawing = $state(false);
  let currentStroke: Point[] = [];
  let lastDrawnStrokeIndex = 0;
  // Predicted future pointer positions (from getPredictedEvents). Drawn on the
  // wet layer as a lookahead to eliminate perceived latency; discarded each frame.
  let predictedPoints: Point[] = [];
  const MAX_PREDICTED_POINTS = 2;
  const MAX_PREDICTED_STEP_PX = 24;
  const MAX_PREDICTED_RANGE_PX = 36;
  let strokes = $state<Stroke[]>([]);
  let redoStack = $state<Stroke[][]>([]);
  let pageGraphs = $state<SurfaceGraphObject[]>([]);
  let selectedPageGraphId = $state<number | null>(null);
  let pageGraphDrag = $state<GraphPointerDrag | null>(null);
  let pageGraphUndoStack = $state<GraphHistoryEntry[]>([]);
  let pageGraphRedoStack = $state<GraphHistoryEntry[]>([]);
  let graphModalOpen = $state(false);
  let graphModalSource = $state<GraphModalSource>("page");
  let graphModalInitialGraph = $state<GraphSpec | null>(null);
  let pendingGraphPlacement = $state<PendingGraphPlacement | null>(null);
  let activePointerId: number | null = null;
  const CHUNK_TAP_MAX_DISTANCE = 10;

  interface PendingChunkTap {
    pointerId: number;
    pointerType: string;
    chunk: ChunkInfo;
    startClientX: number;
    startClientY: number;
    startPoint: Point;
  }

  let pendingChunkTap = $state<PendingChunkTap | null>(null);

  // â”€â”€ Infinite canvas state â”€â”€
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
  let chunkOverlayCanvas = $state<HTMLCanvasElement>(null!);
  let dryCanvas  = $state<HTMLCanvasElement>(null!);
  let wetCanvas  = $state<HTMLCanvasElement>(null!);

  let gridCtx: CanvasRenderingContext2D | null = null;
  let pdfCtx:  CanvasRenderingContext2D | null = null;
  let chunkOverlayCtx: CanvasRenderingContext2D | null = null;
  let dryCtx:  CanvasRenderingContext2D | null = null;
  let wetCtx:  CanvasRenderingContext2D | null = null;

  // rAF render loop
  // dirtyWetOnly: only the wet (in-progress stroke) layer needs updating —
  // set during active drawing so completed dry strokes are not re-rendered
  // every frame. Any other change uses dirty=true which re-renders everything.
  let dirty = false;
  let dirtyWetOnly = false;
  let rafId: number | null = null;

  function scheduleRender() {
    if (rafId !== null) return;
    rafId = requestAnimationFrame(() => {
      rafId = null;
      if (dirty) {
        renderAll();
        dirty = false;
        dirtyWetOnly = false;
      } else if (dirtyWetOnly) {
        renderWetLayer();
        dirtyWetOnly = false;
      }
    });
  }

  function markDirty() { dirty = true; scheduleRender(); }
  // Use during active drawing: skips the expensive dry-stroke re-render.
  function markDirtyWet() { dirtyWetOnly = true; scheduleRender(); }

  // â”€â”€ PDF bitmap cache â”€â”€
  // Keyed by "pageNum:scaleKey". Stores rendered ImageBitmaps.
  const PDF_BITMAP_CACHE_MAX = 10;
  const PDF_PREVIEW_DPR_CAP = 1.25;
  const PDF_PREVIEW_MAX_WIDTH = 1280;
  const NOTEBOOK_PAGE_ASPECT = Math.SQRT2;
  const INK_DOCUMENT_BUFFER_PAGES = 1;
  const pdfBitmapCacheOrder: string[] = [];
  const pdfBitmapCache = new Map<string, ImageBitmap>();
  const pdfBitmapMetaCache = new Map<string, Omit<RenderedPdfBitmap, "bitmap">>();
  const pdfBitmapRequests = new Map<string, Promise<RenderedPdfBitmap>>();
  let currentPdfBitmap: ImageBitmap | null = null;
  let currentPdfBitmapKey: string | null = null;
  let currentPdfPagePoints = { w: 0, h: 0 };
  let pdfLoadVersion = 0;

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
      pdfBitmapMetaCache.delete(evict);
    }
    pdfBitmapCacheOrder.unshift(key);
    pdfBitmapCache.set(key, bm);
  }

  function bitmapMetaCachePut(key: string, rendered: Omit<RenderedPdfBitmap, "bitmap">) {
    pdfBitmapMetaCache.set(key, rendered);
  }

  function makePdfBitmapCacheKey(relativePath: string, pageNum: number, pixelWidth: number) {
    return `${relativePath}:${pageNum}:${pixelWidth}`;
  }

  function getPageDisplayWidth() {
    return Math.max(240, canvasContainer.clientWidth - 144);
  }

  function quantizePdfPixelWidth(pixelWidth: number) {
    return Math.max(256, Math.ceil(pixelWidth / 256) * 256);
  }

  function getPreviewPdfPixelWidth(pageDisplayW = getPageDisplayWidth()) {
    const dpr = window.devicePixelRatio || 1;
    const previewPixelWidth = Math.min(
      pageDisplayW * Math.min(dpr, PDF_PREVIEW_DPR_CAP),
      PDF_PREVIEW_MAX_WIDTH,
    );
    return quantizePdfPixelWidth(previewPixelWidth);
  }

  const PDF_UPGRADE_MAX_WIDTH = 3072;

  function getUpgradePdfPixelWidth() {
    if (!canvasContainer || !pageSize.w || !currentPdfPagePoints.w) return 0;
    const dpr = window.devicePixelRatio || 1;
    const screenWidth = pageSize.w * Math.max(1, camera.scale);
    const raw = quantizePdfPixelWidth(screenWidth * dpr * 1.15);
    return Math.min(raw, PDF_UPGRADE_MAX_WIDTH);
  }

  function hasEnoughPdfResolution(bitmap: ImageBitmap | null, desiredPixelWidth: number) {
    return !!bitmap && bitmap.width >= desiredPixelWidth * 0.9;
  }

  async function decodeRenderedPdfBitmap(buf: ArrayBuffer): Promise<RenderedPdfBitmap> {
    const header = new DataView(buf);
    const pageWidthPoints = header.getFloat32(0, true);
    const pageHeightPoints = header.getFloat32(4, true);
    const width = header.getUint32(8, true);
    const height = header.getUint32(12, true);
    const rgba = new Uint8ClampedArray(buf, 16);
    const bitmap = await createImageBitmap(new ImageData(rgba, width, height));
    return {
      bitmap,
      bitmapWidth: width,
      bitmapHeight: height,
      pageWidthPoints,
      pageHeightPoints,
    };
  }

  async function fetchPdfBitmap(pageNum: number, targetPixelWidth: number): Promise<{ key: string; rendered: RenderedPdfBitmap }> {
    if (isNotebookDocument()) throw new Error("Notebook documents do not have PDF bitmaps");
    if (!selectedBook) throw new Error("No selected book");
    const bookPath = selectedBook.file_path;
    const cacheKey = makePdfBitmapCacheKey(bookPath, pageNum, targetPixelWidth);
    const cached = bitmapCacheGet(cacheKey);

    if (cached) {
      const meta = pdfBitmapMetaCache.get(cacheKey);
      if (!meta) {
        throw new Error(`Missing cached PDF metadata for ${cacheKey}`);
      }
      return {
        key: cacheKey,
        rendered: {
          bitmap: cached,
          ...meta,
        },
      };
    }
    const inFlight = pdfBitmapRequests.get(cacheKey);
    if (inFlight) return { key: cacheKey, rendered: await inFlight };

    const request = (async () => {
      try {
        const buf: ArrayBuffer = await invoke("render_pdf_page", {
          relativePath: bookPath,
          pageNumber: pageNum - 1,
          targetWidth: targetPixelWidth,
        });
        const rendered = await decodeRenderedPdfBitmap(buf);
        bitmapCachePut(cacheKey, rendered.bitmap);
        bitmapMetaCachePut(cacheKey, {
          bitmapWidth: rendered.bitmapWidth,
          bitmapHeight: rendered.bitmapHeight,
          pageWidthPoints: rendered.pageWidthPoints,
          pageHeightPoints: rendered.pageHeightPoints,
        });
        return rendered;
      } finally {
        pdfBitmapRequests.delete(cacheKey);
      }
    })();

    pdfBitmapRequests.set(cacheKey, request);
    return { key: cacheKey, rendered: await request };
  }

  function prefetchPdfBitmap(pageNum: number, targetPixelWidth: number) {
    if (isNotebookDocument()) return;
    if (!selectedBook) return;
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return;

    const cacheKey = makePdfBitmapCacheKey(selectedBook.file_path, pageNum, targetPixelWidth);
    if (pdfBitmapCache.has(cacheKey) || pdfBitmapRequests.has(cacheKey)) return;

    void fetchPdfBitmap(pageNum, targetPixelWidth).catch(() => {
      // Prefetch should never interrupt active navigation.
    });
  }

  function applyRenderedPdfBitmap(
    bookPath: string,
    pageNum: number,
    loadVersion: number,
    cacheKey: string,
    rendered: RenderedPdfBitmap,
  ) {
    if (
      pdfLoadVersion !== loadVersion ||
      !selectedBook ||
      selectedBook.file_path !== bookPath ||
      currentPage !== pageNum
    ) {
      return false;
    }

    currentPdfPagePoints = { w: rendered.pageWidthPoints, h: rendered.pageHeightPoints };
    currentPdfBitmap = rendered.bitmap;
    currentPdfBitmapKey = cacheKey;
    markDirty();
    return true;
  }

  async function requestCurrentPdfBitmap() {
    if (isNotebookDocument()) return;
    if (!selectedBook || !currentPdfPagePoints.w) return;
    const bookPath = selectedBook.file_path;
    const targetPixelWidth = getUpgradePdfPixelWidth();
    if (!targetPixelWidth || hasEnoughPdfResolution(currentPdfBitmap, targetPixelWidth)) return;

    try {
      const loadVersion = pdfLoadVersion;
      const { key, rendered } = await fetchPdfBitmap(currentPage, targetPixelWidth);
      if (!currentPdfBitmap || rendered.bitmapWidth >= currentPdfBitmap.width) {
        applyRenderedPdfBitmap(bookPath, currentPage, loadVersion, key, rendered);
      }
    } catch {
      // Background upgrades should not disrupt navigation or input.
    }
  }

  // â”€â”€ Coordinate transforms â”€â”€

  function screenToWorld(sx: number, sy: number): { x: number; y: number } {
    return {
      x: (sx - camera.x) / camera.scale,
      y: (sy - camera.y) / camera.scale,
    };
  }

  // Cached bounding rect for the canvas container — invalidated on resize to
  // avoid calling getBoundingClientRect() on every pointer event.
  let cachedContainerRect: DOMRect | null = null;

  /** Convert a screen pointer to page-guide-relative space.
   * Notebook coordinates can extend beyond the visible page guide.
   */
  function pointerToNorm(clientX: number, clientY: number): { x: number; y: number } {
    const rect = cachedContainerRect ??= canvasContainer.getBoundingClientRect();
    const sx = clientX - rect.left;
    const sy = clientY - rect.top;
    const wx = (sx - camera.x) / camera.scale;
    const wy = (sy - camera.y) / camera.scale;
    const x = (wx - pageOrigin.x) / pageSize.w;
    const y = (wy - pageOrigin.y) / pageSize.h;
    return { x, y };
  }

  /** Normalised page space â†’ world space coordinates. */
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

  // Skip points within ~1.5 screen pixels of the last recorded point to avoid
  // bloating the stroke array during slow or stationary movements.
  function shouldAddPoint(p: Point): boolean {
    if (currentStroke.length === 0) return true;
    const last = currentStroke[currentStroke.length - 1];
    const dx = (p.x - last.x) * pageSize.w * camera.scale;
    const dy = (p.y - last.y) * pageSize.h * camera.scale;
    return dx * dx + dy * dy >= 2.25; // 1.5² px²
  }

  function clampNorm(v: number): number {
    if (v < 0) return 0;
    if (v > 1) return 1;
    return v;
  }

  function screenDistanceSq(a: Point, b: Point): number {
    const dx = (a.x - b.x) * pageSize.w * camera.scale;
    const dy = (a.y - b.y) * pageSize.h * camera.scale;
    return dx * dx + dy * dy;
  }

  function getSanitizedPredictedPoints(e: PointerEvent, anchor: Point | null): Point[] {
    if (!anchor) return [];
    const events = e.getPredictedEvents?.();
    if (!events || events.length === 0) return [];

    const out: Point[] = [];
    const maxStepSq = MAX_PREDICTED_STEP_PX * MAX_PREDICTED_STEP_PX;
    const maxRangeSq = MAX_PREDICTED_RANGE_PX * MAX_PREDICTED_RANGE_PX;
    let prev = anchor;
    const count = Math.min(events.length, MAX_PREDICTED_POINTS);

    for (let i = 0; i < count; i++) {
      const p = getPagePoint(events[i]);
      const stepSq = screenDistanceSq(prev, p);
      if (stepSq > maxStepSq) break;
      if (screenDistanceSq(anchor, p) > maxRangeSq) break;
      if (stepSq < 0.25) continue;
      out.push(p);
      prev = p;
    }

    return out;
  }

  function makeStrokeGroupId(): string {
    return `shape-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function makeInkClipboardSerial(): string {
    return `ink-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function cloneInkPoints(points: Point[]): Point[] {
    return points.map(({ x, y, pressure }) => ({ x, y, pressure }));
  }

  function cloneStrokeBBox(bbox: Stroke["bbox"]): Stroke["bbox"] {
    return {
      minX: bbox.minX,
      minY: bbox.minY,
      maxX: bbox.maxX,
      maxY: bbox.maxY,
    };
  }

  function cloneStroke(stroke: Stroke): Stroke {
    return {
      id: stroke.id,
      clientId: stroke.clientId ?? makeStrokeClientId(),
      colour: stroke.colour,
      thickness: stroke.thickness,
      points: cloneInkPoints(stroke.points),
      bbox: cloneStrokeBBox(stroke.bbox),
      chunkId: stroke.chunkId,
      groupId: stroke.groupId ?? null,
    };
  }

  function cloneStrokes(strokes: Stroke[]): Stroke[] {
    return strokes.map((stroke) => cloneStroke(stroke));
  }

  function serialisePageStrokeInput(stroke: Stroke) {
    return {
      colour: stroke.colour,
      thickness: stroke.thickness,
      points: stroke.points.map(({ x, y }) => ({ x, y })),
      chunkId: stroke.chunkId,
    };
  }

  function serialiseSurfaceStrokeInput(stroke: Stroke) {
    return {
      colour: stroke.colour,
      thickness: stroke.thickness,
      points: stroke.points.map(({ x, y }) => ({ x, y })),
    };
  }

  function strokePointsMatch(
    a: Array<{ x: number; y: number }>,
    b: Array<{ x: number; y: number }>,
  ): boolean {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i += 1) {
      if (a[i].x !== b[i].x || a[i].y !== b[i].y) return false;
    }
    return true;
  }

  function pageStrokeInputsMatch(
    a: ReturnType<typeof serialisePageStrokeInput>,
    b: ReturnType<typeof serialisePageStrokeInput>,
  ): boolean {
    return (
      a.colour === b.colour
      && a.thickness === b.thickness
      && a.chunkId === b.chunkId
      && strokePointsMatch(a.points, b.points)
    );
  }

  function surfaceStrokeInputsMatch(
    a: ReturnType<typeof serialiseSurfaceStrokeInput>,
    b: ReturnType<typeof serialiseSurfaceStrokeInput>,
  ): boolean {
    return (
      a.colour === b.colour
      && a.thickness === b.thickness
      && strokePointsMatch(a.points, b.points)
    );
  }

  const pendingInkWrites = new Set<Promise<void>>();
  const cancelledPendingStrokeClientIds = new Set<string>();

  function trackInkWrite(promise: Promise<void>): Promise<void> {
    let tracked: Promise<void>;
    tracked = promise.finally(() => {
      pendingInkWrites.delete(tracked);
    });
    pendingInkWrites.add(tracked);
    return tracked;
  }

  async function flushPendingInkWrites(): Promise<void> {
    while (pendingInkWrites.size > 0) {
      await Promise.allSettled([...pendingInkWrites]);
    }
  }

  function cancelPendingStrokeCreate(stroke: Stroke): void {
    if (stroke.id !== null) return;
    cancelledPendingStrokeClientIds.add(ensureStrokeClientId(stroke));
  }

  function prepareStrokeForCreate(stroke: Stroke): string {
    const clientId = ensureStrokeClientId(stroke);
    if (stroke.id === null && cancelledPendingStrokeClientIds.has(clientId)) {
      stroke.clientId = makeStrokeClientId();
      return stroke.clientId;
    }
    return clientId;
  }

  function consumePendingStrokeCancellation(clientId: string): boolean {
    const cancelled = cancelledPendingStrokeClientIds.delete(clientId);
    return cancelled;
  }

  function assignPersistedStrokeId(stroke: Stroke, id: number, clientId: string): Stroke {
    if (stroke.clientId === clientId) stroke.id = id;

    const pageStroke = findStrokeByClientId(strokes, clientId);
    if (pageStroke) pageStroke.id = id;

    const chunkId = stroke.chunkId;
    if (chunkId != null) {
      const viewStroke = chunkView?.chunk.id === chunkId
        ? findStrokeByClientId(chunkView.strokes, clientId)
        : null;
      if (viewStroke) viewStroke.id = id;

      const cachedStroke = chunkSurfaceCache.get(chunkId)?.strokes
        ? findStrokeByClientId(chunkSurfaceCache.get(chunkId)!.strokes, clientId)
        : null;
      if (cachedStroke) cachedStroke.id = id;

      return viewStroke ?? cachedStroke ?? stroke;
    }

    return pageStroke ?? stroke;
  }

  async function persistPageStrokeCreate(
    pageId: number,
    stroke: Stroke,
  ): Promise<void> {
    const clientId = prepareStrokeForCreate(stroke);
    const useSurfaceStroke = isNotebookDocument();
    if (useSurfaceStroke) {
      let surfaceId = currentPageSurfaceId;
      if (surfaceId == null) {
        try {
          const pageSurface = await ensurePageSurface(pageId);
          surfaceId = pageSurface.surfaceId;
          if (currentPageId === pageId) currentPageSurfaceId = surfaceId;
        } catch (err) {
          void appLogWarn(`[ink] failed to resolve notebook surface pageId=${pageId}: ${formatLogError(err)}`);
          return;
        }
      }
      if (surfaceId == null) return;
      const initial = serialiseSurfaceStrokeInput(stroke);
      try {
        const id = await invoke<number>("save_surface_stroke", { surfaceId, stroke: initial });
        if (consumePendingStrokeCancellation(clientId)) {
          await invoke("delete_surface_stroke", { strokeId: id });
          return;
        }
        const liveStroke = assignPersistedStrokeId(stroke, id, clientId);
        const latest = serialiseSurfaceStrokeInput(liveStroke);
        if (!surfaceStrokeInputsMatch(initial, latest)) {
          await invoke("update_surface_stroke", { strokeId: id, stroke: latest });
        }
      } catch (err) {
        void appLogWarn(`[ink] save_surface_stroke failed: ${formatLogError(err)}`);
      }
      return;
    }

    const initial = serialisePageStrokeInput(stroke);
    try {
      const id = await invoke<number>("save_stroke", { pageId, stroke: initial });
      if (consumePendingStrokeCancellation(clientId)) {
        await invoke("delete_stroke", { strokeId: id });
        return;
      }
      const liveStroke = assignPersistedStrokeId(stroke, id, clientId);
      const latest = serialisePageStrokeInput(liveStroke);
      if (!pageStrokeInputsMatch(initial, latest)) {
        await invoke("update_stroke", { strokeId: id, stroke: latest });
      }
    } catch (err) {
      void appLogWarn(`[ink] save_stroke failed: ${formatLogError(err)}`);
    }
  }

  async function persistChunkStrokeCreate(
    surfaceId: number,
    stroke: Stroke,
  ): Promise<void> {
    const clientId = prepareStrokeForCreate(stroke);
    const initial = serialiseSurfaceStrokeInput(stroke);
    try {
      const id = await invoke<number>("save_surface_stroke", { surfaceId, stroke: initial });
      if (consumePendingStrokeCancellation(clientId)) {
        await invoke("delete_surface_stroke", { strokeId: id });
        return;
      }
      const liveStroke = assignPersistedStrokeId(stroke, id, clientId);
      const latest = serialiseSurfaceStrokeInput(liveStroke);
      if (!surfaceStrokeInputsMatch(initial, latest)) {
        await invoke("update_surface_stroke", { strokeId: id, stroke: latest });
      }
    } catch (err) {
      void appLogWarn(`[ink] save_surface_stroke failed: ${formatLogError(err)}`);
    }
  }

  async function persistPageStrokeUpdates(targetStrokes: Stroke[]): Promise<void> {
    if (isNotebookDocument()) {
      if (targetStrokes.length === 0) return;
      await Promise.all(targetStrokes.map(async (stroke) => {
        if (stroke.id === null) return;
        await invoke("update_surface_stroke", {
          strokeId: stroke.id,
          stroke: serialiseSurfaceStrokeInput(stroke),
        });
      })).catch((err) => {
        void appLogWarn(`[ink] update_surface_stroke failed: ${formatLogError(err)}`);
      });
      return;
    }

    const pageId = currentPageId;
    if (pageId === null || targetStrokes.length === 0) return;
    cacheEvict(pageId);
    await Promise.all(targetStrokes.map(async (stroke) => {
      if (stroke.id === null) return;
      await invoke("update_stroke", {
        strokeId: stroke.id,
        stroke: serialisePageStrokeInput(stroke),
      });
    })).catch((err) => {
      void appLogWarn(`[ink] update_stroke failed: ${formatLogError(err)}`);
    });
  }

  function deletePageStroke(strokeId: number): Promise<void> {
    return isNotebookDocument()
      ? invoke("delete_surface_stroke", { strokeId })
      : invoke("delete_stroke", { strokeId });
  }

  async function persistChunkStrokeUpdates(targetStrokes: Stroke[]): Promise<void> {
    if (targetStrokes.length === 0) return;
    await Promise.all(targetStrokes.map(async (stroke) => {
      if (stroke.id === null) return;
      await invoke("update_surface_stroke", {
        strokeId: stroke.id,
        stroke: serialiseSurfaceStrokeInput(stroke),
      });
    })).catch((err) => {
      console.error("update_surface_stroke failed", err);
    });
  }

  function clearPageStrokeTransformHistory() {
    pageStrokeDrag = null;
    pageStrokeTransformUndo = null;
    pageStrokeTransformRedo = null;
  }

  function clearChunkStrokeTransformHistory() {
    chunkStrokeDrag = null;
    chunkStrokeTransformUndo = null;
    chunkStrokeTransformRedo = null;
  }

  function getOrderedSelectedStrokes(allStrokes: Stroke[], selected: Set<Stroke>): Stroke[] {
    if (selected.size === 0) return [];
    return allStrokes.filter((stroke) => selected.has(stroke));
  }

  function buildInkClipboardState(
    scope: InkClipboardScope,
    allStrokes: Stroke[],
    selected: Set<Stroke>,
  ): InkClipboardState | null {
    const ordered = getOrderedSelectedStrokes(allStrokes, selected);
    if (ordered.length === 0) return null;
    return {
      type: "gloss-ink-selection",
      version: 1,
      scope,
      serial: makeInkClipboardSerial(),
      strokes: ordered.map((stroke) => ({
        colour: stroke.colour,
        thickness: stroke.thickness,
        points: cloneInkPoints(stroke.points),
      })),
      pasteCount: 0,
      systemSynced: false,
    };
  }

  function serialiseInkClipboard(payload: InkClipboardPayload): string {
    return `${INK_CLIPBOARD_PREFIX}${JSON.stringify({
      type: payload.type,
      version: payload.version,
      scope: payload.scope,
      serial: payload.serial,
      strokes: payload.strokes.map((stroke) => ({
        colour: stroke.colour,
        thickness: stroke.thickness,
        points: cloneInkPoints(stroke.points),
      })),
    } satisfies InkClipboardPayload)}`;
  }

  function normaliseClipboardPoint(value: unknown): Point | null {
    if (!value || typeof value !== "object") return null;
    const point = value as { x?: unknown; y?: unknown; pressure?: unknown };
    if (typeof point.x !== "number" || !Number.isFinite(point.x)) return null;
    if (typeof point.y !== "number" || !Number.isFinite(point.y)) return null;
    const pressure =
      typeof point.pressure === "number" && Number.isFinite(point.pressure)
        ? point.pressure
        : 0.5;
    return { x: point.x, y: point.y, pressure };
  }

  function parseInkClipboardText(text: string | null | undefined): InkClipboardPayload | null {
    if (!text || !text.startsWith(INK_CLIPBOARD_PREFIX)) return null;
    try {
      const parsed = JSON.parse(text.slice(INK_CLIPBOARD_PREFIX.length)) as {
        type?: unknown;
        version?: unknown;
        scope?: unknown;
        serial?: unknown;
        strokes?: unknown;
      };
      if (parsed.type !== "gloss-ink-selection" || parsed.version !== 1) return null;
      if (parsed.scope !== "page" && parsed.scope !== "chunk") return null;
      if (!Array.isArray(parsed.strokes)) return null;
      const strokes = parsed.strokes
        .map((stroke) => {
          if (!stroke || typeof stroke !== "object") return null;
          const candidate = stroke as {
            colour?: unknown;
            thickness?: unknown;
            points?: unknown;
          };
          if (typeof candidate.colour !== "string") return null;
          if (typeof candidate.thickness !== "number" || !Number.isFinite(candidate.thickness)) {
            return null;
          }
          if (!Array.isArray(candidate.points)) return null;
          const points = candidate.points
            .map((point) => normaliseClipboardPoint(point))
            .filter((point): point is Point => point !== null);
          if (points.length < 2) return null;
          return {
            colour: candidate.colour,
            thickness: candidate.thickness,
            points,
          } satisfies InkClipboardStrokeData;
        })
        .filter((stroke): stroke is InkClipboardStrokeData => stroke !== null);
      if (strokes.length === 0) return null;
      return {
        type: "gloss-ink-selection",
        version: 1,
        scope: parsed.scope,
        serial:
          typeof parsed.serial === "string" && parsed.serial.trim().length > 0
            ? parsed.serial
            : makeInkClipboardSerial(),
        strokes,
      };
    } catch {
      return null;
    }
  }

  function resolvePasteDelta(
    sourceMin: number,
    sourceMax: number,
    targetMin: number,
    targetMax: number,
    desired: number,
  ): number {
    const minAllowed = targetMin - sourceMin;
    const maxAllowed = targetMax - sourceMax;
    if (desired >= minAllowed && desired <= maxAllowed) return desired;
    const opposite = -desired;
    if (opposite >= minAllowed && opposite <= maxAllowed) return opposite;
    return Math.max(minAllowed, Math.min(maxAllowed, desired));
  }

  function preparePastedStrokes(
    clipboard: InkClipboardState,
    surfaceSize: { w: number; h: number },
    chunkId: number | null,
    bounds: { minX: number; maxX: number; minY: number; maxY: number },
    anchorPoint: InkAnchorPoint | null = null,
  ): { strokes: Stroke[]; nextPasteCount: number } | null {
    if (clipboard.strokes.length === 0 || surfaceSize.w <= 0 || surfaceSize.h <= 0) return null;

    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const stroke of clipboard.strokes) {
      const bbox = computeBBox(stroke.points);
      if (bbox.minX < minX) minX = bbox.minX;
      if (bbox.minY < minY) minY = bbox.minY;
      if (bbox.maxX > maxX) maxX = bbox.maxX;
      if (bbox.maxY > maxY) maxY = bbox.maxY;
    }

    if (!Number.isFinite(minX) || !Number.isFinite(minY) || !Number.isFinite(maxX) || !Number.isFinite(maxY)) {
      return null;
    }

    const nextPasteCount = clipboard.pasteCount + 1;
    const desiredDeltaX = anchorPoint
      ? anchorPoint.x - ((minX + maxX) * 0.5)
      : (INK_PASTE_OFFSET_WORLD * nextPasteCount) / surfaceSize.w;
    const desiredDeltaY = anchorPoint
      ? anchorPoint.y - ((minY + maxY) * 0.5)
      : (INK_PASTE_OFFSET_WORLD * nextPasteCount) / surfaceSize.h;
    const deltaX = resolvePasteDelta(minX, maxX, bounds.minX, bounds.maxX, desiredDeltaX);
    const deltaY = resolvePasteDelta(minY, maxY, bounds.minY, bounds.maxY, desiredDeltaY);
    const groupId = clipboard.strokes.length > 1 ? makeStrokeGroupId() : null;

    const strokes = clipboard.strokes.map((stroke) => {
      const points = stroke.points.map(({ x, y, pressure }) => ({
        x: x + deltaX,
        y: y + deltaY,
        pressure,
      }));
      return {
        id: null,
        clientId: makeStrokeClientId(),
        colour: stroke.colour,
        thickness: stroke.thickness,
        points,
        bbox: computeBBox(points),
        chunkId,
        groupId,
      } satisfies Stroke;
    });

    return {
      strokes,
      nextPasteCount,
    };
  }

  function hasDocumentTextSelection(): boolean {
    const selectedText = window.getSelection();
    return !!selectedText && !selectedText.isCollapsed && selectedText.toString().length > 0;
  }

  function pointerMovedPastHoldThreshold(
    hold: InkPasteHoldState,
    clientX: number,
    clientY: number,
  ): boolean {
    return Math.hypot(clientX - hold.startClientX, clientY - hold.startClientY) > INK_PASTE_HOLD_MOVE_PX;
  }

  function screenPointToPopoverStyle(
    screenX: number,
    screenY: number,
    viewportW: number,
    viewportH: number,
  ): string {
    const clampedX = Math.max(
      INK_POPOVER_EDGE_PADDING_PX,
      Math.min(viewportW - INK_POPOVER_EDGE_PADDING_PX, screenX),
    );
    const clampedY = Math.max(
      INK_POPOVER_TOP_PADDING_PX,
      Math.min(viewportH - INK_POPOVER_EDGE_PADDING_PX, screenY),
    );
    return `left: ${clampedX}px; top: ${clampedY}px;`;
  }

  function pageWorldToScreen(worldX: number, worldY: number): { x: number; y: number } {
    return {
      x: worldX * camera.scale + camera.x,
      y: worldY * camera.scale + camera.y,
    };
  }

  function getPageSelectionPopoverStyle(): string {
    if (!selection || !canvasContainer) return "";
    const topLeft = normToWorld(selection.x, selection.y);
    const bottomRight = normToWorld(selection.x + selection.width, selection.y + selection.height);
    const screen = pageWorldToScreen((topLeft.x + bottomRight.x) * 0.5, topLeft.y);
    return screenPointToPopoverStyle(
      screen.x,
      screen.y - 12,
      canvasContainer.clientWidth,
      canvasContainer.clientHeight,
    );
  }

  function getPagePastePopoverStyle(): string {
    if (!pagePasteMenuAnchor || !canvasContainer) return "";
    const anchor = normToWorld(pagePasteMenuAnchor.x, pagePasteMenuAnchor.y);
    const screen = pageWorldToScreen(anchor.x, anchor.y);
    return screenPointToPopoverStyle(
      screen.x,
      screen.y - 12,
      canvasContainer.clientWidth,
      canvasContainer.clientHeight,
    );
  }

  function chunkWorldToScreen(worldX: number, worldY: number): { x: number; y: number } {
    return {
      x: worldX * chunkCamera.scale + chunkCamera.x,
      y: worldY * chunkCamera.scale + chunkCamera.y,
    };
  }

  function getChunkSelectionPopoverStyle(): string {
    if (!chunkSelection) return "";
    const topLeft = chunkPointToWorld({ x: chunkSelection.x, y: chunkSelection.y });
    const bottomRight = chunkPointToWorld({
      x: chunkSelection.x + chunkSelection.width,
      y: chunkSelection.y + chunkSelection.height,
    });
    const screen = chunkWorldToScreen((topLeft.x + bottomRight.x) * 0.5, topLeft.y);
    return screenPointToPopoverStyle(
      screen.x,
      screen.y - 12,
      chunkSurfaceSize.w,
      chunkSurfaceSize.h,
    );
  }

  function getChunkPastePopoverStyle(): string {
    if (!chunkPasteMenuAnchor) return "";
    const anchor = chunkPointToWorld(chunkPasteMenuAnchor);
    const screen = chunkWorldToScreen(anchor.x, anchor.y);
    return screenPointToPopoverStyle(
      screen.x,
      screen.y - 12,
      chunkSurfaceSize.w,
      chunkSurfaceSize.h,
    );
  }

  function getTrailingStrokeGroup(allStrokes: Stroke[]): Stroke[] {
    if (allStrokes.length === 0) return [];
    const last = allStrokes[allStrokes.length - 1];
    const groupId = last.groupId;
    if (!groupId) return [last];
    let start = allStrokes.length - 1;
    while (start > 0 && allStrokes[start - 1].groupId === groupId) start--;
    return allStrokes.slice(start);
  }

  function getPageShapeStrokes(draft: ShapeDraft): Point[][] {
    return buildShapeStrokes(
      shapeKind,
      draft.origin,
      draft.current,
      pageSize.w,
      pageSize.h,
      0.5,
    );
  }

  function commitPageShape(draft: ShapeDraft) {
    if (!pageSize.w || !pageSize.h) return;
    const dragWorld = shapeDragLengthWorld(draft.origin, draft.current, pageSize.w, pageSize.h);
    if (dragWorld < MIN_SHAPE_DRAG_WORLD) return;
    const generated = getPageShapeStrokes(draft);
    if (generated.length === 0) return;
    const groupId = generated.length > 1 ? makeStrokeGroupId() : null;
    const newStrokes: Stroke[] = generated
      .filter((pts) => pts.length >= 2)
      .map((pts) => ({
        id: null,
        clientId: makeStrokeClientId(),
        colour: penColour,
        thickness: penThickness,
        points: pts,
        bbox: computeBBox(pts),
        chunkId: null,
        groupId,
      }));
    if (newStrokes.length === 0) return;

    strokes = [...strokes, ...newStrokes];
    redoStack = [];
    clearPageStrokeTransformHistory();
    if (currentPageId !== null) {
      cacheEvict(currentPageId);
      for (const stroke of newStrokes) {
        void trackInkWrite(persistPageStrokeCreate(currentPageId, stroke));
      }
    }
  }

  const MIN_GRAPH_BBOX_SIZE = 0.08;
  const PAGE_GRAPH_DEFAULT_SIZE = { w: 0.34, h: 0.28 };
  const CHUNK_GRAPH_DEFAULT_SIZE = { w: 0.48, h: 0.4 };

  function graphArraysEquivalent(a: SurfaceGraphObject[], b: SurfaceGraphObject[]): boolean {
    if (a.length !== b.length) return false;
    for (let index = 0; index < a.length; index += 1) {
      const left = a[index];
      const right = b[index];
      if (left.id !== right.id) return false;
      if (!graphObjectsEquivalent(left, right)) return false;
    }
    return true;
  }

  function clampGraphRect(rect: { x: number; y: number; w: number; h: number }) {
    if (isNotebookDocument()) {
      return {
        x: rect.x,
        y: rect.y,
        w: Math.max(MIN_GRAPH_BBOX_SIZE, rect.w),
        h: Math.max(MIN_GRAPH_BBOX_SIZE, rect.h),
      };
    }
    const w = clampNorm(Math.max(MIN_GRAPH_BBOX_SIZE, rect.w));
    const h = clampNorm(Math.max(MIN_GRAPH_BBOX_SIZE, rect.h));
    const x = Math.max(0, Math.min(1 - w, rect.x));
    const y = Math.max(0, Math.min(1 - h, rect.y));
    return { x, y, w, h };
  }

  function graphRectFromCenter(
    center: { x: number; y: number },
    size: { w: number; h: number },
  ) {
    return clampGraphRect({
      x: center.x - size.w * 0.5,
      y: center.y - size.h * 0.5,
      w: size.w,
      h: size.h,
    });
  }

  function clampChunkGraphRect(rect: { x: number; y: number; w: number; h: number }) {
    const w = clampNorm(Math.max(MIN_GRAPH_BBOX_SIZE, rect.w));
    const h = Math.max(MIN_GRAPH_BBOX_SIZE, rect.h);
    const x = Math.max(0, Math.min(1 - w, rect.x));
    const y = Math.max(0, rect.y);
    return { x, y, w, h };
  }

  function chunkGraphRectFromCenter(
    center: { x: number; y: number },
    size: { w: number; h: number },
  ) {
    return clampChunkGraphRect({
      x: center.x - size.w * 0.5,
      y: center.y - size.h * 0.5,
      w: size.w,
      h: size.h,
    });
  }

  function updatePageSurfaceCacheGraphs(next: SurfaceGraphObject[]) {
    if (currentPageId == null) return;
    const cached = pageSurfaceCache.get(currentPageId);
    if (!cached) return;
    cached.graphs = cloneGraphObjects(next);
  }

  function recordPageGraphHistory(
    before: SurfaceGraphObject[],
    after: SurfaceGraphObject[],
  ) {
    if (currentPageSurfaceId == null) return;
    if (graphArraysEquivalent(before, after)) return;
    pageGraphUndoStack = [
      ...pageGraphUndoStack,
      {
        surfaceId: currentPageSurfaceId,
        before: cloneGraphObjects(before),
        after: cloneGraphObjects(after),
      },
    ];
    pageGraphRedoStack = [];
  }

  async function applyPageGraphChange(
    target: SurfaceGraphObject[],
    options: { recordHistory?: boolean; beforeSnapshot?: SurfaceGraphObject[] } = {},
  ): Promise<SurfaceGraphObject[] | null> {
    if (currentPageSurfaceId == null) return null;
    const before = options.beforeSnapshot ? cloneGraphObjects(options.beforeSnapshot) : cloneGraphObjects(pageGraphs);
    const reconciled = await reconcileSurfaceGraphs(currentPageSurfaceId, pageGraphs, target);
    pageGraphs = reconciled;
    updatePageSurfaceCacheGraphs(reconciled);
    if (options.recordHistory) {
      recordPageGraphHistory(before, reconciled);
    }
    markDirty();
    return reconciled;
  }

  function hitTestPageGraph(
    point: { x: number; y: number },
  ): { graphId: number; mode: "move" | "resize" } | null {
    if (pageGraphs.length === 0) return null;
    const handleNormX = pageSize.w > 0 ? (12 / camera.scale) / pageSize.w : 0.02;
    const handleNormY = pageSize.h > 0 ? (12 / camera.scale) / pageSize.h : 0.02;

    for (let index = pageGraphs.length - 1; index >= 0; index -= 1) {
      const graph = pageGraphs[index];
      if (graph.id == null) continue;
      const x2 = graph.bboxX + graph.bboxW;
      const y2 = graph.bboxY + graph.bboxH;
      if (
        point.x < graph.bboxX
        || point.x > x2
        || point.y < graph.bboxY
        || point.y > y2
      ) {
        continue;
      }

      const nearHandle = Math.abs(point.x - x2) <= handleNormX && Math.abs(point.y - y2) <= handleNormY;
      return { graphId: graph.id, mode: nearHandle ? "resize" : "move" };
    }
    return null;
  }

  async function deleteSelectedPageGraph() {
    if (selectedPageGraphId == null) return;
    const before = cloneGraphObjects(pageGraphs);
    const target = pageGraphs.filter((graph) => graph.id !== selectedPageGraphId);
    await applyPageGraphChange(target, {
      recordHistory: true,
      beforeSnapshot: before,
    });
    selectedPageGraphId = null;
  }

  function openGraphComposer(source: GraphModalSource) {
    graphModalSource = source;
    graphModalInitialGraph = null;
    graphModalOpen = true;
    showPenOptions = false;
    showShapeOptions = false;
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
    showPaperOptions = false;
  }

  function closeGraphComposer() {
    graphModalOpen = false;
  }

  function onGraphModalInsertCanvas(spec: GraphSpec) {
    const normalized = normaliseGraphSpec(spec);
    if ((graphModalSource === "chunk" || graphModalSource === "glossary") && chunkView) {
      pendingGraphPlacement = { scope: "chunk", spec: normalized };
      closeGraphComposer();
      return;
    }
    pendingGraphPlacement = { scope: "page", spec: normalized };
    closeGraphComposer();
  }

  function onGraphModalInsertGlossary(spec: GraphSpec) {
    if (!chunkView) {
      closeGraphComposer();
      return;
    }
    insertGraphFenceIntoGlossary(spec);
    closeGraphComposer();
  }

  function updatePageGraphInState(graphId: number, next: SurfaceGraphObject) {
    pageGraphs = pageGraphs.map((graph) => (graph.id === graphId ? next : graph));
    updatePageSurfaceCacheGraphs(pageGraphs);
    markDirty();
  }

  async function placePendingPageGraphAt(point: { x: number; y: number }): Promise<boolean> {
    if (!pendingGraphPlacement || pendingGraphPlacement.scope !== "page") return false;
    const rect = graphRectFromCenter(point, PAGE_GRAPH_DEFAULT_SIZE);
    const graph = buildGraphObjectFromSpec(pendingGraphPlacement.spec, rect);
    const before = cloneGraphObjects(pageGraphs);
    const target = [...before, graph];
    try {
      const applied = await applyPageGraphChange(target, {
        recordHistory: true,
        beforeSnapshot: before,
      });
      if (applied && applied.length > 0) {
        selectedPageGraphId = applied[applied.length - 1].id;
      }
      selectedStrokes = new Set();
      selection = null;
    } catch (err) {
      void appLogWarn(`[graph] failed to place page graph: ${formatLogError(err)}`);
    } finally {
      pendingGraphPlacement = null;
    }
    return true;
  }

  function clearSelectionState() {
    selectedStrokes = new Set();
    selection = null;
    selectedPageGraphId = null;
    pageGraphDrag = null;
    pageStrokeDrag = null;
    pagePasteHold = null;
    pagePasteMenuAnchor = null;
  }

  function activateDrawTool() {
    const wasDrawMode = mode === 'draw';
    mode = 'draw';
    clearSelectionState();
    selectOrigin = null;
    selectRect = null;
    shapeDraft = null;
    if (pendingGraphPlacement?.scope === "page") pendingGraphPlacement = null;
    showPenOptions = wasDrawMode ? !showPenOptions : true;
    showShapeOptions = false;
    showPaperOptions = false;
    markDirty();
  }

  function activateShapeTool() {
    const wasShapeMode = mode === "shape";
    mode = "shape";
    clearSelectionState();
    selectOrigin = null;
    selectRect = null;
    isDrawing = false;
    currentStroke = [];
    predictedPoints = [];
    shapeDraft = null;
    if (pendingGraphPlacement?.scope === "page") pendingGraphPlacement = null;
    showPenOptions = false;
    showShapeOptions = wasShapeMode ? !showShapeOptions : true;
    showPaperOptions = false;
    markDirty();
  }

  function activateGraphTool() {
    clearSelectionState();
    selectOrigin = null;
    selectRect = null;
    shapeDraft = null;
    showPaperOptions = false;
    openGraphComposer("page");
    markDirty();
  }

  function activateEraseTool() {
    mode = 'erase';
    showPenOptions = false;
    showShapeOptions = false;
    showPaperOptions = false;
    clearSelectionState();
    selectOrigin = null;
    selectRect = null;
    shapeDraft = null;
    if (pendingGraphPlacement?.scope === "page") pendingGraphPlacement = null;
    markDirty();
  }

  function activateSelectTool() {
    mode = 'select';
    showPenOptions = false;
    showShapeOptions = false;
    showPaperOptions = false;
    clearSelectionState();
    selectOrigin = null;
    selectRect = null;
    shapeDraft = null;
    if (pendingGraphPlacement?.scope === "page") pendingGraphPlacement = null;
    markDirty();
  }

  // â”€â”€ Erase eraser hit radius in world units â”€â”€
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
    clearPageStrokeTransformHistory();
    if (selectedStrokes.size > 0) {
      const keepSet = new Set(toKeep);
      const nextSelected = new Set(
        [...selectedStrokes].filter((stroke) => keepSet.has(stroke)),
      );
      selectedStrokes = nextSelected;
      selection = nextSelected.size > 0 ? unionBBox(nextSelected) : null;
    }
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of toDelete) {
      if (s.id !== null) void trackInkWrite(deletePageStroke(s.id).catch(() => {}));
      else cancelPendingStrokeCreate(s);
    }
    markDirty();
  }

  // â”€â”€ Touch pan/pinch state â”€â”€
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

  // â”€â”€ Pointer events â”€â”€

  function beginPendingChunkTap(e: PointerEvent, chunk: ChunkInfo) {
    pendingChunkTap = {
      pointerId: e.pointerId,
      pointerType: e.pointerType,
      chunk,
      startClientX: e.clientX,
      startClientY: e.clientY,
      startPoint: getPagePoint(e),
    };
  }

  function pendingChunkTapMoved(e: PointerEvent): boolean {
    if (!pendingChunkTap) return false;
    const dx = e.clientX - pendingChunkTap.startClientX;
    const dy = e.clientY - pendingChunkTap.startClientY;
    return Math.hypot(dx, dy) > CHUNK_TAP_MAX_DISTANCE;
  }

  function onPointerDown(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      if (pendingGraphPlacement?.scope === "page") {
        const { x, y } = pointerToNorm(e.clientX, e.clientY);
        void placePendingPageGraphAt({ x, y });
        e.preventDefault();
        return;
      }
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      touchPointers = [...touchPointers, { id: e.pointerId, x: e.clientX, y: e.clientY }];
      if (touchPointers.length === 1 && mode !== 'erase' && mode !== 'select') {
        const { x, y } = pointerToNorm(e.clientX, e.clientY);
        const hit = chunkAt(x, y);
        if (hit) beginPendingChunkTap(e, hit);
      } else if (touchPointers.length > 1 && pendingChunkTap?.pointerType === 'touch') {
        pendingChunkTap = null;
      }
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
    if (rendering || currentPageId === null) {
      e.preventDefault();
      return;
    }

    if (pendingGraphPlacement?.scope === "page") {
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      void placePendingPageGraphAt({ x, y });
      e.preventDefault();
      return;
    }

    if (mode !== 'erase' && mode !== 'select') {
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      const hit = chunkAt(x, y);
      if (hit) {
        activePointerId = e.pointerId;
        wetCanvas.setPointerCapture(e.pointerId);
        beginPendingChunkTap(e, hit);
        e.preventDefault();
        return;
      }
    }

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
      pagePasteMenuAnchor = null;
      pagePasteHold = null;
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      const graphHit = hitTestPageGraph({ x, y });
      if (graphHit) {
        const graph = pageGraphs.find((entry) => entry.id === graphHit.graphId);
        if (graph) {
          selectedPageGraphId = graphHit.graphId;
          selectedStrokes = new Set();
          selection = {
            x: graph.bboxX,
            y: graph.bboxY,
            width: graph.bboxW,
            height: graph.bboxH,
          };
          pageGraphDrag = {
            graphId: graphHit.graphId,
            mode: graphHit.mode,
            startPoint: { x, y },
            startGraph: cloneGraphObject(graph),
            beforeSnapshot: cloneGraphObjects(pageGraphs),
          };
          selectOrigin = null;
          selectRect = null;
          markDirty();
          e.preventDefault();
          return;
        }
      }
      if (selectedStrokes.size > 0 && selection) {
        const padding = pageSelectionHitPadding();
        if (pointInSelectionBounds({ x, y }, selection, padding.x, padding.y)) {
          const strokeDrag = buildStrokeDragState(strokes, selectedStrokes, { x, y }, selection);
          if (strokeDrag) {
            selectedPageGraphId = null;
            pageGraphDrag = null;
            pageStrokeDrag = strokeDrag;
            selectOrigin = null;
            selectRect = null;
            markDirty();
            e.preventDefault();
            return;
          }
        }
      }
      const canAttemptPaste = !!inkClipboard && inkClipboard.scope === "page" && inkClipboard.strokes.length > 0;
      selectedPageGraphId = null;
      pageGraphDrag = null;
      pageStrokeDrag = null;
      selectedStrokes = new Set();
      selection = null;
      selectRect = null;
      if (canAttemptPaste) {
        pagePasteHold = {
          pointerId: e.pointerId,
          startClientX: e.clientX,
          startClientY: e.clientY,
          startPoint: { x, y },
          startedAt: Date.now(),
        };
        selectOrigin = null;
      } else {
        selectOrigin = { x, y };
      }
      markDirty();
      e.preventDefault();
      return;
    }

    if (mode === "shape") {
      activePointerId = e.pointerId;
      wetCanvas.setPointerCapture(e.pointerId);
      const origin = getPagePoint(e);
      shapeDraft = {
        origin: { x: origin.x, y: origin.y, pressure: 0.5 },
        current: { x: origin.x, y: origin.y, pressure: 0.5 },
      };
      showShapeOptions = false;
      markDirtyWet();
      e.preventDefault();
      return;
    }

    activePointerId = e.pointerId;
    wetCanvas.setPointerCapture(e.pointerId);
    showPenOptions = false;
    showShapeOptions = false;
    isDrawing = true;
    currentStroke = [];
    currentStroke.push(getPagePoint(e));
    lastDrawnStrokeIndex = 0;
    e.preventDefault();
  }

  function onPointerMove(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.map(p => p.id === e.pointerId ? { id: e.pointerId, x: e.clientX, y: e.clientY } : p);
      if (pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === 'touch') {
        if (touchPointers.length > 1) {
          pendingChunkTap = null;
        } else if (!pendingChunkTapMoved(e)) {
          e.preventDefault();
          return;
        } else {
          pendingChunkTap = null;
        }
      }
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
        requestCurrentPdfBitmap();
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

    if (pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === e.pointerType) {
      if (!pendingChunkTapMoved(e)) {
        e.preventDefault();
        return;
      }

      const pending = pendingChunkTap;
      pendingChunkTap = null;

      if (mode === 'select') {
        selectOrigin = { x: pending.startPoint.x, y: pending.startPoint.y };
        selectedStrokes = new Set();
        selectedPageGraphId = null;
        pageGraphDrag = null;
        selection = null;
        const { x, y } = pointerToNorm(e.clientX, e.clientY);
        selectRect = {
          x: Math.min(pending.startPoint.x, x),
          y: Math.min(pending.startPoint.y, y),
          w: Math.abs(x - pending.startPoint.x),
          h: Math.abs(y - pending.startPoint.y),
        };
        markDirtyWet();
        e.preventDefault();
        return;
      }

      if (mode === "shape") {
        const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
        const latest = events[events.length - 1];
        const current = getPagePoint(latest);
        shapeDraft = {
          origin: { x: pending.startPoint.x, y: pending.startPoint.y, pressure: 0.5 },
          current: { x: current.x, y: current.y, pressure: 0.5 },
        };
        showShapeOptions = false;
        markDirtyWet();
        e.preventDefault();
        return;
      }

      showPenOptions = false;
      showShapeOptions = false;
      isDrawing = true;
      currentStroke = [pending.startPoint];
      lastDrawnStrokeIndex = 0;

      const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
      for (const ce of events) {
        const pt = getPagePoint(ce);
        if (shouldAddPoint(pt)) currentStroke.push(pt);
      }
      predictedPoints = getSanitizedPredictedPoints(
        e,
        currentStroke[currentStroke.length - 1] ?? null,
      );
      markDirtyWet();
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
      if (e.pointerId !== activePointerId) return;
      e.preventDefault();
      const drag = pageGraphDrag;
      if (drag) {
        const { x, y } = pointerToNorm(e.clientX, e.clientY);
        const graph = pageGraphs.find((entry) => entry.id === drag.graphId);
        if (!graph) return;

        const deltaX = x - drag.startPoint.x;
        const deltaY = y - drag.startPoint.y;
        let rect = {
          x: drag.startGraph.bboxX,
          y: drag.startGraph.bboxY,
          w: drag.startGraph.bboxW,
          h: drag.startGraph.bboxH,
        };

        if (drag.mode === "move") {
          rect = clampGraphRect({
            x: drag.startGraph.bboxX + deltaX,
            y: drag.startGraph.bboxY + deltaY,
            w: drag.startGraph.bboxW,
            h: drag.startGraph.bboxH,
          });
        } else {
          rect = clampGraphRect({
            x: drag.startGraph.bboxX,
            y: drag.startGraph.bboxY,
            w: drag.startGraph.bboxW + deltaX,
            h: drag.startGraph.bboxH + deltaY,
          });
        }

        const updated: SurfaceGraphObject = {
          ...graph,
          bboxX: rect.x,
          bboxY: rect.y,
          bboxW: rect.w,
          bboxH: rect.h,
        };
        updatePageGraphInState(drag.graphId, updated);
        selection = { x: rect.x, y: rect.y, width: rect.w, height: rect.h };
        return;
      }

      const strokeDrag = pageStrokeDrag;
      if (strokeDrag) {
        const { x, y } = pointerToNorm(e.clientX, e.clientY);
        const rawDeltaX = x - strokeDrag.startPoint.x;
        const rawDeltaY = y - strokeDrag.startPoint.y;
        const delta = { deltaX: rawDeltaX, deltaY: rawDeltaY };
        translateDraggedStrokes(strokeDrag, delta.deltaX, delta.deltaY);
        selection = {
          x: strokeDrag.startSelection.x + delta.deltaX,
          y: strokeDrag.startSelection.y + delta.deltaY,
          width: strokeDrag.startSelection.width,
          height: strokeDrag.startSelection.height,
        };
        markDirty();
        return;
      }

      if (!selectOrigin) return;
      const { x, y } = pointerToNorm(e.clientX, e.clientY);
      selectRect = {
        x: Math.min(selectOrigin.x, x),
        y: Math.min(selectOrigin.y, y),
        w: Math.abs(x - selectOrigin.x),
        h: Math.abs(y - selectOrigin.y),
      };
      markDirtyWet();
      return;
    }

    if (mode === "shape") {
      if (e.pointerId !== activePointerId || !shapeDraft) return;
      e.preventDefault();
      const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
      const latest = events[events.length - 1];
      const current = getPagePoint(latest);
      shapeDraft = {
        origin: shapeDraft.origin,
        current: { x: current.x, y: current.y, pressure: 0.5 },
      };
      markDirtyWet();
      return;
    }

    if (!isDrawing || e.pointerId !== activePointerId) return;
    e.preventDefault();
    const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
    for (const ce of events) {
      const pt = getPagePoint(ce);
      if (shouldAddPoint(pt)) currentStroke.push(pt);
    }
    predictedPoints = getSanitizedPredictedPoints(
      e,
      currentStroke[currentStroke.length - 1] ?? null,
    );
    markDirtyWet();
  }

  function onPointerUp(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      const tappedChunk =
        pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === 'touch'
          ? pendingChunkTap.chunk
          : null;
      if (tappedChunk) pendingChunkTap = null;
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      if (touchPointers.length === 1) {
        lastPinchMid = { x: touchPointers[0].x, y: touchPointers[0].y };
      }
      e.preventDefault();
      if (tappedChunk) {
        void openChunkView(tappedChunk);
      }
      return;
    }

    if (pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === e.pointerType) {
      const tappedChunk = pendingChunkTap.chunk;
      pendingChunkTap = null;
      activePointerId = null;
      isDrawing = false;
      currentStroke = [];
      shapeDraft = null;
      lastDrawnStrokeIndex = 0;
      predictedPoints = [];
      markDirty();
      e.preventDefault();
      void openChunkView(tappedChunk);
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
      if (pageGraphDrag) {
        const before = cloneGraphObjects(pageGraphDrag.beforeSnapshot);
        const target = cloneGraphObjects(pageGraphs);
        const changed = !graphArraysEquivalent(before, target);
        if (changed) {
          void applyPageGraphChange(target, {
            recordHistory: true,
            beforeSnapshot: before,
          }).catch((err) => {
            void appLogWarn(`[graph] failed to save page graph update: ${formatLogError(err)}`);
          });
        }
        pageGraphDrag = null;
        selectOrigin = null;
        selectRect = null;
        markDirty();
        e.preventDefault();
        return;
      }
      if (pageStrokeDrag) {
        const history = buildStrokeTransformHistory(pageStrokeDrag);
        pageStrokeDrag = null;
        if (history) {
          pageStrokeTransformUndo = history;
          pageStrokeTransformRedo = null;
          redoStack = [];
          selectedStrokes = new Set(history.strokes.map((entry) => entry.stroke));
          selection = unionBBox(selectedStrokes);
          void persistPageStrokeUpdates(history.strokes.map((entry) => entry.stroke));
        } else if (selectedStrokes.size > 0) {
          selection = unionBBox(selectedStrokes);
        }
        selectOrigin = null;
        selectRect = null;
        markDirty();
        e.preventDefault();
        return;
      }
      if (selectRect) {
        selectedStrokes = hitTestStrokes(selectRect);
        selectedPageGraphId = null;
        selection = unionBBox(selectedStrokes) ?? { x: selectRect.x, y: selectRect.y, width: selectRect.w, height: selectRect.h };
      }
      selectOrigin = null;
      selectRect = null;
      markDirty();
      e.preventDefault();
      return;
    }

    if (mode === "shape") {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      const draft = shapeDraft;
      shapeDraft = null;
      if (draft) commitPageShape(draft);
      markDirty();
      e.preventDefault();
      return;
    }

    if (e.pointerId !== activePointerId) return;
    if (isDrawing && currentStroke.length >= 2) {
      const completed = currentStroke;
      const stroke: Stroke = {
        id: null,
        clientId: makeStrokeClientId(),
        colour: penColour,
        thickness: penThickness,
        points: completed,
        bbox: computeBBox(completed),
        chunkId: null,
      };
      strokes = [...strokes, stroke];
      redoStack = [];
      clearPageStrokeTransformHistory();
      if (currentPageId !== null) {
        cacheEvict(currentPageId);
        void trackInkWrite(persistPageStrokeCreate(currentPageId, stroke));
      }
    }
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    lastDrawnStrokeIndex = 0;
    predictedPoints = [];
    markDirty();
  }

  function onPointerCancel(e: PointerEvent) {
    if (e.pointerType === 'touch') {
      touchPointers = touchPointers.filter(p => p.id !== e.pointerId);
      if (pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === 'touch') {
        pendingChunkTap = null;
      }
      e.preventDefault();
      return;
    }

    if (pendingChunkTap?.pointerId === e.pointerId && pendingChunkTap.pointerType === e.pointerType) {
      pendingChunkTap = null;
    }

    if (mode === 'erase' || mode === 'select' || mode === "shape") {
      if (e.pointerId !== activePointerId) return;
      activePointerId = null;
      selectOrigin = null;
      selectRect = null;
      pageGraphDrag = null;
      pageStrokeDrag = null;
      shapeDraft = null;
      markDirty();
      return;
    }

    if (e.pointerId !== activePointerId) return;
    isDrawing = false;
    activePointerId = null;
    currentStroke = [];
    lastDrawnStrokeIndex = 0;
    predictedPoints = [];
    shapeDraft = null;
    markDirty();
  }

  // â”€â”€ Render loop â”€â”€

  function renderAll() {
    renderGrid();
    renderPdf();
    renderChunkOverlay();
    renderDryStrokes();
    renderWetLayer();
  }

  function renderChunkOverlay() {
    if (!chunkOverlayCtx || !chunkOverlayCanvas) return;
    const ctx = chunkOverlayCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);
    ctx.clearRect(
      -camera.x / camera.scale, -camera.y / camera.scale,
      chunkOverlayCanvas.width / (camera.scale * dpr), chunkOverlayCanvas.height / (camera.scale * dpr),
    );
    if (!pageSize.w || currentChunks.length === 0) return;

    for (const c of currentChunks) {
      const colour = CHUNK_COLOURS[c.chunk_type];
      if (!colour) continue;
      const tl = normToWorld(c.bbox_x, c.bbox_y);
      const w  = c.bbox_w * pageSize.w;
      const h  = c.bbox_h * pageSize.h;
      const isActive = chunkView?.chunk.id === c.id;
      const barW = 3 / camera.scale;
      ctx.save();
      // Subtle tint over the full bbox
      ctx.globalAlpha = isActive ? 0.14 : 0.06;
      ctx.fillStyle = colour.accent;
      ctx.fillRect(tl.x, tl.y, w, h);
      // Marginal bar on the left edge
      ctx.globalAlpha = isActive ? 0.95 : 0.65;
      ctx.fillStyle = colour.accent;
      ctx.fillRect(tl.x, tl.y, barW, h);
      ctx.restore();
    }
  }

  function renderGrid() {
    if (!gridCtx || !gridCanvas) return;
    const ctx = gridCtx;
    const w = gridCanvas.width;
    const h = gridCanvas.height;
    const dpr = window.devicePixelRatio || 1;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const gridWorld = paperSettings.spacing;
    const pattern = paperSettings.pattern;
    const DOT_R = 1.3;
    const spacing = gridWorld * camera.scale;
    if (spacing < 8 || spacing > 300) return;

    const startW = screenToWorld(0, 0);
    const startX = Math.floor(startW.x / gridWorld) * gridWorld;
    const startY = Math.floor(startW.y / gridWorld) * gridWorld;

    if (pattern === "dotted") {
      ctx.fillStyle = paperPatternColour(paperPatternAlpha(pattern));
      for (let wx = startX; ; wx += gridWorld) {
        const sx = (wx * camera.scale + camera.x) * dpr;
        if (sx > w + DOT_R * dpr) break;
        if (sx < -DOT_R * dpr) continue;
        for (let wy = startY; ; wy += gridWorld) {
          const sy = (wy * camera.scale + camera.y) * dpr;
          if (sy > h + DOT_R * dpr) break;
          if (sy < -DOT_R * dpr) continue;
          ctx.beginPath();
          ctx.arc(sx, sy, DOT_R, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      return;
    }

    ctx.strokeStyle = paperPatternColour(paperPatternAlpha(pattern));
    ctx.lineWidth = Math.max(1, dpr);
    ctx.beginPath();
    if (pattern === "grid") {
      for (let wx = startX; ; wx += gridWorld) {
        const sx = Math.round((wx * camera.scale + camera.x) * dpr) + 0.5;
        if (sx > w + dpr) break;
        if (sx < -dpr) continue;
        ctx.moveTo(sx, 0);
        ctx.lineTo(sx, h);
      }
    }
    for (let wy = startY; ; wy += gridWorld) {
      const sy = (wy * camera.scale + camera.y) * dpr;
      if (sy > h + dpr) break;
      if (sy < -dpr) continue;
      const crispY = Math.round(sy) + 0.5;
      ctx.moveTo(0, crispY);
      ctx.lineTo(w, crispY);
    }
    ctx.stroke();
  }

  function getNotebookDocumentPageCount(): number {
    let maxPage = 1;
    for (const stroke of strokes) {
      if (!strokeHasFiniteBounds(stroke)) continue;
      maxPage = Math.max(maxPage, Math.ceil(Math.max(0, stroke.bbox.maxY)));
    }
    for (const graph of pageGraphs) {
      if (!graphHasFiniteChunkBounds(graph)) continue;
      maxPage = Math.max(maxPage, Math.ceil(Math.max(0, graph.bboxY + graph.bboxH)));
    }
    return maxPage + INK_DOCUMENT_BUFFER_PAGES;
  }

  function drawInkPageGuides(
    ctx: CanvasRenderingContext2D,
    options: {
      originX?: number;
      originY?: number;
      pageWidth: number;
      pageHeight: number;
      pageCount: number;
      scale: number;
      visMinY: number;
      visMaxY: number;
      labelPrefix?: string;
    },
  ) {
    const {
      originX = 0,
      originY = 0,
      pageWidth,
      pageHeight,
      pageCount,
      scale,
      visMinY,
      visMaxY,
      labelPrefix = "Page",
    } = options;
    if (pageWidth <= 0 || pageHeight <= 0 || pageCount <= 0) return;
    const px = 1 / scale;
    const startPage = Math.max(0, Math.floor((visMinY - originY) / pageHeight));
    const endPage = Math.min(pageCount - 1, Math.floor((visMaxY - originY) / pageHeight));

    for (let pageIndex = startPage; pageIndex <= endPage; pageIndex += 1) {
      const pageTop = originY + pageIndex * pageHeight;
      const inset = px * 0.5;
      const label = `${labelPrefix} ${pageIndex + 1}`;
      const fontSize = 11 * px;
      const padX = 8 * px;
      const padY = 4 * px;
      const labelX = originX + 14 * px;
      const labelY = pageTop + 14 * px;
      const darkPaper = isPaperBackgroundDark();

      ctx.save();
      ctx.strokeStyle = darkPaper ? "rgba(255, 255, 255, 0.22)" : "rgba(100, 116, 139, 0.32)";
      ctx.lineWidth = 1.2 * px;
      ctx.strokeRect(
        originX + inset,
        pageTop + inset,
        Math.max(0, pageWidth - px),
        Math.max(0, pageHeight - px),
      );

      ctx.font = `${fontSize}px Inter, system-ui, sans-serif`;
      ctx.textBaseline = "top";
      const labelW = ctx.measureText(label).width + padX * 2;
      const labelH = fontSize + padY * 2;
      ctx.fillStyle = darkPaper ? "rgba(15, 23, 42, 0.88)" : "rgba(248, 250, 252, 0.94)";
      ctx.fillRect(labelX, labelY, labelW, labelH);
      ctx.strokeStyle = darkPaper ? "rgba(255, 255, 255, 0.28)" : "rgba(148, 163, 184, 0.45)";
      ctx.lineWidth = px;
      ctx.strokeRect(labelX, labelY, labelW, labelH);
      ctx.fillStyle = darkPaper ? "rgba(248, 250, 252, 0.88)" : "rgba(51, 65, 85, 0.82)";
      ctx.fillText(label, labelX + padX, labelY + padY);
      ctx.restore();
    }
  }

  function renderNotebookPageGuides(ctx: CanvasRenderingContext2D) {
    if (!canvasContainer || !pageSize.w || !pageSize.h) return;
    const visMinY = -camera.y / camera.scale;
    const visMaxY = visMinY + canvasContainer.clientHeight / camera.scale;
    const visiblePageCount = Math.max(
      1,
      Math.ceil(Math.max(0, visMaxY - pageOrigin.y) / pageSize.h) + 1,
    );
    drawInkPageGuides(ctx, {
      originX: pageOrigin.x,
      originY: pageOrigin.y,
      pageWidth: pageSize.w,
      pageHeight: pageSize.h,
      pageCount: Math.max(getNotebookDocumentPageCount(), visiblePageCount),
      scale: camera.scale,
      visMinY,
      visMaxY,
    });
  }

  function renderPdf() {
    if (!pdfCtx || !pdfCanvas) return;
    const ctx = pdfCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);
    ctx.clearRect(
      -camera.x / camera.scale, -camera.y / camera.scale,
      pdfCanvas.width / (camera.scale * dpr), pdfCanvas.height / (camera.scale * dpr),
    );
    if (isNotebookDocument()) {
      renderNotebookPageGuides(ctx);
      return;
    }
    if (!currentPdfBitmap) return;
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

    for (const stroke of strokes) {
      if (stroke.points.length < 2) continue;
      if (
        stroke.bbox.maxX < visMinX || stroke.bbox.minX > visMaxX ||
        stroke.bbox.maxY < visMinY || stroke.bbox.minY > visMaxY
      ) continue;
      drawStrokePoints(ctx, stroke.points, stroke.colour, stroke.thickness, 0);
    }

    for (const graph of pageGraphs) {
      const minX = graph.bboxX;
      const minY = graph.bboxY;
      const maxX = graph.bboxX + graph.bboxW;
      const maxY = graph.bboxY + graph.bboxH;
      if (
        maxX < visMinX || minX > visMaxX ||
        maxY < visMinY || minY > visMaxY
      ) continue;

      const worldX = pageOrigin.x + graph.bboxX * pageSize.w;
      const worldY = pageOrigin.y + graph.bboxY * pageSize.h;
      const worldW = graph.bboxW * pageSize.w;
      const worldH = graph.bboxH * pageSize.h;
      drawGraphCard(
        ctx,
        graphObjectToSpec(graph),
        worldX,
        worldY,
        worldW,
        worldH,
        {
          selected: selectedPageGraphId != null && graph.id === selectedPageGraphId,
          showResizeHandle: mode === "select" && selectedPageGraphId != null && graph.id === selectedPageGraphId,
        },
      );
    }

    // Draw selection highlight on dry canvas
    if (selectedStrokes.size > 0) {
      ctx.save();
      ctx.strokeStyle = "rgba(255, 140, 0, 0.9)";
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      for (const stroke of selectedStrokes) {
        if (stroke.points.length < 2) continue;
        drawStrokePoints(ctx, stroke.points, "rgba(255, 140, 0, 0.9)", stroke.thickness, 0);
      }
      ctx.restore();
    }
  }

  function renderWetLayer() {
    if (!wetCtx || !wetCanvas) return;
    const ctx = wetCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(camera.scale * dpr, 0, 0, camera.scale * dpr, camera.x * dpr, camera.y * dpr);
    ctx.clearRect(
      -camera.x / camera.scale, -camera.y / camera.scale,
      wetCanvas.width / (camera.scale * dpr), wetCanvas.height / (camera.scale * dpr),
    );

    if (isDrawing && currentStroke.length >= 2) {
      drawStrokePoints(ctx, currentStroke, penColour, penThickness, 0);
      lastDrawnStrokeIndex = currentStroke.length;
      if (predictedPoints.length > 0) {
        ctx.save();
        ctx.globalAlpha = 0.35;
        const anchor = currentStroke.slice(-2);
        drawStrokePoints(ctx, [...anchor, ...predictedPoints], penColour, penThickness, 0);
        ctx.restore();
      }
    } else if (mode === "shape" && shapeDraft) {
      const previewStrokes = getPageShapeStrokes(shapeDraft);
      for (const previewStroke of previewStrokes) {
        if (previewStroke.length < 2) continue;
        drawStrokePoints(ctx, previewStroke, penColour, penThickness, 0);
      }
    } else if (mode === "select") {
      const activeRect = selectRect
        ?? (selection
          ? {
            x: selection.x,
            y: selection.y,
            w: selection.width,
            h: selection.height,
          }
          : null);
      if (!activeRect) return;
      const tl = normToWorld(activeRect.x, activeRect.y);
      const br = normToWorld(activeRect.x + activeRect.w, activeRect.y + activeRect.h);
      ctx.save();
      ctx.strokeStyle = "rgba(57, 108, 216, 0.9)";
      ctx.lineWidth = 1.5 / camera.scale;
      ctx.setLineDash([5 / camera.scale, 4 / camera.scale]);
      ctx.strokeRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.fillStyle = "rgba(57, 108, 216, 0.08)";
      ctx.fillRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.restore();
    }
  }

  function drawStrokePoints(
    ctx: CanvasRenderingContext2D,
    pts: Point[],
    colour: string,
    thickness: number,
    startIdx: number,
  ) {
    drawStrokePointsFn(
      ctx, pts, colour, thickness, startIdx,
      camera.scale, pageOrigin.x, pageOrigin.y, pageSize.w, pageSize.h,
    );
  }

  // â”€â”€ Hit test / selection helpers â”€â”€

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

  function pointInSelectionBounds(
    point: { x: number; y: number },
    bounds: SelectionBounds,
    padX = 0,
    padY = 0,
  ): boolean {
    return (
      point.x >= bounds.x - padX
      && point.x <= bounds.x + bounds.width + padX
      && point.y >= bounds.y - padY
      && point.y <= bounds.y + bounds.height + padY
    );
  }

  function buildStrokeDragState(
    allStrokes: Stroke[],
    selected: Set<Stroke>,
    startPoint: { x: number; y: number },
    startSelection: SelectionBounds,
  ): InkStrokeDragState | null {
    const ordered = getOrderedSelectedStrokes(allStrokes, selected);
    if (ordered.length === 0) return null;
    return {
      startPoint,
      startSelection: {
        x: startSelection.x,
        y: startSelection.y,
        width: startSelection.width,
        height: startSelection.height,
      },
      strokes: ordered.map((stroke) => ({
        stroke,
        points: cloneInkPoints(stroke.points),
        bbox: cloneStrokeBBox(stroke.bbox),
      })),
    };
  }

  function translateDraggedStrokes(
    drag: InkStrokeDragState,
    deltaX: number,
    deltaY: number,
  ) {
    for (const snapshot of drag.strokes) {
      snapshot.stroke.points = snapshot.points.map(({ x, y, pressure }) => ({
        x: x + deltaX,
        y: y + deltaY,
        pressure,
      }));
      snapshot.stroke.bbox = {
        minX: snapshot.bbox.minX + deltaX,
        minY: snapshot.bbox.minY + deltaY,
        maxX: snapshot.bbox.maxX + deltaX,
        maxY: snapshot.bbox.maxY + deltaY,
      };
    }
  }

  function buildStrokeTransformHistory(drag: InkStrokeDragState): StrokeTransformHistoryEntry | null {
    const transformed = drag.strokes
      .map((snapshot) => ({
        stroke: snapshot.stroke,
        beforePoints: cloneInkPoints(snapshot.points),
        beforeBBox: cloneStrokeBBox(snapshot.bbox),
        afterPoints: cloneInkPoints(snapshot.stroke.points),
        afterBBox: cloneStrokeBBox(snapshot.stroke.bbox),
      }))
      .filter((entry) => (
        entry.beforeBBox.minX !== entry.afterBBox.minX
        || entry.beforeBBox.minY !== entry.afterBBox.minY
        || entry.beforeBBox.maxX !== entry.afterBBox.maxX
        || entry.beforeBBox.maxY !== entry.afterBBox.maxY
      ));
    if (transformed.length === 0) return null;
    return { strokes: transformed };
  }

  function applyStrokeTransformHistory(
    entry: StrokeTransformHistoryEntry,
    direction: "before" | "after",
  ) {
    for (const item of entry.strokes) {
      item.stroke.points = cloneInkPoints(
        direction === "before" ? item.beforePoints : item.afterPoints,
      );
      item.stroke.bbox = cloneStrokeBBox(
        direction === "before" ? item.beforeBBox : item.afterBBox,
      );
    }
  }

  function pageSelectionHitPadding() {
    const hitRadiusWorld = 12 / Math.max(camera.scale, 0.001);
    return {
      x: pageSize.w > 0 ? hitRadiusWorld / pageSize.w : 0.02,
      y: pageSize.h > 0 ? hitRadiusWorld / pageSize.h : 0.02,
    };
  }

  function chunkSelectionHitPadding() {
    const hitRadiusWorld = 12 / Math.max(chunkCamera.scale, 0.001);
    const { w, h } = getChunkPageWorldSize();
    return {
      x: w > 0 ? hitRadiusWorld / w : 0.02,
      y: h > 0 ? hitRadiusWorld / h : 0.02,
    };
  }

  function clampChunkStrokeDragDelta(
    bounds: SelectionBounds,
    deltaX: number,
    deltaY: number,
  ) {
    return {
      deltaX: Math.max(-bounds.x, Math.min(1 - (bounds.x + bounds.width), deltaX)),
      deltaY: Math.max(-bounds.y, deltaY),
    };
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

  // â”€â”€ Canvas setup / resize â”€â”€

  function setupCanvases() {
    if (!canvasContainer || !gridCanvas || !pdfCanvas || !chunkOverlayCanvas || !dryCanvas || !wetCanvas) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvasContainer.clientWidth;
    const h = canvasContainer.clientHeight;
    for (const c of [gridCanvas, pdfCanvas, chunkOverlayCanvas, dryCanvas, wetCanvas]) {
      c.width  = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    gridCtx = gridCanvas.getContext("2d");
    pdfCtx  = pdfCanvas.getContext("2d");
    chunkOverlayCtx = chunkOverlayCanvas.getContext("2d");
    dryCtx  = dryCanvas.getContext("2d")!;
    wetCtx  = wetCanvas.getContext("2d")!;
    if (dryCtx) { dryCtx.lineCap = "round"; dryCtx.lineJoin = "round"; }
    if (wetCtx) { wetCtx.lineCap = "round"; wetCtx.lineJoin = "round"; }
    if (isNotebookDocument()) configureNotebookCanvasLayout();
    markDirty();
  }

  let containerResizeObserver: ResizeObserver | null = null;

  function observeContainerResize(node: HTMLElement) {
    containerResizeObserver?.disconnect();
    containerResizeObserver = new ResizeObserver(() => {
      cachedContainerRect = null;
      setupCanvases();
      requestCurrentPdfBitmap();
    });
    containerResizeObserver.observe(node);
  }

  // â”€â”€ Camera helpers â”€â”€

  function zoomAt(screenX: number, screenY: number, factor: number) {
    const newScale = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, camera.scale * factor));
    const wx = (screenX - camera.x) / camera.scale;
    const wy = (screenY - camera.y) / camera.scale;
    camera.x = screenX - wx * newScale;
    camera.y = screenY - wy * newScale;
    camera.scale = newScale;
    markDirty();
    requestCurrentPdfBitmap();
  }

  function centreOnPage() {
    if (!pageSize.w || !canvasContainer) return;
    const vw = canvasContainer.clientWidth;
    camera.x = (vw - pageSize.w * camera.scale) / 2;
    camera.y = 0;
    markDirty();
  }

  function configureNotebookCanvasLayout(options: { recenter?: boolean } = {}) {
    if (!canvasContainer || !isNotebookDocument()) return;
    const pageDisplayW = getPageDisplayWidth();
    pageOrigin = { x: 0, y: 0 };
    pageSize = {
      w: pageDisplayW,
      h: pageDisplayW * NOTEBOOK_PAGE_ASPECT,
    };
    currentPdfPagePoints = { w: pageSize.w, h: pageSize.h };
    currentPdfBitmap = null;
    currentPdfBitmapKey = null;
    if (options.recenter) {
      centreOnPage();
    } else {
      markDirty();
    }
  }

  // â”€â”€ PDF loading â”€â”€

  async function loadPdfPage(pageNum: number) {
    if (!selectedBook) return;
    if (!canvasContainer) {
      await tick();
      setupCanvases();
    }
    if (!canvasContainer) throw new Error("Viewer canvas is not ready");
    rendering = true;
    const loadVersion = ++pdfLoadVersion;
    void appLogInfo(`[viewer] loading doc=${selectedBook.id} page=${pageNum}`);
    try {
      const pageDisplayW = getPageDisplayWidth();
      const previewPixelWidth = getPreviewPdfPixelWidth(pageDisplayW);
      currentPdfBitmap = null;
      currentPdfBitmapKey = null;
      const { key, rendered } = await fetchPdfBitmap(pageNum, previewPixelWidth);
      if (loadVersion !== pdfLoadVersion) return;

      currentPdfPagePoints = { w: rendered.pageWidthPoints, h: rendered.pageHeightPoints };
      pageSize = {
        w: pageDisplayW,
        h: pageDisplayW * (currentPdfPagePoints.h / currentPdfPagePoints.w),
      };
      currentPdfBitmap = rendered.bitmap;
      currentPdfBitmapKey = key;
      centreOnPage();
      markDirty();
      prefetchPdfBitmap(pageNum - 1, previewPixelWidth);
      prefetchPdfBitmap(pageNum + 1, previewPixelWidth);
      const upgradePixelWidth = getUpgradePdfPixelWidth();
      if (upgradePixelWidth > previewPixelWidth * 1.1) {
        void requestCurrentPdfBitmap();
      }
      void appLogInfo(
        `[viewer] loaded doc=${selectedBook.id} page=${pageNum} bitmap=${rendered.bitmapWidth}x${rendered.bitmapHeight}`,
      );
    } catch (err) {
      void appLogError(
        `[viewer] failed to load doc=${selectedBook.id} page=${pageNum}: ${formatLogError(err)}`,
      );
      throw err;
    } finally {
      rendering = false;
    }
  }

  async function loadNotebookCanvas(canvasNumber: number) {
    if (!selectedBook || !isNotebookDocument(selectedBook)) return;
    if (!canvasContainer) {
      await tick();
      setupCanvases();
    }
    if (!canvasContainer) throw new Error("Notebook canvas is not ready");
    rendering = true;
    void appLogInfo(`[notebook] loading doc=${selectedBook.id} canvas=${canvasNumber}`);
    try {
      configureNotebookCanvasLayout({ recenter: true });
      const canvas = await invoke<NotebookCanvasOutput>("get_or_create_notebook_canvas", {
        sourceDocumentId: selectedBook.id,
        canvasNumber,
      });
      currentPageId = canvas.page_id;
      currentPageSurfaceId = canvas.surface_id;
      clearNotebookInkContextCache();
      totalPages = Math.max(1, canvas.canvas_count);
      if (selectedBook.page_count !== totalPages) {
        const updated = { ...selectedBook, page_count: totalPages };
        selectedBook = updated;
        onBookUpdated(updated);
      }
      await loadAndDrawSurfaceStrokes(canvas.surface_id);
      const pageSurface = await ensurePageSurface(canvas.page_id);
      if (!selectedBook || currentPage !== canvasNumber || currentPageId !== canvas.page_id) return;
      currentPageSurfaceId = canvas.surface_id;
      pageGraphs = cloneGraphObjects(pageSurface.graphs);
      currentChunks = [];
      chunkNavigationPageChunks = [];
      currentPageChunkingActive = false;
      currentChunkingStatus = "done";
      markDirty();
      void appLogInfo(`[notebook] loaded doc=${selectedBook.id} canvas=${canvasNumber} count=${totalPages}`);
    } catch (err) {
      void appLogError(`[notebook] failed to load doc=${selectedBook?.id ?? "unknown"} canvas=${canvasNumber}: ${formatLogError(err)}`);
      throw err;
    } finally {
      rendering = false;
    }
  }

  // â”€â”€ Page navigation â”€â”€

  function getModelOptions(task: AiTask, provider: AnyProvider): ModelOption[] {
    if (task === "chunking") return CHUNKING_MODEL_OPTIONS[provider as ChunkingProvider] ?? [];
    if (task === "chat") return CHAT_MODEL_OPTIONS[provider as ChatProvider] ?? [];
    return VISION_MODEL_OPTIONS[provider as VisionProvider] ?? [];
  }

  function isModelInOptions(task: AiTask, provider: AnyProvider, model: string): boolean {
    return getModelOptions(task, provider).some((option) => option.value === model);
  }

  function refreshCustomModelMode(settings: AiTaskSettings = aiTaskSettings) {
    customModelMode = {
      chunking: !isModelInOptions("chunking", settings.chunking.provider, settings.chunking.model),
      chat: !isModelInOptions("chat", settings.chat.provider, settings.chat.model),
      vision: !isModelInOptions("vision", settings.vision.provider, settings.vision.model),
    };
  }

  function setCustomModelMode(task: AiTask, enabled: boolean) {
    if (task === "chunking") {
      customModelMode = { ...customModelMode, chunking: enabled };
      return;
    }
    if (task === "chat") {
      customModelMode = { ...customModelMode, chat: enabled };
      return;
    }
    customModelMode = { ...customModelMode, vision: enabled };
  }

  function loadAiTaskSettings(): AiTaskSettings {
    const defaults = defaultAiTaskSettings();
    if (typeof localStorage === "undefined") return defaults;

    let loaded = defaults;
    const stored = localStorage.getItem(AI_TASK_SETTINGS_STORAGE_KEY);
    if (stored) {
      try {
        const parsed = JSON.parse(stored) as Partial<AiTaskSettings>;
        const chunkingProvider = isChunkingProvider(parsed?.chunking?.provider ?? null)
          ? parsed.chunking!.provider
          : defaults.chunking.provider;
        const chunkingModel = typeof parsed?.chunking?.model === "string"
          ? parsed.chunking.model.trim()
          : "";

        const chatProvider = isChatProvider(parsed?.chat?.provider ?? null)
          ? parsed.chat!.provider
          : defaults.chat.provider;
        const chatModel = typeof parsed?.chat?.model === "string"
          ? parsed.chat.model.trim()
          : "";

        const visionProvider = isVisionProvider(parsed?.vision?.provider ?? null)
          ? parsed.vision!.provider
          : defaults.vision.provider;
        const visionModel = typeof parsed?.vision?.model === "string"
          ? parsed.vision.model.trim()
          : "";

        loaded = {
          chunking: {
            provider: chunkingProvider,
            model: chunkingModel || getDefaultModelForTask("chunking", chunkingProvider),
          },
          chat: {
            provider: chatProvider,
            model: chatModel || getDefaultModelForTask("chat", chatProvider),
          },
          vision: {
            provider: visionProvider,
            model: visionModel || getDefaultModelForTask("vision", visionProvider),
          },
        };
      } catch {
        loaded = defaults;
      }
    }

    if (!stored) {
      const legacy = localStorage.getItem(LEGACY_CHUNKING_PROVIDER_STORAGE_KEY);
      if (isChunkingProvider(legacy)) {
        loaded = {
          ...loaded,
          chunking: {
            provider: legacy,
            model: getDefaultModelForTask("chunking", legacy),
          },
        };
      }
    }

    localStorage.setItem(AI_TASK_SETTINGS_STORAGE_KEY, JSON.stringify(loaded));
    localStorage.removeItem(LEGACY_CHUNKING_PROVIDER_STORAGE_KEY);
    return loaded;
  }

  let strokeClientIdSerial = 0;

  function makeStrokeClientId(): string {
    strokeClientIdSerial += 1;
    return `stroke-${Date.now()}-${strokeClientIdSerial}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function ensureStrokeClientId(stroke: Stroke): string {
    if (!stroke.clientId) {
      stroke.clientId = makeStrokeClientId();
    }
    return stroke.clientId;
  }

  function findStrokeByClientId(allStrokes: Stroke[], clientId: string): Stroke | null {
    return allStrokes.find((stroke) => stroke.clientId === clientId) ?? null;
  }

  function persistAiTaskSettings() {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(AI_TASK_SETTINGS_STORAGE_KEY, JSON.stringify(aiTaskSettings));
    onAiTaskSettingsChange(aiTaskSettings);
  }

  function updateAiTaskSetting(task: AiTask, next: { provider: AnyProvider; model: string }) {
    if (task === "chunking") {
      aiTaskSettings = {
        ...aiTaskSettings,
        chunking: {
          provider: next.provider as ChunkingProvider,
          model: next.model,
        },
      };
    } else if (task === "chat") {
      aiTaskSettings = {
        ...aiTaskSettings,
        chat: {
          provider: next.provider as ChatProvider,
          model: next.model,
        },
      };
    } else {
      aiTaskSettings = {
        ...aiTaskSettings,
        vision: {
          provider: next.provider as VisionProvider,
          model: next.model,
        },
      };
    }
    persistAiTaskSettings();
  }

  function setTaskProvider(task: AiTask, provider: AnyProvider) {
    const defaultModel = getDefaultModelForTask(task, provider);
    updateAiTaskSetting(task, { provider, model: defaultModel });
    setCustomModelMode(task, false);
    void appLogInfo(`[settings] ${task} provider switched to ${provider} model=${defaultModel || "auto"}`);
  }

  function setTaskModel(task: AiTask, model: string) {
    if (task === "chunking") {
      updateAiTaskSetting(task, {
        provider: aiTaskSettings.chunking.provider,
        model,
      });
      return;
    }
    if (task === "chat") {
      updateAiTaskSetting(task, {
        provider: aiTaskSettings.chat.provider,
        model,
      });
      return;
    }
    updateAiTaskSetting(task, {
      provider: aiTaskSettings.vision.provider,
      model,
    });
  }

  function modelSelectValue(task: AiTask): string {
    const isCustom = task === "chunking"
      ? customModelMode.chunking
      : task === "chat"
        ? customModelMode.chat
        : customModelMode.vision;
    if (isCustom) return CUSTOM_MODEL_VALUE;

    const provider = task === "chunking"
      ? aiTaskSettings.chunking.provider
      : task === "chat"
        ? aiTaskSettings.chat.provider
        : aiTaskSettings.vision.provider;
    const model = task === "chunking"
      ? aiTaskSettings.chunking.model
      : task === "chat"
        ? aiTaskSettings.chat.model
        : aiTaskSettings.vision.model;
    const options = getModelOptions(task, provider);
    return options.some((option) => option.value === model) ? model : CUSTOM_MODEL_VALUE;
  }

  function setModelFromSelect(task: AiTask, value: string) {
    if (value === CUSTOM_MODEL_VALUE) {
      setCustomModelMode(task, true);
      return;
    }
    setCustomModelMode(task, false);
    setTaskModel(task, value);
  }

  function openAiKeySettings(firstRun = false) {
    aiKeySheetFirstRun = firstRun;
    aiSettingsError = null;
    batchChunkError = null;
    batchChunkFeedback = null;
    openaiApiKeyInput = "";
    geminiApiKeyInput = "";
    deepseekApiKeyInput = "";
    zaiApiKeyInput = "";
    clearOpenaiApiKey = false;
    clearGeminiApiKey = false;
    clearDeepseekApiKey = false;
    clearZaiApiKey = false;
    const defaultChunkPage = selectedBook
      ? Math.max(1, Math.min(totalPages > 0 ? totalPages : currentPage, currentPage))
      : 1;
    batchChunkStartInput = String(defaultChunkPage);
    batchChunkEndInput = String(defaultChunkPage);
    showAiKeySheet = true;
  }

  function closeAiKeySettings() {
    if (aiSettingsSaving || batchChunkStarting) return;
    showAiKeySheet = false;
    aiKeySheetFirstRun = false;
    aiSettingsError = null;
  }

  function applyAiSettingsState(state: AiSettingsState) {
    aiSettings = state;
    if (!state.running_on_android) return;

    let next = aiTaskSettings;
    let changed = false;
    if (next.chunking.provider === "ollama") {
      next = {
        ...next,
        chunking: {
          provider: "deepseek",
          model: getDefaultModelForTask("chunking", "deepseek"),
        },
      };
      changed = true;
    }
    if (next.chat.provider === "ollama") {
      next = {
        ...next,
        chat: {
          provider: "gemini",
          model: getDefaultModelForTask("chat", "gemini"),
        },
      };
      changed = true;
    }
    if (next.vision.provider === "ollama") {
      next = {
        ...next,
        vision: {
          provider: "zai",
          model: getDefaultModelForTask("vision", "zai"),
        },
      };
      changed = true;
    }
    if (changed) {
      aiTaskSettings = next;
      persistAiTaskSettings();
      refreshCustomModelMode(next);
    }
  }

  async function loadAiSettings() {
    try {
      const state = await invoke<AiSettingsState>("get_ai_settings_state");
      applyAiSettingsState(state);
      if (state.running_on_android && !state.setup_complete) {
        openAiKeySettings(true);
      }
    } catch (err) {
      void appLogWarn(`[settings] failed to load AI settings: ${formatLogError(err)}`);
    }
  }

  async function saveAiKeySettings(setupComplete: boolean) {
    aiSettingsSaving = true;
    aiSettingsError = null;
    try {
      const openaiKey = openaiApiKeyInput.trim();
      const geminiKey = geminiApiKeyInput.trim();
      const deepseekKey = deepseekApiKeyInput.trim();
      const zaiKey = zaiApiKeyInput.trim();
      const state = await invoke<AiSettingsState>("save_ai_api_keys", {
        openaiApiKey: openaiKey.length > 0 ? openaiKey : null,
        geminiApiKey: geminiKey.length > 0 ? geminiKey : null,
        deepseekApiKey: deepseekKey.length > 0 ? deepseekKey : null,
        zaiApiKey: zaiKey.length > 0 ? zaiKey : null,
        clearOpenaiApiKey,
        clearGeminiApiKey,
        clearDeepseekApiKey,
        clearZaiApiKey,
        setupComplete,
      });
      applyAiSettingsState(state);
      showAiKeySheet = false;
      aiKeySheetFirstRun = false;
      aiSettingsError = null;
      void appLogInfo(
        `[settings] AI keys updated openai=${state.openai_api_key_set} gemini=${state.gemini_api_key_set} deepseek=${state.deepseek_api_key_set} zai=${state.zai_api_key_set}`,
      );
    } catch (err) {
      aiSettingsError = formatLogError(err);
      void appLogError(`[settings] failed to save AI keys: ${formatLogError(err)}`);
    } finally {
      aiSettingsSaving = false;
    }
  }

  async function completeAiKeySetupWithoutSaving() {
    aiSettingsSaving = true;
    aiSettingsError = null;
    try {
      const state = await invoke<AiSettingsState>("save_ai_api_keys", {
        openaiApiKey: null,
        geminiApiKey: null,
        deepseekApiKey: null,
        zaiApiKey: null,
        clearOpenaiApiKey: false,
        clearGeminiApiKey: false,
        clearDeepseekApiKey: false,
        clearZaiApiKey: false,
        setupComplete: true,
      });
      applyAiSettingsState(state);
      showAiKeySheet = false;
      aiKeySheetFirstRun = false;
      aiSettingsError = null;
    } catch (err) {
      aiSettingsError = formatLogError(err);
      void appLogError(`[settings] failed to skip AI setup: ${formatLogError(err)}`);
    } finally {
      aiSettingsSaving = false;
    }
  }

  function saveAiKeyForm(event: SubmitEvent) {
    event.preventDefault();
    void saveAiKeySettings(true);
  }

  function dismissAiKeySettings() {
    if (aiSettingsSaving || batchChunkStarting) return;
    if (aiKeySheetFirstRun) {
      void completeAiKeySetupWithoutSaving();
    } else {
      closeAiKeySettings();
    }
  }

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
    if (
      pendingDeleteSourceDocumentId !== null
      && !sourceDocuments.some((doc) => doc.id === pendingDeleteSourceDocumentId)
    ) {
      pendingDeleteSourceDocumentId = null;
    }
    void appLogInfo(`[library] loaded ${sourceDocuments.length} documents`);
  }

  function syncPastPaperInstructionInputs(book: SourceDocument | null) {
    if (!book || book.document_mode !== "past_paper") {
      pastPaperInstructionPagesEnabled = true;
      pastPaperInstructionStartInput = "1";
      pastPaperInstructionEndInput = "1";
      pastPaperInstructionError = null;
      pastPaperInstructionFeedback = null;
      pastPaperInstructionCheckError = null;
      pastPaperInstructionCheckFeedback = null;
      pastPaperInstructionCheckPreview = null;
      return;
    }
    if (book.instruction_page_start != null && book.instruction_page_end != null) {
      pastPaperInstructionPagesEnabled = true;
      pastPaperInstructionStartInput = `${book.instruction_page_start}`;
      pastPaperInstructionEndInput = `${book.instruction_page_end}`;
    } else {
      pastPaperInstructionPagesEnabled = false;
      pastPaperInstructionStartInput = "1";
      pastPaperInstructionEndInput = "1";
    }
    pastPaperInstructionError = null;
    pastPaperInstructionFeedback = null;
    pastPaperInstructionCheckError = null;
    pastPaperInstructionCheckFeedback = null;
    pastPaperInstructionCheckPreview = null;
  }

  function applyUpdatedSourceDocument(updated: SourceDocument) {
    sourceDocuments = sourceDocuments.map((doc) => doc.id === updated.id ? updated : doc);
    if (selectedBook?.id === updated.id) {
      selectedBook = updated;
      syncPastPaperInstructionInputs(updated);
    }
    onBookUpdated(updated);
  }

  async function savePastPaperInstructionRange() {
    const book = selectedBook;
    if (!book || book.document_mode !== "past_paper" || pastPaperInstructionSaving) return;

    pastPaperInstructionError = null;
    pastPaperInstructionFeedback = null;
    pastPaperInstructionCheckError = null;
    pastPaperInstructionCheckFeedback = null;
    pastPaperInstructionCheckPreview = null;
    pastPaperInstructionSaving = true;
    try {
      let startPage: number | null = null;
      let endPage: number | null = null;
      if (pastPaperInstructionPagesEnabled) {
        const start = parseBatchChunkPageInput(pastPaperInstructionStartInput);
        const end = parseBatchChunkPageInput(pastPaperInstructionEndInput);
        if (start == null || end == null) {
          throw new Error("Enter valid instruction page numbers.");
        }
        if (start > end) {
          throw new Error("Instruction start page must be less than or equal to end page.");
        }
        if (totalPages > 0 && end > totalPages) {
          throw new Error(`Instruction range must be within 1-${totalPages}.`);
        }
        startPage = start;
        endPage = end;
      }

      const updated = await invoke<SourceDocument>("save_past_paper_instruction_range", {
        sourceDocumentId: book.id,
        startPage,
        endPage,
      });
      applyUpdatedSourceDocument(updated);
      if (startPage == null || endPage == null) {
        pastPaperInstructionFeedback = "Saved: no instruction pages.";
      } else {
        pastPaperInstructionFeedback = `Saved instruction pages ${startPage}-${endPage}.`;
      }
      await appLogInfo(
        `[past-paper] instruction range saved doc=${book.id} start=${startPage ?? "none"} end=${endPage ?? "none"}`,
      );
    } catch (err) {
      pastPaperInstructionError = formatLogError(err);
      await appLogWarn(`[past-paper] instruction range save failed: ${pastPaperInstructionError}`);
    } finally {
      pastPaperInstructionSaving = false;
    }
  }

  function formatPageNumbersForDebug(pages: number[]): string {
    if (pages.length === 0) return "none";
    if (pages.length <= 10) return pages.join(", ");
    return `${pages.slice(0, 10).join(", ")}, ...`;
  }

  async function checkPastPaperInstructionContext() {
    const book = selectedBook;
    if (!book || book.document_mode !== "past_paper" || pastPaperInstructionCheckLoading) return;

    pastPaperInstructionCheckError = null;
    pastPaperInstructionCheckFeedback = null;
    pastPaperInstructionCheckPreview = null;
    pastPaperInstructionCheckLoading = true;
    try {
      const summary = await invoke<PastPaperInstructionContextDebug>(
        "inspect_past_paper_instruction_context",
        { sourceDocumentId: book.id },
      );

      const configuredRange = summary.instruction_page_start == null || summary.instruction_page_end == null
        ? "none"
        : `${summary.instruction_page_start}-${summary.instruction_page_end}`;
      const expectedCount = summary.expected_instruction_pages.length;
      const extractedCount = summary.extracted_instruction_pages.length;
      const missingCount = summary.missing_instruction_pages.length;

      pastPaperInstructionCheckFeedback =
        `Configured range: ${configuredRange}. Extracted instruction pages: ${extractedCount}/${expectedCount}. ` +
        `Instruction blocks: ${summary.instruction_block_count} (${summary.instruction_transcribed_block_count} transcribed). ` +
        `Chunking status: ${summary.chunking_status}.`;
      pastPaperInstructionCheckPreview = summary.preview_text.trim().length > 0
        ? summary.preview_text.trim()
        : null;

      if (missingCount > 0) {
        pastPaperInstructionCheckError =
          `Missing extracted instruction pages: ${formatPageNumbersForDebug(summary.missing_instruction_pages)}. ` +
          `Run chunking on those pages first.`;
      } else if (summary.instruction_page_start != null && summary.instruction_block_count === 0) {
        pastPaperInstructionCheckError =
          "No instruction text blocks were found in the configured range. Check the range and OCR extraction.";
      }

      await appLogInfo(
        `[past-paper] instruction context check doc=${book.id} range=${configuredRange} extracted_pages=${extractedCount}/${expectedCount} blocks=${summary.instruction_block_count} missing=${missingCount}`,
      );
    } catch (err) {
      pastPaperInstructionCheckError = formatLogError(err);
      await appLogWarn(`[past-paper] instruction context check failed: ${pastPaperInstructionCheckError}`);
    } finally {
      pastPaperInstructionCheckLoading = false;
    }
  }

  async function importPdf(documentMode: DocumentMode = "textbook") {
    error = null;
    pendingDeleteSourceDocumentId = null;
    importing = true;
    try {
      const doc = await invoke<SourceDocument>("import_pdf", { documentMode });
      void appLogInfo(
        `[import] imported doc=${doc.id} mode=${doc.document_mode} title="${doc.title}" path="${doc.file_path}"`,
      );
      void logChunkingStatus(doc.id, "after import");
      if (doc.document_mode === "past_paper") {
        void ensureChunkingForPage(doc.id, 1, "past-paper import");
      }
      await loadSourceDocuments();
    } catch (e: unknown) {
      if (e !== "cancelled") {
        error = String(e);
        void appLogError(`[import] failed: ${formatLogError(e)}`);
      }
    } finally {
      importing = false;
    }
  }

  async function deleteSourceDocument(book: SourceDocument) {
    if (
      importing
      || deletingSourceDocumentId !== null
      || pendingDeleteSourceDocumentId !== book.id
    ) return;

    error = null;
    deletingSourceDocumentId = book.id;
    try {
      await invoke("delete_source_document", { sourceDocumentId: book.id });
      await loadSourceDocuments();
      pendingDeleteSourceDocumentId = null;
      await appLogInfo(`[library] deleted doc=${book.id} title="${book.title}"`);
    } catch (err) {
      error = formatLogError(err);
      await appLogError(
        `[library] delete failed doc=${book.id} title="${book.title}": ${formatLogError(err)}`,
      );
    } finally {
      deletingSourceDocumentId = null;
    }
  }

  function requestSourceDocumentDelete(bookId: number) {
    if (importing || deletingSourceDocumentId !== null) return;
    pendingDeleteSourceDocumentId = bookId;
  }

  function cancelSourceDocumentDelete() {
    if (deletingSourceDocumentId !== null) return;
    pendingDeleteSourceDocumentId = null;
  }

  async function openBook(book: SourceDocument) {
    error = null;
    pendingDeleteSourceDocumentId = null;
    if (batchChunkProgress && batchChunkProgress.docId !== book.id) {
      batchChunkProgress = null;
    }
    if (viewerAiOpen || viewerAiContextPageId !== null || pendingChatAttachment) {
      await closeViewerAiPanel();
    }
    if (chunkView) closeChunkView();
    pendingChunkTap = null;
    showPaperOptions = false;
    void appLogInfo(`[viewer] opening doc=${book.id} title="${book.title}"`);
    selectedBook = book;
    syncPastPaperInstructionInputs(book);
    currentPage = loadSavedPage(book.id);
    currentPageId = null;
    currentPageSurfaceId = null;
    totalPages = 0;
    strokes = [];
    redoStack = [];
    clearPageStrokeTransformHistory();
    pageGraphs = [];
    selectedPageGraphId = null;
    pageGraphDrag = null;
    pageGraphUndoStack = [];
    pageGraphRedoStack = [];
    pendingGraphPlacement = null;
    graphModalOpen = false;
    selectedStrokes = new Set();
    selection = null;
    currentPdfBitmap = null;
    currentPdfBitmapKey = null;
    currentPdfPagePoints = { w: 0, h: 0 };
    currentChunks = [];
    chunkNavigationPageChunks = [];
    currentChunkingStatus = "pending";
    currentPageChunkingActive = false;
    reChunkingPage = false;
    clearChunkPageCaches();
    chunkSurfaceCache.clear();
    pageSurfaceCache.clear();
    pageSurfaceRequests.clear();

    await tick();
    setupCanvases();

    try {
      if (isNotebookDocument(book)) {
        totalPages = Math.max(1, book.page_count ?? 1);
        if (currentPage > totalPages) currentPage = totalPages;
        await loadNotebookCanvas(currentPage);
      } else {
        totalPages = await invoke<number>("get_page_count", { relativePath: book.file_path });
        void appLogInfo(`[viewer] doc=${book.id} page_count=${totalPages}`);
        void logChunkingStatus(book.id, "on open");
        if (currentPage > totalPages) currentPage = 1;
        await loadPdfPage(currentPage);
        resolvePageId(book.id, currentPage);
      }
    } catch (e) {
      error = String(e);
      void appLogError(`[viewer] failed to open doc=${book.id}: ${formatLogError(e)}`);
    }
  }

  async function goToPage(pageNum: number, options: { keepChunkView?: boolean } = {}) {
    if (rendering) return;
    if (isNotebookDocument()) {
      const target = Math.max(1, Math.min(totalPages + 1, pageNum));
      if (target === currentPage && currentPageSurfaceId !== null) return;
      if (viewerAiOpen || viewerAiContextPageId !== null || pendingChatAttachment) {
        await closeViewerAiPanel();
      }
      if (chunkView && !options.keepChunkView) closeChunkView();
      pendingChunkTap = null;
      currentPage = target;
      currentPageId = null;
      currentPageSurfaceId = null;
      strokes = [];
      redoStack = [];
      clearPageStrokeTransformHistory();
      pageGraphs = [];
      selectedPageGraphId = null;
      pageGraphDrag = null;
      pageGraphUndoStack = [];
      pageGraphRedoStack = [];
      pendingGraphPlacement = null;
      selectedStrokes = new Set();
      selection = null;
      currentChunks = [];
      currentPageChunkingActive = false;
      if (selectedBook) saveCurrentPage(selectedBook.id, currentPage);
      await loadNotebookCanvas(currentPage);
      return;
    }

    const clamped = Math.max(1, Math.min(totalPages, pageNum));
    if (clamped === currentPage && currentPdfBitmap) return;
    if (viewerAiOpen || viewerAiContextPageId !== null || pendingChatAttachment) {
      await closeViewerAiPanel();
    }
    if (chunkView && !options.keepChunkView) closeChunkView();
    pendingChunkTap = null;
    currentPage = clamped;
    currentPageId = null;
    currentPageSurfaceId = null;
    strokes = [];
    redoStack = [];
    clearPageStrokeTransformHistory();
    pageGraphs = [];
    selectedPageGraphId = null;
    pageGraphDrag = null;
    pageGraphUndoStack = [];
    pageGraphRedoStack = [];
    pendingGraphPlacement = null;
    selectedStrokes = new Set();
    selection = null;
    currentChunks = [];
    currentPageChunkingActive = false;
    if (selectedBook) saveCurrentPage(selectedBook.id, currentPage);
    await loadPdfPage(currentPage);
    if (selectedBook) await resolvePageId(selectedBook.id, currentPage);
  }

  async function prevPage() { await goToPage(currentPage - 1); }
  async function nextPage() { await goToPage(currentPage + 1); }

  function getClampedPageNumber(rawValue: string) {
    const trimmed = rawValue.trim();
    if (!/^\d+$/.test(trimmed)) return null;
    const parsed = Number(trimmed);
    if (!Number.isSafeInteger(parsed)) return null;
    return Math.max(1, totalPages > 0 ? Math.min(totalPages, parsed) : parsed);
  }

  async function commitPageInput() {
    const targetPage = getClampedPageNumber(pageInputValue);
    if (targetPage === null) {
      pageInputValue = String(currentPage);
      return;
    }

    pageInputValue = String(targetPage);
    await goToPage(targetPage);
  }

  function handlePageInputKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      void commitPageInput();
      return;
    }

    if (event.key === "Escape") {
      event.preventDefault();
      pageInputValue = String(currentPage);
      pageInputFocused = false;
      (event.currentTarget as HTMLInputElement).blur();
    }
  }

  async function closeViewer() {
    await closeViewerAiPanel();
    if (chunkView) closeChunkView();
    await flushPendingInkWrites();
    await flushChunkTextSaves();
    await flushGlossarySave();
    pendingChunkTap = null;
    showPaperOptions = false;
    batchChunkProgress = null;
    selectedBook = null;
    syncPastPaperInstructionInputs(null);
    currentPage = 1;
    currentPageId = null;
    currentPageSurfaceId = null;
    totalPages = 0;
    strokes = [];
    redoStack = [];
    pageGraphs = [];
    selectedPageGraphId = null;
    pageGraphDrag = null;
    pageGraphUndoStack = [];
    pageGraphRedoStack = [];
    pendingGraphPlacement = null;
    graphModalOpen = false;
    selectedStrokes = new Set();
    selection = null;
    currentPdfBitmap = null;
    currentPdfBitmapKey = null;
    currentPdfPagePoints = { w: 0, h: 0 };
    currentChunks = [];
    chunkNavigationPageChunks = [];
    currentChunkingStatus = "pending";
    currentPageChunkingActive = false;
    reChunkingPage = false;
    clearChunkPageCaches();
    chunkSurfaceCache.clear();
    pageSurfaceCache.clear();
    pageSurfaceRequests.clear();
    chunkView = null;
    onViewerBatchStateChange(null);
    onClose();
  }

  // â”€â”€ Undo / Redo â”€â”€

  async function undoStroke() {
    if (pageStrokeTransformUndo) {
      const entry = pageStrokeTransformUndo;
      pageStrokeTransformUndo = null;
      applyStrokeTransformHistory(entry, "before");
      pageStrokeTransformRedo = entry;
      selectedPageGraphId = null;
      selectedStrokes = new Set(entry.strokes.map((item) => item.stroke));
      selection = unionBBox(selectedStrokes);
      await persistPageStrokeUpdates(entry.strokes.map((item) => item.stroke));
      markDirty();
      return;
    }
    if (pageGraphUndoStack.length > 0) {
      const entry = pageGraphUndoStack[pageGraphUndoStack.length - 1];
      pageGraphUndoStack = pageGraphUndoStack.slice(0, -1);
      try {
        await applyPageGraphChange(entry.before, { recordHistory: false });
        pageGraphRedoStack = [...pageGraphRedoStack, entry];
        selectedPageGraphId = null;
      } catch (err) {
        void appLogWarn(`[graph] undo failed: ${formatLogError(err)}`);
      }
      return;
    }
    if (strokes.length === 0) return;
    const removed = getTrailingStrokeGroup(strokes);
    if (removed.length === 0) return;
    strokes = strokes.slice(0, strokes.length - removed.length);
    redoStack = [...redoStack, removed];
    clearPageStrokeTransformHistory();
    if (selectedStrokes.size > 0) {
      const keepSet = new Set(strokes);
      const nextSelected = new Set(
        [...selectedStrokes].filter((stroke) => keepSet.has(stroke)),
      );
      selectedStrokes = nextSelected;
      selection = nextSelected.size > 0 ? unionBBox(nextSelected) : null;
    }
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const stroke of removed) {
      if (stroke.id !== null) void trackInkWrite(deletePageStroke(stroke.id).catch(() => {}));
      else cancelPendingStrokeCreate(stroke);
    }
    markDirty();
  }

  async function redoStroke() {
    if (pageStrokeTransformRedo) {
      const entry = pageStrokeTransformRedo;
      pageStrokeTransformRedo = null;
      applyStrokeTransformHistory(entry, "after");
      pageStrokeTransformUndo = entry;
      selectedPageGraphId = null;
      selectedStrokes = new Set(entry.strokes.map((item) => item.stroke));
      selection = unionBBox(selectedStrokes);
      await persistPageStrokeUpdates(entry.strokes.map((item) => item.stroke));
      markDirty();
      return;
    }
    if (pageGraphRedoStack.length > 0) {
      const entry = pageGraphRedoStack[pageGraphRedoStack.length - 1];
      pageGraphRedoStack = pageGraphRedoStack.slice(0, -1);
      try {
        await applyPageGraphChange(entry.after, { recordHistory: false });
        pageGraphUndoStack = [...pageGraphUndoStack, entry];
        selectedPageGraphId = null;
      } catch (err) {
        void appLogWarn(`[graph] redo failed: ${formatLogError(err)}`);
      }
      return;
    }
    if (redoStack.length === 0) return;
    const entry = redoStack[redoStack.length - 1];
    redoStack = redoStack.slice(0, -1);
    strokes = [...strokes, ...entry];
    clearPageStrokeTransformHistory();
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const restored of entry) {
      if (currentPageId !== null) {
        void trackInkWrite(persistPageStrokeCreate(currentPageId, restored));
      }
    }
    markDirty();
  }

  // â”€â”€ Delete selected â”€â”€

  async function deleteSelected() {
    if (selectedPageGraphId != null) {
      await deleteSelectedPageGraph();
      return;
    }
    if (selectedStrokes.size === 0) return;
    const deleted = [...selectedStrokes];
    clearPageStrokeTransformHistory();
    if (currentPageId !== null) cacheEvict(currentPageId);
    for (const s of deleted) {
      if (s.id !== null) void trackInkWrite(deletePageStroke(s.id).catch(() => {}));
      else cancelPendingStrokeCreate(s);
    }
    redoStack = [...redoStack, deleted];
    strokes = strokes.filter(s => !selectedStrokes.has(s));
    selectedStrokes = new Set();
    selection = null;
    markDirty();
  }

  async function copySelectedPageInk(): Promise<boolean> {
    const nextClipboard = buildInkClipboardState("page", strokes, selectedStrokes);
    if (!nextClipboard) return false;
    inkClipboard = nextClipboard;
    try {
      await copyTextToClipboard(serialiseInkClipboard(nextClipboard));
      inkClipboard = { ...nextClipboard, systemSynced: true };
    } catch {
      // Keep an in-memory copy so paste still works inside Gloss when clipboard APIs are unavailable.
    }
    return true;
  }

  async function openPagePasteMenuAt(anchor: InkAnchorPoint): Promise<boolean> {
    const clipboard = await getInkClipboardForScope("page");
    if (!clipboard) return false;
    pagePasteMenuAnchor = anchor;
    return true;
  }

  async function pastePageInk(anchorPoint: InkAnchorPoint | null = null): Promise<boolean> {
    if (mode !== "select") return false;
    const clipboard = await getInkClipboardForScope("page");
    if (!clipboard) return false;
    const pasteBounds = isNotebookDocument()
      ? {
        minX: Number.NEGATIVE_INFINITY,
        maxX: Number.POSITIVE_INFINITY,
        minY: Number.NEGATIVE_INFINITY,
        maxY: Number.POSITIVE_INFINITY,
      }
      : { minX: 0, maxX: 1, minY: 0, maxY: 1 };
    const prepared = preparePastedStrokes(
      clipboard,
      { w: pageSize.w, h: pageSize.h },
      null,
      pasteBounds,
      anchorPoint,
    );
    if (!prepared) return false;

    const pastedSelection = new Set(prepared.strokes);
    strokes = [...strokes, ...prepared.strokes];
    redoStack = [];
    clearPageStrokeTransformHistory();
    selectedPageGraphId = null;
    pageGraphDrag = null;
    selectOrigin = null;
    selectRect = null;
    selectedStrokes = pastedSelection;
    selection = unionBBox(pastedSelection);
    pagePasteHold = null;
    pagePasteMenuAnchor = null;

    const pageId = currentPageId;
    if (pageId !== null) {
      cacheEvict(pageId);
      for (const stroke of prepared.strokes) {
        void trackInkWrite(persistPageStrokeCreate(pageId, stroke));
      }
    }

    inkClipboard = { ...clipboard, pasteCount: prepared.nextPasteCount };
    markDirty();
    return true;
  }

  async function pastePageInkFromMenu() {
    if (!pagePasteMenuAnchor) return;
    await pastePageInk(pagePasteMenuAnchor);
  }

  // â”€â”€ AI rasterisation + chat attachments â”€â”€
  let aiWorking = $state(false);
  let chunkAiWorking = $state(false);
  let viewerAiOpen = $state(false);
  let viewerAiContextPageId = $state<number | null>(null);
  interface PendingChatAttachment {
    imageBase64: string;
    imageDataUrl: string;
    pageNumber: number;
    createdAt: number;
    transcribedBody: string | null;
  }
  let pendingChatAttachment = $state<PendingChatAttachment | null>(null);
  const CHUNK_TRANSCRIPTION_TARGET_WIDTH = 1400;
  const CHUNK_TRANSCRIPTION_MAX_PAGE_WIDTH = 4096;
  const CHUNK_INK_CONTEXT_TARGET_EDGE = 1200;
  const CHUNK_INK_CONTEXT_MIN_EDGE = 320;
  const CHUNK_INK_CONTEXT_MAX_EDGE = 1600;

  interface ChunkFormattedBodyOutput {
    body_markdown: string;
  }

  function getActiveChatChunkId(): number | null {
    return chunkView?.chunk.id ?? null;
  }

  function getActiveViewerChatPageId(): number | null {
    if (!viewerAiOpen || chunkView) return null;
    return viewerAiContextPageId ?? currentPageId;
  }

  function isChatContextReady(): boolean {
    if (chunkView) return getActiveChatChunkId() != null;
    return getActiveViewerChatPageId() != null;
  }

  $effect(() => {
    if (!viewerAiOpen || chunkView) return;
    if (viewerAiContextPageId !== currentPageId) {
      viewerAiContextPageId = currentPageId;
    }
  });

  function clearPendingChatAttachment() {
    pendingChatAttachment = null;
  }

  async function closeViewerAiPanel() {
    viewerAiOpen = false;
    viewerAiContextPageId = null;
    clearPendingChatAttachment();
    await cancelChunkChatForReset();
  }

  // OffscreenCanvas.convertToBlob() hangs on some Android WebViews.
  // Use transferToImageBitmap + regular canvas + toDataURL instead.
  function offscreenToBase64(offscreen: OffscreenCanvas): string {
    const tmp = document.createElement("canvas");
    tmp.width = offscreen.width;
    tmp.height = offscreen.height;
    const ctx = tmp.getContext("2d")!;
    const bitmap = offscreen.transferToImageBitmap();
    ctx.drawImage(bitmap, 0, 0);
    bitmap.close();
    return tmp.toDataURL("image/png").split(",")[1];
  }

  async function rasteriseInkSelection(
    rect: { x: number; y: number; width: number; height: number },
    sourceStrokes: Stroke[],
    sourceGraphs: SurfaceGraphObject[],
    baseWidth: number,
    baseHeight: number,
  ): Promise<string> {
    const selectionWidth = Math.max(0.001, rect.width);
    const selectionHeight = Math.max(0.001, rect.height);
    const sourceWidth = Math.max(1, Math.round(selectionWidth * baseWidth));
    const sourceHeight = Math.max(1, Math.round(selectionHeight * baseHeight));
    const sourceLongest = Math.max(sourceWidth, sourceHeight);
    const targetLongest = Math.min(
      CHUNK_TRANSCRIPTION_TARGET_WIDTH,
      Math.max(480, sourceLongest),
    );
    const scale = targetLongest / sourceLongest;
    const outWidth = Math.max(1, Math.round(sourceWidth * scale));
    const outHeight = Math.max(1, Math.round(sourceHeight * scale));

    const offscreen = new OffscreenCanvas(outWidth, outHeight);
    const ctx = offscreen.getContext("2d");
    if (!ctx) throw new Error("Failed to create ink selection canvas");
    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, outWidth, outHeight);

    const cropMinX = rect.x;
    const cropMinY = rect.y;
    const cropMaxX = cropMinX + selectionWidth;
    const cropMaxY = cropMinY + selectionHeight;
    const pxPerNormX = outWidth / selectionWidth;
    const pxPerNormY = outHeight / selectionHeight;
    const lineWidthScale = pxPerNormX / Math.max(baseWidth, 1);

    for (const stroke of sourceStrokes) {
      if (stroke.points.length < 2) continue;
      if (
        stroke.bbox.maxX < cropMinX || stroke.bbox.minX > cropMaxX
        || stroke.bbox.maxY < cropMinY || stroke.bbox.minY > cropMaxY
      ) {
        continue;
      }
      const lineWidth = Math.max(1, stroke.thickness * lineWidthScale);
      drawChunkInkContextStroke(
        ctx,
        stroke,
        cropMinX,
        cropMinY,
        pxPerNormX,
        pxPerNormY,
        lineWidth,
      );
    }

    for (const graph of sourceGraphs) {
      const x2 = graph.bboxX + graph.bboxW;
      const y2 = graph.bboxY + graph.bboxH;
      if (
        x2 < cropMinX || graph.bboxX > cropMaxX
        || y2 < cropMinY || graph.bboxY > cropMaxY
      ) {
        continue;
      }
      drawGraphCard(
        ctx as unknown as CanvasRenderingContext2D,
        graphObjectToSpec(graph),
        (graph.bboxX - cropMinX) * pxPerNormX,
        (graph.bboxY - cropMinY) * pxPerNormY,
        graph.bboxW * pxPerNormX,
        graph.bboxH * pxPerNormY,
      );
    }

    return offscreenToBase64(offscreen);
  }

  async function rasteriseNotebookSelection(): Promise<string> {
    if (!selection || !isNotebookDocument()) throw new Error("Nothing selected");
    return rasteriseInkSelection(selection, strokes, pageGraphs, pageSize.w, pageSize.h);
  }

  async function rasteriseSelection(): Promise<string> {
    if (!selection) throw new Error("Nothing selected");
    if (isNotebookDocument()) return rasteriseNotebookSelection();
    if (!currentPdfBitmap) throw new Error("No PDF bitmap available");
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
      // Scale to fill sw x sh
      const renderS = sw / worldW;
      const inkCanvas = new OffscreenCanvas(Math.round(sw), Math.round(sh));
      const inkCtx = inkCanvas.getContext("2d")!;
      inkCtx.setTransform(renderS, 0, 0, renderS, -wTL.x * renderS, -wTL.y * renderS);
      inkCtx.lineCap = "round";
      inkCtx.lineJoin = "round";
      for (const stroke of strokes) {
        if (stroke.points.length < 2) continue;
        if (stroke.bbox.maxX < selection.x || stroke.bbox.minX > selection.x + selection.width ||
            stroke.bbox.maxY < selection.y || stroke.bbox.minY > selection.y + selection.height) continue;
        drawStrokePoints(inkCtx as unknown as CanvasRenderingContext2D, stroke.points, stroke.colour, stroke.thickness, 0);
      }
      for (const graph of pageGraphs) {
        const x2 = graph.bboxX + graph.bboxW;
        const y2 = graph.bboxY + graph.bboxH;
        if (
          x2 < selection.x || graph.bboxX > selection.x + selection.width
          || y2 < selection.y || graph.bboxY > selection.y + selection.height
        ) {
          continue;
        }
        drawGraphCard(
          inkCtx as unknown as CanvasRenderingContext2D,
          graphObjectToSpec(graph),
          pageOrigin.x + graph.bboxX * pageSize.w,
          pageOrigin.y + graph.bboxY * pageSize.h,
          graph.bboxW * pageSize.w,
          graph.bboxH * pageSize.h,
        );
      }
      ctx.drawImage(inkCanvas, 0, 0);
    }

    return offscreenToBase64(offscreen);
  }

  async function onAiClick() {
    if (aiWorking) return;
    viewerAiContextPageId = currentPageId;
    viewerAiOpen = true;
    chunkChatError = null;
    if (!selection) return;

    aiWorking = true;
    try {
      const b64 = await rasteriseSelection();
      pendingChatAttachment = {
        imageBase64: b64,
        imageDataUrl: `data:image/png;base64,${b64}`,
        pageNumber: currentPage,
        createdAt: Date.now(),
        transcribedBody: null,
      };
    } catch (err) {
      chunkChatError = "Failed to rasterise the selected area.";
      await appLogWarn(`[viewer-ai] selection rasterisation failed: ${formatLogError(err)}`);
    } finally {
      aiWorking = false;
    }
  }

  async function rasteriseChunkSelection(): Promise<string> {
    if (!chunkSelection || !chunkView) throw new Error("Nothing selected");
    const { w: baseWidth, h: baseHeight } = getChunkPageWorldSize();
    return rasteriseInkSelection(chunkSelection, chunkView.strokes, chunkView.graphs, baseWidth, baseHeight);
  }

  async function onChunkAiClick() {
    if (!chunkSelection || !chunkView || chunkAiWorking) return;
    chunkAiWorking = true;
    try {
      const b64 = await rasteriseChunkSelection();
      pendingChatAttachment = {
        imageBase64: b64,
        imageDataUrl: `data:image/png;base64,${b64}`,
        pageNumber: chunkView.pageNumber,
        createdAt: Date.now(),
        transcribedBody: null,
      };
      viewerAiOpen = false;
      chunkTab = 'ai';
      showChunkPenOptions = false;
      showChunkShapeOptions = false;
      chunkChatError = null;
    } catch (err) {
      chunkChatError = "Failed to rasterise the selected area.";
      await appLogWarn(`[chunk-ai] selection rasterisation failed: ${formatLogError(err)}`);
    } finally {
      chunkAiWorking = false;
    }
  }

  // â”€â”€ Chunks â”€â”€
  interface ChunkInfo {
    id: number;
    chunk_type: string;
    bbox_x: number;
    bbox_y: number;
    bbox_w: number;
    bbox_h: number;
    status: string;
    title: string | null;
    subject: string | null;
    proves_chunk_id: number | null;
    ocr_text: string | null;
    formatted_body_md: string | null;
    glossary_md: string | null;
    glossary_format: GlossaryFormat;
    question_label: string | null;
    available_marks: number | null;
    achieved_marks: number | null;
  }

  interface ChunkForTranscription extends ChunkInfo {
    source_document_id: number;
    page_number: number;
  }

  interface SurfaceStrokeOutput {
    id: number;
    colour: string;
    thickness: number;
    points: { x: number; y: number }[];
    min_x: number;
    min_y: number;
    max_x: number;
    max_y: number;
  }

  interface SurfaceGraphObjectOutput {
    id: number;
    surface_id: number;
    mode: "equation" | "points";
    equation: string | null;
    points_json: string | null;
    x_min: number;
    x_max: number;
    y_min: number;
    y_max: number;
    bbox_x: number;
    bbox_y: number;
    bbox_w: number;
    bbox_h: number;
    line_colour: string;
    line_width: number;
  }

  interface SurfaceGraphObject {
    id: number | null;
    mode: "equation" | "points";
    equation: string | null;
    pointsJson: string | null;
    xMin: number;
    xMax: number;
    yMin: number;
    yMax: number;
    bboxX: number;
    bboxY: number;
    bboxW: number;
    bboxH: number;
    lineColour: string;
    lineWidth: number;
  }

  interface SurfaceGraphCacheEntry {
    surfaceId: number;
    graphs: SurfaceGraphObject[];
  }

  interface GraphHistoryEntry {
    surfaceId: number;
    before: SurfaceGraphObject[];
    after: SurfaceGraphObject[];
  }

  interface GraphPointerDrag {
    graphId: number;
    mode: "move" | "resize";
    startPoint: { x: number; y: number };
    startGraph: SurfaceGraphObject;
    beforeSnapshot: SurfaceGraphObject[];
  }

  type GraphModalSource = "page" | "chunk" | "glossary";
  type GraphPlacementScope = "page" | "chunk";
  type GlossaryFormat = "markdown" | "typst";

  interface PendingGraphPlacement {
    scope: GraphPlacementScope;
    spec: GraphSpec;
  }

  interface ChunkSurfaceCache {
    surfaceId: number;
    strokes: Stroke[];
    graphs: SurfaceGraphObject[];
  }

  interface ChunkViewState {
    chunk: ChunkInfo;
    pageNumber: number;
    surfaceId: number;
    strokes: Stroke[];
    graphs: SurfaceGraphObject[];
  }

  const CHUNK_COLOURS = SHARED_CHUNK_COLOURS;

  let currentChunks = $state<ChunkInfo[]>([]);
  const chunkSurfaceCache = new Map<number, ChunkSurfaceCache>();
  const chunkSurfaceRequests = new Map<number, Promise<ChunkSurfaceCache>>();
  const chunkBodyRequests = new Map<number, Promise<boolean>>();
  let chunkMode = $state<'draw' | 'shape' | 'erase' | 'select'>('draw');
  let chunkTab = $state<'ink' | 'glossary' | 'ai'>('ink');
  let showChunkPenOptions = $state(false);
  let showChunkShapeOptions = $state(false);
  let chunkSelectOrigin = $state<{ x: number; y: number } | null>(null);
  let chunkSelectRect = $state<Rect | null>(null);
  let chunkSelectedStrokes = $state<Set<Stroke>>(new Set());
  let chunkStrokeDrag = $state<InkStrokeDragState | null>(null);
  let chunkStrokeTransformUndo = $state<StrokeTransformHistoryEntry | null>(null);
  let chunkStrokeTransformRedo = $state<StrokeTransformHistoryEntry | null>(null);
  let chunkSelectedGraphId = $state<number | null>(null);
  let chunkGraphDrag = $state<GraphPointerDrag | null>(null);
  let chunkGraphUndoStack = $state<GraphHistoryEntry[]>([]);
  let chunkGraphRedoStack = $state<GraphHistoryEntry[]>([]);
  let chunkSelection = $state<{ x: number; y: number; width: number; height: number } | null>(null);
  let chunkPasteMenuAnchor = $state<InkAnchorPoint | null>(null);
  let chunkPasteHold = $state<InkPasteHoldState | null>(null);
  const CHUNK_LEFT_PANEL_DEFAULT_WIDTH = 360;
  const CHUNK_LEFT_PANEL_MIN_WIDTH = 260;
  const CHUNK_LEFT_PANEL_MAX_WIDTH = 760;
  let chunkLeftPanelWidth = $state(CHUNK_LEFT_PANEL_DEFAULT_WIDTH);
  let chunkSheet = $state<HTMLDivElement>(null!);
  let chunkResizingPointerId: number | null = null;
  let questionAvailableMarksDraft = $state("");
  let questionAvailableMarksEditing = $state(false);
  let questionAvailableMarksSaving = $state(false);
  let questionAvailableMarksError = $state<string | null>(null);
  let questionAvailableMarksInputEl = $state<HTMLInputElement>(null!);
  let questionAchievedMarksDraft = $state("");
  let questionAchievedMarksSaving = $state(false);
  let questionAchievedMarksError = $state<string | null>(null);
  interface QuestionSourceSlice {
    page_id: number;
    page_number: number;
    bbox_x: number;
    bbox_y: number;
    bbox_w: number;
    bbox_h: number;
    slice_order: number;
  }
  let questionSourceSlices = $state<QuestionSourceSlice[]>([]);
  let questionSourceLoading = $state(false);
  let questionSourceError = $state<string | null>(null);

  function clearChunkSelectionState() {
    chunkSelectOrigin = null;
    chunkSelectRect = null;
    chunkSelectedStrokes = new Set();
    chunkSelectedGraphId = null;
    chunkGraphDrag = null;
    chunkSelection = null;
    chunkStrokeDrag = null;
    chunkPasteHold = null;
    chunkPasteMenuAnchor = null;
  }

  function activateChunkDrawTool() {
    const wasDrawMode = chunkMode === 'draw';
    chunkMode = 'draw';
    clearChunkSelectionState();
    chunkShapeDraft = null;
    if (pendingGraphPlacement?.scope === "chunk") pendingGraphPlacement = null;
    showChunkPenOptions = wasDrawMode ? !showChunkPenOptions : true;
    showChunkShapeOptions = false;
    showPaperOptions = false;
    redrawChunkCanvases();
  }

  function activateChunkShapeTool() {
    const wasShapeMode = chunkMode === "shape";
    chunkMode = "shape";
    clearChunkSelectionState();
    chunkIsDrawing = false;
    chunkCurrentStroke = [];
    chunkShapeDraft = null;
    if (pendingGraphPlacement?.scope === "chunk") pendingGraphPlacement = null;
    showChunkPenOptions = false;
    showChunkShapeOptions = wasShapeMode ? !showChunkShapeOptions : true;
    showPaperOptions = false;
    redrawChunkCanvases();
  }

  function activateChunkGraphTool() {
    clearChunkSelectionState();
    chunkIsDrawing = false;
    chunkCurrentStroke = [];
    chunkShapeDraft = null;
    showPaperOptions = false;
    openGraphComposer("chunk");
    redrawChunkCanvases();
  }

  function activateChunkEraseTool() {
    chunkMode = 'erase';
    clearChunkSelectionState();
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
    showPaperOptions = false;
    chunkShapeDraft = null;
    if (pendingGraphPlacement?.scope === "chunk") pendingGraphPlacement = null;
    redrawChunkCanvases();
  }

  function activateChunkSelectTool() {
    chunkMode = 'select';
    clearChunkSelectionState();
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
    showPaperOptions = false;
    chunkShapeDraft = null;
    if (pendingGraphPlacement?.scope === "chunk") pendingGraphPlacement = null;
    redrawChunkCanvases();
  }

  function getChunkLeftPanelBounds() {
    const sheetWidth = chunkSheet?.clientWidth ?? window.innerWidth;
    const maxBySheet = Math.floor(sheetWidth * 0.6);
    const max = Math.max(CHUNK_LEFT_PANEL_MIN_WIDTH, Math.min(CHUNK_LEFT_PANEL_MAX_WIDTH, maxBySheet));
    return { min: CHUNK_LEFT_PANEL_MIN_WIDTH, max };
  }

  function clampChunkLeftPanelWidth(width: number) {
    const { min, max } = getChunkLeftPanelBounds();
    return Math.min(max, Math.max(min, Math.round(width)));
  }

  function applyChunkLeftPanelWidthFromClientX(clientX: number) {
    if (!chunkSheet) return;
    const rect = chunkSheet.getBoundingClientRect();
    chunkLeftPanelWidth = clampChunkLeftPanelWidth(clientX - rect.left);
  }

  function stopChunkPanelResize() {
    if (chunkResizingPointerId === null) return;
    chunkResizingPointerId = null;
    window.removeEventListener("pointermove", onChunkPanelResizeMove);
    window.removeEventListener("pointerup", onChunkPanelResizeEnd);
    window.removeEventListener("pointercancel", onChunkPanelResizeEnd);
  }

  function onChunkPanelResizeMove(e: PointerEvent) {
    if (e.pointerId !== chunkResizingPointerId) return;
    applyChunkLeftPanelWidthFromClientX(e.clientX);
  }

  function onChunkPanelResizeEnd(e: PointerEvent) {
    if (e.pointerId !== chunkResizingPointerId) return;
    stopChunkPanelResize();
  }

  function startChunkPanelResize(e: PointerEvent) {
    if (window.matchMedia("(max-width: 720px)").matches) return;
    const target = e.currentTarget as HTMLElement;
    chunkResizingPointerId = e.pointerId;
    target.setPointerCapture(e.pointerId);
    window.addEventListener("pointermove", onChunkPanelResizeMove);
    window.addEventListener("pointerup", onChunkPanelResizeEnd);
    window.addEventListener("pointercancel", onChunkPanelResizeEnd);
    applyChunkLeftPanelWidthFromClientX(e.clientX);
    e.preventDefault();
  }

  function onChunkPanelResizeKeydown(e: KeyboardEvent) {
    if (window.matchMedia("(max-width: 720px)").matches) return;
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      chunkLeftPanelWidth = clampChunkLeftPanelWidth(chunkLeftPanelWidth - 20);
      return;
    }
    if (e.key === "ArrowRight") {
      e.preventDefault();
      chunkLeftPanelWidth = clampChunkLeftPanelWidth(chunkLeftPanelWidth + 20);
      return;
    }
    if (e.key === "Home") {
      e.preventDefault();
      chunkLeftPanelWidth = getChunkLeftPanelBounds().min;
      return;
    }
    if (e.key === "End") {
      e.preventDefault();
      chunkLeftPanelWidth = getChunkLeftPanelBounds().max;
    }
  }

  interface TypstPreviewDocument {
    pages: string[];
  }

  const TYPST_PREVIEW_DEBOUNCE_MS = 400;
  const typstPreviewDocumentCache = new Map<string, TypstPreviewDocument>();
  const typstPreviewPendingCache = new Map<string, Promise<TypstPreviewDocument>>();

  function normaliseGlossaryFormat(value: string | null | undefined): GlossaryFormat {
    return value === "typst" ? "typst" : "markdown";
  }

  function glossaryFormatLabel(format: GlossaryFormat): string {
    return format === "typst" ? "Typst" : "Markdown";
  }

  function glossaryPlaceholder(format: GlossaryFormat, isQuestion: boolean): string {
    if (format === "typst") {
      return isQuestion
        ? "Write your answer in Typst. This mode treats the whole note as Typst source."
        : "Write your own explanation in Typst. This mode treats the whole note as Typst source.";
    }
    return isQuestion
      ? "Write your answer. Markdown works, and LaTeX via $...$ inline or $$...$$ block."
      : "Write your own explanation. Markdown works, and LaTeX via $...$ inline or $$...$$ block.";
  }

  async function loadTypstPreviewDocument(source: string): Promise<TypstPreviewDocument> {
    const cached = typstPreviewDocumentCache.get(source);
    if (cached) return cached;

    const pending = typstPreviewPendingCache.get(source);
    if (pending) return await pending;

    const request = invoke<TypstPreviewDocument>("render_typst_note_preview", { source })
      .then((document) => {
        typstPreviewDocumentCache.set(source, document);
        return document;
      })
      .finally(() => {
        if (typstPreviewPendingCache.get(source) === request) {
          typstPreviewPendingCache.delete(source);
        }
      });
    typstPreviewPendingCache.set(source, request);
    return await request;
  }

  interface TypstPdfImage {
    pageWidthPt: number;
    pageHeightPt: number;
    pixelWidth: number;
    pixelHeight: number;
    jpegBytes: Uint8Array;
  }

  const PDF_HEADER_BYTES = new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34, 0x0a, 0x25, 0xff, 0xff, 0xff, 0xff, 0x0a]);
  const pdfTextEncoder = new TextEncoder();

  function encodePdfText(value: string): Uint8Array {
    return pdfTextEncoder.encode(value);
  }

  function concatUint8Arrays(parts: Uint8Array[]): Uint8Array {
    const totalLength = parts.reduce((sum, part) => sum + part.length, 0);
    const merged = new Uint8Array(totalLength);
    let offset = 0;
    for (const part of parts) {
      merged.set(part, offset);
      offset += part.length;
    }
    return merged;
  }

  function formatPdfNumber(value: number): string {
    return Number(value.toFixed(3)).toString();
  }

  function parseSvgLength(value: string | null): number | null {
    if (!value) return null;
    const match = value.trim().match(/^([0-9]*\.?[0-9]+)([a-z%]*)$/i);
    if (!match) return null;

    const numeric = Number(match[1]);
    if (!Number.isFinite(numeric) || numeric <= 0) return null;

    const unit = match[2].toLowerCase();
    switch (unit) {
      case "":
      case "pt":
      case "px":
        return numeric;
      case "in":
        return numeric * 72;
      case "cm":
        return numeric * 72 / 2.54;
      case "mm":
        return numeric * 72 / 25.4;
      default:
        return numeric;
    }
  }

  function readTypstPreviewPageSize(pageSvg: string): { width: number; height: number } {
    const document = new DOMParser().parseFromString(pageSvg, "image/svg+xml");
    if (document.querySelector("parsererror")) {
      throw new Error("Couldn't parse the rendered Typst preview.");
    }

    const svg = document.documentElement;
    if (!svg || svg.tagName.toLowerCase() !== "svg") {
      throw new Error("The rendered Typst preview was not valid SVG.");
    }

    const viewBox = svg.getAttribute("viewBox");
    if (viewBox) {
      const parts = viewBox
        .trim()
        .split(/[\s,]+/)
        .map((part) => Number(part));
      if (
        parts.length === 4
        && Number.isFinite(parts[2])
        && Number.isFinite(parts[3])
        && parts[2] > 0
        && parts[3] > 0
      ) {
        return { width: parts[2], height: parts[3] };
      }
    }

    const width = parseSvgLength(svg.getAttribute("width"));
    const height = parseSvgLength(svg.getAttribute("height"));
    if (width && height) {
      return { width, height };
    }

    throw new Error("Couldn't determine the size of the rendered Typst preview.");
  }

  async function loadImageElement(url: string): Promise<HTMLImageElement> {
    return await new Promise((resolve, reject) => {
      const image = new Image();
      image.onload = () => resolve(image);
      image.onerror = () => reject(new Error("Couldn't prepare the Typst preview for PDF export."));
      image.src = url;
    });
  }

  async function canvasToJpegBlob(canvas: HTMLCanvasElement): Promise<Blob> {
    return await new Promise((resolve, reject) => {
      canvas.toBlob(
        (blob) => {
          if (blob) {
            resolve(blob);
          } else {
            reject(new Error("Couldn't encode the Typst preview as an image."));
          }
        },
        "image/jpeg",
        0.98,
      );
    });
  }

  async function blobToBase64(blob: Blob): Promise<string> {
    return await new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(new Error("Couldn't prepare the PDF export."));
      reader.onload = () => {
        if (typeof reader.result !== "string") {
          reject(new Error("Couldn't prepare the PDF export."));
          return;
        }
        const parts = reader.result.split(",", 2);
        if (parts.length !== 2 || !parts[1]) {
          reject(new Error("Couldn't prepare the PDF export."));
          return;
        }
        resolve(parts[1]);
      };
      reader.readAsDataURL(blob);
    });
  }

  async function rasterizeTypstPreviewPageForPdf(pageSvg: string): Promise<TypstPdfImage> {
    const { width: pageWidthPt, height: pageHeightPt } = readTypstPreviewPageSize(pageSvg);
    const maxDimensionPx = 4000;
    const scale = Math.min(2.25, maxDimensionPx / Math.max(pageWidthPt, pageHeightPt));
    const pixelWidth = Math.max(1, Math.round(pageWidthPt * scale));
    const pixelHeight = Math.max(1, Math.round(pageHeightPt * scale));

    const svgBlob = new Blob([pageSvg], { type: "image/svg+xml;charset=utf-8" });
    const svgUrl = URL.createObjectURL(svgBlob);
    try {
      const image = await loadImageElement(svgUrl);
      const canvas = document.createElement("canvas");
      canvas.width = pixelWidth;
      canvas.height = pixelHeight;

      const ctx = canvas.getContext("2d", { alpha: false });
      if (!ctx) {
        throw new Error("Couldn't create an image canvas for the PDF export.");
      }

      ctx.fillStyle = "#ffffff";
      ctx.fillRect(0, 0, pixelWidth, pixelHeight);
      ctx.drawImage(image, 0, 0, pixelWidth, pixelHeight);

      const jpegBlob = await canvasToJpegBlob(canvas);
      const jpegBytes = new Uint8Array(await jpegBlob.arrayBuffer());
      return { pageWidthPt, pageHeightPt, pixelWidth, pixelHeight, jpegBytes };
    } finally {
      URL.revokeObjectURL(svgUrl);
    }
  }

  function buildPdfFromJpegs(images: TypstPdfImage[]): Uint8Array {
    if (images.length === 0) {
      throw new Error("Couldn't build a PDF because the Typst preview had no pages.");
    }

    const parts: Uint8Array[] = [PDF_HEADER_BYTES];
    const objectOffsets: number[] = [0];
    let totalLength = PDF_HEADER_BYTES.length;
    const firstPageObject = 3;
    const totalObjects = 2 + images.length * 3;

    function push(part: string | Uint8Array) {
      const bytes = typeof part === "string" ? encodePdfText(part) : part;
      parts.push(bytes);
      totalLength += bytes.length;
    }

    function startObject(objectNumber: number) {
      objectOffsets[objectNumber] = totalLength;
      push(`${objectNumber} 0 obj\n`);
    }

    startObject(1);
    push("<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    startObject(2);
    push(
      `<< /Type /Pages /Count ${images.length} /Kids [${images
        .map((_, index) => `${firstPageObject + index * 3} 0 R`)
        .join(" ")}] >>\nendobj\n`,
    );

    for (const [index, image] of images.entries()) {
      const pageObject = firstPageObject + index * 3;
      const contentObject = pageObject + 1;
      const imageObject = pageObject + 2;
      const contentStream =
        `q\n${formatPdfNumber(image.pageWidthPt)} 0 0 ${formatPdfNumber(image.pageHeightPt)} 0 0 cm\n/Im0 Do\nQ\n`;
      const contentBytes = encodePdfText(contentStream);

      startObject(pageObject);
      push(
        `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${formatPdfNumber(image.pageWidthPt)} ${formatPdfNumber(image.pageHeightPt)}] `
        + `/Resources << /ProcSet [/PDF /ImageC] /XObject << /Im0 ${imageObject} 0 R >> >> /Contents ${contentObject} 0 R >>\nendobj\n`,
      );

      startObject(contentObject);
      push(`<< /Length ${contentBytes.length} >>\nstream\n`);
      push(contentBytes);
      push("endstream\nendobj\n");

      startObject(imageObject);
      push(
        `<< /Type /XObject /Subtype /Image /Width ${image.pixelWidth} /Height ${image.pixelHeight} `
        + `/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length ${image.jpegBytes.length} >>\nstream\n`,
      );
      push(image.jpegBytes);
      push("\nendstream\nendobj\n");
    }

    const xrefOffset = totalLength;
    push(`xref\n0 ${totalObjects + 1}\n0000000000 65535 f \n${Array.from({ length: totalObjects }, (_, index) => index + 1)
      .map((objectNumber) => `${objectOffsets[objectNumber].toString().padStart(10, "0")} 00000 n \n`)
      .join("")}`);
    push(`trailer\n<< /Size ${totalObjects + 1} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF`);

    return concatUint8Arrays(parts);
  }

  function sanitizeFileNameSegment(value: string | null | undefined): string {
    return (value ?? "")
      .normalize("NFKC")
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, " ")
      .replace(/\s+/g, " ")
      .trim();
  }

  function buildTypstPdfFileName(chunk: ChunkInfo): string {
    const parts: string[] = [];
    const bookTitle = sanitizeFileNameSegment(selectedBook?.title);
    if (bookTitle) parts.push(bookTitle);

    if (chunk.chunk_type === "question") {
      const questionLabel = sanitizeFileNameSegment(chunk.question_label ? `Q${chunk.question_label}` : "");
      if (questionLabel) parts.push(questionLabel);
      parts.push("Answer");
    } else {
      const chunkTitle = sanitizeFileNameSegment(
        chunkViewTitleDisplay?.heading
        ?? chunk.title
        ?? chunk.subject
        ?? CHUNK_COLOURS[chunk.chunk_type]?.label
        ?? "Glossary",
      );
      if (chunkTitle) parts.push(chunkTitle);
      parts.push("Glossary");
    }

    const baseName = parts.join(" - ").slice(0, 120).trim();
    return `${baseName || "Gloss note"}.pdf`;
  }

  function isChunkSheetMobileLayout(): boolean {
    return typeof window !== "undefined" && window.matchMedia("(max-width: 720px)").matches;
  }

  let glossaryDraft = $state("");
  let glossaryFormat = $state<GlossaryFormat>("markdown");
  let glossaryMode = $state<'edit' | 'preview'>('edit');
  let glossarySaving = $state(false);
  let glossarySaveError = $state<string | null>(null);
  let glossaryFormatNotice = $state<string | null>(null);
  let glossaryPdfExporting = $state(false);
  let glossaryPdfExportError = $state<string | null>(null);
  let glossaryPdfExportFeedback = $state<string | null>(null);
  let glossarySaveTimer: ReturnType<typeof setTimeout> | null = null;
  let glossaryPendingChunkId: number | null = null;
  let glossaryTextareaEl = $state<HTMLTextAreaElement | null>(null);
  let glossaryMentionRefs = $state<ResolvedReference[]>([]);
  let glossaryMentionResolveRequestId = 0;
  let glossaryMarkdownHtml = $derived(renderChunkBodyHtml(glossaryDraft, glossaryMentionRefs));
  let glossaryTypstPreviewDocument = $state<TypstPreviewDocument | null>(null);
  let glossaryTypstPreviewError = $state<string | null>(null);
  let glossaryTypstPreviewLoading = $state(false);
  let glossaryTypstPreviewTimer: ReturnType<typeof setTimeout> | null = null;
  let glossaryTypstPreviewRequestId = 0;
  let glossaryTypstPreviewActiveRequestId = 0;
  let glossaryRewriteTypstPreviewDocument = $state<TypstPreviewDocument | null>(null);
  let glossaryRewriteTypstPreviewError = $state<string | null>(null);
  let glossaryRewriteTypstPreviewLoading = $state(false);
  let glossaryRewriteTypstPreviewRequestId = 0;
  let glossaryRewriteTypstPreviewActiveRequestId = 0;

  function clearGlossaryTypstPreviewTimer() {
    if (!glossaryTypstPreviewTimer) return;
    clearTimeout(glossaryTypstPreviewTimer);
    glossaryTypstPreviewTimer = null;
  }

  function resetGlossaryTypstPreviewState(clearPreview = true) {
    clearGlossaryTypstPreviewTimer();
    glossaryTypstPreviewActiveRequestId = ++glossaryTypstPreviewRequestId;
    glossaryTypstPreviewLoading = false;
    glossaryTypstPreviewError = null;
    if (clearPreview) {
      glossaryTypstPreviewDocument = null;
    }
  }

  function resetGlossaryRewriteTypstPreviewState(clearPreview = true) {
    glossaryRewriteTypstPreviewActiveRequestId = ++glossaryRewriteTypstPreviewRequestId;
    glossaryRewriteTypstPreviewLoading = false;
    glossaryRewriteTypstPreviewError = null;
    if (clearPreview) {
      glossaryRewriteTypstPreviewDocument = null;
    }
  }

  function shouldRenderGlossaryTypstPreview() {
    return !!chunkView
      && chunkTab === "glossary"
      && glossaryFormat === "typst"
      && (!isChunkSheetMobileLayout() || glossaryMode === "preview");
  }

  async function refreshGlossaryTypstPreview() {
    const view = chunkView;
    if (!view || glossaryFormat !== "typst" || chunkTab !== "glossary") return;

    const source = glossaryDraft.trim();
    if (!source) {
      glossaryTypstPreviewLoading = false;
      glossaryTypstPreviewError = null;
      glossaryTypstPreviewDocument = null;
      return;
    }

    const requestId = ++glossaryTypstPreviewRequestId;
    glossaryTypstPreviewActiveRequestId = requestId;
    glossaryTypstPreviewLoading = true;
    try {
      const document = await loadTypstPreviewDocument(source);
      if (
        glossaryTypstPreviewActiveRequestId !== requestId
        || chunkView?.chunk.id !== view.chunk.id
        || glossaryFormat !== "typst"
        || chunkTab !== "glossary"
      ) {
        return;
      }
      glossaryTypstPreviewDocument = document;
      glossaryTypstPreviewError = null;
    } catch (err) {
      if (glossaryTypstPreviewActiveRequestId !== requestId || chunkView?.chunk.id !== view.chunk.id) return;
      glossaryTypstPreviewError = formatLogError(err);
      await appLogWarn(`[typst] preview failed chunkId=${view.chunk.id}: ${glossaryTypstPreviewError}`);
    } finally {
      if (glossaryTypstPreviewActiveRequestId === requestId) {
        glossaryTypstPreviewLoading = false;
      }
    }
  }

  function scheduleGlossaryTypstPreview(delay = TYPST_PREVIEW_DEBOUNCE_MS) {
    clearGlossaryTypstPreviewTimer();
    if (!shouldRenderGlossaryTypstPreview()) return;
    if (!glossaryDraft.trim()) {
      glossaryTypstPreviewLoading = false;
      glossaryTypstPreviewError = null;
      glossaryTypstPreviewDocument = null;
      return;
    }
    glossaryTypstPreviewTimer = setTimeout(() => {
      glossaryTypstPreviewTimer = null;
      void refreshGlossaryTypstPreview();
    }, delay);
  }

  async function refreshGlossaryRewriteTypstPreview(source: string) {
    const noteSource = source.trim();
    if (!noteSource) {
      glossaryRewriteTypstPreviewLoading = false;
      glossaryRewriteTypstPreviewError = null;
      glossaryRewriteTypstPreviewDocument = null;
      return;
    }

    const requestId = ++glossaryRewriteTypstPreviewRequestId;
    glossaryRewriteTypstPreviewActiveRequestId = requestId;
    glossaryRewriteTypstPreviewLoading = true;
    try {
      const document = await loadTypstPreviewDocument(noteSource);
      if (glossaryRewriteTypstPreviewActiveRequestId !== requestId) return;
      glossaryRewriteTypstPreviewDocument = document;
      glossaryRewriteTypstPreviewError = null;
    } catch (err) {
      if (glossaryRewriteTypstPreviewActiveRequestId !== requestId) return;
      glossaryRewriteTypstPreviewError = formatLogError(err);
    } finally {
      if (glossaryRewriteTypstPreviewActiveRequestId === requestId) {
        glossaryRewriteTypstPreviewLoading = false;
      }
    }
  }

  function setGlossaryMode(mode: 'edit' | 'preview') {
    if (mode === "preview") {
      void flushGlossarySave();
      if (glossaryFormat === "typst") {
        scheduleGlossaryTypstPreview(0);
      }
    }
    glossaryMode = mode;
  }

  function clearGlossaryPdfExportNotices() {
    glossaryPdfExportError = null;
    glossaryPdfExportFeedback = null;
  }

  function setGlossaryFormat(nextFormat: GlossaryFormat) {
    if (nextFormat === glossaryFormat) return;
    const previous = glossaryFormat;
    glossaryFormat = nextFormat;
    glossaryFormatNotice = glossaryDraft.trim()
      ? `Switched from ${glossaryFormatLabel(previous)} to ${glossaryFormatLabel(nextFormat)}. Existing content was kept as-is and not converted.`
      : null;
    glossarySaveError = null;
    clearGlossaryPdfExportNotices();
    if (graphModalOpen && graphModalSource === "glossary" && nextFormat === "typst") {
      graphModalOpen = false;
    }
    scheduleGlossarySave();
    if (nextFormat === "typst") {
      scheduleGlossaryTypstPreview(0);
    } else {
      resetGlossaryTypstPreviewState();
      resetGlossaryRewriteTypstPreviewState();
    }
  }

  async function flushGlossarySave() {
    if (glossarySaveTimer) {
      clearTimeout(glossarySaveTimer);
      glossarySaveTimer = null;
    }
    if (glossaryPendingChunkId == null) return;
    const chunkId = glossaryPendingChunkId;
    const value = glossaryDraft;
    const format = glossaryFormat;
    glossaryPendingChunkId = null;
    glossarySaving = true;
    try {
      await invoke("save_chunk_glossary", { chunkId, glossaryMd: value, glossaryFormat: format });
      glossarySaveError = null;
      const stored: string | null = value.trim() ? value : null;
      currentChunks = currentChunks.map((c) =>
        c.id === chunkId ? { ...c, glossary_md: stored, glossary_format: format } : c,
      );
      if (chunkView?.chunk.id === chunkId) {
        chunkView = {
          ...chunkView,
          chunk: { ...chunkView.chunk, glossary_md: stored, glossary_format: format },
        };
      }
    } catch (err) {
      glossarySaveError = formatLogError(err);
      await appLogWarn(`[glossary] save failed chunkId=${chunkId}: ${glossarySaveError}`);
    } finally {
      glossarySaving = false;
    }
  }

  function scheduleGlossarySave() {
    if (!chunkView) return;
    glossaryPendingChunkId = chunkView.chunk.id;
    if (glossarySaveTimer) clearTimeout(glossarySaveTimer);
    glossarySaveTimer = setTimeout(() => {
      glossarySaveTimer = null;
      void flushGlossarySave();
    }, 500);
  }

  function onGlossaryInput(event: Event) {
    const target = event.target as HTMLTextAreaElement;
    glossaryDraft = target.value;
    glossaryFormatNotice = null;
    clearGlossaryPdfExportNotices();
    scheduleGlossarySave();
    if (glossaryFormat === "typst") {
      scheduleGlossaryTypstPreview();
    }
  }

  async function exportGlossaryTypstPdf() {
    const view = chunkView;
    const source = glossaryDraft.trim();
    if (!view || glossaryFormat !== "typst" || !source || glossaryPdfExporting) return;

    glossaryPdfExporting = true;
    glossaryPdfExportError = null;
    glossaryPdfExportFeedback = null;

    try {
      await flushGlossarySave();
      if (chunkView?.chunk.id !== view.chunk.id) return;

      const document = await loadTypstPreviewDocument(source);
      if (chunkView?.chunk.id !== view.chunk.id) return;

      const pdfImages: TypstPdfImage[] = [];
      for (const pageSvg of document.pages) {
        pdfImages.push(await rasterizeTypstPreviewPageForPdf(pageSvg));
      }
      const pdfBytes = buildPdfFromJpegs(pdfImages);
      const pdfBase64 = await blobToBase64(new Blob([pdfBytes], { type: "application/pdf" }));
      if (chunkView?.chunk.id !== view.chunk.id) return;

      const destination = await invoke<string>("export_typst_note_pdf", {
        suggestedFileName: buildTypstPdfFileName(view.chunk),
        pdfBase64,
      });
      if (chunkView?.chunk.id !== view.chunk.id) return;

      glossaryPdfExportFeedback = `PDF saved to ${destination}.`;
      await appLogInfo(`[typst] exported pdf chunkId=${view.chunk.id} to ${destination}`);
    } catch (err) {
      if (err !== "cancelled" && chunkView?.chunk.id === view.chunk.id) {
        glossaryPdfExportError = formatLogError(err);
        await appLogWarn(`[typst] pdf export failed chunkId=${view.chunk.id}: ${glossaryPdfExportError}`);
      }
    } finally {
      glossaryPdfExporting = false;
    }
  }

  function insertGraphFenceIntoGlossary(spec: GraphSpec) {
    if (glossaryFormat === "typst") return;
    const fence = graphSpecToFence(spec);
    const textarea = glossaryTextareaEl;
    if (
      textarea
      && glossaryMode === "edit"
      && document.activeElement === textarea
    ) {
      const start = textarea.selectionStart ?? glossaryDraft.length;
      const end = textarea.selectionEnd ?? start;
      const prefix = glossaryDraft.slice(0, start);
      const suffix = glossaryDraft.slice(end);
      const separatorBefore = prefix.trim().length > 0 && !prefix.endsWith("\n\n") ? "\n\n" : "";
      const separatorAfter = suffix.trim().length > 0 && !suffix.startsWith("\n") ? "\n\n" : "";
      const inserted = `${separatorBefore}${fence}${separatorAfter}`;
      glossaryDraft = `${prefix}${inserted}${suffix}`;
      const caret = prefix.length + inserted.length;
      void tick().then(() => {
        textarea.focus();
        textarea.setSelectionRange(caret, caret);
      });
    } else {
      const sep = glossaryDraft.trim().length > 0 ? "\n\n" : "";
      glossaryDraft = `${glossaryDraft}${sep}${fence}`;
    }
    scheduleGlossarySave();
  }

  function openGlossaryGraphComposer() {
    if (glossaryFormat === "typst") return;
    openGraphComposer("glossary");
  }

  let chunkView = $state<ChunkViewState | null>(null);

  function snapshotChunkViewToCache(view: ChunkViewState) {
    const cached = chunkSurfaceCache.get(view.chunk.id);
    if (!cached) return;
    cached.strokes = cloneStrokes(view.strokes);
    cached.graphs = cloneGraphObjects(view.graphs);
  }

  const chunkGlossaryTabLabel = $derived(
    chunkView?.chunk.chunk_type === "question" ? "Answer" : "Glossary",
  );
  const chunkIsQuestion = $derived(chunkView?.chunk.chunk_type === "question");
  const chunkAchievedMarksDisplay = $derived.by(() => {
    if (!chunkView || chunkView.chunk.chunk_type !== "question") return null;
    return chunkView.chunk.achieved_marks == null
      ? "—"
      : formatQuestionMarksValue(chunkView.chunk.achieved_marks);
  });
  const chunkMarksDisplay = $derived.by(() => {
    if (!chunkView || chunkView.chunk.chunk_type !== "question") return null;
    const available = chunkView.chunk.available_marks;
    const achieved = chunkAchievedMarksDisplay ?? "—";
    const availableText = available == null ? "?" : `${available}`;
    return `${achieved}/${availableText}`;
  });
  let chunkTitleDraft = $state("");
  let chunkBodyDraft = $state("");
  let chunkTitleSaving = $state(false);
  let chunkBodySaving = $state(false);
  let chunkTitleSaveError = $state<string | null>(null);
  let chunkBodySaveError = $state<string | null>(null);
  let chunkTitleSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let chunkBodySaveTimer: ReturnType<typeof setTimeout> | null = null;
  let chunkTitlePendingChunkId: number | null = null;
  let chunkBodyPendingChunkId: number | null = null;
  let chunkTextSaving = $derived(chunkTitleSaving || chunkBodySaving);
  let chunkTextSaveError = $derived(chunkBodySaveError ?? chunkTitleSaveError);
  let chunkTitleEditing = $state(false);
  let chunkBodyEditing = $state(false);
  let chunkTitleInputEl = $state<HTMLInputElement>(null!);
  let chunkBodyTextareaEl = $state<HTMLTextAreaElement>(null!);
  let chunkViewTitleDisplay = $derived(
    chunkView
      ? getChunkTitleDisplay({
        chunk_type: chunkView.chunk.chunk_type,
        title: chunkTitleDraft,
      })
      : null,
  );
  let chunkViewRefs = $state<ResolvedReference[]>([]);
  let chunkViewBodyHtml = $derived(
    chunkView
      ? renderChunkBodyHtml(chunkBodyDraft, chunkViewRefs, {
        allowHeadings: false,
        allowStrong: false,
        suppressLeadingText: chunkViewTitleDisplay?.suppressionCandidates ?? [],
      })
      : "",
  );
  let chunkPeek = $state<{
    chunkId: number;
    anchorRect: DOMRect;
  } | null>(null);
  let linkedChunkLabel = $state<string | null>(null);
  let linkedChunkEmptyText = $state("No linked chunk yet.");
  let linkedChunkTargetId = $state<number | null>(null);
  let linkedChunkTargetLoading = $state(false);
  let linkedChunkTargetError = $state<string | null>(null);
  let linkedChunkExpanded = $state(false);
  let linkedChunkPreview = $state<ChunkPreview | null>(null);
  let linkedChunkPreviewLoading = $state(false);
  let linkedChunkPreviewError = $state<string | null>(null);
  let linkedChunkPreviewBodyHtml = $derived(
    linkedChunkPreview?.body_preview
      ? renderChunkBodyPreviewHtml(linkedChunkPreview.body_preview, {
        allowHeadings: false,
        allowStrong: false,
      })
      : "",
  );
  let chunkNavigationBusy = $state(false);
  let chunkNavigationPageChunks = $state<ChunkInfo[]>([]);
  let chunkNavigationNavigableChunks = $derived(
    chunkNavigationPageChunks.filter(isChunkNavigableChunk),
  );
  let chunkViewIndex = $derived(
    chunkView ? chunkNavigationNavigableChunks.findIndex((entry) => entry.id === chunkView?.chunk.id) : -1,
  );
  let canOpenPreviousChunk = $derived(
    !!chunkView && (
      hasNavigableChunkInDirection(chunkNavigationPageChunks, chunkView.chunk.id, -1)
      || chunkView.pageNumber > 1
    ),
  );
  let canOpenNextChunk = $derived(
    !!chunkView && (
      hasNavigableChunkInDirection(chunkNavigationPageChunks, chunkView.chunk.id, 1)
      || chunkView.pageNumber < totalPages
    ),
  );

  interface ChunkNavigationTarget {
    chunk: ChunkInfo;
    pageNumber: number;
    pageChunks: ChunkInfo[];
  }

  function isChunkNavigableChunk(chunk: ChunkInfo): boolean {
    return chunk.chunk_type !== "noise";
  }

  function findNavigableChunkFromIndex(
    chunks: ChunkInfo[],
    startIndex: number,
    direction: -1 | 1,
  ): ChunkInfo | null {
    for (
      let i = startIndex + direction;
      i >= 0 && i < chunks.length;
      i += direction
    ) {
      const candidate = chunks[i];
      if (isChunkNavigableChunk(candidate)) return candidate;
    }
    return null;
  }

  function hasNavigableChunkInDirection(
    chunks: ChunkInfo[],
    currentChunkId: number,
    direction: -1 | 1,
  ): boolean {
    const currentIndex = chunks.findIndex((entry) => entry.id === currentChunkId);
    const startIndex = currentIndex >= 0
      ? currentIndex
      : direction > 0 ? -1 : chunks.length;
    return findNavigableChunkFromIndex(chunks, startIndex, direction) !== null;
  }

  async function ensureChunksForNavigation(bookId: number, pageNum: number): Promise<ChunkInfo[]> {
    if (pageNum < 1 || (totalPages > 0 && pageNum > totalPages)) return [];
    if (selectedBook?.id === bookId && currentPage === pageNum) {
      return currentChunks;
    }
    const key = pageKey(bookId, pageNum);
    const cached = chunkPageCache.get(key);
    if (cached) return cached;
    return await getChunksForDocumentPage(bookId, pageNum);
  }

  async function findChunkNeighbour(direction: -1 | 1): Promise<ChunkNavigationTarget | null> {
    const view = chunkView;
    const book = selectedBook;
    if (!view || !book) return null;

    let pageNumber = view.pageNumber;
    let pageChunks = await ensureChunksForNavigation(book.id, pageNumber);
    let index = pageChunks.findIndex((entry) => entry.id === view.chunk.id);
    if (index < 0) {
      index = chunkNavigationPageChunks.findIndex((entry) => entry.id === view.chunk.id);
    }

    const startIndex = index >= 0
      ? index
      : direction > 0 ? -1 : pageChunks.length;
    const samePageTarget = findNavigableChunkFromIndex(pageChunks, startIndex, direction);
    if (samePageTarget) {
      return {
        chunk: samePageTarget,
        pageNumber,
        pageChunks,
      };
    }

    pageNumber += direction;
    while (pageNumber >= 1 && pageNumber <= totalPages) {
      pageChunks = await ensureChunksForNavigation(book.id, pageNumber);
      const boundaryIndex = direction > 0 ? -1 : pageChunks.length;
      const target = findNavigableChunkFromIndex(pageChunks, boundaryIndex, direction);
      if (target) {
        return {
          chunk: target,
          pageNumber,
          pageChunks,
        };
      }
      pageNumber += direction;
    }
    return null;
  }

  async function navigateChunk(direction: -1 | 1) {
    const book = selectedBook;
    if (!chunkView || !book || chunkNavigationBusy) return;
    chunkNavigationBusy = true;
    try {
      const target = await findChunkNeighbour(direction);
      if (!target) return;

      if (target.pageNumber !== currentPage) {
        await goToPage(target.pageNumber, { keepChunkView: true });
        if (currentPage !== target.pageNumber) return;
      }

      const activePageChunks = target.pageNumber === currentPage
        ? currentChunks
        : await ensureChunksForNavigation(book.id, target.pageNumber);
      const nextChunk = activePageChunks.find((entry) => entry.id === target.chunk.id) ?? target.chunk;
      await openChunkView(nextChunk, {
        pageNumber: target.pageNumber,
        pageChunks: activePageChunks,
      });
      prefetchPage(book.id, target.pageNumber + 1);
      prefetchPage(book.id, target.pageNumber - 1);
    } finally {
      chunkNavigationBusy = false;
    }
  }

  type ChunkChatMessageState = "complete" | "streaming" | "stopped" | "error";

  interface ChunkChatMessage {
    id: string;
    role: "user" | "assistant";
    content: string;
    state: ChunkChatMessageState;
    historyContent?: string;
    historyImageBase64List?: string[];
    imageDataUrls?: string[];
    html?: string;
  }

  interface ChunkChatHistoryItem {
    role: "user" | "assistant";
    content: string;
    image_base64_list?: string[];
  }

  interface ChunkAiStreamEventPayload {
    request_id: string;
    context_kind: "chunk" | "page";
    chunk_id?: number | null;
    page_id?: number | null;
    phase: "delta" | "completed" | "error" | "cancelled";
    delta?: string | null;
    error?: string | null;
  }

  let chunkChatMessages = $state<ChunkChatMessage[]>([]);
  let chunkChatDraft = $state("");
  let chunkChatLoadingContext = $state(false);
  let chunkChatStreaming = $state(false);
  let chunkChatError = $state<string | null>(null);
  let chunkChatActiveRequestId = $state<string | null>(null);
  let chatAttachmentTranscribing = $state(false);
  let chunkInkContextTranscribing = $state(false);
  let chunkChatTranscript = $state<HTMLDivElement>(null!);
  const CHUNK_CODE_COPY_RESET_MS = 1400;
  let includeChunkAiVisuals = $state(false);
  let chunkHasInkContext = $derived(
    !!chunkView && (
      getLikelyChunkInkStrokes(chunkView.strokes).length > 0
      || getRenderableChunkGraphs(chunkView.graphs).length > 0
    ),
  );

  interface ChunkInkContextCache {
    chunkId: number;
    fingerprint: string;
    transcription: string | null;
  }

  interface NotebookInkContextCache {
    pageId: number;
    fingerprint: string;
    transcription: string | null;
  }

  interface ChunkVisualPageImage {
    pageIndex: number;
    imageBase64: string;
    imageDataUrl: string;
  }

  let chunkInkContextCache = $state<ChunkInkContextCache | null>(null);
  let notebookInkContextCache = $state<NotebookInkContextCache | null>(null);

  let chunkAiRewriteTab = $state<'body' | 'glossary'>('body');
  let chunkRewritePrompt = $state("");
  let chunkRewriteLoading = $state(false);
  let chunkRewriteError = $state<string | null>(null);
  let chunkRewriteApplying = $state(false);
  let chunkRewriteSuggestion = $state<ChunkRewriteSuggestionOutput | null>(null);
  let chunkRewriteBodyPreviewHtml = $derived(
    chunkRewriteSuggestion?.body_markdown
      ? renderChunkBodyHtml(chunkRewriteSuggestion.body_markdown, undefined, {
        copyCodeBlocks: false,
        allowHeadings: false,
        allowStrong: false,
      })
      : "",
  );
  let glossaryRewritePrompt = $state("");
  let glossaryRewriteLoading = $state(false);
  let glossaryRewriteError = $state<string | null>(null);
  let glossaryRewriteApplying = $state(false);
  let glossaryRewriteSuggestion = $state<ChunkGlossaryRewriteSuggestionOutput | null>(null);
  let glossaryRewriteMentionRefs = $state<ResolvedReference[]>([]);
  let glossaryRewriteMentionResolveRequestId = 0;
  let glossaryRewriteMarkdownPreviewHtml = $derived(
    glossaryRewriteSuggestion?.glossary_markdown
      ? renderChunkBodyHtml(glossaryRewriteSuggestion.glossary_markdown, glossaryRewriteMentionRefs, {
        copyCodeBlocks: false,
        allowHeadings: false,
        allowStrong: false,
      })
      : "",
  );
  let questionMarkingLoading = $state(false);
  let questionMarkingApplying = $state(false);
  let questionMarkingError = $state<string | null>(null);
  let questionMarkingSuggestion = $state<QuestionMarkSuggestion | null>(null);
  let questionMarkingSuggestionIncludeVisuals = $state(false);
  let questionMarkAttempts = $state<QuestionMarkAttemptView[]>([]);
  let questionMarkingSuggestionFeedbackHtml = $derived(
    questionMarkingSuggestion
      ? renderChunkBodyHtml(questionMarkingSuggestion.feedback_md, undefined, {
        copyCodeBlocks: false,
        allowHeadings: false,
      })
      : "",
  );

  interface ChunkRewriteSuggestionOutput {
    title: string | null;
    body_markdown: string | null;
  }

  interface ChunkGlossaryRewriteSuggestionOutput {
    glossary_markdown: string;
  }

  interface QuestionMarkAttemptView {
    id: number;
    source: string;
    achieved_marks: number | null;
    available_marks_snapshot: number | null;
    feedback_md: string | null;
    include_visuals: boolean;
    provider: string | null;
    model: string | null;
    created_at: string;
  }

  interface QuestionMarkSuggestion {
    achieved_marks: number | null;
    feedback_md: string;
  }

  function formatQuestionMarksValue(value: number | null): string {
    return value == null ? "?" : `${Number(value.toFixed(2))}`;
  }

  function formatQuestionAttemptScore(attempt: QuestionMarkAttemptView): string {
    const achieved = formatQuestionMarksValue(attempt.achieved_marks);
    const available = attempt.available_marks_snapshot == null
      ? "?"
      : `${attempt.available_marks_snapshot}`;
    return `${achieved}/${available}`;
  }

  function formatQuestionAttemptTimestamp(createdAt: string): string {
    const normalized = createdAt.includes("T")
      ? createdAt
      : `${createdAt.replace(" ", "T")}Z`;
    const parsed = new Date(normalized);
    if (Number.isNaN(parsed.getTime())) return createdAt;
    return parsed.toLocaleString();
  }

  function renderQuestionAttemptFeedback(feedbackMd: string | null): string {
    if (!feedbackMd || !feedbackMd.trim()) return "";
    return renderChunkBodyHtml(feedbackMd, undefined, {
      copyCodeBlocks: false,
      allowHeadings: false,
    });
  }

  interface BackendChunkReference {
    matched_text: string;
    span_start: number;
    span_end: number;
    ref_kind: string;
    target_id: number | null;
    target_title: string | null;
    target_type: string | null;
  }

  interface BackendResolvedChunkMention {
    matched_text: string;
    alias: string;
    span_start: number;
    span_end: number;
    target_id: number;
    target_type: string;
    target_title: string | null;
    target_subject: string | null;
    page_number: number | null;
    body_preview: string | null;
  }

  interface ChunkPreview {
    id: number;
    chunk_type: string;
    title: string | null;
    subject: string | null;
    status: string;
    body_preview: string | null;
    glossary_preview?: string | null;
    glossary_format: GlossaryFormat;
    has_formatted_body: boolean;
    has_self_explanation: boolean;
    question_label?: string | null;
    available_marks?: number | null;
    achieved_marks?: number | null;
  }

  async function loadChunkReferences(chunkId: number) {
    try {
      const rows = await invoke<BackendChunkReference[]>("get_chunk_references", { chunkId });
      if (chunkView?.chunk.id !== chunkId) return;
      chunkViewRefs = rows
        .filter((r): r is BackendChunkReference & { target_id: number } => r.target_id != null)
        .map((r) => ({
          matched_text: r.matched_text,
          span_start: r.span_start,
          span_end: r.span_end,
          target_id: r.target_id,
        }));
    } catch (err) {
      await appLogWarn(`[chunk] load references failed chunkId=${chunkId}: ${formatLogError(err)}`);
    }
  }

  function mapResolvedMentionsToRefs(rows: BackendResolvedChunkMention[]): ResolvedReference[] {
    return rows.map((row) => ({
      matched_text: row.matched_text,
      span_start: row.span_start,
      span_end: row.span_end,
      target_id: row.target_id,
    }));
  }

  async function resolveMentionsForCurrentDocument(
    source: string,
    limit = 32,
  ): Promise<BackendResolvedChunkMention[]> {
    const book = selectedBook;
    const trimmed = source.trim();
    if (!book || !trimmed) return [];
    return await invoke<BackendResolvedChunkMention[]>("resolve_chunk_mentions", {
      sourceDocumentId: book.id,
      source,
      limit,
    });
  }

  async function refreshGlossaryMarkdownMentions() {
    const view = chunkView;
    const requestId = ++glossaryMentionResolveRequestId;
    const shouldResolve = !!view
      && chunkTab === "glossary"
      && glossaryFormat === "markdown"
      && glossaryMode === "preview"
      && !!glossaryDraft.trim();
    if (!shouldResolve) {
      glossaryMentionRefs = [];
      return;
    }

    try {
      const rows = await resolveMentionsForCurrentDocument(glossaryDraft);
      if (
        glossaryMentionResolveRequestId !== requestId
        || chunkView?.chunk.id !== view.chunk.id
        || chunkTab !== "glossary"
        || glossaryFormat !== "markdown"
        || glossaryMode !== "preview"
      ) {
        return;
      }
      glossaryMentionRefs = mapResolvedMentionsToRefs(rows);
    } catch (err) {
      if (glossaryMentionResolveRequestId === requestId) {
        glossaryMentionRefs = [];
      }
      await appLogWarn(`[glossary] mention resolution failed: ${formatLogError(err)}`);
    }
  }

  async function refreshGlossaryRewriteMarkdownMentions(source: string) {
    const requestId = ++glossaryRewriteMentionResolveRequestId;
    const trimmed = source.trim();
    if (glossaryFormat !== "markdown" || !trimmed) {
      glossaryRewriteMentionRefs = [];
      return;
    }

    try {
      const rows = await resolveMentionsForCurrentDocument(source);
      if (
        glossaryRewriteMentionResolveRequestId !== requestId
        || glossaryFormat !== "markdown"
        || glossaryRewriteSuggestion?.glossary_markdown !== source
      ) {
        return;
      }
      glossaryRewriteMentionRefs = mapResolvedMentionsToRefs(rows);
    } catch (err) {
      if (glossaryRewriteMentionResolveRequestId === requestId) {
        glossaryRewriteMentionRefs = [];
      }
      await appLogWarn(`[glossary] rewrite mention resolution failed: ${formatLogError(err)}`);
    }
  }

  function buildMentionContextBlock(rows: BackendResolvedChunkMention[]): string | null {
    if (rows.length === 0) return null;

    const unique = new Map<number, BackendResolvedChunkMention>();
    for (const row of rows) {
      if (!unique.has(row.target_id)) {
        unique.set(row.target_id, row);
      }
    }
    if (unique.size === 0) return null;

    const parts = ["Referenced chunks:"];
    for (const row of unique.values()) {
      const headerParts = [`- ${row.matched_text} -> chunk ${row.target_id} (${row.target_type})`];
      if (row.page_number != null) {
        headerParts.push(`page ${row.page_number}`);
      }
      parts.push(headerParts.join(", "));
      if (row.target_title?.trim()) {
        parts.push(`  title: ${row.target_title.trim()}`);
      }
      if (row.target_subject?.trim()) {
        parts.push(`  subject: ${row.target_subject.trim()}`);
      }
      if (row.body_preview?.trim()) {
        parts.push("  body preview:");
        for (const line of row.body_preview.trim().split("\n")) {
          parts.push(`    ${line}`);
        }
      }
    }

    return parts.join("\n");
  }

  async function flushChunkTitleSave() {
    if (chunkTitleSaveTimer) {
      clearTimeout(chunkTitleSaveTimer);
      chunkTitleSaveTimer = null;
    }
    if (chunkTitlePendingChunkId == null) return;
    const chunkId = chunkTitlePendingChunkId;
    const value = chunkTitleDraft;
    chunkTitlePendingChunkId = null;
    chunkTitleSaving = true;
    try {
      await invoke("save_chunk_title", { chunkId, title: value });
      chunkTitleSaveError = null;
      const stored: string | null = value.trim() ? value : null;
      currentChunks = currentChunks.map((chunk) =>
        chunk.id === chunkId ? { ...chunk, title: stored } : chunk,
      );
      if (chunkView?.chunk.id === chunkId) {
        chunkView = {
          ...chunkView,
          chunk: { ...chunkView.chunk, title: stored },
        };
      }
    } catch (err) {
      chunkTitleSaveError = formatLogError(err);
      await appLogWarn(`[chunk] title save failed chunkId=${chunkId}: ${chunkTitleSaveError}`);
    } finally {
      chunkTitleSaving = false;
    }
  }

  async function flushChunkBodySave() {
    if (chunkBodySaveTimer) {
      clearTimeout(chunkBodySaveTimer);
      chunkBodySaveTimer = null;
    }
    if (chunkBodyPendingChunkId == null) return;
    const chunkId = chunkBodyPendingChunkId;
    const value = chunkBodyDraft;
    chunkBodyPendingChunkId = null;
    chunkBodySaving = true;
    try {
      await invoke("save_chunk_body_markdown", { chunkId, bodyMarkdown: value });
      chunkBodySaveError = null;
      const stored: string | null = value.trim() ? value : null;
      currentChunks = currentChunks.map((chunk) =>
        chunk.id === chunkId ? { ...chunk, formatted_body_md: stored } : chunk,
      );
      if (chunkView?.chunk.id === chunkId) {
        const updatedChunk = { ...chunkView.chunk, formatted_body_md: stored };
        chunkView = {
          ...chunkView,
          chunk: updatedChunk,
        };
        // Empty manual body means clear formatted text and show OCR fallback.
        if (stored === null) {
          chunkBodyDraft = getChunkDisplayBody(updatedChunk);
        }
      }
      // Body edits can change reference spans; refresh to keep in-memory links in sync.
      await loadChunkReferences(chunkId);
    } catch (err) {
      chunkBodySaveError = formatLogError(err);
      await appLogWarn(`[chunk] body save failed chunkId=${chunkId}: ${chunkBodySaveError}`);
    } finally {
      chunkBodySaving = false;
    }
  }

  async function flushChunkTextSaves() {
    await flushChunkTitleSave();
    await flushChunkBodySave();
  }

  function onChunkTitleInput(event: Event) {
    const target = event.target as HTMLInputElement;
    chunkTitleDraft = target.value;
    chunkTitleSaveError = null;
  }

  function onChunkBodyInput(event: Event) {
    const target = event.target as HTMLTextAreaElement;
    chunkBodyDraft = target.value;
    chunkBodySaveError = null;
  }

  function beginChunkTitleEdit() {
    if (!chunkView) return;
    chunkTitleEditing = true;
    void tick().then(() => {
      chunkTitleInputEl?.focus();
      chunkTitleInputEl?.select();
    });
  }

  async function finishChunkTitleEdit() {
    if (chunkView) {
      chunkTitlePendingChunkId = chunkView.chunk.id;
      await flushChunkTitleSave();
    }
    chunkTitleEditing = false;
  }

  function cancelChunkTitleEdit() {
    if (!chunkView) return;
    chunkTitleDraft = chunkView.chunk.title ?? "";
    chunkTitleEditing = false;
    chunkTitleSaveError = null;
  }

  function onChunkTitleEditorKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      (event.currentTarget as HTMLInputElement).blur();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      cancelChunkTitleEdit();
    }
  }

  function beginChunkBodyEdit() {
    if (!chunkView) return;
    chunkBodyEditing = true;
    void tick().then(() => {
      chunkBodyTextareaEl?.focus();
      const length = chunkBodyTextareaEl?.value.length ?? 0;
      chunkBodyTextareaEl?.setSelectionRange(length, length);
    });
  }

  async function finishChunkBodyEdit() {
    if (chunkView) {
      chunkBodyPendingChunkId = chunkView.chunk.id;
      await flushChunkBodySave();
    }
    chunkBodyEditing = false;
  }

  function cancelChunkBodyEdit() {
    if (!chunkView) return;
    chunkBodyDraft = getChunkDisplayBody(chunkView.chunk);
    chunkBodyEditing = false;
    chunkBodySaveError = null;
  }

  function onChunkBodyEditorKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      cancelChunkBodyEdit();
    }
  }

  function onChunkBodyDisplayClick(event: MouseEvent) {
    const target = event.target;
    if (!(target instanceof HTMLElement)) {
      beginChunkBodyEdit();
      return;
    }

    const anchor = target.closest("a.chunk-xref");
    if (anchor instanceof HTMLElement) {
      event.preventDefault();
      const raw = anchor.getAttribute("data-chunk-id");
      const targetId = raw ? Number(raw) : NaN;
      if (!Number.isInteger(targetId)) return;
      chunkPeek = {
        chunkId: targetId,
        anchorRect: anchor.getBoundingClientRect(),
      };
      return;
    }

    beginChunkBodyEdit();
  }

  function onChunkBodyDisplayKeydown(event: KeyboardEvent) {
    const target = event.target;
    if (target instanceof HTMLElement) {
      const anchor = target.closest("a.chunk-xref");
      if (anchor instanceof HTMLElement) {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          const raw = anchor.getAttribute("data-chunk-id");
          const targetId = raw ? Number(raw) : NaN;
          if (!Number.isInteger(targetId)) return;
          chunkPeek = {
            chunkId: targetId,
            anchorRect: anchor.getBoundingClientRect(),
          };
        }
        return;
      }
    }
    if (event.target !== event.currentTarget) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      beginChunkBodyEdit();
    }
  }

  async function openChunkById(chunkId: number) {
    const book = selectedBook;
    if (!book) return;

    const cached = currentChunks.find((c) => c.id === chunkId);
    if (cached) {
      await openChunkView(cached, {
        pageNumber: currentPage,
        pageChunks: currentChunks,
      });
      return;
    }

    try {
      const fetched = await invoke<ChunkForTranscription>("get_chunk_for_transcription", { chunkId });
      if (fetched.source_document_id !== book.id) {
        void appLogWarn(
          `[chunk] open request ignored chunkId=${chunkId} doc=${fetched.source_document_id} activeDoc=${book.id}`,
        );
        return;
      }

      const targetPage = fetched.page_number;
      if (targetPage !== currentPage) {
        await goToPage(targetPage, { keepChunkView: true });
        if (currentPage !== targetPage) return;
      }

      const pageChunks = targetPage === currentPage
        ? currentChunks
        : await ensureChunksForNavigation(book.id, targetPage);
      const targetChunk = pageChunks.find((entry) => entry.id === chunkId);
      if (!targetChunk) return;

      await openChunkView(targetChunk, {
        pageNumber: targetPage,
        pageChunks,
      });
    } catch (err) {
      await appLogWarn(`[chunk] open chunk failed chunkId=${chunkId}: ${formatLogError(err)}`);
    }
  }

  function onPeekOpen(chunkId: number) {
    chunkPeek = null;
    void openChunkById(chunkId);
  }

  function onPeekClose() {
    chunkPeek = null;
  }

  async function openLinkedChunk(chunkId: number) {
    await openChunkById(chunkId);
  }

  function openLinkedChunkFromPanel() {
    if (linkedChunkTargetId == null) return;
    void openLinkedChunk(linkedChunkTargetId);
  }

  $effect(() => {
    const view = chunkView;
    linkedChunkLabel = null;
    linkedChunkEmptyText = "No linked chunk yet.";
    linkedChunkTargetId = null;
    linkedChunkTargetLoading = false;
    linkedChunkTargetError = null;
    linkedChunkExpanded = false;
    linkedChunkPreview = null;
    linkedChunkPreviewLoading = false;
    linkedChunkPreviewError = null;

    if (!view) return;

    if (view.chunk.chunk_type === "proof") {
      linkedChunkLabel = "Proves";
      linkedChunkEmptyText = "No linked result yet.";
      linkedChunkTargetId = view.chunk.proves_chunk_id;
      return;
    }

    if (view.chunk.chunk_type !== "theorem") return;

    linkedChunkLabel = "Proof";
    linkedChunkEmptyText = "No linked proof yet.";
    const localProof = currentChunks.find(
      (entry) => entry.chunk_type === "proof" && entry.proves_chunk_id === view.chunk.id,
    );
    if (localProof) {
      linkedChunkTargetId = localProof.id;
      return;
    }

    let cancelled = false;
    linkedChunkTargetLoading = true;
    void (async () => {
      try {
        const proofChunkId = await invoke<number | null>("get_proof_chunk_for_target", {
          targetChunkId: view.chunk.id,
        });
        if (cancelled) return;
        linkedChunkTargetId = proofChunkId;
      } catch (err) {
        if (cancelled) return;
        linkedChunkTargetError = formatLogError(err);
      } finally {
        if (!cancelled) linkedChunkTargetLoading = false;
      }
    })();

    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const targetChunkId = linkedChunkTargetId;
    const expanded = linkedChunkExpanded;
    if (!expanded || targetChunkId == null) {
      linkedChunkPreviewLoading = false;
      return;
    }
    if (linkedChunkPreview?.id === targetChunkId) return;

    let cancelled = false;
    linkedChunkPreview = null;
    linkedChunkPreviewError = null;
    linkedChunkPreviewLoading = true;
    void (async () => {
      try {
        const preview = await invoke<ChunkPreview>("get_chunk_preview", { chunkId: targetChunkId });
        if (cancelled) return;
        linkedChunkPreview = preview;
      } catch (err) {
        if (cancelled) return;
        linkedChunkPreviewError = formatLogError(err);
      } finally {
        if (!cancelled) linkedChunkPreviewLoading = false;
      }
    })();

    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const id = chunkView?.chunk.id;
    if (id == null) {
      chunkViewRefs = [];
      return;
    }
    void loadChunkReferences(id);
  });

  $effect(() => {
    // Re-run when saved body changes (references may now hit different spans).
    const body = chunkView ? getChunkDisplayBody(chunkView.chunk) : "";
    if (chunkView && body) void loadChunkReferences(chunkView.chunk.id);
  });

  $effect(() => {
    const chunkId = chunkView?.chunk.id ?? null;
    const source = glossaryDraft;
    const tab = chunkTab;
    const format = glossaryFormat;
    const mode = glossaryMode;
    if (!chunkId || tab !== "glossary" || format !== "markdown" || mode !== "preview" || !source.trim()) {
      glossaryMentionResolveRequestId += 1;
      glossaryMentionRefs = [];
      return;
    }
    void refreshGlossaryMarkdownMentions();
  });

  $effect(() => {
    const source = glossaryRewriteSuggestion?.glossary_markdown ?? "";
    const format = glossaryFormat;
    if (format !== "markdown" || !source.trim()) {
      glossaryRewriteMentionResolveRequestId += 1;
      glossaryRewriteMentionRefs = [];
      return;
    }
    void refreshGlossaryRewriteMarkdownMentions(source);
  });

  function makeChunkChatRequestId() {
    return `chunk-chat-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
  }

  function resetChunkChatState() {
    chunkChatMessages = [];
    chunkChatDraft = "";
    chunkChatLoadingContext = false;
    chunkChatStreaming = false;
    chunkChatError = null;
    chunkChatActiveRequestId = null;
    chatAttachmentTranscribing = false;
    clearPendingChatAttachment();
  }

  function updateChunkChatMessage(
    messageId: string,
    updater: (message: ChunkChatMessage) => ChunkChatMessage,
  ) {
    chunkChatMessages = chunkChatMessages.map((message) =>
      message.id === messageId ? updater(message) : message,
    );
  }

  async function scrollChunkChatToBottom() {
    await tick();
    if (chunkChatTranscript) {
      chunkChatTranscript.scrollTop = chunkChatTranscript.scrollHeight;
    }
  }

  function queueChunkChatScroll() {
    void scrollChunkChatToBottom();
  }

  async function copyTextToClipboard(text: string) {
    if (navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText(text);
        return;
      } catch {
        // Fall back to a temporary textarea for WebViews without Clipboard API support.
      }
    }

    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.inset = "0 auto auto 0";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.focus();
    textarea.select();
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } finally {
      textarea.remove();
    }

    if (!copied) {
      throw new Error("Clipboard copy failed");
    }
  }

  async function readTextFromClipboard(): Promise<string | null> {
    if (!navigator.clipboard?.readText) return null;
    try {
      return await navigator.clipboard.readText();
    } catch {
      return null;
    }
  }

  async function getInkClipboardForScope(scope: InkClipboardScope): Promise<InkClipboardState | null> {
    const fallback = inkClipboard?.scope === scope ? inkClipboard : null;
    const text = await readTextFromClipboard();
    if (text === null) return fallback;

    const parsed = parseInkClipboardText(text);
    if (!parsed) {
      if (inkClipboard?.systemSynced) inkClipboard = null;
      return fallback && !fallback.systemSynced ? fallback : null;
    }

    if (fallback && fallback.serial === parsed.serial) {
      if (!fallback.systemSynced) {
        inkClipboard = { ...fallback, systemSynced: true };
        return inkClipboard;
      }
      return fallback;
    }

    const syncedState: InkClipboardState = {
      ...parsed,
      pasteCount: 0,
      systemSynced: true,
    };
    inkClipboard = syncedState;
    return syncedState.scope === scope ? syncedState : null;
  }

  async function onChunkChatTranscriptClick(event: MouseEvent) {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;

    const button = target.closest("button.chunk-code-copy");
    if (!(button instanceof HTMLButtonElement)) return;

    const block = button.closest(".chunk-code-block");
    const code = block?.querySelector("pre code");
    const text = code?.textContent ?? "";
    if (!text) return;

    event.preventDefault();
    const originalLabel = button.textContent || "Copy";
    button.disabled = true;

    try {
      await copyTextToClipboard(text);
      button.textContent = "Copied";
    } catch (err) {
      button.textContent = "Failed";
      void appLogWarn(`[chunk-ai] code block copy failed: ${formatLogError(err)}`);
    } finally {
      window.setTimeout(() => {
        button.textContent = originalLabel;
        button.disabled = false;
      }, CHUNK_CODE_COPY_RESET_MS);
    }
  }

  function chunkChatCodeCopy(node: HTMLDivElement) {
    const handleClick = (event: MouseEvent) => {
      void onChunkChatTranscriptClick(event);
    };
    node.addEventListener("click", handleClick);
    return {
      destroy() {
        node.removeEventListener("click", handleClick);
      },
    };
  }

  function chatProviderSupportsDirectImage(provider: ChatProvider): boolean {
    return provider === "openai" || provider === "gemini" || provider === "ollama";
  }

  function getChunkChatHistory(): ChunkChatHistoryItem[] {
    return chunkChatMessages
      .filter((message) => message.state === "complete")
      .map((message) => ({
        role: message.role,
        content: (message.historyContent ?? message.content).trim(),
        image_base64_list: message.role === "user" ? message.historyImageBase64List : undefined,
      }));
  }

  function findChunkById(chunkId: number): ChunkInfo | null {
    return currentChunks.find((entry) => entry.id === chunkId)
      ?? (chunkView?.chunk.id === chunkId ? chunkView.chunk : null);
  }

  async function ensureChunkChatContext(chunkId: number) {
    const chunk = findChunkById(chunkId);
    if (!chunk) throw new Error(`Chunk ${chunkId} is no longer available`);

    if (!chunkHasFormattedBody(chunk)) {
      chunkChatLoadingContext = true;
      try {
        await ensureChunkFormattedBody(chunkId);
      } finally {
        if (getActiveChatChunkId() === chunkId) {
          chunkChatLoadingContext = false;
        }
      }
    }

    const refreshed = findChunkById(chunkId) ?? chunk;
    if (!getChunkDisplayBody(refreshed).trim()) {
      throw new Error("No chunk text is available for AI chat yet.");
    }
  }

  function clearChunkInkContextCache() {
    chunkInkContextCache = null;
  }

  function clearNotebookInkContextCache() {
    notebookInkContextCache = null;
  }

  function strokeHasFiniteBounds(stroke: Stroke): boolean {
    return Number.isFinite(stroke.bbox.minX)
      && Number.isFinite(stroke.bbox.minY)
      && Number.isFinite(stroke.bbox.maxX)
      && Number.isFinite(stroke.bbox.maxY);
  }

  function graphHasFiniteChunkBounds(graph: SurfaceGraphObject): boolean {
    return Number.isFinite(graph.bboxX)
      && Number.isFinite(graph.bboxY)
      && Number.isFinite(graph.bboxW)
      && Number.isFinite(graph.bboxH)
      && graph.bboxW > 0
      && graph.bboxH > 0;
  }

  function getLikelyChunkInkStrokes(strokes: Stroke[]): Stroke[] {
    return strokes.filter((stroke) => stroke.points.length > 1 && strokeHasFiniteBounds(stroke));
  }

  function getRenderableChunkGraphs(graphs: SurfaceGraphObject[]): SurfaceGraphObject[] {
    return graphs.filter(graphHasFiniteChunkBounds);
  }

  function chunkInkContextFingerprint(strokes: Stroke[], graphs: SurfaceGraphObject[]): string {
    let totalPoints = 0;
    let checksum = 0;
    for (const stroke of strokes) {
      totalPoints += stroke.points.length;
      checksum += Math.round(stroke.thickness * 10);
      checksum += Math.round(stroke.bbox.minX * 1000);
      checksum += Math.round(stroke.bbox.minY * 1000);
      checksum += Math.round(stroke.bbox.maxX * 1000);
      checksum += Math.round(stroke.bbox.maxY * 1000);
      if (stroke.points.length > 0) {
        const first = stroke.points[0];
        const last = stroke.points[stroke.points.length - 1];
        checksum += Math.round((first.x + first.y + last.x + last.y) * 1000);
      }
    }
    for (const graph of graphs) {
      checksum += Math.round(graph.bboxX * 1000);
      checksum += Math.round(graph.bboxY * 1000);
      checksum += Math.round(graph.bboxW * 1000);
      checksum += Math.round(graph.bboxH * 1000);
      checksum += Math.round(graph.lineWidth * 10);
      if (graph.equation?.trim()) {
        checksum += graph.equation.trim().length * 17;
      }
      if (graph.pointsJson?.trim()) {
        checksum += graph.pointsJson.trim().length * 13;
      }
    }
    return `${strokes.length}:${graphs.length}:${totalPoints}:${checksum}`;
  }

  function getChunkVisualPageBounds(
    strokes: Stroke[],
    graphs: SurfaceGraphObject[],
  ): { minPage: number; maxPage: number } | null {
    let minPage = Number.POSITIVE_INFINITY;
    let maxPage = Number.NEGATIVE_INFINITY;
    const includeRange = (minY: number, maxY: number) => {
      if (!Number.isFinite(minY) || !Number.isFinite(maxY)) return;
      const firstPage = Math.max(0, Math.floor(minY));
      const lastPage = Math.max(firstPage, Math.ceil(maxY) - 1);
      if (firstPage < minPage) minPage = firstPage;
      if (lastPage > maxPage) maxPage = lastPage;
    };

    for (const stroke of strokes) {
      includeRange(stroke.bbox.minY, stroke.bbox.maxY);
    }
    for (const graph of graphs) {
      includeRange(graph.bboxY, graph.bboxY + graph.bboxH);
    }

    if (!Number.isFinite(minPage) || !Number.isFinite(maxPage)) {
      return null;
    }
    return { minPage, maxPage };
  }

  function chunkVisualIntersectsPage(
    pageIndex: number,
    minY: number,
    maxY: number,
  ): boolean {
    const pageTop = pageIndex;
    const pageBottom = pageIndex + 1;
    return maxY > pageTop && minY < pageBottom;
  }

  function drawChunkInkContextStroke(
    ctx: OffscreenCanvasRenderingContext2D,
    stroke: Stroke,
    cropMinX: number,
    cropMinY: number,
    pxPerNormX: number,
    pxPerNormY: number,
    lineWidth: number,
  ) {
    if (stroke.points.length < 2) return;
    const toPixel = (point: Point) => ({
      x: (point.x - cropMinX) * pxPerNormX,
      y: (point.y - cropMinY) * pxPerNormY,
    });
    const pts = stroke.points.map(toPixel);
    ctx.strokeStyle = stroke.colour;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.lineWidth = lineWidth;
    ctx.beginPath();
    ctx.moveTo(pts[0].x, pts[0].y);
    for (let i = 1; i < pts.length - 1; i++) {
      const current = pts[i];
      const next = pts[i + 1];
      const midX = (current.x + next.x) / 2;
      const midY = (current.y + next.y) / 2;
      ctx.quadraticCurveTo(current.x, current.y, midX, midY);
    }
    const lastIndex = pts.length - 1;
    ctx.quadraticCurveTo(
      pts[lastIndex - 1].x,
      pts[lastIndex - 1].y,
      pts[lastIndex].x,
      pts[lastIndex].y,
    );
    ctx.stroke();
  }

  async function rasteriseInkVisualPages(
    drawableStrokes: Stroke[],
    drawableGraphs: SurfaceGraphObject[],
    pageAspect: number,
    pageWidthWorld: number,
  ): Promise<ChunkVisualPageImage[]> {
    if (drawableStrokes.length === 0 && drawableGraphs.length === 0) return [];

    const pageBounds = getChunkVisualPageBounds(drawableStrokes, drawableGraphs);
    if (!pageBounds) return [];

    const width = CHUNK_INK_CONTEXT_TARGET_EDGE;
    const height = Math.max(
      CHUNK_INK_CONTEXT_MIN_EDGE,
      Math.min(CHUNK_INK_CONTEXT_MAX_EDGE, Math.round(width * pageAspect)),
    );
    const lineWidthScale = width / Math.max(1, pageWidthWorld);
    const pages: ChunkVisualPageImage[] = [];

    for (let pageIndex = pageBounds.minPage; pageIndex <= pageBounds.maxPage; pageIndex += 1) {
      const pageHasContent = drawableStrokes.some((s) =>
        chunkVisualIntersectsPage(pageIndex, s.bbox.minY, s.bbox.maxY),
      ) || drawableGraphs.some((g) =>
        chunkVisualIntersectsPage(pageIndex, g.bboxY, g.bboxY + g.bboxH),
      );
      if (!pageHasContent) continue;

      const offscreen = new OffscreenCanvas(width, height);
      const ctx = offscreen.getContext("2d");
      if (!ctx) throw new Error("Failed to create chunk ink context canvas");

      ctx.fillStyle = "#ffffff";
      ctx.fillRect(0, 0, width, height);

      for (const stroke of drawableStrokes) {
        if (!chunkVisualIntersectsPage(pageIndex, stroke.bbox.minY, stroke.bbox.maxY)) continue;
        const lineWidth = Math.max(1, stroke.thickness * lineWidthScale);
        drawChunkInkContextStroke(
          ctx,
          stroke,
          0,
          pageIndex,
          width,
          height,
          lineWidth,
        );
      }

      for (const graph of drawableGraphs) {
        const graphMinY = graph.bboxY;
        const graphMaxY = graph.bboxY + graph.bboxH;
        if (!chunkVisualIntersectsPage(pageIndex, graphMinY, graphMaxY)) continue;
        drawGraphCard(
          ctx,
          graphObjectToSpec(graph),
          graph.bboxX * width,
          (graph.bboxY - pageIndex) * height,
          graph.bboxW * width,
          graph.bboxH * height,
          {
            selected: false,
            showResizeHandle: false,
          },
        );
      }

      const imageBase64 = offscreenToBase64(offscreen);
      pages.push({
        pageIndex,
        imageBase64,
        imageDataUrl: `data:image/png;base64,${imageBase64}`,
      });
    }

    return pages;
  }

  async function rasteriseChunkVisualPages(chunkId: number): Promise<ChunkVisualPageImage[]> {
    const view = chunkView;
    if (!view || view.chunk.id !== chunkId) return [];
    return rasteriseInkVisualPages(
      getLikelyChunkInkStrokes(view.strokes),
      getRenderableChunkGraphs(view.graphs),
      getChunkPageAspect(),
      getChunkPageWorldSize().w,
    );
  }

  async function rasteriseNotebookVisualPages(pageId: number): Promise<ChunkVisualPageImage[]> {
    if (!isNotebookDocument() || currentPageId !== pageId) return [];
    const pageAspect = pageSize.w > 0 ? pageSize.h / pageSize.w : NOTEBOOK_PAGE_ASPECT;
    return rasteriseInkVisualPages(
      getLikelyChunkInkStrokes(strokes),
      getRenderableChunkGraphs(pageGraphs),
      pageAspect,
      Math.max(1, pageSize.w),
    );
  }

  async function transcribeChunkInkContext(chunkId: number): Promise<string | null> {
    if (!includeChunkAiVisuals) return null;
    const view = chunkView;
    if (!view || view.chunk.id !== chunkId) return null;
    const drawableStrokes = getLikelyChunkInkStrokes(view.strokes);
    const drawableGraphs = getRenderableChunkGraphs(view.graphs);
    if (drawableStrokes.length === 0 && drawableGraphs.length === 0) return null;

    const fingerprint = chunkInkContextFingerprint(drawableStrokes, drawableGraphs);
    if (
      chunkInkContextCache
      && chunkInkContextCache.chunkId === chunkId
      && chunkInkContextCache.fingerprint === fingerprint
    ) {
      return chunkInkContextCache.transcription;
    }

    const visionProvider = aiTaskSettings.vision.provider;
    const visionModel = aiTaskSettings.vision.model.trim();
    const chunkType = view.chunk.chunk_type;
    const title = view.chunk.title ?? null;
    const subject = view.chunk.subject ?? null;

    chunkInkContextTranscribing = true;
    try {
      const pages = await rasteriseChunkVisualPages(chunkId);
      if (pages.length === 0) return null;
      const sections: string[] = [];
      for (const page of pages) {
        const result = await invoke<ChunkFormattedBodyOutput>("transcribe_ai_chat_image", {
          provider: visionProvider,
          model: visionModel.length > 0 ? visionModel : null,
          imageBase64: page.imageBase64,
          chunkType,
          title,
          subject,
        });
        const body = result.body_markdown.trim();
        if (!body) continue;
        if (pages.length === 1) {
          sections.push(body);
        } else {
          sections.push(`Chunk visuals page ${page.pageIndex + 1}:\n---\n${body}\n---`);
        }
      }
      const transcription = sections.join("\n\n").trim();
      if (chunkView?.chunk.id === chunkId) {
        chunkInkContextCache = {
          chunkId,
          fingerprint,
          transcription: transcription || null,
        };
      }
      return transcription || null;
    } finally {
      chunkInkContextTranscribing = false;
    }
  }

  async function transcribeNotebookInkContext(pageId: number): Promise<string | null> {
    if (!isNotebookDocument() || currentPageId !== pageId) return null;
    const drawableStrokes = getLikelyChunkInkStrokes(strokes);
    const drawableGraphs = getRenderableChunkGraphs(pageGraphs);
    if (drawableStrokes.length === 0 && drawableGraphs.length === 0) return null;

    const fingerprint = chunkInkContextFingerprint(drawableStrokes, drawableGraphs);
    if (
      notebookInkContextCache
      && notebookInkContextCache.pageId === pageId
      && notebookInkContextCache.fingerprint === fingerprint
    ) {
      return notebookInkContextCache.transcription;
    }

    const visionProvider = aiTaskSettings.vision.provider;
    const visionModel = aiTaskSettings.vision.model.trim();
    const title = selectedBook?.title ?? `Canvas ${currentPage}`;

    chunkInkContextTranscribing = true;
    try {
      const pages = await rasteriseNotebookVisualPages(pageId);
      if (pages.length === 0) return null;
      const sections: string[] = [];
      for (const page of pages) {
        const result = await invoke<ChunkFormattedBodyOutput>("transcribe_ai_chat_image", {
          provider: visionProvider,
          model: visionModel.length > 0 ? visionModel : null,
          imageBase64: page.imageBase64,
          chunkType: "explanation",
          title,
          subject: `Notebook canvas ${currentPage}`,
        });
        const body = result.body_markdown.trim();
        if (!body) continue;
        if (pages.length === 1) {
          sections.push(body);
        } else {
          sections.push(`Notebook canvas page ${page.pageIndex + 1}:\n---\n${body}\n---`);
        }
      }
      const transcription = sections.join("\n\n").trim();
      if (currentPageId === pageId) {
        notebookInkContextCache = {
          pageId,
          fingerprint,
          transcription: transcription || null,
        };
      }
      return transcription || null;
    } finally {
      chunkInkContextTranscribing = false;
    }
  }

  async function enrichPromptWithChunkInkContext(chunkId: number, prompt: string): Promise<string> {
    if (!includeChunkAiVisuals) return prompt;
    try {
      const transcription = await transcribeChunkInkContext(chunkId);
      if (!transcription) return prompt;
      return `${prompt}\n\nAdditional context from chunk ink notes:\n---\n${transcription}\n---`;
    } catch (err) {
      await appLogWarn(`[chunk-ai] chunk ink context failed chunkId=${chunkId}: ${formatLogError(err)}`);
      return prompt;
    }
  }

  async function transcribePendingChatAttachment(
    options: {
      chunkId: number | null;
      pageNumber: number;
    },
    attachment: PendingChatAttachment,
  ): Promise<string | null> {
    if (attachment.transcribedBody?.trim()) return attachment.transcribedBody.trim();
    const contextChunk = options.chunkId == null ? null : findChunkById(options.chunkId);
    const visionProvider = aiTaskSettings.vision.provider;
    const visionModel = aiTaskSettings.vision.model.trim();
    const fallbackTitle = contextChunk ? null : `Page ${options.pageNumber}`;

    chatAttachmentTranscribing = true;
    try {
      const result = await invoke<ChunkFormattedBodyOutput>("transcribe_ai_chat_image", {
        provider: visionProvider,
        model: visionModel.length > 0 ? visionModel : null,
        imageBase64: attachment.imageBase64,
        chunkType: contextChunk?.chunk_type ?? "explanation",
        title: contextChunk?.title ?? fallbackTitle,
        subject: contextChunk?.subject ?? null,
      });
      const body = result.body_markdown.trim();
      if (
        pendingChatAttachment
        && pendingChatAttachment.createdAt === attachment.createdAt
      ) {
        pendingChatAttachment = {
          ...pendingChatAttachment,
          transcribedBody: body || null,
        };
      }
      return body || null;
    } finally {
      chatAttachmentTranscribing = false;
    }
  }

  interface PreparedChatUserMessage {
    displayContent: string;
    historyContent: string;
    imageBase64List?: string[];
    imageDataUrls?: string[];
    attachmentCreatedAt?: number;
  }

  async function prepareChunkChatUserMessage(
    draftContent: string,
    options: {
      chunkId: number | null;
      pageId: number | null;
      pageNumber: number;
    },
  ): Promise<PreparedChatUserMessage> {
    const { chunkId, pageId, pageNumber } = options;
    const attachment = pendingChatAttachment;
    const supportsDirectImage = chatProviderSupportsDirectImage(aiTaskSettings.chat.provider);
    const historyParts: string[] = [];

    const imageBase64List: string[] = [];
    const imageDataUrls: string[] = [];
    let attachmentCreatedAt: number | undefined;
    let attachedImageLabel: string | null = null;

    const shouldAttachChunkVisuals = chunkId != null && includeChunkAiVisuals;
    let attachedChunkVisualPages = 0;
    if (shouldAttachChunkVisuals && supportsDirectImage) {
      try {
        const visualPages = await rasteriseChunkVisualPages(chunkId);
        if (visualPages.length > 0) {
          attachedChunkVisualPages = visualPages.length;
          imageBase64List.push(...visualPages.map((page) => page.imageBase64));
          imageDataUrls.push(...visualPages.map((page) => page.imageDataUrl));
          attachedImageLabel = visualPages.length === 1
            ? "Attached visuals"
            : `Attached ${visualPages.length} visual pages`;
        }
      } catch (err) {
        await appLogWarn(`[chunk-ai] chunk ink rasterisation failed chunkId=${chunkId}: ${formatLogError(err)}`);
      }
    }

    const shouldAttachNotebookVisuals = chunkId == null && pageId != null && isNotebookDocument();
    let attachedNotebookVisualPages = 0;
    if (shouldAttachNotebookVisuals && supportsDirectImage) {
      try {
        const visualPages = await rasteriseNotebookVisualPages(pageId);
        if (visualPages.length > 0) {
          attachedNotebookVisualPages = visualPages.length;
          imageBase64List.push(...visualPages.map((page) => page.imageBase64));
          imageDataUrls.push(...visualPages.map((page) => page.imageDataUrl));
          attachedImageLabel = visualPages.length === 1
            ? "Attached canvas ink"
            : `Attached ${visualPages.length} canvas ink pages`;
        }
      } catch (err) {
        await appLogWarn(`[notebook-ai] canvas ink rasterisation failed pageId=${pageId}: ${formatLogError(err)}`);
      }
    }

    if (attachment) {
      attachmentCreatedAt = attachment.createdAt;
      if (supportsDirectImage) {
        imageBase64List.push(attachment.imageBase64);
        imageDataUrls.push(attachment.imageDataUrl);
        attachedImageLabel = imageBase64List.length > 1
          ? `Attached ${imageBase64List.length} images`
          : "Attached selection";
      } else {
        let transcription: string | null = null;
        try {
          transcription = await transcribePendingChatAttachment({ chunkId, pageNumber }, attachment);
        } catch (err) {
          await appLogWarn(`[chunk-ai] attachment transcription failed: ${formatLogError(err)}`);
        }
        if (transcription) {
          historyParts.push(`Attached selection transcription:\n---\n${transcription}\n---`);
        } else {
          historyParts.push("Attached selection image context was provided, but transcription was unavailable.");
        }
      }
    }

    if (shouldAttachNotebookVisuals && (!supportsDirectImage || attachedNotebookVisualPages === 0)) {
      try {
        const inkTranscription = await transcribeNotebookInkContext(pageId);
        if (inkTranscription) {
          historyParts.push(`Notebook canvas ink transcription:\n---\n${inkTranscription}\n---`);
        }
      } catch (err) {
        await appLogWarn(`[notebook-ai] canvas ink context failed pageId=${pageId}: ${formatLogError(err)}`);
      }
    }

    if (shouldAttachChunkVisuals && (!supportsDirectImage || attachedChunkVisualPages === 0)) {
      try {
        const inkTranscription = await transcribeChunkInkContext(chunkId);
        if (inkTranscription) {
          historyParts.push(`Chunk ink notes transcription:\n---\n${inkTranscription}\n---`);
        }
      } catch (err) {
        await appLogWarn(`[chunk-ai] chunk ink context failed chunkId=${chunkId}: ${formatLogError(err)}`);
      }
    }

    if (draftContent) {
      try {
        const mentionRows = await resolveMentionsForCurrentDocument(draftContent, 12);
        const mentionContext = buildMentionContextBlock(mentionRows);
        if (mentionContext) {
          historyParts.push(mentionContext);
        }
      } catch (err) {
        await appLogWarn(`[chunk-ai] mention resolution failed: ${formatLogError(err)}`);
      }
    }

    const leadText = draftContent || (
      imageBase64List.length > 0 || attachment
        ? `Use the attached ${imageBase64List.length > 1 ? "images" : "image"} as additional context.`
        : shouldAttachNotebookVisuals
          ? "Use the current notebook canvas ink context."
          : chunkId != null
            ? "Use the available chunk context."
            : "Use the current page context."
    );
    historyParts.unshift(leadText);

    return {
      displayContent: draftContent || attachedImageLabel || "Message",
      historyContent: historyParts.join("\n\n"),
      imageBase64List: imageBase64List.length > 0 ? imageBase64List : undefined,
      imageDataUrls: imageDataUrls.length > 0 ? imageDataUrls : undefined,
      attachmentCreatedAt,
    };
  }

  async function cancelChunkChatForReset() {
    const requestId = chunkChatActiveRequestId;
    resetChunkChatState();
    if (!requestId) return;
    try {
      await invoke("cancel_chunk_ai_stream", { requestId });
    } catch (err) {
      await appLogWarn(`[chunk-ai] cancel on reset failed requestId=${requestId}: ${formatLogError(err)}`);
    }
  }

  async function stopChunkChat() {
    const requestId = chunkChatActiveRequestId;
    if (!requestId) return;

    updateChunkChatMessage(requestId, (message) => ({
      ...message,
      state: "stopped",
      content: message.content || "Stopped.",
    }));
    chunkChatActiveRequestId = null;
    chunkChatStreaming = false;
    chunkChatLoadingContext = false;
    chunkChatError = null;
    queueChunkChatScroll();

    try {
      await invoke("cancel_chunk_ai_stream", { requestId });
    } catch (err) {
      await appLogWarn(`[chunk-ai] stop failed requestId=${requestId}: ${formatLogError(err)}`);
    }
  }

  async function sendChunkChatMessage() {
    const chunkId = getActiveChatChunkId();
    const pageId = getActiveViewerChatPageId();
    const pageNumber = chunkView?.pageNumber ?? currentPage;
    const draftContent = chunkChatDraft.trim();
    const hasAttachment = !!pendingChatAttachment;
    if (chunkId == null && pageId == null) return;
    if (chunkChatStreaming || chunkChatLoadingContext) return;
    if (!draftContent && !hasAttachment) return;

    chunkChatError = null;
    chunkChatLoadingContext = true;

    let prepared: PreparedChatUserMessage;
    try {
      await flushChunkTextSaves();
      await flushGlossarySave();
      prepared = await prepareChunkChatUserMessage(draftContent, { chunkId, pageId, pageNumber });
      if (chunkId != null) {
        await ensureChunkChatContext(chunkId);
        if (getActiveChatChunkId() !== chunkId) {
          chunkChatLoadingContext = false;
          return;
        }
      } else if (pageId != null) {
        if (getActiveViewerChatPageId() !== pageId) {
          chunkChatLoadingContext = false;
          return;
        }
      }
    } catch (err) {
      chunkChatLoadingContext = false;
      chunkChatError = formatLogError(err);
      return;
    }

    const requestId = makeChunkChatRequestId();
    if (
      prepared.attachmentCreatedAt != null
      && pendingChatAttachment?.createdAt === prepared.attachmentCreatedAt
    ) {
      clearPendingChatAttachment();
    }
    chunkChatDraft = "";
    chunkChatMessages = [
      ...chunkChatMessages,
      {
        id: `${requestId}-user`,
        role: "user",
        content: prepared.displayContent,
        historyContent: prepared.historyContent,
        historyImageBase64List: prepared.imageBase64List,
        imageDataUrls: prepared.imageDataUrls,
        state: "complete",
      },
      {
        id: requestId,
        role: "assistant",
        content: "",
        state: "streaming",
      },
    ];
    chunkChatStreaming = true;
    chunkChatLoadingContext = false;
    chunkChatActiveRequestId = requestId;
    queueChunkChatScroll();

    try {
      const history = getChunkChatHistory();
      const chatProvider = aiTaskSettings.chat.provider;
      const chatModel = aiTaskSettings.chat.model.trim();
      if (chunkId != null) {
        await invoke("start_chunk_ai_stream", {
          requestId,
          chunkId,
          provider: chatProvider,
          model: chatModel.length > 0 ? chatModel : null,
          history,
          searchQuery: draftContent || null,
        });
        await appLogInfo(
          `[chunk-ai] started requestId=${requestId} chunkId=${chunkId} provider=${chatProvider} model=${chatModel || "auto"} history=${history.length}`,
        );
      } else if (pageId != null) {
        await invoke("start_page_ai_stream", {
          requestId,
          pageId,
          provider: chatProvider,
          model: chatModel.length > 0 ? chatModel : null,
          history,
          searchQuery: draftContent || null,
        });
        await appLogInfo(
          `[viewer-ai] started requestId=${requestId} pageId=${pageId} provider=${chatProvider} model=${chatModel || "auto"} history=${history.length}`,
        );
      } else {
        throw new Error("AI context is not ready.");
      }
    } catch (err) {
      const message = formatLogError(err);
      chunkChatError = message;
      if (chunkChatActiveRequestId === requestId) {
        chunkChatActiveRequestId = null;
        chunkChatStreaming = false;
        chunkChatLoadingContext = false;
      }
      updateChunkChatMessage(requestId, (assistant) => ({
        ...assistant,
        state: "error",
        content: assistant.content || "Unable to reply.",
      }));
      queueChunkChatScroll();
      const contextLabel = chunkId != null ? `chunkId=${chunkId}` : `pageId=${pageId ?? "unknown"}`;
      await appLogWarn(
        `[chunk-ai] failed to start requestId=${requestId} ${contextLabel}: ${message}`,
      );
    }
  }

  async function runChunkRewritePrompt() {
    const view = chunkView;
    const prompt = chunkRewritePrompt.trim();
    if (!view || !prompt || chunkRewriteLoading || chunkRewriteApplying) return;

    chunkRewriteLoading = true;
    chunkRewriteError = null;
    chunkRewriteSuggestion = null;
    try {
      await flushChunkTextSaves();
      if (chunkView?.chunk.id !== view.chunk.id) return;
      const promptWithInkContext = await enrichPromptWithChunkInkContext(view.chunk.id, prompt);
      if (chunkView?.chunk.id !== view.chunk.id) return;

      const chatProvider = aiTaskSettings.chat.provider;
      const chatModel = aiTaskSettings.chat.model.trim();
      const suggestion = await invoke<ChunkRewriteSuggestionOutput>("rewrite_chunk_text_with_prompt", {
        chunkId: view.chunk.id,
        prompt: promptWithInkContext,
        provider: chatProvider,
        model: chatModel.length > 0 ? chatModel : null,
      });
      if (chunkView?.chunk.id !== view.chunk.id) return;

      chunkRewriteSuggestion = suggestion;
      await appLogInfo(
        `[chunk-ai] rewrite ready chunkId=${view.chunk.id} provider=${chatProvider} model=${chatModel || "auto"} titleChanged=${suggestion.title !== null} bodyChanged=${suggestion.body_markdown !== null}`,
      );
    } catch (err) {
      const message = formatLogError(err);
      if (chunkView?.chunk.id !== view.chunk.id) return;
      chunkRewriteError = message;
      await appLogWarn(`[chunk-ai] rewrite failed chunkId=${view.chunk.id}: ${message}`);
    } finally {
      chunkRewriteLoading = false;
    }
  }

  function discardChunkRewriteSuggestion() {
    chunkRewriteSuggestion = null;
    chunkRewriteError = null;
  }

  async function applyChunkRewriteSuggestion() {
    if (!chunkView || !chunkRewriteSuggestion || chunkRewriteApplying) return;
    const chunkId = chunkView.chunk.id;
    const suggestion = chunkRewriteSuggestion;
    chunkRewriteApplying = true;
    try {
      if (suggestion.title !== null) {
        chunkTitleDraft = suggestion.title;
        chunkTitleSaveError = null;
        chunkTitlePendingChunkId = chunkId;
      }
      if (suggestion.body_markdown !== null) {
        chunkBodyDraft = suggestion.body_markdown;
        chunkBodySaveError = null;
        chunkBodyPendingChunkId = chunkId;
      }

      // Keep the read-first UX consistent: AI apply exits inline edit mode.
      chunkTitleEditing = false;
      chunkBodyEditing = false;

      await flushChunkTextSaves();
      chunkRewriteSuggestion = null;
      chunkRewriteError = null;
    } finally {
      chunkRewriteApplying = false;
    }
  }

  async function runGlossaryRewritePrompt() {
    const view = chunkView;
    const prompt = glossaryRewritePrompt.trim();
    if (!view || !prompt || glossaryRewriteLoading || glossaryRewriteApplying) return;

    glossaryRewriteLoading = true;
    glossaryRewriteError = null;
    glossaryRewriteSuggestion = null;
    resetGlossaryRewriteTypstPreviewState();
    try {
      await flushChunkTextSaves();
      await flushGlossarySave();
      if (chunkView?.chunk.id !== view.chunk.id) return;
      const promptWithInkContext = await enrichPromptWithChunkInkContext(view.chunk.id, prompt);
      if (chunkView?.chunk.id !== view.chunk.id) return;

      const chatProvider = aiTaskSettings.chat.provider;
      const chatModel = aiTaskSettings.chat.model.trim();
      const suggestion = await invoke<ChunkGlossaryRewriteSuggestionOutput>("rewrite_chunk_glossary_with_prompt", {
        chunkId: view.chunk.id,
        prompt: promptWithInkContext,
        provider: chatProvider,
        model: chatModel.length > 0 ? chatModel : null,
      });
      if (chunkView?.chunk.id !== view.chunk.id) return;

      glossaryRewriteSuggestion = suggestion;
      if (glossaryFormat === "typst") {
        void refreshGlossaryRewriteTypstPreview(suggestion.glossary_markdown);
      }
      await appLogInfo(
        `[chunk-ai] glossary rewrite ready chunkId=${view.chunk.id} provider=${chatProvider} model=${chatModel || "auto"} chars=${suggestion.glossary_markdown.length}`,
      );
    } catch (err) {
      const message = formatLogError(err);
      if (chunkView?.chunk.id !== view.chunk.id) return;
      glossaryRewriteError = message;
      await appLogWarn(`[chunk-ai] glossary rewrite failed chunkId=${view.chunk.id}: ${message}`);
    } finally {
      glossaryRewriteLoading = false;
    }
  }

  function discardGlossaryRewriteSuggestion() {
    glossaryRewriteSuggestion = null;
    glossaryRewriteError = null;
    resetGlossaryRewriteTypstPreviewState();
  }

  async function applyGlossaryRewriteSuggestion() {
    if (!chunkView || !glossaryRewriteSuggestion || glossaryRewriteApplying) return;
    const chunkId = chunkView.chunk.id;
    glossaryRewriteApplying = true;
    try {
      glossaryDraft = glossaryRewriteSuggestion.glossary_markdown;
      glossarySaveError = null;
      clearGlossaryPdfExportNotices();
      glossaryPendingChunkId = chunkId;
      await flushGlossarySave();
      setGlossaryMode('preview');
      if (glossaryFormat === "typst") {
        scheduleGlossaryTypstPreview(0);
      }
      glossaryRewriteSuggestion = null;
      glossaryRewriteError = null;
      resetGlossaryRewriteTypstPreviewState();
    } finally {
      glossaryRewriteApplying = false;
    }
  }

  function onChunkChatKeydown(event: KeyboardEvent) {
    if (event.key !== "Enter" || event.shiftKey) return;
    event.preventDefault();
    void sendChunkChatMessage();
  }

  let chunkingBusy = $derived(
    reChunkingPage || currentPageChunkingActive,
  );
  let canRechunkPage = $derived(
    !!selectedBook && !isNotebookDocument() && currentChunks.length > 0 && !chunkingBusy,
  );
  let canBatchChunkDocument = $derived(
    !!selectedBook && !isNotebookDocument() && totalPages > 0 && !aiSettingsSaving && !batchChunkStarting,
  );

  function parseBatchChunkPageInput(rawValue: string): number | null {
    const trimmed = rawValue.trim();
    if (!/^\d+$/.test(trimmed)) return null;
    const parsed = Number(trimmed);
    if (!Number.isSafeInteger(parsed) || parsed < 1) return null;
    return parsed;
  }

  let batchChunkProgressPercent = $derived(
    !batchChunkProgress
      ? 0
      : batchChunkProgress.totalPages <= 0
        ? 100
        : Math.round((batchChunkProgress.completedPages / batchChunkProgress.totalPages) * 100),
  );

  function pushBatchChunkProgressMessage(message: string) {
    if (!batchChunkProgress) return;
    const timestamp = new Date().toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    batchChunkProgress = {
      ...batchChunkProgress,
      statusMessage: message,
      messages: [`${timestamp}  ${message}`, ...batchChunkProgress.messages].slice(0, 6),
    };
  }

  function beginBatchChunkProgress(options: {
    docId: number;
    startPage: number;
    endPage: number;
    providerLabel: string;
    modelLabel: string;
  }) {
    const initialStatus = `Starting batch chunking with ${options.providerLabel}${options.modelLabel ? ` (${options.modelLabel})` : ""}...`;
    batchChunkProgress = {
      docId: options.docId,
      startPage: options.startPage,
      endPage: options.endPage,
      providerLabel: options.providerLabel,
      modelLabel: options.modelLabel,
      totalPages: 0,
      completedPages: 0,
      failedPages: 0,
      skippedPages: 0,
      startedPages: [],
      extractedPages: [],
      groupedPages: [],
      failedPageNumbers: [],
      statusMessage: initialStatus,
      messages: [],
      finished: false,
    };
    pushBatchChunkProgressMessage(initialStatus);
  }

  function dismissBatchChunkProgress() {
    batchChunkProgress = null;
  }

  function onBatchChunkProgressEvent(phase: string, pageNumber: number) {
    const progress = batchChunkProgress;
    if (!progress) return;
    if (!progress.startedPages.includes(pageNumber)) return;

    if (phase === "extracted") {
      if (progress.extractedPages.includes(pageNumber)) return;
      batchChunkProgress = {
        ...progress,
        extractedPages: [...progress.extractedPages, pageNumber],
      };
      pushBatchChunkProgressMessage(`Page ${pageNumber}: extracted text, grouping next.`);
      return;
    }

    if (phase === "grouped") {
      if (progress.groupedPages.includes(pageNumber) || progress.failedPageNumbers.includes(pageNumber)) return;
      const completedPages = progress.completedPages + 1;
      const groupedPages = [...progress.groupedPages, pageNumber];
      const finished = completedPages >= progress.totalPages;
      let statusMessage = `Page ${pageNumber}: chunked (${completedPages}/${progress.totalPages}).`;
      if (finished) {
        statusMessage = progress.failedPages > 0
          ? `Batch finished: ${progress.totalPages - progress.failedPages}/${progress.totalPages} pages succeeded, ${progress.failedPages} failed, ${progress.skippedPages} skipped.`
          : `Batch finished: ${progress.totalPages}/${progress.totalPages} pages chunked, ${progress.skippedPages} skipped.`;
      }
      batchChunkProgress = {
        ...progress,
        completedPages,
        groupedPages,
        finished,
      };
      pushBatchChunkProgressMessage(statusMessage);
      return;
    }

    if (phase === "failed") {
      if (progress.failedPageNumbers.includes(pageNumber) || progress.groupedPages.includes(pageNumber)) return;
      const completedPages = progress.completedPages + 1;
      const failedPages = progress.failedPages + 1;
      const failedPageNumbers = [...progress.failedPageNumbers, pageNumber];
      const finished = completedPages >= progress.totalPages;
      const statusMessage = finished
        ? `Batch finished with failures: ${progress.totalPages - failedPages}/${progress.totalPages} pages succeeded, ${failedPages} failed, ${progress.skippedPages} skipped.`
        : `Page ${pageNumber}: chunking failed (${completedPages}/${progress.totalPages} finished).`;
      batchChunkProgress = {
        ...progress,
        completedPages,
        failedPages,
        failedPageNumbers,
        finished,
      };
      pushBatchChunkProgressMessage(statusMessage);
    }
  }

  async function reChunkCurrentPage() {
    if (!selectedBook || !canRechunkPage) return;

    const chunkingProvider = aiTaskSettings.chunking.provider;
    const chunkingModel = aiTaskSettings.chunking.model.trim();
    const visionProvider = aiTaskSettings.vision.provider;
    const visionModel = aiTaskSettings.vision.model.trim();
    const providerLabel = getChunkingProviderLabel(chunkingProvider);
    const visionProviderLabel = getProviderLabel(visionProvider);
    const visionModelLabel = visionModel || "auto";
    const confirmed = window.confirm(
      `Re-chunk this page with ${providerLabel}?\n\nChunk-attached notes on this page will be deleted when the new chunks are saved. Page notes will stay.\n\nAfter re-chunking, chunk text will be regenerated with ${visionProviderLabel} (${visionModelLabel}). This may use additional API calls.`,
    );
    if (!confirmed) return;

    error = null;
    reChunkingPage = true;
    currentPageChunkingActive = true;
    currentChunkingStatus = "grouping";

    try {
      await invoke("rechunk_page", {
        sourceDocumentId: selectedBook.id,
        pageNumber: currentPage,
        provider: chunkingProvider,
        model: chunkingModel.length > 0 ? chunkingModel : null,
      });
      await appLogInfo(
        `[chunking] manual re-chunk complete: doc=${selectedBook.id} page=${currentPage} provider=${chunkingProvider} model=${chunkingModel || "auto"}`,
      );
      if (currentPageId !== null) {
        await loadChunksForPage(currentPageId);
      }
      const chunksForVisionRegeneration = currentChunks.filter((chunk) => chunk.chunk_type !== "noise");
      if (chunksForVisionRegeneration.length > 0) {
        await appLogInfo(
          `[chunking] manual re-chunk: regenerating formatted bodies for ${chunksForVisionRegeneration.length} chunks using vision provider=${visionProvider} model=${visionModelLabel}`,
        );
        let regeneratedCount = 0;
        let failedCount = 0;
        for (const chunk of chunksForVisionRegeneration) {
          const ok = await ensureChunkFormattedBody(chunk.id, true);
          if (ok) regeneratedCount += 1;
          else failedCount += 1;
        }
        if (failedCount > 0) {
          error = `Re-chunked page, but ${failedCount}/${chunksForVisionRegeneration.length} chunk bodies failed to regenerate with ${visionProviderLabel} (${visionModelLabel}).`;
          await appLogWarn(
            `[chunking] manual re-chunk: formatted body regeneration partial failure provider=${visionProvider} model=${visionModelLabel} regenerated=${regeneratedCount} failed=${failedCount}`,
          );
        } else {
          await appLogInfo(
            `[chunking] manual re-chunk: formatted body regeneration complete provider=${visionProvider} model=${visionModelLabel} regenerated=${regeneratedCount}`,
          );
        }
      }
      await logChunkingStatus(selectedBook.id, "after manual re-chunk");
    } catch (err) {
      currentChunkingStatus = "failed";
      error = String(err);
      await appLogError(
        `[chunking] manual re-chunk failed: doc=${selectedBook.id} page=${currentPage} provider=${chunkingProvider} model=${chunkingModel || "auto"}: ${formatLogError(err)}`,
      );
    } finally {
      reChunkingPage = false;
      if (selectedBook) {
        void logChunkingStatus(selectedBook.id, "after manual re-chunk settle");
        void refreshCurrentPageChunkingActive(selectedBook.id, currentPage);
      }
    }
  }

  async function startBatchChunking(startPage: number, endPage: number, mode: "range" | "all") {
    const activeBook = selectedBook;
    if (!activeBook || !canBatchChunkDocument) return;

    // Use the currently selected dropdown values, even if settings were not "saved".
    const selectedChunkingProvider = aiTaskSettings.chunking.provider;
    const selectedChunkingModel = aiTaskSettings.chunking.model.trim();
    const skipChunkedPages = batchChunkSkipChunkedPages;
    const providerLabel = getChunkingProviderLabel(selectedChunkingProvider);
    const pageLabel = mode === "all"
      ? `all pages (${startPage}-${endPage})`
      : startPage === endPage ? `page ${startPage}` : `pages ${startPage}-${endPage}`;
    const skipNote = skipChunkedPages
      ? "Already chunked pages are skipped."
      : "Already chunked pages will be re-chunked. Chunk-attached notes on those pages will be deleted when new chunks are saved. Page notes stay.";
    const confirmed = window.confirm(
      `Chunk ${pageLabel} with ${providerLabel}?\n\n${skipNote}`,
    );
    if (!confirmed) return;

    closeAiKeySettings();
    batchChunkError = null;
    batchChunkFeedback = null;
    batchChunkStarting = true;
    beginBatchChunkProgress({
      docId: activeBook.id,
      startPage,
      endPage,
      providerLabel,
      modelLabel: selectedChunkingModel || "auto",
    });

    try {
      const result = await invoke<EnsureChunkingRangeResult>("ensure_chunking_for_page_range", {
        sourceDocumentId: activeBook.id,
        startPage,
        endPage,
        skipChunkedPages,
        provider: selectedChunkingProvider,
        model: selectedChunkingModel.length > 0 ? selectedChunkingModel : null,
      });
      const started = result.started_pages;
      const skipped = result.skipped_pages;
      const startedLabel = started === 1 ? "page" : "pages";
      batchChunkFeedback = started > 0
        ? `Started chunking ${started} ${startedLabel}; skipped ${skipped}.`
        : `No pages started; skipped ${skipped}.`;
      if (batchChunkProgress && batchChunkProgress.docId === activeBook.id) {
        const startedPages = result.started_page_numbers;
        const finished = startedPages.length === 0;
        batchChunkProgress = {
          ...batchChunkProgress,
          totalPages: startedPages.length,
          completedPages: 0,
          failedPages: 0,
          skippedPages: skipped,
          startedPages,
          extractedPages: [],
          groupedPages: [],
          failedPageNumbers: [],
          finished,
        };
        pushBatchChunkProgressMessage(
          started > 0
            ? `Queued ${started} ${startedLabel} (${startedPages.join(", ")}). Skipped ${skipped}.`
            : `No new pages queued. Skipped ${skipped}.`,
        );
      }
      if (selectedBook?.id === activeBook.id && currentPage >= startPage && currentPage <= endPage && started > 0) {
        currentChunkingStatus = "extracting";
        void refreshCurrentPageChunkingActive(activeBook.id, currentPage);
      }
      await appLogInfo(
        `[chunking] batch start: doc=${activeBook.id} pages=${startPage}-${endPage} skipChunkedPages=${skipChunkedPages} provider=${selectedChunkingProvider} model=${selectedChunkingModel || "auto"} started=${started} skipped=${skipped}`,
      );
      void logChunkingStatus(activeBook.id, "after batch chunk start");
    } catch (err) {
      batchChunkError = formatLogError(err);
      if (batchChunkProgress && batchChunkProgress.docId === activeBook.id) {
        batchChunkProgress = {
          ...batchChunkProgress,
          finished: true,
        };
        pushBatchChunkProgressMessage(`Failed to start batch chunking: ${formatLogError(err)}`);
      }
      await appLogError(
        `[chunking] batch start failed: doc=${activeBook.id} pages=${startPage}-${endPage} provider=${selectedChunkingProvider} model=${selectedChunkingModel || "auto"}: ${formatLogError(err)}`,
      );
    } finally {
      batchChunkStarting = false;
    }
  }

  async function chunkPageRangeFromInputs() {
    batchChunkError = null;
    batchChunkFeedback = null;

    if (!selectedBook) {
      batchChunkError = "Open a PDF to use batch chunking.";
      return;
    }
    if (totalPages < 1) {
      batchChunkError = "This PDF has no pages to chunk.";
      return;
    }

    const startPage = parseBatchChunkPageInput(batchChunkStartInput);
    const endPage = parseBatchChunkPageInput(batchChunkEndInput);
    if (startPage === null || endPage === null) {
      batchChunkError = `Enter valid page numbers between 1 and ${totalPages}.`;
      return;
    }
    if (startPage > totalPages || endPage > totalPages) {
      batchChunkError = `Page range must be within 1-${totalPages}.`;
      return;
    }
    if (startPage > endPage) {
      batchChunkError = "Start page must be less than or equal to end page.";
      return;
    }

    batchChunkStartInput = String(startPage);
    batchChunkEndInput = String(endPage);
    await startBatchChunking(startPage, endPage, "range");
  }

  async function chunkWholePdf() {
    batchChunkError = null;
    batchChunkFeedback = null;

    if (!selectedBook) {
      batchChunkError = "Open a PDF to use batch chunking.";
      return;
    }
    if (totalPages < 1) {
      batchChunkError = "This PDF has no pages to chunk.";
      return;
    }

    batchChunkStartInput = "1";
    batchChunkEndInput = String(totalPages);
    await startBatchChunking(1, totalPages, "all");
  }

  async function loadChunksForPage(
    pageId: number,
    pageNum = currentPage,
    bookId: number | null = selectedBook?.id ?? null,
  ): Promise<ChunkInfo[]> {
    let chunks: ChunkInfo[] = [];
    try {
      chunks = await invoke<ChunkInfo[]>("get_chunks_for_page", { pageId });
      void appLogInfo(
        `[chunking] page data loaded: pageId=${pageId} page=${pageNum} chunks=${chunks.length}`,
      );
    } catch {
      void appLogWarn(`[chunking] failed to load chunks for pageId=${pageId}`);
    }
    if (bookId != null) {
      chunkPageCache.set(pageKey(bookId, pageNum), chunks);
    }
    if (bookId != null && selectedBook?.id === bookId && currentPage === pageNum && currentPageId === pageId) {
      currentChunks = chunks;
      markDirty();
    }
    if (chunkView?.pageNumber === pageNum) {
      chunkNavigationPageChunks = chunks;
    }
    // Preload surfaces for this page's chunks so tap-open is instant.
    for (const c of chunks) void ensureChunkSurface(c.id);
    return chunks;
  }

  async function ensureChunkSurface(chunkId: number): Promise<ChunkSurfaceCache> {
    const cached = chunkSurfaceCache.get(chunkId);
    if (cached) return cached;
    const existing = chunkSurfaceRequests.get(chunkId);
    if (existing) return existing;

    const req = (async () => {
      const surfaceId = await invoke<number>("get_or_create_chunk_surface", { chunkId });
      const raw = await invoke<SurfaceStrokeOutput[]>("load_surface_strokes", { surfaceId });
      const rawGraphs = await invoke<SurfaceGraphObjectOutput[]>("load_surface_graph_objects", { surfaceId });
      const strokes: Stroke[] = raw.map(s => ({
        id: s.id,
        clientId: makeStrokeClientId(),
        colour: s.colour,
        thickness: s.thickness ?? 1,
        points: s.points.map(p => ({ x: p.x, y: p.y, pressure: 0.5 })),
        bbox: { minX: s.min_x, minY: s.min_y, maxX: s.max_x, maxY: s.max_y },
        chunkId,
      }));
      const graphs = rawGraphs.map((graph) => graphOutputToObject(graph));
      const entry: ChunkSurfaceCache = { surfaceId, strokes, graphs };
      chunkSurfaceCache.set(chunkId, entry);
      return entry;
    })();
    chunkSurfaceRequests.set(chunkId, req);
    try {
      return await req;
    } finally {
      chunkSurfaceRequests.delete(chunkId);
    }
  }

  function chunkAt(normX: number, normY: number): ChunkInfo | null {
    let bestMatch: ChunkInfo | null = null;
    let bestArea = Number.POSITIVE_INFINITY;

    for (const c of currentChunks) {
      if (c.chunk_type === "noise") continue;
      if (
        normX < c.bbox_x || normX > c.bbox_x + c.bbox_w ||
        normY < c.bbox_y || normY > c.bbox_y + c.bbox_h
      ) continue;

      const area = Math.max(0, c.bbox_w) * Math.max(0, c.bbox_h);
      if (!bestMatch || area < bestArea) {
        bestMatch = c;
        bestArea = area;
      }
    }

    return bestMatch;
  }

  function chunkHasFormattedBody(chunk: ChunkInfo): boolean {
    return !!chunk.formatted_body_md?.trim();
  }

  interface ChunkTitleDisplay {
    heading: string;
    indexLabel: string | null;
    suppressionCandidates: string[];
  }

  const CHUNK_TITLE_TYPE_ALIASES: Record<string, string[]> = {
    definition: ["definition"],
    theorem: ["theorem", "lemma", "proposition", "corollary"],
    proof: ["proof"],
    exercise: ["exercise"],
    example: ["example"],
    explanation: ["explanation", "note", "remark"],
  };

  function getChunkTitleDisplay(chunk: Pick<ChunkInfo, "chunk_type" | "title">): ChunkTitleDisplay | null {
    const rawTitle = normalizeChunkTitleText(chunk.title ?? "");
    if (!rawTitle) return null;

    let heading = rawTitle;
    let indexLabel: string | null = null;
    let typeLabel: string | null = null;

    const indexedTypeMatch = heading.match(/^(\d+(?:\.\d+)*[a-z]?)\s+([a-z][a-z0-9-]*)\s*:\s*(.+)$/i);
    if (indexedTypeMatch && isChunkTypeLabel(indexedTypeMatch[2], chunk.chunk_type)) {
      indexLabel = indexedTypeMatch[1];
      typeLabel = indexedTypeMatch[2];
      heading = normalizeChunkTitleText(indexedTypeMatch[3]);
    } else {
      const typeMatch = heading.match(/^([a-z][a-z0-9-]*)\s*:\s*(.+)$/i);
      if (typeMatch && isChunkTypeLabel(typeMatch[1], chunk.chunk_type)) {
        typeLabel = typeMatch[1];
        heading = normalizeChunkTitleText(typeMatch[2]);
      }
    }

    const suppressionCandidates = [rawTitle, heading];
    if (typeLabel) suppressionCandidates.push(`${typeLabel}: ${heading}`);
    if (indexLabel) suppressionCandidates.push(`${indexLabel} ${heading}`);
    if (indexLabel && typeLabel) suppressionCandidates.push(`${indexLabel} ${typeLabel}: ${heading}`);

    return {
      heading,
      indexLabel,
      suppressionCandidates: [...new Set(suppressionCandidates.map(normalizeChunkTitleText).filter(Boolean))],
    };
  }

  function isChunkTypeLabel(label: string, chunkType: string): boolean {
    const normalisedLabel = normaliseTypeToken(label);
    const aliases = CHUNK_TITLE_TYPE_ALIASES[chunkType] ?? [chunkType];
    return aliases.some((alias) => {
      const normalisedAlias = normaliseTypeToken(alias);
      return normalisedLabel === normalisedAlias
        || singulariseTypeToken(normalisedLabel) === normalisedAlias
        || normalisedLabel === singulariseTypeToken(normalisedAlias);
    });
  }

  function normaliseTypeToken(value: string): string {
    return value.toLocaleLowerCase().replace(/[^a-z]/g, "");
  }

  function singulariseTypeToken(value: string): string {
    return value.endsWith("s") ? value.slice(0, -1) : value;
  }

  function normalizeChunkTitleText(value: string): string {
    return value.replace(/\s+/g, " ").trim();
  }

  function getChunkDisplayBody(chunk: ChunkInfo): string {
    if (chunkHasFormattedBody(chunk)) return chunk.formatted_body_md ?? '';
    return chunk.ocr_text ?? '';
  }

  function currentRenderedPdfBitmap(): RenderedPdfBitmap | null {
    if (!currentPdfBitmap) return null;
    return {
      bitmap: currentPdfBitmap,
      bitmapWidth: currentPdfBitmap.width,
      bitmapHeight: currentPdfBitmap.height,
      pageWidthPoints: currentPdfPagePoints.w,
      pageHeightPoints: currentPdfPagePoints.h,
    };
  }

  async function rasteriseChunkForTranscription(
    chunk: Pick<ChunkInfo, "id" | "bbox_x" | "bbox_y" | "bbox_w" | "bbox_h">,
    pageNumber = currentPage,
  ): Promise<string> {
    if (!selectedBook) throw new Error("No selected book");
    if (chunk.bbox_w <= 0 || chunk.bbox_h <= 0) {
      throw new Error(`Invalid chunk bounds for chunk ${chunk.id}`);
    }

    const desiredPageWidth = Math.min(
      CHUNK_TRANSCRIPTION_MAX_PAGE_WIDTH,
      Math.max(
        getPreviewPdfPixelWidth(),
        quantizePdfPixelWidth(CHUNK_TRANSCRIPTION_TARGET_WIDTH / Math.max(chunk.bbox_w, 0.001)),
      ),
    );

    let rendered = pageNumber === currentPage ? currentRenderedPdfBitmap() : null;
    if (!rendered || !hasEnoughPdfResolution(rendered.bitmap, desiredPageWidth)) {
      const fetched = await fetchPdfBitmap(pageNumber, desiredPageWidth);
      rendered = fetched.rendered;
    }

    const sx = Math.min(
      rendered.bitmapWidth - 1,
      Math.max(0, Math.floor(chunk.bbox_x * rendered.bitmapWidth)),
    );
    const sy = Math.min(
      rendered.bitmapHeight - 1,
      Math.max(0, Math.floor(chunk.bbox_y * rendered.bitmapHeight)),
    );
    const sw = Math.max(
      1,
      Math.min(
        rendered.bitmapWidth - sx,
        Math.ceil(chunk.bbox_w * rendered.bitmapWidth),
      ),
    );
    const sh = Math.max(
      1,
      Math.min(
        rendered.bitmapHeight - sy,
        Math.ceil(chunk.bbox_h * rendered.bitmapHeight),
      ),
    );

    const offscreen = new OffscreenCanvas(sw, sh);
    const ctx = offscreen.getContext("2d");
    if (!ctx) throw new Error("Failed to create chunk transcription canvas");
    ctx.drawImage(rendered.bitmap, sx, sy, sw, sh, 0, 0, sw, sh);

    return offscreenToBase64(offscreen);
  }

  function applyChunkFormattedBody(chunkId: number, bodyMarkdown: string) {
    currentChunks = currentChunks.map((chunk) =>
      chunk.id === chunkId
        ? { ...chunk, formatted_body_md: bodyMarkdown }
        : chunk,
    );
    const view = chunkView;
    if (view?.chunk.id === chunkId) {
      const updatedChunk = {
        ...view.chunk,
        formatted_body_md: bodyMarkdown,
      };
      chunkView = {
        ...view,
        chunk: updatedChunk,
      };
      const hasPendingManualBodyEdit = chunkBodyPendingChunkId === chunkId || chunkBodySaving;
      if (!hasPendingManualBodyEdit) {
        chunkBodyDraft = getChunkDisplayBody(updatedChunk);
      }
    }
  }

  async function loadChunkForTranscription(chunkId: number): Promise<ChunkForTranscription | null> {
    if (!selectedBook) return null;
    const localOnCurrentPage = currentChunks.find((entry) => entry.id === chunkId);
    if (localOnCurrentPage) {
      return {
        ...localOnCurrentPage,
        source_document_id: selectedBook.id,
        page_number: currentPage,
      };
    }
    if (chunkView?.chunk.id === chunkId) {
      return {
        ...chunkView.chunk,
        source_document_id: selectedBook.id,
        page_number: chunkView.pageNumber,
      };
    }

    const fetched = await invoke<ChunkForTranscription>("get_chunk_for_transcription", { chunkId });
    if (fetched.source_document_id !== selectedBook.id) {
      throw new Error(`Chunk ${chunkId} belongs to a different document`);
    }
    return fetched;
  }

  async function ensureChunkFormattedBody(chunkId: number, force = false): Promise<boolean> {
    const existing = chunkBodyRequests.get(chunkId);
    if (existing) return existing;

    const localChunk = currentChunks.find((entry) => entry.id === chunkId)
      ?? (chunkView?.chunk.id === chunkId ? chunkView.chunk : null);
    if (!force && localChunk && chunkHasFormattedBody(localChunk)) return true;

    const request = (async (): Promise<boolean> => {
      try {
        const chunk = await loadChunkForTranscription(chunkId);
        if (!chunk) return false;
        if (!force && chunkHasFormattedBody(chunk)) {
          applyChunkFormattedBody(chunkId, chunk.formatted_body_md ?? "");
          return true;
        }

        const imageBase64 = await rasteriseChunkForTranscription(chunk, chunk.page_number);
        const visionProvider = aiTaskSettings.vision.provider;
        const visionModel = aiTaskSettings.vision.model.trim();
        const result = await invoke<ChunkFormattedBodyOutput>("generate_chunk_formatted_body", {
          chunkId,
          provider: visionProvider,
          model: visionModel.length > 0 ? visionModel : null,
          imageBase64,
          force,
        });
        applyChunkFormattedBody(chunkId, result.body_markdown);
        await appLogInfo(
          `[chunk] formatted body ready chunkId=${chunkId} provider=${visionProvider} model=${visionModel || "auto"} chars=${result.body_markdown.length}`,
        );
        return true;
      } catch (err) {
        await appLogWarn(
          `[chunk] formatted body failed chunkId=${chunkId} provider=${aiTaskSettings.vision.provider} model=${aiTaskSettings.vision.model.trim() || "auto"}: ${formatLogError(err)}`,
        );
        return false;
      } finally {
        chunkBodyRequests.delete(chunkId);
      }
    })();

    chunkBodyRequests.set(chunkId, request);
    return request;
  }

  async function loadQuestionSourceSlices(chunkId: number) {
    questionSourceLoading = true;
    questionSourceError = null;
    try {
      questionSourceSlices = await invoke<QuestionSourceSlice[]>("get_question_source_slices", { chunkId });
    } catch (err) {
      questionSourceError = formatLogError(err);
      questionSourceSlices = [];
      await appLogWarn(`[question] source slices failed chunkId=${chunkId}: ${questionSourceError}`);
    } finally {
      questionSourceLoading = false;
    }
  }

  async function loadQuestionMarkAttempts(chunkId: number) {
    try {
      questionMarkAttempts = await invoke<QuestionMarkAttemptView[]>("list_question_mark_attempts", { chunkId });
    } catch (err) {
      questionMarkAttempts = [];
      await appLogWarn(`[question] mark attempts failed chunkId=${chunkId}: ${formatLogError(err)}`);
    }
  }

  function beginQuestionAvailableMarksEdit() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question" || questionAvailableMarksSaving) return;
    questionAvailableMarksEditing = true;
    questionAvailableMarksError = null;
    questionAvailableMarksDraft = view.chunk.available_marks == null
      ? ""
      : `${view.chunk.available_marks}`;
    void tick().then(() => {
      questionAvailableMarksInputEl?.focus();
      questionAvailableMarksInputEl?.select();
    });
  }

  function cancelQuestionAvailableMarksEdit() {
    const view = chunkView;
    if (view?.chunk.chunk_type === "question") {
      questionAvailableMarksDraft = view.chunk.available_marks == null
        ? ""
        : `${view.chunk.available_marks}`;
    } else {
      questionAvailableMarksDraft = "";
    }
    questionAvailableMarksEditing = false;
    questionAvailableMarksError = null;
  }

  async function saveQuestionAvailableMarks() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question" || questionAvailableMarksSaving) return;

    const draft = questionAvailableMarksDraft.trim();
    const parsed = draft.length === 0 ? null : Number(draft);
    if (parsed != null && (!Number.isFinite(parsed) || !Number.isInteger(parsed) || parsed < 0)) {
      questionAvailableMarksError = "Enter a non-negative whole number.";
      return;
    }

    questionAvailableMarksSaving = true;
    questionAvailableMarksError = null;
    try {
      await invoke("save_question_available_marks", {
        chunkId: view.chunk.id,
        availableMarks: parsed,
      });
      currentChunks = currentChunks.map((chunk) =>
        chunk.id === view.chunk.id
          ? { ...chunk, available_marks: parsed }
          : chunk,
      );
      if (chunkView?.chunk.id === view.chunk.id) {
        chunkView = {
          ...chunkView,
          chunk: { ...chunkView.chunk, available_marks: parsed },
        };
      }
      questionAvailableMarksDraft = parsed == null ? "" : `${parsed}`;
      questionAvailableMarksEditing = false;
    } catch (err) {
      questionAvailableMarksError = formatLogError(err);
      await appLogWarn(`[question] save available marks failed chunkId=${view.chunk.id}: ${questionAvailableMarksError}`);
    } finally {
      questionAvailableMarksSaving = false;
    }
  }

  async function saveQuestionAchievedMarks() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question" || questionAchievedMarksSaving) return;

    const draft = questionAchievedMarksDraft.trim();
    const parsed = draft.length === 0 ? null : Number(draft);
    if (parsed != null && (!Number.isFinite(parsed) || parsed < 0)) {
      questionAchievedMarksError = "Enter a non-negative number.";
      return;
    }

    questionAchievedMarksSaving = true;
    questionAchievedMarksError = null;
    try {
      await invoke("save_question_achieved_marks", {
        chunkId: view.chunk.id,
        achievedMarks: parsed,
      });
      currentChunks = currentChunks.map((chunk) =>
        chunk.id === view.chunk.id
          ? { ...chunk, achieved_marks: parsed }
          : chunk,
      );
      if (chunkView?.chunk.id === view.chunk.id) {
        chunkView = {
          ...chunkView,
          chunk: { ...chunkView.chunk, achieved_marks: parsed },
        };
      }
      questionAchievedMarksDraft = parsed == null ? "" : formatQuestionMarksValue(parsed);
      await loadQuestionMarkAttempts(view.chunk.id);
    } catch (err) {
      questionAchievedMarksError = formatLogError(err);
      await appLogWarn(`[question] save achieved marks failed chunkId=${view.chunk.id}: ${questionAchievedMarksError}`);
    } finally {
      questionAchievedMarksSaving = false;
    }
  }

  async function openQuestionSource() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question") return;
    if (questionSourceSlices.length === 0) {
      if (!questionSourceLoading) await loadQuestionSourceSlices(view.chunk.id);
      if (questionSourceSlices.length === 0) return;
    }
    const target = questionSourceSlices[0];
    await goToPage(target.page_number);
  }

  async function runQuestionMarking() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question" || questionMarkingLoading || questionMarkingApplying) return;

    questionMarkingLoading = true;
    questionMarkingError = null;
    questionMarkingSuggestion = null;
    try {
      await flushGlossarySave();
      const chatProvider = aiTaskSettings.chat.provider;
      const chatModel = aiTaskSettings.chat.model.trim();
      const visualContextImages = includeChunkAiVisuals
        ? (await rasteriseChunkVisualPages(view.chunk.id)).map(p => p.imageBase64)
        : [];
      if (chunkView?.chunk.id !== view.chunk.id) return;
      const suggestion = await invoke<QuestionMarkSuggestion>("mark_question_answer_with_ai", {
        chunkId: view.chunk.id,
        provider: chatProvider,
        model: chatModel.length > 0 ? chatModel : null,
        includeVisuals: includeChunkAiVisuals,
        visualContextImages,
      });
      questionMarkingSuggestion = suggestion;
      questionMarkingSuggestionIncludeVisuals = includeChunkAiVisuals;
    } catch (err) {
      questionMarkingError = formatLogError(err);
      await appLogWarn(`[question] AI marking failed chunkId=${view.chunk.id}: ${questionMarkingError}`);
    } finally {
      questionMarkingLoading = false;
    }
  }

  async function applyQuestionMarkingSuggestion() {
    const view = chunkView;
    if (!view || view.chunk.chunk_type !== "question" || !questionMarkingSuggestion || questionMarkingApplying) return;

    questionMarkingApplying = true;
    questionMarkingError = null;
    try {
      const chatProvider = aiTaskSettings.chat.provider;
      const chatModel = aiTaskSettings.chat.model.trim();
      await invoke("apply_question_mark_attempt", {
        chunkId: view.chunk.id,
        source: "ai",
        achievedMarks: questionMarkingSuggestion.achieved_marks,
        feedbackMd: questionMarkingSuggestion.feedback_md,
        includeVisuals: questionMarkingSuggestionIncludeVisuals,
        provider: chatProvider,
        model: chatModel.length > 0 ? chatModel : null,
      });

      const achieved = questionMarkingSuggestion.achieved_marks;
      currentChunks = currentChunks.map((chunk) =>
        chunk.id === view.chunk.id
          ? { ...chunk, achieved_marks: achieved }
          : chunk,
      );
      if (chunkView?.chunk.id === view.chunk.id) {
        chunkView = {
          ...chunkView,
          chunk: { ...chunkView.chunk, achieved_marks: achieved },
        };
      }
      questionAchievedMarksDraft = achieved == null ? "" : formatQuestionMarksValue(achieved);
      await loadQuestionMarkAttempts(view.chunk.id);
      questionMarkingSuggestion = null;
      questionMarkingSuggestionIncludeVisuals = false;
    } catch (err) {
      questionMarkingError = formatLogError(err);
      await appLogWarn(`[question] apply AI marking failed chunkId=${view.chunk.id}: ${questionMarkingError}`);
    } finally {
      questionMarkingApplying = false;
    }
  }

  async function openChunkView(
    chunk: ChunkInfo,
    options: { pageNumber?: number; pageChunks?: ChunkInfo[] } = {},
  ) {
    const pageNumber = options.pageNumber ?? currentPage;
    const pageChunks = options.pageChunks ?? (pageNumber === currentPage ? currentChunks : chunkNavigationPageChunks);
    viewerAiOpen = false;
    viewerAiContextPageId = null;
    clearPendingChatAttachment();
    void cancelChunkChatForReset();
    await flushGlossarySave();
    await flushChunkTextSaves();
    stopChunkPanelResize();
    if (chunkView) {
      snapshotChunkViewToCache(chunkView);
    }
    const cache = await ensureChunkSurface(chunk.id);
    void appLogInfo(
      `[chunk] open chunkId=${chunk.id} type=${chunk.chunk_type} cachedStrokes=${cache.strokes.length}`,
    );
    chunkMode = 'draw';
    chunkTab = 'ink';
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
    showPaperOptions = false;
    chunkCamera = { x: 0, y: 0, scale: 1 };
    cachedChunkRect = null;
    if (chunkWetCanvas) {
      // Canvas is already mounted (chunk-to-chunk navigation). Use actual layout dimensions
      // so coordinate math is correct immediately — avoids strokes being recorded at wrong
      // normalised positions while chunkSurfaceSize = {w:1,h:1}.
      const sw = Math.max(1, chunkWetCanvas.offsetWidth);
      const sh = Math.max(1, chunkWetCanvas.offsetHeight);
      chunkSurfaceSize = { w: sw, h: sh };
      chunkHomeViewSize = { w: sw, h: sh };
    } else {
      chunkSurfaceSize = { w: 1, h: 1 };
      chunkHomeViewSize = { w: 0, h: 0 };
    }
    chunkTouchPointers = [];
    chunkLastPinchDist = 0;
    chunkLastPinchMid = { x: 0, y: 0 };
    chunkShapeDraft = null;
    clearChunkSelectionState();
    chunkGraphUndoStack = [];
    chunkGraphRedoStack = [];
    clearChunkStrokeTransformHistory();
    chunkView = {
      chunk,
      pageNumber,
      surfaceId: cache.surfaceId,
      strokes: cloneStrokes(cache.strokes),
      graphs: cloneGraphObjects(cache.graphs),
    };
    chunkNavigationPageChunks = pageChunks;
    chunkTitleDraft = chunk.title ?? "";
    chunkBodyDraft = getChunkDisplayBody(chunk);
    chunkTitleEditing = false;
    chunkBodyEditing = false;
    chunkTitleSaveError = null;
    chunkBodySaveError = null;
    chunkTitlePendingChunkId = null;
    chunkBodyPendingChunkId = null;
    if (chunkTitleSaveTimer) {
      clearTimeout(chunkTitleSaveTimer);
      chunkTitleSaveTimer = null;
    }
    if (chunkBodySaveTimer) {
      clearTimeout(chunkBodySaveTimer);
      chunkBodySaveTimer = null;
    }
    glossaryDraft = chunk.glossary_md ?? "";
    glossaryFormat = normaliseGlossaryFormat(chunk.glossary_format);
    glossaryMode = 'edit';
    glossarySaveError = null;
    glossaryFormatNotice = null;
    glossaryPdfExporting = false;
    clearGlossaryPdfExportNotices();
    glossaryPendingChunkId = null;
    if (glossarySaveTimer) {
      clearTimeout(glossarySaveTimer);
      glossarySaveTimer = null;
    }
    resetGlossaryTypstPreviewState();
    includeChunkAiVisuals = false;
    clearChunkInkContextCache();
    chunkAiRewriteTab = 'body';
    chunkRewritePrompt = "";
    chunkRewriteError = null;
    chunkRewriteSuggestion = null;
    chunkRewriteLoading = false;
    chunkRewriteApplying = false;
    glossaryRewritePrompt = "";
    glossaryRewriteError = null;
    glossaryRewriteSuggestion = null;
    glossaryRewriteLoading = false;
    glossaryRewriteApplying = false;
    resetGlossaryRewriteTypstPreviewState();
    questionAvailableMarksDraft = chunk.available_marks == null
      ? ""
      : `${chunk.available_marks}`;
    questionAvailableMarksEditing = false;
    questionAvailableMarksSaving = false;
    questionAvailableMarksError = null;
    questionAchievedMarksDraft = chunk.achieved_marks == null
      ? ""
      : formatQuestionMarksValue(chunk.achieved_marks);
    questionAchievedMarksSaving = false;
    questionAchievedMarksError = null;
    questionMarkingLoading = false;
    questionMarkingApplying = false;
    questionMarkingError = null;
    questionMarkingSuggestion = null;
    questionMarkingSuggestionIncludeVisuals = false;
    questionSourceSlices = [];
    questionSourceError = null;
    questionSourceLoading = false;
    questionMarkAttempts = [];
    if (chunk.chunk_type === "question") {
      void loadQuestionSourceSlices(chunk.id);
      void loadQuestionMarkAttempts(chunk.id);
    }
    redoStack = [];
    markDirty();
    redrawChunkCanvases();
    if (!chunkHasFormattedBody(chunk)) {
      void ensureChunkFormattedBody(chunk.id);
    }
    if (selectedBook) {
      prefetchPage(selectedBook.id, pageNumber + 1);
      prefetchPage(selectedBook.id, pageNumber - 1);
    }
  }

  function closeChunkView() {
    void cancelChunkChatForReset();
    void flushGlossarySave();
    void flushChunkTextSaves();
    stopChunkPanelResize();
    if (chunkView) {
      void appLogInfo(
        `[chunk] close chunkId=${chunkView.chunk.id} strokes=${chunkView.strokes.length}`,
      );
      snapshotChunkViewToCache(chunkView);
    }
    chunkView = null;
    clearChunkStrokeTransformHistory();
    chunkNavigationPageChunks = [];
    chunkMode = 'draw';
    showChunkPenOptions = false;
    showChunkShapeOptions = false;
    showPaperOptions = false;
    chunkWetCtx = null;
    chunkDryCtx = null;
    chunkWetCanvas = null!;
    chunkDryCanvas = null!;
    cachedChunkRect = null;
    chunkIsDrawing = false;
    chunkCurrentStroke = [];
    chunkShapeDraft = null;
    chunkActivePointerId = null;
    chunkCamera = { x: 0, y: 0, scale: 1 };
    chunkSurfaceSize = { w: 1, h: 1 };
    chunkHomeViewSize = { w: 0, h: 0 };
    chunkTouchPointers = [];
    chunkLastPinchDist = 0;
    chunkLastPinchMid = { x: 0, y: 0 };
    clearChunkSelectionState();
    chunkGraphUndoStack = [];
    chunkGraphRedoStack = [];
    if (pendingGraphPlacement?.scope === "chunk") {
      pendingGraphPlacement = null;
    }
    if (graphModalOpen && (graphModalSource === "chunk" || graphModalSource === "glossary")) {
      graphModalOpen = false;
    }
    redoStack = [];
    chunkTitleEditing = false;
    chunkBodyEditing = false;
    includeChunkAiVisuals = false;
    clearChunkInkContextCache();
    chunkAiRewriteTab = 'body';
    chunkRewritePrompt = "";
    chunkRewriteError = null;
    chunkRewriteSuggestion = null;
    chunkRewriteLoading = false;
    chunkRewriteApplying = false;
    glossaryFormat = "markdown";
    glossaryFormatNotice = null;
    glossaryPdfExporting = false;
    clearGlossaryPdfExportNotices();
    resetGlossaryTypstPreviewState();
    glossaryRewritePrompt = "";
    glossaryRewriteError = null;
    glossaryRewriteSuggestion = null;
    glossaryRewriteLoading = false;
    glossaryRewriteApplying = false;
    resetGlossaryRewriteTypstPreviewState();
    questionAvailableMarksDraft = "";
    questionAvailableMarksEditing = false;
    questionAvailableMarksSaving = false;
    questionAvailableMarksError = null;
    questionAchievedMarksDraft = "";
    questionAchievedMarksSaving = false;
    questionAchievedMarksError = null;
    questionMarkingLoading = false;
    questionMarkingApplying = false;
    questionMarkingError = null;
    questionMarkingSuggestion = null;
    questionMarkingSuggestionIncludeVisuals = false;
    questionSourceSlices = [];
    questionSourceLoading = false;
    questionSourceError = null;
    questionMarkAttempts = [];
    markDirty();
  }


  // â”€â”€ Chunk view canvases â”€â”€
  interface ChunkCamera { x: number; y: number; scale: number }
  const CHUNK_ZOOM_MIN = 0.35;
  const CHUNK_ZOOM_MAX = 8.0;
  const CHUNK_ERASE_RADIUS_WORLD = 8;
  let chunkCamera = $state<ChunkCamera>({ x: 0, y: 0, scale: 1 });
  let chunkSurfaceSize = { w: 1, h: 1 };
  let chunkHomeViewSize = { w: 0, h: 0 };
  let chunkZoomPercent = $derived(Math.round(chunkCamera.scale * 100));
  let chunkGridStyle = $derived.by(() => {
    const step = paperSettings.spacing * chunkCamera.scale;
    const dotOffset = step * 0.5;
    return [
      `--paper-background-colour: ${paperSettings.backgroundColour}`,
      `--paper-pattern-colour: ${paperPatternColour(paperPatternAlpha())}`,
      `--paper-grid-size: ${step}px`,
      `--paper-grid-offset-x: ${chunkCamera.x}px`,
      `--paper-grid-offset-y: ${chunkCamera.y}px`,
      `--paper-dot-offset-x: ${chunkCamera.x - dotOffset}px`,
      `--paper-dot-offset-y: ${chunkCamera.y - dotOffset}px`,
    ].join("; ");
  });

  let chunkWetCanvas = $state<HTMLCanvasElement>(null!);
  let chunkDryCanvas = $state<HTMLCanvasElement>(null!);
  let chunkWetCtx: CanvasRenderingContext2D | null = null;
  let chunkDryCtx: CanvasRenderingContext2D | null = null;

  // Chunk drawing state (separate from page strokes)
  let chunkIsDrawing = false;
  let chunkCurrentStroke: Point[] = [];
  let chunkShapeDraft = $state<ShapeDraft | null>(null);
  let chunkActivePointerId: number | null = null;
  let chunkTouchPointers: TouchPointer[] = [];
  let chunkLastPinchDist = 0;
  let chunkLastPinchMid = { x: 0, y: 0 };

  function getChunkPageAspect(): number {
    const baseW = chunkHomeViewSize.w > 0 ? chunkHomeViewSize.w : chunkSurfaceSize.w;
    const baseH = chunkHomeViewSize.h > 0 ? chunkHomeViewSize.h : chunkSurfaceSize.h;
    if (baseW <= 0 || baseH <= 0) return 1;
    return baseH / baseW;
  }

  function getChunkPageWorldSize(): { w: number; h: number } {
    const w = Math.max(1, chunkSurfaceSize.w);
    return {
      w,
      h: Math.max(1, w * getChunkPageAspect()),
    };
  }

  function chunkStrokeWorldBounds(stroke: Stroke) {
    const { w, h } = getChunkPageWorldSize();
    return {
      minX: stroke.bbox.minX * w,
      minY: stroke.bbox.minY * h,
      maxX: stroke.bbox.maxX * w,
      maxY: stroke.bbox.maxY * h,
    };
  }

  function chunkGraphWorldBounds(graph: SurfaceGraphObject) {
    const { w, h } = getChunkPageWorldSize();
    return {
      minX: graph.bboxX * w,
      minY: graph.bboxY * h,
      maxX: (graph.bboxX + graph.bboxW) * w,
      maxY: (graph.bboxY + graph.bboxH) * h,
      width: graph.bboxW * w,
      height: graph.bboxH * h,
    };
  }

  function getChunkDocumentPageCount(): number {
    let maxPage = 1;
    if (chunkView) {
      for (const stroke of chunkView.strokes) {
        if (!strokeHasFiniteBounds(stroke)) continue;
        maxPage = Math.max(maxPage, Math.ceil(Math.max(0, stroke.bbox.maxY)));
      }
      for (const graph of chunkView.graphs) {
        if (!graphHasFiniteChunkBounds(graph)) continue;
        maxPage = Math.max(maxPage, Math.ceil(Math.max(0, graph.bboxY + graph.bboxH)));
      }
    }
    return maxPage + INK_DOCUMENT_BUFFER_PAGES;
  }

  function getChunkDocumentWorldHeight(): number {
    return getChunkPageWorldSize().h * getChunkDocumentPageCount();
  }

  function clampChunkCamera() {
    const scale = Math.max(CHUNK_ZOOM_MIN, Math.min(CHUNK_ZOOM_MAX, chunkCamera.scale || 1));
    const { w: docWidth } = getChunkPageWorldSize();
    const docHeight = getChunkDocumentWorldHeight();
    const viewportWorldW = chunkSurfaceSize.w / scale;
    const viewportWorldH = chunkSurfaceSize.h / scale;

    let x: number;
    if (viewportWorldW >= docWidth) {
      x = (chunkSurfaceSize.w - docWidth * scale) / 2;
    } else {
      const currentLeft = -chunkCamera.x / scale;
      const maxLeft = docWidth - viewportWorldW;
      const left = Math.max(0, Math.min(maxLeft, currentLeft));
      x = -left * scale;
    }

    let y: number;
    if (viewportWorldH >= docHeight) {
      y = 0;
    } else {
      const currentTop = -chunkCamera.y / scale;
      const maxTop = docHeight - viewportWorldH;
      const top = Math.max(0, Math.min(maxTop, currentTop));
      y = -top * scale;
    }

    chunkCamera = { x, y, scale };
  }

  function chunkPointToWorld(point: Pick<Point, "x" | "y">): { x: number; y: number } {
    const { w, h } = getChunkPageWorldSize();
    return {
      x: point.x * w,
      y: point.y * h,
    };
  }

  function chunkWorldToPoint(worldX: number, worldY: number, pressure = 0.5): Point {
    const { w, h } = getChunkPageWorldSize();
    return {
      x: clampNorm(worldX / w),
      y: Math.max(0, worldY / h),
      pressure,
    };
  }

  function getChunkShapeStrokes(draft: ShapeDraft): Point[][] {
    const { w, h } = getChunkPageWorldSize();
    return buildShapeStrokes(
      shapeKind,
      draft.origin,
      draft.current,
      w,
      h,
      0.5,
    );
  }

  async function commitChunkShape(draft: ShapeDraft) {
    const view = chunkView;
    if (!view) return;
    const { w, h } = getChunkPageWorldSize();
    const dragWorld = shapeDragLengthWorld(draft.origin, draft.current, w, h);
    if (dragWorld < MIN_SHAPE_DRAG_WORLD) return;
    const generated = getChunkShapeStrokes(draft);
    if (generated.length === 0) return;
    const groupId = generated.length > 1 ? makeStrokeGroupId() : null;
    const newStrokes: Stroke[] = generated
      .filter((pts) => pts.length >= 2)
      .map((pts) => ({
        id: null,
        clientId: makeStrokeClientId(),
        colour: penColour,
        thickness: penThickness,
        points: pts,
        bbox: computeBBox(pts),
        chunkId: view.chunk.id,
        groupId,
      }));
    if (newStrokes.length === 0) return;

    view.strokes = [...view.strokes, ...newStrokes];
    clearChunkInkContextCache();
    clearChunkStrokeTransformHistory();
    redrawChunkDry();

    for (const stroke of newStrokes) {
      await persistChunkStrokeCreate(view.surfaceId, stroke);
    }
  }

  function commitChunkStrokePoints(pts: Point[]) {
    const view = chunkView;
    if (!view || pts.length < 2) return;

    const stroke: Stroke = {
      id: null,
      clientId: makeStrokeClientId(),
      colour: penColour,
      thickness: penThickness,
      points: pts,
      bbox: computeBBox(pts),
      chunkId: view.chunk.id,
      groupId: null,
    };
    view.strokes = [...view.strokes, stroke];
    clearChunkInkContextCache();
    clearChunkStrokeTransformHistory();
    redrawChunkDry();

    void trackInkWrite(persistChunkStrokeCreate(view.surfaceId, stroke));
  }

  function updateChunkSurfaceCacheGraphs(next: SurfaceGraphObject[]) {
    if (!chunkView) return;
    const cached = chunkSurfaceCache.get(chunkView.chunk.id);
    if (!cached) return;
    cached.graphs = cloneGraphObjects(next);
  }

  function recordChunkGraphHistory(
    before: SurfaceGraphObject[],
    after: SurfaceGraphObject[],
  ) {
    if (!chunkView) return;
    if (graphArraysEquivalent(before, after)) return;
    chunkGraphUndoStack = [
      ...chunkGraphUndoStack,
      {
        surfaceId: chunkView.surfaceId,
        before: cloneGraphObjects(before),
        after: cloneGraphObjects(after),
      },
    ];
    chunkGraphRedoStack = [];
  }

  async function applyChunkGraphChange(
    target: SurfaceGraphObject[],
    options: { recordHistory?: boolean; beforeSnapshot?: SurfaceGraphObject[] } = {},
  ): Promise<SurfaceGraphObject[] | null> {
    if (!chunkView) return null;
    const before = options.beforeSnapshot ? cloneGraphObjects(options.beforeSnapshot) : cloneGraphObjects(chunkView.graphs);
    const reconciled = await reconcileSurfaceGraphs(chunkView.surfaceId, chunkView.graphs, target);
    chunkView.graphs = reconciled;
    updateChunkSurfaceCacheGraphs(reconciled);
    if (options.recordHistory) {
      recordChunkGraphHistory(before, reconciled);
    }
    clearChunkInkContextCache();
    redrawChunkDry();
    return reconciled;
  }

  function hitTestChunkGraph(
    point: { x: number; y: number },
  ): { graphId: number; mode: "move" | "resize" } | null {
    if (!chunkView || chunkView.graphs.length === 0) return null;
    const { w, h } = getChunkPageWorldSize();
    const handleNormX = w > 0 ? (12 / chunkCamera.scale) / w : 0.03;
    const handleNormY = h > 0 ? (12 / chunkCamera.scale) / h : 0.03;

    for (let index = chunkView.graphs.length - 1; index >= 0; index -= 1) {
      const graph = chunkView.graphs[index];
      if (graph.id == null) continue;
      const x2 = graph.bboxX + graph.bboxW;
      const y2 = graph.bboxY + graph.bboxH;
      if (point.x < graph.bboxX || point.x > x2 || point.y < graph.bboxY || point.y > y2) continue;
      const nearHandle = Math.abs(point.x - x2) <= handleNormX && Math.abs(point.y - y2) <= handleNormY;
      return { graphId: graph.id, mode: nearHandle ? "resize" : "move" };
    }
    return null;
  }

  function updateChunkGraphInState(graphId: number, next: SurfaceGraphObject) {
    if (!chunkView) return;
    chunkView.graphs = chunkView.graphs.map((graph) => (graph.id === graphId ? next : graph));
    updateChunkSurfaceCacheGraphs(chunkView.graphs);
    clearChunkInkContextCache();
    redrawChunkDry();
  }

  async function placePendingChunkGraphAt(point: { x: number; y: number }): Promise<boolean> {
    if (!chunkView) return false;
    if (!pendingGraphPlacement || pendingGraphPlacement.scope !== "chunk") return false;
    const rect = chunkGraphRectFromCenter(point, CHUNK_GRAPH_DEFAULT_SIZE);
    const graph = buildGraphObjectFromSpec(pendingGraphPlacement.spec, rect);
    const before = cloneGraphObjects(chunkView.graphs);
    const target = [...before, graph];
    try {
      const applied = await applyChunkGraphChange(target, {
        recordHistory: true,
        beforeSnapshot: before,
      });
      if (applied && applied.length > 0) {
        chunkSelectedGraphId = applied[applied.length - 1].id;
      }
      chunkSelectedStrokes = new Set();
      chunkSelection = null;
    } catch (err) {
      console.warn("placePendingChunkGraphAt failed", err);
    } finally {
      pendingGraphPlacement = null;
    }
    return true;
  }

  async function deleteSelectedChunkGraph() {
    if (!chunkView || chunkSelectedGraphId == null) return;
    const before = cloneGraphObjects(chunkView.graphs);
    const target = chunkView.graphs.filter((graph) => graph.id !== chunkSelectedGraphId);
    await applyChunkGraphChange(target, {
      recordHistory: true,
      beforeSnapshot: before,
    });
    chunkSelectedGraphId = null;
  }

  let cachedChunkRect: DOMRect | null = null;

  function chunkClientToScreen(clientX: number, clientY: number): { x: number; y: number } {
    const rect = cachedChunkRect ??= chunkWetCanvas.getBoundingClientRect();
    return {
      x: clientX - rect.left,
      y: clientY - rect.top,
    };
  }

  function chunkScreenToWorld(screenX: number, screenY: number): { x: number; y: number } {
    return {
      x: (screenX - chunkCamera.x) / chunkCamera.scale,
      y: (screenY - chunkCamera.y) / chunkCamera.scale,
    };
  }

  function pointerToChunkWorld(clientX: number, clientY: number): { x: number; y: number } {
    const { x, y } = chunkClientToScreen(clientX, clientY);
    return chunkScreenToWorld(x, y);
  }

  function drawChunkStrokePoints(
    ctx: CanvasRenderingContext2D,
    pts: Point[],
    colour: string,
    thickness: number,
  ) {
    const { w, h } = getChunkPageWorldSize();
    drawStrokePointsFn(
      ctx, pts, colour, thickness, 0,
      chunkCamera.scale, 0, 0, w, h,
    );
  }

  function redrawChunkCanvases() {
    redrawChunkDry();
    drawChunkWet();
  }

  function zoomChunkAt(screenX: number, screenY: number, factor: number) {
    const newScale = Math.max(CHUNK_ZOOM_MIN, Math.min(CHUNK_ZOOM_MAX, chunkCamera.scale * factor));
    const wx = (screenX - chunkCamera.x) / chunkCamera.scale;
    const wy = (screenY - chunkCamera.y) / chunkCamera.scale;
    chunkCamera.x = screenX - wx * newScale;
    chunkCamera.y = screenY - wy * newScale;
    chunkCamera.scale = newScale;
    clampChunkCamera();
    redrawChunkCanvases();
  }

  function zoomChunkAtViewportCenter(factor: number) {
    zoomChunkAt(chunkSurfaceSize.w / 2, chunkSurfaceSize.h / 2, factor);
  }

  function hitTestChunkStrokes(rect: Rect): Set<Stroke> {
    if (!chunkView) return new Set();
    const r2 = rect.x + rect.w;
    const b2 = rect.y + rect.h;
    const hit = new Set<Stroke>();
    for (const stroke of chunkView.strokes) {
      if (
        stroke.bbox.maxX < rect.x || stroke.bbox.minX > r2 ||
        stroke.bbox.maxY < rect.y || stroke.bbox.minY > b2
      ) continue;
      for (const p of stroke.points) {
        if (p.x >= rect.x && p.x <= r2 && p.y >= rect.y && p.y <= b2) {
          hit.add(stroke);
          break;
        }
      }
    }
    return hit;
  }

  function drawChunkPageGuides(
    ctx: CanvasRenderingContext2D,
    visMinY: number,
    visMaxY: number,
  ) {
    const { w: pageWidth, h: pageHeight } = getChunkPageWorldSize();
    const pageCount = getChunkDocumentPageCount();
    drawInkPageGuides(ctx, {
      pageWidth,
      pageHeight,
      pageCount,
      scale: chunkCamera.scale,
      visMinY,
      visMaxY,
    });
  }

  function restoreChunkHomeView() {
    chunkCamera = {
      x: 0,
      y: 0,
      scale: 1,
    };
    clampChunkCamera();
    redrawChunkCanvases();
  }

  function setupChunkCanvases(node: HTMLDivElement) {
    const resize = () => {
      if (!chunkWetCanvas || !chunkDryCanvas) return;
      cachedChunkRect = null;
      const dpr = window.devicePixelRatio || 1;
      const w = Math.max(1, node.clientWidth);
      const h = Math.max(1, node.clientHeight);
      chunkSurfaceSize = { w, h };
      if (chunkHomeViewSize.w <= 0 || chunkHomeViewSize.h <= 0) {
        chunkHomeViewSize = { w, h };
      }
      for (const c of [chunkDryCanvas, chunkWetCanvas]) {
        c.width = Math.round(w * dpr);
        c.height = Math.round(h * dpr);
      }
      chunkDryCtx = chunkDryCanvas.getContext("2d");
      chunkWetCtx = chunkWetCanvas.getContext("2d");
      if (chunkDryCtx) { chunkDryCtx.lineCap = "round"; chunkDryCtx.lineJoin = "round"; }
      if (chunkWetCtx) { chunkWetCtx.lineCap = "round"; chunkWetCtx.lineJoin = "round"; }
      clampChunkCamera();
      redrawChunkCanvases();
    };
    resize();
    queueMicrotask(resize);
    const ro = new ResizeObserver(resize);
    ro.observe(node);
    return { destroy() { ro.disconnect(); } };
  }

  function redrawChunkDry() {
    if (!chunkDryCtx || !chunkDryCanvas || !chunkView) return;
    clampChunkCamera();
    const ctx = chunkDryCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(
      chunkCamera.scale * dpr,
      0,
      0,
      chunkCamera.scale * dpr,
      chunkCamera.x * dpr,
      chunkCamera.y * dpr,
    );
    ctx.clearRect(
      -chunkCamera.x / chunkCamera.scale,
      -chunkCamera.y / chunkCamera.scale,
      chunkDryCanvas.width / (chunkCamera.scale * dpr),
      chunkDryCanvas.height / (chunkCamera.scale * dpr),
    );

    const visMinX = -chunkCamera.x / chunkCamera.scale;
    const visMinY = -chunkCamera.y / chunkCamera.scale;
    const visMaxX = visMinX + chunkSurfaceSize.w / chunkCamera.scale;
    const visMaxY = visMinY + chunkSurfaceSize.h / chunkCamera.scale;
    drawChunkPageGuides(ctx, visMinY, visMaxY);
    for (const stroke of chunkView.strokes) {
      if (stroke.points.length < 2) continue;
      const { minX, minY, maxX, maxY } = chunkStrokeWorldBounds(stroke);
      if (
        maxX < visMinX || minX > visMaxX ||
        maxY < visMinY || minY > visMaxY
      ) continue;
      drawChunkStrokePoints(ctx, stroke.points, stroke.colour, stroke.thickness);
    }

    for (const graph of chunkView.graphs) {
      const { minX, minY, maxX, maxY, width, height } = chunkGraphWorldBounds(graph);
      if (
        maxX < visMinX || minX > visMaxX ||
        maxY < visMinY || minY > visMaxY
      ) continue;

      drawGraphCard(
        ctx,
        graphObjectToSpec(graph),
        minX,
        minY,
        width,
        height,
        {
          selected: chunkSelectedGraphId != null && graph.id === chunkSelectedGraphId,
          showResizeHandle: chunkMode === "select" && chunkSelectedGraphId != null && graph.id === chunkSelectedGraphId,
        },
      );
    }

    if (chunkSelectedStrokes.size > 0) {
      for (const stroke of chunkSelectedStrokes) {
        if (stroke.points.length < 2) continue;
        drawChunkStrokePoints(ctx, stroke.points, "rgba(255, 140, 0, 0.92)", stroke.thickness);
      }
    }
  }

  function drawChunkWet() {
    if (!chunkWetCtx || !chunkWetCanvas) return;
    const ctx = chunkWetCtx;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(
      chunkCamera.scale * dpr,
      0,
      0,
      chunkCamera.scale * dpr,
      chunkCamera.x * dpr,
      chunkCamera.y * dpr,
    );
    ctx.clearRect(
      -chunkCamera.x / chunkCamera.scale,
      -chunkCamera.y / chunkCamera.scale,
      chunkWetCanvas.width / (chunkCamera.scale * dpr),
      chunkWetCanvas.height / (chunkCamera.scale * dpr),
    );
    if (chunkMode === "select") {
      const activeRect = chunkSelectRect
        ?? (chunkSelection
          ? {
            x: chunkSelection.x,
            y: chunkSelection.y,
            w: chunkSelection.width,
            h: chunkSelection.height,
          }
          : null);
      if (!activeRect) return;
      const tl = chunkPointToWorld({ x: activeRect.x, y: activeRect.y });
      const br = chunkPointToWorld({ x: activeRect.x + activeRect.w, y: activeRect.y + activeRect.h });
      ctx.save();
      ctx.strokeStyle = "rgba(57, 108, 216, 0.9)";
      ctx.lineWidth = 1.5 / chunkCamera.scale;
      ctx.setLineDash([5 / chunkCamera.scale, 4 / chunkCamera.scale]);
      ctx.strokeRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.fillStyle = "rgba(57, 108, 216, 0.08)";
      ctx.fillRect(tl.x, tl.y, br.x - tl.x, br.y - tl.y);
      ctx.restore();
      return;
    }
    if (chunkMode === "shape" && chunkShapeDraft) {
      const previewStrokes = getChunkShapeStrokes(chunkShapeDraft);
      for (const previewStroke of previewStrokes) {
        if (previewStroke.length < 2) continue;
        drawChunkStrokePoints(ctx, previewStroke, penColour, penThickness);
      }
      return;
    }
    if (!chunkIsDrawing || chunkCurrentStroke.length < 2) return;
    drawChunkStrokePoints(ctx, chunkCurrentStroke, penColour, penThickness);
  }

  function onChunkPointerDown(e: PointerEvent) {
    if (!chunkView) return;
    if (e.pointerType === "touch") {
      if (pendingGraphPlacement?.scope === "chunk") {
        const { x, y } = pointerToChunkWorld(e.clientX, e.clientY);
        const point = chunkWorldToPoint(x, y);
        void placePendingChunkGraphAt({ x: point.x, y: point.y });
        e.preventDefault();
        return;
      }
      const local = chunkClientToScreen(e.clientX, e.clientY);
      chunkWetCanvas.setPointerCapture(e.pointerId);
      chunkTouchPointers = chunkTouchPointers.filter((p) => p.id !== e.pointerId);
      chunkTouchPointers = [...chunkTouchPointers, { id: e.pointerId, x: local.x, y: local.y }];
      if (chunkTouchPointers.length === 2) {
        chunkLastPinchDist = touchDist(chunkTouchPointers[0], chunkTouchPointers[1]);
        chunkLastPinchMid = touchMid(chunkTouchPointers[0], chunkTouchPointers[1]);
      } else if (chunkTouchPointers.length === 1) {
        chunkLastPinchMid = { x: local.x, y: local.y };
      }
      e.preventDefault();
      return;
    }

    if (e.pointerType !== "pen" && e.pointerType !== "mouse") return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (chunkActivePointerId !== null) return;

    const { x, y } = pointerToChunkWorld(e.clientX, e.clientY);
    const point = chunkWorldToPoint(x, y);
    if (pendingGraphPlacement?.scope === "chunk") {
      void placePendingChunkGraphAt({ x: point.x, y: point.y });
      e.preventDefault();
      return;
    }

    chunkActivePointerId = e.pointerId;
    chunkWetCanvas.setPointerCapture(e.pointerId);

    if (chunkMode === "select") {
      const graphHit = hitTestChunkGraph({ x: point.x, y: point.y });
      if (graphHit) {
        const graph = chunkView.graphs.find((entry) => entry.id === graphHit.graphId);
        if (graph) {
          chunkSelectedGraphId = graphHit.graphId;
          chunkSelectedStrokes = new Set();
          chunkSelection = {
            x: graph.bboxX,
            y: graph.bboxY,
            width: graph.bboxW,
            height: graph.bboxH,
          };
          chunkGraphDrag = {
            graphId: graphHit.graphId,
            mode: graphHit.mode,
            startPoint: { x: point.x, y: point.y },
            startGraph: cloneGraphObject(graph),
            beforeSnapshot: cloneGraphObjects(chunkView.graphs),
          };
          chunkSelectOrigin = null;
          chunkSelectRect = null;
          redrawChunkCanvases();
          e.preventDefault();
          return;
        }
      }
      if (chunkSelectedStrokes.size > 0 && chunkSelection) {
        const padding = chunkSelectionHitPadding();
        if (pointInSelectionBounds(point, chunkSelection, padding.x, padding.y)) {
          const strokeDrag = buildStrokeDragState(
            chunkView.strokes,
            chunkSelectedStrokes,
            { x: point.x, y: point.y },
            chunkSelection,
          );
          if (strokeDrag) {
            chunkSelectedGraphId = null;
            chunkGraphDrag = null;
            chunkStrokeDrag = strokeDrag;
            chunkSelectOrigin = null;
            chunkSelectRect = null;
            redrawChunkCanvases();
            e.preventDefault();
            return;
          }
        }
      }
      chunkSelectedGraphId = null;
      chunkGraphDrag = null;
      chunkStrokeDrag = null;
      chunkSelectOrigin = { x: point.x, y: point.y };
      chunkSelectRect = null;
      chunkSelectedStrokes = new Set();
      chunkSelection = null;
      redrawChunkCanvases();
      e.preventDefault();
      return;
    }

    if (chunkMode === "erase") {
      eraseInChunk(x, y);
      e.preventDefault();
      return;
    }

    if (chunkMode === "shape") {
      const origin = { ...point, pressure: 0.5 };
      chunkShapeDraft = {
        origin,
        current: { ...origin },
      };
      showChunkShapeOptions = false;
      drawChunkWet();
      e.preventDefault();
      return;
    }

    chunkIsDrawing = true;
    showChunkShapeOptions = false;
    chunkCurrentStroke = [{ ...point, pressure: e.pressure > 0 ? e.pressure : 0.5 }];
    e.preventDefault();
  }

  function onChunkPointerMove(e: PointerEvent) {
    if (!chunkView) return;
    if (e.pointerType === "touch") {
      const local = chunkClientToScreen(e.clientX, e.clientY);
      chunkTouchPointers = chunkTouchPointers.map((p) =>
        p.id === e.pointerId ? { id: e.pointerId, x: local.x, y: local.y } : p,
      );

      if (chunkActivePointerId !== null) {
        e.preventDefault();
        return;
      }

      if (chunkTouchPointers.length === 2) {
        const [a, b] = chunkTouchPointers;
        const newDist = touchDist(a, b);
        const newMid = touchMid(a, b);
        if (chunkLastPinchDist > 0) {
          const zoomFactor = newDist / chunkLastPinchDist;
          const newScale = Math.max(CHUNK_ZOOM_MIN, Math.min(CHUNK_ZOOM_MAX, chunkCamera.scale * zoomFactor));
          const wx = (newMid.x - chunkCamera.x) / chunkCamera.scale;
          const wy = (newMid.y - chunkCamera.y) / chunkCamera.scale;
          chunkCamera.x = newMid.x - wx * newScale;
          chunkCamera.y = newMid.y - wy * newScale;
          chunkCamera.x += newMid.x - chunkLastPinchMid.x;
          chunkCamera.y += newMid.y - chunkLastPinchMid.y;
          chunkCamera.scale = newScale;
        }
        chunkLastPinchDist = newDist;
        chunkLastPinchMid = newMid;
      } else if (chunkTouchPointers.length === 1) {
        const [touch] = chunkTouchPointers;
        const dx = touch.x - chunkLastPinchMid.x;
        const dy = touch.y - chunkLastPinchMid.y;
        chunkCamera.x += dx;
        chunkCamera.y += dy;
        chunkLastPinchMid = { x: touch.x, y: touch.y };
      }

      clampChunkCamera();
      redrawChunkCanvases();
      e.preventDefault();
      return;
    }

    if (e.pointerId !== chunkActivePointerId) return;
    e.preventDefault();
    const events: PointerEvent[] = e.getCoalescedEvents?.() ?? [e];
    if (chunkMode === "select") {
      const latest = events[events.length - 1];
      const { x, y } = pointerToChunkWorld(latest.clientX, latest.clientY);
      const point = chunkWorldToPoint(x, y);
      const drag = chunkGraphDrag;
      if (drag) {
        const graph = chunkView.graphs.find((entry) => entry.id === drag.graphId);
        if (!graph) return;
        const deltaX = point.x - drag.startPoint.x;
        const deltaY = point.y - drag.startPoint.y;
        let rect = {
          x: drag.startGraph.bboxX,
          y: drag.startGraph.bboxY,
          w: drag.startGraph.bboxW,
          h: drag.startGraph.bboxH,
        };
        if (drag.mode === "move") {
          rect = clampChunkGraphRect({
            x: drag.startGraph.bboxX + deltaX,
            y: drag.startGraph.bboxY + deltaY,
            w: drag.startGraph.bboxW,
            h: drag.startGraph.bboxH,
          });
        } else {
          rect = clampChunkGraphRect({
            x: drag.startGraph.bboxX,
            y: drag.startGraph.bboxY,
            w: drag.startGraph.bboxW + deltaX,
            h: drag.startGraph.bboxH + deltaY,
          });
        }
        const updated: SurfaceGraphObject = {
          ...graph,
          bboxX: rect.x,
          bboxY: rect.y,
          bboxW: rect.w,
          bboxH: rect.h,
        };
        updateChunkGraphInState(drag.graphId, updated);
        chunkSelection = { x: rect.x, y: rect.y, width: rect.w, height: rect.h };
        return;
      }
      if (chunkStrokeDrag) {
        const delta = clampChunkStrokeDragDelta(
          chunkStrokeDrag.startSelection,
          point.x - chunkStrokeDrag.startPoint.x,
          point.y - chunkStrokeDrag.startPoint.y,
        );
        translateDraggedStrokes(chunkStrokeDrag, delta.deltaX, delta.deltaY);
        chunkSelection = {
          x: chunkStrokeDrag.startSelection.x + delta.deltaX,
          y: chunkStrokeDrag.startSelection.y + delta.deltaY,
          width: chunkStrokeDrag.startSelection.width,
          height: chunkStrokeDrag.startSelection.height,
        };
        redrawChunkCanvases();
        return;
      }
      if (chunkSelectOrigin) {
        chunkSelectRect = {
          x: Math.min(chunkSelectOrigin.x, point.x),
          y: Math.min(chunkSelectOrigin.y, point.y),
          w: Math.abs(point.x - chunkSelectOrigin.x),
          h: Math.abs(point.y - chunkSelectOrigin.y),
        };
      }
      drawChunkWet();
      return;
    }
    if (chunkMode === "erase") {
      for (const event of events) {
        const { x, y } = pointerToChunkWorld(event.clientX, event.clientY);
        eraseInChunk(x, y);
      }
      return;
    }
    if (chunkMode === "shape") {
      if (!chunkShapeDraft) return;
      const latest = events[events.length - 1];
      const { x, y } = pointerToChunkWorld(latest.clientX, latest.clientY);
      const point = chunkWorldToPoint(x, y, 0.5);
      chunkShapeDraft = {
        origin: chunkShapeDraft.origin,
        current: { x: point.x, y: point.y, pressure: 0.5 },
      };
      drawChunkWet();
      return;
    }
    if (!chunkIsDrawing) return;
    for (const event of events) {
      const { x, y } = pointerToChunkWorld(event.clientX, event.clientY);
      chunkCurrentStroke.push(chunkWorldToPoint(x, y, event.pressure > 0 ? event.pressure : 0.5));
    }
    drawChunkWet();
  }

  async function onChunkPointerUp(e: PointerEvent) {
    const isActivePointer = chunkView && e.pointerId === chunkActivePointerId;
    if (!isActivePointer && e.pointerType === "touch") {
      chunkTouchPointers = chunkTouchPointers.filter((p) => p.id !== e.pointerId);
      if (chunkTouchPointers.length === 1) {
        chunkLastPinchMid = { x: chunkTouchPointers[0].x, y: chunkTouchPointers[0].y };
      } else if (chunkTouchPointers.length < 2) {
        chunkLastPinchDist = 0;
      }
      e.preventDefault();
      return;
    }
    if (!chunkView || e.pointerId !== chunkActivePointerId) return;
    e.preventDefault();
    chunkActivePointerId = null;
    if (chunkMode === "select") {
      if (chunkGraphDrag) {
        const before = cloneGraphObjects(chunkGraphDrag.beforeSnapshot);
        const target = cloneGraphObjects(chunkView.graphs);
        const changed = !graphArraysEquivalent(before, target);
        if (changed) {
          void applyChunkGraphChange(target, {
            recordHistory: true,
            beforeSnapshot: before,
          }).catch((err) => {
            console.warn("applyChunkGraphChange failed", err);
          });
        }
        chunkGraphDrag = null;
        chunkSelectOrigin = null;
        chunkSelectRect = null;
        redrawChunkCanvases();
        return;
      }
      if (chunkStrokeDrag) {
        const history = buildStrokeTransformHistory(chunkStrokeDrag);
        chunkStrokeDrag = null;
        if (history) {
          chunkStrokeTransformUndo = history;
          chunkStrokeTransformRedo = null;
          chunkSelectedStrokes = new Set(history.strokes.map((entry) => entry.stroke));
          chunkSelection = unionBBox(chunkSelectedStrokes);
          void persistChunkStrokeUpdates(history.strokes.map((entry) => entry.stroke));
        } else if (chunkSelectedStrokes.size > 0) {
          chunkSelection = unionBBox(chunkSelectedStrokes);
        }
        chunkSelectOrigin = null;
        chunkSelectRect = null;
        redrawChunkCanvases();
        return;
      }
      if (chunkSelectRect) {
        const hits = hitTestChunkStrokes(chunkSelectRect);
        chunkSelectedStrokes = hits;
        chunkSelectedGraphId = null;
        chunkSelection = unionBBox(hits)
          ?? { x: chunkSelectRect.x, y: chunkSelectRect.y, width: chunkSelectRect.w, height: chunkSelectRect.h };
      } else {
        chunkSelectedStrokes = new Set();
        if (chunkSelectedGraphId == null) {
          chunkSelection = null;
        }
      }
      chunkSelectOrigin = null;
      chunkSelectRect = null;
      redrawChunkCanvases();
      return;
    }
    if (chunkMode === 'erase') return;
    if (chunkMode === "shape") {
      const draft = chunkShapeDraft;
      chunkShapeDraft = null;
      drawChunkWet();
      if (!draft) return;
      await commitChunkShape(draft);
      return;
    }
    if (!chunkIsDrawing) return;
    chunkIsDrawing = false;

    const pts = chunkCurrentStroke;
    chunkCurrentStroke = [];
    drawChunkWet();
    commitChunkStrokePoints(pts);
  }

  function onChunkPointerCancel(e: PointerEvent) {
    const isActivePointer = chunkView && e.pointerId === chunkActivePointerId;
    if (!isActivePointer && e.pointerType === "touch") {
      chunkTouchPointers = chunkTouchPointers.filter((p) => p.id !== e.pointerId);
      if (chunkTouchPointers.length === 1) {
        chunkLastPinchMid = { x: chunkTouchPointers[0].x, y: chunkTouchPointers[0].y };
      } else if (chunkTouchPointers.length < 2) {
        chunkLastPinchDist = 0;
      }
      e.preventDefault();
      return;
    }
    if (!chunkView || e.pointerId !== chunkActivePointerId) return;
    chunkActivePointerId = null;
    if (chunkMode === "select") {
      chunkSelectOrigin = null;
      chunkSelectRect = null;
      chunkGraphDrag = null;
      chunkStrokeDrag = null;
      drawChunkWet();
      return;
    }
    if (chunkMode === "shape") {
      chunkShapeDraft = null;
      drawChunkWet();
      return;
    }
    const pts = chunkCurrentStroke;
    chunkIsDrawing = false;
    chunkCurrentStroke = [];
    drawChunkWet();
    commitChunkStrokePoints(pts);
  }

  function onChunkWheel(e: WheelEvent) {
    if (!chunkView) return;
    e.preventDefault();
    const target = e.currentTarget as HTMLDivElement;
    const rect = target.getBoundingClientRect();
    const sx = e.clientX - rect.left;
    const sy = e.clientY - rect.top;
    if (e.ctrlKey || e.metaKey) {
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      zoomChunkAt(sx, sy, factor);
      return;
    }
    chunkCamera.x -= e.deltaX;
    chunkCamera.y -= e.deltaY;
    clampChunkCamera();
    redrawChunkCanvases();
  }

  async function undoChunkStroke() {
    if (!chunkView) return;
    if (chunkStrokeTransformUndo) {
      const entry = chunkStrokeTransformUndo;
      chunkStrokeTransformUndo = null;
      applyStrokeTransformHistory(entry, "before");
      chunkStrokeTransformRedo = entry;
      chunkSelectedGraphId = null;
      chunkSelectedStrokes = new Set(entry.strokes.map((item) => item.stroke));
      chunkSelection = unionBBox(chunkSelectedStrokes);
      clearChunkInkContextCache();
      redrawChunkCanvases();
      await persistChunkStrokeUpdates(entry.strokes.map((item) => item.stroke));
      return;
    }
    if (chunkGraphUndoStack.length > 0) {
      const entry = chunkGraphUndoStack[chunkGraphUndoStack.length - 1];
      chunkGraphUndoStack = chunkGraphUndoStack.slice(0, -1);
      try {
        await applyChunkGraphChange(entry.before, { recordHistory: false });
        chunkGraphRedoStack = [...chunkGraphRedoStack, entry];
        chunkSelectedGraphId = null;
      } catch (err) {
        console.warn("chunk graph undo failed", err);
      }
      return;
    }
    if (chunkView.strokes.length === 0) return;
    const removed = getTrailingStrokeGroup(chunkView.strokes);
    if (removed.length === 0) return;
    chunkView.strokes = chunkView.strokes.slice(0, chunkView.strokes.length - removed.length);
    clearChunkStrokeTransformHistory();
    clearChunkInkContextCache();
    const removedSet = new Set(removed);
    const nextSelected = new Set([...chunkSelectedStrokes].filter((stroke) => !removedSet.has(stroke)));
    chunkSelectedStrokes = nextSelected;
    if (nextSelected.size === 0) {
      chunkSelection = null;
      chunkSelectRect = null;
      chunkSelectOrigin = null;
    } else {
      chunkSelection = unionBBox(nextSelected);
    }
    redrawChunkDry();
    for (const stroke of removed) {
      if (stroke.id === null) {
        cancelPendingStrokeCreate(stroke);
        continue;
      }
      try {
        await invoke("delete_surface_stroke", { strokeId: stroke.id });
      } catch (err) {
        console.error("delete_surface_stroke failed", err);
      }
    }
  }

  async function redoChunkGraphAction() {
    if (!chunkView) return;
    if (chunkStrokeTransformRedo) {
      const entry = chunkStrokeTransformRedo;
      chunkStrokeTransformRedo = null;
      applyStrokeTransformHistory(entry, "after");
      chunkStrokeTransformUndo = entry;
      chunkSelectedGraphId = null;
      chunkSelectedStrokes = new Set(entry.strokes.map((item) => item.stroke));
      chunkSelection = unionBBox(chunkSelectedStrokes);
      clearChunkInkContextCache();
      redrawChunkCanvases();
      await persistChunkStrokeUpdates(entry.strokes.map((item) => item.stroke));
      return;
    }
    if (chunkGraphRedoStack.length === 0) return;
    const entry = chunkGraphRedoStack[chunkGraphRedoStack.length - 1];
    chunkGraphRedoStack = chunkGraphRedoStack.slice(0, -1);
    try {
      await applyChunkGraphChange(entry.after, { recordHistory: false });
      chunkGraphUndoStack = [...chunkGraphUndoStack, entry];
      chunkSelectedGraphId = null;
    } catch (err) {
      console.warn("chunk graph redo failed", err);
    }
  }

  async function eraseInChunk(worldX: number, worldY: number) {
    if (!chunkView) return;
    const radius = CHUNK_ERASE_RADIUS_WORLD;
    const radiusSq = radius * radius;
    const keep: Stroke[] = [];
    const remove: Stroke[] = [];
    for (const s of chunkView.strokes) {
      const { minX, minY, maxX, maxY } = chunkStrokeWorldBounds(s);
      if (
        worldX < minX - radius || worldX > maxX + radius ||
        worldY < minY - radius || worldY > maxY + radius
      ) {
        keep.push(s);
        continue;
      }
      const hit = s.points.some((p) => {
        const wp = chunkPointToWorld(p);
        const dx = wp.x - worldX;
        const dy = wp.y - worldY;
        return dx * dx + dy * dy <= radiusSq;
      });
      if (hit) remove.push(s); else keep.push(s);
    }
    if (remove.length === 0) return;
    chunkView.strokes = keep;
    clearChunkStrokeTransformHistory();
    clearChunkInkContextCache();
    if (chunkSelectedStrokes.size > 0) {
      const keepSet = new Set(keep);
      const nextSelected = new Set(
        [...chunkSelectedStrokes].filter((stroke) => keepSet.has(stroke)),
      );
      chunkSelectedStrokes = nextSelected;
      if (nextSelected.size === 0) {
        chunkSelection = null;
        chunkSelectRect = null;
        chunkSelectOrigin = null;
      } else {
        chunkSelection = unionBBox(nextSelected);
      }
    }
    redrawChunkDry();
    for (const s of remove) {
      if (s.id !== null) void trackInkWrite(invoke<void>("delete_surface_stroke", { strokeId: s.id }).catch(() => {}));
      else cancelPendingStrokeCreate(s);
    }
  }

  async function deleteChunkSelectedStrokes() {
    if (chunkSelectedGraphId != null) {
      await deleteSelectedChunkGraph();
      return;
    }
    if (!chunkView || chunkSelectedStrokes.size === 0) return;
    const remove = [...chunkSelectedStrokes];
    clearChunkStrokeTransformHistory();
    chunkView.strokes = chunkView.strokes.filter((stroke) => !chunkSelectedStrokes.has(stroke));
    clearChunkInkContextCache();
    clearChunkSelectionState();
    redrawChunkCanvases();
    for (const stroke of remove) {
      if (stroke.id !== null) void trackInkWrite(invoke<void>("delete_surface_stroke", { strokeId: stroke.id }).catch(() => {}));
      else cancelPendingStrokeCreate(stroke);
    }
  }

  async function copySelectedChunkInk(): Promise<boolean> {
    if (!chunkView) return false;
    const nextClipboard = buildInkClipboardState("chunk", chunkView.strokes, chunkSelectedStrokes);
    if (!nextClipboard) return false;
    inkClipboard = nextClipboard;
    try {
      await copyTextToClipboard(serialiseInkClipboard(nextClipboard));
      inkClipboard = { ...nextClipboard, systemSynced: true };
    } catch {
      // Keep an in-memory copy so paste still works inside Gloss when clipboard APIs are unavailable.
    }
    return true;
  }

  async function openChunkPasteMenuAt(anchor: InkAnchorPoint): Promise<boolean> {
    const clipboard = await getInkClipboardForScope("chunk");
    if (!clipboard) return false;
    chunkPasteMenuAnchor = anchor;
    return true;
  }

  async function pasteChunkInk(anchorPoint: InkAnchorPoint | null = null): Promise<boolean> {
    if (chunkMode !== "select" || !chunkView) return false;
    const clipboard = await getInkClipboardForScope("chunk");
    if (!clipboard || !chunkView) return false;
    const prepared = preparePastedStrokes(
      clipboard,
      getChunkPageWorldSize(),
      chunkView.chunk.id,
      { minX: 0, maxX: 1, minY: 0, maxY: getChunkDocumentPageCount() },
      anchorPoint,
    );
    if (!prepared || !chunkView) return false;

    const view = chunkView;
    const pastedSelection = new Set(prepared.strokes);
    view.strokes = [...view.strokes, ...prepared.strokes];
    clearChunkInkContextCache();
    clearChunkStrokeTransformHistory();
    chunkSelectOrigin = null;
    chunkSelectRect = null;
    chunkSelectedGraphId = null;
    chunkGraphDrag = null;
    chunkSelectedStrokes = pastedSelection;
    chunkSelection = unionBBox(pastedSelection);
    chunkPasteHold = null;
    chunkPasteMenuAnchor = null;
    redrawChunkCanvases();

    for (const stroke of prepared.strokes) {
      void trackInkWrite(persistChunkStrokeCreate(view.surfaceId, stroke));
    }

    inkClipboard = { ...clipboard, pasteCount: prepared.nextPasteCount };
    return true;
  }

  async function pasteChunkInkFromMenu() {
    if (!chunkPasteMenuAnchor) return;
    await pasteChunkInk(chunkPasteMenuAnchor);
  }

  // â”€â”€ Chunking progress events â”€â”€
  let chunkingUnlisten: UnlistenFn | null = null;
  let chunkAiUnlisten: UnlistenFn | null = null;
  async function setupChunkingListener() {
    chunkingUnlisten = await listen<{ source_document_id: number; page_number: number; phase: string }>(
      "chunking_progress",
      (e) => {
        void appLogInfo(
        `[chunking] progress event doc=${e.payload.source_document_id} page=${e.payload.page_number} phase=${e.payload.phase}`,
        );
        if (!selectedBook) return;
        if (e.payload.source_document_id !== selectedBook.id) return;
        onBatchChunkProgressEvent(e.payload.phase, e.payload.page_number);
        if (e.payload.phase === "grouped" || e.payload.phase === "failed") {
          const key = pageKey(selectedBook.id, e.payload.page_number);
          chunkPageCache.delete(key);
          chunkPageRequests.delete(key);
        }
        if (e.payload.page_number === currentPage) {
          if (e.payload.phase === "extracted") currentChunkingStatus = "grouping";
          if (e.payload.phase === "grouped") {
            currentChunkingStatus = "done";
            currentPageChunkingActive = false;
            if (currentPageId !== null) void loadChunksForPage(currentPageId);
          }
          if (e.payload.phase === "failed") {
            currentChunkingStatus = "failed";
            currentPageChunkingActive = false;
          }
          return;
        }
        if (e.payload.phase === "grouped" || e.payload.phase === "failed") {
          void refreshCurrentPageChunkingActive(selectedBook.id, currentPage);
        }
      },
    );
  }

  async function setupChunkAiListener() {
    chunkAiUnlisten = await listen<ChunkAiStreamEventPayload>(
      "chunk_ai_stream",
      (event) => {
        const payload = event.payload;
        const activeRequestId = chunkChatActiveRequestId;
        if (!activeRequestId || payload.request_id !== activeRequestId) return;

        const activeChunkId = getActiveChatChunkId();
        const activePageId = getActiveViewerChatPageId();
        if (payload.context_kind === "chunk") {
          if (!activeChunkId || payload.chunk_id !== activeChunkId) return;
        } else if (payload.context_kind === "page") {
          if (!activePageId || payload.page_id !== activePageId) return;
        } else {
          return;
        }

        if (payload.phase === "delta") {
          if (!payload.delta) return;
          updateChunkChatMessage(payload.request_id, (message) => ({
            ...message,
            content: message.content + payload.delta,
          }));
          queueChunkChatScroll();
          return;
        }

        if (payload.phase === "completed") {
          updateChunkChatMessage(payload.request_id, (message) => ({
            ...message,
            state: "complete",
            html: renderChunkBodyHtml(message.content, undefined, {
              copyCodeBlocks: true,
              allowHeadings: false,
              allowStrong: false,
            }),
          }));
          chunkChatStreaming = false;
          chunkChatLoadingContext = false;
          chunkChatError = null;
          chunkChatActiveRequestId = null;
          queueChunkChatScroll();
          return;
        }

        if (payload.phase === "cancelled") {
          updateChunkChatMessage(payload.request_id, (message) => ({
            ...message,
            state: "stopped",
            content: message.content || "Stopped.",
          }));
          chunkChatStreaming = false;
          chunkChatLoadingContext = false;
          chunkChatError = null;
          chunkChatActiveRequestId = null;
          queueChunkChatScroll();
          return;
        }

        if (payload.phase === "error") {
          updateChunkChatMessage(payload.request_id, (message) => ({
            ...message,
            state: "error",
            content: message.content || payload.error || "Unable to reply.",
          }));
          chunkChatStreaming = false;
          chunkChatLoadingContext = false;
          chunkChatError = payload.error ?? "AI chat failed.";
          chunkChatActiveRequestId = null;
          queueChunkChatScroll();
        }
      },
    );
  }

  // â”€â”€ Wheel handler â”€â”€

  function handleWheel(e: WheelEvent) {
    if (!selectedBook) return;
    if (chunkView) return;
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

  // â”€â”€ Keyboard â”€â”€

  async function handleKeydown(e: KeyboardEvent) {
    if (showAiKeySheet) {
      if (e.key === "Escape") {
        e.preventDefault();
        dismissAiKeySettings();
      }
      return;
    }
    if (graphModalOpen) {
      if (e.key === "Escape") {
        e.preventDefault();
        closeGraphComposer();
      }
      return;
    }
    if (!selectedBook) return;
    if (e.defaultPrevented) return;
    if (showPaperOptions && e.key === "Escape") {
      e.preventDefault();
      showPaperOptions = false;
      return;
    }
    const eventTarget = e.target;
    if (
      eventTarget instanceof HTMLInputElement
      || eventTarget instanceof HTMLTextAreaElement
      || eventTarget instanceof HTMLSelectElement
      || (eventTarget instanceof HTMLElement && eventTarget.isContentEditable)
    ) {
      return;
    }

    if (chunkView) {
      if (e.key === "Escape") {
        e.preventDefault();
        closeChunkView();
      } else if (
        (e.ctrlKey || e.metaKey)
        && !e.altKey
        && !e.shiftKey
        && e.key.toLowerCase() === "c"
        && chunkMode === "select"
        && chunkSelectedStrokes.size > 0
        && !hasDocumentTextSelection()
      ) {
        e.preventDefault();
        await copySelectedChunkInk();
      } else if (
        (e.ctrlKey || e.metaKey)
        && !e.altKey
        && !e.shiftKey
        && e.key.toLowerCase() === "v"
        && chunkMode === "select"
      ) {
        if (await pasteChunkInk()) e.preventDefault();
      } else if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "z" && !e.shiftKey) {
        e.preventDefault();
        await undoChunkStroke();
      } else if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z"))) {
        e.preventDefault();
        await redoChunkGraphAction();
      } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key === "0") {
        e.preventDefault();
        restoreChunkHomeView();
      } else if (!e.ctrlKey && !e.metaKey && !e.altKey && (e.key === "+" || e.key === "=")) {
        e.preventDefault();
        zoomChunkAtViewportCenter(1 + ZOOM_STEP);
      } else if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key === "-") {
        e.preventDefault();
        zoomChunkAtViewportCenter(1 - ZOOM_STEP);
      } else if (!e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey && e.key === "ArrowLeft") {
        e.preventDefault();
        await navigateChunk(-1);
      } else if (!e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey && e.key === "ArrowRight") {
        e.preventDefault();
        await navigateChunk(1);
      } else if (
        (e.key === "Delete" || e.key === "Backspace")
        && chunkMode === "select"
        && (chunkSelectedStrokes.size > 0 || chunkSelectedGraphId != null)
      ) {
        e.preventDefault();
        await deleteChunkSelectedStrokes();
      }
      return;
    }

    if (
      (e.ctrlKey || e.metaKey)
      && !e.altKey
      && !e.shiftKey
      && e.key.toLowerCase() === "c"
      && mode === "select"
      && selectedStrokes.size > 0
      && !hasDocumentTextSelection()
    ) {
      e.preventDefault();
      await copySelectedPageInk();
    } else if (
      (e.ctrlKey || e.metaKey)
      && !e.altKey
      && !e.shiftKey
      && e.key.toLowerCase() === "v"
      && mode === "select"
    ) {
      if (await pastePageInk()) e.preventDefault();
    } else if ((e.ctrlKey || e.metaKey) && e.key === "z") {
      e.preventDefault();
      await undoStroke();
    } else if ((e.ctrlKey || e.metaKey) && (e.key === "y" || (e.shiftKey && e.key === "z"))) {
      e.preventDefault();
      await redoStroke();
    } else if (
      (e.key === "Delete" || e.key === "Backspace")
      && mode === "select"
      && (selectedStrokes.size > 0 || selectedPageGraphId != null)
    ) {
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
    refreshCustomModelMode(aiTaskSettings);
    paperSettings = loadPaperSettings();
    // Dev-only: expose invoke on window for ad-hoc debugging from devtools.
    (window as unknown as { glossInvoke?: typeof invoke }).glossInvoke = invoke;
    void (async () => {
      try {
        detachLogConsole = await attachConsole();
        await appLogInfo("[logging] attached webview console logger");
      } catch (err) {
        console.warn("[logging] failed to attach webview console logger", err);
      }
    })();
    window.addEventListener("keydown", handleKeydown);
    window.addEventListener("wheel", handleWheel, { passive: false });
    void setupChunkingListener();
    void setupChunkAiListener();
    void openBook(initialBook);
  });

  onDestroy(() => {
    void cancelChunkChatForReset();
    void flushPendingInkWrites();
    void flushChunkTextSaves();
    void flushGlossarySave();
    stopChunkPanelResize();
    clearGlossaryTypstPreviewTimer();
    resetGlossaryTypstPreviewState();
    resetGlossaryRewriteTypstPreviewState();
    window.removeEventListener("keydown", handleKeydown);
    window.removeEventListener("wheel", handleWheel);
    containerResizeObserver?.disconnect();
    chunkingUnlisten?.();
    chunkAiUnlisten?.();
    detachLogConsole?.();
    onViewerBatchStateChange(null);
  });

  let zoomPercent = $derived(Math.round(camera.scale * 100));
  let mainPaperSurfaceStyle = $derived.by(() => (
    `--paper-background-colour: ${paperSettings.backgroundColour};`
  ));

  $effect(() => {
    currentPage;
    if (pageInputFocused) return;
    pageInputValue = String(currentPage);
  });

  $effect(() => {
    providedAiTaskSettings;
    aiTaskSettings = providedAiTaskSettings;
    refreshCustomModelMode(aiTaskSettings);
  });

  $effect(() => {
    aiSettings = providedAiSettings;
  });

  $effect(() => {
    if (!chunkView || !chunkSheet) return;
    const syncWidth = () => {
      chunkLeftPanelWidth = clampChunkLeftPanelWidth(chunkLeftPanelWidth);
    };
    syncWidth();
    const observer = new ResizeObserver(syncWidth);
    observer.observe(chunkSheet);
    return () => observer.disconnect();
  });

  $effect(() => {
    selectedBook;
    totalPages;
    canBatchChunkDocument;
    batchChunkStarting;
    batchChunkError;
    batchChunkFeedback;
    batchChunkStartInput;
    batchChunkEndInput;
    batchChunkSkipChunkedPages;
    pastPaperInstructionPagesEnabled;
    pastPaperInstructionStartInput;
    pastPaperInstructionEndInput;
    pastPaperInstructionSaving;
    pastPaperInstructionError;
    pastPaperInstructionFeedback;
    pastPaperInstructionCheckLoading;
    pastPaperInstructionCheckError;
    pastPaperInstructionCheckFeedback;
    pastPaperInstructionCheckPreview;
    onViewerBatchStateChange({
      selectedBook,
      totalPages,
      canBatchChunkDocument,
      batchChunkStarting,
      batchChunkError,
      batchChunkFeedback,
      batchChunkStartInput,
      batchChunkEndInput,
      batchChunkSkipChunkedPages,
      setBatchChunkStartInput: (value: string) => batchChunkStartInput = value,
      setBatchChunkEndInput: (value: string) => batchChunkEndInput = value,
      setBatchChunkSkipChunkedPages: (value: boolean) => batchChunkSkipChunkedPages = value,
      chunkPageRangeFromInputs,
      chunkWholePdf,
      pastPaperInstructionPagesEnabled,
      setPastPaperInstructionPagesEnabled: (value: boolean) => pastPaperInstructionPagesEnabled = value,
      pastPaperInstructionStartInput,
      pastPaperInstructionEndInput,
      setPastPaperInstructionStartInput: (value: string) => pastPaperInstructionStartInput = value,
      setPastPaperInstructionEndInput: (value: string) => pastPaperInstructionEndInput = value,
      pastPaperInstructionSaving,
      pastPaperInstructionError,
      pastPaperInstructionFeedback,
      pastPaperInstructionCheckLoading,
      pastPaperInstructionCheckError,
      pastPaperInstructionCheckFeedback,
      pastPaperInstructionCheckPreview,
      savePastPaperInstructionRange,
      checkPastPaperInstructionContext,
    });
  });
</script>

<main class:viewer-open={!!selectedBook}>
  {#if selectedBook}
    <!-- PDF Viewer -->
    <div class="viewer">
      <ViewerHeader
        selectedBook={selectedBook}
        {aiTaskSettings}
        {aiSettings}
        {setTaskProvider}
        openAiSettings={openAiSettings}
        {reChunkCurrentPage}
        {canRechunkPage}
        {reChunkingPage}
        {closeViewer}
      />

      {#if error}
        <p class="error">{error}</p>
      {/if}

      {#if batchChunkProgress && batchChunkProgress.docId === selectedBook.id}
        <BatchChunkProgressCard
          {batchChunkProgress}
          {batchChunkProgressPercent}
          {dismissBatchChunkProgress}
        />
      {/if}

      <!-- Infinite canvas -->
      <div
        class="infinite-canvas"
        bind:this={canvasContainer}
        class:mode-erase={mode === 'erase'}
        class:mode-shape={mode === 'shape'}
        class:mode-select={mode === 'select'}
        class:notebook-surface={isNotebookDocument()}
        style={mainPaperSurfaceStyle}
        use:observeContainerResize
      >
        <canvas bind:this={gridCanvas} class="layer layer-grid"></canvas>
        <canvas bind:this={pdfCanvas}  class="layer layer-pdf"></canvas>
        <canvas bind:this={chunkOverlayCanvas} class="layer layer-chunk"></canvas>
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

      {#if viewerAiOpen && !chunkView}
        <ViewerAiPanel
          viewerPageNumber={currentPage}
          {chunkChatMessages}
          bind:chunkChatDraft
          {chunkChatStreaming}
          {chunkChatLoadingContext}
          {chunkChatError}
          {chatAttachmentTranscribing}
          {chunkInkContextTranscribing}
          {pendingChatAttachment}
          {clearPendingChatAttachment}
          {stopChunkChat}
          {sendChunkChatMessage}
          isChatContextReady={isChatContextReady()}
          {onChunkChatKeydown}
          closeViewerAiPanel={closeViewerAiPanel}
          {chunkChatCodeCopy}
          bind:chunkChatTranscript
          viewerContextReady={getActiveViewerChatPageId() != null}
          isNotebookContext={isNotebookDocument()}
        />
      {/if}

      {#if chunkView}
        {@const cc = CHUNK_COLOURS[chunkView.chunk.chunk_type] ?? FALLBACK_CHUNK_COLOUR}
        <div class="chunk-sheet-backdrop" style={`--chunk-accent: ${cc.accent}; --chunk-tint: ${cc.tint};`}>
          <button
            class="chunk-nav-btn chunk-nav-prev"
            type="button"
            onclick={() => void navigateChunk(-1)}
            disabled={!canOpenPreviousChunk || chunkNavigationBusy}
            aria-label="Previous chunk"
            title="Previous chunk (Left Arrow)"
          >
            <svg viewBox="0 0 16 16" fill="none" width="15" height="15">
              <path d="M9.6 3.4L5.2 8l4.4 4.6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          </button>
          <div
            class="chunk-sheet"
            bind:this={chunkSheet}
            role="dialog"
            aria-modal="true"
            aria-label={`${cc.label} notes`}
            transition:fade={{ duration: 140 }}
          >

            <!-- Left panel: chunk content -->
            <div class="chunk-panel-left" style={`--chunk-accent: ${cc.accent}; --chunk-tint: ${cc.tint}; --chunk-panel-left-width: ${chunkLeftPanelWidth}px;`}>
              <div class="cpl-meta">
                <span class="chunk-badge">{cc.short}</span>
                <span class="chunk-status-dot status-{chunkView.chunk.status}"></span>
                {#if chunkViewTitleDisplay?.indexLabel}
                  <span class="chunk-index-label">{chunkViewTitleDisplay.indexLabel}</span>
                {/if}
                <button class="chunk-close-btn" type="button" onclick={closeChunkView} aria-label="Close chunk notes">
                  <svg viewBox="0 0 16 16" fill="none" width="14" height="14">
                    <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
                  </svg>
                </button>
              </div>
              <div class="cpl-edit-status">
                {#if chunkTextSaveError}
                  <span class="cpl-edit-status-error">Save failed</span>
                {:else if chunkTextSaving}
                  Savingâ€¦
                {:else}
                  Saved
                {/if}
              </div>
              {#if chunkTitleEditing}
                <input
                  class="cpl-title-input"
                  type="text"
                  bind:value={chunkTitleDraft}
                  bind:this={chunkTitleInputEl}
                  placeholder={chunkView.chunk.subject ? `Proof of ${chunkView.chunk.subject}` : "Add title"}
                  oninput={onChunkTitleInput}
                  onkeydown={onChunkTitleEditorKeydown}
                  onblur={() => void finishChunkTitleEdit()}
                  spellcheck="true"
                />
              {:else if chunkViewTitleDisplay}
                <button
                  class="cpl-title cpl-title-display"
                  type="button"
                  onclick={beginChunkTitleEdit}
                  aria-label="Edit chunk title"
                >{chunkViewTitleDisplay.heading}</button>
	              {:else}
	                <button class="cpl-title-add-btn" type="button" onclick={beginChunkTitleEdit}>
	                  {chunkView.chunk.subject ? `Proof of ${chunkView.chunk.subject}` : "Add title"}
	                </button>
	              {/if}

	              {#if chunkIsQuestion}
	                <section class="cpl-question-meta">
	                  <div class="cpl-question-row">
	                    <span class="cpl-question-label">Question</span>
	                    <span class="cpl-question-value">
	                      {chunkView.chunk.question_label ? `Q${chunkView.chunk.question_label}` : "Unknown"}
	                    </span>
	                  </div>
	                  <div class="cpl-question-row">
	                    <span class="cpl-question-label">Marks</span>
	                    {#if questionAvailableMarksEditing}
	                      <span class="cpl-question-value cpl-question-marks-editor">
	                        <span class="cpl-question-marks-prefix">{chunkAchievedMarksDisplay ?? "—"}/</span>
	                        <input
	                          class="cpl-question-marks-input"
	                          type="text"
	                          inputmode="numeric"
	                          pattern="[0-9]*"
	                          placeholder="?"
	                          bind:this={questionAvailableMarksInputEl}
	                          bind:value={questionAvailableMarksDraft}
	                          disabled={questionAvailableMarksSaving}
	                          onkeydown={(event) => {
	                            if (event.key === "Enter") {
	                              event.preventDefault();
	                              void saveQuestionAvailableMarks();
	                            }
	                            if (event.key === "Escape") {
	                              event.preventDefault();
	                              cancelQuestionAvailableMarksEdit();
	                            }
	                          }}
	                          onblur={() => {
	                            if (!questionAvailableMarksSaving) {
	                              void saveQuestionAvailableMarks();
	                            }
	                          }}
	                        />
	                      </span>
	                    {:else}
	                      <button
	                        class="cpl-question-value cpl-question-value-button"
	                        type="button"
	                        onclick={beginQuestionAvailableMarksEdit}
	                      >
	                        {chunkMarksDisplay ?? "—/?"}
	                      </button>
	                    {/if}
	                  </div>
	                  <div class="cpl-question-entry">
	                    <input
	                      class="cpl-question-input"
	                      type="text"
	                      inputmode="decimal"
	                      placeholder="Achieved marks"
	                      bind:value={questionAchievedMarksDraft}
	                      disabled={questionAchievedMarksSaving}
	                      onkeydown={(event) => {
	                        if (event.key === "Enter") {
	                          event.preventDefault();
	                          void saveQuestionAchievedMarks();
	                        }
	                      }}
	                    />
	                    <button
	                      class="cpl-question-save"
	                      type="button"
	                      onclick={() => void saveQuestionAchievedMarks()}
	                      disabled={questionAchievedMarksSaving}
	                    >
	                      {questionAchievedMarksSaving ? "Saving..." : "Save"}
	                    </button>
	                  </div>
	                  <div class="cpl-question-source-row">
	                    <button
	                      class="cpl-question-source"
	                      type="button"
	                      onclick={() => void openQuestionSource()}
	                      disabled={questionSourceLoading}
	                    >
	                      {questionSourceLoading ? "Loading source..." : "Open source"}
	                    </button>
	                    {#if questionSourceSlices.length > 0}
	                      <span class="cpl-question-source-count">
	                        {questionSourceSlices.length} source {questionSourceSlices.length === 1 ? "slice" : "slices"}
	                      </span>
	                    {/if}
	                  </div>
	                  {#if questionAchievedMarksError}
	                    <p class="cpl-question-error">{questionAchievedMarksError}</p>
	                  {/if}
	                  {#if questionAvailableMarksError}
	                    <p class="cpl-question-error">{questionAvailableMarksError}</p>
	                  {/if}
	                  {#if questionSourceError}
	                    <p class="cpl-question-error">{questionSourceError}</p>
	                  {/if}
	                </section>
	              {/if}

	              {#if linkedChunkLabel}
	                <section class="cpl-linked">
                  <button
                    class="cpl-linked-toggle"
                    type="button"
                    aria-expanded={linkedChunkExpanded}
                    onclick={() => linkedChunkExpanded = !linkedChunkExpanded}
                  >
                    <span class="cpl-linked-toggle-label">{linkedChunkLabel}</span>
                    <span class="cpl-linked-toggle-hint">{linkedChunkExpanded ? "Hide" : "Show"}</span>
                    <svg class="cpl-linked-toggle-chevron" class:open={linkedChunkExpanded} viewBox="0 0 16 16" fill="none" width="12" height="12" aria-hidden="true">
                      <path d="M3.8 5.8L8 10l4.2-4.2" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
                    </svg>
                  </button>
                  {#if linkedChunkExpanded}
                    <div class="cpl-linked-content">
                      {#if linkedChunkTargetLoading}
                        <p class="cpl-linked-empty">Finding linked chunk...</p>
                      {:else if linkedChunkTargetError}
                        <p class="cpl-linked-error">Couldn't find linked chunk.</p>
                      {:else if linkedChunkTargetId == null}
                        <p class="cpl-linked-empty">{linkedChunkEmptyText}</p>
                      {:else if linkedChunkPreviewLoading}
                        <p class="cpl-linked-empty">Loading linked chunk...</p>
                      {:else if linkedChunkPreviewError}
                        <p class="cpl-linked-error">Couldn't load linked chunk.</p>
                      {:else if linkedChunkPreview}
                        {@const linkedChunkColour = CHUNK_COLOURS[linkedChunkPreview.chunk_type] ?? FALLBACK_CHUNK_COLOUR}
                        {@const linkedChunkTitleDisplay = getChunkTitleDisplay({ chunk_type: linkedChunkPreview.chunk_type, title: linkedChunkPreview.title })}
                        <div
                          class="cpl-linked-card"
                          style={`--linked-chunk-accent: ${linkedChunkColour.accent}; --linked-chunk-tint: ${linkedChunkColour.tint};`}
                        >
                          <div class="cpl-linked-meta">
                            <span class="cpl-linked-badge">{linkedChunkColour.short}</span>
                            <span class="cpl-linked-title">
                              {linkedChunkTitleDisplay?.heading ?? linkedChunkPreview.subject ?? linkedChunkColour.label}
                            </span>
                          </div>
                          {#if linkedChunkPreviewBodyHtml}
                            <div class="cpl-linked-body">{@html linkedChunkPreviewBodyHtml}</div>
                          {:else}
                            <div class="cpl-linked-body cpl-linked-body-muted">No text available yet.</div>
                          {/if}
                          <div class="cpl-linked-actions">
                            <button class="cpl-linked-open-btn" type="button" onclick={openLinkedChunkFromPanel}>
                              Open chunk
                            </button>
                          </div>
                        </div>
                      {:else}
                        <p class="cpl-linked-empty">{linkedChunkEmptyText}</p>
                      {/if}
                    </div>
                  {/if}
                </section>
              {/if}

              {#if chunkBodyEditing}
                <textarea
                  class="cpl-body-editor"
                  bind:value={chunkBodyDraft}
                  bind:this={chunkBodyTextareaEl}
                  placeholder="Edit chunk body text in Markdown. LaTeX works via $â€¦$ inline or $$â€¦$$ block."
                  oninput={onChunkBodyInput}
                  onkeydown={onChunkBodyEditorKeydown}
                  onblur={() => void finishChunkBodyEdit()}
                  spellcheck="true"
                ></textarea>
              {:else}
                <div
                  class="cpl-body cpl-body-display"
                  role="button"
                  tabindex="0"
                  onclick={onChunkBodyDisplayClick}
                  onkeydown={onChunkBodyDisplayKeydown}
                >
                  {@html chunkViewBodyHtml}
                </div>
              {/if}
            </div>

            <div
              class="chunk-panel-resizer"
              style={`--chunk-accent: ${cc.accent};`}
              role="slider"
              aria-label="Resize chunk text panel"
              aria-orientation="vertical"
              aria-valuemin={getChunkLeftPanelBounds().min}
              aria-valuemax={getChunkLeftPanelBounds().max}
              aria-valuenow={Math.round(chunkLeftPanelWidth)}
              tabindex="0"
              onpointerdown={startChunkPanelResize}
              onkeydown={onChunkPanelResizeKeydown}
            ></div>

            <!-- Right panel: tabs + content -->
            <div class="chunk-panel-right" style={`--chunk-accent: ${cc.accent};`}>
              <div class="chunk-tab-bar">
                <button
                  class="chunk-tab"
                  class:active={chunkTab === 'ink'}
                  onclick={() => {
                    chunkTab = 'ink';
                    showChunkPenOptions = false;
                    showChunkShapeOptions = false;
                    showPaperOptions = false;
                  }}
                >
                  <svg viewBox="0 0 16 16" fill="none" width="13" height="13">
                    <path d="M3 13l1.8-0.7 7-7-1.1-1.1-7 7z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
                    <circle cx="3" cy="13" r="0.7" fill="currentColor"/>
                  </svg>
                  Ink
                </button>
                <button
                  class="chunk-tab"
                  class:active={chunkTab === 'glossary'}
                  onclick={() => {
                    chunkTab = 'glossary';
                    showChunkPenOptions = false;
                    showChunkShapeOptions = false;
                    showPaperOptions = false;
                    if (glossaryFormat === "typst") {
                      scheduleGlossaryTypstPreview(0);
                    }
                  }}
                >
	                  <svg viewBox="0 0 16 16" fill="none" width="13" height="13">
	                    <rect x="4" y="2" width="7" height="9" rx="0.8" stroke="currentColor" stroke-width="1.2"/>
	                    <path d="M3 12.5h10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
	                  </svg>
	                  {chunkGlossaryTabLabel}
	                </button>
                <button
                  class="chunk-tab"
                  class:active={chunkTab === 'ai'}
                  onclick={() => {
                    chunkTab = 'ai';
                    showChunkPenOptions = false;
                    showChunkShapeOptions = false;
                    showPaperOptions = false;
                  }}
                >
                  AI
                </button>
                <button
                  class="chunk-tab-settings"
                  type="button"
                  onclick={openAiSettings}
                >
                  AI settings
                </button>
              </div>

              {#if chunkTab === 'ink'}
                <div
                  class="chunk-sheet-surface"
                  class:mode-erase={chunkMode === 'erase'}
                  class:mode-shape={chunkMode === 'shape'}
                  class:mode-select={chunkMode === 'select'}
                  class:paper-dotted={paperSettings.pattern === 'dotted'}
                  class:paper-grid={paperSettings.pattern === 'grid'}
                  class:paper-line={paperSettings.pattern === 'line'}
                  style={chunkGridStyle}
                  use:setupChunkCanvases
                  onwheel={onChunkWheel}
                >
                  <canvas bind:this={chunkDryCanvas} class="chunk-layer chunk-layer-dry"></canvas>
                  <canvas
                    bind:this={chunkWetCanvas}
                    class="chunk-layer chunk-layer-wet"
                    onpointerdown={onChunkPointerDown}
                    onpointermove={onChunkPointerMove}
                    onpointerup={onChunkPointerUp}
                    onpointercancel={onChunkPointerCancel}
                  ></canvas>
                  <div class="chunk-ink-bar">
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={undoChunkStroke}
                      disabled={!chunkStrokeTransformUndo && chunkView.strokes.length === 0 && chunkGraphUndoStack.length === 0}
                      aria-label="Undo chunk action"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M9 14 4 9l5-5"/>
                        <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>
                      </svg>
                    </button>
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={redoChunkGraphAction}
                      disabled={!chunkStrokeTransformRedo && chunkGraphRedoStack.length === 0}
                      aria-label="Redo chunk action"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M15 14l5-5-5-5"/>
                        <path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>
                      </svg>
                    </button>
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={restoreChunkHomeView}
                      aria-label="Return to default view"
                      title="Return to default view (Ctrl+0)"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <rect x="3" y="3" width="18" height="18" rx="2"/>
                        <line x1="12" y1="8" x2="12" y2="16"/>
                        <line x1="8" y1="12" x2="16" y2="12"/>
                      </svg>
                    </button>
                    <div class="paper-tool">
                      <button
                        class="tool-btn"
                        class:active={showPaperOptions}
                        type="button"
                        onclick={togglePaperOptions}
                        aria-label="Paper options"
                        aria-pressed={showPaperOptions}
                        title="Paper options"
                      >
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
                          <rect x="5" y="3.5" width="14" height="17" rx="2"/>
                          <path d="M9 8h6"/>
                          <path d="M9 12h6"/>
                          <path d="M9 16h4"/>
                        </svg>
                      </button>
                      {#if showPaperOptions}
                        <div class="paper-popout chunk-paper-popout" transition:fade={{ duration: 140 }}>
                          <div class="paper-popout-header">
                            <span class="paper-popout-title">Paper</span>
                            <span class="paper-sample" style={`--paper-background-colour: ${paperSettings.backgroundColour}; --paper-pattern-colour: ${paperPatternColour(paperPatternAlpha())};`}>
                              <span
                                class="paper-sample-pattern"
                                class:paper-dotted={paperSettings.pattern === 'dotted'}
                                class:paper-grid={paperSettings.pattern === 'grid'}
                                class:paper-line={paperSettings.pattern === 'line'}
                              ></span>
                            </span>
                          </div>
                          <div class="paper-pattern-options" aria-label="Paper pattern">
                            {#each PAPER_PATTERN_OPTIONS as option}
                              <button
                                class="paper-option"
                                class:active={paperSettings.pattern === option.value}
                                type="button"
                                onclick={() => updatePaperSettings({ pattern: option.value })}
                                aria-pressed={paperSettings.pattern === option.value}
                              >
                                {option.label}
                              </button>
                            {/each}
                          </div>
                          <label class="pen-slider-group" for="chunk-paper-spacing">
                            <span>Spacing</span>
                            <span>{paperSettings.spacing} px</span>
                          </label>
                          <input
                            id="chunk-paper-spacing"
                            class="pen-slider"
                            type="range"
                            min={PAPER_SPACING_MIN}
                            max={PAPER_SPACING_MAX}
                            step="1"
                            value={paperSettings.spacing}
                            oninput={(e) => updatePaperSettings({ spacing: Number((e.currentTarget as HTMLInputElement).value) })}
                          />
                          <div class="paper-colours" aria-label="Paper colours">
                            {#each PAPER_BACKGROUND_SWATCHES as colour}
                              <button
                                class="paper-colour-swatch"
                                class:selected={paperSettings.backgroundColour === colour}
                                type="button"
                                onclick={() => updatePaperSettings({ backgroundColour: colour })}
                                aria-label={`Select ${colour} paper`}
                                aria-pressed={paperSettings.backgroundColour === colour}
                                style={`--paper-swatch-colour: ${colour};`}
                              ></button>
                            {/each}
                            <label
                              class="paper-colour-custom"
                              class:selected={!PAPER_BACKGROUND_SWATCHES.includes(paperSettings.backgroundColour)}
                              aria-label="Custom paper colour"
                              style={`--paper-swatch-colour: ${paperSettings.backgroundColour};`}
                            >
                              <input
                                type="color"
                                value={paperSettings.backgroundColour}
                                oninput={(e) => updatePaperSettings({ backgroundColour: (e.currentTarget as HTMLInputElement).value })}
                              />
                            </label>
                          </div>
                        </div>
                      {/if}
                    </div>
                    <div class="divider"></div>
                    <div class="pen-tool">
                      <button
                        class="tool-btn"
                        class:active={chunkMode === 'draw'}
                        type="button"
                        onclick={activateChunkDrawTool}
                        aria-label="Draw"
                        aria-pressed={chunkMode === 'draw'}
                      >
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                          <path d="M12 19l7-7 3 3-7 7-3-3z"/>
                          <path d="M18 13l-1.5-7.5L2 2l3.5 14.5L13 18l5-5z"/>
                          <path d="M2 2l7.586 7.586"/>
                          <circle cx="11" cy="11" r="2"/>
                        </svg>
                      </button>
                      {#if showChunkPenOptions}
                        <div class="pen-popout chunk-pen-popout" transition:fade={{ duration: 140 }}>
                          <div class="pen-popout-header">
                            <span class="pen-popout-title">Pen</span>
                            <span class="pen-preview" style={`--pen-preview-colour: ${penColour}; --pen-preview-size: ${penThickness}px;`}>
                              <span class="pen-preview-dot"></span>
                            </span>
                          </div>
                          <label class="pen-slider-group" for="chunk-pen-thickness">
                            <span>Thickness</span>
                            <span>{penThickness.toFixed(1)} px</span>
                          </label>
                          <input
                            id="chunk-pen-thickness"
                            class="pen-slider"
                            type="range"
                            min="1"
                            max="12"
                            step="0.5"
                            value={penThickness}
                            oninput={(e) => penThickness = Number((e.currentTarget as HTMLInputElement).value)}
                          />
                          <div class="pen-colours" aria-label="Pen colours">
                            {#each PEN_COLOURS as colour}
                              <button
                                class="colour-swatch"
                                class:selected={penColour === colour}
                                type="button"
                                onclick={() => penColour = colour}
                                aria-label={`Select ${colour} pen`}
                                aria-pressed={penColour === colour}
                                style={`--swatch-colour: ${colour};`}
                              ></button>
                            {/each}
                          </div>
                        </div>
                      {/if}
                    </div>
                    <div class="shape-tool">
                      <button
                        class="tool-btn"
                        class:active={chunkMode === 'shape'}
                        type="button"
                        onclick={activateChunkShapeTool}
                        aria-label="Shape"
                        aria-pressed={chunkMode === 'shape'}
                      >
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
                          <circle cx="8" cy="8" r="3.2"/>
                          <line x1="13" y1="16" x2="21" y2="16"/>
                          <line x1="17" y1="12" x2="17" y2="20"/>
                        </svg>
                      </button>
                      {#if showChunkShapeOptions}
                        <div class="shape-popout chunk-shape-popout" transition:fade={{ duration: 140 }}>
                          {#each SHAPE_OPTIONS as option}
                            <button
                              class="shape-option"
                              class:active={shapeKind === option.value}
                              type="button"
                              onclick={() => shapeKind = option.value}
                              aria-pressed={shapeKind === option.value}
                            >
                              <span class="shape-option-title">{option.label}</span>
                              <span class="shape-option-hint">{option.hint}</span>
                            </button>
                          {/each}
                        </div>
                      {/if}
                    </div>
                    <button
                      class="tool-btn"
                      class:active={pendingGraphPlacement?.scope === "chunk"}
                      type="button"
                      onclick={activateChunkGraphTool}
                      aria-label="Graph"
                      aria-pressed={pendingGraphPlacement?.scope === "chunk"}
                      title="Compose a graph"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M4 20V4"/>
                        <path d="M4 20H20"/>
                        <path d="M6 16c1.6-2.8 3-4.2 4.6-4.2 1.9 0 2.7 2.7 4.5 2.7 1.4 0 2.8-1.4 4.9-5.5"/>
                      </svg>
                    </button>
                    <button
                      class="tool-btn"
                      class:active={chunkMode === 'erase'}
                      type="button"
                      onclick={activateChunkEraseTool}
                      aria-label="Erase"
                      aria-pressed={chunkMode === 'erase'}
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M20 20H7L3 16l10-10 7 7-2.5 2.5"/>
                        <path d="M6.5 17.5l5-5"/>
                      </svg>
                    </button>
                    <button
                      class="tool-btn"
                      class:active={chunkMode === 'select'}
                      type="button"
                      onclick={activateChunkSelectTool}
                      aria-label="Select"
                      aria-pressed={chunkMode === 'select'}
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M5 3l14 9-7 1-4 7-3-17z"/>
                      </svg>
                    </button>
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={() => void copySelectedChunkInk()}
                      disabled={chunkMode !== 'select' || chunkSelectedStrokes.size === 0}
                      aria-label="Copy selected ink"
                      title="Copy selected ink (Ctrl/Cmd+C)"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <rect x="9" y="9" width="11" height="11" rx="2"/>
                        <path d="M15 5h-9a2 2 0 0 0-2 2v9"/>
                      </svg>
                    </button>
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={() => void pasteChunkInk()}
                      disabled={chunkMode !== 'select' || !hasChunkInkClipboard}
                      aria-label="Paste copied ink"
                      title="Paste copied ink (Ctrl/Cmd+V)"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <rect x="6" y="4" width="12" height="16" rx="2"/>
                        <path d="M9 4.5V3h6v1.5"/>
                        <path d="M9 9h6"/>
                      </svg>
                    </button>
                    <button
                      class="tool-btn ai-btn"
                      class:active={!!chunkSelection}
                      type="button"
                      onclick={onChunkAiClick}
                      disabled={!chunkSelection || chunkAiWorking}
                      aria-label="Ask AI about selected chunk area"
                      title="Open AI chat with selected area"
                    >
                      {#if chunkAiWorking}
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
                    <button
                      class="ink-btn"
                      type="button"
                      onclick={() => void deleteChunkSelectedStrokes()}
                      disabled={chunkSelectedStrokes.size === 0 && chunkSelectedGraphId == null}
                      aria-label="Delete selected strokes"
                      title="Delete selected strokes"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                        <polyline points="3 6 5 6 21 6"/>
                        <path d="M19 6l-1 14H6L5 6m3 0V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/>
                        <line x1="10" y1="11" x2="10" y2="17"/>
                        <line x1="14" y1="11" x2="14" y2="17"/>
                      </svg>
                    </button>
                    <div class="divider"></div>
                    <button
                      class="zoom-btn"
                      type="button"
                      onclick={() => zoomChunkAtViewportCenter(1 - ZOOM_STEP)}
                      disabled={chunkCamera.scale <= CHUNK_ZOOM_MIN}
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
                      type="button"
                      onclick={restoreChunkHomeView}
                      title="Return to default view (Ctrl+0)"
                      aria-label="Return to default view"
                    >
                      {chunkZoomPercent}%
                    </button>
                    <button
                      class="zoom-btn"
                      type="button"
                      onclick={() => zoomChunkAtViewportCenter(1 + ZOOM_STEP)}
                      disabled={chunkCamera.scale >= CHUNK_ZOOM_MAX}
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
              {:else if chunkTab === 'glossary'}
                <div class="chunk-glossary-pane">
                  <div class="chunk-glossary-bar">
                    <div class="chunk-glossary-bar-main">
                      <div class="chunk-glossary-mode">
                        <button
                          type="button"
                          class="chunk-glossary-mode-btn"
                          class:active={glossaryMode === 'edit'}
                          onclick={() => setGlossaryMode('edit')}
                        >Edit</button>
                        <button
                          type="button"
                          class="chunk-glossary-mode-btn"
                          class:active={glossaryMode === 'preview'}
                          onclick={() => setGlossaryMode('preview')}
                        >Preview</button>
                      </div>
                      <div class="chunk-glossary-format-picker" role="group" aria-label="Note format">
                        <button
                          type="button"
                          class="chunk-glossary-format-btn"
                          class:active={glossaryFormat === 'markdown'}
                          onclick={() => setGlossaryFormat('markdown')}
                        >Markdown</button>
                        <button
                          type="button"
                          class="chunk-glossary-format-btn"
                          class:active={glossaryFormat === 'typst'}
                          onclick={() => setGlossaryFormat('typst')}
                        >Typst</button>
                      </div>
                      <button
                        type="button"
                        class="chunk-glossary-insert-graph"
                        onclick={openGlossaryGraphComposer}
                        disabled={glossaryFormat === 'typst'}
                        aria-disabled={glossaryFormat === 'typst'}
                        title={glossaryFormat === 'typst' ? "Insert graph is only available in markdown mode." : "Insert graph"}
                      >Insert graph</button>
                      {#if glossaryFormat === 'typst'}
                        <button
                          type="button"
                          class="chunk-glossary-export-pdf"
                          onclick={() => void exportGlossaryTypstPdf()}
                          disabled={!glossaryDraft.trim() || glossaryPdfExporting}
                          title={!glossaryDraft.trim() ? "Write some Typst before saving." : "Save this Typst note as a PDF."}
                        >{glossaryPdfExporting ? "Saving PDF..." : "Save PDF"}</button>
                      {/if}
                    </div>
                    <span class="chunk-glossary-status" title={glossaryPdfExportError ?? glossaryPdfExportFeedback ?? undefined}>
                      {#if glossaryPdfExportError}
                        <span class="chunk-glossary-status-error">PDF save failed</span>
                      {:else if glossaryPdfExporting}
                        Saving PDF…
                      {:else if glossaryPdfExportFeedback}
                        PDF saved
                      {:else if glossarySaveError}
                        <span class="chunk-glossary-status-error">Save failed</span>
                      {:else if glossarySaving}
                        Saving…
                      {:else if glossaryDraft.trim()}
                        Saved
                      {/if}
                    </span>
                  </div>
                  {#if glossaryFormatNotice}
                    <p class="chunk-glossary-format-notice">{glossaryFormatNotice}</p>
                  {/if}
                  {#if glossaryFormat === 'typst'}
                    <div
                      class="chunk-glossary-typst-workspace"
                      class:mode-edit={glossaryMode === 'edit'}
                      class:mode-preview={glossaryMode === 'preview'}
                    >
                      <section class="chunk-glossary-typst-editor-pane">
                        <div class="chunk-glossary-pane-header">
                          <strong>Source</strong>
                          <span>{chunkIsQuestion ? "Full Typst answer source" : "Full Typst glossary source"}</span>
                        </div>
                        <textarea
                          class="chunk-glossary-editor chunk-glossary-editor-typst"
                          bind:this={glossaryTextareaEl}
                          placeholder={glossaryPlaceholder(glossaryFormat, chunkIsQuestion)}
                          value={glossaryDraft}
                          oninput={onGlossaryInput}
                          onblur={() => void flushGlossarySave()}
                          spellcheck="false"
                        ></textarea>
                      </section>
                      <section class="chunk-glossary-typst-preview-pane">
                        <div class="chunk-glossary-pane-header">
                          <strong>Preview</strong>
                          <span>
                            {#if glossaryTypstPreviewError}
                              Error
                            {:else if glossaryTypstPreviewLoading}
                              Rendering…
                            {:else if glossaryTypstPreviewDocument}
                              Up to date
                            {:else}
                              Waiting for content
                            {/if}
                          </span>
                        </div>
                        <div class="chunk-glossary-preview chunk-glossary-preview-typst">
                          {#if glossaryTypstPreviewError}
                            <p class="chunk-typst-preview-error">{glossaryTypstPreviewError}</p>
                          {/if}
                          {#if glossaryTypstPreviewDocument}
                            <div class="chunk-typst-preview-document">
                              {#each glossaryTypstPreviewDocument.pages as pageSvg, pageIndex (pageIndex)}
                                <div class="chunk-typst-preview-svg rendered" aria-label={`Typst preview page ${pageIndex + 1}`}>
                                  {@html pageSvg}
                                </div>
                              {/each}
                            </div>
                          {:else if !glossaryDraft.trim()}
                            <p class="chunk-glossary-preview-empty">Nothing yet.</p>
                          {:else if glossaryTypstPreviewLoading}
                            <p class="chunk-glossary-preview-empty">Rendering preview…</p>
                          {/if}
                        </div>
                      </section>
                    </div>
                  {:else if glossaryMode === 'edit'}
                    <textarea
                      class="chunk-glossary-editor"
                      bind:this={glossaryTextareaEl}
                      placeholder={glossaryPlaceholder(glossaryFormat, chunkIsQuestion)}
                      value={glossaryDraft}
                      oninput={onGlossaryInput}
                      onblur={() => void flushGlossarySave()}
                      spellcheck="true"
                    ></textarea>
                  {:else}
                    <div class="chunk-glossary-preview">
                      {#if glossaryDraft.trim()}
                        {@html glossaryMarkdownHtml}
                      {:else}
                        <p class="chunk-glossary-preview-empty">Nothing yet.</p>
                      {/if}
                    </div>
                  {/if}
                </div>
              {:else if chunkTab === 'ai'}
	                <div class="chunk-ai-pane">
	                  {#if chunkIsQuestion}
	                    <section class="chunk-ai-marking">
	                      <div class="chunk-ai-marking-header">
	                        <strong>Mark my answer</strong>
	                        <span>{chunkMarksDisplay ?? "—/?"}</span>
	                      </div>
	                      <div class="chunk-ai-marking-controls">
	                        <button
	                          class="chunk-ai-context-toggle"
	                          class:active={includeChunkAiVisuals}
	                          type="button"
	                          aria-pressed={includeChunkAiVisuals}
	                          onclick={() => includeChunkAiVisuals = !includeChunkAiVisuals}
	                          disabled={questionMarkingLoading || questionMarkingApplying}
	                        >
	                          {includeChunkAiVisuals ? "Include visuals: On" : "Include visuals: Off"}
	                        </button>
	                        <button
	                          class="chunk-ai-action chunk-ai-send"
	                          type="button"
	                          onclick={() => void runQuestionMarking()}
	                          disabled={questionMarkingLoading || questionMarkingApplying}
	                        >
	                          {questionMarkingLoading ? "Marking..." : "Mark answer"}
	                        </button>
	                      </div>
	                      {#if questionMarkingError}
	                        <p class="chunk-ai-rewrite-error">{questionMarkingError}</p>
	                      {/if}
	                      {#if questionMarkingSuggestion}
	                        <div class="chunk-ai-mark-suggestion">
	                          <p class="chunk-ai-rewrite-field">
	                            <span>Suggested mark:</span>
	                            {formatQuestionMarksValue(questionMarkingSuggestion.achieved_marks)}
	                            /
	                            {chunkView.chunk.available_marks == null ? "?" : chunkView.chunk.available_marks}
	                          </p>
	                          <div class="chunk-ai-rewrite-body-preview rendered">
	                            {@html questionMarkingSuggestionFeedbackHtml}
	                          </div>
	                          <div class="chunk-ai-rewrite-actions">
	                            <button
	                              class="chunk-ai-action chunk-ai-send"
	                              type="button"
	                              onclick={() => void applyQuestionMarkingSuggestion()}
	                              disabled={questionMarkingApplying}
	                            >
	                              {questionMarkingApplying ? "Applying..." : "Apply"}
	                            </button>
	                            <button
	                              class="chunk-ai-action chunk-ai-stop"
	                              type="button"
	                              onclick={() => {
	                                questionMarkingSuggestion = null;
	                                questionMarkingSuggestionIncludeVisuals = false;
	                              }}
	                              disabled={questionMarkingApplying}
	                            >
	                              Discard
	                            </button>
	                          </div>
	                        </div>
	                      {/if}
	                      <div class="chunk-ai-mark-history">
	                        <p class="chunk-ai-mark-history-title">Mark history</p>
	                        {#if questionMarkAttempts.length === 0}
	                          <p class="chunk-ai-mark-history-empty">No saved attempts yet.</p>
	                        {:else}
	                          <ul class="chunk-ai-mark-history-list">
	                            {#each questionMarkAttempts as attempt (attempt.id)}
	                              <li class="chunk-ai-mark-history-item">
	                                <div class="chunk-ai-mark-history-meta">
	                                  <span class="chunk-ai-mark-history-score">{formatQuestionAttemptScore(attempt)}</span>
	                                  <span class="chunk-ai-mark-history-source">{attempt.source.toUpperCase()}</span>
	                                  <span class="chunk-ai-mark-history-time">
	                                    {formatQuestionAttemptTimestamp(attempt.created_at)}
	                                  </span>
	                                </div>
	                                {#if attempt.feedback_md}
	                                  <div class="chunk-ai-mark-history-feedback rendered">
	                                    {@html renderQuestionAttemptFeedback(attempt.feedback_md)}
	                                  </div>
	                                {/if}
	                              </li>
	                            {/each}
	                          </ul>
	                        {/if}
	                      </div>
	                    </section>
	                  {/if}

	                  <div class="chunk-ai-rewrite chunk-ai-rewrite-compact">
                    {#if !chunkIsQuestion}
                      <div class="chunk-ai-context-row">
                        <button
                          class="chunk-ai-context-toggle"
                          class:active={includeChunkAiVisuals}
                          type="button"
                          aria-pressed={includeChunkAiVisuals}
                          onclick={() => includeChunkAiVisuals = !includeChunkAiVisuals}
                          disabled={chunkInkContextTranscribing}
                        >
                          {includeChunkAiVisuals ? "Include visuals: On" : "Include visuals: Off"}
                        </button>
                        <span>
                          {chunkHasInkContext
                            ? "Use chunk visuals for rewrite and chat context."
                            : "No chunk ink yet. Add ink notes to use visual context."}
                        </span>
                      </div>
                    {/if}
                    <div class="chunk-ai-rewrite-header">
	                      <strong>Rewrite</strong>
	                      <span>
	                        {chunkAiRewriteTab === 'body'
	                          ? 'Drafts title/body markdown'
	                          : chunkIsQuestion ? 'Drafts answer markdown' : 'Drafts glossary markdown'}
	                      </span>
	                    </div>
                    <div class="chunk-ai-rewrite-tabs" role="tablist" aria-label="Rewrite target">
                      <button
                        class="chunk-ai-rewrite-tab"
                        class:active={chunkAiRewriteTab === 'body'}
                        type="button"
                        role="tab"
                        aria-selected={chunkAiRewriteTab === 'body'}
                        onclick={() => chunkAiRewriteTab = 'body'}
                        disabled={chunkRewriteLoading || chunkRewriteApplying || glossaryRewriteLoading || glossaryRewriteApplying}
                      >
                        Body
                      </button>
                      <button
                        class="chunk-ai-rewrite-tab"
                        class:active={chunkAiRewriteTab === 'glossary'}
                        type="button"
                        role="tab"
                        aria-selected={chunkAiRewriteTab === 'glossary'}
                        onclick={() => chunkAiRewriteTab = 'glossary'}
	                        disabled={chunkRewriteLoading || chunkRewriteApplying || glossaryRewriteLoading || glossaryRewriteApplying}
	                      >
	                        {chunkIsQuestion ? "Answer" : "Glossary"}
	                      </button>
                    </div>

                    {#if chunkAiRewriteTab === 'body'}
                      <textarea
                        bind:value={chunkRewritePrompt}
                        class="chunk-ai-rewrite-input"
                        rows="2"
                        placeholder="Example: Make this shorter and clearer for revision."
                        disabled={chunkRewriteLoading || chunkRewriteApplying}
                      ></textarea>
                      <div class="chunk-ai-rewrite-actions">
                        <button
                          class="chunk-ai-action chunk-ai-send"
                          type="button"
                          onclick={runChunkRewritePrompt}
                          disabled={!chunkRewritePrompt.trim() || chunkRewriteLoading || chunkRewriteApplying}
                        >
                          {chunkRewriteLoading ? "Generating..." : "Generate draft"}
                        </button>
                      </div>
                      {#if chunkRewriteError}
                        <p class="chunk-ai-rewrite-error">{chunkRewriteError}</p>
                      {/if}
                      {#if chunkRewriteSuggestion}
                        <div class="chunk-ai-rewrite-preview">
                          <p class="chunk-ai-rewrite-field">
                            <span>Title:</span>
                            {chunkRewriteSuggestion.title ?? "No change"}
                          </p>
                          <p class="chunk-ai-rewrite-field">
                            <span>Body:</span>
                            {chunkRewriteSuggestion.body_markdown === null ? "No change" : "Updated"}
                          </p>
                          {#if chunkRewriteSuggestion.body_markdown !== null}
                            <div class="chunk-ai-rewrite-body-preview rendered">
                              {@html chunkRewriteBodyPreviewHtml}
                            </div>
                          {/if}
                          <div class="chunk-ai-rewrite-actions">
                            <button
                              class="chunk-ai-action chunk-ai-send"
                              type="button"
                              onclick={applyChunkRewriteSuggestion}
                              disabled={chunkRewriteApplying}
                            >
                              {chunkRewriteApplying ? "Applying..." : "Apply"}
                            </button>
                            <button
                              class="chunk-ai-action chunk-ai-stop"
                              type="button"
                              onclick={discardChunkRewriteSuggestion}
                              disabled={chunkRewriteApplying}
                            >
                              Discard
                            </button>
                          </div>
                        </div>
                      {/if}
                    {:else}
                      <textarea
                        bind:value={glossaryRewritePrompt}
                        class="chunk-ai-rewrite-input"
                        rows="2"
                        placeholder="Example: Make this glossary entry shorter and more exam-focused."
                        disabled={glossaryRewriteLoading || glossaryRewriteApplying}
                      ></textarea>
                      <div class="chunk-ai-rewrite-actions">
                        <button
                          class="chunk-ai-action chunk-ai-send"
                          type="button"
                          onclick={runGlossaryRewritePrompt}
                          disabled={!glossaryRewritePrompt.trim() || glossaryRewriteLoading || glossaryRewriteApplying}
                        >
                          {glossaryRewriteLoading ? "Generating..." : "Generate draft"}
                        </button>
                      </div>
                      {#if glossaryRewriteError}
                        <p class="chunk-ai-rewrite-error">{glossaryRewriteError}</p>
                      {/if}
                      {#if glossaryRewriteSuggestion}
                        <div class="chunk-ai-rewrite-preview">
                          <p class="chunk-ai-rewrite-field">
                            <span>{chunkIsQuestion ? "Answer" : "Glossary"}:</span>
                            {glossaryFormat === "typst" ? "Updated Typst source" : "Updated"}
                          </p>
                          {#if glossaryFormat === "typst"}
                            <div class="chunk-ai-rewrite-body-preview">
                              {#if glossaryRewriteTypstPreviewError}
                                <p class="chunk-typst-preview-error">{glossaryRewriteTypstPreviewError}</p>
                              {/if}
                              {#if glossaryRewriteTypstPreviewDocument}
                                <div class="chunk-typst-preview-document">
                                  {#each glossaryRewriteTypstPreviewDocument.pages as pageSvg, pageIndex (pageIndex)}
                                    <div class="chunk-typst-preview-svg rendered" aria-label={`Typst preview page ${pageIndex + 1}`}>
                                      {@html pageSvg}
                                    </div>
                                  {/each}
                                </div>
                              {:else if glossaryRewriteTypstPreviewLoading}
                                <p class="chunk-glossary-preview-empty">Rendering preview…</p>
                              {/if}
                            </div>
                          {:else}
                            <div class="chunk-ai-rewrite-body-preview rendered">
                              {@html glossaryRewriteMarkdownPreviewHtml}
                            </div>
                          {/if}
                          <div class="chunk-ai-rewrite-actions">
                            <button
                              class="chunk-ai-action chunk-ai-send"
                              type="button"
                              onclick={applyGlossaryRewriteSuggestion}
                              disabled={glossaryRewriteApplying}
                            >
                              {glossaryRewriteApplying ? "Applying..." : "Apply"}
                            </button>
                            <button
                              class="chunk-ai-action chunk-ai-stop"
                              type="button"
                              onclick={discardGlossaryRewriteSuggestion}
                              disabled={glossaryRewriteApplying}
                            >
                              Discard
                            </button>
                          </div>
                        </div>
                      {/if}
                    {/if}
                  </div>

                  <div class="chunk-ai-transcript" bind:this={chunkChatTranscript} use:chunkChatCodeCopy>
	                    {#if chunkChatMessages.length === 0}
	                      <div class="chunk-ai-empty">
	                        <p>Ask about this chunk.</p>
	                        <p>The AI sees the book title, chunk body, {chunkIsQuestion ? "answer entries" : "glossary entries"}, aliases, linked chunks, and references.</p>
	                      </div>
                    {:else}
                      {#each chunkChatMessages as message (message.id)}
                        <article
                          class="chunk-chat-message"
                          class:user={message.role === "user"}
                          class:assistant={message.role === "assistant"}
                        >
                          <div class="chunk-chat-meta">
                            <span>{message.role === "user" ? "You" : "AI"}</span>
                            {#if message.role === "assistant" && message.state === "streaming"}
                              <span>Streaming...</span>
                            {:else if message.role === "assistant" && message.state === "stopped"}
                              <span>Stopped</span>
                            {:else if message.role === "assistant" && message.state === "error"}
                              <span>Error</span>
                            {/if}
                          </div>
                          {#if message.role === "assistant" && message.state === "complete" && message.html}
                            <div class="chunk-chat-bubble assistant-bubble rendered">{@html message.html}</div>
                          {:else}
                            <div
                              class="chunk-chat-bubble"
                              class:user-bubble={message.role === "user"}
                              class:assistant-bubble={message.role === "assistant"}
                              class:is-error={message.role === "assistant" && message.state === "error"}
                            >
                              {#if message.imageDataUrls?.length}
                                <div class="chunk-chat-image-strip">
                                  {#each message.imageDataUrls as imageDataUrl, imageIndex (`${message.id}-chunk-${imageIndex}`)}
                                    <img class="chunk-chat-image" src={imageDataUrl} alt="Attached context" />
                                  {/each}
                                </div>
                              {/if}
                              {message.content || (message.role === "assistant" && message.state === "streaming" ? "Thinking..." : "")}
                            </div>
                          {/if}
                        </article>
                      {/each}
                    {/if}
                  </div>

                  <div class="chunk-ai-status-row">
                    {#if chatAttachmentTranscribing}
                      <span class="chunk-ai-status">Transcribing attached image...</span>
                    {/if}
                    {#if chunkInkContextTranscribing}
                      <span class="chunk-ai-status">Reading chunk visuals...</span>
                    {/if}
                    {#if chunkChatLoadingContext}
                      <span class="chunk-ai-status">Preparing context...</span>
                    {/if}
                    {#if chunkChatError}
                      <span class="chunk-ai-error">{chunkChatError}</span>
                    {/if}
                  </div>

                  <div class="chunk-ai-composer">
                    {#if pendingChatAttachment}
                      <div class="chunk-chat-attachment-preview">
                        <img src={pendingChatAttachment.imageDataUrl} alt="Selected area attachment" />
                        <div class="chunk-chat-attachment-meta">
                          <strong>Selection attached</strong>
                          <span>Page {pendingChatAttachment.pageNumber}</span>
                        </div>
                        <button
                          class="chunk-ai-action chunk-ai-stop"
                          type="button"
                          onclick={clearPendingChatAttachment}
                          disabled={chunkChatStreaming || chunkChatLoadingContext}
                        >
                          Remove
                        </button>
                      </div>
                    {/if}
                    <textarea
                      bind:value={chunkChatDraft}
                      class="chunk-ai-input"
                      rows="3"
                      placeholder="Ask about this chunk..."
                      onkeydown={onChunkChatKeydown}
                      disabled={chunkChatStreaming || chunkChatLoadingContext}
                    ></textarea>
                    <div class="chunk-ai-actions">
                      {#if chunkChatStreaming}
                        <button class="chunk-ai-action chunk-ai-stop" type="button" onclick={stopChunkChat}>
                          Stop
                        </button>
                      {/if}
                      <button
                        class="chunk-ai-action chunk-ai-send"
                        type="button"
                        onclick={sendChunkChatMessage}
                        disabled={chunkChatStreaming || chunkChatLoadingContext || getActiveChatChunkId() == null || (!chunkChatDraft.trim() && !pendingChatAttachment)}
                      >
                        Send
                      </button>
                    </div>
                  </div>
                </div>
              {/if}
            </div>

          </div>
          <button
            class="chunk-nav-btn chunk-nav-next"
            type="button"
            onclick={() => void navigateChunk(1)}
            disabled={!canOpenNextChunk || chunkNavigationBusy}
            aria-label="Next chunk"
            title="Next chunk (Right Arrow)"
          >
            <svg viewBox="0 0 16 16" fill="none" width="15" height="15">
              <path d="M6.4 3.4L10.8 8l-4.4 4.6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          </button>
        </div>
      {/if}

      {#if chunkPeek}
        <ChunkPeek
          chunkId={chunkPeek.chunkId}
          anchorRect={chunkPeek.anchorRect}
          colours={CHUNK_COLOURS}
          onEnsureBody={ensureChunkFormattedBody}
          onOpen={onPeekOpen}
          onClose={onPeekClose}
        />
      {/if}

      <div class="controls">
        <!-- Prev -->
        <button
          class="chevron"
          onclick={prevPage}
          disabled={currentPage <= 1 || rendering}
          aria-label={isNotebookDocument() ? "Previous canvas" : "Previous page"}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="15 18 9 12 15 6"/>
          </svg>
        </button>

        <div class="page-indicator">
          <span class="page-kind">{isNotebookDocument() ? "Canvas" : "Page"}</span>
          <input
            class="page-input"
            type="text"
            inputmode="numeric"
            pattern="[0-9]*"
            aria-label={isNotebookDocument() ? "Canvas number" : "Page number"}
            disabled={rendering}
            value={pageInputValue}
            onfocus={(event) => {
              pageInputFocused = true;
              event.currentTarget.select();
            }}
            oninput={(event) => pageInputValue = event.currentTarget.value.replace(/\D/g, '')}
            onblur={async () => {
              pageInputFocused = false;
              await commitPageInput();
            }}
            onkeydown={handlePageInputKeydown}
          />
          {#if totalPages > 0}
            <span class="page-total">/ {totalPages}</span>
          {/if}
        </div>

        <!-- Next -->
        <button
          class="chevron"
          onclick={nextPage}
          disabled={isNotebookDocument() ? rendering : currentPage >= totalPages || rendering}
          aria-label={isNotebookDocument() ? "Next canvas" : "Next page"}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="9 18 15 12 9 6"/>
          </svg>
        </button>

        <div class="divider"></div>

        <!-- Undo -->
        <button class="ink-btn" onclick={undoStroke} disabled={!pageStrokeTransformUndo && strokes.length === 0 && pageGraphUndoStack.length === 0} aria-label="Undo stroke">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 14 4 9l5-5"/>
            <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>
          </svg>
        </button>

        <!-- Redo -->
        <button class="ink-btn" onclick={redoStroke} disabled={!pageStrokeTransformRedo && redoStack.length === 0 && pageGraphRedoStack.length === 0} aria-label="Redo stroke">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M15 14l5-5-5-5"/>
            <path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>
          </svg>
        </button>

        <div class="divider"></div>

        <!-- Draw -->
        <div class="pen-tool">
          <button
            class="tool-btn"
            class:active={mode === 'draw'}
            onclick={activateDrawTool}
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
          {#if showPenOptions}
            <div class="pen-popout" transition:fade={{ duration: 140 }}>
              <div class="pen-popout-header">
                <span class="pen-popout-title">Pen</span>
                <span class="pen-preview" style={`--pen-preview-colour: ${penColour}; --pen-preview-size: ${penThickness}px;`}>
                  <span class="pen-preview-dot"></span>
                </span>
              </div>
              <label class="pen-slider-group" for="pen-thickness">
                <span>Thickness</span>
                <span>{penThickness.toFixed(1)} px</span>
              </label>
              <input
                id="pen-thickness"
                class="pen-slider"
                type="range"
                min="1"
                max="12"
                step="0.5"
                value={penThickness}
                oninput={(e) => penThickness = Number((e.currentTarget as HTMLInputElement).value)}
              />
              <div class="pen-colours" aria-label="Pen colours">
                {#each PEN_COLOURS as colour}
                  <button
                    class="colour-swatch"
                    class:selected={penColour === colour}
                    type="button"
                    onclick={() => penColour = colour}
                    aria-label={`Select ${colour} pen`}
                    aria-pressed={penColour === colour}
                    style={`--swatch-colour: ${colour};`}
                  ></button>
                {/each}
              </div>
            </div>
          {/if}
        </div>

        <div class="shape-tool">
          <button
            class="tool-btn"
            class:active={mode === 'shape'}
            onclick={activateShapeTool}
            aria-label="Shape"
            aria-pressed={mode === 'shape'}
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="8" cy="8" r="3.2"/>
              <line x1="13" y1="16" x2="21" y2="16"/>
              <line x1="17" y1="12" x2="17" y2="20"/>
            </svg>
          </button>
          {#if showShapeOptions}
            <div class="shape-popout" transition:fade={{ duration: 140 }}>
              {#each SHAPE_OPTIONS as option}
                <button
                  class="shape-option"
                  class:active={shapeKind === option.value}
                  type="button"
                  onclick={() => shapeKind = option.value}
                  aria-pressed={shapeKind === option.value}
                >
                  <span class="shape-option-title">{option.label}</span>
                  <span class="shape-option-hint">{option.hint}</span>
                </button>
              {/each}
            </div>
          {/if}
        </div>

        <button
          class="tool-btn"
          class:active={pendingGraphPlacement?.scope === "page"}
          onclick={activateGraphTool}
          aria-label="Graph"
          aria-pressed={pendingGraphPlacement?.scope === "page"}
          title="Compose a graph"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
            <path d="M4 20V4"/>
            <path d="M4 20H20"/>
            <path d="M6 16c1.6-2.8 3-4.2 4.6-4.2 1.9 0 2.7 2.7 4.5 2.7 1.4 0 2.8-1.4 4.9-5.5"/>
          </svg>
        </button>

        <div class="paper-tool">
          <button
            class="tool-btn"
            class:active={showPaperOptions}
            type="button"
            onclick={togglePaperOptions}
            aria-label="Paper options"
            aria-pressed={showPaperOptions}
            title="Paper options"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
              <rect x="5" y="3.5" width="14" height="17" rx="2"/>
              <path d="M9 8h6"/>
              <path d="M9 12h6"/>
              <path d="M9 16h4"/>
            </svg>
          </button>
          {#if showPaperOptions}
            <div class="paper-popout" transition:fade={{ duration: 140 }}>
              <div class="paper-popout-header">
                <span class="paper-popout-title">Paper</span>
                <span class="paper-sample" style={`--paper-background-colour: ${paperSettings.backgroundColour}; --paper-pattern-colour: ${paperPatternColour(paperPatternAlpha())};`}>
                  <span
                    class="paper-sample-pattern"
                    class:paper-dotted={paperSettings.pattern === 'dotted'}
                    class:paper-grid={paperSettings.pattern === 'grid'}
                    class:paper-line={paperSettings.pattern === 'line'}
                  ></span>
                </span>
              </div>
              <div class="paper-pattern-options" aria-label="Paper pattern">
                {#each PAPER_PATTERN_OPTIONS as option}
                  <button
                    class="paper-option"
                    class:active={paperSettings.pattern === option.value}
                    type="button"
                    onclick={() => updatePaperSettings({ pattern: option.value })}
                    aria-pressed={paperSettings.pattern === option.value}
                  >
                    {option.label}
                  </button>
                {/each}
              </div>
              <label class="pen-slider-group" for="paper-spacing">
                <span>Spacing</span>
                <span>{paperSettings.spacing} px</span>
              </label>
              <input
                id="paper-spacing"
                class="pen-slider"
                type="range"
                min={PAPER_SPACING_MIN}
                max={PAPER_SPACING_MAX}
                step="1"
                value={paperSettings.spacing}
                oninput={(e) => updatePaperSettings({ spacing: Number((e.currentTarget as HTMLInputElement).value) })}
              />
              <div class="paper-colours" aria-label="Paper colours">
                {#each PAPER_BACKGROUND_SWATCHES as colour}
                  <button
                    class="paper-colour-swatch"
                    class:selected={paperSettings.backgroundColour === colour}
                    type="button"
                    onclick={() => updatePaperSettings({ backgroundColour: colour })}
                    aria-label={`Select ${colour} paper`}
                    aria-pressed={paperSettings.backgroundColour === colour}
                    style={`--paper-swatch-colour: ${colour};`}
                  ></button>
                {/each}
                <label
                  class="paper-colour-custom"
                  class:selected={!PAPER_BACKGROUND_SWATCHES.includes(paperSettings.backgroundColour)}
                  aria-label="Custom paper colour"
                  style={`--paper-swatch-colour: ${paperSettings.backgroundColour};`}
                >
                  <input
                    type="color"
                    value={paperSettings.backgroundColour}
                    oninput={(e) => updatePaperSettings({ backgroundColour: (e.currentTarget as HTMLInputElement).value })}
                  />
                </label>
              </div>
            </div>
          {/if}
        </div>

        <!-- Erase -->
        <button
          class="tool-btn"
          class:active={mode === 'erase'}
          onclick={activateEraseTool}
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
          onclick={activateSelectTool}
          aria-label="Select"
          aria-pressed={mode === 'select'}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M5 3l14 9-7 1-4 7-3-17z"/>
          </svg>
        </button>

        <button
          class="ink-btn"
          onclick={() => void copySelectedPageInk()}
          disabled={mode !== 'select' || selectedStrokes.size === 0}
          aria-label="Copy selected ink"
          title="Copy selected ink (Ctrl/Cmd+C)"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="9" y="9" width="11" height="11" rx="2"/>
            <path d="M15 5h-9a2 2 0 0 0-2 2v9"/>
          </svg>
        </button>

        <button
          class="ink-btn"
          onclick={() => void pastePageInk()}
          disabled={mode !== 'select' || !hasPageInkClipboard}
          aria-label="Paste copied ink"
          title="Paste copied ink (Ctrl/Cmd+V)"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="6" y="4" width="12" height="16" rx="2"/>
            <path d="M9 4.5V3h6v1.5"/>
            <path d="M9 9h6"/>
          </svg>
        </button>

        <!-- AI -->
        <button
          class="tool-btn ai-btn"
          class:active={viewerAiOpen}
          onclick={onAiClick}
          disabled={aiWorking}
          aria-label={isNotebookDocument() ? "Open AI chat for this canvas" : "Open AI chat for this page"}
          title={isNotebookDocument() ? "Open AI chat for this canvas" : "Open AI chat for this page"}
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

  {/if}
  <GraphComposerModal
    open={graphModalOpen}
    initialGraph={graphModalInitialGraph}
    title={graphModalSource === "glossary" ? "Insert graph in glossary" : "Compose graph"}
    showGlossaryAction={!!chunkView}
    onCancel={closeGraphComposer}
    onInsertCanvas={onGraphModalInsertCanvas}
    onInsertGlossary={onGraphModalInsertGlossary}
  />
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

  .error {
    color: #c00;
    font-size: 0.9em;
    margin: 0.5rem 0;
  }

  /* â”€â”€ Viewer â”€â”€ */
  .viewer {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  /* â”€â”€ Infinite canvas â”€â”€ */
  .infinite-canvas {
    position: relative;
    flex: 1;
    overflow: hidden;
    min-height: 0;
    background: var(--paper-background-colour, #e8e8e8);
    cursor: crosshair;
    touch-action: none;
  }

  .infinite-canvas.notebook-surface {
    background: var(--paper-background-colour, #f4f7fb);
  }

  .infinite-canvas.mode-erase { cursor: cell; }
  .infinite-canvas.mode-shape { cursor: crosshair; }
  .infinite-canvas.mode-select { cursor: default; }

  .viewer-ai-panel {
    position: absolute;
    top: 86px;
    right: 14px;
    bottom: 86px;
    width: min(430px, calc(100% - 28px));
    z-index: 72;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    border: 1px solid #d6dde9;
    border-radius: 14px;
    background: rgba(250, 252, 255, 0.98);
    box-shadow: 0 18px 44px rgba(15, 23, 42, 0.18);
    backdrop-filter: blur(6px);
  }

  .viewer-ai-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 10px;
    padding: 11px 13px 10px;
    border-bottom: 1px solid rgba(203, 213, 225, 0.7);
    background: rgba(255, 255, 255, 0.86);
  }

  .viewer-ai-kicker {
    margin: 0;
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #667085;
    font-family: Inter, system-ui, sans-serif;
  }

  .viewer-ai-context {
    margin: 4px 0 0;
    font-size: 12px;
    line-height: 1.42;
    color: #334155;
    font-family: Inter, system-ui, sans-serif;
  }

  .viewer-ai-close {
    width: 30px;
    height: 30px;
    border-radius: 8px;
    border: 1px solid #d2dae7;
    background: #f8fafc;
    color: #4b5563;
    font-size: 18px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    flex-shrink: 0;
  }

  .viewer-ai-close:hover:not(:disabled) {
    background: #edf2f8 !important;
  }

  @media (max-width: 720px) {
    .viewer-ai-panel {
      top: 94px;
      right: 8px;
      left: 8px;
      bottom: 82px;
      width: auto;
    }
  }

  .layer {
    position: absolute;
    top: 0;
    left: 0;
    /* width/height set in JS to clientWidth/clientHeight in CSS px,
       but the bitmap is dpr-scaled â€” keep CSS size at 100% */
    width: 100%;
    height: 100%;
  }

  .layer-grid { pointer-events: none; }
  .layer-pdf  { pointer-events: none; }
  .layer-chunk { pointer-events: none; }
  .layer-dry  { pointer-events: none; }
  /* .layer-wet receives all pointer events â€” no overrides needed */

  .page-kind {
    color: #64748b;
    font-size: 0.78rem;
    font-weight: 700;
  }

  /* â”€â”€ Chunk note sheet â”€â”€ */
  .chunk-sheet-backdrop {
    position: fixed;
    inset: 0;
    z-index: 120;
    display: flex;
    padding: 0;
    background: #ffffff;
  }

  .chunk-nav-btn {
    position: absolute;
    bottom: 14px;
    z-index: 24;
    width: 34px;
    height: 34px;
    border-radius: 10px;
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 20%, #cbd5e1);
    background: color-mix(in oklch, var(--chunk-tint) 62%, white);
    color: #64748b;
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0.62;
    backdrop-filter: blur(4px);
    transition: background 0.14s, color 0.14s, border-color 0.14s, opacity 0.14s, transform 0.14s;
  }

  .chunk-nav-prev { left: 10px; }
  .chunk-nav-next { right: 10px; }

  .chunk-nav-btn:hover:not(:disabled),
  .chunk-nav-btn:focus-visible {
    background: color-mix(in oklch, var(--chunk-accent) 18%, white);
    border-color: color-mix(in oklch, var(--chunk-accent) 55%, white);
    color: var(--chunk-accent);
    opacity: 1;
    transform: translateY(-1px);
    outline: none;
  }

  .chunk-nav-btn:disabled {
    opacity: 0.22;
    cursor: default;
  }

  .chunk-nav-btn svg {
    width: 15px;
    height: 15px;
  }

  .chunk-sheet {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: row;
    overflow: hidden;
    isolation: isolate;
    background: #fff;
    border: none;
    border-radius: 0;
    box-shadow: none;
  }

  /* Left panel */
  .chunk-panel-left {
    position: relative;
    z-index: 1;
    width: var(--chunk-panel-left-width, 360px);
    flex-shrink: 0;
    border-right: 1px solid rgba(0, 0, 0, 0.07);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: #f9fafb;
    border-left: 3px solid var(--chunk-accent);
  }

  .chunk-panel-resizer {
    position: relative;
    width: 10px;
    padding: 0;
    border: none;
    flex-shrink: 0;
    cursor: col-resize;
    background: transparent;
    appearance: none;
    z-index: 3;
    touch-action: none;
  }

  .chunk-panel-resizer::before {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    transform: translateX(-50%);
    background: rgba(148, 163, 184, 0.42);
    transition: background 0.12s;
  }

  .chunk-panel-resizer:hover::before,
  .chunk-panel-resizer:focus-visible::before {
    background: color-mix(in oklch, var(--chunk-accent) 72%, white);
  }

  .chunk-panel-resizer:focus-visible {
    outline: none;
  }

  .cpl-meta {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 10px 12px 10px 10px;
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
    flex-shrink: 0;
  }

  .chunk-badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    background: var(--chunk-tint);
    color: var(--chunk-accent);
    line-height: 15px;
  }

  .chunk-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .chunk-status-dot.status-complete      { background: oklch(0.62 0.14 145); }
  .chunk-status-dot.status-in_progress   { background: oklch(0.72 0.13 65); }
  .chunk-status-dot.status-incomplete    { border: 1.5px solid #9ca3af; }

  .chunk-index-label {
    font-size: 10.5px;
    line-height: 1;
    color: #6b7280;
    letter-spacing: 0.03em;
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
  }

  .chunk-close-btn {
    margin-left: auto;
    width: 26px;
    height: 26px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 6px;
    color: #6b7280;
    flex-shrink: 0;
  }

  .chunk-close-btn:hover { background: #f3f4f6; color: #374151; }

  .cpl-edit-status {
    padding: 7px 12px 0 10px;
    min-height: 20px;
    font-size: 11px;
    color: #6b7280;
    font-family: Inter, system-ui, sans-serif;
    flex-shrink: 0;
  }

  .cpl-edit-status-error {
    color: #b91c1c;
  }

  .cpl-title-input {
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid rgba(156, 163, 175, 0.52);
    background: #fff;
    color: #111827;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 14px;
    font-style: italic;
    line-height: 1.35;
  }

  .cpl-title-input:focus,
  .cpl-body-editor:focus {
    outline: none;
    border-color: color-mix(in oklch, var(--chunk-accent) 56%, white);
    box-shadow: 0 0 0 3px color-mix(in oklch, var(--chunk-accent) 16%, transparent);
  }

  .cpl-body-editor {
    flex: 1;
    margin: 0 10px 12px;
    padding: 9px 10px;
    border-radius: 8px;
    border: 1px solid rgba(156, 163, 175, 0.52);
    background: #fff;
    color: #4b5563;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 12.5px;
    line-height: 1.62;
    resize: none;
    min-height: 170px;
    font-variant-numeric: lining-nums slashed-zero;
    font-feature-settings: "zero" 1;
  }

  .cpl-title {
    padding: 9px 12px 2px 10px;
    font-size: 13.5px;
    font-style: italic;
    font-weight: 400;
    color: #111827;
    font-family: Georgia, 'Times New Roman', serif;
    flex-shrink: 0;
  }

  .cpl-title-display {
    display: block;
    width: calc(100% - 20px);
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: text;
  }

  .cpl-title-display:hover,
  .cpl-title-display:focus-visible {
    outline: none;
    background: rgba(255, 255, 255, 0.75);
    border-color: rgba(156, 163, 175, 0.45);
  }

  .cpl-title-add-btn {
    margin: 0 10px 8px;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px dashed rgba(156, 163, 175, 0.65);
    background: rgba(255, 255, 255, 0.7);
    color: #4b5563;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 13px;
    font-style: italic;
    text-align: left;
  }

  .cpl-title-add-btn:hover {
    border-color: color-mix(in oklch, var(--chunk-accent) 55%, white);
    color: #111827;
  }

  .cpl-question-meta {
    margin: 0 10px 8px;
    padding: 8px 9px;
    border-radius: 8px;
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, #cbd5e1);
    background: color-mix(in oklch, var(--chunk-tint) 66%, white);
    display: flex;
    flex-direction: column;
    gap: 7px;
    flex-shrink: 0;
  }

  .cpl-question-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .cpl-question-label {
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: #6b7280;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-question-value {
    font-size: 12px;
    color: #111827;
    font-family: Inter, system-ui, sans-serif;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
  }

  .cpl-question-value-button {
    min-height: 24px;
    padding: 0 6px;
    border: 1px dashed color-mix(in oklch, var(--chunk-accent) 32%, #cbd5e1);
    border-radius: 7px;
    background: #fff;
    color: #111827;
    cursor: text;
  }

  .cpl-question-value-button:hover {
    border-color: color-mix(in oklch, var(--chunk-accent) 52%, #cbd5e1);
  }

  .cpl-question-marks-editor {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }

  .cpl-question-marks-prefix {
    color: #111827;
  }

  .cpl-question-marks-input {
    width: 44px;
    height: 24px;
    padding: 0 5px;
    border-radius: 6px;
    border: 1px solid rgba(148, 163, 184, 0.7);
    background: #fff;
    color: #111827;
    font-size: 12px;
    font-family: Inter, system-ui, sans-serif;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
    text-align: center;
  }

  .cpl-question-marks-input:focus {
    outline: none;
    border-color: color-mix(in oklch, var(--chunk-accent) 56%, white);
    box-shadow: 0 0 0 3px color-mix(in oklch, var(--chunk-accent) 16%, transparent);
  }

  .cpl-question-entry {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .cpl-question-input {
    min-width: 0;
    flex: 1;
    height: 30px;
    padding: 0 9px;
    border-radius: 7px;
    border: 1px solid rgba(148, 163, 184, 0.62);
    background: #fff;
    color: #111827;
    font-size: 12px;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-question-input:focus {
    outline: none;
    border-color: color-mix(in oklch, var(--chunk-accent) 56%, white);
    box-shadow: 0 0 0 3px color-mix(in oklch, var(--chunk-accent) 16%, transparent);
  }

  .cpl-question-save,
  .cpl-question-source {
    height: 30px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 11.5px;
    font-weight: 600;
    font-family: Inter, system-ui, sans-serif;
    transition: background 0.12s, border-color 0.12s, transform 0.12s;
  }

  .cpl-question-save {
    background: var(--chunk-accent);
    color: #fff;
  }

  .cpl-question-source {
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 32%, #cbd5e1);
    background: #fff;
    color: #334155;
  }

  .cpl-question-save:hover:not(:disabled),
  .cpl-question-source:hover:not(:disabled) {
    transform: translateY(-1px);
  }

  .cpl-question-source-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .cpl-question-source-count {
    font-size: 10.5px;
    color: #64748b;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-question-error {
    margin: 0;
    color: #b91c1c;
    font-size: 11px;
    line-height: 1.4;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-linked {
    margin: 0 10px 8px;
    border-radius: 8px;
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, #cbd5e1);
    background: color-mix(in oklch, var(--chunk-tint) 62%, white);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    flex-shrink: 0;
  }

  .cpl-linked-toggle {
    width: 100%;
    min-height: 34px;
    padding: 7px 10px;
    display: flex;
    align-items: center;
    gap: 8px;
    background: transparent;
    color: inherit;
    text-align: left;
  }

  .cpl-linked-toggle:hover {
    background: color-mix(in oklch, var(--chunk-tint) 24%, transparent);
  }

  .cpl-linked-toggle-label {
    font-size: 10px;
    line-height: 1;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: #6b7280;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-linked-toggle-hint {
    margin-left: auto;
    font-size: 11px;
    line-height: 1;
    color: #4b5563;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-linked-toggle-chevron {
    color: #6b7280;
    flex-shrink: 0;
    transition: transform 0.12s ease;
  }

  .cpl-linked-toggle-chevron.open {
    transform: rotate(180deg);
  }

  .cpl-linked-content {
    border-top: 1px solid rgba(148, 163, 184, 0.28);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .cpl-linked-empty,
  .cpl-linked-error {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.45;
    color: #6b7280;
    font-family: Georgia, 'Times New Roman', serif;
    font-style: italic;
  }

  .cpl-linked-error {
    color: #b91c1c;
  }

  .cpl-linked-card {
    width: 100%;
    border-radius: 7px;
    border: 1px solid color-mix(in oklch, var(--linked-chunk-accent) 28%, #cbd5e1);
    background: #fff;
    padding: 8px 9px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .cpl-linked-meta {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }

  .cpl-linked-badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 9px;
    line-height: 14px;
    font-weight: 700;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    background: var(--linked-chunk-tint);
    color: var(--linked-chunk-accent);
    flex-shrink: 0;
    font-family: Inter, system-ui, sans-serif;
  }

  .cpl-linked-title {
    min-width: 0;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 12.5px;
    line-height: 1.35;
    color: #111827;
    font-style: italic;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cpl-linked-body {
    max-height: 82px;
    overflow-y: auto;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 11.5px;
    line-height: 1.5;
    color: #4b5563;
  }

  .cpl-linked-body-muted {
    color: #9ca3af;
    font-style: italic;
  }

  .cpl-linked-body :global(p) {
    margin: 0;
  }

  .cpl-linked-body :global(p + p) {
    margin-top: 0.5em;
  }

  .cpl-linked-actions {
    display: flex;
    justify-content: flex-end;
  }

  .cpl-linked-open-btn {
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    background: var(--linked-chunk-accent);
    color: #fff;
    font-size: 11px;
    font-weight: 600;
    font-family: Inter, system-ui, sans-serif;
    transition: filter 0.12s;
  }

  .cpl-linked-open-btn:hover {
    filter: brightness(1.05);
  }

  .cpl-body {
    flex: 1;
    overflow-y: auto;
    padding: 7px 12px 14px 10px;
    font-size: 12px;
    line-height: 1.65;
    color: #4b5563;
    font-family: Georgia, 'Times New Roman', serif;
    font-variant-numeric: lining-nums slashed-zero;
    font-feature-settings: "zero" 1;
  }

  .cpl-body-display {
    margin: 0 10px 12px;
    padding: 9px 10px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: #fff;
    cursor: text;
  }

  .cpl-body-display:hover,
  .cpl-body-display:focus-visible {
    outline: none;
    border-color: rgba(156, 163, 175, 0.45);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.85);
  }

  .cpl-body:empty::after {
    content: 'No text extracted yet.';
    color: #9ca3af;
    font-style: italic;
  }

  .cpl-body :global(p) {
    margin: 0;
  }

  .cpl-body :global(a.chunk-xref) {
    color: var(--chunk-accent);
    text-decoration: none;
    border-bottom: 1px dashed color-mix(in oklch, var(--chunk-accent) 55%, transparent);
    cursor: pointer;
    padding: 0 1px;
    border-radius: 2px;
  }

  .cpl-body :global(a.chunk-xref:hover),
  .cpl-body :global(a.chunk-xref:focus-visible) {
    background: var(--chunk-tint);
    outline: none;
    border-bottom-style: solid;
  }

  .cpl-body :global(p + p) {
    margin-top: 0.72em;
  }

  .cpl-body :global(h1),
  .cpl-body :global(h2),
  .cpl-body :global(h3),
  .cpl-body :global(h4),
  .cpl-body :global(h5),
  .cpl-body :global(h6) {
    margin: 0.9em 0 0;
    color: #111827;
    font-weight: 600;
    line-height: 1.3;
  }

  .cpl-body :global(h1:first-child),
  .cpl-body :global(h2:first-child),
  .cpl-body :global(h3:first-child),
  .cpl-body :global(h4:first-child),
  .cpl-body :global(h5:first-child),
  .cpl-body :global(h6:first-child) {
    margin-top: 0;
  }

  .cpl-body :global(h1) { font-size: 1.28em; }
  .cpl-body :global(h2) { font-size: 1.18em; }
  .cpl-body :global(h3) { font-size: 1.1em; }
  .cpl-body :global(h4),
  .cpl-body :global(h5),
  .cpl-body :global(h6) { font-size: 1em; }

  .cpl-body :global(ol),
  .cpl-body :global(ul) {
    margin: 0.55em 0 0;
    padding-left: 1.15rem;
  }

  .cpl-body :global(li + li) {
    margin-top: 0.22rem;
  }

  .cpl-body :global(.chunk-math-display) {
    margin: 0.6em 0;
    overflow-x: auto;
    overflow-y: hidden;
    -webkit-overflow-scrolling: touch;
  }

  .cpl-body :global(.chunk-math-display + .chunk-math-display) {
    margin-top: 0.2em;
  }

  .cpl-body :global(.katex-display) {
    margin: 0;
  }

  .cpl-body :global(code) {
    padding: 0.05em 0.32em;
    border-radius: 4px;
    background: rgba(15, 23, 42, 0.06);
    font-size: 0.92em;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }

  .cpl-body :global(pre) {
    margin: 0.65em 0 0;
    padding: 0.65em 0.75em;
    border-radius: 8px;
    background: rgba(15, 23, 42, 0.05);
    overflow-x: auto;
  }

  .cpl-body :global(pre code) {
    padding: 0;
    background: transparent;
  }

  .cpl-body :global(a) {
    color: var(--chunk-accent);
    text-decoration-thickness: 0.08em;
    text-underline-offset: 0.12em;
  }

  /* Right panel */
  .chunk-panel-right {
    position: relative;
    z-index: 2;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: visible;
  }

  .chunk-tab-bar {
    display: flex;
    align-items: center;
    gap: 2px;
    border-bottom: 1px solid rgba(0, 0, 0, 0.07);
    background: #fff;
    flex-shrink: 0;
  }

  .chunk-tab {
    height: 36px;
    padding: 0 14px;
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    font-weight: 400;
    color: #6b7280;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    cursor: pointer;
    transition: color 0.12s;
    user-select: none;
  }

  .chunk-tab.active {
    font-weight: 600;
    color: var(--chunk-accent);
    border-bottom-color: var(--chunk-accent);
  }

  .chunk-tab-todo {
    opacity: 0.4;
    cursor: default;
    pointer-events: none;
  }

  .chunk-tab-settings {
    margin-left: auto;
    margin-right: 10px;
    height: 28px;
    padding: 0 11px;
    border-radius: 999px;
    border: 1px solid rgba(148, 163, 184, 0.55);
    background: #fff;
    color: #475569;
    font-size: 11px;
    font-weight: 700;
    font-family: Inter, system-ui, sans-serif;
    cursor: pointer;
    transition: background 0.12s ease, border-color 0.12s ease, color 0.12s ease, transform 0.12s ease;
  }

  .chunk-tab-settings:hover:not(:disabled) {
    transform: translateY(-1px);
    border-color: color-mix(in oklch, var(--chunk-accent) 36%, white);
    background: color-mix(in oklch, var(--chunk-accent) 8%, white);
    color: color-mix(in oklch, var(--chunk-accent) 72%, black);
  }

  .chunk-sheet-surface {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    background-color: var(--paper-background-colour, #f4f7fb);
    touch-action: none;
  }

  .chunk-sheet-surface.paper-dotted {
    background-image:
      radial-gradient(circle at center, var(--paper-pattern-colour, rgba(15, 23, 42, 0.2)) 1.3px, transparent 1.45px);
    background-size: var(--paper-grid-size, 40px) var(--paper-grid-size, 40px);
    background-position: var(--paper-dot-offset-x, 0) var(--paper-dot-offset-y, 0);
  }

  .chunk-sheet-surface.paper-grid {
    background-image:
      linear-gradient(var(--paper-pattern-colour, rgba(15, 23, 42, 0.15)) 1px, transparent 1px),
      linear-gradient(90deg, var(--paper-pattern-colour, rgba(15, 23, 42, 0.15)) 1px, transparent 1px);
    background-size: var(--paper-grid-size, 40px) var(--paper-grid-size, 40px);
    background-position: var(--paper-grid-offset-x, 0) var(--paper-grid-offset-y, 0);
  }

  .chunk-sheet-surface.paper-line {
    background-image:
      linear-gradient(var(--paper-pattern-colour, rgba(15, 23, 42, 0.15)) 1px, transparent 1px);
    background-size: var(--paper-grid-size, 40px) var(--paper-grid-size, 40px);
    background-position: 0 var(--paper-grid-offset-y, 0);
  }

  .chunk-sheet-surface.mode-erase { cursor: cell; }
  .chunk-sheet-surface.mode-shape { cursor: crosshair; }
  .chunk-sheet-surface.mode-select { cursor: default; }

  .chunk-layer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    z-index: 1;
  }

  .chunk-layer-dry { pointer-events: none; z-index: 1; }
  .chunk-layer-wet { z-index: 2; }

  .chunk-ink-bar {
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 38px;
    display: flex;
    align-items: center;
    padding: 0 10px;
    gap: 4px;
    background: rgba(255, 255, 255, 0.92);
    backdrop-filter: blur(6px);
    border-top: 1px solid rgba(0, 0, 0, 0.07);
    z-index: 20;
    overflow: visible;
  }

  .chunk-ink-bar .divider {
    width: 1px;
    height: 20px;
    background: rgba(15, 23, 42, 0.14);
    margin: 0 2px;
  }

  .chunk-ink-bar .ink-btn,
  .chunk-ink-bar .tool-btn,
  .chunk-ink-bar .zoom-btn {
    width: 30px;
    height: 30px;
    color: #6b7280;
  }

  .chunk-ink-bar .ink-btn:hover:not(:disabled),
  .chunk-ink-bar .tool-btn:hover:not(:disabled),
  .chunk-ink-bar .zoom-btn:hover:not(:disabled) {
    background: #f3f4f6 !important;
    color: #374151;
  }

  .chunk-ink-bar .tool-btn.active {
    background: color-mix(in oklch, var(--chunk-accent) 14%, white) !important;
    color: var(--chunk-accent);
  }

  .chunk-ink-bar .ink-btn:disabled,
  .chunk-ink-bar .zoom-btn:disabled {
    opacity: 0.4;
  }

  .chunk-ink-bar .zoom-level {
    min-width: 4.2ch;
    font-size: 0.8em;
  }

  .chunk-ink-bar .pen-popout {
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    box-shadow: 0 14px 34px color-mix(in oklch, var(--chunk-accent) 14%, transparent);
    z-index: 120;
  }

  .chunk-ink-bar .chunk-shape-popout {
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    box-shadow: 0 14px 34px color-mix(in oklch, var(--chunk-accent) 14%, transparent);
    z-index: 120;
  }

  .chunk-ink-bar .chunk-paper-popout {
    border: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    box-shadow: 0 14px 34px color-mix(in oklch, var(--chunk-accent) 14%, transparent);
    z-index: 120;
  }

  .chunk-ink-bar .pen-popout::after {
    border-right: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    border-bottom: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
  }

  .chunk-ink-bar .chunk-shape-popout::after {
    border-right: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    border-bottom: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
  }

  .chunk-ink-bar .chunk-paper-popout::after {
    border-right: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
    border-bottom: 1px solid color-mix(in oklch, var(--chunk-accent) 26%, transparent);
  }

  .chunk-ink-bar .shape-option:hover {
    background: color-mix(in oklch, var(--chunk-accent) 10%, white);
  }

  .chunk-ink-bar .shape-option.active {
    background: color-mix(in oklch, var(--chunk-accent) 14%, white);
    border-color: color-mix(in oklch, var(--chunk-accent) 24%, transparent);
    color: var(--chunk-accent);
  }

  .chunk-ink-bar .paper-option:hover {
    background: color-mix(in oklch, var(--chunk-accent) 10%, white);
  }

  .chunk-ink-bar .paper-option.active {
    background: color-mix(in oklch, var(--chunk-accent) 14%, white);
    border-color: color-mix(in oklch, var(--chunk-accent) 24%, transparent);
    color: var(--chunk-accent);
  }

  .chunk-ink-bar .pen-slider {
    accent-color: var(--chunk-accent);
  }

  .chunk-ink-bar .colour-swatch.selected {
    box-shadow:
      0 0 0 2px color-mix(in oklch, var(--chunk-accent) 72%, black),
      0 0 0 5px color-mix(in oklch, var(--chunk-accent) 28%, transparent);
  }

  .chunk-tab-placeholder {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #9ca3af;
    font-size: 13px;
    font-style: italic;
  }

  .chunk-glossary-pane {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: #ffffff;
  }

  .chunk-glossary-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
    padding: 6px 10px;
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
    background: #fafafa;
    flex-shrink: 0;
  }

  .chunk-glossary-bar-main {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    min-width: 0;
  }

  .chunk-glossary-mode {
    display: flex;
    gap: 2px;
    padding: 2px;
    background: #eef0f3;
    border-radius: 6px;
  }

  .chunk-glossary-mode-btn {
    height: 22px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 500;
    color: #6b7280;
    background: transparent;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s;
  }

  .chunk-glossary-mode-btn:hover { color: #374151; }
  .chunk-glossary-mode-btn.active {
    background: #ffffff;
    color: #111827;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }

  .chunk-glossary-format-picker {
    display: flex;
    gap: 2px;
    padding: 2px;
    background: #f2f4f7;
    border-radius: 6px;
  }

  .chunk-glossary-format-btn {
    height: 22px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 600;
    color: #52606d;
    background: transparent;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s;
  }

  .chunk-glossary-format-btn.active {
    background: #ffffff;
    color: #0f172a;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
  }

  .chunk-glossary-insert-graph {
    height: 22px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 600;
    color: #1d4ed8;
    background: #ffffff;
    border: 1px solid #bfdbfe;
    border-radius: 4px;
    cursor: pointer;
  }

  .chunk-glossary-insert-graph:hover {
    background: #eff6ff;
  }

  .chunk-glossary-insert-graph:disabled {
    cursor: not-allowed;
    color: #94a3b8;
    border-color: #cbd5e1;
    background: #f8fafc;
  }

  .chunk-glossary-export-pdf {
    height: 22px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 600;
    color: #0f172a;
    background: #ffffff;
    border: 1px solid #cbd5e1;
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s, color 0.12s;
  }

  .chunk-glossary-export-pdf:hover:not(:disabled) {
    background: #f8fafc;
    border-color: #94a3b8;
  }

  .chunk-glossary-export-pdf:disabled {
    cursor: not-allowed;
    color: #94a3b8;
    border-color: #cbd5e1;
    background: #f8fafc;
  }

  .chunk-glossary-status {
    margin-left: auto;
    font-size: 11px;
    color: #9ca3af;
  }
  .chunk-glossary-status-error { color: #b91c1c; }

  .chunk-glossary-format-notice {
    margin: 0;
    padding: 8px 10px;
    border-bottom: 1px solid rgba(191, 219, 254, 0.85);
    background: #eff6ff;
    color: #1d4ed8;
    font-size: 12px;
    line-height: 1.45;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-glossary-typst-workspace {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    overflow: hidden;
  }

  .chunk-glossary-typst-workspace.mode-preview {
    grid-template-columns: minmax(0, 1fr);
  }

  .chunk-glossary-typst-workspace.mode-preview .chunk-glossary-typst-editor-pane {
    display: none;
  }

  .chunk-glossary-typst-workspace.mode-preview .chunk-glossary-typst-preview-pane .chunk-glossary-pane-header {
    display: none;
  }

  .chunk-glossary-typst-editor-pane,
  .chunk-glossary-typst-preview-pane {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .chunk-glossary-typst-editor-pane {
    border-right: 1px solid rgba(203, 213, 225, 0.72);
  }

  .chunk-glossary-pane-header {
    min-height: 34px;
    padding: 8px 12px;
    border-bottom: 1px solid rgba(203, 213, 225, 0.72);
    background: #f8fafc;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-glossary-pane-header strong {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #334155;
  }

  .chunk-glossary-pane-header span {
    font-size: 11px;
    color: #64748b;
  }

  .chunk-glossary-editor {
    flex: 1;
    min-height: 0;
    width: 100%;
    padding: 14px 16px;
    border: none;
    outline: none;
    resize: none;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 14px;
    line-height: 1.55;
    color: #1f2937;
    background: #ffffff;
    box-sizing: border-box;
  }

  .chunk-glossary-editor-typst {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 13px;
    line-height: 1.6;
    tab-size: 2;
    border-radius: 0;
  }

  .chunk-glossary-editor::placeholder {
    color: #9ca3af;
    font-style: italic;
  }

  .chunk-glossary-preview {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 14px 16px;
    font-family: Georgia, 'Times New Roman', serif;
    font-size: 14px;
    line-height: 1.55;
    color: #1f2937;
  }

  .chunk-glossary-preview-typst {
    padding: 12px;
    background: linear-gradient(180deg, #eef2f8 0%, #e7edf5 100%);
  }

  .chunk-glossary-typst-workspace.mode-preview .chunk-glossary-preview-typst {
    padding: 16px;
  }

  .chunk-glossary-preview :global(p) { margin: 0 0 0.7em; }
  .chunk-glossary-preview :global(p:last-child) { margin-bottom: 0; }
  .chunk-glossary-preview :global(h1),
  .chunk-glossary-preview :global(h2),
  .chunk-glossary-preview :global(h3),
  .chunk-glossary-preview :global(h4) {
    font-family: Inter, system-ui, sans-serif;
    margin: 0.9em 0 0.4em;
    line-height: 1.25;
  }
  .chunk-glossary-preview :global(h1) { font-size: 18px; }
  .chunk-glossary-preview :global(h2) { font-size: 16px; }
  .chunk-glossary-preview :global(h3) { font-size: 14px; }
  .chunk-glossary-preview :global(ul),
  .chunk-glossary-preview :global(ol) {
    margin: 0 0 0.7em;
    padding-left: 1.4em;
  }
  .chunk-glossary-preview :global(code) {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.9em;
    padding: 1px 4px;
    background: #f3f4f6;
    border-radius: 3px;
  }
  .chunk-glossary-preview :global(pre) {
    margin: 0 0 0.7em;
    padding: 10px 12px;
    background: #f3f4f6;
    border-radius: 6px;
    overflow-x: auto;
  }
  .chunk-glossary-preview :global(pre code) {
    padding: 0;
    background: transparent;
  }
  .chunk-glossary-preview :global(.chunk-math-display) {
    margin: 0.8em 0;
    text-align: center;
    overflow-x: auto;
  }

  .chunk-glossary-preview-empty {
    color: #9ca3af;
    font-style: italic;
    margin: 0;
  }

  .chunk-typst-preview-error {
    margin: 0 0 10px;
    padding: 9px 10px;
    border-radius: 8px;
    border: 1px solid #fecaca;
    background: #fef2f2;
    color: #991b1b;
    font-family: Inter, system-ui, sans-serif;
    font-size: 12px;
    line-height: 1.45;
    white-space: pre-wrap;
  }

  .chunk-typst-preview-document {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-height: 100%;
  }

  .chunk-typst-preview-svg {
    width: 100%;
    border: 1px solid rgba(203, 213, 225, 0.9);
    background: #ffffff;
    overflow: hidden;
  }

  .chunk-typst-preview-svg :global(svg) {
    display: block;
    width: 100%;
    height: auto;
  }

  :global(.chunk-graph-block) {
    margin: 0.85em 0;
    padding: 8px;
    border-radius: 9px;
    border: 1px solid #d1d5db;
    background: #f8fafc;
  }

  :global(.chunk-graph-block svg) {
    width: 100%;
    height: auto;
    display: block;
  }

  :global(.chunk-graph-block figcaption) {
    margin-top: 6px;
    font-family: Inter, system-ui, sans-serif;
    font-size: 11px;
    color: #475569;
  }

  :global(.chunk-graph-error) {
    margin: 0.7em 0;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid #fecaca;
    background: #fef2f2;
    color: #991b1b;
    font-family: Inter, system-ui, sans-serif;
    font-size: 12px;
  }

  .chunk-ai-pane {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: linear-gradient(180deg, #fbfcfe 0%, #f4f7fb 100%);
  }

  .chunk-ai-marking {
    padding: 10px 14px 8px;
    border-bottom: 1px solid rgba(203, 213, 225, 0.72);
    background: rgba(255, 255, 255, 0.88);
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex-shrink: 0;
  }

  .chunk-ai-marking-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-marking-header strong {
    font-size: 12px;
    color: #1f2937;
  }

  .chunk-ai-marking-header span {
    font-size: 12px;
    color: #334155;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
  }

  .chunk-ai-marking-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .chunk-ai-mark-suggestion {
    border: 1px solid rgba(148, 163, 184, 0.45);
    border-radius: 10px;
    padding: 9px 10px;
    background: #f8fafc;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .chunk-ai-mark-history {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .chunk-ai-mark-history-title {
    margin: 0;
    color: #6b7280;
    font-size: 10.5px;
    font-family: Inter, system-ui, sans-serif;
    font-weight: 800;
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }

  .chunk-ai-mark-history-empty {
    margin: 0;
    color: #94a3b8;
    font-size: 12px;
    font-style: italic;
    font-family: Georgia, 'Times New Roman', serif;
  }

  .chunk-ai-mark-history-list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .chunk-ai-mark-history-item {
    border: 1px solid rgba(148, 163, 184, 0.35);
    border-radius: 8px;
    background: #fff;
    padding: 7px 8px;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .chunk-ai-mark-history-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-family: Inter, system-ui, sans-serif;
    color: #475569;
    font-size: 11px;
  }

  .chunk-ai-mark-history-score {
    font-weight: 700;
    color: #0f172a;
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
  }

  .chunk-ai-mark-history-source {
    border-radius: 999px;
    padding: 1px 7px;
    background: #eef2f7;
    color: #475569;
    font-weight: 700;
    letter-spacing: 0.04em;
    font-size: 10px;
  }

  .chunk-ai-mark-history-time {
    color: #64748b;
    font-size: 10.5px;
  }

  .chunk-ai-mark-history-feedback {
    margin: 0;
    padding: 7px 8px;
    border-radius: 7px;
    border: 1px solid rgba(148, 163, 184, 0.35);
    background: #f8fafc;
    font-size: 12px;
    line-height: 1.5;
    color: #334155;
    font-family: Georgia, 'Times New Roman', serif;
  }

  .chunk-ai-mark-history-feedback :global(p) {
    margin: 0;
  }

  .chunk-ai-mark-history-feedback :global(p + p) {
    margin-top: 0.65em;
  }

  .chunk-ai-rewrite {
    padding: 12px 14px 10px;
    border-bottom: 1px solid rgba(203, 213, 225, 0.7);
    background: rgba(255, 255, 255, 0.78);
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex-shrink: 0;
  }

  .chunk-ai-rewrite-compact {
    padding-top: 10px;
    padding-bottom: 9px;
    gap: 7px;
  }

  .chunk-ai-context-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-context-row span {
    font-size: 11px;
    color: #64748b;
  }

  .chunk-ai-context-toggle {
    border: 1px solid rgba(148, 163, 184, 0.7);
    background: #ffffff;
    color: #334155;
    border-radius: 999px;
    height: 25px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.12s ease, border-color 0.12s ease, color 0.12s ease;
  }

  .chunk-ai-context-toggle:hover:not(:disabled) {
    border-color: color-mix(in oklch, var(--chunk-accent) 42%, white);
    color: #0f172a;
  }

  .chunk-ai-context-toggle.active {
    border-color: color-mix(in oklch, var(--chunk-accent) 58%, white);
    background: color-mix(in oklch, var(--chunk-accent) 12%, white);
    color: color-mix(in oklch, var(--chunk-accent) 72%, black);
  }

  .chunk-ai-context-toggle:disabled {
    opacity: 0.58;
    cursor: default;
  }

  .chunk-ai-rewrite-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-rewrite-header strong {
    font-size: 12px;
    color: #1f2937;
  }

  .chunk-ai-rewrite-header span {
    font-size: 11px;
    color: #6b7280;
  }

  .chunk-ai-rewrite-tabs {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    width: fit-content;
    padding: 2px;
    border-radius: 7px;
    background: #eef2f7;
  }

  .chunk-ai-rewrite-tab {
    border: none;
    height: 24px;
    padding: 0 10px;
    border-radius: 5px;
    background: transparent;
    color: #64748b;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    font-family: Inter, system-ui, sans-serif;
    transition: color 0.12s ease, background 0.12s ease;
  }

  .chunk-ai-rewrite-tab:hover:not(:disabled) {
    color: #334155;
  }

  .chunk-ai-rewrite-tab.active {
    background: #ffffff;
    color: #0f172a;
    box-shadow: 0 1px 2px rgba(15, 23, 42, 0.14);
  }

  .chunk-ai-rewrite-tab:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .chunk-ai-rewrite-input {
    width: 100%;
    min-height: 58px;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid rgba(148, 163, 184, 0.66);
    background: #fff;
    color: #111827;
    font-size: 13px;
    line-height: 1.45;
    resize: vertical;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-rewrite-input:focus {
    outline: none;
    border-color: color-mix(in oklch, var(--chunk-accent) 55%, white);
    box-shadow: 0 0 0 3px color-mix(in oklch, var(--chunk-accent) 16%, transparent);
  }

  .chunk-ai-rewrite-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .chunk-ai-rewrite-error {
    margin: 0;
    color: #b91c1c;
    font-size: 12px;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-rewrite-preview {
    border: 1px solid rgba(148, 163, 184, 0.45);
    border-radius: 10px;
    padding: 9px 10px;
    background: #f8fafc;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .chunk-ai-rewrite-field {
    margin: 0;
    color: #334155;
    font-size: 12px;
    line-height: 1.35;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-ai-rewrite-field span {
    font-weight: 700;
    color: #0f172a;
  }

  .chunk-ai-rewrite-body-preview {
    margin: 0;
    padding: 8px 9px;
    border-radius: 8px;
    border: 1px solid rgba(148, 163, 184, 0.45);
    background: #fff;
    max-height: 180px;
    overflow: auto;
    font-size: 12.5px;
    line-height: 1.6;
    color: #334155;
    font-family: Georgia, 'Times New Roman', serif;
    white-space: normal;
    word-break: break-word;
    font-variant-numeric: lining-nums slashed-zero;
    font-feature-settings: "zero" 1;
  }

  .chunk-ai-rewrite-body-preview:empty::after {
    content: "No preview text.";
    color: #9ca3af;
    font-style: italic;
  }

  .chunk-ai-rewrite-body-preview :global(p) {
    margin: 0;
  }

  .chunk-ai-rewrite-body-preview :global(p + p) {
    margin-top: 0.72em;
  }

  .chunk-ai-rewrite-body-preview :global(ol),
  .chunk-ai-rewrite-body-preview :global(ul) {
    margin: 0.55em 0 0;
    padding-left: 1.15rem;
  }

  .chunk-ai-rewrite-body-preview :global(li + li) {
    margin-top: 0.22rem;
  }

  .chunk-ai-rewrite-body-preview :global(.chunk-math-display) {
    margin: 0.6em 0;
    overflow-x: auto;
    overflow-y: hidden;
  }

  .chunk-ai-rewrite-body-preview :global(.katex-display) {
    margin: 0;
  }

  .chunk-ai-rewrite-body-preview :global(code) {
    padding: 0.05em 0.32em;
    border-radius: 4px;
    background: rgba(15, 23, 42, 0.06);
    font-size: 0.92em;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }

  .chunk-ai-rewrite-body-preview :global(pre) {
    margin: 0.65em 0 0;
    padding: 0.65em 0.75em;
    border-radius: 8px;
    background: rgba(15, 23, 42, 0.05);
    overflow-x: auto;
  }

  .chunk-ai-rewrite-body-preview :global(pre code) {
    padding: 0;
    background: transparent;
  }

  .chunk-ai-rewrite-body-preview :global(a) {
    color: var(--chunk-accent);
  }

  .chunk-ai-transcript {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 14px 16px 10px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .chunk-ai-empty {
    margin: auto 0;
    padding: 18px 16px;
    border-radius: 14px;
    border: 1px dashed rgba(148, 163, 184, 0.7);
    background: rgba(255, 255, 255, 0.78);
    color: #64748b;
    font-size: 13px;
    line-height: 1.55;
  }

  .chunk-ai-empty p {
    margin: 0;
  }

  .chunk-ai-empty p + p {
    margin-top: 0.45rem;
  }

  .chunk-chat-message {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .chunk-chat-message.user {
    align-items: flex-end;
  }

  .chunk-chat-message.assistant {
    align-items: flex-start;
  }

  .chunk-chat-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #7b8798;
  }

  .chunk-chat-bubble {
    width: fit-content;
    max-width: min(100%, 540px);
    padding: 12px 14px;
    border-radius: 14px;
    border: 1px solid rgba(203, 213, 225, 0.9);
    background: #fff;
    color: #1f2937;
    font-size: 13px;
    line-height: 1.58;
    white-space: pre-wrap;
    word-break: break-word;
    box-shadow: 0 6px 18px rgba(15, 23, 42, 0.05);
  }

  .chunk-chat-image {
    display: block;
    width: min(100%, 320px);
    max-height: 220px;
    object-fit: contain;
    border-radius: 10px;
    border: 1px solid rgba(148, 163, 184, 0.55);
    background: #fff;
    margin-bottom: 8px;
  }

  .chunk-chat-image-strip {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .chunk-chat-image-strip .chunk-chat-image {
    margin-bottom: 0;
  }

  .chunk-chat-bubble.user-bubble {
    background: color-mix(in oklch, var(--chunk-accent) 11%, white);
    border-color: color-mix(in oklch, var(--chunk-accent) 28%, white);
    color: #142033;
  }

  .chunk-chat-bubble.assistant-bubble {
    background: rgba(255, 255, 255, 0.94);
  }

  .chunk-chat-bubble.is-error {
    background: #fff5f5;
    border-color: #f1b7b7;
    color: #991b1b;
  }

  .chunk-chat-bubble.rendered {
    white-space: normal;
    font-family: Georgia, 'Times New Roman', serif;
    font-variant-numeric: lining-nums slashed-zero;
    font-feature-settings: "zero" 1;
  }

  .chunk-chat-bubble.rendered :global(p) {
    margin: 0;
  }

  .chunk-chat-bubble.rendered :global(p + p) {
    margin-top: 0.72em;
  }

  .chunk-chat-bubble.rendered :global(ol),
  .chunk-chat-bubble.rendered :global(ul) {
    margin: 0.55em 0 0;
    padding-left: 1.15rem;
  }

  .chunk-chat-bubble.rendered :global(li + li) {
    margin-top: 0.22rem;
  }

  .chunk-chat-bubble.rendered :global(.chunk-math-display) {
    margin: 0.6em 0;
    overflow-x: auto;
    overflow-y: hidden;
    -webkit-overflow-scrolling: touch;
  }

  .chunk-chat-bubble.rendered :global(.katex-display) {
    margin: 0;
  }

  .chunk-chat-bubble.rendered :global(code) {
    padding: 0.05em 0.32em;
    border-radius: 4px;
    background: rgba(15, 23, 42, 0.06);
    font-size: 0.92em;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }

  .chunk-chat-bubble.rendered :global(pre) {
    margin: 0.65em 0 0;
    padding: 0.65em 0.75em;
    border-radius: 8px;
    background: rgba(15, 23, 42, 0.05);
    overflow-x: auto;
  }

  .chunk-chat-bubble.rendered :global(pre code) {
    padding: 0;
    background: transparent;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block) {
    margin: 0.72em 0 0;
    border: 1px solid rgba(203, 213, 225, 0.95);
    border-radius: 8px;
    background: #f8fafc;
    overflow: hidden;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-toolbar) {
    min-height: 34px;
    padding: 5px 6px 5px 10px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    border-bottom: 1px solid rgba(203, 213, 225, 0.75);
    background: rgba(255, 255, 255, 0.74);
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-language) {
    min-width: 0;
    color: #64748b;
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-copy) {
    min-width: 58px;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    background: #fff;
    border: 1px solid rgba(148, 163, 184, 0.75);
    color: #334155;
    font-family: Inter, system-ui, sans-serif;
    font-size: 12px;
    font-weight: 700;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-copy:disabled) {
    opacity: 0.68;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block pre) {
    margin: 0;
    padding: 0.72em 0.78em;
    border-radius: 0;
    background: transparent;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block pre code) {
    display: block;
    width: max-content;
    min-width: 100%;
    line-height: 1.45;
    white-space: pre;
    word-break: normal;
  }

  .chunk-chat-bubble.rendered :global(a) {
    color: var(--chunk-accent);
    text-decoration-thickness: 0.08em;
    text-underline-offset: 0.12em;
  }

  .chunk-ai-status-row {
    min-height: 18px;
    padding: 0 16px 8px;
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }

  .chunk-ai-status {
    font-size: 12px;
    color: #64748b;
  }

  .chunk-ai-error {
    font-size: 12px;
    color: #b42318;
  }

  .chunk-ai-composer {
    padding: 0 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .chunk-chat-attachment-preview {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 10px;
    border: 1px solid rgba(148, 163, 184, 0.55);
    background: rgba(255, 255, 255, 0.88);
  }

  .chunk-chat-attachment-preview img {
    width: 80px;
    height: 56px;
    object-fit: cover;
    border-radius: 8px;
    border: 1px solid rgba(148, 163, 184, 0.5);
    flex-shrink: 0;
    background: #fff;
  }

  .chunk-chat-attachment-meta {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-family: Inter, system-ui, sans-serif;
  }

  .chunk-chat-attachment-meta strong {
    font-size: 12px;
    color: #0f172a;
  }

  .chunk-chat-attachment-meta span {
    font-size: 11px;
    color: #64748b;
  }

  .chunk-chat-attachment-preview .chunk-ai-action {
    min-width: 68px;
    height: 30px;
    padding: 0 10px;
    font-size: 12px;
    border-radius: 8px;
  }

  .chunk-ai-input {
    width: 100%;
    min-height: 86px;
    resize: vertical;
    padding: 12px 14px;
    border: 1px solid rgba(203, 213, 225, 0.95);
    border-radius: 14px;
    background: rgba(255, 255, 255, 0.96);
    color: #1f2937;
    font: inherit;
    line-height: 1.5;
  }

  .chunk-ai-input:focus {
    outline: none;
    border-color: color-mix(in oklch, var(--chunk-accent) 55%, white);
    box-shadow: 0 0 0 3px color-mix(in oklch, var(--chunk-accent) 16%, transparent);
  }

  .chunk-ai-input:disabled {
    background: #f8fafc;
    color: #94a3b8;
  }

  .chunk-ai-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .chunk-ai-action {
    min-width: 88px;
    height: 36px;
    padding: 0 14px;
    border-radius: 10px;
    font-size: 13px;
    font-weight: 600;
    transition: transform 0.12s, opacity 0.12s, background 0.12s;
  }

  .chunk-ai-action:hover:not(:disabled) {
    transform: translateY(-1px);
  }

  .chunk-ai-action:disabled {
    opacity: 0.5;
  }

  .chunk-ai-send {
    background: var(--chunk-accent);
    color: #fff;
  }

  .chunk-ai-stop {
    background: #eef2f7;
    color: #334155;
    border: 1px solid rgba(148, 163, 184, 0.45);
  }

  @media (max-width: 720px) {
    .chunk-nav-btn {
      width: 32px;
      height: 32px;
      border-radius: 9px;
      bottom: 8px;
    }
    .chunk-nav-prev { left: 8px; }
    .chunk-nav-next { right: 8px; }
    .chunk-sheet { flex-direction: column; }
    .chunk-panel-resizer { display: none; }
    .chunk-panel-left { width: 100%; max-height: 180px; border-right: none; border-bottom: 1px solid rgba(0,0,0,0.07); border-left: none; border-top: 3px solid var(--chunk-accent); }
    .cpl-title-input { margin-bottom: 6px; }
    .cpl-question-meta {
      margin-bottom: 6px;
      padding: 7px 8px;
      gap: 6px;
    }
    .cpl-question-entry {
      flex-wrap: wrap;
    }
    .cpl-question-input {
      min-width: 120px;
    }
    .cpl-linked {
      margin-bottom: 6px;
    }
    .cpl-linked-toggle {
      min-height: 32px;
      padding: 6px 9px;
    }
    .cpl-linked-content {
      padding: 7px 9px;
    }
    .cpl-linked-body {
      max-height: 48px;
    }
    .cpl-body-editor {
      min-height: 84px;
      margin-bottom: 8px;
      font-size: 12px;
      line-height: 1.45;
    }
    .chunk-glossary-bar {
      align-items: flex-start;
    }
    .chunk-glossary-bar-main {
      width: 100%;
      flex-wrap: wrap;
    }
    .chunk-glossary-status {
      width: 100%;
      margin-left: 0;
    }
    .chunk-glossary-typst-workspace {
      display: block;
    }
    .chunk-glossary-typst-workspace .chunk-glossary-typst-editor-pane,
    .chunk-glossary-typst-workspace .chunk-glossary-typst-preview-pane {
      display: none;
    }
    .chunk-glossary-typst-workspace.mode-edit .chunk-glossary-typst-editor-pane,
    .chunk-glossary-typst-workspace.mode-preview .chunk-glossary-typst-preview-pane {
      display: flex;
    }
    .chunk-glossary-typst-editor-pane {
      border-right: none;
    }
    .chunk-glossary-preview-typst {
      padding: 10px;
    }
    .chunk-ai-rewrite-body-preview {
      max-height: 120px;
    }
    .chunk-ai-marking {
      padding-left: 10px;
      padding-right: 10px;
    }
    .chunk-ai-mark-history-item {
      padding: 6px 7px;
    }
  }

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
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.9em;
    color: #666;
    font-variant-numeric: tabular-nums;
  }

  .page-input {
    width: 4.5ch;
    padding: 0.2rem 0.35rem;
    background: #fff;
    color: inherit;
    border: 1px solid #ccc;
    border-radius: 6px;
    text-align: center;
    font: inherit;
    font-variant-numeric: tabular-nums;
  }

  .page-input:focus {
    outline: none;
    border-color: #999;
    box-shadow: 0 0 0 2px rgba(32, 64, 160, 0.12);
  }

  .page-input:disabled {
    background: #f3f3f3;
    color: #888;
  }

  .page-total {
    min-width: 3ch;
    text-align: left;
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

  .pen-tool {
    position: relative;
    display: flex;
    align-items: center;
    z-index: 40;
  }

  .shape-tool {
    position: relative;
    display: flex;
    align-items: center;
    z-index: 40;
  }

  .paper-tool {
    position: relative;
    display: flex;
    align-items: center;
    z-index: 40;
  }

  .pen-popout {
    position: absolute;
    bottom: calc(100% + 10px);
    left: 50%;
    transform: translateX(-50%);
    width: 230px;
    padding: 0.8rem;
    background: rgba(255, 255, 255, 0.96);
    border: 1px solid rgba(32, 64, 160, 0.14);
    border-radius: 14px;
    box-shadow: 0 12px 32px rgba(25, 34, 68, 0.18);
    backdrop-filter: blur(8px);
    z-index: 120;
  }

  .shape-popout {
    position: absolute;
    bottom: calc(100% + 10px);
    left: 50%;
    transform: translateX(-50%);
    width: 260px;
    padding: 0.55rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    background: rgba(255, 255, 255, 0.96);
    border: 1px solid rgba(32, 64, 160, 0.14);
    border-radius: 14px;
    box-shadow: 0 12px 32px rgba(25, 34, 68, 0.18);
    backdrop-filter: blur(8px);
    z-index: 120;
  }

  .paper-popout {
    position: absolute;
    bottom: calc(100% + 10px);
    left: 50%;
    transform: translateX(-50%);
    width: 250px;
    padding: 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
    background: rgba(255, 255, 255, 0.96);
    border: 1px solid rgba(32, 64, 160, 0.14);
    border-radius: 14px;
    box-shadow: 0 12px 32px rgba(25, 34, 68, 0.18);
    backdrop-filter: blur(8px);
    z-index: 120;
  }

  .pen-popout::after {
    content: "";
    position: absolute;
    top: 100%;
    left: 50%;
    width: 14px;
    height: 14px;
    background: rgba(255, 255, 255, 0.96);
    border-right: 1px solid rgba(32, 64, 160, 0.14);
    border-bottom: 1px solid rgba(32, 64, 160, 0.14);
    transform: translate(-50%, -50%) rotate(45deg);
  }

  .shape-popout::after {
    content: "";
    position: absolute;
    top: 100%;
    left: 50%;
    width: 14px;
    height: 14px;
    background: rgba(255, 255, 255, 0.96);
    border-right: 1px solid rgba(32, 64, 160, 0.14);
    border-bottom: 1px solid rgba(32, 64, 160, 0.14);
    transform: translate(-50%, -50%) rotate(45deg);
  }

  .paper-popout::after {
    content: "";
    position: absolute;
    top: 100%;
    left: 50%;
    width: 14px;
    height: 14px;
    background: rgba(255, 255, 255, 0.96);
    border-right: 1px solid rgba(32, 64, 160, 0.14);
    border-bottom: 1px solid rgba(32, 64, 160, 0.14);
    transform: translate(-50%, -50%) rotate(45deg);
  }

  .paper-popout-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.7rem;
  }

  .paper-popout-title {
    font-size: 0.88rem;
    font-weight: 600;
    color: #28405e;
  }

  .paper-sample {
    width: 42px;
    height: 30px;
    border-radius: 7px;
    border: 1px solid rgba(51, 65, 85, 0.18);
    overflow: hidden;
    background: var(--paper-background-colour, #f4f7fb);
    flex-shrink: 0;
  }

  .paper-sample-pattern {
    display: block;
    width: 100%;
    height: 100%;
  }

  .paper-sample-pattern.paper-dotted {
    background-image: radial-gradient(circle at center, var(--paper-pattern-colour) 1.2px, transparent 1.35px);
    background-size: 10px 10px;
  }

  .paper-sample-pattern.paper-grid {
    background-image:
      linear-gradient(var(--paper-pattern-colour) 1px, transparent 1px),
      linear-gradient(90deg, var(--paper-pattern-colour) 1px, transparent 1px);
    background-size: 10px 10px;
  }

  .paper-sample-pattern.paper-line {
    background-image: linear-gradient(var(--paper-pattern-colour) 1px, transparent 1px);
    background-size: 10px 10px;
  }

  .paper-pattern-options {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0.35rem;
  }

  .paper-option {
    height: 30px;
    padding: 0 0.45rem;
    border-radius: 8px;
    border: 1px solid rgba(148, 163, 184, 0.48);
    background: #fff;
    color: #475569;
    font-size: 0.78rem;
    font-weight: 600;
    transition: background 0.12s, border-color 0.12s, color 0.12s;
  }

  .paper-option:hover {
    background: #eef3ff;
  }

  .paper-option.active {
    background: #e7efff;
    border-color: rgba(32, 64, 160, 0.28);
    color: #19397f;
  }

  .paper-colours {
    display: grid;
    grid-template-columns: repeat(7, minmax(0, 1fr));
    align-items: center;
    gap: 0.45rem;
  }

  .paper-colour-swatch,
  .paper-colour-custom {
    width: 26px;
    height: 26px;
    padding: 0;
    justify-self: center;
    border-radius: 7px;
    background: var(--paper-swatch-colour);
    border: 2px solid rgba(255, 255, 255, 0.96);
    box-shadow: 0 0 0 1px rgba(50, 62, 88, 0.18);
    transition: transform 0.12s, box-shadow 0.12s;
  }

  .paper-colour-swatch:hover,
  .paper-colour-custom:hover {
    transform: scale(1.06);
  }

  .paper-colour-swatch.selected,
  .paper-colour-custom.selected {
    box-shadow: 0 0 0 2px #1f3f93, 0 0 0 5px rgba(31, 63, 147, 0.16);
  }

  .paper-colour-custom {
    position: relative;
    display: block;
    overflow: hidden;
    background: var(--paper-swatch-colour, #fff);
  }

  .paper-colour-custom input {
    position: absolute;
    inset: -4px;
    width: calc(100% + 8px);
    height: calc(100% + 8px);
    padding: 0;
    border: 0;
    opacity: 0;
    cursor: pointer;
  }

  .shape-option {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.12rem;
    padding: 0.45rem 0.55rem;
    border-radius: 10px;
    border: 1px solid transparent;
    background: transparent;
    color: #384b67;
    text-align: left;
    transition: background 0.12s ease, border-color 0.12s ease, color 0.12s ease;
  }

  .shape-option:hover {
    background: #eef3ff;
  }

  .shape-option.active {
    background: #e7efff;
    border-color: rgba(32, 64, 160, 0.22);
    color: #19397f;
  }

  .shape-option-title {
    font-size: 0.82rem;
    font-weight: 600;
    line-height: 1.25;
  }

  .shape-option-hint {
    font-size: 0.72rem;
    line-height: 1.35;
    color: #66758e;
  }

  .pen-popout-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.7rem;
  }

  .pen-popout-title {
    font-size: 0.88rem;
    font-weight: 600;
    color: #28405e;
  }

  .pen-preview {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 999px;
    background: #f3f6fb;
    border: 1px solid rgba(40, 64, 94, 0.12);
  }

  .pen-preview-dot {
    width: max(6px, var(--pen-preview-size));
    height: max(6px, var(--pen-preview-size));
    border-radius: 999px;
    background: var(--pen-preview-colour);
  }

  .pen-slider-group {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.35rem;
    font-size: 0.8rem;
    color: #4a5870;
  }

  .pen-slider {
    width: 100%;
    margin: 0 0 0.8rem;
    accent-color: #2351d1;
  }

  .pen-colours {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    gap: 0.55rem;
  }

  .colour-swatch {
    width: 28px;
    height: 28px;
    padding: 0;
    justify-self: center;
    border-radius: 999px;
    background: var(--swatch-colour);
    border: 2px solid rgba(255, 255, 255, 0.96);
    box-shadow: 0 0 0 1px rgba(50, 62, 88, 0.18);
    transition: transform 0.12s, box-shadow 0.12s;
  }

  .colour-swatch:hover {
    transform: scale(1.08);
  }

  .colour-swatch.selected {
    box-shadow: 0 0 0 2px #1f3f93, 0 0 0 5px rgba(31, 63, 147, 0.16);
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

</style>

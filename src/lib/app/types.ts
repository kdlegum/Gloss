export type DocumentMode = "textbook" | "past_paper" | "notebook";
export type SyncSetupStatus = "not_configured" | "folder_missing" | "initialised" | "ready";

export interface SourceDocument {
  id: number;
  title: string;
  file_path: string;
  page_count: number | null;
  document_mode: DocumentMode;
  instruction_page_start: number | null;
  instruction_page_end: number | null;
}

export interface PastPaperInstructionContextDebug {
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

export interface AiSettingsState {
  running_on_android: boolean;
  setup_complete: boolean;
  openai_api_key_set: boolean;
  gemini_api_key_set: boolean;
  deepseek_api_key_set: boolean;
  zai_api_key_set: boolean;
}

export interface SyncSnapshotInfo {
  revision: number;
  hash: string;
  updated_at_unix: number;
  updated_by: string;
  path: string;
}

export interface SyncDevicePresence {
  device_id: string;
  device_name: string;
  active_writer: boolean;
  last_seen_unix: number;
  lease_expires_unix: number;
}

export interface SyncConflict {
  id: string;
  path: string;
  kind: string;
}

export interface SyncState {
  setup_status: SyncSetupStatus;
  enabled: boolean;
  folder_kind: "path" | "android_tree";
  folder_path: string | null;
  folder_label: string | null;
  running_on_android: boolean;
  folder_ready: boolean;
  device_id: string;
  dirty: boolean;
  last_exported_revision: number | null;
  last_imported_revision: number | null;
  remote_snapshot: SyncSnapshotInfo | null;
  devices: SyncDevicePresence[];
  conflicts: SyncConflict[];
  missing_pdfs: string[];
  read_only: boolean;
  active_writer: SyncDevicePresence | null;
  message: string;
}

export type AutoSyncAction = "idle" | "waiting" | "blocked" | "imported" | "exported";

export interface AutoSyncResult {
  action: AutoSyncAction;
  message: string | null;
  state: SyncState;
}

export type ChunkingProvider = "ollama" | "openai" | "gemini" | "deepseek";
export type ChatProvider = "ollama" | "openai" | "gemini" | "deepseek";
export type VisionProvider = "ollama" | "openai" | "gemini" | "zai";
export type AnyProvider = ChunkingProvider | VisionProvider;
export type AiTask = "chunking" | "chat" | "vision";

export interface AiTaskSettings {
  chunking: { provider: ChunkingProvider; model: string };
  chat: { provider: ChatProvider; model: string };
  vision: { provider: VisionProvider; model: string };
}

export interface ModelOption {
  value: string;
  label: string;
  legacy?: boolean;
}

export interface EnsureChunkingRangeResult {
  requested_start_page: number;
  requested_end_page: number;
  started_pages: number;
  started_page_numbers: number[];
  skipped_pages: number;
}

export interface BatchChunkProgressState {
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

export interface ViewerBatchSettingsState {
  selectedBook: SourceDocument | null;
  totalPages: number;
  canBatchChunkDocument: boolean;
  batchChunkStarting: boolean;
  batchChunkError: string | null;
  batchChunkFeedback: string | null;
  batchChunkStartInput: string;
  batchChunkEndInput: string;
  batchChunkSkipChunkedPages: boolean;
  setBatchChunkStartInput: (value: string) => void;
  setBatchChunkEndInput: (value: string) => void;
  setBatchChunkSkipChunkedPages: (value: boolean) => void;
  chunkPageRangeFromInputs: () => Promise<void>;
  chunkWholePdf: () => Promise<void>;
  pastPaperInstructionPagesEnabled: boolean;
  setPastPaperInstructionPagesEnabled: (value: boolean) => void;
  pastPaperInstructionStartInput: string;
  pastPaperInstructionEndInput: string;
  setPastPaperInstructionStartInput: (value: string) => void;
  setPastPaperInstructionEndInput: (value: string) => void;
  pastPaperInstructionSaving: boolean;
  pastPaperInstructionError: string | null;
  pastPaperInstructionFeedback: string | null;
  pastPaperInstructionCheckLoading: boolean;
  pastPaperInstructionCheckError: string | null;
  pastPaperInstructionCheckFeedback: string | null;
  pastPaperInstructionCheckPreview: string | null;
  savePastPaperInstructionRange: () => Promise<void>;
  checkPastPaperInstructionContext: () => Promise<void>;
}

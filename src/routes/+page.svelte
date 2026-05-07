<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import LibraryScreen from "$lib/screens/LibraryScreen.svelte";
  import ReaderWorkspace from "$lib/screens/ReaderWorkspace.svelte";
  import AiSettingsSheet from "$lib/screens/AiSettingsSheet.svelte";
  import {
    AI_TASK_SETTINGS_STORAGE_KEY,
    LEGACY_CHUNKING_PROVIDER_STORAGE_KEY,
    CUSTOM_MODEL_VALUE,
    defaultAiTaskSettings,
    getDefaultModelForTask,
    getModelOptions,
    isChatProvider,
    isChunkingProvider,
    isModelInOptions,
    isVisionProvider,
  } from "$lib/app/ai";
  import { appLogError, appLogInfo, appLogWarn, formatLogError } from "$lib/app/log";
  import type {
    AiSettingsState,
    AiTask,
    AiTaskSettings,
    AnyProvider,
    AutoSyncResult,
    ChatProvider,
    ChunkingProvider,
    SourceDocument,
    SyncState,
    ViewerBatchSettingsState,
    VisionProvider,
  } from "$lib/app/types";

  let sourceDocuments = $state<SourceDocument[]>([]);
  let importing = $state(false);
  let syncState = $state<SyncState | null>(null);
  let syncBusy = $state(false);
  let syncMode = $state<string | null>(null);
  let syncError = $state<string | null>(null);
  let syncFeedback = $state<string | null>(null);
  let showSyncSheet = $state(false);
  let syncFolderInput = $state("");
  let autoSyncBusy = $state(false);
  let lastAutoSyncNotice = $state<string | null>(null);
  let pendingDeleteSourceDocumentId = $state<number | null>(null);
  let deletingSourceDocumentId = $state<number | null>(null);
  let renamingSourceDocumentId = $state<number | null>(null);
  let error = $state<string | null>(null);
  let selectedBook = $state<SourceDocument | null>(null);

  let aiTaskSettings = $state<AiTaskSettings>(defaultAiTaskSettings());
  let aiSettings = $state<AiSettingsState | null>(null);
  let customModelMode = $state<{ chunking: boolean; chat: boolean; vision: boolean }>({
    chunking: false,
    chat: false,
    vision: false,
  });
  let showAiKeySheet = $state(false);
  let aiKeySheetFirstRun = $state(false);
  let aiSettingsSaving = $state(false);
  let aiSettingsError = $state<string | null>(null);
  let openaiApiKeyInput = $state("");
  let geminiApiKeyInput = $state("");
  let deepseekApiKeyInput = $state("");
  let zaiApiKeyInput = $state("");
  let clearOpenaiApiKey = $state(false);
  let clearGeminiApiKey = $state(false);
  let clearDeepseekApiKey = $state(false);
  let clearZaiApiKey = $state(false);
  let viewerBatchState = $state<ViewerBatchSettingsState | null>(null);

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

  function refreshCustomModelMode(settings: AiTaskSettings = aiTaskSettings) {
    customModelMode = {
      chunking: !isModelInOptions("chunking", settings.chunking.provider, settings.chunking.model),
      chat: !isModelInOptions("chat", settings.chat.provider, settings.chat.model),
      vision: !isModelInOptions("vision", settings.vision.provider, settings.vision.model),
    };
  }

  function persistAiTaskSettings() {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(AI_TASK_SETTINGS_STORAGE_KEY, JSON.stringify(aiTaskSettings));
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
    return isModelInOptions(task, provider, model) ? model : CUSTOM_MODEL_VALUE;
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
    openaiApiKeyInput = "";
    geminiApiKeyInput = "";
    deepseekApiKeyInput = "";
    zaiApiKeyInput = "";
    clearOpenaiApiKey = false;
    clearGeminiApiKey = false;
    clearDeepseekApiKey = false;
    clearZaiApiKey = false;
    showAiKeySheet = true;
  }

  function closeAiKeySettings() {
    if (aiSettingsSaving || viewerBatchState?.batchChunkStarting) return;
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
    if (aiSettingsSaving || viewerBatchState?.batchChunkStarting) return;
    if (aiKeySheetFirstRun) {
      void completeAiKeySetupWithoutSaving();
    } else {
      closeAiKeySettings();
    }
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

  function applyUpdatedSourceDocument(updated: SourceDocument) {
    sourceDocuments = sourceDocuments.map((doc) => doc.id === updated.id ? updated : doc);
    if (selectedBook?.id === updated.id) {
      selectedBook = updated;
    }
  }

  async function loadSyncState() {
    try {
      syncState = await invoke<SyncState>("get_sync_state");
      if (syncState.folder_path) {
        syncFolderInput = syncState.folder_path;
      }
    } catch (err) {
      syncError = formatLogError(err);
      void appLogWarn(`[sync] failed to load local sync state: ${syncError}`);
    }
  }

  function openLocalSyncSheet() {
    syncError = null;
    syncFeedback = null;
    if (syncState?.folder_path) syncFolderInput = syncState.folder_path;
    showSyncSheet = true;
    void loadSyncState();
  }

  function closeLocalSyncSheet() {
    if (syncBusy) return;
    showSyncSheet = false;
  }

  async function runSyncAction<T>(mode: string, action: () => Promise<T>, after?: (result: T) => Promise<void> | void) {
    if (
      syncBusy
      || autoSyncBusy
      || importing
      || deletingSourceDocumentId !== null
      || renamingSourceDocumentId !== null
    ) return;
    error = null;
    syncError = null;
    syncFeedback = null;
    pendingDeleteSourceDocumentId = null;
    syncBusy = true;
    syncMode = mode;
    try {
      const result = await action();
      await after?.(result);
      await loadSyncState();
    } catch (err) {
      if (err !== "cancelled") {
        syncError = formatLogError(err);
        await appLogError(`[sync] ${mode} failed: ${syncError}`);
      }
    } finally {
      syncBusy = false;
      syncMode = null;
    }
  }

  async function runAutoSyncOnce() {
    if (
      syncBusy
      || autoSyncBusy
      || importing
      || deletingSourceDocumentId !== null
      || renamingSourceDocumentId !== null
      || showAiKeySheet
    ) return;

    const state = syncState;
    if (!state?.enabled || !state.folder_ready) return;

    autoSyncBusy = true;
    syncMode = "auto";
    const clearAutoSyncNotice = () => {
      if (lastAutoSyncNotice !== null && syncError?.includes(lastAutoSyncNotice)) {
        syncError = null;
      }
      lastAutoSyncNotice = null;
    };
    try {
      const result = await invoke<AutoSyncResult>("auto_sync_once", {
        allowImport: selectedBook === null,
      });
      syncState = result.state;
      if (result.state.folder_path) {
        syncFolderInput = result.state.folder_path;
      }
      if (result.action === "imported") {
        await loadSourceDocuments();
        syncFeedback = result.message ?? "Imported synced changes.";
        clearAutoSyncNotice();
        await appLogInfo(`[sync] ${syncFeedback}`);
      } else if (result.action === "exported") {
        syncFeedback = result.message ?? "Published local changes.";
        clearAutoSyncNotice();
        await appLogInfo(`[sync] ${syncFeedback}`);
      } else if (result.action === "blocked" && result.message) {
        syncError = result.message;
        if (lastAutoSyncNotice !== result.message) {
          lastAutoSyncNotice = result.message;
          await appLogWarn(`[sync] ${result.message}`);
        }
      } else if (result.action === "idle" || result.action === "waiting") {
        clearAutoSyncNotice();
      }
    } catch (err) {
      const message = formatLogError(err);
      syncError = `Auto sync paused: ${message}`;
      if (lastAutoSyncNotice !== message) {
        lastAutoSyncNotice = message;
        await appLogWarn(`[sync] auto sync failed: ${message}`);
      }
    } finally {
      autoSyncBusy = false;
      syncMode = null;
    }
  }

  async function chooseSyncFolder() {
    await runSyncAction("choose", async () => {
      const folder = await invoke<string>("choose_sync_folder");
      syncFolderInput = folder;
      return invoke<SyncState>("enable_sync_folder", { folderPath: folder });
    }, async (state) => {
      syncState = state;
      syncFolderInput = state.folder_path ?? syncFolderInput;
      syncFeedback = state.remote_snapshot
        ? "Local sync is connected. Gloss will import or publish automatically when it is safe."
        : "Local sync folder initialized. Gloss will publish local changes automatically.";
      await appLogInfo(`[sync] chose and connected folder=${syncFolderInput}`);
    });
  }

  async function enableSyncFolder() {
    const folderPath = syncFolderInput.trim();
    if (!folderPath) {
      syncError = "Choose a folder first.";
      return;
    }
    await runSyncAction(
      "enable",
      () => invoke<SyncState>("enable_sync_folder", { folderPath }),
      async (state) => {
        syncState = state;
        syncFeedback = state.remote_snapshot
          ? "Local sync is connected. Gloss will import or publish automatically when it is safe."
          : "Local sync folder initialized. Gloss will publish local changes automatically.";
        await appLogInfo(`[sync] enabled folder=${folderPath}`);
      },
    );
  }

  async function syncNow() {
    await runSyncAction(
      "sync",
      () => invoke<SyncState>("sync_now"),
      async (state) => {
        syncState = state;
        syncFeedback = `Published snapshot revision ${state.remote_snapshot?.revision ?? "new"}.`;
        await appLogInfo("[sync] published latest local snapshot");
      },
    );
  }

  async function importLatestSyncSnapshot() {
    const state = syncState;
    const remoteRevision = state?.remote_snapshot?.revision ?? null;
    const localRevision = Math.max(
      state?.last_exported_revision ?? 0,
      state?.last_imported_revision ?? 0,
    );
    if (remoteRevision === null || remoteRevision <= localRevision) {
      syncFeedback = "Local sync is already up to date.";
      return;
    }

    const confirmed = window.confirm(
      "Import the latest Gloss sync snapshot?\n\nThis replaces this device's local Gloss library with the synced copy. Local AI API keys stay on this device.",
    );
    if (!confirmed) return;
    await runSyncAction(
      "import",
      () => invoke<SyncState>("import_latest_sync_snapshot"),
      async (state) => {
        syncState = state;
        await loadSourceDocuments();
        syncFeedback = `Imported snapshot revision ${state.last_imported_revision ?? state.remote_snapshot?.revision ?? ""}.`;
        await appLogInfo("[sync] imported latest snapshot");
      },
    );
  }

  async function takeSyncEditingLease() {
    await runSyncAction(
      "lease",
      () => invoke<SyncState>("take_sync_editing_lease"),
      async (state) => {
        syncState = state;
        syncFeedback = "This device is now the active editor for the next 10 minutes.";
      },
    );
  }

  async function disableSync() {
    const confirmed = window.confirm("Disable local sync on this device? The sync folder and local Gloss data will not be deleted.");
    if (!confirmed) return;
    await runSyncAction(
      "disable",
      () => invoke<SyncState>("disable_sync"),
      async (state) => {
        syncState = state;
        syncFeedback = "Local sync disabled on this device.";
      },
    );
  }

  async function resolveSyncConflict(resolution: "keep_local" | "use_remote") {
    const confirmed = window.confirm(
      resolution === "keep_local"
        ? "Publish this device's local copy over the synced snapshot?"
        : "Replace this device's local copy with the synced snapshot?",
    );
    if (!confirmed) return;
    await runSyncAction(
      "resolve",
      () => invoke<SyncState>("resolve_sync_conflict", { resolution }),
      async (state) => {
        syncState = state;
        await loadSourceDocuments();
        syncFeedback = resolution === "keep_local" ? "Published this device's copy." : "Imported the synced copy.";
      },
    );
  }

  async function importPdf(documentMode: "textbook" | "past_paper" = "textbook") {
    if (syncBusy || autoSyncBusy || renamingSourceDocumentId !== null) return;
    error = null;
    pendingDeleteSourceDocumentId = null;
    importing = true;
    try {
      const doc = await invoke<SourceDocument>("import_pdf", { documentMode });
      void appLogInfo(
        `[import] imported doc=${doc.id} mode=${doc.document_mode} title="${doc.title}" path="${doc.file_path}"`,
      );
      await loadSourceDocuments();
      await loadSyncState();
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
      || syncBusy
      || autoSyncBusy
      || deletingSourceDocumentId !== null
      || renamingSourceDocumentId !== null
      || pendingDeleteSourceDocumentId !== book.id
    ) return;

    error = null;
    deletingSourceDocumentId = book.id;
    try {
      await invoke("delete_source_document", { sourceDocumentId: book.id });
      await loadSourceDocuments();
      await loadSyncState();
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

  async function renameSourceDocument(book: SourceDocument, title: string): Promise<boolean> {
    if (
      importing
      || syncBusy
      || autoSyncBusy
      || deletingSourceDocumentId !== null
      || renamingSourceDocumentId !== null
    ) return false;

    const nextTitle = title.trim();
    if (!nextTitle) {
      error = "Book title cannot be empty.";
      return false;
    }
    error = null;
    if (nextTitle === book.title) {
      return true;
    }

    pendingDeleteSourceDocumentId = null;
    renamingSourceDocumentId = book.id;
    try {
      const updated = await invoke<SourceDocument>("rename_source_document", {
        sourceDocumentId: book.id,
        title: nextTitle,
      });
      applyUpdatedSourceDocument(updated);
      await loadSyncState();
      await appLogInfo(
        `[library] renamed doc=${book.id} from "${book.title}" to "${updated.title}"`,
      );
      return true;
    } catch (err) {
      error = formatLogError(err);
      await appLogError(
        `[library] rename failed doc=${book.id} title="${book.title}": ${formatLogError(err)}`,
      );
      return false;
    } finally {
      renamingSourceDocumentId = null;
    }
  }

  function requestSourceDocumentDelete(bookId: number) {
    if (
      importing
      || syncBusy
      || autoSyncBusy
      || deletingSourceDocumentId !== null
      || renamingSourceDocumentId !== null
    ) return;
    pendingDeleteSourceDocumentId = pendingDeleteSourceDocumentId === bookId ? null : bookId;
  }

  function cancelSourceDocumentDelete() {
    if (deletingSourceDocumentId !== null) return;
    pendingDeleteSourceDocumentId = null;
  }

  function openBook(book: SourceDocument) {
    pendingDeleteSourceDocumentId = null;
    selectedBook = book;
  }

  function closeBook() {
    selectedBook = null;
    viewerBatchState = null;
    void loadSyncState();
  }

  function handleReaderAiTaskSettingsChange(settings: AiTaskSettings) {
    aiTaskSettings = settings;
    refreshCustomModelMode(settings);
  }

  onMount(() => {
    aiTaskSettings = loadAiTaskSettings();
    refreshCustomModelMode(aiTaskSettings);
    void loadAiSettings();
    void loadSourceDocuments();
    void loadSyncState();
    const autoSyncTimer = window.setInterval(() => {
      void runAutoSyncOnce();
    }, 15_000);
    const leaseTimer = window.setInterval(() => {
      const state = syncState;
      const self = state?.devices.find((device) => device.device_id === state.device_id);
      const leaseStillActive = !!self?.active_writer && self.lease_expires_unix * 1000 > Date.now();
      if (!state?.enabled || !leaseStillActive || syncBusy || autoSyncBusy) return;
      void (async () => {
        try {
          syncState = await invoke<SyncState>("take_sync_editing_lease");
        } catch (err) {
          void appLogWarn(`[sync] failed to refresh editing lease: ${formatLogError(err)}`);
        }
      })();
    }, 60_000);
    return () => {
      window.clearInterval(autoSyncTimer);
      window.clearInterval(leaseTimer);
    };
  });
</script>

<main>
  {#if selectedBook}
    <ReaderWorkspace
      book={selectedBook}
      providedAiTaskSettings={aiTaskSettings}
      providedAiSettings={aiSettings}
      openAiSettings={() => openAiKeySettings(false)}
      onClose={closeBook}
      onBookUpdated={applyUpdatedSourceDocument}
      onAiTaskSettingsChange={handleReaderAiTaskSettingsChange}
      onViewerBatchStateChange={(state) => viewerBatchState = state}
    />
  {:else}
      <LibraryScreen
        {sourceDocuments}
        {importing}
        {syncState}
        syncBusy={syncBusy || autoSyncBusy}
        {syncMode}
        {syncError}
        {syncFeedback}
        {showSyncSheet}
        {syncFolderInput}
        {pendingDeleteSourceDocumentId}
        {deletingSourceDocumentId}
        {renamingSourceDocumentId}
        {error}
        openAiSettings={() => openAiKeySettings(false)}
        {importPdf}
        {openLocalSyncSheet}
        {closeLocalSyncSheet}
        {chooseSyncFolder}
        {enableSyncFolder}
        {syncNow}
        {importLatestSyncSnapshot}
        {takeSyncEditingLease}
        {disableSync}
        {resolveSyncConflict}
        {openBook}
        {renameSourceDocument}
        {deleteSourceDocument}
        {requestSourceDocumentDelete}
        {cancelSourceDocumentDelete}
    />
  {/if}

  <AiSettingsSheet
    show={showAiKeySheet}
    {aiKeySheetFirstRun}
    {aiSettingsSaving}
    {aiSettingsError}
    {aiTaskSettings}
    {aiSettings}
    viewerState={viewerBatchState}
    bind:openaiApiKeyInput
    bind:geminiApiKeyInput
    bind:deepseekApiKeyInput
    bind:zaiApiKeyInput
    bind:clearOpenaiApiKey
    bind:clearGeminiApiKey
    bind:clearDeepseekApiKey
    bind:clearZaiApiKey
    {dismissAiKeySettings}
    {saveAiKeyForm}
    {setTaskProvider}
    {modelSelectValue}
    {setModelFromSelect}
    {setTaskModel}
  />
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
  }

  :global(html, body) {
    margin: 0;
    padding: 0;
    width: 100%;
    height: 100%;
    overflow: hidden;
    font-family: Inter, system-ui, sans-serif;
    background: #fafafa;
  }

  main {
    min-height: 100vh;
  }
</style>

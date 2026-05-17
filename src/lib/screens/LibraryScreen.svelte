<script lang="ts">
  import { tick } from "svelte";
  import type { DocumentMode, SourceDocument, SyncState, UpdateCheckResult } from "$lib/app/types";

  type PdfDocumentMode = Exclude<DocumentMode, "notebook">;

  let {
    sourceDocuments,
    importing,
    creatingNotebook,
    syncState,
    syncBusy,
    syncMode,
    syncError,
    syncFeedback,
    showSyncSheet,
    syncFolderInput,
    pendingDeleteSourceDocumentId,
    deletingSourceDocumentId,
    renamingSourceDocumentId,
    error,
    updateCheck,
    updateChecking,
    updateInstalling,
    updateInstallProgress,
    updateError,
    updateFeedback,
    openAiSettings,
    checkForUpdates,
    installUpdate,
    openUpdateRelease,
    importPdf,
    createBlankNotebook,
    openLocalSyncSheet,
    closeLocalSyncSheet,
    chooseSyncFolder,
    enableSyncFolder,
    syncNow,
    importLatestSyncSnapshot,
    takeSyncEditingLease,
    disableSync,
    resolveSyncConflict,
    openBook,
    renameSourceDocument,
    deleteSourceDocument,
    requestSourceDocumentDelete,
    cancelSourceDocumentDelete,
  }: {
    sourceDocuments: SourceDocument[];
    importing: boolean;
    creatingNotebook: boolean;
    syncState: SyncState | null;
    syncBusy: boolean;
    syncMode: string | null;
    syncError: string | null;
    syncFeedback: string | null;
    showSyncSheet: boolean;
    syncFolderInput: string;
    pendingDeleteSourceDocumentId: number | null;
    deletingSourceDocumentId: number | null;
    renamingSourceDocumentId: number | null;
    error: string | null;
    updateCheck: UpdateCheckResult | null;
    updateChecking: boolean;
    updateInstalling: boolean;
    updateInstallProgress: string | null;
    updateError: string | null;
    updateFeedback: string | null;
    openAiSettings: () => void;
    checkForUpdates: () => void;
    installUpdate: () => void;
    openUpdateRelease: () => void;
    importPdf: (documentMode: PdfDocumentMode) => Promise<void>;
    createBlankNotebook: () => Promise<void>;
    openLocalSyncSheet: () => void;
    closeLocalSyncSheet: () => void;
    chooseSyncFolder: () => Promise<void>;
    enableSyncFolder: () => Promise<void>;
    syncNow: () => Promise<void>;
    importLatestSyncSnapshot: () => Promise<void>;
    takeSyncEditingLease: () => Promise<void>;
    disableSync: () => Promise<void>;
    resolveSyncConflict: (resolution: "keep_local" | "use_remote") => Promise<void>;
    openBook: (book: SourceDocument) => void | Promise<void>;
    renameSourceDocument: (book: SourceDocument, title: string) => Promise<boolean>;
    deleteSourceDocument: (book: SourceDocument) => Promise<void>;
    requestSourceDocumentDelete: (bookId: number) => void;
    cancelSourceDocumentDelete: () => void;
  } = $props();

  let editingSourceDocumentId = $state<number | null>(null);
  let renameDraft = $state("");
  let renameInput = $state<HTMLInputElement | null>(null);
  let settingsMenuOpen = $state(false);
  let settingsMenuElement = $state<HTMLDivElement | null>(null);

  const remoteRevision = $derived(syncState?.remote_snapshot?.revision ?? null);
  const localRevision = $derived(Math.max(
    syncState?.last_exported_revision ?? 0,
    syncState?.last_imported_revision ?? 0,
  ));
  const remoteIsNewer = $derived(remoteRevision !== null && remoteRevision > localRevision);
  const hasSyncConflicts = $derived((syncState?.conflicts.length ?? 0) > 0);
  const hasMissingPdfs = $derived((syncState?.missing_pdfs.length ?? 0) > 0);
  const syncFolderDisplay = $derived(
    syncState?.folder_label || syncFolderInput || syncState?.folder_path || "No folder selected",
  );
  const canSyncNow = $derived(
    !!syncState?.enabled && !(syncState?.read_only ?? false) && !hasSyncConflicts && !hasMissingPdfs,
  );
  const canImportLatest = $derived(
    !!syncState?.enabled && remoteIsNewer && !hasSyncConflicts && !hasMissingPdfs,
  );
  const syncStatusLabel = $derived(syncState?.enabled
    ? syncState.read_only
      ? "Read-only"
      : syncState.dirty
        ? "Unsynced changes"
        : "Ready"
    : "Not set up");
  const libraryMutating = $derived(
    importing || creatingNotebook || syncBusy || deletingSourceDocumentId !== null || renamingSourceDocumentId !== null,
  );
  const updateStatusLabel = $derived(updateInstalling
    ? updateInstallProgress ?? "Installing..."
    : updateChecking
      ? "Checking..."
      : updateCheck?.available
        ? "Update available"
        : updateError
          ? "Check failed"
          : updateFeedback || "Ready");
  const settingsNeedsAttention = $derived(
    !!updateCheck?.available || hasSyncConflicts || hasMissingPdfs || !!syncState?.dirty,
  );

  function documentMeta(book: SourceDocument): string {
    if (book.document_mode === "notebook") {
      const count = Math.max(1, book.page_count ?? 1);
      return `${count} canvas${count === 1 ? "" : "es"} · Notebook`;
    }
    if (book.document_mode === "past_paper") return `${book.file_path} · Past paper`;
    return `${book.file_path} · Textbook`;
  }

  async function beginSourceDocumentRename(book: SourceDocument) {
    if (libraryMutating) return;
    cancelSourceDocumentDelete();
    editingSourceDocumentId = book.id;
    renameDraft = book.title;
    await tick();
    renameInput?.focus();
    renameInput?.select();
  }

  function cancelSourceDocumentRename() {
    if (renamingSourceDocumentId !== null) return;
    editingSourceDocumentId = null;
    renameDraft = "";
  }

  async function saveSourceDocumentRename(event: SubmitEvent, book: SourceDocument) {
    event.preventDefault();
    if (renamingSourceDocumentId !== null) return;

    const saved = await renameSourceDocument(book, renameDraft);
    if (saved) {
      editingSourceDocumentId = null;
      renameDraft = "";
    }
  }

  function handleRenameKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    event.preventDefault();
    cancelSourceDocumentRename();
  }

  function toggleSettingsMenu() {
    settingsMenuOpen = !settingsMenuOpen;
  }

  function closeSettingsMenu() {
    settingsMenuOpen = false;
  }

  function handleSettingsWindowClick(event: MouseEvent) {
    if (!settingsMenuOpen || !settingsMenuElement) return;

    const target = event.target;
    if (target instanceof Node && settingsMenuElement.contains(target)) return;
    closeSettingsMenu();
  }

  function handleSettingsWindowKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    closeSettingsMenu();
  }

  function openAiSettingsFromMenu() {
    closeSettingsMenu();
    openAiSettings();
  }

  function runUpdateActionFromMenu() {
    if (updateChecking || updateInstalling) return;
    closeSettingsMenu();
    if (updateCheck?.available) {
      installUpdate();
      return;
    }
    checkForUpdates();
  }

  function openLocalSyncFromMenu() {
    if (libraryMutating || editingSourceDocumentId !== null) return;
    closeSettingsMenu();
    openLocalSyncSheet();
  }
</script>

<svelte:window onclick={handleSettingsWindowClick} onkeydown={handleSettingsWindowKeydown} />

<div class="library">
  <div class="library-header">
    <h1>Gloss</h1>
    <div class="library-actions">
      <div class="import-mode-group">
        <button
          onclick={() => void createBlankNotebook()}
          disabled={libraryMutating || editingSourceDocumentId !== null}
          class="import-btn import-btn-notebook"
        >
          {creatingNotebook ? "Creating..." : "New Notebook"}
        </button>
        <button
          onclick={() => void importPdf("textbook")}
          disabled={libraryMutating || editingSourceDocumentId !== null}
          class="import-btn import-btn-secondary"
        >
          {importing ? "Importing..." : "Import Textbook"}
        </button>
        <button
          onclick={() => void importPdf("past_paper")}
          disabled={libraryMutating || editingSourceDocumentId !== null}
          class="import-btn"
        >
          {importing ? "Importing..." : "Import Past Paper"}
        </button>
      </div>
      <div class="settings-menu" bind:this={settingsMenuElement}>
        <button
          class="settings-menu-btn"
          class:needs-attention={settingsNeedsAttention}
          type="button"
          onclick={toggleSettingsMenu}
          aria-haspopup="menu"
          aria-expanded={settingsMenuOpen}
        >
          <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.05.05a2 2 0 0 1-2.83 2.83l-.05-.05a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.04 1.56V21a2 2 0 0 1-4 0v-.08a1.7 1.7 0 0 0-1.04-1.56 1.7 1.7 0 0 0-1.87.34l-.05.05a2 2 0 1 1-2.83-2.83l.05-.05A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.04H3a2 2 0 0 1 0-4h.04A1.7 1.7 0 0 0 4.6 8.92a1.7 1.7 0 0 0-.34-1.87l-.05-.05a2 2 0 1 1 2.83-2.83l.05.05a1.7 1.7 0 0 0 1.87.34A1.7 1.7 0 0 0 10 3V3a2 2 0 0 1 4 0v.08a1.7 1.7 0 0 0 1.04 1.56 1.7 1.7 0 0 0 1.87-.34l.05-.05a2 2 0 0 1 2.83 2.83l-.05.05a1.7 1.7 0 0 0-.34 1.87 1.7 1.7 0 0 0 1.56 1.04H21a2 2 0 0 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z" />
          </svg>
          Settings
          {#if settingsNeedsAttention}
            <span class="settings-attention-dot" aria-hidden="true"></span>
          {/if}
        </button>

        {#if settingsMenuOpen}
          <div class="settings-popover" role="menu" aria-label="Settings">
            <button class="settings-menu-item" type="button" role="menuitem" onclick={openAiSettingsFromMenu}>
              <span class="settings-item-icon settings-item-icon-text">AI</span>
              <span class="settings-item-copy">
                <strong>AI settings</strong>
                <span>Providers and API keys</span>
              </span>
            </button>

            <button
              class="settings-menu-item"
              type="button"
              role="menuitem"
              onclick={runUpdateActionFromMenu}
              disabled={updateChecking || updateInstalling}
            >
              <span class="settings-item-icon">
                <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path d="M20 12a8 8 0 1 1-2.34-5.66" />
                  <path d="M20 4v5h-5" />
                </svg>
              </span>
              <span class="settings-item-copy">
                <strong>Updates</strong>
                <span>{updateCheck?.available ? `Gloss ${updateCheck.latest_version}` : updateStatusLabel}</span>
              </span>
              <span class:settings-badge-alert={updateCheck?.available} class="settings-badge">
                {updateInstalling ? "Install" : updateCheck?.available ? "Install" : "Check"}
              </span>
            </button>

            <button
              class="settings-menu-item"
              type="button"
              role="menuitem"
              onclick={openLocalSyncFromMenu}
              disabled={libraryMutating || editingSourceDocumentId !== null}
            >
              <span class="settings-item-icon">
                <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path d="M17 2l4 4-4 4" />
                  <path d="M3 11V9a3 3 0 0 1 3-3h15" />
                  <path d="M7 22l-4-4 4-4" />
                  <path d="M21 13v2a3 3 0 0 1-3 3H3" />
                </svg>
              </span>
              <span class="settings-item-copy">
                <strong>Local sync</strong>
                <span>{syncFolderDisplay}</span>
              </span>
              <span class:settings-badge-alert={hasSyncConflicts || hasMissingPdfs || !!syncState?.dirty} class="settings-badge">{syncStatusLabel}</span>
            </button>
          </div>
        {/if}
      </div>
    </div>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {/if}
  {#if updateCheck?.available}
    <div class="update-banner">
      <div>
        <strong>Gloss {updateCheck.latest_version} is available.</strong>
        <span>{updateInstallProgress ?? updateCheck.release_name ?? "Download the latest release to update."}</span>
      </div>
      <div class="update-banner-actions">
        <button type="button" onclick={installUpdate} disabled={updateInstalling}>
          {updateInstalling ? "Installing..." : "Install"}
        </button>
        <button type="button" onclick={openUpdateRelease} disabled={updateInstalling}>Open release</button>
      </div>
    </div>
  {:else if updateError}
    <p class="db-transfer-hint">Update check failed: {updateError}</p>
  {:else if updateFeedback}
    <p class="db-transfer-feedback">{updateFeedback}</p>
  {/if}
  {#if syncError}
    <p class="error">{syncError}</p>
  {/if}
  {#if syncFeedback}
    <p class="db-transfer-feedback">{syncFeedback}</p>
  {/if}
  {#if syncState?.message}
    <p class="db-transfer-hint">{syncState.message}</p>
  {/if}

  {#if sourceDocuments.length === 0}
    <p class="empty">No documents yet. Create a notebook or import a PDF to get started.</p>
  {:else}
    <ul>
      {#each sourceDocuments as book (book.id)}
        <li>
          <div class="book-row">
            {#if editingSourceDocumentId === book.id}
              <form class="book-rename-form" onsubmit={(event) => void saveSourceDocumentRename(event, book)}>
                <div class="book-rename-fields">
                  <label class="sr-only" for={`book-rename-${book.id}`}>Book title</label>
                  <input
                    id={`book-rename-${book.id}`}
                    class="book-rename-input"
                    bind:this={renameInput}
                    bind:value={renameDraft}
                    maxlength="240"
                    disabled={renamingSourceDocumentId === book.id}
                    onkeydown={handleRenameKeydown}
                  />
                  <span class="path">{documentMeta(book)}</span>
                </div>
                <div class="book-rename-actions">
                  <button
                    class="book-rename-save"
                    type="submit"
                    disabled={renamingSourceDocumentId !== null || !renameDraft.trim()}
                  >
                    {renamingSourceDocumentId === book.id ? "Saving..." : "Save"}
                  </button>
                  <button
                    class="book-rename-cancel"
                    type="button"
                    onclick={cancelSourceDocumentRename}
                    disabled={renamingSourceDocumentId !== null}
                  >
                    Cancel
                  </button>
                </div>
              </form>
            {:else}
              <button
                class="book-item"
                onclick={() => openBook(book)}
                disabled={libraryMutating || editingSourceDocumentId !== null}
              >
                <span class="title">{book.title}</span>
                <span class="path">{documentMeta(book)}</span>
              </button>
              <div class="book-action-group">
                <button
                  class="book-rename"
                  type="button"
                  onclick={() => void beginSourceDocumentRename(book)}
                  disabled={libraryMutating || editingSourceDocumentId !== null}
                  aria-label={`Rename ${book.title}`}
                  title={`Rename ${book.title}`}
                >
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M12 20h9" />
                    <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
                  </svg>
                </button>
                <div class="book-delete-group">
                  {#if pendingDeleteSourceDocumentId === book.id}
                    <button
                      class="book-delete-confirm"
                      class:loading={deletingSourceDocumentId === book.id}
                      type="button"
                      onclick={() => void deleteSourceDocument(book)}
                      disabled={libraryMutating || editingSourceDocumentId !== null}
                      aria-label={`Confirm delete ${book.title}`}
                      title={`Confirm delete ${book.title}`}
                    >
                      {deletingSourceDocumentId === book.id ? "Deleting..." : "Confirm"}
                    </button>
                    <button
                      class="book-delete-cancel"
                      type="button"
                      onclick={cancelSourceDocumentDelete}
                      disabled={libraryMutating || editingSourceDocumentId !== null}
                      aria-label={`Cancel delete ${book.title}`}
                      title={`Cancel delete ${book.title}`}
                    >
                      Cancel
                    </button>
                  {:else}
                    <button
                      class="book-delete"
                      type="button"
                      onclick={() => requestSourceDocumentDelete(book.id)}
                      disabled={libraryMutating || editingSourceDocumentId !== null}
                      aria-label={`Delete ${book.title}`}
                      title={`Delete ${book.title}`}
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                        <polyline points="3 6 5 6 21 6" />
                        <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
                        <path d="M19 6l-1 14a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1L5 6" />
                        <line x1="10" y1="11" x2="10" y2="17" />
                        <line x1="14" y1="11" x2="14" y2="17" />
                      </svg>
                    </button>
                  {/if}
                </div>
              </div>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if showSyncSheet}
  <div class="sync-backdrop" role="presentation">
    <div class="sync-sheet" role="dialog" aria-modal="true" aria-labelledby="sync-title">
      <header class="sync-sheet-header">
        <div>
          <p class="sync-kicker">Syncthing</p>
          <h2 id="sync-title">Local sync</h2>
        </div>
        <button class="sync-close" type="button" onclick={closeLocalSyncSheet} disabled={syncBusy} aria-label="Close local sync">
          <svg viewBox="0 0 16 16" fill="none" width="14" height="14" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>
          </svg>
        </button>
      </header>

      <div class="sync-status-row">
        <span class:active={syncState?.enabled}>{syncStatusLabel}</span>
        {#if remoteRevision !== null}
          <span>Revision {remoteRevision}</span>
        {/if}
        {#if syncState?.dirty}
          <span>Local changes</span>
        {/if}
      </div>

      <p class="sync-message">{syncState?.message ?? "Choose a local folder to connect Gloss with Syncthing."}</p>

      <div class="sync-folder-row">
        <code>{syncFolderDisplay}</code>
        <button type="button" onclick={() => void chooseSyncFolder()} disabled={syncBusy}>
          {syncBusy && syncMode === "choose" ? "Connecting..." : syncState?.running_on_android ? "Choose folder" : "Choose"}
        </button>
        <button type="button" onclick={() => void enableSyncFolder()} disabled={syncBusy || !(syncFolderInput || syncState?.folder_path)}>
          {syncBusy && syncMode === "enable" ? "Connecting..." : syncState?.enabled ? "Reconnect" : "Connect"}
        </button>
      </div>

      <div class="sync-action-grid">
        <button type="button" onclick={() => void takeSyncEditingLease()} disabled={syncBusy || !syncState?.enabled}>
          {syncBusy && syncMode === "lease" ? "Taking..." : "Take editing lease"}
        </button>
        <button type="button" onclick={() => void syncNow()} disabled={syncBusy || !canSyncNow}>
          {syncBusy && syncMode === "sync" ? "Syncing..." : "Sync now"}
        </button>
        <button type="button" onclick={() => void importLatestSyncSnapshot()} disabled={syncBusy || !canImportLatest}>
          {syncBusy && syncMode === "import" ? "Importing..." : "Import latest"}
        </button>
        <button type="button" onclick={() => void disableSync()} disabled={syncBusy || !syncState?.enabled}>
          Disable
        </button>
      </div>

      {#if syncState?.active_writer}
        <div class="sync-warning">
          <strong>{syncState.active_writer.device_name}</strong> has the editing lease until {new Date(syncState.active_writer.lease_expires_unix * 1000).toLocaleTimeString()}.
        </div>
      {/if}

      {#if hasMissingPdfs}
        <div class="sync-warning">
          <strong>Waiting for PDFs</strong>
          <ul>
            {#each syncState?.missing_pdfs ?? [] as pdf}
              <li>{pdf}</li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if hasSyncConflicts}
        <div class="sync-warning">
          <strong>Conflicts</strong>
          <ul>
            {#each syncState?.conflicts ?? [] as conflict}
              <li>{conflict.path}</li>
            {/each}
          </ul>
          <div class="sync-conflict-actions">
            <button type="button" onclick={() => void resolveSyncConflict("use_remote")} disabled={syncBusy}>
              Use synced copy
            </button>
            <button type="button" onclick={() => void resolveSyncConflict("keep_local")} disabled={syncBusy}>
              Keep this device
            </button>
          </div>
        </div>
      {/if}

      {#if syncError}
        <p class="error">{syncError}</p>
      {/if}
      {#if syncFeedback}
        <p class="db-transfer-feedback">{syncFeedback}</p>
      {/if}

      <footer class="sync-footnote">
        Add this folder to Syncthing as Send & Receive, then enable Simple File Versioning with at least 5 versions. Gloss imports and publishes automatically when it is safe.
      </footer>
    </div>
  </div>
{/if}

<style>
  .library {
    max-width: 640px;
    margin: 0 auto;
    padding: 2rem 1.5rem;
  }

  .library-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.5rem;
  }

  .library-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    justify-content: flex-end;
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

  .import-mode-group {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .import-btn-secondary {
    background: #eef2f8;
    color: #243149;
    border: 1px solid #d4dce8;
  }

  .import-btn-notebook {
    background: #243149;
    color: #fff;
    border: 1px solid #243149;
  }

  .import-btn-secondary:hover:not(:disabled) {
    background: #e5ebf5;
  }

  .import-btn:hover:not(:disabled) {
    background: #333;
  }

  .import-btn:disabled {
    background: #888;
  }

  .import-btn-secondary:disabled {
    background: #eff3f8;
    color: #8a96aa;
  }

  .db-transfer-feedback {
    color: #1f5133;
    font-size: 0.9em;
    margin: 0.5rem 0 0;
  }

  .db-transfer-hint {
    margin: 0.3rem 0 0;
    color: #667085;
    font-size: 0.8rem;
  }

  .update-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin: 0.8rem 0 0;
    padding: 0.85rem 1rem;
    border: 1px solid #f2c46d;
    border-radius: 8px;
    background: #fff8e6;
    color: #4f3422;
  }

  .update-banner div {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }

  .update-banner strong {
    font-size: 0.92rem;
  }

  .update-banner span {
    color: #70513a;
    font-size: 0.82rem;
    overflow-wrap: anywhere;
  }

  .update-banner-actions {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    flex: 0 0 auto;
  }

  .update-banner-actions button {
    flex: 0 0 auto;
    padding: 0.45rem 0.8rem;
    border-radius: 6px;
    background: #1f2937;
    color: #fff;
    font-weight: 650;
  }

  .update-banner-actions button:last-child {
    background: #fff;
    color: #4f3422;
    border: 1px solid #e5ba66;
  }

  .settings-menu {
    position: relative;
  }

  .settings-menu-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.52rem 0.85rem;
    background: #f7f9f8;
    color: #243149;
    border: 1px solid #d8dee9;
    border-radius: 8px;
    font-size: 0.88rem;
    font-weight: 650;
    transition: background 0.15s, border-color 0.15s, color 0.15s;
  }

  .settings-menu-btn:hover:not(:disabled) {
    background: #edf2f6 !important;
    border-color: #cbd5e1;
  }

  .settings-menu-btn.needs-attention {
    background: #fff8e6;
    border-color: #f2c46d;
    color: #5f3e12;
  }

  .settings-menu-btn svg {
    width: 17px;
    height: 17px;
    flex: 0 0 auto;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .settings-attention-dot {
    width: 0.42rem;
    height: 0.42rem;
    border-radius: 999px;
    background: #d97706;
  }

  .settings-popover {
    position: absolute;
    top: calc(100% + 0.45rem);
    right: 0;
    z-index: 70;
    width: min(340px, calc(100vw - 2rem));
    padding: 0.45rem;
    border: 1px solid #d7dde7;
    border-radius: 10px;
    background: #fff;
    box-shadow: 0 18px 44px rgba(15, 23, 42, 0.16);
  }

  .settings-menu-item {
    width: 100%;
    min-height: 58px;
    display: grid;
    grid-template-columns: 34px minmax(0, 1fr) auto;
    gap: 0.65rem;
    align-items: center;
    padding: 0.65rem 0.7rem;
    border-radius: 8px;
    background: transparent;
    color: #243149;
    text-align: left;
    font: inherit;
    transition: background 0.15s, opacity 0.15s;
  }

  .settings-menu-item:hover:not(:disabled) {
    background: #f3f6fa !important;
  }

  .settings-menu-item:disabled {
    opacity: 0.55;
  }

  .settings-item-icon {
    width: 34px;
    height: 34px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 8px;
    background: #edf2f7;
    color: #34445c;
  }

  .settings-item-icon svg {
    width: 18px;
    height: 18px;
    stroke: currentColor;
    stroke-width: 1.9;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .settings-item-icon-text {
    font-size: 0.72rem;
    font-weight: 800;
    letter-spacing: 0;
  }

  .settings-item-copy {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.12rem;
  }

  .settings-item-copy strong {
    color: #1f2937;
    font-size: 0.9rem;
    font-weight: 750;
  }

  .settings-item-copy span {
    color: #667085;
    font-size: 0.78rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .settings-badge {
    max-width: 108px;
    padding: 0.16rem 0.42rem;
    border-radius: 999px;
    background: #eef2f6;
    color: #475569;
    font-size: 0.72rem;
    font-weight: 800;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .settings-badge-alert {
    background: #fff1d6;
    color: #9a3412;
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

  .book-row {
    display: flex;
    align-items: stretch;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .book-item {
    flex: 1;
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

  .book-item:hover:not(:disabled) {
    background: #e0e0e0 !important;
  }

  .book-action-group {
    display: flex;
    align-items: stretch;
  }

  .book-rename,
  .book-delete {
    width: 46px;
    min-width: 46px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: #efe4e6;
    color: #8f2532;
    border-left: 1px solid #dfc7cc;
    transition: background 0.15s, color 0.15s;
    font-weight: 700;
  }

  .book-rename {
    background: #e7ebf1;
    color: #34445c;
    border-left: 1px solid #d3d9e3;
  }

  .book-rename:hover:not(:disabled) {
    background: #dce3ed !important;
    color: #25354d;
  }

  .book-rename svg,
  .book-delete svg {
    width: 17px;
    height: 17px;
  }

  .book-delete:hover:not(:disabled) {
    background: #e8d4d8 !important;
    color: #7e1d2a;
  }

  .book-delete:disabled {
    opacity: 0.7;
  }

  .book-rename-form {
    flex: 1;
    display: flex;
    align-items: stretch;
    min-width: 0;
    width: 100%;
    background: #f8fafc;
  }

  .book-rename-fields {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.55rem 0.75rem;
  }

  .book-rename-input {
    width: 100%;
    min-height: 34px;
    padding: 0.35rem 0.5rem;
    border: 1px solid #cbd5e1;
    border-radius: 6px;
    background: #fff;
    color: #1f2937;
    font: inherit;
    font-weight: 650;
  }

  .book-rename-input:focus {
    outline: 2px solid rgba(59, 130, 246, 0.22);
    border-color: #8fb0dc;
  }

  .book-rename-actions {
    display: flex;
    align-items: stretch;
  }

  .book-delete-group {
    display: flex;
    align-items: stretch;
  }

  .book-rename-save,
  .book-rename-cancel,
  .book-delete-confirm,
  .book-delete-cancel {
    min-width: 72px;
    padding: 0 0.65rem;
    font-size: 0.78rem;
    font-weight: 700;
    border-left: 1px solid #d3d7de;
    transition: background 0.15s, color 0.15s, opacity 0.15s;
  }

  .book-rename-save {
    background: #e3f2ea;
    color: #1f6846;
  }

  .book-rename-save:hover:not(:disabled) {
    background: #d4eadf !important;
    color: #175739;
  }

  .book-rename-cancel {
    background: #e9edf3;
    color: #3f4d63;
  }

  .book-rename-cancel:hover:not(:disabled) {
    background: #dde4ee !important;
    color: #2f3d54;
  }

  .book-delete-confirm {
    background: #fbe6e8;
    color: #8f2532;
  }

  .book-delete-confirm:hover:not(:disabled) {
    background: #f6d7dc !important;
    color: #7e1d2a;
  }

  .book-delete-confirm.loading {
    color: #7a4b51;
  }

  .book-delete-cancel {
    background: #e9edf3;
    color: #3f4d63;
  }

  .book-delete-cancel:hover:not(:disabled) {
    background: #dde4ee !important;
    color: #2f3d54;
  }

  .book-rename-save:disabled,
  .book-rename-cancel:disabled,
  .book-delete-confirm:disabled,
  .book-delete-cancel:disabled {
    opacity: 0.72;
  }

  .title {
    font-weight: 600;
  }

  .path {
    font-size: 0.8em;
    color: #666;
    font-family: monospace;
  }

  .sync-backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
    background: rgba(15, 23, 42, 0.32);
  }

  .sync-sheet {
    width: min(780px, 100%);
    max-height: min(760px, calc(100vh - 2rem));
    overflow: auto;
    padding: 1.25rem;
    border-radius: 8px;
    background: #fff;
    box-shadow: 0 24px 70px rgba(15, 23, 42, 0.2);
  }

  .sync-sheet-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.9rem;
  }

  .sync-kicker {
    margin: 0 0 0.15rem;
    color: #607083;
    font-size: 0.75rem;
    font-weight: 750;
    text-transform: uppercase;
  }

  .sync-sheet h2 {
    margin: 0;
    font-size: 1.25rem;
  }

  .sync-close {
    width: 32px;
    height: 32px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border-radius: 6px;
    background: #f4f6f8;
    color: #334155;
  }

  .sync-status-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.45rem;
    margin-bottom: 0.8rem;
  }

  .sync-status-row span {
    padding: 0.2rem 0.5rem;
    border-radius: 999px;
    background: #eef2f6;
    color: #475569;
    font-size: 0.78rem;
    font-weight: 700;
  }

  .sync-status-row span.active {
    background: #e5f4ed;
    color: #216143;
  }

  .sync-message {
    margin: 0 0 1rem;
    color: #42526a;
    font-size: 0.92rem;
    line-height: 1.4;
  }

  .sync-folder-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 0.75rem;
  }

  .sync-folder-row code {
    min-height: 38px;
    display: flex;
    align-items: center;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 0.55rem 0.65rem;
    border: 1px solid #d6dde6;
    border-radius: 6px;
    background: #f8fafc;
    color: #334155;
    font-size: 0.8rem;
  }

  .sync-folder-row button,
  .sync-action-grid button,
  .sync-conflict-actions button {
    min-height: 38px;
    padding: 0.45rem 0.75rem;
    border: 1px solid #d6dde6;
    border-radius: 6px;
    background: #f7f9fb;
    color: #243149;
    font-weight: 700;
  }

  .sync-folder-row button:hover:not(:disabled),
  .sync-action-grid button:hover:not(:disabled),
  .sync-conflict-actions button:hover:not(:disabled) {
    background: #edf2f6 !important;
  }

  .sync-action-grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 0.5rem;
    margin-bottom: 0.9rem;
  }

  .sync-warning {
    margin-top: 0.7rem;
    padding: 0.75rem;
    border: 1px solid #ecd7a8;
    border-radius: 8px;
    background: #fff8e8;
    color: #63480e;
    font-size: 0.88rem;
  }

  .sync-warning ul {
    margin: 0.45rem 0 0;
    gap: 0.25rem;
  }

  .sync-warning li {
    padding: 0.35rem 0.45rem;
    background: rgba(255, 255, 255, 0.62);
    border-radius: 5px;
    overflow-wrap: anywhere;
  }

  .sync-conflict-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.65rem;
  }

  .sync-footnote {
    margin-top: 1rem;
    color: #64748b;
    font-size: 0.8rem;
    line-height: 1.4;
  }

  @media (max-width: 720px) {
    .library-header {
      flex-direction: column;
      align-items: flex-start;
    }

    .library-actions {
      width: 100%;
      justify-content: flex-start;
    }

    .import-mode-group {
      width: 100%;
    }

    .import-btn {
      flex: 1;
      text-align: center;
    }

    .settings-menu,
    .settings-menu-btn {
      width: 100%;
    }

    .settings-menu-btn {
      justify-content: center;
    }

    .settings-popover {
      left: 0;
      right: 0;
      width: 100%;
    }

    .sync-folder-row,
    .sync-action-grid {
      grid-template-columns: 1fr;
    }
  }
</style>

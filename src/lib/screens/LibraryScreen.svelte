<script lang="ts">
  import { tick } from "svelte";
  import type { DocumentMode, SourceDocument, SyncState } from "$lib/app/types";

  let {
    sourceDocuments,
    importing,
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
    openAiSettings,
    importPdf,
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
    openAiSettings: () => void;
    importPdf: (documentMode: DocumentMode) => Promise<void>;
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
    importing || syncBusy || deletingSourceDocumentId !== null || renamingSourceDocumentId !== null,
  );

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
</script>

<div class="library">
  <div class="library-header">
    <h1>Gloss</h1>
    <div class="library-actions">
      <button
        class="ai-settings-btn"
        onclick={openAiSettings}
        type="button"
      >
        AI settings
      </button>
      <div class="import-mode-group">
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
      <button
        class="sync-open-btn"
        type="button"
        onclick={openLocalSyncSheet}
        disabled={libraryMutating || editingSourceDocumentId !== null}
      >
        Local sync
        <span>{syncStatusLabel}</span>
      </button>
    </div>
  </div>

  {#if error}
    <p class="error">{error}</p>
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
    <p class="empty">No documents yet. Import a textbook or past paper to get started.</p>
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
                  <span class="path">{book.file_path}</span>
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
                <span class="path">{book.file_path}</span>
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

  .ai-settings-btn {
    padding: 0.52rem 0.85rem;
    background: #f6f7fa;
    color: #243149;
    border: 1px solid #d8dee9;
    border-radius: 8px;
    font-size: 0.88rem;
    font-weight: 650;
    transition: background 0.15s, border-color 0.15s;
  }

  .ai-settings-btn:hover:not(:disabled) {
    background: #eceff5 !important;
    border-color: #c9d2df;
  }

  .sync-open-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.52rem 0.85rem;
    background: #f7f9f8;
    color: #243149;
    border: 1px solid #d4ded9;
    border-radius: 8px;
    font-size: 0.88rem;
    font-weight: 650;
  }

  .sync-open-btn span {
    padding: 0.12rem 0.38rem;
    border-radius: 999px;
    background: #e8f3ee;
    color: #256247;
    font-size: 0.72rem;
    white-space: nowrap;
  }

  .sync-open-btn:hover:not(:disabled) {
    background: #edf4f1 !important;
    border-color: #c5d4cc;
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

    .sync-open-btn {
      width: 100%;
      justify-content: center;
    }

    .sync-folder-row,
    .sync-action-grid {
      grid-template-columns: 1fr;
    }
  }
</style>

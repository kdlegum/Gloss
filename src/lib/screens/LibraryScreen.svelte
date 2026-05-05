<script lang="ts">
  import type { DatabaseTransferMode, DocumentMode, SourceDocument } from "$lib/app/types";

  let {
    sourceDocuments,
    importing,
    dbTransferBusy,
    dbTransferMode,
    dbTransferError,
    dbTransferFeedback,
    pendingDeleteSourceDocumentId,
    deletingSourceDocumentId,
    error,
    openAiSettings,
    importPdf,
    exportDatabaseFile,
    importDatabaseFile,
    openBook,
    deleteSourceDocument,
    requestSourceDocumentDelete,
    cancelSourceDocumentDelete,
  }: {
    sourceDocuments: SourceDocument[];
    importing: boolean;
    dbTransferBusy: boolean;
    dbTransferMode: DatabaseTransferMode | null;
    dbTransferError: string | null;
    dbTransferFeedback: string | null;
    pendingDeleteSourceDocumentId: number | null;
    deletingSourceDocumentId: number | null;
    error: string | null;
    openAiSettings: () => void;
    importPdf: (documentMode: DocumentMode) => Promise<void>;
    exportDatabaseFile: () => Promise<void>;
    importDatabaseFile: () => Promise<void>;
    openBook: (book: SourceDocument) => void | Promise<void>;
    deleteSourceDocument: (book: SourceDocument) => Promise<void>;
    requestSourceDocumentDelete: (bookId: number) => void;
    cancelSourceDocumentDelete: () => void;
  } = $props();
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
          disabled={importing || dbTransferBusy}
          class="import-btn import-btn-secondary"
        >
          {importing ? "Importing..." : "Import Textbook"}
        </button>
        <button
          onclick={() => void importPdf("past_paper")}
          disabled={importing || dbTransferBusy}
          class="import-btn"
        >
          {importing ? "Importing..." : "Import Past Paper"}
        </button>
      </div>
      <div class="db-sync-group">
        <button
          class="import-btn import-btn-secondary"
          type="button"
          onclick={() => void exportDatabaseFile()}
          disabled={importing || deletingSourceDocumentId !== null || dbTransferBusy}
        >
          {dbTransferBusy && dbTransferMode === "export" ? "Exporting..." : "Export DB"}
        </button>
        <button
          class="import-btn import-btn-secondary"
          type="button"
          onclick={() => void importDatabaseFile()}
          disabled={importing || deletingSourceDocumentId !== null || dbTransferBusy}
        >
          {dbTransferBusy && dbTransferMode === "import" ? "Importing..." : "Import DB"}
        </button>
      </div>
    </div>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {/if}
  {#if dbTransferError}
    <p class="error">{dbTransferError}</p>
  {/if}
  {#if dbTransferFeedback}
    <p class="db-transfer-feedback">{dbTransferFeedback}</p>
  {/if}
  <p class="db-transfer-hint">
    DB export/import is for notes/settings sync. Keep PDF files synced separately.
  </p>

  {#if sourceDocuments.length === 0}
    <p class="empty">No documents yet. Import a textbook or past paper to get started.</p>
  {:else}
    <ul>
      {#each sourceDocuments as book (book.id)}
        <li>
          <div class="book-row">
            <button
              class="book-item"
              onclick={() => openBook(book)}
              disabled={deletingSourceDocumentId !== null || dbTransferBusy}
            >
              <span class="title">{book.title}</span>
              <span class="path">{book.file_path}</span>
            </button>
            <div class="book-delete-group">
              {#if pendingDeleteSourceDocumentId === book.id}
                <button
                  class="book-delete-confirm"
                  class:loading={deletingSourceDocumentId === book.id}
                  type="button"
                  onclick={() => void deleteSourceDocument(book)}
                  disabled={importing || dbTransferBusy || deletingSourceDocumentId !== null}
                  aria-label={`Confirm delete ${book.title}`}
                  title={`Confirm delete ${book.title}`}
                >
                  {deletingSourceDocumentId === book.id ? "Deleting..." : "Confirm"}
                </button>
                <button
                  class="book-delete-cancel"
                  type="button"
                  onclick={cancelSourceDocumentDelete}
                  disabled={dbTransferBusy || deletingSourceDocumentId !== null}
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
                  disabled={importing || dbTransferBusy || deletingSourceDocumentId !== null}
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
        </li>
      {/each}
    </ul>
  {/if}
</div>

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

  .import-mode-group,
  .db-sync-group {
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

  .book-delete-group {
    display: flex;
    align-items: stretch;
  }

  .book-delete-confirm,
  .book-delete-cancel {
    min-width: 72px;
    padding: 0 0.65rem;
    font-size: 0.78rem;
    font-weight: 700;
    border-left: 1px solid #d3d7de;
    transition: background 0.15s, color 0.15s, opacity 0.15s;
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

  @media (max-width: 720px) {
    .library-header {
      flex-direction: column;
      align-items: flex-start;
    }

    .library-actions {
      width: 100%;
      justify-content: flex-start;
    }

    .import-mode-group,
    .db-sync-group {
      width: 100%;
    }

    .import-btn {
      flex: 1;
      text-align: center;
    }
  }
</style>

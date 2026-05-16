<script lang="ts">
  import {
    CHAT_PROVIDER_OPTIONS,
    CHUNKING_PROVIDER_OPTIONS,
  } from "$lib/app/ai";
  import type {
    AiSettingsState,
    AiTaskSettings,
    ChatProvider,
    ChunkingProvider,
    SourceDocument,
  } from "$lib/app/types";

  let {
    selectedBook,
    aiTaskSettings,
    aiSettings,
    setTaskProvider,
    openAiSettings,
    reChunkCurrentPage,
    canRechunkPage,
    reChunkingPage,
    closeViewer,
  }: {
    selectedBook: SourceDocument;
    aiTaskSettings: AiTaskSettings;
    aiSettings: AiSettingsState | null;
    setTaskProvider: (task: "chunking" | "chat", provider: ChunkingProvider | ChatProvider) => void;
    openAiSettings: () => void;
    reChunkCurrentPage: () => Promise<void>;
    canRechunkPage: boolean;
    reChunkingPage: boolean;
    closeViewer: () => void;
  } = $props();

  const isNotebook = $derived(selectedBook.document_mode === "notebook");
</script>

<div class="viewer-header">
  <button class="back-btn" onclick={closeViewer} aria-label="Back to library">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
      <polyline points="15 18 9 12 15 6"/>
    </svg>
  </button>
  <span class="viewer-title">{selectedBook.title}</span>
  <div class="viewer-header-actions">
    {#if !isNotebook}
      <div class="provider-shortcuts">
        <label class="provider-shortcut">
          <span>CK</span>
          <select
            value={aiTaskSettings.chunking.provider}
            onchange={(event) => setTaskProvider("chunking", (event.currentTarget as HTMLSelectElement).value as ChunkingProvider)}
          >
            {#each CHUNKING_PROVIDER_OPTIONS as option}
              {#if !aiSettings?.running_on_android || option.value !== "ollama"}
                <option value={option.value}>{option.short}</option>
              {/if}
            {/each}
          </select>
        </label>
        <label class="provider-shortcut">
          <span>AI</span>
          <select
            value={aiTaskSettings.chat.provider}
            onchange={(event) => setTaskProvider("chat", (event.currentTarget as HTMLSelectElement).value as ChatProvider)}
          >
            {#each CHAT_PROVIDER_OPTIONS as option}
              {#if !aiSettings?.running_on_android || option.value !== "ollama"}
                <option value={option.value}>{option.short}</option>
              {/if}
            {/each}
          </select>
        </label>
      </div>
    {/if}
    <button
      class="ai-settings-btn"
      onclick={openAiSettings}
      type="button"
    >
      AI settings
    </button>
    {#if !isNotebook}
      <button
        class="rechunk-btn"
        onclick={() => void reChunkCurrentPage()}
        disabled={!canRechunkPage}
        type="button"
      >
        {reChunkingPage ? "Re-chunking..." : "Re-chunk page"}
      </button>
    {/if}
  </div>
</div>

<style>
  .viewer-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #ddd;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  .viewer-title {
    flex: 1 1 220px;
    min-width: 0;
    font-weight: 600;
    font-size: 1.05em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .viewer-header-actions {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }

  .provider-shortcuts {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    flex-wrap: wrap;
  }

  .provider-shortcut {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.22rem 0.35rem;
    border: 1px solid #d7dde7;
    border-radius: 9px;
    background: #f8fafe;
    color: #445166;
    font-size: 0.74rem;
    font-weight: 700;
  }

  .provider-shortcut span {
    letter-spacing: 0.01em;
  }

  .provider-shortcut select {
    border: 1px solid #d6dde8;
    border-radius: 7px;
    background: #fff;
    color: #334155;
    padding: 0.18rem 0.3rem;
    min-height: 26px;
    font-size: 0.74rem;
    font-weight: 700;
  }

  .rechunk-btn {
    padding: 0.55rem 0.9rem;
    background: #eef2f8;
    color: #1f2f49;
    border: 1px solid #d6dde8;
    border-radius: 10px;
    font-size: 0.88rem;
    font-weight: 600;
    transition: background 0.15s, border-color 0.15s, opacity 0.15s;
  }

  .rechunk-btn:hover:not(:disabled) {
    background: #e5ebf5 !important;
    border-color: #c5cfdd;
  }

  .rechunk-btn:disabled {
    opacity: 0.55;
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

  @media (max-width: 720px) {
    .viewer-header-actions {
      width: 100%;
      justify-content: space-between;
      margin-left: 0;
    }

    .provider-shortcuts {
      flex: 1 1 100%;
    }
  }
</style>

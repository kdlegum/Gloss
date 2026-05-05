<script lang="ts">
  import {
    CUSTOM_MODEL_VALUE,
    CHAT_PROVIDER_OPTIONS,
    CHUNKING_PROVIDER_OPTIONS,
    VISION_PROVIDER_OPTIONS,
    getModelOptions,
  } from "$lib/app/ai";
  import type {
    AiSettingsState,
    AiTask,
    AiTaskSettings,
    ChatProvider,
    ChunkingProvider,
    SourceDocument,
    ViewerBatchSettingsState,
    VisionProvider,
  } from "$lib/app/types";
  import { fade } from "svelte/transition";

  let {
    show,
    aiKeySheetFirstRun,
    aiSettingsSaving,
    aiSettingsError,
    aiTaskSettings,
    aiSettings,
    viewerState = null,
    openaiApiKeyInput = $bindable(""),
    geminiApiKeyInput = $bindable(""),
    deepseekApiKeyInput = $bindable(""),
    zaiApiKeyInput = $bindable(""),
    clearOpenaiApiKey = $bindable(false),
    clearGeminiApiKey = $bindable(false),
    clearDeepseekApiKey = $bindable(false),
    clearZaiApiKey = $bindable(false),
    dismissAiKeySettings,
    saveAiKeyForm,
    setTaskProvider,
    modelSelectValue,
    setModelFromSelect,
    setTaskModel,
  }: {
    show: boolean;
    aiKeySheetFirstRun: boolean;
    aiSettingsSaving: boolean;
    aiSettingsError: string | null;
    aiTaskSettings: AiTaskSettings;
    aiSettings: AiSettingsState | null;
    viewerState?: ViewerBatchSettingsState | null;
    openaiApiKeyInput?: string;
    geminiApiKeyInput?: string;
    deepseekApiKeyInput?: string;
    zaiApiKeyInput?: string;
    clearOpenaiApiKey?: boolean;
    clearGeminiApiKey?: boolean;
    clearDeepseekApiKey?: boolean;
    clearZaiApiKey?: boolean;
    dismissAiKeySettings: () => void;
    saveAiKeyForm: (event: SubmitEvent) => void;
    setTaskProvider: (task: AiTask, provider: ChunkingProvider | ChatProvider | VisionProvider) => void;
    modelSelectValue: (task: AiTask) => string;
    setModelFromSelect: (task: AiTask, value: string) => void;
    setTaskModel: (task: AiTask, model: string) => void;
  } = $props();

  const closeDisabled = $derived(aiSettingsSaving || !!viewerState?.batchChunkStarting);
  const viewerBook = $derived(viewerState?.selectedBook ?? null);
</script>

{#if show}
  <div class="ai-key-backdrop">
    <div
      class="ai-key-sheet"
      role="dialog"
      aria-modal="true"
      aria-labelledby="ai-key-title"
      transition:fade={{ duration: 120 }}
    >
      <div class="ai-key-heading">
        <div>
          <p class="ai-key-kicker">{aiKeySheetFirstRun ? "First run" : "Settings"}</p>
          <h2 id="ai-key-title">AI settings</h2>
        </div>
        <button
          class="ai-key-close"
          type="button"
          onclick={dismissAiKeySettings}
          disabled={closeDisabled}
          aria-label="Close AI settings"
        >
          <svg viewBox="0 0 16 16" fill="none" width="14" height="14">
            <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>
          </svg>
        </button>
      </div>

      <p class="ai-key-copy">
        Choose provider + model defaults for chunking, chat, and vision transcription. Keys are optional and can also be provided via environment variables.
      </p>

      {#if aiSettingsError}
        <p class="ai-key-error">{aiSettingsError}</p>
      {/if}

      <form class="ai-key-form" onsubmit={saveAiKeyForm}>
        <section class="ai-task-section">
          <h3>Task defaults</h3>

          <div class="ai-task-row">
            <p>Chunking</p>
            <label class="ai-task-control">
              <span>Provider</span>
              <select
                value={aiTaskSettings.chunking.provider}
                onchange={(event) => setTaskProvider("chunking", (event.currentTarget as HTMLSelectElement).value as ChunkingProvider)}
                disabled={aiSettingsSaving}
              >
                {#each CHUNKING_PROVIDER_OPTIONS as option}
                  {#if !aiSettings?.running_on_android || option.value !== "ollama"}
                    <option value={option.value}>{option.label}</option>
                  {/if}
                {/each}
              </select>
            </label>
            <label class="ai-task-control">
              <span>Model</span>
              <select
                value={modelSelectValue("chunking")}
                onchange={(event) => setModelFromSelect("chunking", (event.currentTarget as HTMLSelectElement).value)}
                disabled={aiSettingsSaving}
              >
                {#each getModelOptions("chunking", aiTaskSettings.chunking.provider) as option}
                  <option value={option.value}>{option.label}{option.legacy ? " (legacy)" : ""}</option>
                {/each}
                <option value={CUSTOM_MODEL_VALUE}>Custom...</option>
              </select>
            </label>
            {#if modelSelectValue("chunking") === CUSTOM_MODEL_VALUE}
              <input
                class="ai-custom-model-input"
                type="text"
                spellcheck="false"
                value={aiTaskSettings.chunking.model}
                oninput={(event) => setTaskModel("chunking", (event.currentTarget as HTMLInputElement).value)}
                disabled={aiSettingsSaving}
                placeholder="Enter model ID"
              />
            {/if}
          </div>

          <div class="ai-task-row">
            <p>Chat</p>
            <label class="ai-task-control">
              <span>Provider</span>
              <select
                value={aiTaskSettings.chat.provider}
                onchange={(event) => setTaskProvider("chat", (event.currentTarget as HTMLSelectElement).value as ChatProvider)}
                disabled={aiSettingsSaving}
              >
                {#each CHAT_PROVIDER_OPTIONS as option}
                  {#if !aiSettings?.running_on_android || option.value !== "ollama"}
                    <option value={option.value}>{option.label}</option>
                  {/if}
                {/each}
              </select>
            </label>
            <label class="ai-task-control">
              <span>Model</span>
              <select
                value={modelSelectValue("chat")}
                onchange={(event) => setModelFromSelect("chat", (event.currentTarget as HTMLSelectElement).value)}
                disabled={aiSettingsSaving}
              >
                {#each getModelOptions("chat", aiTaskSettings.chat.provider) as option}
                  <option value={option.value}>{option.label}{option.legacy ? " (legacy)" : ""}</option>
                {/each}
                <option value={CUSTOM_MODEL_VALUE}>Custom...</option>
              </select>
            </label>
            {#if modelSelectValue("chat") === CUSTOM_MODEL_VALUE}
              <input
                class="ai-custom-model-input"
                type="text"
                spellcheck="false"
                value={aiTaskSettings.chat.model}
                oninput={(event) => setTaskModel("chat", (event.currentTarget as HTMLInputElement).value)}
                disabled={aiSettingsSaving}
                placeholder="Enter model ID"
              />
            {/if}
          </div>

          <div class="ai-task-row">
            <p>Vision OCR</p>
            <label class="ai-task-control">
              <span>Provider</span>
              <select
                value={aiTaskSettings.vision.provider}
                onchange={(event) => setTaskProvider("vision", (event.currentTarget as HTMLSelectElement).value as VisionProvider)}
                disabled={aiSettingsSaving}
              >
                {#each VISION_PROVIDER_OPTIONS as option}
                  {#if !aiSettings?.running_on_android || option.value !== "ollama"}
                    <option value={option.value}>{option.label}</option>
                  {/if}
                {/each}
              </select>
            </label>
            <label class="ai-task-control">
              <span>Model</span>
              <select
                value={modelSelectValue("vision")}
                onchange={(event) => setModelFromSelect("vision", (event.currentTarget as HTMLSelectElement).value)}
                disabled={aiSettingsSaving}
              >
                {#each getModelOptions("vision", aiTaskSettings.vision.provider) as option}
                  <option value={option.value}>{option.label}{option.legacy ? " (legacy)" : ""}</option>
                {/each}
                <option value={CUSTOM_MODEL_VALUE}>Custom...</option>
              </select>
            </label>
            {#if modelSelectValue("vision") === CUSTOM_MODEL_VALUE}
              <input
                class="ai-custom-model-input"
                type="text"
                spellcheck="false"
                value={aiTaskSettings.vision.model}
                oninput={(event) => setTaskModel("vision", (event.currentTarget as HTMLInputElement).value)}
                disabled={aiSettingsSaving}
                placeholder="Enter model ID"
              />
            {/if}
          </div>

          <p class="ai-model-note">
            `deepseek-reasoner` is legacy and scheduled for deprecation on July 24, 2026.
          </p>
        </section>

        <section class="ai-batch-section">
          <h3>Batch chunking</h3>
          {#if viewerBook?.document_mode === "past_paper" && viewerState}
            <div class="ai-pastpaper-range">
              <p class="ai-batch-copy">
                Instruction pages are used to extract exam-level rules and mark hints before question chunking.
              </p>
              <label class="ai-batch-toggle">
                <input
                  type="checkbox"
                  checked={viewerState.pastPaperInstructionPagesEnabled}
                  onchange={(event) => viewerState.setPastPaperInstructionPagesEnabled((event.currentTarget as HTMLInputElement).checked)}
                  disabled={viewerState.pastPaperInstructionSaving}
                />
                This paper has instruction pages
              </label>
              {#if viewerState.pastPaperInstructionPagesEnabled}
                <div class="ai-pastpaper-range-grid">
                  <label class="ai-batch-field">
                    <span>Instruction from</span>
                    <input
                      type="text"
                      inputmode="numeric"
                      pattern="[0-9]*"
                      value={viewerState.pastPaperInstructionStartInput}
                      oninput={(event) => viewerState.setPastPaperInstructionStartInput((event.currentTarget as HTMLInputElement).value)}
                      disabled={viewerState.pastPaperInstructionSaving}
                    />
                  </label>
                  <label class="ai-batch-field">
                    <span>Instruction to</span>
                    <input
                      type="text"
                      inputmode="numeric"
                      pattern="[0-9]*"
                      value={viewerState.pastPaperInstructionEndInput}
                      oninput={(event) => viewerState.setPastPaperInstructionEndInput((event.currentTarget as HTMLInputElement).value)}
                      disabled={viewerState.pastPaperInstructionSaving}
                    />
                  </label>
                </div>
              {/if}
              <button
                class="ai-batch-btn ai-batch-btn-secondary"
                type="button"
                onclick={() => void viewerState.savePastPaperInstructionRange()}
                disabled={viewerState.pastPaperInstructionSaving}
              >
                {viewerState.pastPaperInstructionSaving ? "Saving..." : "Save instruction pages"}
              </button>
              <button
                class="ai-batch-btn ai-batch-btn-secondary"
                type="button"
                onclick={() => void viewerState.checkPastPaperInstructionContext()}
                disabled={viewerState.pastPaperInstructionCheckLoading}
              >
                {viewerState.pastPaperInstructionCheckLoading ? "Checking..." : "Check instruction context"}
              </button>
              {#if viewerState.pastPaperInstructionError}
                <p class="ai-batch-error">{viewerState.pastPaperInstructionError}</p>
              {/if}
              {#if viewerState.pastPaperInstructionFeedback}
                <p class="ai-batch-feedback">{viewerState.pastPaperInstructionFeedback}</p>
              {/if}
              {#if viewerState.pastPaperInstructionCheckError}
                <p class="ai-batch-error">{viewerState.pastPaperInstructionCheckError}</p>
              {/if}
              {#if viewerState.pastPaperInstructionCheckFeedback}
                <p class="ai-batch-feedback">{viewerState.pastPaperInstructionCheckFeedback}</p>
              {/if}
              {#if viewerState.pastPaperInstructionCheckPreview}
                <div class="ai-pastpaper-preview">
                  <p class="ai-pastpaper-preview-title">Instruction preview</p>
                  <p class="ai-pastpaper-preview-body">{viewerState.pastPaperInstructionCheckPreview}</p>
                </div>
              {/if}
            </div>
          {/if}
          {#if viewerBook && viewerState && viewerState.totalPages > 0}
            <p class="ai-batch-copy">
              Run chunking over a page range for <strong>{viewerBook.title}</strong>. Uses the currently selected Chunking provider/model.
            </p>
            <label class="ai-batch-toggle">
              <input
                type="checkbox"
                checked={viewerState.batchChunkSkipChunkedPages}
                onchange={(event) => viewerState.setBatchChunkSkipChunkedPages((event.currentTarget as HTMLInputElement).checked)}
                disabled={!viewerState.canBatchChunkDocument}
              />
              Skip already chunked pages
            </label>
            <div class="ai-batch-grid">
              <label class="ai-batch-field">
                <span>From</span>
                <input
                  type="text"
                  inputmode="numeric"
                  pattern="[0-9]*"
                  value={viewerState.batchChunkStartInput}
                  oninput={(event) => viewerState.setBatchChunkStartInput((event.currentTarget as HTMLInputElement).value)}
                  disabled={!viewerState.canBatchChunkDocument}
                />
              </label>
              <label class="ai-batch-field">
                <span>To</span>
                <input
                  type="text"
                  inputmode="numeric"
                  pattern="[0-9]*"
                  value={viewerState.batchChunkEndInput}
                  oninput={(event) => viewerState.setBatchChunkEndInput((event.currentTarget as HTMLInputElement).value)}
                  disabled={!viewerState.canBatchChunkDocument}
                />
              </label>
              <button
                class="ai-batch-btn ai-batch-btn-primary"
                type="button"
                onclick={() => void viewerState.chunkPageRangeFromInputs()}
                disabled={!viewerState.canBatchChunkDocument}
              >
                {viewerState.batchChunkStarting ? "Starting..." : "Chunk range"}
              </button>
              <button
                class="ai-batch-btn ai-batch-btn-secondary"
                type="button"
                onclick={() => void viewerState.chunkWholePdf()}
                disabled={!viewerState.canBatchChunkDocument}
              >
                Chunk whole PDF
              </button>
            </div>
            <p class="ai-batch-hint">Range must be between 1 and {viewerState.totalPages}.</p>
          {:else}
            <p class="ai-batch-copy">Open a PDF in the viewer to enable batch chunking.</p>
          {/if}
          {#if viewerState?.batchChunkError}
            <p class="ai-batch-error">{viewerState.batchChunkError}</p>
          {/if}
          {#if viewerState?.batchChunkFeedback}
            <p class="ai-batch-feedback">{viewerState.batchChunkFeedback}</p>
          {/if}
        </section>

        <section class="ai-keys-section">
          <h3>API keys</h3>

          <label class="ai-key-field">
            <span>
              OpenAI API key
              {#if aiSettings?.openai_api_key_set}
                <em>Saved</em>
              {/if}
            </span>
            <input
              type="password"
              autocomplete="off"
              spellcheck="false"
              placeholder={aiSettings?.openai_api_key_set ? "Leave blank to keep saved key" : "sk-..."}
              bind:value={openaiApiKeyInput}
              disabled={clearOpenaiApiKey || aiSettingsSaving}
            />
          </label>

          {#if aiSettings?.openai_api_key_set}
            <label class="ai-key-clear">
              <input
                type="checkbox"
                bind:checked={clearOpenaiApiKey}
                disabled={aiSettingsSaving}
              />
              Clear OpenAI key
            </label>
          {/if}

          <label class="ai-key-field">
            <span>
              Gemini API key
              {#if aiSettings?.gemini_api_key_set}
                <em>Saved</em>
              {/if}
            </span>
            <input
              type="password"
              autocomplete="off"
              spellcheck="false"
              placeholder={aiSettings?.gemini_api_key_set ? "Leave blank to keep saved key" : "AIza..."}
              bind:value={geminiApiKeyInput}
              disabled={clearGeminiApiKey || aiSettingsSaving}
            />
          </label>

          {#if aiSettings?.gemini_api_key_set}
            <label class="ai-key-clear">
              <input
                type="checkbox"
                bind:checked={clearGeminiApiKey}
                disabled={aiSettingsSaving}
              />
              Clear Gemini key
            </label>
          {/if}

          <label class="ai-key-field">
            <span>
              DeepSeek API key
              {#if aiSettings?.deepseek_api_key_set}
                <em>Saved</em>
              {/if}
            </span>
            <input
              type="password"
              autocomplete="off"
              spellcheck="false"
              placeholder={aiSettings?.deepseek_api_key_set ? "Leave blank to keep saved key" : "sk-..."}
              bind:value={deepseekApiKeyInput}
              disabled={clearDeepseekApiKey || aiSettingsSaving}
            />
          </label>

          {#if aiSettings?.deepseek_api_key_set}
            <label class="ai-key-clear">
              <input
                type="checkbox"
                bind:checked={clearDeepseekApiKey}
                disabled={aiSettingsSaving}
              />
              Clear DeepSeek key
            </label>
          {/if}

          <label class="ai-key-field">
            <span>
              Z.AI API key
              {#if aiSettings?.zai_api_key_set}
                <em>Saved</em>
              {/if}
            </span>
            <input
              type="password"
              autocomplete="off"
              spellcheck="false"
              placeholder={aiSettings?.zai_api_key_set ? "Leave blank to keep saved key" : "zai_..."}
              bind:value={zaiApiKeyInput}
              disabled={clearZaiApiKey || aiSettingsSaving}
            />
          </label>

          {#if aiSettings?.zai_api_key_set}
            <label class="ai-key-clear">
              <input
                type="checkbox"
                bind:checked={clearZaiApiKey}
                disabled={aiSettingsSaving}
              />
              Clear Z.AI key
            </label>
          {/if}
        </section>

        <div class="ai-key-actions">
          <button
            class="ai-key-secondary"
            type="button"
            onclick={dismissAiKeySettings}
            disabled={closeDisabled}
          >
            {aiKeySheetFirstRun ? "Skip" : "Cancel"}
          </button>
          <button class="ai-key-primary" type="submit" disabled={closeDisabled}>
            {aiSettingsSaving ? "Saving..." : "Save"}
          </button>
        </div>
      </form>
    </div>
  </div>
{/if}

<style>
  .ai-key-backdrop {
    position: fixed;
    inset: 0;
    z-index: 300;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
    background: rgba(15, 23, 42, 0.38);
    backdrop-filter: blur(5px);
  }

  .ai-key-sheet {
    width: min(780px, 100%);
    max-height: min(760px, 100%);
    overflow: auto;
    background: #fff;
    border: 1px solid rgba(35, 46, 68, 0.16);
    border-radius: 14px;
    box-shadow: 0 24px 70px rgba(15, 23, 42, 0.28);
    padding: 1.2rem;
  }

  .ai-key-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
  }

  .ai-key-kicker {
    margin: 0 0 0.15rem;
    color: #667085;
    font-size: 0.78rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  .ai-key-heading h2 {
    margin: 0;
    color: #111827;
    font-size: 1.35rem;
    letter-spacing: 0;
  }

  .ai-key-close {
    width: 32px;
    height: 32px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    color: #475467;
    background: #f4f6f8;
    border: 1px solid #d9dee8;
    border-radius: 8px;
  }

  .ai-key-close:hover:not(:disabled) {
    background: #e9edf3 !important;
  }

  .ai-key-copy {
    margin: 0.75rem 0 1rem;
    color: #526071;
    line-height: 1.45;
  }

  .ai-key-error {
    margin: 0 0 0.9rem;
    padding: 0.7rem 0.8rem;
    color: #991b1b;
    background: #fef2f2;
    border: 1px solid #fecaca;
    border-radius: 8px;
    font-size: 0.88rem;
  }

  .ai-key-form {
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
  }

  .ai-task-section,
  .ai-batch-section,
  .ai-keys-section {
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
    padding: 0.8rem;
    border: 1px solid #d7dde7;
    border-radius: 10px;
    background: #fafbfd;
  }

  .ai-task-section h3,
  .ai-batch-section h3,
  .ai-keys-section h3 {
    margin: 0;
    font-size: 0.92rem;
    color: #24324a;
    font-weight: 700;
  }

  .ai-task-row {
    display: grid;
    grid-template-columns: 108px 1fr 1fr;
    gap: 0.55rem;
    align-items: end;
    padding: 0.55rem 0.1rem;
    border-top: 1px solid #e4e8f0;
  }

  .ai-task-row:first-of-type {
    border-top: none;
    padding-top: 0.2rem;
  }

  .ai-task-row p {
    margin: 0;
    font-size: 0.86rem;
    font-weight: 700;
    color: #374151;
  }

  .ai-task-control {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.78rem;
    color: #556274;
    font-weight: 650;
  }

  .ai-task-control select {
    width: 100%;
    min-height: 38px;
    padding: 0.45rem 0.55rem;
    color: #0f172a;
    border: 1px solid #cfd6e2;
    border-radius: 8px;
    background: #fff;
    font: inherit;
  }

  .ai-custom-model-input {
    grid-column: 2 / span 2;
    width: 100%;
    min-height: 38px;
    padding: 0.45rem 0.55rem;
    color: #0f172a;
    border: 1px solid #cfd6e2;
    border-radius: 8px;
    background: #fff;
    font: inherit;
  }

  .ai-model-note {
    margin: 0.15rem 0 0;
    color: #667085;
    font-size: 0.8rem;
    line-height: 1.35;
  }

  .ai-batch-copy {
    margin: 0;
    color: #526071;
    line-height: 1.4;
  }

  .ai-pastpaper-range {
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    padding: 0.65rem;
    border: 1px solid #dce3ee;
    border-radius: 9px;
    background: #f6f9ff;
  }

  .ai-pastpaper-range-grid {
    display: grid;
    grid-template-columns: 140px 140px;
    gap: 0.55rem;
    align-items: end;
  }

  .ai-pastpaper-preview {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    margin: 0;
    padding: 0.6rem;
    border: 1px dashed #ccd7ea;
    border-radius: 8px;
    background: #ffffff;
  }

  .ai-pastpaper-preview-title {
    margin: 0;
    color: #4b5b73;
    font-size: 0.78rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .ai-pastpaper-preview-body {
    margin: 0;
    color: #243041;
    font-size: 0.84rem;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .ai-batch-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    color: #374151;
    font-size: 0.86rem;
    font-weight: 600;
  }

  .ai-batch-toggle input {
    width: 16px;
    height: 16px;
  }

  .ai-batch-grid {
    display: grid;
    grid-template-columns: 110px 110px minmax(118px, auto) minmax(150px, auto);
    gap: 0.55rem;
    align-items: end;
  }

  .ai-batch-field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.78rem;
    color: #556274;
    font-weight: 650;
  }

  .ai-batch-field input {
    width: 100%;
    min-height: 38px;
    padding: 0.45rem 0.55rem;
    color: #0f172a;
    border: 1px solid #cfd6e2;
    border-radius: 8px;
    background: #fff;
    font: inherit;
  }

  .ai-batch-btn {
    min-height: 38px;
    padding: 0.5rem 0.8rem;
    border-radius: 8px;
    font: inherit;
    font-weight: 700;
    border: 1px solid #d6dde8;
    transition: background 0.15s, border-color 0.15s, opacity 0.15s;
  }

  .ai-batch-btn-primary {
    background: #1f2f49;
    border-color: #1f2f49;
    color: #fff;
  }

  .ai-batch-btn-primary:hover:not(:disabled) {
    background: #2d4265 !important;
    border-color: #2d4265;
  }

  .ai-batch-btn-secondary {
    background: #eef2f8;
    color: #1f2f49;
  }

  .ai-batch-btn-secondary:hover:not(:disabled) {
    background: #e5ebf5 !important;
    border-color: #c5cfdd;
  }

  .ai-batch-btn:disabled {
    opacity: 0.55;
  }

  .ai-batch-hint {
    margin: 0;
    color: #667085;
    font-size: 0.8rem;
  }

  .ai-batch-error {
    margin: 0;
    color: #991b1b;
    font-size: 0.85rem;
    font-weight: 700;
  }

  .ai-batch-feedback {
    margin: 0;
    color: #065f46;
    font-size: 0.85rem;
    font-weight: 700;
  }

  .ai-key-field {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    color: #1f2937;
    font-weight: 650;
  }

  .ai-key-field span {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .ai-key-field em {
    color: #047857;
    font-size: 0.76rem;
    font-style: normal;
    font-weight: 800;
    text-transform: uppercase;
  }

  .ai-key-field input[type="password"] {
    width: 100%;
    min-height: 42px;
    padding: 0.65rem 0.75rem;
    color: #111827;
    background: #fbfcfe;
    border: 1px solid #cfd6e2;
    border-radius: 8px;
    font: inherit;
  }

  .ai-key-field input[type="password"]:focus {
    outline: none;
    border-color: #50688f;
    box-shadow: 0 0 0 3px rgba(80, 104, 143, 0.14);
  }

  .ai-key-field input[type="password"]:disabled {
    color: #7b8494;
    background: #eef1f5;
  }

  .ai-key-clear {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: #4b5563;
    font-size: 0.88rem;
  }

  .ai-key-clear input {
    width: 16px;
    height: 16px;
  }

  .ai-key-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.65rem;
    margin-top: 0.35rem;
  }

  .ai-key-secondary,
  .ai-key-primary {
    min-height: 40px;
    padding: 0.6rem 0.95rem;
    border-radius: 8px;
    font-weight: 700;
  }

  .ai-key-secondary {
    color: #334155;
    background: #f5f7fa;
    border: 1px solid #d8dee9;
  }

  .ai-key-secondary:hover:not(:disabled) {
    background: #e9edf3 !important;
  }

  .ai-key-primary {
    color: #fff;
    background: #1f2f49;
  }

  .ai-key-primary:hover:not(:disabled) {
    background: #2d4265 !important;
  }

  @media (max-width: 720px) {
    .ai-task-row {
      grid-template-columns: 1fr;
      gap: 0.45rem;
    }

    .ai-custom-model-input {
      grid-column: auto;
    }

    .ai-batch-grid,
    .ai-pastpaper-range-grid {
      grid-template-columns: 1fr;
    }
  }
</style>

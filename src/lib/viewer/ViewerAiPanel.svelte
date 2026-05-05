<script lang="ts">
  interface ChunkChatMessage {
    id: string;
    role: "user" | "assistant";
    content: string;
    state: "complete" | "streaming" | "stopped" | "error";
    historyContent?: string;
    historyImageBase64List?: string[];
    imageDataUrls?: string[];
    html?: string;
  }

  interface PendingChatAttachment {
    imageBase64: string;
    imageDataUrl: string;
    pageNumber: number;
    createdAt: number;
    transcribedBody: string | null;
  }

  let {
    viewerPageNumber,
    chunkChatMessages,
    chunkChatDraft = $bindable(""),
    chunkChatStreaming,
    chunkChatLoadingContext,
    chunkChatError,
    chatAttachmentTranscribing,
    chunkInkContextTranscribing,
    pendingChatAttachment,
    clearPendingChatAttachment,
    stopChunkChat,
    sendChunkChatMessage,
    isChatContextReady,
    onChunkChatKeydown,
    closeViewerAiPanel,
    chunkChatCodeCopy,
    chunkChatTranscript = $bindable<HTMLDivElement | null>(null),
    viewerContextReady,
  }: {
    viewerPageNumber: number;
    chunkChatMessages: ChunkChatMessage[];
    chunkChatDraft?: string;
    chunkChatStreaming: boolean;
    chunkChatLoadingContext: boolean;
    chunkChatError: string | null;
    chatAttachmentTranscribing: boolean;
    chunkInkContextTranscribing: boolean;
    pendingChatAttachment: PendingChatAttachment | null;
    clearPendingChatAttachment: () => void;
    stopChunkChat: () => Promise<void>;
    sendChunkChatMessage: () => Promise<void>;
    isChatContextReady: boolean;
    onChunkChatKeydown: (event: KeyboardEvent) => void;
    closeViewerAiPanel: () => Promise<void>;
    chunkChatCodeCopy: (node: HTMLDivElement) => { destroy?: () => void } | void;
    chunkChatTranscript?: HTMLDivElement | null;
    viewerContextReady: boolean;
  } = $props();
</script>

<aside class="viewer-ai-panel" aria-label="AI chat">
  <div class="viewer-ai-header">
    <div>
      <p class="viewer-ai-kicker">AI chat</p>
      <p class="viewer-ai-context">
        {#if viewerContextReady}
          Context: Page {viewerPageNumber}
        {:else}
          Context: Preparing page context...
        {/if}
      </p>
    </div>
    <button
      class="viewer-ai-close"
      type="button"
      onclick={() => void closeViewerAiPanel()}
      aria-label="Close AI chat"
    >
      ×
    </button>
  </div>

  <div class="chunk-ai-transcript" bind:this={chunkChatTranscript} use:chunkChatCodeCopy>
    {#if chunkChatMessages.length === 0}
      <div class="chunk-ai-empty">
        <p>Ask about this page.</p>
        <p>The AI sees the page's extracted text, plus any attached selection image.</p>
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
                  {#each message.imageDataUrls as imageDataUrl, imageIndex (`${message.id}-viewer-${imageIndex}`)}
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
    {#if !viewerContextReady}
      <span class="chunk-ai-status">Preparing page context...</span>
    {/if}
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
      placeholder="Ask about this page..."
      onkeydown={onChunkChatKeydown}
      disabled={chunkChatStreaming || chunkChatLoadingContext}
    ></textarea>
    <div class="chunk-ai-actions">
      {#if chunkChatStreaming}
        <button class="chunk-ai-action chunk-ai-stop" type="button" onclick={() => void stopChunkChat()}>
          Stop
        </button>
      {/if}
      <button
        class="chunk-ai-action chunk-ai-send"
        type="button"
        onclick={() => void sendChunkChatMessage()}
        disabled={chunkChatStreaming || chunkChatLoadingContext || !isChatContextReady || (!chunkChatDraft.trim() && !pendingChatAttachment)}
      >
        Send
      </button>
    </div>
  </div>
</aside>

<style>
  .viewer-ai-panel {
    position: absolute;
    top: 74px;
    right: 16px;
    width: min(380px, calc(100vw - 32px));
    max-height: calc(100vh - 120px);
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
    padding: 0.9rem;
    background: rgba(255, 255, 255, 0.97);
    border: 1px solid rgba(31, 45, 70, 0.14);
    border-radius: 16px;
    box-shadow: 0 20px 50px rgba(15, 23, 42, 0.22);
    backdrop-filter: blur(12px);
    z-index: 160;
  }

  .viewer-ai-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.8rem;
  }

  .viewer-ai-kicker {
    margin: 0;
    font-size: 0.72rem;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #64748b;
  }

  .viewer-ai-context {
    margin: 0.15rem 0 0;
    font-size: 0.92rem;
    color: #1f2937;
    font-weight: 600;
  }

  .viewer-ai-close {
    width: 32px;
    height: 32px;
    border-radius: 8px;
    border: 1px solid #d7dde7;
    background: #fff;
    color: #475467;
    font-size: 1.15rem;
    line-height: 1;
    padding: 0;
  }

  .viewer-ai-close:hover {
    background: #eef2f8 !important;
  }

  .chunk-ai-transcript {
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .chunk-ai-empty {
    padding: 0.9rem;
    border-radius: 12px;
    background: #f8fafc;
    border: 1px dashed #d5dde8;
    color: #556274;
  }

  .chunk-ai-empty p {
    margin: 0;
  }

  .chunk-ai-empty p + p {
    margin-top: 0.35rem;
  }

  .chunk-chat-message {
    display: flex;
    flex-direction: column;
    gap: 0.28rem;
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
    gap: 0.45rem;
    font-size: 0.72rem;
    font-weight: 700;
    color: #64748b;
  }

  .chunk-chat-bubble {
    max-width: 100%;
    padding: 0.75rem 0.82rem;
    border-radius: 14px;
    border: 1px solid #dce3ee;
    background: #fff;
    color: #111827;
    line-height: 1.45;
    overflow-wrap: anywhere;
  }

  .chunk-chat-image {
    display: block;
    width: 100%;
    max-height: 180px;
    object-fit: contain;
    border-radius: 10px;
    border: 1px solid #d7dde7;
    background: #fff;
  }

  .chunk-chat-image-strip {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    margin-bottom: 0.6rem;
  }

  .chunk-chat-image-strip .chunk-chat-image {
    max-height: 140px;
  }

  .chunk-chat-bubble.user-bubble {
    background: #eef4ff;
    border-color: #cddaf7;
  }

  .chunk-chat-bubble.assistant-bubble {
    background: #fff;
  }

  .chunk-chat-bubble.is-error {
    border-color: #f1b4b8;
    background: #fff5f5;
    color: #991b1b;
  }

  .chunk-chat-bubble.rendered :global(p) {
    margin: 0;
  }

  .chunk-chat-bubble.rendered :global(p + p),
  .chunk-chat-bubble.rendered :global(ol),
  .chunk-chat-bubble.rendered :global(ul) {
    margin-top: 0.7rem;
  }

  .chunk-chat-bubble.rendered :global(li + li) {
    margin-top: 0.35rem;
  }

  .chunk-chat-bubble.rendered :global(.chunk-math-display),
  .chunk-chat-bubble.rendered :global(.katex-display) {
    margin: 0.85rem 0;
  }

  .chunk-chat-bubble.rendered :global(code) {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }

  .chunk-chat-bubble.rendered :global(pre) {
    margin: 0.85rem 0;
    padding: 0.8rem;
    border-radius: 10px;
    background: #f7f8fb;
    overflow: auto;
  }

  .chunk-chat-bubble.rendered :global(pre code) {
    background: transparent;
    padding: 0;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block) {
    border: 1px solid #dbe2ec;
    border-radius: 10px;
    overflow: hidden;
    background: #f8fafc;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-toolbar) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.55rem 0.7rem;
    border-bottom: 1px solid #dbe2ec;
    background: #eef2f8;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-language) {
    font-size: 0.72rem;
    font-weight: 800;
    color: #5d6b80;
    text-transform: uppercase;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-copy) {
    min-height: 28px;
    padding: 0.2rem 0.55rem;
    border-radius: 7px;
    border: 1px solid #cfd8e5;
    background: #fff;
    color: #334155;
    font-size: 0.74rem;
    font-weight: 700;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-copy:disabled) {
    opacity: 0.55;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block pre) {
    margin: 0;
    border-radius: 0;
    background: transparent;
  }

  .chunk-chat-bubble.rendered :global(.chunk-code-block pre code) {
    white-space: pre;
  }

  .chunk-chat-bubble.rendered :global(a) {
    color: #2040a0;
  }

  .chunk-ai-status-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.45rem 0.6rem;
    min-height: 1.3rem;
  }

  .chunk-ai-status {
    color: #5c6777;
    font-size: 0.8rem;
    font-weight: 600;
  }

  .chunk-ai-error {
    color: #b91c1c;
    font-size: 0.8rem;
    font-weight: 700;
  }

  .chunk-ai-composer {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .chunk-chat-attachment-preview {
    display: flex;
    align-items: center;
    gap: 0.65rem;
    padding: 0.65rem;
    border-radius: 12px;
    border: 1px solid #d7dde7;
    background: #f8fafc;
  }

  .chunk-chat-attachment-preview img {
    width: 72px;
    height: 72px;
    object-fit: cover;
    border-radius: 10px;
    border: 1px solid #d7dde7;
  }

  .chunk-chat-attachment-meta {
    display: flex;
    flex-direction: column;
    gap: 0.18rem;
    min-width: 0;
  }

  .chunk-chat-attachment-meta strong {
    color: #1f2937;
  }

  .chunk-chat-attachment-meta span {
    color: #667085;
    font-size: 0.82rem;
  }

  .chunk-chat-attachment-preview .chunk-ai-action {
    margin-left: auto;
  }

  .chunk-ai-input {
    width: 100%;
    min-height: 92px;
    resize: vertical;
    padding: 0.8rem 0.85rem;
    border-radius: 12px;
    border: 1px solid #d7dde7;
    background: #fff;
    color: #111827;
    font: inherit;
    line-height: 1.45;
  }

  .chunk-ai-input:focus {
    outline: none;
    border-color: #9aa8bf;
    box-shadow: 0 0 0 3px rgba(80, 104, 143, 0.14);
  }

  .chunk-ai-input:disabled {
    background: #f5f7fa;
    color: #7b8494;
  }

  .chunk-ai-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.55rem;
  }

  .chunk-ai-action {
    min-height: 38px;
    padding: 0.55rem 0.9rem;
    border-radius: 10px;
    font-weight: 700;
    border: 1px solid #d7dde7;
  }

  .chunk-ai-action:hover:not(:disabled) {
    background: #eef2f8 !important;
  }

  .chunk-ai-action:disabled {
    opacity: 0.55;
  }

  .chunk-ai-send {
    background: #2040a0;
    border-color: #2040a0;
    color: #fff;
  }

  .chunk-ai-stop {
    background: #fff;
    color: #334155;
  }

  @media (max-width: 720px) {
    .viewer-ai-panel {
      top: auto;
      right: 10px;
      left: 10px;
      bottom: 10px;
      width: auto;
      max-height: calc(100vh - 96px);
    }
  }
</style>

<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { renderChunkBodyHtml } from "$lib/chunkBody";

  interface ChunkPreview {
    id: number;
    chunk_type: string;
    title: string | null;
    subject: string | null;
    status: string;
    body_preview: string | null;
    has_formatted_body: boolean;
    has_self_explanation: boolean;
  }

  interface ChunkColour {
    accent: string;
    tint: string;
    label: string;
    short: string;
  }

  let {
    chunkId,
    anchorRect,
    colours,
    onEnsureBody,
    onOpen,
    onClose,
  }: {
    chunkId: number;
    anchorRect: DOMRect;
    colours: Record<string, ChunkColour>;
    onEnsureBody: (chunkId: number) => Promise<boolean>;
    onOpen: (chunkId: number) => void;
    onClose: () => void;
  } = $props();

  let preview = $state<ChunkPreview | null>(null);
  let loadError = $state<string | null>(null);
  let card: HTMLDivElement | null = $state(null);

  const placement = $derived(computePlacement(anchorRect));
  const colour = $derived<ChunkColour>(
    (preview && colours[preview.chunk_type])
    ?? { accent: "oklch(0.52 0.03 240)", tint: "oklch(0.975 0.008 240)", label: "Chunk", short: "?" },
  );
  const bodyHtml = $derived(
    preview?.body_preview ? renderChunkBodyHtml(preview.body_preview) : "",
  );
  const displayTitle = $derived(preview?.title ?? preview?.subject ?? null);

  function computePlacement(rect: DOMRect) {
    const cardWidth = 320;
    const cardMaxHeight = 280;
    const margin = 8;
    const vw = typeof window !== "undefined" ? window.innerWidth : 1024;
    const vh = typeof window !== "undefined" ? window.innerHeight : 768;

    // Prefer below the anchor; flip above if not enough room.
    const spaceBelow = vh - rect.bottom;
    const showAbove = spaceBelow < cardMaxHeight + margin && rect.top > cardMaxHeight + margin;

    let left = rect.left + rect.width / 2 - cardWidth / 2;
    left = Math.max(margin, Math.min(left, vw - cardWidth - margin));

    const top = showAbove ? rect.top - margin : rect.bottom + margin;

    return {
      left,
      top,
      transform: showAbove ? "translateY(-100%)" : "none",
    };
  }

  onMount(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const initial = await invoke<ChunkPreview>("get_chunk_preview", { chunkId });
        if (cancelled) return;
        preview = initial;
        if (!initial.has_formatted_body) {
          const ready = await onEnsureBody(chunkId);
          if (cancelled) return;
          if (ready) {
            preview = await invoke<ChunkPreview>("get_chunk_preview", { chunkId });
          }
        }
      } catch (err) {
        if (!cancelled) loadError = typeof err === "string" ? err : String(err);
      }
    };
    void load();

    const onDocPointer = (event: MouseEvent) => {
      if (!card) return;
      if (event.target instanceof Node && card.contains(event.target)) return;
      onClose();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    // Delay listener until after the click that opened the peek has finished.
    const handle = window.setTimeout(() => {
      document.addEventListener("pointerdown", onDocPointer);
    }, 0);
    document.addEventListener("keydown", onKey);

    return () => {
      cancelled = true;
      window.clearTimeout(handle);
      document.removeEventListener("pointerdown", onDocPointer);
      document.removeEventListener("keydown", onKey);
    };
  });
</script>

<div
  bind:this={card}
  class="chunk-peek"
  style:left="{placement.left}px"
  style:top="{placement.top}px"
  style:transform={placement.transform}
  style:--peek-accent={colour.accent}
  style:--peek-tint={colour.tint}
  role="dialog"
  aria-label="Referenced chunk preview"
>
  {#if loadError}
    <div class="peek-error">Couldn't load preview: {loadError}</div>
  {:else if !preview}
    <div class="peek-loading">Loading…</div>
  {:else}
    <header class="peek-header">
      <span class="peek-badge">{colour.short}</span>
      {#if displayTitle}
        <span class="peek-title">{displayTitle}</span>
      {:else}
        <span class="peek-title muted">{colour.label}</span>
      {/if}
    </header>
    {#if bodyHtml}
      <div class="peek-body">{@html bodyHtml}</div>
    {:else}
      <div class="peek-body muted">No body content yet.</div>
    {/if}
    {#if preview.has_self_explanation}
      <div class="peek-attached">Your self-explanation is attached.</div>
    {/if}
    <div class="peek-actions">
      <button type="button" class="peek-open" onclick={() => onOpen(preview!.id)}>Open</button>
    </div>
  {/if}
</div>

<style>
  .chunk-peek {
    position: fixed;
    z-index: 1000;
    width: 320px;
    max-height: 280px;
    background: #fff;
    border: 1px solid rgba(0, 0, 0, 0.08);
    border-left: 3px solid var(--peek-accent);
    border-radius: 6px;
    box-shadow:
      0 10px 30px rgba(0, 0, 0, 0.12),
      0 2px 6px rgba(0, 0, 0, 0.08);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    font-family: "Inter", system-ui, sans-serif;
  }

  .peek-header {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    background: var(--peek-tint);
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
  }

  .peek-badge {
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--peek-accent);
    flex: none;
  }

  .peek-title {
    font-family: Georgia, serif;
    font-style: italic;
    font-size: 0.95rem;
    color: #111;
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .peek-title.muted {
    font-style: normal;
    color: #555;
  }

  .peek-body {
    padding: 0.5rem 0.75rem;
    font-family: Georgia, serif;
    font-size: 0.85rem;
    line-height: 1.45;
    color: #222;
    overflow-y: auto;
    flex: 1;
  }

  .peek-body :global(p) {
    margin: 0 0 0.5rem;
  }

  .peek-body :global(p:last-child) {
    margin-bottom: 0;
  }

  .peek-body.muted {
    color: #777;
  }

  .peek-attached {
    padding: 0.4rem 0.75rem;
    font-size: 0.75rem;
    color: var(--peek-accent);
    border-top: 1px solid rgba(0, 0, 0, 0.06);
    background: rgba(0, 0, 0, 0.015);
  }

  .peek-actions {
    display: flex;
    justify-content: flex-end;
    padding: 0.4rem 0.75rem;
    border-top: 1px solid rgba(0, 0, 0, 0.06);
    background: #fafafa;
  }

  .peek-open {
    font-size: 0.8rem;
    font-weight: 500;
    padding: 0.25rem 0.65rem;
    background: var(--peek-accent);
    color: #fff;
    border: none;
    border-radius: 4px;
    cursor: pointer;
  }

  .peek-open:hover {
    filter: brightness(1.05);
  }

  .peek-loading,
  .peek-error {
    padding: 0.75rem;
    font-size: 0.85rem;
    color: #555;
  }

  .peek-error {
    color: #9a2a2a;
  }
</style>

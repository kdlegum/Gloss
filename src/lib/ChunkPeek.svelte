<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { renderChunkBodyHtml } from "$lib/chunkBody";

  type GlossaryFormat = "markdown" | "typst";

  interface ChunkPreview {
    id: number;
    chunk_type: string;
    title: string | null;
    subject: string | null;
    status: string;
    body_preview: string | null;
    glossary_preview: string | null;
    glossary_format: GlossaryFormat;
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
  let activeTab = $state<"chunk" | "glossary">("chunk");

  const placement = $derived(computePlacement(anchorRect));
  const colour = $derived<ChunkColour>(
    (preview && colours[preview.chunk_type])
    ?? { accent: "oklch(0.52 0.03 240)", tint: "oklch(0.975 0.008 240)", label: "Chunk", short: "?" },
  );
  const bodyHtml = $derived(
    preview?.body_preview ? renderChunkBodyHtml(preview.body_preview) : "",
  );
  const glossaryFormat = $derived<GlossaryFormat>(preview?.glossary_format ?? "markdown");
  const glossaryIsTypst = $derived(glossaryFormat === "typst");
  const glossaryHtml = $derived(
    preview?.glossary_preview && !glossaryIsTypst
      ? renderChunkBodyHtml(preview.glossary_preview)
      : "",
  );
  const glossaryTypstPreview = $derived.by(() => {
    if (!preview?.glossary_preview || !glossaryIsTypst) return "";
    const trimmed = preview.glossary_preview.trim();
    if (!trimmed) return "";
    return trimmed.length > 320 ? `${trimmed.slice(0, 320).trimEnd()}...` : trimmed;
  });
  const hasGlossary = $derived(Boolean(preview?.glossary_preview?.trim()));
  const displayTitle = $derived(preview?.title ?? preview?.subject ?? null);

  $effect(() => {
    if (!preview) {
      activeTab = "chunk";
      return;
    }
    if (hasGlossary && !bodyHtml) {
      activeTab = "glossary";
      return;
    }
    if (!hasGlossary) {
      activeTab = "chunk";
    }
  });

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
    {#if hasGlossary}
      <div class="peek-tab-bar" role="tablist" aria-label="Chunk preview sections">
        <button
          type="button"
          class="peek-tab"
          class:active={activeTab === "chunk"}
          onclick={() => activeTab = "chunk"}
        >Chunk</button>
        <button
          type="button"
          class="peek-tab"
          class:active={activeTab === "glossary"}
          onclick={() => activeTab = "glossary"}
        >Glossary</button>
      </div>
    {/if}

    {#if activeTab === "glossary"}
      {#if glossaryIsTypst && glossaryTypstPreview}
        <div class="peek-body">
          <div class="peek-note-format">Typst</div>
          <pre class="peek-typst-preview">{glossaryTypstPreview}</pre>
        </div>
      {:else if glossaryHtml}
        <div class="peek-body">{@html glossaryHtml}</div>
      {:else}
        <div class="peek-body muted">No glossary entry yet.</div>
      {/if}
    {:else if bodyHtml}
      <div class="peek-body">{@html bodyHtml}</div>
    {:else}
      <div class="peek-body muted">No body content yet.</div>
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

  .peek-body :global(h1),
  .peek-body :global(h2),
  .peek-body :global(h3),
  .peek-body :global(h4) {
    margin: 0.2em 0 0.45em;
    font-family: Georgia, serif;
    font-weight: 600;
    line-height: 1.25;
    color: #111827;
  }

  .peek-body :global(h1) { font-size: 1.02rem; }
  .peek-body :global(h2) { font-size: 0.96rem; }
  .peek-body :global(h3) { font-size: 0.9rem; }

  .peek-body :global(ul),
  .peek-body :global(ol) {
    margin: 0.2em 0 0.6em 1.1em;
  }

  .peek-body :global(code) {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.82em;
    background: rgba(15, 23, 42, 0.07);
    padding: 0.1em 0.26em;
    border-radius: 0.2em;
  }

  .peek-body :global(pre) {
    margin: 0.25em 0 0.7em;
    padding: 0.5em 0.6em;
    border-radius: 6px;
    background: rgba(15, 23, 42, 0.05);
    overflow-x: auto;
  }

  .peek-body :global(pre code) {
    background: transparent;
    padding: 0;
  }

  .peek-tab-bar {
    display: flex;
    gap: 0.25rem;
    padding: 0.35rem 0.65rem;
    border-bottom: 1px solid rgba(0, 0, 0, 0.06);
    background: #fff;
  }

  .peek-tab {
    font-size: 0.72rem;
    font-weight: 600;
    color: #6b7280;
    border: 1px solid rgba(0, 0, 0, 0.12);
    background: #fff;
    border-radius: 999px;
    padding: 0.18rem 0.55rem;
    cursor: pointer;
    transition: color 120ms ease, background-color 120ms ease, border-color 120ms ease;
  }

  .peek-tab:hover {
    color: #374151;
  }

  .peek-tab.active {
    color: var(--peek-accent);
    border-color: var(--peek-accent);
    background: var(--peek-tint);
  }

  .peek-body.muted {
    color: #777;
  }

  .peek-note-format {
    display: inline-flex;
    align-items: center;
    margin-bottom: 0.55rem;
    padding: 0.18rem 0.5rem;
    border-radius: 999px;
    background: rgba(15, 23, 42, 0.06);
    color: #475569;
    font-family: "Inter", system-ui, sans-serif;
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .peek-typst-preview {
    margin: 0;
    padding: 0.55rem 0.65rem;
    border-radius: 8px;
    background: rgba(15, 23, 42, 0.05);
    color: #1e293b;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.77rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
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

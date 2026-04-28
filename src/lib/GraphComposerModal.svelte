<script lang="ts">
  import { tick } from "svelte";
  import { fade } from "svelte/transition";
  import {
    DEFAULT_GRAPH_SPEC,
    drawGraphCard,
    normaliseGraphSpec,
    parsePointsText,
    pointsToText,
    validateGraphSpec,
    type GraphSpec,
  } from "$lib/graph";

  let {
    open,
    initialGraph = null,
    showGlossaryAction = true,
    title = "Insert graph",
    onCancel = () => {},
    onInsertCanvas = (_graph: GraphSpec) => {},
    onInsertGlossary = (_graph: GraphSpec) => {},
  }: {
    open: boolean;
    initialGraph?: GraphSpec | null;
    showGlossaryAction?: boolean;
    title?: string;
    onCancel?: () => void;
    onInsertCanvas?: (graph: GraphSpec) => void;
    onInsertGlossary?: (graph: GraphSpec) => void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | null>(null);
  let firstFocusableEl = $state<HTMLButtonElement | null>(null);
  let previewCanvas = $state<HTMLCanvasElement | null>(null);

  let mode = $state<"equation" | "points">("equation");
  let equationInput = $state("");
  let pointsInput = $state("");
  let xMinInput = $state(DEFAULT_GRAPH_SPEC.xMin);
  let xMaxInput = $state(DEFAULT_GRAPH_SPEC.xMax);
  let yMinInput = $state(DEFAULT_GRAPH_SPEC.yMin);
  let yMaxInput = $state(DEFAULT_GRAPH_SPEC.yMax);
  let lineColour = $state(DEFAULT_GRAPH_SPEC.lineColour);
  let lineWidthInput = $state(DEFAULT_GRAPH_SPEC.lineWidth);
  let formError = $state<string | null>(null);

  const parsedPoints = $derived(parsePointsText(pointsInput));

  const draftSpec = $derived(
    normaliseGraphSpec({
      mode,
      equation: equationInput,
      points: mode === "points" ? parsedPoints.points : null,
      xMin: xMinInput,
      xMax: xMaxInput,
      yMin: yMinInput,
      yMax: yMaxInput,
      lineColour,
      lineWidth: lineWidthInput,
    }),
  );

  const validationError = $derived(() => {
    if (mode === "points" && parsedPoints.error) return parsedPoints.error;
    return validateGraphSpec(draftSpec);
  });

  function resetFromInitial() {
    const source = normaliseGraphSpec(initialGraph ?? DEFAULT_GRAPH_SPEC);
    mode = source.mode;
    equationInput = source.equation ?? "";
    pointsInput = pointsToText(source.points);
    xMinInput = source.xMin;
    xMaxInput = source.xMax;
    yMinInput = source.yMin;
    yMaxInput = source.yMax;
    lineColour = source.lineColour;
    lineWidthInput = source.lineWidth;
    formError = null;
  }

  function focusablesInDialog(): HTMLElement[] {
    if (!dialogEl) return [];
    const nodes = dialogEl.querySelectorAll<HTMLElement>(
      "button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])",
    );
    return [...nodes].filter((node) => node.offsetParent !== null);
  }

  function trapTabFocus(event: KeyboardEvent) {
    if (event.key !== "Tab") return;
    const focusables = focusablesInDialog();
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;

    if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
      return;
    }

    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    }
  }

  function drawPreview() {
    if (!previewCanvas) return;
    const rect = previewCanvas.getBoundingClientRect();
    const width = Math.max(180, Math.round(rect.width || 360));
    const height = Math.max(140, Math.round(rect.height || 220));
    const dpr = window.devicePixelRatio || 1;
    previewCanvas.width = Math.round(width * dpr);
    previewCanvas.height = Math.round(height * dpr);

    const ctx = previewCanvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
    ctx.scale(dpr, dpr);
    drawGraphCard(ctx, draftSpec, 0, 0, width, height, {
      selected: false,
      showResizeHandle: false,
    });
  }

  function resolveSubmitGraph(): GraphSpec | null {
    if (mode === "points") {
      const parsed = parsePointsText(pointsInput);
      if (parsed.error) {
        formError = parsed.error;
        return null;
      }
    }
    const spec = normaliseGraphSpec({
      mode,
      equation: equationInput,
      points: mode === "points" ? parsePointsText(pointsInput).points : null,
      xMin: xMinInput,
      xMax: xMaxInput,
      yMin: yMinInput,
      yMax: yMaxInput,
      lineColour,
      lineWidth: lineWidthInput,
    });
    const error = validateGraphSpec(spec);
    if (error) {
      formError = error;
      return null;
    }
    formError = null;
    return spec;
  }

  function submitCanvas() {
    const spec = resolveSubmitGraph();
    if (!spec) return;
    onInsertCanvas(spec);
  }

  function submitGlossary() {
    const spec = resolveSubmitGraph();
    if (!spec) return;
    onInsertGlossary(spec);
  }

  function handleBackdropPointerDown(event: PointerEvent) {
    if (event.target !== event.currentTarget) return;
    onCancel();
  }

  $effect(() => {
    if (!open) return;

    resetFromInitial();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCancel();
        return;
      }
      trapTabFocus(event);
    };

    document.addEventListener("keydown", onKeyDown);

    void tick().then(() => {
      firstFocusableEl?.focus();
      drawPreview();
    });

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.body.style.overflow = previousOverflow;
    };
  });

  $effect(() => {
    if (!open) return;
    drawPreview();
  });
</script>

{#if open}
  <div class="graph-modal-backdrop" role="presentation" onpointerdown={handleBackdropPointerDown} transition:fade={{ duration: 120 }}>
    <div class="graph-modal" role="dialog" aria-modal="true" aria-label="Graph composer" bind:this={dialogEl}>
      <div class="graph-modal-header">
        <h3>{title}</h3>
        <button class="graph-modal-close" type="button" aria-label="Close graph modal" onclick={onCancel}>×</button>
      </div>

      <div class="graph-modal-grid">
        <section class="graph-modal-fields">
          <div class="graph-mode-toggle" role="tablist" aria-label="Graph input mode">
            <button
              class="graph-mode-btn"
              class:active={mode === "equation"}
              type="button"
              role="tab"
              aria-selected={mode === "equation"}
              onclick={() => mode = "equation"}
              bind:this={firstFocusableEl}
            >Equation</button>
            <button
              class="graph-mode-btn"
              class:active={mode === "points"}
              type="button"
              role="tab"
              aria-selected={mode === "points"}
              onclick={() => mode = "points"}
            >Points</button>
          </div>

          {#if mode === "equation"}
            <label class="graph-field">
              <span>Equation (y = f(x))</span>
              <input
                type="text"
                value={equationInput}
                placeholder="e.g. sin(x) + x^2 / 4"
                oninput={(event) => {
                  equationInput = (event.currentTarget as HTMLInputElement).value;
                  formError = null;
                }}
              />
            </label>
          {:else}
            <label class="graph-field">
              <span>Points (one per line)</span>
              <textarea
                rows="7"
                value={pointsInput}
                placeholder="-2, 4&#10;-1, 1&#10;0, 0&#10;1, 1&#10;2, 4"
                oninput={(event) => {
                  pointsInput = (event.currentTarget as HTMLTextAreaElement).value;
                  formError = null;
                }}
              ></textarea>
            </label>
          {/if}

          <div class="graph-range-grid">
            <label class="graph-field">
              <span>x min</span>
              <input type="number" step="0.5" value={xMinInput} oninput={(event) => xMinInput = Number((event.currentTarget as HTMLInputElement).value)} />
            </label>
            <label class="graph-field">
              <span>x max</span>
              <input type="number" step="0.5" value={xMaxInput} oninput={(event) => xMaxInput = Number((event.currentTarget as HTMLInputElement).value)} />
            </label>
            <label class="graph-field">
              <span>y min</span>
              <input type="number" step="0.5" value={yMinInput} oninput={(event) => yMinInput = Number((event.currentTarget as HTMLInputElement).value)} />
            </label>
            <label class="graph-field">
              <span>y max</span>
              <input type="number" step="0.5" value={yMaxInput} oninput={(event) => yMaxInput = Number((event.currentTarget as HTMLInputElement).value)} />
            </label>
          </div>

          <div class="graph-style-row">
            <label class="graph-field graph-colour">
              <span>Line colour</span>
              <input type="color" value={lineColour} oninput={(event) => lineColour = (event.currentTarget as HTMLInputElement).value} />
            </label>
            <label class="graph-field graph-line-width">
              <span>Line width</span>
              <input
                type="range"
                min="1"
                max="8"
                step="0.5"
                value={lineWidthInput}
                oninput={(event) => lineWidthInput = Number((event.currentTarget as HTMLInputElement).value)}
              />
              <small>{lineWidthInput.toFixed(1)} px</small>
            </label>
          </div>

          {#if formError || validationError}
            <p class="graph-validation-error">{formError ?? validationError}</p>
          {/if}
        </section>

        <section class="graph-modal-preview">
          <div class="graph-preview-label">Live preview</div>
          <canvas bind:this={previewCanvas}></canvas>
          <p class="graph-preview-hint">This preview uses the same graph renderer as canvas and glossary blocks.</p>
        </section>
      </div>

      <div class="graph-modal-actions">
        <button type="button" class="graph-btn graph-btn-secondary" onclick={onCancel}>Cancel</button>
        {#if showGlossaryAction}
          <button type="button" class="graph-btn graph-btn-secondary" onclick={submitGlossary}>Insert in glossary</button>
        {/if}
        <button type="button" class="graph-btn graph-btn-primary" onclick={submitCanvas}>Insert on canvas</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .graph-modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 23, 42, 0.45);
    display: flex;
    justify-content: center;
    align-items: center;
    padding: 20px;
    z-index: 80;
  }

  .graph-modal {
    width: min(980px, 100%);
    max-height: min(760px, calc(100vh - 32px));
    background: #ffffff;
    border: 1px solid #d1d5db;
    border-radius: 16px;
    box-shadow: 0 20px 45px rgba(15, 23, 42, 0.26);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .graph-modal-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px;
    border-bottom: 1px solid #e5e7eb;
  }

  .graph-modal-header h3 {
    margin: 0;
    font-size: 16px;
    font-weight: 700;
    color: #0f172a;
  }

  .graph-modal-close {
    border: none;
    background: transparent;
    color: #475569;
    font-size: 24px;
    line-height: 1;
    cursor: pointer;
    border-radius: 8px;
    padding: 2px 6px;
  }

  .graph-modal-close:hover {
    background: #e2e8f0;
    color: #0f172a;
  }

  .graph-modal-grid {
    display: grid;
    grid-template-columns: 1.1fr 1fr;
    gap: 16px;
    padding: 16px;
    overflow: auto;
  }

  .graph-modal-fields {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .graph-mode-toggle {
    display: inline-flex;
    border: 1px solid #cbd5e1;
    border-radius: 10px;
    overflow: hidden;
    width: fit-content;
  }

  .graph-mode-btn {
    border: none;
    background: #f8fafc;
    color: #334155;
    font-weight: 600;
    font-size: 13px;
    padding: 7px 12px;
    cursor: pointer;
  }

  .graph-mode-btn.active {
    background: #2351d1;
    color: #ffffff;
  }

  .graph-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .graph-field span {
    color: #334155;
    font-size: 12px;
    font-weight: 600;
  }

  .graph-field input,
  .graph-field textarea {
    border: 1px solid #cbd5e1;
    border-radius: 9px;
    padding: 8px 10px;
    font: 500 13px Inter, system-ui, sans-serif;
    color: #111827;
    background: #ffffff;
  }

  .graph-field textarea {
    resize: vertical;
    min-height: 130px;
  }

  .graph-range-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }

  .graph-style-row {
    display: grid;
    grid-template-columns: minmax(0, 130px) minmax(0, 1fr);
    gap: 12px;
    align-items: end;
  }

  .graph-colour input[type="color"] {
    width: 100%;
    height: 38px;
    padding: 4px;
    cursor: pointer;
  }

  .graph-line-width small {
    color: #64748b;
    font-size: 11px;
  }

  .graph-validation-error {
    margin: 0;
    font-size: 12px;
    color: #b91c1c;
    background: #fef2f2;
    border: 1px solid #fecaca;
    border-radius: 8px;
    padding: 7px 9px;
  }

  .graph-modal-preview {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 320px;
  }

  .graph-preview-label {
    color: #334155;
    font-size: 12px;
    font-weight: 700;
  }

  .graph-modal-preview canvas {
    width: 100%;
    min-height: 280px;
    border-radius: 12px;
    border: 1px solid #cbd5e1;
    background: #ffffff;
  }

  .graph-preview-hint {
    margin: 0;
    color: #64748b;
    font-size: 11px;
  }

  .graph-modal-actions {
    border-top: 1px solid #e5e7eb;
    padding: 12px 16px;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .graph-btn {
    border-radius: 9px;
    border: 1px solid #cbd5e1;
    padding: 8px 13px;
    font-weight: 600;
    cursor: pointer;
    font-size: 13px;
  }

  .graph-btn-secondary {
    background: #ffffff;
    color: #334155;
  }

  .graph-btn-secondary:hover {
    background: #f8fafc;
  }

  .graph-btn-primary {
    border-color: #1d4ed8;
    background: #1d4ed8;
    color: #ffffff;
  }

  .graph-btn-primary:hover {
    background: #1e40af;
    border-color: #1e40af;
  }

  @media (max-width: 900px) {
    .graph-modal-grid {
      grid-template-columns: 1fr;
    }

    .graph-modal-preview canvas {
      min-height: 230px;
    }
  }
</style>

<script lang="ts">
  import type { BatchChunkProgressState } from "$lib/app/types";

  let {
    batchChunkProgress,
    batchChunkProgressPercent,
    dismissBatchChunkProgress,
  }: {
    batchChunkProgress: BatchChunkProgressState;
    batchChunkProgressPercent: number;
    dismissBatchChunkProgress: () => void;
  } = $props();
</script>

<div class="batch-progress-card" class:is-finished={batchChunkProgress.finished}>
  <div class="batch-progress-head">
    <div>
      <p class="batch-progress-title">
        Batch chunking {batchChunkProgress.startPage}-{batchChunkProgress.endPage}
      </p>
      <p class="batch-progress-status">{batchChunkProgress.statusMessage}</p>
    </div>
    <button
      class="batch-progress-close"
      type="button"
      onclick={dismissBatchChunkProgress}
      aria-label="Dismiss batch chunking progress"
    >
      x
    </button>
  </div>
  <div class="batch-progress-track" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={batchChunkProgressPercent}>
    <div class="batch-progress-fill" style={`width: ${batchChunkProgressPercent}%`}></div>
  </div>
  <div class="batch-progress-meta">
    <span>{batchChunkProgressPercent}%</span>
    <span>
      {batchChunkProgress.totalPages === 0
        ? "No pages queued"
        : `${batchChunkProgress.completedPages}/${batchChunkProgress.totalPages} pages finished`}
    </span>
    <span>Skipped {batchChunkProgress.skippedPages}</span>
    {#if batchChunkProgress.failedPages > 0}
      <span>Failed {batchChunkProgress.failedPages}</span>
    {/if}
  </div>
  {#if batchChunkProgress.messages.length > 0}
    <ul class="batch-progress-messages">
      {#each batchChunkProgress.messages as message}
        <li>{message}</li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .batch-progress-card {
    margin: 0.6rem 1rem 0.2rem;
    padding: 0.7rem 0.8rem;
    border: 1px solid #d6dde8;
    border-radius: 10px;
    background: #f8fafd;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }

  .batch-progress-card.is-finished {
    background: #f4f8f5;
    border-color: #c8decf;
  }

  .batch-progress-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.8rem;
  }

  .batch-progress-title {
    margin: 0;
    color: #1f2937;
    font-size: 0.88rem;
    font-weight: 700;
  }

  .batch-progress-status {
    margin: 0.12rem 0 0;
    color: #475467;
    font-size: 0.82rem;
    line-height: 1.35;
  }

  .batch-progress-close {
    width: 26px;
    height: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 7px;
    border: 1px solid #d6dde8;
    background: #fff;
    color: #475467;
    font-size: 1rem;
    line-height: 1;
    padding: 0;
  }

  .batch-progress-close:hover {
    background: #eef3f9 !important;
  }

  .batch-progress-track {
    width: 100%;
    height: 10px;
    border-radius: 999px;
    background: #e3e8f1;
    overflow: hidden;
  }

  .batch-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: linear-gradient(90deg, #4b678f 0%, #2f4668 100%);
    transition: width 0.2s ease;
  }

  .batch-progress-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.65rem;
    color: #475467;
    font-size: 0.79rem;
    font-weight: 600;
  }

  .batch-progress-messages {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 0.18rem;
    color: #5c6777;
    font-size: 0.76rem;
    line-height: 1.32;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
</style>

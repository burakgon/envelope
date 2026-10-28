<script lang="ts">
  import { toasts } from '../stores/toasts.svelte';
  import { AlertCircle, CheckCircle, Spinner, X } from '../icons';
  import { fly } from 'svelte/transition';
  import { flip } from 'svelte/animate';
</script>

<div class="stack" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div
      class="toast"
      class:error={t.kind === 'error'}
      in:fly={{ y: 16, duration: 220, opacity: 0 }}
      out:fly={{ y: 8, duration: 160 }}
      animate:flip={{ duration: 200 }}
    >
      {#if t.kind === 'error'}
        <AlertCircle size={16} />
      {:else if t.kind === 'success'}
        <CheckCircle size={16} />
      {:else if t.kind === 'progress'}
        <span class="spin"><Spinner size={15} /></span>
      {/if}
      <span class="msg">{t.message}</span>
      {#if t.action}
        <button class="action" onclick={() => t.action?.run()}>
          {t.action.label}
          {#if t.action.key}<span class="key">{t.action.key}</span>{/if}
        </button>
      {/if}
      <button class="close" aria-label="Dismiss" onclick={() => toasts.dismiss(t.id)}><X size={13} /></button>
      {#if t.duration > 0 && t.action}
        <span class="timer" style:animation-duration="{t.duration}ms"></span>
      {/if}
    </div>
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    left: 50%;
    bottom: 22px;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    z-index: 2000;
    pointer-events: none;
  }
  .toast {
    position: relative;
    overflow: hidden;
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 40px;
    padding: 0 8px 0 14px;
    border-radius: 12px;
    background: #1d1e24;
    color: #f2f2f5;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.28), 0 0 0 1px rgb(255 255 255 / 0.06) inset;
    font-size: var(--text-base);
    font-weight: 500;
    max-width: min(560px, 90vw);
  }
  :global([data-theme='dark']) .toast {
    background: #2a2b33;
  }
  .toast.error {
    background: #3a1517;
    color: #ffd9da;
  }
  .msg {
    flex: 1;
    padding: 9px 0;
  }
  .action {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 10px;
    border-radius: 7px;
    color: #c9c1ff;
    font-weight: 600;
  }
  .action:hover {
    background: rgb(255 255 255 / 0.08);
  }
  .key {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 4px;
    background: rgb(255 255 255 / 0.1);
    color: rgb(255 255 255 / 0.6);
  }
  .close {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    color: rgb(255 255 255 / 0.5);
  }
  .close:hover {
    background: rgb(255 255 255 / 0.08);
    color: #fff;
  }
  .spin {
    display: grid;
    animation: spin 0.9s linear infinite;
  }
  .timer {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 2px;
    width: 100%;
    background: oklch(0.7 0.18 var(--accent-h));
    transform-origin: left;
    animation-name: shrink;
    animation-timing-function: linear;
    animation-fill-mode: forwards;
  }
  @keyframes shrink {
    to {
      transform: scaleX(0);
    }
  }
</style>

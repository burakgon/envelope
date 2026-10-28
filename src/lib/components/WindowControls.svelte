<script lang="ts">
  import { api } from '../api';
  import { ui } from '../stores/ui.svelte';
</script>

{#if !ui.settings.nativeTitlebar && !api.isDemo}
  <div class="controls">
    <button class="ctl" aria-label="Minimize" onclick={() => api.window.minimize()}>
      <svg viewBox="0 0 12 12"><path d="M2.5 6h7" /></svg>
    </button>
    <button class="ctl" aria-label="Maximize" onclick={() => api.window.toggleMaximize()}>
      {#if ui.maximized}
        <svg viewBox="0 0 12 12"><path d="M4 2.5h5.5V8M2.5 4h5.5v5.5H2.5z" /></svg>
      {:else}
        <svg viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" rx="1" /></svg>
      {/if}
    </button>
    <button class="ctl close" aria-label="Close" onclick={() => api.window.close()}>
      <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg>
    </button>
  </div>
{/if}

<style>
  .controls {
    display: flex;
    gap: 8px;
    margin-left: 6px;
    padding-left: 4px;
  }
  .ctl {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--bg-active);
    color: var(--text-2);
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .ctl:hover {
    background: var(--border-strong);
    color: var(--text);
  }
  .ctl.close:hover {
    background: var(--red);
    color: #fff;
  }
  svg {
    width: 11px;
    height: 11px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
  }
</style>

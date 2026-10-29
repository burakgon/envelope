<script lang="ts">
  // A newer Envelope is out: its page opens from here, or the notice is put away until the next one.
  import { api } from '../api';
  import { t } from '../i18n';
  import { Download, X } from '../icons';
  import { ui } from '../stores/ui.svelte';
  import { updates } from '../stores/updates.svelte';
</script>

{#if updates.notice}
  {@const release = updates.notice}
  <div class="banner">
    <button class="open" title={release.url} onclick={() => api.openUrl(release.url)}>
      <Download size={14} />
      <span class="text">
        <b>{t('updates.available', { version: release.version })}</b>
        <span>{t('updates.whatsNew')}</span>
      </span>
    </button>
    <button class="icon-btn sm" aria-label={t('updates.dismiss')} title={t('updates.dismiss')} onclick={() => ui.set('dismissedUpdate', release.version)}><X size={13} /></button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 4px;
    margin: 0 10px 8px;
    padding: 2px 4px 2px 0;
    border-radius: 10px;
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--accent) 28%, transparent);
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 4px 8px 12px;
    border-radius: 10px;
    text-align: left;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .open > :global(svg) {
    flex-shrink: 0;
    color: var(--accent-text);
  }
  .text {
    min-width: 0;
  }
  b {
    display: block;
    color: var(--text);
    font-weight: 600;
  }
  .text span {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

<script lang="ts">
  import { linkStatus } from '../stores/linkStatus.svelte';
  import { shorten } from '../utils/links';
  import { t } from '../i18n';
  import { CalendarPlus, Globe, Mail, Phone, Warning } from '../icons';

  const link = $derived(linkStatus.current);

  /** The address in three parts, the host stressed: what decides where it goes. */
  const parts = $derived.by(() => {
    if (!link || link.kind !== 'web') return null;
    const m = /^(https?:\/\/)(?:[^@/]*@)?([^/:?#]+)(.*)$/i.exec(link.href);
    if (!m) return { scheme: '', host: shorten(link.href), rest: '' };
    return { scheme: m[1], host: m[2], rest: shorten(m[3], 64) };
  });
</script>

{#if link}
  <div class="pill" class:warn={!!link.claimed} role="status" aria-live="polite">
    {#if link.claimed}
      <span class="ico"><Warning size={14} /></span>
      <span class="lines">
        <span class="say">{t('mailFrame.linkElsewhere', { host: link.host ?? '', claimed: link.claimed })}</span>
        <span class="url truncate">{shorten(link.href, 90)}</span>
      </span>
    {:else if link.kind === 'mail'}
      <span class="ico"><Mail size={13} /></span><span class="truncate">{t('mailFrame.linkWriteTo', { address: link.href })}</span>
    {:else if link.kind === 'phone'}
      <span class="ico"><Phone size={13} /></span><span class="truncate">{t('mailFrame.linkCall', { number: link.href })}</span>
    {:else if link.kind === 'calendar'}
      <span class="ico"><CalendarPlus size={13} /></span><span class="truncate">{t('mailFrame.linkCalendar', { url: shorten(link.href, 70) })}</span>
    {:else if parts}
      <span class="ico"><Globe size={13} /></span>
      <span class="truncate"><span class="dim">{parts.scheme}</span><b>{parts.host}</b><span class="dim">{parts.rest}</span></span>
    {:else}
      <span class="truncate">{shorten(link.href)}</span>
    {/if}
  </div>
{/if}

<style>
  .pill {
    position: absolute;
    left: 14px;
    bottom: 14px;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: 7px;
    max-width: min(640px, calc(100% - 28px));
    min-height: 28px;
    padding: 4px 12px 4px 9px;
    border-radius: 14px;
    font-size: var(--text-sm);
    color: var(--text-2);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
    pointer-events: none;
    animation: fade-in var(--dur-fast) var(--ease);
  }
  .ico {
    display: grid;
    place-items: center;
    flex-shrink: 0;
    color: var(--text-3);
  }
  b {
    font-weight: 650;
    color: var(--text);
  }
  .dim {
    color: var(--text-3);
  }
  .pill.warn {
    align-items: flex-start;
    padding: 7px 14px 7px 10px;
    color: var(--text);
    background: color-mix(in oklab, var(--bg-elevated) 86%, var(--amber));
    box-shadow: var(--shadow-md), inset 0 0 0 1px color-mix(in oklab, var(--amber) 55%, transparent);
  }
  .warn .ico {
    color: var(--amber);
    margin-top: 1px;
  }
  .lines {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .say {
    font-weight: 600;
  }
  .url {
    color: var(--text-2);
  }
</style>

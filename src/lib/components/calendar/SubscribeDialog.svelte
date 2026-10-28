<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api } from '../../api';
  import { t } from '../../i18n';
  import { Spinner } from '../../icons';
  import { cal } from '../../stores/calendar.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';

  /** Subscribing to a calendar published at an address (holidays, sports, school, team calendars). */
  let { initial = '', onclose }: { initial?: string; onclose: () => void } = $props();

  let url = $state(untrack(() => initial));
  let busy = $state(false);
  let failure = $state<string | null>(null);
  let input: HTMLInputElement | undefined = $state();
  onMount(() => input?.focus());

  async function subscribe() {
    if (!url.trim() || busy) return;
    busy = true;
    failure = null;
    try {
      const added = await api.calendar.subscribe(url.trim());
      toasts.show(t('calendar.subscribe.subscribed', { name: added.name }), { kind: 'success' });
      await cal.loadCalendars();
      cal.load();
      onclose();
    } catch (e) {
      failure = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      subscribe();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onclose}>
  <div class="dialog" role="dialog" aria-label={t('calendar.subscribe.title')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key}>
    <h3>{t('calendar.subscribe.title')}</h3>
    <p>{t('calendar.subscribe.description')}</p>
    <input bind:this={input} class="input" placeholder={t('calendar.subscribe.urlPlaceholder')} bind:value={url} spellcheck="false" />
    {#if failure}<p class="failure">{failure}</p>{/if}
    <footer>
      <button class="btn" onclick={onclose}>{t('common.cancel')}</button>
      <button class="btn primary" disabled={!url.trim() || busy} onclick={subscribe}>
        {#if busy}<span class="spin"><Spinner size={14} /></span>{/if}{t('calendar.subscribe.subscribe')}
      </button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1400;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    width: min(440px, calc(100vw - 32px));
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  h3 {
    margin: 0 0 6px;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  p {
    margin: 0 0 14px;
    color: var(--text-2);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .failure {
    margin: 10px 0 0;
    color: var(--red);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>

<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { ContactSummary } from '../../api';
  import { t } from '../../i18n';
  import { Spinner, Users } from '../../icons';
  import { people } from '../../stores/contacts.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import Avatar from '../Avatar.svelte';

  let { onclose }: { onclose: () => void } = $props();

  let groups = $state<ContactSummary[][] | null>(null);
  /** Which contact each group keeps: the fullest, first, unless another is chosen. */
  let keep = $state<number[]>([]);
  let busy = $state(false);

  onMount(load);

  async function load() {
    try {
      groups = await api.contacts.duplicates();
      keep = groups.map(() => 0);
    } catch (e) {
      toasts.error(errorMessage(e));
      onclose();
    }
  }

  /** Merges the groups at these positions, each into the contact it keeps. */
  async function merge(which: number[]) {
    if (!groups || busy) return;
    busy = true;
    try {
      let count = 0;
      for (const i of which) {
        const group = groups[i];
        const kept = group[keep[i]];
        count += await api.contacts.merge([kept.id, ...group.filter((c) => c !== kept).map((c) => c.id)]);
      }
      toasts.show(t('duplicates.merged', { count }), { kind: 'success' });
      people.load();
      await load();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onclose}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="dup-title" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key}>
    <header>
      <span class="icon"><Users size={18} /></span>
      <div>
        <h3 id="dup-title">{t('duplicates.title')}</h3>
        <span class="sub">{t('duplicates.sub')}</span>
      </div>
    </header>

    {#if !groups}
      <div class="loading"><span class="spin"><Spinner size={18} /></span></div>
    {:else if !groups.length}
      <p class="none">{t('duplicates.none')}</p>
    {:else}
      <div class="groups">
        {#each groups as group, i (group.map((c) => c.id).join(' '))}
          <section class="group">
            {#each group as c, j (c.id)}
              <label class="person" class:kept={keep[i] === j}>
                <input type="radio" name="keep-{i}" checked={keep[i] === j} onchange={() => (keep[i] = j)} aria-label={t('duplicates.keep')} />
                {#if c.photo}
                  <img class="photo" src={c.photo} alt="" />
                {:else}
                  <Avatar person={{ name: c.name, email: c.email ?? c.name }} size={30} />
                {/if}
                <span class="who">
                  <b>{c.name}</b>
                  <span>{[c.email, c.organization].filter(Boolean).join(' · ')}</span>
                </span>
                {#if keep[i] === j}<span class="tag">{t('duplicates.keep')}</span>{/if}
              </label>
            {/each}
            <div class="row-actions">
              <button class="btn sm" disabled={busy} onclick={() => merge([i])}>{t('duplicates.merge')}</button>
            </div>
          </section>
        {/each}
      </div>
    {/if}

    <footer>
      <button class="btn" onclick={onclose}>{t('common.close')}</button>
      {#if groups && groups.length > 1}
        <button class="btn primary" disabled={busy} onclick={() => merge(groups!.map((_, i) => i))}>
          {#if busy}<span class="spin"><Spinner size={14} /></span>{/if}{t('duplicates.mergeAll')}
        </button>
      {/if}
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
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow: auto;
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 16px;
  }
  .icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 10px;
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  h3 {
    margin: 0 0 2px;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .groups {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .group {
    padding: 6px;
    border-radius: 12px;
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .person {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px;
    border-radius: 8px;
    cursor: pointer;
  }
  .person:hover {
    background: var(--bg-hover);
  }
  .person input {
    accent-color: var(--accent);
  }
  .photo {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    object-fit: cover;
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .who b,
  .who span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who b {
    font-weight: 600;
  }
  .who span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .tag {
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 1px 8px;
    border-radius: 999px;
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .row-actions {
    display: flex;
    justify-content: flex-end;
    padding: 4px 4px 2px;
  }
  .none,
  .loading {
    display: grid;
    place-items: center;
    min-height: 100px;
    margin: 0;
    color: var(--text-3);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>

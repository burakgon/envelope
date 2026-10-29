<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { ContactSummary } from '../../api';
  import { t } from '../../i18n';
  import { Search, Spinner, UserPlus } from '../../icons';
  import { bookName, fold, people } from '../../stores/contacts.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import Avatar from '../Avatar.svelte';
  import { modal } from '../../actions/modal';

  // "Add people…": the contacts of a group's address book that aren't in it, to pick from.
  let { groupId, onclose }: { groupId: number; onclose: () => void } = $props();

  const uid = $props.id();
  const group = $derived(people.groups.find((g) => g.id === groupId) ?? null);
  const book = $derived(people.books.find((b) => b.id === group?.bookId));
  let all = $state<ContactSummary[] | null>(null);
  let query = $state('');
  let chosen = $state<number[]>([]);
  let busy = $state(false);

  onMount(async () => {
    const g = people.groups.find((x) => x.id === groupId);
    if (!g) {
      onclose();
      return;
    }
    try {
      all = (await api.contacts.list('', g.bookId, 0, 5000)).items;
    } catch (e) {
      toasts.error(errorMessage(e));
      onclose();
    }
  });

  const candidates = $derived.by(() => {
    if (!all || !group) return [];
    const inGroup = new Set(group.members.map((m) => m.contactId));
    const q = fold(query.trim());
    return all.filter((c) => !inGroup.has(c.id) && (!q || [c.name, c.email ?? '', c.organization ?? ''].some((v) => fold(v).includes(q))));
  });

  function toggle(id: number) {
    chosen = chosen.includes(id) ? chosen.filter((x) => x !== id) : [...chosen, id];
  }

  async function add() {
    if (!group || !chosen.length || busy) return;
    busy = true;
    await people.addToGroup(group, chosen);
    busy = false;
    onclose();
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onclose}>
  <div class="dialog" role="dialog" aria-labelledby="{uid}-title" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} use:modal>
    <header>
      <span class="icon"><UserPlus size={18} /></span>
      <div>
        <h3 id="{uid}-title">{t('groups.addTitle', { name: group?.name ?? '' })}</h3>
        {#if book}<span class="sub">{t('contacts.inBook', { book: bookName(book) })}</span>{/if}
      </div>
    </header>

    <div class="search">
      <Search size={15} />
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={query} placeholder={t('contacts.searchPlaceholder')} aria-label={t('contacts.searchPlaceholder')} spellcheck="false" autofocus />
    </div>

    <div class="people">
      {#if !all}
        <div class="none"><span class="spin"><Spinner size={18} /></span></div>
      {:else}
        {#each candidates as c (c.id)}
          <label class="person">
            <input type="checkbox" checked={chosen.includes(c.id)} onchange={() => toggle(c.id)} />
            {#if c.photo}
              <img class="photo" src={c.photo} alt="" />
            {:else}
              <Avatar person={{ name: c.name, email: c.email ?? c.name }} size={30} />
            {/if}
            <span class="who">
              <b>{c.name}</b>
              <span>{[c.email, c.organization].filter(Boolean).join(' · ')}</span>
            </span>
          </label>
        {:else}
          <p class="none">{query.trim() ? t('contacts.noMatches', { query: query.trim() }) : t('groups.everyoneIn')}</p>
        {/each}
      {/if}
    </div>

    <footer>
      <button class="btn" onclick={onclose}>{t('common.cancel')}</button>
      <button class="btn primary" disabled={!chosen.length || busy} onclick={add}>
        {#if busy}<span class="spin"><Spinner size={14} /></span>{/if}{t('groups.addCount', { count: chosen.length })}
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
    display: flex;
    flex-direction: column;
    width: min(480px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
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
    margin-bottom: 14px;
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
    overflow-wrap: anywhere;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    padding: 0 12px;
    margin-bottom: 8px;
    border-radius: 10px;
    color: var(--text-3);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    color: var(--text);
  }
  .search input:focus {
    outline: none;
  }
  .people {
    flex: 1;
    min-height: 120px;
    max-height: 360px;
    overflow-y: auto;
    margin: 0 -6px;
    padding: 0 6px;
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
  .none {
    display: grid;
    place-items: center;
    min-height: 100px;
    margin: 0;
    color: var(--text-3);
    text-align: center;
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

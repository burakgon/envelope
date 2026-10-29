<script lang="ts">
  // Below the search box: people for the `from:` or `to:` being typed, recent searches when the
  // box is empty, and the search options that write a query.
  import { api } from '../api';
  import type { ContactSuggestion } from '../api';
  import { t } from '../i18n';
  import { mail, roleLabel } from '../stores/mail.svelte';
  import { forgetSearch } from '../stores/searches.svelte';
  import { ui } from '../stores/ui.svelte';
  import { untrack } from 'svelte';
  import { addressAtEnd, buildQuery, completeAddress, noOptions, parseQuery, type SearchOptions } from '../utils/searchquery';
  import { Clock, X } from '../icons';
  import Avatar from './Avatar.svelte';

  let {
    query,
    building,
    onpick,
    open = $bindable(false),
  }: {
    query: string;
    /** The search options are open. */
    building: boolean;
    /** Puts a query in the box; with `run`, searches it at once. */
    onpick: (query: string, run: boolean) => void;
    /** Whether the panel shows anything (people, recent searches or the options). */
    open?: boolean;
  } = $props();

  const address = $derived(addressAtEnd(query));
  let people = $state<ContactSuggestion[]>([]);
  $effect(() => {
    const typed = address?.text ?? '';
    if (!typed) {
      people = [];
      return;
    }
    let live = true;
    api
      .suggestContacts(typed)
      .then((found) => live && (people = found.slice(0, 6)))
      .catch(() => {});
    return () => (live = false);
  });

  let options = $state<SearchOptions>({ ...noOptions });
  // Opened, the options show the search in the box, to change it.
  $effect(() => {
    if (building) options = parseQuery(untrack(() => query));
  });
  /**
   * The folder on screen, offered as the place to search: by role for the standard ones (`in:sent`),
   * else by path, and in its own account (`account:`) unless it is every account's inbox.
   */
  const here = $derived.by(() => {
    const box = mail.mailbox;
    if (box.kind === 'unified') return { folder: box.role, account: '', label: mail.mailboxTitle };
    const f = box.kind === 'folder' ? mail.folderById.get(box.id) : undefined;
    if (!f) return null;
    const account = mail.accountById.get(f.accountId);
    const name = f.role ? roleLabel(f.role) : f.name;
    return {
      folder: f.role || f.path,
      account: account?.email ?? '',
      label: t('search.thisFolder', { folder: mail.multiAccount && account ? `${name} (${account.label ?? account.email})` : name }),
    };
  });
  /** Where the options search: '' all mail, 'here' the folder on screen, 'other' a folder typed in the query. */
  const place = $derived(!options.folder ? '' : here && options.folder === here.folder && options.account === here.account ? 'here' : 'other');
  function choosePlace(value: string) {
    options.folder = value === 'here' && here ? here.folder : '';
    options.account = value === 'here' && here ? here.account : '';
  }
  const within = [
    { value: '', label: 'search.anyTime' },
    { value: '7d', label: 'search.lastWeek' },
    { value: '1m', label: 'search.lastMonth' },
    { value: '1y', label: 'search.lastYear' },
  ] as const;

  const recent = $derived(query.trim() ? [] : ui.settings.recentSearches);
  const showPeople = $derived(!building && people.length > 0);
  const showRecent = $derived(!building && !showPeople && recent.length > 0);
  $effect(() => {
    open = showPeople || showRecent || building;
    // Gone, it shows nothing.
    return () => (open = false);
  });

  let panel: HTMLDivElement | undefined = $state();
  /** Arrows move between the panel's buttons and fields; up from the first goes back to the box. */
  function key(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const target = e.target as HTMLElement;
    if (target.tagName === 'SELECT' || (target.tagName === 'INPUT' && (target as HTMLInputElement).type !== 'checkbox')) return;
    const items = [...(panel?.querySelectorAll<HTMLElement>('button.item, button.option') ?? [])];
    const i = items.indexOf(target);
    if (i < 0) return;
    e.preventDefault();
    if (e.key === 'ArrowUp' && i === 0) panel?.parentElement?.querySelector<HTMLInputElement>('input')?.focus();
    else items[Math.min(items.length - 1, Math.max(0, i + (e.key === 'ArrowDown' ? 1 : -1)))]?.focus();
  }

  function search(e: SubmitEvent) {
    e.preventDefault();
    const q = buildQuery(options);
    if (q) onpick(q, true);
  }
</script>

{#if showPeople || showRecent || building}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="panel" id="search-panel" tabindex="-1" bind:this={panel} onkeydown={key}>
    {#if showPeople}
      <div class="head">{t('search.people')}</div>
      {#each people as p (p.email)}
        <button class="item person" onclick={() => onpick(completeAddress(query, p.email), false)}>
          <Avatar person={p} size={22} />
          <span class="who">{p.name ?? p.email}</span>
          {#if p.name}<span class="sub">{p.email}</span>{/if}
        </button>
      {/each}
    {:else if showRecent}
      <div class="head">{t('search.recent')}</div>
      {#each recent as r (r)}
        <div class="row">
          <button class="item" onclick={() => onpick(r, true)}><Clock size={14} /><span class="who">{r}</span></button>
          <button class="forget icon-btn sm" aria-label={t('search.forget', { query: r })} onclick={() => forgetSearch(r)}><X size={13} /></button>
        </div>
      {/each}
    {:else}
      <form class="options" onsubmit={search}>
        <div class="head">{t('search.options')}</div>
        <div class="fields">
          <label class="field"><span>{t('search.from')}</span><input class="input" bind:value={options.from} spellcheck="false" /></label>
          <label class="field"><span>{t('search.to')}</span><input class="input" bind:value={options.to} spellcheck="false" /></label>
          <label class="field"><span>{t('search.subject')}</span><input class="input" bind:value={options.subject} /></label>
          <label class="field"><span>{t('search.words')}</span><input class="input" bind:value={options.words} /></label>
          <label class="field">
            <span>{t('search.date')}</span>
            <select class="input" bind:value={options.within}>
              {#each within as w (w.value)}<option value={w.value}>{t(w.label)}</option>{/each}
            </select>
          </label>
          {#if here || place === 'other'}
            <label class="field">
              <span>{t('search.where')}</span>
              <select class="input" value={place} onchange={(e) => choosePlace((e.currentTarget as HTMLSelectElement).value)}>
                <option value="">{t('search.allMail')}</option>
                {#if here}<option value="here">{here.label}</option>{/if}
                {#if place === 'other'}<option value="other">{t('search.thisFolder', { folder: options.folder })}</option>{/if}
              </select>
            </label>
          {/if}
        </div>
        <div class="checks">
          <label><input type="checkbox" bind:checked={options.attachment} /> {t('search.hasAttachment')}</label>
          <label><input type="checkbox" bind:checked={options.unread} /> {t('search.unread')}</label>
          <label><input type="checkbox" bind:checked={options.starred} /> {t('search.starred')}</label>
          <label><input type="checkbox" bind:checked={options.vip} /> {t('search.vip')}</label>
        </div>
        <div class="actions">
          <button type="button" class="btn option" onclick={() => (options = { ...noOptions })}>{t('common.clear')}</button>
          <button type="submit" class="btn primary option" disabled={!buildQuery(options)}>{t('common.search')}</button>
        </div>
      </form>
    {/if}
  </div>
{/if}

<style>
  .panel {
    position: absolute;
    left: 0;
    right: 0;
    top: calc(100% + 6px);
    z-index: 30;
    max-height: min(480px, calc(100vh - 160px));
    overflow-y: auto;
    padding: 6px;
    border-radius: 12px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
    animation: pop-in var(--dur-fast) var(--ease-out);
  }
  .head {
    padding: 6px 8px 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .row {
    display: flex;
    align-items: center;
  }
  .item {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 7px 8px;
    border-radius: 8px;
    text-align: left;
    color: var(--text);
  }
  .person {
    align-items: center;
  }
  .item :global(svg) {
    align-self: center;
    flex-shrink: 0;
    color: var(--text-3);
  }
  .item:hover,
  .item:focus-visible {
    background: var(--bg-hover);
  }
  .who {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .forget {
    opacity: 0;
  }
  .row:hover .forget,
  .forget:focus-visible {
    opacity: 1;
  }
  .options {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 0 4px 4px;
  }
  /* One grid for all fields, so the names take the width of the longest one. */
  .fields {
    display: grid;
    grid-template-columns: max-content 1fr;
    align-items: center;
    gap: 6px 10px;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .field {
    display: contents;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    padding: 4px 0;
    font-size: var(--text-sm);
  }
  .checks label {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }
  .checks input {
    accent-color: var(--accent);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
</style>

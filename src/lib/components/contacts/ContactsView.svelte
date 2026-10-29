<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import { people, bookName } from '../../stores/contacts.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { ui } from '../../stores/ui.svelte';
  import Avatar from '../Avatar.svelte';
  import FileViewer from '../FileViewer.svelte';
  import Popover from '../Popover.svelte';
  import WindowControls from '../WindowControls.svelte';
  import ContactDetail from './ContactDetail.svelte';
  import ContactEditor from './ContactEditor.svelte';
  import DuplicatesDialog from './DuplicatesDialog.svelte';
  import { intlLocale, t } from '../../i18n';
  import { Download, Merge, More, PanelLeft, Plus, Search, Upload, Users } from '../../icons';

  let searchEl: HTMLInputElement | undefined = $state();
  let menu = $state<{ x: number; y: number } | null>(null);
  /** A file of contacts being looked at, to choose whom to add. */
  let importing = $state<string | null>(null);
  let duplicates = $state(false);

  /** Opens a vCard file or a CSV export (Gmail, Outlook, Thunderbird) to choose whom to add. */
  async function importContacts() {
    menu = null;
    const path = await api.pickFile(t('contacts.importTitle'), [{ name: t('contacts.importFiles'), extensions: ['vcf', 'vcard', 'csv'] }]);
    if (path) importing = path;
  }

  /** Writes the address book shown, or every one, to a vCard file. */
  async function exportContacts() {
    menu = null;
    const book = people.books.find((b) => b.id === people.bookId);
    const path = await api.pickSavePath(`${book ? bookName(book) : t('contacts.title')}.vcf`);
    if (!path) return;
    try {
      const count = await api.contacts.export(people.bookId, path);
      toasts.show(t('contacts.exported', { count }), {
        kind: 'success',
        action: { label: t('message.showInFolder'), run: () => api.revealPath(path).catch((e) => toasts.error(errorMessage(e))) },
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  onMount(() => {
    people.init();
  });

  $effect(() => {
    if (ui.focusSearch && ui.view === 'contacts') searchEl?.focus();
  });

  // Alphabetical sections, with the letters upper-cased by the rules of the interface language.
  const sections = $derived.by(() => {
    const out: { key: string; letter: string; items: typeof people.items }[] = [];
    const locale = intlLocale();
    for (const c of people.items) {
      const ch = c.name.trim().charAt(0).toLocaleUpperCase(locale);
      const letter = /\p{L}/u.test(ch) ? ch : '#';
      const last = out[out.length - 1];
      if (last?.letter === letter) last.items.push(c);
      // The list comes sorted by language-neutral case rules, so a letter can return after another
      // (Turkish "ilker" is İ here but sorts with "Irmak"); a repeated section needs a key of its own.
      else out.push({ key: out.some((s) => s.letter === letter) ? `${letter}${out.length}` : letter, letter, items: [c] });
    }
    return out;
  });

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      people.step(1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      people.step(-1);
    } else if (e.key === 'Escape') {
      if (people.query) people.search('');
      searchEl?.blur();
    }
  }
</script>

<section class="contacts">
  <div class="list">
    <header data-tauri-drag-region>
      {#if ui.sidebarCollapsed}
        <button class="icon-btn" title={t('contacts.showSidebar')} onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
      {/if}
      <h1 data-tauri-drag-region>{t('contacts.title')}</h1>
      <span class="count" data-tauri-drag-region>{people.total}</span>
      <span class="grow" data-tauri-drag-region></span>
      <button
        class="icon-btn"
        title={t('common.more')}
        aria-label={t('common.more')}
        aria-haspopup="menu"
        onclick={(e) => {
          const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
          menu = { x: r.right, y: r.bottom + 4 };
        }}><More size={18} /></button>
      <button class="icon-btn" title={t('contacts.newContactShortcut', { key: 'C' })} onclick={() => people.newContact()}><Plus size={18} /></button>
    </header>
    <div class="search">
      <Search size={15} />
      <input
        bind:this={searchEl}
        placeholder={t('contacts.searchPlaceholder')}
        value={people.query}
        oninput={(e) => people.search((e.currentTarget as HTMLInputElement).value)}
        onkeydown={key}
        spellcheck="false"
      />
    </div>
    <div class="items">
      {#each sections as s (s.key)}
        <div class="section">
        <div class="letter">{s.letter}</div>
        {#each s.items as c (c.id)}
          <button class="row" class:on={people.selectedId === c.id} onclick={() => people.select(c.id)}>
            {#if c.photo}
              <img class="photo" src={c.photo} alt="" />
            {:else}
              <Avatar person={{ name: c.name, email: c.email ?? c.name }} size={34} />
            {/if}
            <span class="txt">
              <b class="truncate">{c.name}</b>
              <span class="truncate">{c.organization ?? c.email ?? ''}</span>
            </span>
          </button>
        {/each}
        </div>
      {:else}
        <div class="empty">
          {#if people.query}
            {t('contacts.noMatches', { query: people.query })}
          {:else if !people.loading}
            <Users size={28} />
            <p>{t('contacts.empty')}</p>
            <button class="btn" onclick={() => people.newContact()}><Plus size={15} />{t('contacts.newContact')}</button>
          {/if}
        </div>
      {/each}
    </div>
  </div>

  <div class="pane">
    <header data-tauri-drag-region><span class="grow" data-tauri-drag-region></span><WindowControls /></header>
    {#if people.editing}
      {#key people.editing.id ?? 'new'}
        <ContactEditor />
      {/key}
    {:else if people.detail}
      {#key people.detail.id}
        <ContactDetail contact={people.detail} />
      {/key}
    {:else}
      <div class="none">
        <Users size={40} strokeWidth={1.4} />
        <p>{t('contacts.selectPrompt')}</p>
      </div>
    {/if}
  </div>
</section>

{#if menu}
  <Popover x={menu.x} y={menu.y} align="end" onclose={() => (menu = null)} width={240}>
    <button class="menu-item" onclick={importContacts}><Upload size={15} />{t('contacts.import')}</button>
    <button class="menu-item" onclick={exportContacts}><Download size={15} />{t('contacts.export')}</button>
    <button class="menu-item" onclick={() => { menu = null; duplicates = true; }}><Merge size={15} />{t('contacts.findDuplicates')}</button>
  </Popover>
{/if}
{#if importing}<FileViewer path={importing} onclose={() => (importing = null)} />{/if}
{#if duplicates}<DuplicatesDialog onclose={() => (duplicates = false)} />{/if}

<style>
  .contacts {
    flex: 1;
    min-width: 0;
    display: flex;
    background: var(--bg);
  }
  .list {
    width: 340px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    background: var(--bg-list, var(--bg));
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--header-h);
    padding: 0 10px 0 18px;
    flex-shrink: 0;
  }
  h1 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  .count {
    font-size: var(--text-sm);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
    margin-top: 3px;
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    margin: 0 12px 8px;
    padding: 0 12px;
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
  .items {
    flex: 1;
    overflow-y: auto;
    padding: 0 8px 16px;
  }
  .letter {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 8px 10px 4px;
    font-size: 11.5px;
    font-weight: 700;
    color: var(--text-3);
    background: var(--bg-list, var(--bg));
  }
  .row {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    height: 50px;
    padding: 0 10px;
    border-radius: 10px;
    text-align: left;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.on {
    background: var(--bg-selected);
  }
  .photo {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    object-fit: cover;
    flex-shrink: 0;
  }
  .txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .txt b {
    font-weight: 600;
  }
  .row.on .txt b {
    color: var(--accent-text);
  }
  .txt span {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 48px 20px;
    color: var(--text-3);
    text-align: center;
  }
  .empty p {
    margin: 0;
  }
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .pane header {
    padding: 0 10px;
  }
  .none {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--text-3);
  }
  .none p {
    margin: 0;
    color: var(--text-3);
  }
</style>

<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import { people, bookName, groupAddresses } from '../../stores/contacts.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { ui } from '../../stores/ui.svelte';
  import Avatar from '../Avatar.svelte';
  import FileViewer from '../FileViewer.svelte';
  import Popover from '../Popover.svelte';
  import WindowControls from '../WindowControls.svelte';
  import AddPeopleDialog from './AddPeopleDialog.svelte';
  import ContactDetail from './ContactDetail.svelte';
  import ContactEditor from './ContactEditor.svelte';
  import DuplicatesDialog from './DuplicatesDialog.svelte';
  import { intlLocale, t } from '../../i18n';
  import { Download, Mail, Merge, More, PanelLeft, Pen, Plus, Search, Trash, Upload, UserMinus, UserPlus, Users, UsersRound } from '../../icons';

  let searchEl: HTMLInputElement | undefined = $state();
  let menu = $state<{ x: number; y: number } | null>(null);
  /** The menu of the group shown (Rename…, Delete group…). */
  let groupMenu = $state<{ x: number; y: number } | null>(null);
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
      {#if ui.sidebarHidden}
        <button class="icon-btn" title={t('contacts.showSidebar')} onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
      {/if}
      {#if people.group}
        <h1 class="truncate" title={people.group.name} data-tauri-drag-region>{people.group.name}</h1>
        <span class="count" data-tauri-drag-region>{people.group.members.length}</span>
      {:else}
        <h1 data-tauri-drag-region>{t('contacts.title')}</h1>
        <span class="count" data-tauri-drag-region>{people.total}</span>
      {/if}
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
    {#if people.group}
      {@const g = people.group}
      <div class="group-bar">
        <button class="btn primary sm" title={t('groups.write')} disabled={!groupAddresses(g).length} onclick={() => people.writeTo(g)}>
          <Mail size={14} /><span class="truncate">{t('groups.write')}</span>
        </button>
        {#if g.readOnly}
          <span class="note">{t('groups.readOnly')}</span>
        {:else}
          <button class="btn sm" title={t('groups.addPeople')} onclick={() => (people.adding = g.id)}>
            <UserPlus size={14} /><span class="truncate">{t('groups.addPeople')}</span>
          </button>
          <span class="grow"></span>
          <button
            class="icon-btn sm group-menu"
            title={t('common.more')}
            aria-label={t('common.more')}
            aria-haspopup="menu"
            onclick={(e) => {
              const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
              groupMenu = { x: r.right, y: r.bottom + 4 };
            }}><More size={16} /></button>
        {/if}
      </div>
    {/if}
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
      {#if people.group}
        {@const g = people.group}
        {#each people.members as m (m.contactId ?? m.email)}
          <div class="member">
            {#if m.contactId != null}
              <button class="row" class:on={people.selectedId === m.contactId} onclick={() => people.select(m.contactId)}>
                {@render person(m.name, m.email, m.photo, m.organization ?? m.email)}
              </button>
            {:else}
              <div class="row">{@render person(m.name, m.email, null, m.name !== m.email ? m.email : null)}</div>
            {/if}
            {#if !g.readOnly}
              <button
                class="icon-btn sm remove"
                title={t('groups.remove')}
                aria-label={t('groups.removeName', { name: m.name })}
                onclick={() => people.removeFromGroup(g, m)}><UserMinus size={15} /></button>
            {/if}
          </div>
        {:else}
          <div class="empty">
            {#if people.query}
              {t('contacts.noMatches', { query: people.query })}
            {:else}
              <UsersRound size={28} />
              <p>{t('groups.empty')}</p>
              {#if !g.readOnly}<button class="btn" onclick={() => (people.adding = g.id)}><UserPlus size={15} />{t('groups.addPeople')}</button>{/if}
            {/if}
          </div>
        {/each}
      {:else}
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
      {/if}
    </div>
  </div>

  {#snippet person(name: string, email: string | null, photo: string | null, line: string | null)}
    {#if photo}
      <img class="photo" src={photo} alt="" />
    {:else}
      <Avatar person={{ name, email: email ?? name }} size={34} />
    {/if}
    <span class="txt">
      <b class="truncate">{name}</b>
      <span class="truncate">{line ?? ''}</span>
    </span>
  {/snippet}

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
    <button class="menu-item" onclick={() => { menu = null; people.newGroup(); }}><UsersRound size={15} />{t('groups.newGroupItem')}</button>
    <button class="menu-item" onclick={importContacts}><Upload size={15} />{t('contacts.import')}</button>
    <button class="menu-item" onclick={exportContacts}><Download size={15} />{t('contacts.export')}</button>
    <button class="menu-item" onclick={() => { menu = null; duplicates = true; }}><Merge size={15} />{t('contacts.findDuplicates')}</button>
  </Popover>
{/if}
{#if importing}<FileViewer path={importing} onclose={() => (importing = null)} />{/if}
{#if groupMenu && people.group}
  {@const g = people.group}
  <Popover x={groupMenu.x} y={groupMenu.y} align="end" onclose={() => (groupMenu = null)} width={220}>
    <button class="menu-item" onclick={() => { groupMenu = null; people.renameGroup(g); }}><Pen size={15} />{t('groups.renameItem')}</button>
    <button class="menu-item danger" onclick={() => { groupMenu = null; people.deleteGroup(g); }}><Trash size={15} />{t('groups.deleteItem')}</button>
  </Popover>
{/if}
{#if duplicates}<DuplicatesDialog onclose={() => (duplicates = false)} />{/if}
{#if people.adding != null}<AddPeopleDialog groupId={people.adding} onclose={() => (people.adding = null)} />{/if}

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
  h1.truncate {
    min-width: 0;
  }
  /* What can be done with the group shown; long labels shorten rather than wrap. */
  .group-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 12px 10px;
  }
  .group-bar .btn {
    gap: 6px;
    min-width: 0;
  }
  .group-bar :global(svg) {
    flex-shrink: 0;
  }
  .note {
    font-size: var(--text-sm);
    color: var(--text-3);
    white-space: nowrap;
  }
  /* One of the group's people, with Remove on hover or when reached with the keyboard. */
  .member {
    position: relative;
  }
  .member:has(.remove) .row {
    padding-right: 40px;
  }
  .remove {
    position: absolute;
    top: 50%;
    right: 8px;
    transform: translateY(-50%);
    opacity: 0;
    transition: opacity var(--dur-fast);
  }
  .member:hover .remove,
  .remove:focus-visible {
    opacity: 1;
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

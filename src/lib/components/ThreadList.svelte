<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { ThreadSummary } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import ThreadRow from './ThreadRow.svelte';
  import Popover from './Popover.svelte';
  import SnoozeMenu from './SnoozeMenu.svelte';
  import MoveMenu from './MoveMenu.svelte';
  import EmptyState from './EmptyState.svelte';
  import {
    Archive, Check, CheckCircle, Clock, Filter, Folder, Forward, Mail, MailOpen, More, Newspaper, PanelLeft, Paperclip,
    Bell, Reply, ReplyAll, Search, ShieldAlert, Star, Tag, Trash, Users, X, Refresh,
  } from '../icons';

  let viewport: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let height = $state(600);
  let searchInput: HTMLInputElement | undefined = $state();
  let query = $state('');
  let searchFocused = $state(false);
  let menu = $state<{ x: number; y: number; thread: ThreadSummary } | null>(null);
  let snoozeAt = $state<{ x: number; y: number; ids: number[] } | null>(null);
  let moveAt = $state<{ x: number; y: number; ids: number[]; labels: boolean } | null>(null);
  let filterAt = $state<{ x: number; y: number } | null>(null);
  let resizing = $state(false);

  const rowH = $derived(ui.settings.density === 'compact' ? 54 : 76);
  const overscan = 8;
  const start = $derived(Math.max(0, Math.floor(scrollTop / rowH) - overscan));
  const end = $derived(Math.min(mail.items.length, Math.ceil((scrollTop + height) / rowH) + overscan));
  const visible = $derived(mail.items.slice(start, end));
  const filterActive = $derived(!!(mail.filter.unread || mail.filter.starred || mail.filter.attachments));

  const tabs = [
    { id: 'people', label: 'People', icon: Users },
    { id: 'newsletters', label: 'Newsletters', icon: Newspaper },
    { id: 'notifications', label: 'Notifications', icon: Bell },
    { id: null, label: 'All', icon: Mail },
  ] as const;

  $effect(() => {
    // Infinite scroll.
    if (end > mail.items.length - 40 && mail.items.length < mail.total) mail.loadMore();
  });

  $effect(() => {
    // Keep the selected row in view when navigating with the keyboard.
    const id = mail.selectedId;
    if (id == null || !viewport) return;
    const i = mail.items.findIndex((t) => t.id === id);
    if (i < 0) return;
    const top = i * rowH;
    const vp = viewport;
    if (top < vp.scrollTop) vp.scrollTop = top - 4;
    else if (top + rowH > vp.scrollTop + vp.clientHeight) vp.scrollTop = top + rowH - vp.clientHeight + 4;
  });

  $effect(() => {
    if (ui.focusSearch) tick().then(() => searchInput?.focus());
  });

  $effect(() => {
    // Reset scroll when the mailbox changes.
    void mail.mailbox;
    void mail.category;
    if (viewport) viewport.scrollTop = 0;
  });

  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (viewport) height = viewport.clientHeight;
    });
    if (viewport) ro.observe(viewport);
    return () => ro.disconnect();
  });

  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearchInput() {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => mail.setSearch(query.trim()), 140);
  }

  function clearSearch() {
    query = '';
    mail.setSearch('');
    searchInput?.blur();
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      clearSearch();
    } else if (e.key === 'Enter') {
      if (searchTimer) clearTimeout(searchTimer);
      mail.setSearch(query.trim());
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      searchInput?.blur();
      mail.move(0);
    }
  }

  function open(e: MouseEvent, t: ThreadSummary) {
    if (e.shiftKey) mail.checkRange(t.id);
    else if (e.ctrlKey || e.metaKey) mail.toggleCheck(t.id);
    else {
      if (mail.checked.size) mail.checked.clear();
      mail.select(t.id);
    }
  }

  function context(e: MouseEvent, t: ThreadSummary) {
    e.preventDefault();
    if (!mail.checked.has(t.id) && mail.selectedId !== t.id) mail.select(t.id);
    menu = { x: e.clientX, y: e.clientY, thread: t };
  }

  function menuIds(): number[] {
    if (!menu) return [];
    return mail.checked.has(menu.thread.id) ? [...mail.checked] : [menu.thread.id];
  }

  async function replyTo(mode: 'reply' | 'replyAll' | 'forward') {
    const t = menu?.thread;
    menu = null;
    if (!t) return;
    mail.select(t.id);
    const d = await import('../api').then(({ api }) => api.thread(t.id));
    const last = d.messages[d.messages.length - 1];
    if (last) compose.reply(last.id, mode, mode !== 'forward');
  }

  function startResize(e: PointerEvent) {
    resizing = true;
    const startX = e.clientX;
    const startW = ui.settings.listWidth;
    const move = (ev: PointerEvent) => {
      ui.settings.listWidth = Math.min(640, Math.max(300, startW + ev.clientX - startX));
    };
    const up = () => {
      resizing = false;
      ui.set('listWidth', ui.settings.listWidth);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function bulkRect(e: MouseEvent): { x: number; y: number } {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: r.left, y: r.bottom + 4 };
  }

  const inTrash = $derived(mail.mailbox.kind === 'folder' && mail.folderById.get(mail.mailbox.id)?.role === 'trash');
  const inSnoozed = $derived(mail.mailbox.kind === 'snoozed');
</script>

<section class="list" class:resizing style:width="{ui.settings.listWidth}px">
  <header data-tauri-drag-region>
    {#if ui.sidebarCollapsed}
      <button class="icon-btn" title="Show sidebar" onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
    {/if}
    <div class="title" data-tauri-drag-region>
      <h1 data-tauri-drag-region>{mail.mailboxTitle}</h1>
      {#if mail.search}
        <span class="sub">{mail.total} {mail.total === 1 ? 'result' : 'results'}</span>
      {:else if mail.mailboxSubtitle}
        <span class="sub">{mail.mailboxSubtitle}</span>
      {/if}
    </div>
    <div class="tools">
      <button class="icon-btn" class:active={filterActive} title="Filter" onclick={(e) => (filterAt = bulkRect(e))}><Filter size={16} /></button>
      <button class="icon-btn" title="Refresh" onclick={() => import('../api').then(({ api }) => api.syncNow(null))}><Refresh size={16} /></button>
    </div>
  </header>

  <div class="search" class:focused={searchFocused}>
    <Search size={15} />
    <input
      bind:this={searchInput}
      bind:value={query}
      oninput={onSearchInput}
      onkeydown={onSearchKey}
      onfocus={() => (searchFocused = true)}
      onblur={() => (searchFocused = false)}
      placeholder="Search mail"
      spellcheck="false"
      aria-label="Search mail"
    />
    {#if query}
      <button class="clear" aria-label="Clear search" onclick={clearSearch}><X size={13} /></button>
    {:else}
      <span class="kbd">/</span>
    {/if}
  </div>

  {#if mail.splitActive}
    <div class="tabs" role="tablist">
      {#each tabs as t (t.label)}
        <button role="tab" aria-selected={mail.category === t.id} class:on={mail.category === t.id} onclick={() => mail.setCategory(t.id)}>
          {t.label}
          {#if t.id && mail.categoryUnread[t.id]}<span class="badge">{mail.categoryUnread[t.id]}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  {#if mail.checked.size > 0}
    <div class="bulk">
      <button class="icon-btn sm" title="Clear selection" onclick={() => mail.checked.clear()}><X size={15} /></button>
      <span class="sel">{mail.checked.size} selected</span>
      <button class="link" onclick={() => mail.items.forEach((t) => mail.checked.add(t.id))}>Select all</button>
      <span class="spacer"></span>
      <button class="icon-btn sm" title="Archive" onclick={() => mail.archive()}><Archive size={15} /></button>
      <button class="icon-btn sm" title="Delete" onclick={() => mail.trash()}><Trash size={15} /></button>
      <button class="icon-btn sm" title="Mark read" onclick={() => mail.setRead(mail.targets(), true)}><MailOpen size={15} /></button>
      <button class="icon-btn sm" title="Star" onclick={() => mail.toggleStar()}><Star size={15} /></button>
      <button class="icon-btn sm" title="Snooze" onclick={(e) => (snoozeAt = { ...bulkRect(e), ids: mail.targets() })}><Clock size={15} /></button>
      <button class="icon-btn sm" title="Move" onclick={(e) => (moveAt = { ...bulkRect(e), ids: mail.targets(), labels: false })}><Folder size={15} /></button>
    </div>
  {/if}

  <div class="viewport" bind:this={viewport} onscroll={() => (scrollTop = viewport?.scrollTop ?? 0)} role="listbox" aria-label="Conversations" tabindex="0">
    {#if mail.loading && mail.items.length === 0}
      {#each Array(8) as _, i (i)}
        <div class="skel" style:height="{rowH}px">
          <span class="skeleton" style="width:34px;height:34px;border-radius:50%"></span>
          <span class="lines">
            <span class="skeleton" style="width:{40 + ((i * 17) % 35)}%;height:12px"></span>
            <span class="skeleton" style="width:{60 + ((i * 13) % 30)}%;height:10px"></span>
            <span class="skeleton" style="width:{70 + ((i * 7) % 25)}%;height:10px"></span>
          </span>
        </div>
      {/each}
    {:else if mail.items.length === 0}
      <EmptyState />
    {:else}
      <div class="spacer-box" style:height="{mail.items.length * rowH + 12}px">
        <div class="rows" style:transform="translateY({start * rowH}px)">
          {#each visible as t (t.id)}
            <ThreadRow
              thread={t}
              selected={mail.selectedId === t.id}
              checked={mail.checked.has(t.id)}
              onopen={(e) => open(e, t)}
              oncontext={(e) => context(e, t)}
              onsnooze={(e) => {
                const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                snoozeAt = { x: r.right, y: r.bottom + 4, ids: [t.id] };
              }}
            />
          {/each}
        </div>
      </div>
    {/if}
  </div>
  <div class="resizer" role="separator" aria-orientation="vertical" onpointerdown={startResize}></div>
</section>

{#if menu}
  {@const ids = menuIds()}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={232}>
    {#if ids.length === 1}
      <button class="menu-item" onclick={() => replyTo('reply')}><Reply size={15} />Reply<span class="hint">R</span></button>
      <button class="menu-item" onclick={() => replyTo('replyAll')}><ReplyAll size={15} />Reply all<span class="hint">A</span></button>
      <button class="menu-item" onclick={() => replyTo('forward')}><Forward size={15} />Forward<span class="hint">F</span></button>
      <div class="divider"></div>
    {/if}
    {#if inSnoozed}
      <button class="menu-item" onclick={() => { mail.unsnooze(ids); menu = null; }}><Mail size={15} />Unsnooze</button>
    {/if}
    <button class="menu-item" onclick={() => { mail.archive(ids); menu = null; }}><Archive size={15} />Archive<span class="hint">E</span></button>
    <button class="menu-item" onclick={() => { mail.trash(ids); menu = null; }}><Trash size={15} />{inTrash ? 'Delete forever' : 'Delete'}<span class="hint">#</span></button>
    <button class="menu-item" onclick={() => { mail.toggleRead(ids); menu = null; }}>
      {#if menu.thread.unreadCount > 0}<MailOpen size={15} />Mark as read{:else}<Mail size={15} />Mark as unread{/if}<span class="hint">U</span>
    </button>
    <button class="menu-item" onclick={() => { mail.toggleStar(ids); menu = null; }}><Star size={15} />{menu.thread.flagged ? 'Unstar' : 'Star'}<span class="hint">S</span></button>
    <button class="menu-item" onclick={() => { snoozeAt = { x: menu!.x, y: menu!.y, ids }; menu = null; }}><Clock size={15} />Snooze…<span class="hint">H</span></button>
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { moveAt = { x: menu!.x, y: menu!.y, ids, labels: false }; menu = null; }}><Folder size={15} />Move to…<span class="hint">V</span></button>
    {#if mail.accountById.get(menu.thread.accountId)?.kind === 'gmail'}
      <button class="menu-item" onclick={() => { moveAt = { x: menu!.x, y: menu!.y, ids, labels: true }; menu = null; }}><Tag size={15} />Label…<span class="hint">L</span></button>
    {/if}
    <button class="menu-item" onclick={() => { mail.moveTo({ kind: 'junk' }, ids); menu = null; }}><ShieldAlert size={15} />Report spam<span class="hint">!</span></button>
  </Popover>
{/if}

{#if snoozeAt}
  {@const ids = snoozeAt.ids}
  <SnoozeMenu x={snoozeAt.x} y={snoozeAt.y} align="end" onclose={() => (snoozeAt = null)} onpick={(ts) => { snoozeAt = null; mail.snooze(ts, ids); }} />
{/if}

{#if moveAt}
  <MoveMenu x={moveAt.x} y={moveAt.y} ids={moveAt.ids} labels={moveAt.labels} onclose={() => (moveAt = null)} />
{/if}

{#if filterAt}
  <Popover x={filterAt.x} y={filterAt.y} onclose={() => (filterAt = null)} width={220}>
    <div class="menu-label">Show only</div>
    {#each [{ k: 'unread', l: 'Unread', i: Mail }, { k: 'starred', l: 'Starred', i: Star }, { k: 'attachments', l: 'With attachments', i: Paperclip }] as f (f.k)}
      {@const on = !!mail.filter[f.k as 'unread']}
      <button class="menu-item" onclick={() => mail.setFilter({ ...mail.filter, [f.k]: !on })}>
        <f.i size={15} />{f.l}
        {#if on}<span class="hint"><Check size={14} /></span>{/if}
      </button>
    {/each}
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { mail.markAllRead(); filterAt = null; }}><CheckCircle size={15} />Mark all as read</button>
    <button class="menu-item" onclick={() => { ui.set('density', ui.settings.density === 'compact' ? 'comfortable' : 'compact'); filterAt = null; }}>
      <More size={15} />{ui.settings.density === 'compact' ? 'Comfortable rows' : 'Compact rows'}
    </button>
  </Popover>
{/if}

<style>
  .list {
    position: relative;
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
    height: 100%;
    min-width: 0;
    background: var(--bg);
    border-right: 1px solid var(--border);
  }
  header {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--header-h);
    padding: 0 10px 0 18px;
    flex-shrink: 0;
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h1 {
    margin: 0;
    font-size: 17px;
    font-weight: 650;
    letter-spacing: -0.015em;
    white-space: nowrap;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tools {
    display: flex;
    gap: 2px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 34px;
    margin: 0 12px 8px;
    padding: 0 8px 0 11px;
    border-radius: 10px;
    background: var(--bg-sunken);
    color: var(--text-3);
    box-shadow: inset 0 0 0 1px var(--border);
    transition: box-shadow var(--dur-fast), background var(--dur-fast);
  }
  .search.focused {
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--accent), 0 0 0 3px var(--accent-soft);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    background: transparent;
    height: 100%;
    color: var(--text);
  }
  .search .kbd {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 5px;
    background: var(--bg-active);
    color: var(--text-3);
  }
  .clear {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--bg-active);
    color: var(--text-2);
  }
  .tabs {
    display: flex;
    gap: 2px;
    padding: 0 12px 8px;
    flex-shrink: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar {
    display: none;
  }
  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px;
    flex-shrink: 0;
    white-space: nowrap;
    border-radius: 8px;
    color: var(--text-3);
    font-weight: 550;
    font-size: var(--text-sm);
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .tabs button:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .tabs button.on {
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .badge {
    font-size: 10.5px;
    font-weight: 700;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    background: var(--accent);
    color: var(--accent-fg);
  }
  .bulk {
    display: flex;
    align-items: center;
    gap: 4px;
    margin: 0 10px 6px;
    padding: 4px 6px;
    border-radius: 10px;
    background: var(--accent-soft);
    animation: slide-up var(--dur) var(--ease-out);
  }
  .sel {
    font-weight: 600;
    color: var(--accent-text);
    margin-left: 2px;
  }
  .link {
    color: var(--accent-text);
    font-size: var(--text-sm);
    margin-left: 6px;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .bulk .spacer {
    flex: 1;
  }
  .viewport {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    contain: strict;
    padding-top: 2px;
  }
  .spacer-box {
    position: relative;
  }
  .rows {
    position: absolute;
    inset: 0 0 auto 0;
    will-change: transform;
  }
  .skel {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 14px 18px;
  }
  .lines {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .resizer {
    position: absolute;
    top: 0;
    right: -3px;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    z-index: 5;
  }
  .resizer:hover,
  .resizing .resizer {
    background: linear-gradient(to right, transparent 2px, var(--accent) 2px, var(--accent) 4px, transparent 4px);
    opacity: 0.6;
  }
</style>

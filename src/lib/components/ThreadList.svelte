<script lang="ts">
  import { api } from '../api';
  import { onMount, tick, untrack } from 'svelte';
  import type { ThreadSummary } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { ai } from '../stores/ai.svelte';
  import ThreadRow from './ThreadRow.svelte';
  import Popover from './Popover.svelte';
  import SnoozeMenu from './SnoozeMenu.svelte';
  import MoveMenu from './MoveMenu.svelte';
  import EmptyState from './EmptyState.svelte';
  import {
    Archive, Check, CheckCircle, Clock, FileArrow, Filter, Folder, Forward, Mail, MailOpen, More, Newspaper, PanelLeft, Paperclip,
    Bell, Pen, Reply, ReplyAll, Search, ShieldAlert, Star, Tag, Trash, Users, X, Refresh, Sparkles,
    AlarmClock,
    ShieldCheck,
    Ban,
    BookmarkPlus,
    Sliders,
    Crown,
    LayoutPanel, Mails, PanelBottom, PanelOff, PanelRight, SortNewest, SortOldest, SortSender, PopOut,
  } from '../icons';
  import type { SortOrder } from '../api';
  import type { ReadingPane } from '../stores/ui.svelte';
  import WindowControls from './WindowControls.svelte';
  import { vips } from '../stores/vips.svelte';
  import { blockSender, senderOf } from '../stores/rules.svelte';
  import { displayName } from '../utils/format';
  import AccountAlerts from './AccountAlerts.svelte';
  import SearchPanel from './SearchPanel.svelte';
  import { rememberSearch, saveSearch } from '../stores/searches.svelte';
  import { t } from '../i18n';

  let viewport: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let height = $state(600);
  let searchInput: HTMLInputElement | undefined = $state();
  let query = $state(untrack(() => mail.search));
  /** What this box last searched for: a search set anywhere else, or cleared by going to a folder, shows in it. */
  let sent = untrack(() => mail.search);
  let searchFocused = $state(false);
  /** The focus is in the search box or the panel below it. */
  let searchWithin = $state(false);
  /** The search options are open below the box. */
  let building = $state(false);
  let searchArea: HTMLDivElement | undefined = $state();
  let menu = $state<{ x: number; y: number; thread: ThreadSummary } | null>(null);
  let snoozeAt = $state<{ x: number; y: number; ids: number[] } | null>(null);
  let remindAt = $state<{ x: number; y: number; ids: number[] } | null>(null);
  let moveAt = $state<{ x: number; y: number; ids: number[]; labels: boolean } | null>(null);
  let filterAt = $state<{ x: number; y: number } | null>(null);
  /** The View menu: where the reading pane goes, conversations or messages, the order. */
  let viewAt = $state<{ x: number; y: number } | null>(null);
  let resizing = $state(false);
  let section: HTMLElement | undefined = $state();

  const panes: { id: ReadingPane; label: 'view.paneRight' | 'view.paneBottom' | 'view.paneOff'; icon: typeof PanelRight }[] = [
    { id: 'right', label: 'view.paneRight', icon: PanelRight },
    { id: 'bottom', label: 'view.paneBottom', icon: PanelBottom },
    { id: 'off', label: 'view.paneOff', icon: PanelOff },
  ];
  const sorts: { id: SortOrder; label: 'view.sortNewest' | 'view.sortOldest' | 'view.sortSender'; icon: typeof SortNewest }[] = [
    { id: 'newest', label: 'view.sortNewest', icon: SortNewest },
    { id: 'oldest', label: 'view.sortOldest', icon: SortOldest },
    { id: 'sender', label: 'view.sortSender', icon: SortSender },
  ];

  const rowH = $derived(ui.settings.density === 'compact' ? 54 : 76);
  const overscan = 8;
  const start = $derived(Math.max(0, Math.floor(scrollTop / rowH) - overscan));
  const end = $derived(Math.min(mail.items.length, Math.ceil((scrollTop + height) / rowH) + overscan));
  const visible = $derived(mail.items.slice(start, end));
  const filterActive = $derived(!!(mail.filter.unread || mail.filter.starred || mail.filter.attachments));

  const tabs = [
    { id: 'people', label: 'threadList.tabPeople', icon: Users },
    { id: 'newsletters', label: 'threadList.tabNewsletters', icon: Newspaper },
    { id: 'notifications', label: 'threadList.tabNotifications', icon: Bell },
    { id: null, label: 'threadList.tabAll', icon: Mail },
  ] as const;

  const filters = [
    { k: 'unread', label: 'threadList.filterUnread', icon: Mail },
    { k: 'starred', label: 'threadList.filterStarred', icon: Star },
    { k: 'attachments', label: 'threadList.filterAttachments', icon: Paperclip },
  ] as const;

  $effect(() => {
    // Infinite scroll.
    if (end > mail.items.length - 40 && mail.items.length < mail.total) mail.loadMore();
  });

  $effect(() => {
    // Keep the selected row in view when navigating with the keyboard.
    const id = mail.selectedId;
    if (id == null || !viewport) return;
    const i = mail.items.findIndex((th) => th.id === id);
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
    // Back from a conversation with one pane at a time: the keyboard goes on in the list.
    if (ui.single && !document.querySelector('[aria-modal="true"]')) viewport?.focus({ preventScroll: true });
    return () => ro.disconnect();
  });

  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function search(text: string) {
    if (searchTimer) clearTimeout(searchTimer);
    sent = text;
    mail.setSearch(text);
  }

  $effect(() => {
    const now = mail.search;
    if (now === sent) return;
    sent = now;
    query = now;
    if (searchTimer) clearTimeout(searchTimer);
  });

  function onSearchInput() {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => search(query.trim()), 140);
  }

  let aiBusy = $state(false);
  async function aiSearch() {
    aiBusy = true;
    try {
      const q = await ai.run({ kind: 'searchQuery', question: query }).promise;
      if (q.trim()) {
        query = q.trim();
        search(query);
      }
    } catch {
      /* keep the plain query */
    } finally {
      aiBusy = false;
    }
  }

  function clearSearch() {
    query = '';
    search('');
    building = false;
    searchInput?.blur();
  }

  /** A query from the panel below the box: searched at once (and kept among the recent ones), or put in the box to go on with. */
  function pick(q: string, run: boolean) {
    query = q;
    if (run) {
      search(q.trim());
      rememberSearch(q);
      building = false;
      searchInput?.blur();
    } else {
      searchInput?.focus();
      onSearchInput();
    }
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      clearSearch();
    } else if (e.key === 'Enter') {
      search(query.trim());
      rememberSearch(query);
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      // Into the panel below the box when it shows something, else to the list.
      const first = searchArea?.querySelector<HTMLElement>('.panel button, .panel input');
      if (first) first.focus();
      else {
        searchInput?.blur();
        mail.move(0);
      }
    }
  }

  function open(e: MouseEvent, th: ThreadSummary) {
    // The list keeps the focus, and names the row it is on (aria-activedescendant).
    viewport?.focus({ preventScroll: true });
    if (e.shiftKey) mail.checkRange(th.id);
    else if (e.ctrlKey || e.metaKey) mail.toggleCheck(th.id);
    else {
      if (mail.checked.size) mail.checked.clear();
      // In Drafts a draft opens to be edited.
      compose.openRow(th);
    }
  }

  function context(e: MouseEvent, th: ThreadSummary) {
    e.preventDefault();
    if (!mail.checked.has(th.id) && mail.selectedId !== th.id) mail.select(th.id);
    menu = { x: e.clientX, y: e.clientY, thread: th };
  }

  /** The context menu of the selected conversation, opened from the keyboard (Shift+F10 or the Menu key). */
  export function openRowMenu() {
    const th = mail.items.find((x) => x.id === mail.selectedId);
    const row = viewport?.querySelector<HTMLElement>('.row.selected');
    if (!th || !row) return;
    const r = row.getBoundingClientRect();
    menu = { x: r.left + 48, y: r.top + r.height / 2, thread: th };
  }

  function menuIds(): number[] {
    if (!menu) return [];
    return mail.checked.has(menu.thread.id) ? [...mail.checked] : [menu.thread.id];
  }

  async function replyTo(mode: 'reply' | 'replyAll' | 'forward' | 'attachment') {
    const th = menu?.thread;
    menu = null;
    if (!th) return;
    mail.select(th.id);
    const d = await mail.load(th.id);
    // Replies go to the newest message that is not a draft.
    const last = [...d.messages].reverse().find((m) => !m.flags.draft) ?? d.messages[d.messages.length - 1];
    if (!last) return;
    if (mode === 'attachment') compose.forwardAsAttachment(last);
    else compose.reply(last.id, mode, mode !== 'forward', last.date);
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

  /** The divider between the list and the conversation below it. */
  function startResizeHeight(e: PointerEvent) {
    resizing = true;
    const startY = e.clientY;
    const startH = ui.settings.listHeight;
    // The conversation below keeps room to be read.
    const room = (section?.parentElement?.clientHeight ?? innerHeight) - 200;
    const move = (ev: PointerEvent) => {
      ui.settings.listHeight = Math.round(Math.min(Math.max(160, room), Math.max(160, startH + ev.clientY - startY)));
    };
    const up = () => {
      resizing = false;
      ui.set('listHeight', ui.settings.listHeight);
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

<section
  class="list"
  class:resizing
  class:single={ui.single}
  class:below={ui.paneBelow}
  bind:this={section}
  style:width={ui.single || ui.paneBelow ? null : `${ui.settings.listWidth}px`}
  style:height={ui.paneBelow ? `${ui.settings.listHeight}px` : null}
>
  <header data-tauri-drag-region>
    {#if ui.sidebarHidden}
      <button class="icon-btn" title={t('threadList.showSidebar')} onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
    {/if}
    <div class="title" data-tauri-drag-region>
      <h1 data-tauri-drag-region>{mail.mailboxTitle}</h1>
      {#if mail.search}
        <span class="sub">{t('threadList.resultCount', { count: mail.total })}</span>
      {:else if mail.mailboxSubtitle}
        <span class="sub">{mail.mailboxSubtitle}</span>
      {/if}
    </div>
    <div class="tools">
      {#if mail.search}
        <button class="icon-btn" title={t('search.save')} aria-label={t('search.save')} onclick={() => saveSearch()}><BookmarkPlus size={16} /></button>
      {:else if mail.splitActive && mail.category === 'newsletters'}
        <button class="icon-btn" title={t('subscriptions.manage')} aria-label={t('subscriptions.manage')} onclick={() => ui.open('subscriptions')}><Newspaper size={16} /></button>
      {/if}
      {#if ai.ready}
        <button class="icon-btn ask" title={t('threadList.askInbox')} onclick={() => { ui.askAi = ''; ui.open('ask'); }}><Sparkles size={16} /></button>
      {/if}
      <button class="icon-btn" class:active={filterActive} title={t('threadList.filter')} onclick={(e) => (filterAt = bulkRect(e))}><Filter size={16} /></button>
      <button class="icon-btn" title={t('view.menu')} aria-label={t('view.menu')} aria-haspopup="menu" aria-expanded={!!viewAt} onclick={(e) => (viewAt = bulkRect(e))}><LayoutPanel size={16} /></button>
      <button class="icon-btn" title={t('threadList.refresh')} onclick={() => api.syncNow(null)}><Refresh size={16} /></button>
    </div>
    <!-- The list is at the top right of the window: with the conversation below, or in its place. -->
    {#if ui.single || ui.paneBelow}<WindowControls />{/if}
  </header>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="search"
    class:focused={searchFocused}
    bind:this={searchArea}
    onfocusin={() => (searchWithin = true)}
    onfocusout={(e) => {
      if (!searchArea?.contains(e.relatedTarget as Node)) searchWithin = false;
    }}
    onkeydown={(e) => {
      if (e.key === 'Escape' && building) {
        e.stopPropagation();
        building = false;
        searchInput?.focus();
      }
    }}
  >
    <Search size={15} />
    <input
      bind:this={searchInput}
      bind:value={query}
      oninput={onSearchInput}
      onkeydown={onSearchKey}
      onfocus={() => (searchFocused = true)}
      onblur={() => (searchFocused = false)}
      placeholder={t('threadList.searchMail')}
      spellcheck="false"
      aria-label={t('threadList.searchMail')}
    />
    {#if query && ai.ready && /\s/.test(query.trim()) && !/\w+:/.test(query)}
      <button class="ai-search" title={t('threadList.aiSearchTitle')} onclick={aiSearch} disabled={aiBusy}><Sparkles size={13} />{aiBusy ? '…' : t('threadList.aiSearchButton')}</button>
    {/if}
    <button
      class="opts"
      class:on={building}
      aria-label={t('search.options')}
      aria-expanded={building}
      title={t('search.options')}
      onclick={() => {
        building = !building;
        if (!building) searchInput?.focus();
      }}><Sliders size={13} /></button>
    {#if query}
      <button class="clear" aria-label={t('threadList.clearSearch')} onclick={clearSearch}><X size={13} /></button>
    {:else}
      <span class="kbd">/</span>
    {/if}
    {#if searchWithin || building}<SearchPanel {query} {building} onpick={pick} />{/if}
  </div>

  <AccountAlerts />

  {#if mail.splitActive}
    <div class="tabs" role="tablist">
      {#each tabs as tab (tab.label)}
        <button role="tab" aria-selected={mail.category === tab.id} class:on={mail.category === tab.id} title={t(tab.label)} onclick={() => mail.setCategory(tab.id)}>
          <span class="tab-label">{t(tab.label)}</span>
          {#if tab.id && mail.categoryUnread[tab.id]}<span class="badge">{mail.categoryUnread[tab.id]}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  {#if mail.checked.size > 0}
    <div class="bulk">
      <button class="icon-btn sm" title={t('threadList.clearSelection')} onclick={() => mail.checked.clear()}><X size={15} /></button>
      <!-- Side by side when they fit, stacked when a translation is longer. -->
      <span class="sel-box">
        <span class="sel">{t('threadList.selectedCount', { count: mail.checked.size })}</span>
        {#if mail.moreToCheck}
          <!-- More conversations than the rows loaded: all of them, for the actions to take. -->
          {@const all = t(
            mail.byMessage ? (mail.search ? 'messages.selectAllFound' : 'messages.selectAllInFolder') : mail.search ? 'threadList.selectAllFound' : 'threadList.selectAllInFolder',
            { count: mail.total },
          )}
          <button class="link" title={all} onclick={() => mail.checkAll()}>{all}</button>
        {:else if mail.items.some((th) => th.id > 0 && !mail.checked.has(th.id))}
          <button class="link" title={t('threadList.selectAll')} onclick={() => mail.items.forEach((th) => th.id > 0 && mail.checked.add(th.id))}>{t('threadList.selectAll')}</button>
        {/if}
      </span>
      <span class="spacer"></span>
      <button class="icon-btn sm" title={t('threadList.archive')} onclick={() => mail.archive()}><Archive size={15} /></button>
      <button class="icon-btn sm" title={t('common.delete')} onclick={() => mail.trash()}><Trash size={15} /></button>
      <button class="icon-btn sm" title={t('threadList.markRead')} onclick={() => mail.setRead(mail.targets(), true)}><MailOpen size={15} /></button>
      <button class="icon-btn sm" title={t('threadList.star')} onclick={() => mail.toggleStar()}><Star size={15} /></button>
      <button class="icon-btn sm" title={t('threadList.snooze')} onclick={(e) => (snoozeAt = { ...bulkRect(e), ids: mail.targets() })}><Clock size={15} /></button>
      <button class="icon-btn sm" title={t('threadList.move')} onclick={(e) => (moveAt = { ...bulkRect(e), ids: mail.targets(), labels: false })}><Folder size={15} /></button>
    </div>
  {/if}

  <div
    class="viewport"
    bind:this={viewport}
    onscroll={() => (scrollTop = viewport?.scrollTop ?? 0)}
    role="listbox"
    aria-label={mail.byMessage ? t('messages.list') : t('threadList.conversations')}
    oncontextmenu={(e) => {
      // The Menu key with the focus on the list (not a right click on a row).
      if (e.target !== e.currentTarget) return;
      e.preventDefault();
      openRowMenu();
    }}
    aria-activedescendant={visible.some((th) => th.id === mail.selectedId) ? `thread-${mail.selectedId}` : undefined}
    tabindex="0"
  >
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
          {#each visible as th (th.id)}
            <ThreadRow
              thread={th}
              selected={mail.selectedId === th.id}
              checked={mail.checked.has(th.id)}
              onopen={(e) => open(e, th)}
              onwindow={() => {
                // In Drafts the click opened the draft to write on; Pop out moves it into a window.
                if (!(mail.inDrafts && th.draftCount > 0)) mail.openWindow(th);
              }}
              oncontext={(e) => context(e, th)}
              onsnooze={(e) => {
                const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                snoozeAt = { x: r.right, y: r.bottom + 4, ids: [th.id] };
              }}
            />
          {/each}
        </div>
      </div>
    {/if}
  </div>
  {#if ui.paneBelow}
    <div class="resizer across" role="separator" aria-orientation="horizontal" aria-label={t('view.resizeList')} onpointerdown={startResizeHeight}></div>
  {:else if !ui.single}
    <div class="resizer" role="separator" aria-orientation="vertical" aria-label={t('view.resizeList')} onpointerdown={startResize}></div>
  {/if}
  <!-- While resizing, the pointer stays over this window: a message's frame would take its events. -->
  {#if resizing}<div class="shield" class:across={ui.paneBelow}></div>{/if}
</section>

{#if menu && menu.thread.localDraftId != null}
  {@const row = menu.thread}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={232}>
    <button class="menu-item" onclick={() => { compose.openRow(row); menu = null; }}><Pen size={15} />{t('message.editDraft')}</button>
    <button class="menu-item" onclick={() => { mail.trash([row.id]); menu = null; }}><Trash size={15} />{t('composer.discardDraft')}<span class="hint">#</span></button>
  </Popover>
{:else if menu}
  {@const ids = menuIds()}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={232}>
    {#if ids.length === 1}
      <button class="menu-item" onclick={() => replyTo('reply')}><Reply size={15} />{t('threadList.reply')}<span class="hint">R</span></button>
      <button class="menu-item" onclick={() => replyTo('replyAll')}><ReplyAll size={15} />{t('threadList.replyAll')}<span class="hint">A</span></button>
      <button class="menu-item" onclick={() => replyTo('forward')}><Forward size={15} />{t('threadList.forward')}<span class="hint">F</span></button>
      <button class="menu-item" onclick={() => replyTo('attachment')}><FileArrow size={15} />{t('message.forwardAsAttachment')}</button>
      <button class="menu-item" onclick={() => { mail.openWindow(menu!.thread); menu = null; }}><PopOut size={15} />{t('view.openInWindow')}<span class="hint">{t('keys.shift')}+{t('keys.enter')}</span></button>
      <div class="divider"></div>
    {/if}
    {#if inSnoozed}
      <button class="menu-item" onclick={() => { mail.unsnooze(ids); menu = null; }}><Mail size={15} />{t('threadList.unsnooze')}</button>
    {/if}
    <button class="menu-item" onclick={() => { mail.archive(ids); menu = null; }}><Archive size={15} />{t('threadList.archive')}<span class="hint">E</span></button>
    <button class="menu-item" onclick={() => { mail.trash(ids); menu = null; }}><Trash size={15} />{inTrash ? t('threadList.deleteForever') : t('common.delete')}<span class="hint">#</span></button>
    <button class="menu-item" onclick={() => { mail.toggleRead(ids); menu = null; }}>
      {#if menu.thread.unreadCount > 0}<MailOpen size={15} />{t('threadList.markAsRead')}{:else}<Mail size={15} />{t('threadList.markAsUnread')}{/if}<span class="hint">U</span>
    </button>
    <button class="menu-item" onclick={() => { mail.toggleStar(ids); menu = null; }}><Star size={15} />{menu.thread.flagged ? t('threadList.unstar') : t('threadList.star')}<span class="hint">S</span></button>
    <button class="menu-item" onclick={() => { snoozeAt = { x: menu!.x, y: menu!.y, ids }; menu = null; }}><Clock size={15} />{t('threadList.snoozeMenu')}<span class="hint">H</span></button>
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { moveAt = { x: menu!.x, y: menu!.y, ids, labels: false }; menu = null; }}><Folder size={15} />{t('threadList.moveTo')}<span class="hint">V</span></button>
    {#if ['gmail', 'microsoft', 'exchange'].includes(mail.accountById.get(menu.thread.accountId)?.kind ?? '')}
      <button class="menu-item" onclick={() => { moveAt = { x: menu!.x, y: menu!.y, ids, labels: true }; menu = null; }}><Tag size={15} />{mail.accountById.get(menu.thread.accountId)?.kind === 'gmail' ? t('threadList.labelMenu') : t('threadList.categorizeMenu')}<span class="hint">L</span></button>
    {/if}
    {#if mail.isSpam(ids)}
      <button class="menu-item" onclick={() => { mail.notSpam(ids); menu = null; }}><ShieldCheck size={15} />{t('threadList.notSpam')}<span class="hint">!</span></button>
    {:else}
      <button class="menu-item" onclick={() => { mail.moveTo({ kind: 'junk' }, ids); menu = null; }}><ShieldAlert size={15} />{t('threadList.reportSpam')}<span class="hint">!</span></button>
    {/if}
    {@const sender = ids.length === 1 ? senderOf(menu.thread) : null}
    {#if sender}
      {@const vip = vips.has(sender.email)}
      <button class="menu-item" onclick={() => { vips.set(sender, !vip); menu = null; }}><Crown size={15} /><span class="truncate">{t(vip ? 'vip.removeNamed' : 'vip.addNamed', { name: displayName(sender) })}</span></button>
      <button class="menu-item" onclick={() => { blockSender(sender); menu = null; }}><Ban size={15} /><span class="truncate">{t('threadList.blockSender', { name: displayName(sender) })}</span></button>
    {/if}
    {#if mail.inFollowUp}
      <div class="divider"></div>
      <button class="menu-item" onclick={() => { mail.followUpDone(ids); menu = null; }}><Check size={15} />{t('threadList.followUpDone')}</button>
      <button class="menu-item" onclick={() => { remindAt = { x: menu!.x, y: menu!.y, ids }; menu = null; }}><AlarmClock size={15} />{t('threadList.followUpLater')}</button>
    {/if}
  </Popover>
{/if}

{#if remindAt}
  {@const ids = remindAt.ids}
  <SnoozeMenu x={remindAt.x} y={remindAt.y} title={t('reader.followUpRemindTitle')} onclose={() => (remindAt = null)} onpick={(ts) => { mail.followUpLater(ts, ids); remindAt = null; }} />
{/if}

{#if snoozeAt}
  {@const ids = snoozeAt.ids}
  <SnoozeMenu x={snoozeAt.x} y={snoozeAt.y} align="end" onclose={() => (snoozeAt = null)} onpick={(ts) => { mail.snooze(ts, ids); snoozeAt = null; }} />
{/if}

{#if moveAt}
  <MoveMenu x={moveAt.x} y={moveAt.y} ids={moveAt.ids} labels={moveAt.labels} onclose={() => (moveAt = null)} />
{/if}

{#if filterAt}
  <Popover x={filterAt.x} y={filterAt.y} onclose={() => (filterAt = null)} width={220}>
    <div class="menu-label">{t('threadList.showOnly')}</div>
    {#each filters as f (f.k)}
      {@const on = !!mail.filter[f.k]}
      <button class="menu-item" onclick={() => mail.setFilter({ ...mail.filter, [f.k]: !on })}>
        <f.icon size={15} />{t(f.label)}
        {#if on}<span class="hint"><Check size={14} /></span>{/if}
      </button>
    {/each}
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { mail.markAllRead(); filterAt = null; }}><CheckCircle size={15} />{t('threadList.markAllRead')}</button>
    <button class="menu-item" onclick={() => { ui.set('density', ui.settings.density === 'compact' ? 'comfortable' : 'compact'); filterAt = null; }}>
      <More size={15} />{ui.settings.density === 'compact' ? t('threadList.comfortableRows') : t('threadList.compactRows')}
    </button>
  </Popover>
{/if}

{#if viewAt}
  <Popover x={viewAt.x} y={viewAt.y} onclose={() => (viewAt = null)} width={248}>
    <div class="menu-label">{t('view.readingPane')}</div>
    {#each panes as p (p.id)}
      {@const on = ui.settings.readingPane === p.id}
      <button class="menu-item" role="menuitemradio" aria-checked={on} onclick={() => { ui.set('readingPane', p.id); viewAt = null; }}>
        <p.icon size={15} />{t(p.label)}
        {#if on}<span class="hint"><Check size={14} /></span>{/if}
      </button>
    {/each}
    <div class="divider"></div>
    <button class="menu-item" role="menuitemcheckbox" aria-checked={!mail.byMessage} onclick={() => { mail.setGrouped(mail.byMessage); viewAt = null; }}>
      <Mails size={15} />{t('view.groupConversations')}
      {#if !mail.byMessage}<span class="hint"><Check size={14} /></span>{/if}
    </button>
    <div class="divider"></div>
    <div class="menu-label">{t('view.sort')}</div>
    {#each sorts as o (o.id)}
      {@const on = mail.sort === o.id}
      <button class="menu-item" role="menuitemradio" aria-checked={on} onclick={() => { mail.setSort(o.id); viewAt = null; }}>
        <o.icon size={15} />{t(o.label)}
        {#if on}<span class="hint"><Check size={14} /></span>{/if}
      </button>
    {/each}
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
  /* However wide it was made, the conversation beside it keeps room to be read. */
  .list:not(.single) {
    max-width: max(300px, calc(100% - 440px));
  }
  /* The list alone in a narrow window. */
  .list.single {
    flex: 1;
    border-right: none;
  }
  /* Above the conversation: the width of the window, and however tall it was made. */
  .list.below {
    flex-shrink: 0;
    max-width: none;
    min-height: 160px;
    max-height: calc(100% - 200px);
    border-right: none;
    border-bottom: 1px solid var(--border);
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
    min-width: 0;
    font-size: 17px;
    font-weight: 650;
    letter-spacing: -0.015em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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
    position: relative;
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
  .ai-search {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 7px;
    border-radius: 7px;
    font-size: 11px;
    font-weight: 700;
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .ask {
    color: var(--accent-text);
  }
  .opts {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    color: var(--text-3);
  }
  .opts:hover,
  .opts.on {
    background: var(--bg-hover);
    color: var(--text);
  }
  .opts.on {
    color: var(--accent-text);
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
  }
  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 8px;
    /* Long translations shorten with an ellipsis (the full name is the tooltip) instead of wrapping. */
    min-width: 0;
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
  .tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .badge {
    flex-shrink: 0;
    font-size: 10.5px;
    font-weight: 700;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    background: var(--accent-solid);
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
  .sel-box {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    column-gap: 8px;
    min-width: 0;
    margin-left: 2px;
    line-height: 1.25;
  }
  .sel {
    max-width: 100%;
    font-weight: 600;
    color: var(--accent-text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .link {
    max-width: 100%;
    color: var(--accent-text);
    font-size: var(--text-sm);
    text-decoration: underline;
    text-underline-offset: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  /* The keyboard is on the list: the row it is on shows it. */
  .viewport:focus-visible :global(.row.selected) {
    box-shadow: inset 0 0 0 2px var(--accent);
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
  .shield {
    position: fixed;
    inset: 0;
    z-index: 3000;
    cursor: col-resize;
  }
  .shield.across {
    cursor: row-resize;
  }
  .resizer.across {
    top: auto;
    left: 0;
    right: 0;
    bottom: -3px;
    width: 100%;
    height: 6px;
    cursor: row-resize;
  }
  .resizer.across:hover,
  .resizing .resizer.across {
    background: linear-gradient(to bottom, transparent 2px, var(--accent) 2px, var(--accent) 4px, transparent 4px);
  }
</style>

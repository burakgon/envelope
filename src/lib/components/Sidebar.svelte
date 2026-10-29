<script lang="ts">
  import type { Account, Folder, FolderRole, Mailbox } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail, roleLabel, roleOrder } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { toasts } from '../stores/toasts.svelte';
  import {
    AlertCircle, Archive, Calendar, ChevronDown, Clock, Compose, File, Folder as FolderIcon, Inbox, Mail,
    Mails, PanelLeft, Plus, Send, Settings, ShieldAlert, Spinner, Star, Tag, Trash, WifiOff, Flag, BookUser, UserPlus, Users, BellRing, ListChecks,
    Bookmark, Pen, Crown, UsersRound,
  } from '../icons';
  import { vips } from '../stores/vips.svelte';
  import { updates } from '../stores/updates.svelte';
  import Popover from './Popover.svelte';
  import { deleteSearch, renameSearch, runSearch } from '../stores/searches.svelte';
  import type { SavedSearch } from '../stores/ui.svelte';
  import Logo from './Logo.svelte';
  import OutboxMenu from './OutboxMenu.svelte';
  import UpdateBanner from './UpdateBanner.svelte';
  import FolderMenu from './FolderMenu.svelte';
  import { canNest, isCategory, isFixed, moveTargetOf, nestFolder, newFolder } from '../stores/folders.svelte';
  import DemoBanner from './DemoBanner.svelte';
  import AutoReplyBanner from './AutoReplyBanner.svelte';
  import { api } from '../api';
  import CalendarSidebar from './calendar/CalendarSidebar.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { people, bookName, groupsEditable, newGroupTitle } from '../stores/contacts.svelte';
  import { tasks } from '../stores/tasks.svelte';
  import TasksSidebar from './tasks/TasksSidebar.svelte';
  import { intlLocale, t } from '../i18n';
  import { modal } from '../actions/modal';
  import type { View } from '../stores/ui.svelte';

  /** A narrow window: a rail of icons, with the whole sidebar opening over the window from it. */
  let { rail = false }: { rail?: boolean } = $props();

  const roleIcon: Record<FolderRole, typeof Inbox> = {
    inbox: Inbox, sent: Send, drafts: File, trash: Trash, junk: ShieldAlert, archive: Archive, all: Mails, starred: Star, important: Flag,
  };

  let collapsedAccounts = $state<Record<number, boolean>>({});
  let outboxAt = $state<{ x: number; y: number } | null>(null);
  const pending = $derived(mail.outbox.filter((i) => i.state !== 'sent'));
  let dragOver = $state<number | null>(null);
  /** The folder being dragged onto another to nest it. */
  let draggedFolder = $state<Folder | null>(null);
  /** The account heading a folder is dragged over (to the top of the account). */
  let dragOverAccount = $state<number | null>(null);
  let folderMenu = $state<{ x: number; y: number; account: Account; folder: Folder | null } | null>(null);
  let searchMenu = $state<{ x: number; y: number; search: SavedSearch } | null>(null);
  let resizing = $state(false);

  const unified = $derived.by(() => {
    const sum = (role: FolderRole, unreadOnly = true) =>
      mail.folders.filter((f) => f.role === role).reduce((n, f) => n + (unreadOnly ? f.unreadCount : f.totalCount), 0);
    return { inbox: sum('inbox'), drafts: sum('drafts', false), junk: sum('junk') };
  });

  function accountFolders(a: Account): { folder: Folder; depth: number }[] {
    const list = mail.foldersOf(a.id).filter((f) => f.selectable || f.kind === 'label' || f.role);
    const byParent = new Map<number | null, Folder[]>();
    for (const f of list) {
      const key = f.parentId && list.some((x) => x.id === f.parentId) ? f.parentId : null;
      byParent.set(key, [...(byParent.get(key) ?? []), f]);
    }
    const sortFn = (x: Folder, y: Folder) => {
      const rx = x.role ? roleOrder.indexOf(x.role) : 99;
      const ry = y.role ? roleOrder.indexOf(y.role) : 99;
      if (rx !== ry) return rx - ry;
      if (x.kind !== y.kind) return x.kind === 'folder' ? -1 : 1;
      return x.name.localeCompare(y.name, intlLocale());
    };
    const out: { folder: Folder; depth: number }[] = [];
    const walk = (parent: number | null, depth: number) => {
      for (const f of (byParent.get(parent) ?? []).sort(sortFn)) {
        // Gmail's starred/important and All Mail are reachable from smart mailboxes.
        if (a.kind === 'gmail' && (f.role === 'starred' || f.role === 'important')) continue;
        out.push({ folder: f, depth });
        walk(f.id, depth + 1);
      }
    };
    walk(null, 0);
    return out;
  }

  const single = $derived(mail.accounts.length === 1 ? mail.accounts[0] : null);
  const singleRoles = $derived.by(() => {
    if (!single) return [];
    const roles: FolderRole[] = single.kind === 'gmail' ? ['all', 'junk', 'trash'] : ['archive', 'junk', 'trash'];
    return roles.map((r) => mail.roleFolder(single.id, r)).filter((f): f is Folder => !!f);
  });

  function go(mb: Mailbox) {
    mail.setMailbox(mb);
  }

  function onDragOver(e: DragEvent, f: Folder) {
    const types = e.dataTransfer?.types ?? [];
    const folder = types.includes('application/x-envelope-folder');
    if (!types.includes('application/x-envelope-threads') && !(folder && draggedFolder && canNest(draggedFolder, f))) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = 'move';
    dragOver = f.id;
  }

  function onDrop(e: DragEvent, f: Folder) {
    dragOver = null;
    if (draggedFolder) {
      e.preventDefault();
      const dragged = draggedFolder;
      draggedFolder = null;
      nestFolder(dragged, f);
      return;
    }
    const raw = e.dataTransfer?.getData('application/x-envelope-threads');
    if (!raw) return;
    e.preventDefault();
    const ids: number[] = JSON.parse(raw);
    if (f.kind === 'label') mail.toggleLabel(f.id, ids);
    else mail.moveTo(moveTargetOf(f), ids, f.role ? roleLabel(f.role) : f.name);
  }

  function startFolderDrag(e: DragEvent, f: Folder) {
    if (!e.dataTransfer) return;
    draggedFolder = f;
    e.dataTransfer.setData('application/x-envelope-folder', String(f.id));
    e.dataTransfer.setData('text/plain', f.name);
    e.dataTransfer.effectAllowed = 'move';
  }

  /** A folder dragged onto its account's heading goes to the top of the account. */
  function onAccountDragOver(e: DragEvent, a: Account) {
    if (!draggedFolder || draggedFolder.accountId !== a.id || !canNest(draggedFolder, null)) return;
    e.preventDefault();
    e.dataTransfer!.dropEffect = 'move';
    dragOverAccount = a.id;
  }

  function onAccountDrop(e: DragEvent) {
    dragOverAccount = null;
    if (!draggedFolder) return;
    e.preventDefault();
    const dragged = draggedFolder;
    draggedFolder = null;
    nestFolder(dragged, null);
  }

  function openFolderMenu(e: MouseEvent | KeyboardEvent, account: Account, folder: Folder | null) {
    if (e instanceof KeyboardEvent && !(e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10'))) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const at = e instanceof MouseEvent && e.clientX ? { x: e.clientX, y: e.clientY } : { x: r.left + 24, y: r.bottom + 2 };
    folderMenu = { ...at, account, folder };
  }

  /** A saved search's menu: right-click, Shift+F10 or the Menu key. */
  function openSearchMenu(e: MouseEvent | KeyboardEvent, search: SavedSearch) {
    if (e instanceof KeyboardEvent && !(e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10'))) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const at = e instanceof MouseEvent && e.clientX ? { x: e.clientX, y: e.clientY } : { x: r.left + 24, y: r.bottom + 2 };
    searchMenu = { ...at, search };
  }

  /** Closes the saved search's menu, then acts on the search. */
  function fromSearchMenu(act: (search: SavedSearch) => void) {
    const search = searchMenu?.search;
    searchMenu = null;
    if (search) act(search);
  }

  function startResize(e: PointerEvent) {
    resizing = true;
    const startX = e.clientX;
    const start = ui.settings.sidebarWidth;
    const move = (ev: PointerEvent) => {
      ui.settings.sidebarWidth = Math.min(360, Math.max(190, start + ev.clientX - startX));
    };
    const up = () => {
      resizing = false;
      ui.set('sidebarWidth', ui.settings.sidebarWidth);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function syncLabel(a: Account): string {
    switch (a.sync.state) {
      case 'connecting':
        return t('sidebar.syncConnecting');
      case 'syncing':
        return a.sync.progress != null ? t('sidebar.syncPercent', { percent: Math.round(a.sync.progress * 100) }) : t('sidebar.syncRunning');
      case 'offline':
        return t('sidebar.syncOffline');
      case 'authError':
        return t('sidebar.syncSignInNeeded');
      case 'error':
        return a.sync.message;
      default:
        return t('sidebar.syncUpToDate');
    }
  }

  // Mail and Calendar keep their names while they fit beside the icon-only tabs; else every tab is an icon.
  let modes: HTMLDivElement | undefined = $state();
  let iconModes = $state(false);
  $effect(() => {
    const el = modes;
    if (!el) return;
    const fit = () => {
      const s = getComputedStyle(el);
      const room = el.clientWidth - parseFloat(s.paddingLeft) - parseFloat(s.paddingRight);
      const labels = [...el.querySelectorAll<HTMLElement>('.mode-label')].reduce((n, l) => n + l.scrollWidth, 0);
      // Beside the labels: two icons with their gaps and a little air, two 32px tabs, three 2px gaps.
      iconModes = labels + 2 * (15 + 6 + 4) + 2 * 32 + 3 * 2 > room;
    };
    const ro = new ResizeObserver(fit);
    ro.observe(el);
    el.querySelectorAll('.mode-label').forEach((l) => ro.observe(l));
    return () => ro.disconnect();
  });

  function accountAlert(a: Account) {
    ui.open('settings', 'accounts');
    if (a.sync.state === 'authError') toasts.show(t('sidebar.signInAgain', { email: a.email }));
  }

  // The rail: what the top of the sidebar offers, as icons.
  const primary = $derived(
    ui.view === 'tasks'
      ? { label: t('tasks.sidebar.newTask'), icon: Plus, run: () => tasks.newTask() }
      : ui.view === 'contacts'
        ? { label: t('sidebar.newContact'), icon: UserPlus, run: () => people.newContact() }
        : ui.view === 'calendar'
          ? { label: t('sidebar.newEvent'), icon: Plus, run: () => cal.newEvent() }
          : { label: t('sidebar.newMessage'), icon: Compose, run: () => compose.new() },
  );
  const views = $derived<{ view: View; label: string; icon: typeof Mail }[]>([
    { view: 'mail', label: t('sidebar.modeMail'), icon: Mail },
    { view: 'calendar', label: t('sidebar.modeCalendar'), icon: Calendar },
    { view: 'contacts', label: t('sidebar.modeContacts'), icon: BookUser },
    { view: 'tasks', label: t('tasks.sidebar.mode'), icon: ListChecks },
  ]);
  const boxes = $derived<{ box: Mailbox; label: string; icon: typeof Mail; count?: number }[]>([
    { box: { kind: 'unified', role: 'inbox' }, label: mail.multiAccount ? t('mailbox.allInboxes') : t('mailbox.inbox'), icon: Inbox, count: unified.inbox },
    { box: { kind: 'starred' }, label: t('mailbox.starred'), icon: Star },
    ...(vips.list.length ? [{ box: { kind: 'vip' } as Mailbox, label: t('mailbox.vip'), icon: Crown }] : []),
    { box: { kind: 'snoozed' }, label: t('mailbox.snoozed'), icon: Clock },
    ...(mail.followUpCount > 0 ? [{ box: { kind: 'followUp' } as Mailbox, label: t('mailbox.followUp'), icon: BellRing, count: mail.followUpCount }] : []),
    { box: { kind: 'unified', role: 'drafts' }, label: t('mailbox.drafts'), icon: File },
    { box: { kind: 'unified', role: 'sent' }, label: t('mailbox.sent'), icon: Send },
  ]);

  // The whole sidebar over the window closes with the rail, as the window grows wide again.
  $effect(() => () => {
    if (rail) ui.railOpen = false;
  });

  /** Going somewhere from the sidebar over the window (a folder, a new message, Settings) closes it. */
  function leaveDrawer(e: MouseEvent) {
    if ((e.target as HTMLElement).closest('button.item, button.compose')) ui.railOpen = false;
  }
</script>

{#if rail}
  <nav class="rail">
    <div class="rail-head" data-tauri-drag-region>
      <button
        class="icon-btn expand"
        title={updates.notice ? `${t('threadList.showSidebar')} — ${t('updates.available', { version: updates.notice.version })}` : t('threadList.showSidebar')}
        aria-label={updates.notice ? `${t('threadList.showSidebar')} — ${t('updates.available', { version: updates.notice.version })}` : t('threadList.showSidebar')}
        aria-expanded={ui.railOpen}
        onclick={() => (ui.railOpen = true)}
      >
        <PanelLeft size={17} />
        <!-- A newer version is out: its notice is in the full sidebar. -->
        {#if updates.notice}<span class="dot"></span>{/if}
      </button>
    </div>
    <button class="rail-compose" title={primary.label} aria-label={primary.label} onclick={primary.run}><primary.icon size={17} strokeWidth={2.3} /></button>
    <div class="rail-group" role="tablist" aria-orientation="vertical">
      {#each views as v (v.view)}
        <button class="rail-item" class:on={ui.view === v.view} role="tab" aria-selected={ui.view === v.view} title={v.label} aria-label={v.label} onclick={() => (ui.view = v.view)}>
          <v.icon size={17} />
        </button>
      {/each}
    </div>
    {#if ui.view === 'mail'}
      <div class="rail-group">
        {#each boxes as b (b.label)}
          {@const on = mail.isMailbox(b.box)}
          <button class="rail-item" class:on title={b.label} aria-label={b.count ? `${b.label} (${b.count})` : b.label} aria-current={on ? 'page' : undefined} onclick={() => go(b.box)}>
            <b.icon size={17} />
            {#if b.count}<span class="badge">{b.count > 99 ? '99+' : b.count}</span>{/if}
          </button>
        {/each}
      </div>
      {#if ui.settings.savedSearches.length}
        <div class="rail-group" role="group" aria-label={t('search.savedTitle')}>
          {#each ui.settings.savedSearches as saved (saved.id)}
            {@const on = mail.search === saved.query}
            <button
              class="rail-item"
              class:on
              title={`${saved.name} — ${saved.query}`}
              aria-label={saved.name}
              aria-current={on ? 'page' : undefined}
              onclick={() => runSearch(saved.query)}
              oncontextmenu={(e) => openSearchMenu(e, saved)}
              onkeydown={(e) => openSearchMenu(e, saved)}
            >
              <Bookmark size={17} />
            </button>
          {/each}
        </div>
      {/if}
    {/if}
    <span class="rail-grow" data-tauri-drag-region></span>
    <button class="rail-item" title={t('sidebar.settings')} aria-label={t('sidebar.settings')} onclick={() => ui.open('settings')}><Settings size={17} /></button>
  </nav>
  {#if ui.railOpen}
    <div class="scrim" role="presentation" onclick={() => (ui.railOpen = false)}></div>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="drawer"
      role="dialog"
      aria-label={t('sidebar.title')}
      tabindex="-1"
      onclick={leaveDrawer}
      onkeydown={(e) => {
        if (e.key !== 'Escape' || e.defaultPrevented) return;
        e.stopPropagation();
        ui.railOpen = false;
      }}
      use:modal
    >
      {@render panel()}
    </div>
  {/if}
{:else}
  {@render panel()}
{/if}

{#snippet panel()}
<aside class="sidebar" class:resizing style:width="{ui.settings.sidebarWidth}px">
  <header data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Logo size={22} />
      <span class="wordmark" data-tauri-drag-region>Envelope</span>
    </div>
    <button class="icon-btn sm" title={t('sidebar.hideSidebar')} onclick={() => (rail ? (ui.railOpen = false) : (ui.sidebarCollapsed = true))}><PanelLeft size={16} /></button>
  </header>

  <div class="top">
    {#if ui.view === 'tasks'}
      <button class="compose" title={t('tasks.sidebar.newTask')} onclick={() => tasks.newTask()}>
        <Plus size={16} strokeWidth={2.4} />
        <span>{t('tasks.sidebar.newTask')}</span>
        <span class="kbd-hint">C</span>
      </button>
    {:else if ui.view === 'contacts'}
      <button class="compose" title={t('sidebar.newContact')} onclick={() => people.newContact()}>
        <UserPlus size={16} strokeWidth={2.2} />
        <span>{t('sidebar.newContact')}</span>
        <span class="kbd-hint">C</span>
      </button>
    {:else if ui.view === 'calendar'}
      <button class="compose" title={t('sidebar.newEvent')} onclick={() => cal.newEvent()}>
        <Plus size={16} strokeWidth={2.4} />
        <span>{t('sidebar.newEvent')}</span>
        <span class="kbd-hint">C</span>
      </button>
    {:else}
      <button class="compose" title={t('sidebar.newMessage')} onclick={() => compose.new()}>
        <Compose size={16} strokeWidth={2.2} />
        <span>{t('sidebar.newMessage')}</span>
        <span class="kbd-hint">C</span>
      </button>
    {/if}
    <div class="modes" class:icons={iconModes} role="tablist" bind:this={modes}>
      <button role="tab" aria-selected={ui.view === 'mail'} class:on={ui.view === 'mail'} title={t('sidebar.modeMail')} aria-label={t('sidebar.modeMail')} onclick={(e) => { ui.view = 'mail'; e.currentTarget.blur(); }}>
        <Mail size={15} /><span class="mode-label" aria-hidden="true">{t('sidebar.modeMail')}</span>
      </button>
      <button role="tab" aria-selected={ui.view === 'calendar'} class:on={ui.view === 'calendar'} title={t('sidebar.modeCalendar')} aria-label={t('sidebar.modeCalendar')} onclick={(e) => { ui.view = 'calendar'; e.currentTarget.blur(); }}>
        <Calendar size={15} /><span class="mode-label" aria-hidden="true">{t('sidebar.modeCalendar')}</span>
      </button>
      <button role="tab" aria-selected={ui.view === 'contacts'} class:on={ui.view === 'contacts'} title={t('sidebar.modeContacts')} aria-label={t('sidebar.modeContacts')} onclick={(e) => { ui.view = 'contacts'; e.currentTarget.blur(); }}>
        <BookUser size={15} />
      </button>
      <button role="tab" aria-selected={ui.view === 'tasks'} class:on={ui.view === 'tasks'} title={t('tasks.sidebar.mode')} aria-label={t('tasks.sidebar.mode')} onclick={(e) => { ui.view = 'tasks'; e.currentTarget.blur(); }}>
        <ListChecks size={15} />
      </button>
    </div>
  </div>

  {#if ui.view === 'calendar'}
  <nav class="scroll">
    <CalendarSidebar />
  </nav>
  {:else if ui.view === 'tasks'}
  <nav class="scroll" aria-label={t('tasks.sidebar.mode')}>
    <TasksSidebar />
  </nav>
  {:else if ui.view === 'contacts'}
  <nav class="scroll">
    <div class="group">
      <button class="item" class:active={people.bookId == null && people.groupId == null} onclick={() => people.setBook(null)}>
        <Users size={16} /><span class="name">{t('sidebar.allContacts')}</span>
      </button>
      {#each people.books as b (b.id)}
        <div class="book">
          <button class="item" class:active={people.bookId === b.id && people.groupId == null} onclick={() => people.setBook(b.id)}>
            <BookUser size={16} /><span class="name truncate">{bookName(b)}</span>
          </button>
          {#if groupsEditable(b)}
            <button class="add-group" title={newGroupTitle(b)} aria-label={newGroupTitle(b)} onclick={() => people.newGroup(b.id)}><Plus size={14} /></button>
          {/if}
        </div>
        {#each people.groups.filter((g) => g.bookId === b.id) as g (g.id)}
          <button class="item in-book" class:active={people.groupId === g.id} onclick={() => people.setGroup(g.id)}>
            <UsersRound size={15} /><span class="name truncate">{g.name}</span><span class="count">{g.members.length}</span>
          </button>
        {/each}
      {/each}
    </div>
  </nav>
  {:else}
  <nav class="scroll">
    <div class="group">
      <button class="item" class:active={ui.view === 'mail' && mail.isMailbox({ kind: 'unified', role: 'inbox' })} onclick={() => go({ kind: 'unified', role: 'inbox' })}>
        <Inbox size={16} />
        <span class="name">{mail.multiAccount ? t('mailbox.allInboxes') : t('mailbox.inbox')}</span>
        {#if unified.inbox > 0}<span class="count unread">{unified.inbox}</span>{/if}
      </button>
      <button class="item" class:active={mail.isMailbox({ kind: 'starred' })} onclick={() => go({ kind: 'starred' })}>
        <Star size={16} /><span class="name">{t('mailbox.starred')}</span>
      </button>
      {#if vips.list.length || mail.isMailbox({ kind: 'vip' })}
        <button class="item" class:active={mail.isMailbox({ kind: 'vip' })} onclick={() => go({ kind: 'vip' })}>
          <Crown size={16} /><span class="name">{t('mailbox.vip')}</span>
        </button>
      {/if}
      <button class="item" class:active={mail.isMailbox({ kind: 'snoozed' })} onclick={() => go({ kind: 'snoozed' })}>
        <Clock size={16} /><span class="name">{t('mailbox.snoozed')}</span>
      </button>
      {#if mail.followUpCount > 0 || mail.isMailbox({ kind: 'followUp' })}
        <button class="item" class:active={mail.isMailbox({ kind: 'followUp' })} onclick={() => go({ kind: 'followUp' })}>
          <BellRing size={16} /><span class="name">{t('mailbox.followUp')}</span>
          {#if mail.followUpCount > 0}<span class="count unread">{mail.followUpCount}</span>{/if}
        </button>
      {/if}
      <button class="item" class:active={mail.isMailbox({ kind: 'unified', role: 'drafts' })} onclick={() => go({ kind: 'unified', role: 'drafts' })}>
        <File size={16} /><span class="name">{t('mailbox.drafts')}</span>
        {#if unified.drafts > 0}<span class="count">{unified.drafts}</span>{/if}
      </button>
      <button class="item" class:active={mail.isMailbox({ kind: 'unified', role: 'sent' })} onclick={() => go({ kind: 'unified', role: 'sent' })}>
        <Send size={16} /><span class="name">{t('mailbox.sent')}</span>
      </button>
      {#if pending.length}
        <button
          class="item"
          class:alert={pending.some((i) => i.state === 'failed')}
          onclick={(e) => {
            const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
            outboxAt = { x: r.right + 6, y: r.top };
          }}
        >
          {#if pending.some((i) => i.state === 'failed')}<AlertCircle size={16} />{:else}<Clock size={16} />{/if}
          <span class="name">{pending.every((i) => i.state === 'queued' && i.sendAt > Date.now() + 60_000) ? t('mailbox.scheduled') : t('mailbox.outbox')}</span>
          <span class="count">{pending.length}</span>
        </button>
      {/if}
      {#if single}
        {#each singleRoles as f (f.id)}
          {@const Icon = roleIcon[f.role!]}
          <button
            class="item"
            class:active={mail.isMailbox({ kind: 'folder', id: f.id })}
            class:drop={dragOver === f.id}
            onclick={() => go({ kind: 'folder', id: f.id })}
            oncontextmenu={(e) => openFolderMenu(e, single!, f)}
            onkeydown={(e) => openFolderMenu(e, single!, f)}
            ondragover={(e) => onDragOver(e, f)}
            ondragleave={() => (dragOver = null)}
            ondrop={(e) => onDrop(e, f)}
          >
            <Icon size={16} /><span class="name">{f.role ? roleLabel(f.role) : f.name}</span>
            {#if f.role === 'junk' && f.unreadCount > 0}<span class="count">{f.unreadCount}</span>{/if}
          </button>
        {/each}
      {:else}
        <button class="item" class:active={mail.isMailbox({ kind: 'everything' })} onclick={() => go({ kind: 'everything' })}>
          <Mails size={16} /><span class="name">{t('mailbox.all')}</span>
        </button>
      {/if}
    </div>

    {#if ui.settings.savedSearches.length}
      <div class="group" role="group" aria-labelledby="saved-searches">
        <div class="saved-title" id="saved-searches">{t('search.savedTitle')}</div>
        {#each ui.settings.savedSearches as saved (saved.id)}
          <button
            class="item"
            class:active={mail.search === saved.query}
            title={`${saved.name} — ${saved.query}`}
            onclick={() => runSearch(saved.query)}
            oncontextmenu={(e) => openSearchMenu(e, saved)}
            onkeydown={(e) => openSearchMenu(e, saved)}
          >
            <Bookmark size={16} /><span class="name">{saved.name}</span>
          </button>
        {/each}
      </div>
    {/if}

    {#each mail.accounts as account (account.id)}
      {@const open = !collapsedAccounts[account.id]}
      {@const list = accountFolders(account).filter((x) => !single || !x.folder.role)}
      {#if !single || list.length > 0}
      <div class="group account">
        <div
          class="account-head"
          class:drop={dragOverAccount === account.id}
          role="group"
          ondragover={(e) => onAccountDragOver(e, account)}
          ondragleave={() => (dragOverAccount = null)}
          ondrop={onAccountDrop}
        >
          <button
            class="account-title"
            onclick={() => (collapsedAccounts[account.id] = open)}
            oncontextmenu={(e) => openFolderMenu(e, account, null)}
            onkeydown={(e) => openFolderMenu(e, account, null)}
          >
            {#if single}
              <span class="label truncate">{t('sidebar.folders')}</span>
            {:else}
              <span class="dot" style:background={account.color}></span>
              <span class="label truncate" class:email={!account.label} title={account.email}>{account.label ?? account.email}</span>
            {/if}
            <span class="chev" class:closed={!open}><ChevronDown size={13} /></span>
          </button>
          <button
            class="add-folder"
            title={t(account.kind === 'gmail' ? 'folders.newLabel' : 'folders.newFolder')}
            aria-label={t(account.kind === 'gmail' ? 'folders.newLabel' : 'folders.newFolder')}
            onclick={() => newFolder(account)}
          ><Plus size={14} /></button>
          <span class="status" title={syncLabel(account)}>
            {#if account.sync.state === 'syncing' || account.sync.state === 'connecting'}
              <span class="spin"><Spinner size={13} /></span>
            {:else if account.sync.state === 'offline'}
              <WifiOff size={13} />
            {:else if account.sync.state === 'authError' || account.sync.state === 'error'}
              <button class="alert" onclick={() => accountAlert(account)}><AlertCircle size={14} /></button>
            {/if}
          </span>
        </div>
        {#if open}
          <div class="folders">
            {#each list as { folder, depth } (folder.id)}
              {@const Icon = folder.role ? roleIcon[folder.role] : folder.kind === 'label' ? Tag : FolderIcon}
              <button
                class="item"
                class:active={mail.isMailbox({ kind: 'folder', id: folder.id })}
                class:drop={dragOver === folder.id}
                class:dragging={draggedFolder?.id === folder.id}
                style:padding-left="{10 + depth * 14}px"
                draggable={!isFixed(folder) && !isCategory(folder)}
                onclick={() => go({ kind: 'folder', id: folder.id })}
                oncontextmenu={(e) => openFolderMenu(e, account, folder)}
                onkeydown={(e) => openFolderMenu(e, account, folder)}
                ondragstart={(e) => startFolderDrag(e, folder)}
                ondragend={() => (draggedFolder = null)}
                ondragover={(e) => onDragOver(e, folder)}
                ondragleave={() => (dragOver = null)}
                ondrop={(e) => onDrop(e, folder)}
              >
                {#if folder.kind === 'label' && folder.color}
                  <span class="label-dot" style:background={folder.color}></span>
                {:else}
                  <Icon size={16} />
                {/if}
                <span class="name truncate">{folder.role ? roleLabel(folder.role) : folder.name}</span>
                {#if folder.role === 'drafts' && folder.totalCount > 0}
                  <span class="count">{folder.totalCount}</span>
                {:else if folder.unreadCount > 0 && folder.role !== 'trash' && folder.role !== 'sent' && folder.role !== 'all'}
                  <span class="count" class:unread={folder.role === 'inbox'}>{folder.unreadCount}</span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      </div>
      {/if}
    {/each}

    <button class="item add" onclick={() => ui.open('addAccount')}>
      <Plus size={16} /><span class="name">{t('sidebar.addAccount')}</span>
    </button>
  </nav>
  {/if}

  <AutoReplyBanner />
  <UpdateBanner />
  {#if api.isDemo}<DemoBanner />{/if}
  <footer>
    <button class="item" onclick={() => ui.open('settings')}>
      <Settings size={16} /><span class="name">{t('sidebar.settings')}</span>
      <span class="kbd">,</span>
    </button>
  </footer>

  {#if !rail}<div class="resizer" role="separator" aria-orientation="vertical" onpointerdown={startResize}></div>{/if}
</aside>
{/snippet}

{#if outboxAt}
  <OutboxMenu x={outboxAt.x} y={outboxAt.y} onclose={() => (outboxAt = null)} />
{/if}

{#if folderMenu}
  <FolderMenu x={folderMenu.x} y={folderMenu.y} account={folderMenu.account} folder={folderMenu.folder} onclose={() => (folderMenu = null)} />
{/if}
{#if searchMenu}
  <Popover x={searchMenu.x} y={searchMenu.y} onclose={() => (searchMenu = null)} width={220}>
    <button class="menu-item" onclick={() => fromSearchMenu(renameSearch)}><Pen size={15} />{t('search.renameItem')}</button>
    <button class="menu-item danger" onclick={() => fromSearchMenu(deleteSearch)}><Trash size={15} />{t('search.deleteItem')}</button>
  </Popover>
{/if}

<style>
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    width: 56px;
    flex-shrink: 0;
    height: 100%;
    padding-bottom: 10px;
    /* A window too short for every icon scrolls them. */
    overflow-y: auto;
    scrollbar-width: none;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
  }
  .rail::-webkit-scrollbar {
    display: none;
  }
  .rail-head {
    display: grid;
    place-items: center;
    align-self: stretch;
    height: var(--header-h);
    flex-shrink: 0;
  }
  .rail-compose {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    flex-shrink: 0;
    border-radius: 11px;
    background: var(--accent-solid);
    color: var(--accent-fg);
    box-shadow: 0 1px 2px oklch(0.35 0.2 var(--accent-h) / 0.35), 0 4px 14px oklch(0.55 0.22 var(--accent-h) / 0.28), inset 0 1px 0 rgb(255 255 255 / 0.18);
    transition: background var(--dur-fast);
  }
  .rail-compose:hover {
    background: var(--accent-solid-hover);
  }
  .rail-group {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .rail-head + .rail-compose + .rail-group {
    margin-top: 6px;
  }
  .rail-item {
    position: relative;
    display: grid;
    place-items: center;
    flex-shrink: 0;
    width: 38px;
    height: 32px;
    border-radius: 9px;
    color: var(--text-2);
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .rail-item:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .rail-item.on {
    background: var(--bg-selected);
    color: var(--accent-text);
  }
  .expand {
    position: relative;
  }
  .expand .dot {
    position: absolute;
    top: 6px;
    right: 6px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 0 2px var(--bg-sidebar, var(--bg));
  }
  .rail-item .badge {
    position: absolute;
    top: 1px;
    right: 0;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 700;
    line-height: 16px;
    text-align: center;
    background: var(--accent-solid);
    color: var(--accent-fg);
    box-shadow: 0 0 0 2px var(--bg-sidebar);
  }
  .rail-grow {
    flex: 1;
    align-self: stretch;
  }
  /* The whole sidebar over the window, opened from the rail. */
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 900;
    background: var(--overlay);
    animation: fade-in var(--dur-fast);
  }
  .drawer {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    z-index: 901;
    display: flex;
    max-width: calc(100vw - 48px);
    box-shadow: var(--shadow-lg);
    animation: drawer-in var(--dur) var(--ease-out);
  }
  .drawer .sidebar {
    max-width: 100%;
  }
  @keyframes drawer-in {
    from {
      transform: translateX(-24px);
      opacity: 0;
    }
  }
  .sidebar {
    position: relative;
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
    height: 100%;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-h);
    padding: 0 10px 0 16px;
    flex-shrink: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .wordmark {
    font-size: 15px;
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  .top {
    padding: 2px 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .compose {
    display: flex;
    align-items: center;
    gap: 9px;
    height: 38px;
    padding: 0 12px 0 14px;
    border-radius: 11px;
    background: var(--accent-solid);
    color: var(--accent-fg);
    font-weight: 600;
    font-size: var(--text-md);
    box-shadow: 0 1px 2px oklch(0.35 0.2 var(--accent-h) / 0.35), 0 4px 14px oklch(0.55 0.22 var(--accent-h) / 0.28), inset 0 1px 0 rgb(255 255 255 / 0.18);
    transition: transform var(--dur-fast) var(--ease), background var(--dur-fast);
  }
  .compose:hover {
    background: var(--accent-solid-hover);
  }
  .compose:active {
    transform: scale(0.98);
  }
  .compose span:first-of-type {
    flex: 1;
    min-width: 0;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .compose :global(svg) {
    flex-shrink: 0;
  }
  .kbd-hint {
    flex-shrink: 0;
    font-size: 11px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 5px;
    background: rgb(255 255 255 / 0.18);
  }
  .modes {
    position: relative;
    display: grid;
    /* Equal halves when the labels fit; a longer label ("Calendrier", "カレンダー") takes what it needs. */
    grid-template-columns: minmax(min-content, 1fr) minmax(min-content, 1fr) 32px 32px;
    padding: 3px;
    gap: 2px;
    border-radius: 10px;
    background: var(--bg-active);
  }
  .modes.icons {
    grid-template-columns: repeat(4, 1fr);
  }
  /* Still measured while out of sight, to bring the names back when there is room. */
  .modes.icons .mode-label {
    position: absolute;
    visibility: hidden;
    pointer-events: none;
  }
  .modes button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 0;
    height: 28px;
    border-radius: 8px;
    font-weight: 500;
    color: var(--text-2);
    transition: background var(--dur), color var(--dur), box-shadow var(--dur);
  }
  .modes button :global(svg) {
    flex-shrink: 0;
  }
  .mode-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .modes button.on {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 2px 8px 12px;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-bottom: 10px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 31px;
    padding: 0 10px;
    border-radius: 8px;
    color: var(--text-2);
    font-weight: 500;
    text-align: left;
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .item:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .item.active {
    background: var(--bg-selected);
    color: var(--accent-text);
  }
  .item.active .count {
    color: var(--accent-text);
  }
  .item.alert,
  .item.alert .count {
    color: var(--red);
  }
  .item.drop,
  .account-head.drop {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .account-head.drop {
    border-radius: 8px;
  }
  .item.dragging {
    opacity: 0.5;
  }
  .item :global(svg) {
    flex-shrink: 0;
    opacity: 0.9;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
    font-weight: 500;
  }
  .count.unread {
    color: var(--text);
    font-weight: 650;
  }
  .label-dot {
    width: 9px;
    height: 9px;
    margin: 0 3.5px;
    border-radius: 3px;
    flex-shrink: 0;
  }
  .account {
    margin-top: 6px;
  }
  .account-head {
    display: flex;
    align-items: center;
    gap: 4px;
    padding-right: 6px;
  }
  .saved-title {
    height: 28px;
    display: flex;
    align-items: center;
    padding: 0 10px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-3);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .account-title {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 28px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-3);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .account-title:hover {
    color: var(--text-2);
  }
  .account-title .label {
    max-width: 100%;
  }
  .account-title .label.email {
    text-transform: none;
    letter-spacing: 0;
    font-size: var(--text-sm);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .chev {
    display: grid;
    transition: transform var(--dur) var(--ease);
    opacity: 0;
  }
  .account-title:hover .chev,
  .chev.closed {
    opacity: 1;
  }
  .chev.closed {
    transform: rotate(-90deg);
  }
  .status {
    display: grid;
    place-items: center;
    color: var(--text-3);
  }
  /* "New folder": shown with the heading's hover, or when reached with the keyboard. */
  .add-folder {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    color: var(--text-3);
    opacity: 0;
    transition: opacity var(--dur-fast), background var(--dur-fast);
  }
  .account-head:hover .add-folder,
  .add-folder:focus-visible {
    opacity: 1;
  }
  .add-folder:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  /* An address book, with "New group" shown on hover or when reached with the keyboard. */
  .book {
    position: relative;
  }
  .book .item {
    padding-right: 34px;
  }
  .add-group {
    position: absolute;
    top: 4.5px;
    right: 6px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    color: var(--text-3);
    opacity: 0;
    transition: opacity var(--dur-fast), background var(--dur-fast);
  }
  .book:hover .add-group,
  .add-group:focus-visible {
    opacity: 1;
  }
  .add-group:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  /* A group, under its address book. */
  .item.in-book {
    padding-left: 24px;
  }
  .alert {
    display: grid;
    color: var(--amber-text);
  }
  .spin {
    display: grid;
    animation: spin 0.9s linear infinite;
  }
  .folders {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .add {
    color: var(--text-3);
  }
  footer {
    padding: 8px;
    border-top: 1px solid var(--border);
  }
  footer .kbd {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 5px;
    color: var(--text-3);
    background: var(--bg-active);
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

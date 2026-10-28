<script lang="ts">
  import type { Account, Folder, FolderRole, Mailbox } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail, roleOrder } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { toasts } from '../stores/toasts.svelte';
  import {
    AlertCircle, Archive, Calendar, ChevronDown, Clock, Compose, File, Folder as FolderIcon, Inbox, Mail,
    Mails, PanelLeft, Plus, Send, Settings, ShieldAlert, Spinner, Star, Tag, Trash, WifiOff, Flag, BookUser, UserPlus, Users, BellRing,
  } from '../icons';
  import Logo from './Logo.svelte';
  import OutboxMenu from './OutboxMenu.svelte';
  import DemoBanner from './DemoBanner.svelte';
  import AutoReplyBanner from './AutoReplyBanner.svelte';
  import { api } from '../api';
  import CalendarSidebar from './calendar/CalendarSidebar.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { people } from '../stores/contacts.svelte';

  const roleIcon: Record<FolderRole, typeof Inbox> = {
    inbox: Inbox, sent: Send, drafts: File, trash: Trash, junk: ShieldAlert, archive: Archive, all: Mails, starred: Star, important: Flag,
  };

  let collapsedAccounts = $state<Record<number, boolean>>({});
  let outboxAt = $state<{ x: number; y: number } | null>(null);
  const pending = $derived(mail.outbox.filter((i) => i.state !== 'sent'));
  let dragOver = $state<number | null>(null);
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
      return x.name.localeCompare(y.name);
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

  function onDragOver(e: DragEvent, folderId: number) {
    if (!e.dataTransfer?.types.includes('application/x-envelope-threads')) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    dragOver = folderId;
  }

  function onDrop(e: DragEvent, f: Folder) {
    dragOver = null;
    const raw = e.dataTransfer?.getData('application/x-envelope-threads');
    if (!raw) return;
    e.preventDefault();
    const ids: number[] = JSON.parse(raw);
    const target =
      f.role === 'inbox' ? { kind: 'inbox' as const }
      : f.role === 'trash' ? { kind: 'trash' as const }
      : f.role === 'junk' ? { kind: 'junk' as const }
      : f.role === 'archive' || f.role === 'all' ? { kind: 'archive' as const }
      : { kind: 'folder' as const, id: f.id };
    if (f.kind === 'label') mail.toggleLabel(f.id, ids);
    else mail.moveTo(target, ids, `Moved to ${f.name}`);
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
        return 'Connecting…';
      case 'syncing':
        return a.sync.progress != null ? `Syncing ${Math.round(a.sync.progress * 100)}%` : 'Syncing…';
      case 'offline':
        return 'Offline — will retry';
      case 'authError':
        return 'Sign-in needed';
      case 'error':
        return a.sync.message;
      default:
        return 'Up to date';
    }
  }

  function accountAlert(a: Account) {
    if (a.sync.state === 'authError') {
      ui.open('settings', 'accounts');
      toasts.show(`Sign in to ${a.email} again to keep syncing.`);
    }
  }
</script>

<aside class="sidebar" class:resizing style:width="{ui.settings.sidebarWidth}px">
  <header data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Logo size={22} />
      <span class="wordmark" data-tauri-drag-region>Envelope</span>
    </div>
    <button class="icon-btn sm" title="Hide sidebar" onclick={() => (ui.sidebarCollapsed = true)}><PanelLeft size={16} /></button>
  </header>

  <div class="top">
    {#if ui.view === 'contacts'}
      <button class="compose" onclick={() => people.newContact()}>
        <UserPlus size={16} strokeWidth={2.2} />
        <span>New contact</span>
        <span class="kbd-hint">C</span>
      </button>
    {:else if ui.view === 'calendar'}
      <button class="compose" onclick={() => cal.newEvent()}>
        <Plus size={16} strokeWidth={2.4} />
        <span>New event</span>
        <span class="kbd-hint">C</span>
      </button>
    {:else}
      <button class="compose" onclick={() => compose.new()}>
        <Compose size={16} strokeWidth={2.2} />
        <span>New message</span>
        <span class="kbd-hint">C</span>
      </button>
    {/if}
    <div class="modes" role="tablist">
      <button role="tab" aria-selected={ui.view === 'mail'} class:on={ui.view === 'mail'} onclick={(e) => { ui.view = 'mail'; e.currentTarget.blur(); }}>
        <Mail size={15} /> Mail
      </button>
      <button role="tab" aria-selected={ui.view === 'calendar'} class:on={ui.view === 'calendar'} onclick={(e) => { ui.view = 'calendar'; e.currentTarget.blur(); }}>
        <Calendar size={15} /> Calendar
      </button>
      <button role="tab" aria-selected={ui.view === 'contacts'} class:on={ui.view === 'contacts'} title="Contacts" aria-label="Contacts" onclick={(e) => { ui.view = 'contacts'; e.currentTarget.blur(); }}>
        <BookUser size={15} />
      </button>
    </div>
  </div>

  {#if ui.view === 'calendar'}
  <nav class="scroll">
    <CalendarSidebar />
  </nav>
  {:else if ui.view === 'contacts'}
  <nav class="scroll">
    <div class="group">
      <button class="item" class:active={people.bookId == null} onclick={() => people.setBook(null)}>
        <Users size={16} /><span class="name">All contacts</span>
      </button>
      {#each people.books as b (b.id)}
        <button class="item" class:active={people.bookId === b.id} onclick={() => people.setBook(b.id)}>
          <BookUser size={16} /><span class="name truncate">{b.name}</span>
        </button>
      {/each}
    </div>
  </nav>
  {:else}
  <nav class="scroll">
    <div class="group">
      <button class="item" class:active={ui.view === 'mail' && mail.isMailbox({ kind: 'unified', role: 'inbox' })} onclick={() => go({ kind: 'unified', role: 'inbox' })}>
        <Inbox size={16} />
        <span class="name">{mail.multiAccount ? 'All Inboxes' : 'Inbox'}</span>
        {#if unified.inbox > 0}<span class="count unread">{unified.inbox}</span>{/if}
      </button>
      <button class="item" class:active={mail.isMailbox({ kind: 'starred' })} onclick={() => go({ kind: 'starred' })}>
        <Star size={16} /><span class="name">Starred</span>
      </button>
      <button class="item" class:active={mail.isMailbox({ kind: 'snoozed' })} onclick={() => go({ kind: 'snoozed' })}>
        <Clock size={16} /><span class="name">Snoozed</span>
      </button>
      {#if mail.followUpCount > 0 || mail.isMailbox({ kind: 'followUp' })}
        <button class="item" class:active={mail.isMailbox({ kind: 'followUp' })} onclick={() => go({ kind: 'followUp' })}>
          <BellRing size={16} /><span class="name">Follow up</span>
          {#if mail.followUpCount > 0}<span class="count unread">{mail.followUpCount}</span>{/if}
        </button>
      {/if}
      <button class="item" class:active={mail.isMailbox({ kind: 'unified', role: 'drafts' })} onclick={() => go({ kind: 'unified', role: 'drafts' })}>
        <File size={16} /><span class="name">Drafts</span>
        {#if unified.drafts > 0}<span class="count">{unified.drafts}</span>{/if}
      </button>
      <button class="item" class:active={mail.isMailbox({ kind: 'unified', role: 'sent' })} onclick={() => go({ kind: 'unified', role: 'sent' })}>
        <Send size={16} /><span class="name">Sent</span>
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
          <span class="name">{pending.every((i) => i.state === 'queued' && i.sendAt > Date.now() + 60_000) ? 'Scheduled' : 'Outbox'}</span>
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
            ondragover={(e) => onDragOver(e, f.id)}
            ondragleave={() => (dragOver = null)}
            ondrop={(e) => onDrop(e, f)}
          >
            <Icon size={16} /><span class="name">{f.role === 'junk' ? 'Spam' : f.role === 'all' ? 'All Mail' : f.name}</span>
            {#if f.role === 'junk' && f.unreadCount > 0}<span class="count">{f.unreadCount}</span>{/if}
          </button>
        {/each}
      {:else}
        <button class="item" class:active={mail.isMailbox({ kind: 'everything' })} onclick={() => go({ kind: 'everything' })}>
          <Mails size={16} /><span class="name">All Mail</span>
        </button>
      {/if}
    </div>

    {#each mail.accounts as account (account.id)}
      {@const open = !collapsedAccounts[account.id]}
      {@const list = accountFolders(account).filter((x) => !single || !x.folder.role)}
      {#if !single || list.length > 0}
      <div class="group account">
        <div class="account-head">
          <button class="account-title" onclick={() => (collapsedAccounts[account.id] = open)}>
            {#if single}
              <span class="label truncate">Folders</span>
            {:else}
              <span class="dot" style:background={account.color}></span>
              <span class="label truncate" class:email={!account.label} title={account.email}>{account.label ?? account.email}</span>
            {/if}
            <span class="chev" class:closed={!open}><ChevronDown size={13} /></span>
          </button>
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
                style:padding-left="{10 + depth * 14}px"
                onclick={() => go({ kind: 'folder', id: folder.id })}
                ondragover={(e) => onDragOver(e, folder.id)}
                ondragleave={() => (dragOver = null)}
                ondrop={(e) => onDrop(e, folder)}
              >
                {#if folder.kind === 'label' && folder.color}
                  <span class="label-dot" style:background={folder.color}></span>
                {:else}
                  <Icon size={16} />
                {/if}
                <span class="name truncate">{folder.role === 'junk' ? 'Spam' : folder.name}</span>
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
      <Plus size={16} /><span class="name">Add account</span>
    </button>
  </nav>
  {/if}

  <AutoReplyBanner />
  {#if api.isDemo}<DemoBanner />{/if}
  <footer>
    <button class="item" onclick={() => ui.open('settings')}>
      <Settings size={16} /><span class="name">Settings</span>
      <span class="kbd">,</span>
    </button>
  </footer>

  <div class="resizer" role="separator" aria-orientation="vertical" onpointerdown={startResize}></div>
</aside>

{#if outboxAt}
  <OutboxMenu x={outboxAt.x} y={outboxAt.y} onclose={() => (outboxAt = null)} />
{/if}

<style>
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
    background: var(--accent);
    color: var(--accent-fg);
    font-weight: 600;
    font-size: var(--text-md);
    box-shadow: 0 1px 2px oklch(0.35 0.2 var(--accent-h) / 0.35), 0 4px 14px oklch(0.55 0.22 var(--accent-h) / 0.28), inset 0 1px 0 rgb(255 255 255 / 0.18);
    transition: transform var(--dur-fast) var(--ease), background var(--dur-fast);
  }
  .compose:hover {
    background: var(--accent-hover);
  }
  .compose:active {
    transform: scale(0.98);
  }
  .compose span:first-of-type {
    flex: 1;
    text-align: left;
  }
  .kbd-hint {
    font-size: 11px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 5px;
    background: rgb(255 255 255 / 0.18);
  }
  .modes {
    display: grid;
    grid-template-columns: 1fr 1fr 36px;
    padding: 3px;
    gap: 2px;
    border-radius: 10px;
    background: var(--bg-active);
  }
  .modes button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 28px;
    border-radius: 8px;
    font-weight: 500;
    color: var(--text-2);
    transition: background var(--dur), color var(--dur), box-shadow var(--dur);
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
  .item.drop {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .item :global(svg) {
    flex-shrink: 0;
    opacity: 0.9;
  }
  .name {
    flex: 1;
    min-width: 0;
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
  .alert {
    display: grid;
    color: var(--amber);
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

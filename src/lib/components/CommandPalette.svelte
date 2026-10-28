<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { Occurrence, ThreadSummary } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail, roleLabels } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { cal } from '../stores/calendar.svelte';
  import type { CalView } from '../stores/calendar.svelte';
  import { listDate, displayName } from '../utils/format';
  import {
    Archive, Calendar, CalendarPlus, Compose, Folder, Inbox, Keyboard, Mail, Moon, Refresh, Search, Settings, Sparkles, Star, Sun, Tag, Trash, Clock, Send, File, Mails,
  } from '../icons';

  interface Command {
    id: string;
    label: string;
    group: string;
    icon: typeof Inbox;
    hint?: string;
    keywords?: string;
    /** Only listed when the query matches, not in the default list. */
    searchOnly?: boolean;
    run: () => void;
  }

  let q = $state('');
  let hi = $state(0);
  let input: HTMLInputElement | undefined = $state();
  let threads = $state<ThreadSummary[]>([]);
  let listEl: HTMLDivElement | undefined = $state();

  const base: Command[] = [
    { id: 'compose', label: 'New message', group: 'Actions', icon: Compose, hint: 'C', run: () => compose.new() },
    { id: 'search', label: 'Search mail', group: 'Actions', icon: Search, hint: '/', run: () => (ui.focusSearch++) },
    { id: 'sync', label: 'Sync all accounts', group: 'Actions', icon: Refresh, run: () => api.syncNow(null) },
    { id: 'read-all', label: 'Mark all as read', group: 'Actions', icon: Mail, run: () => mail.markAllRead() },
    { id: 'theme', label: ui.isDark ? 'Switch to light theme' : 'Switch to dark theme', group: 'Actions', icon: ui.isDark ? Sun : Moon, keywords: 'dark light theme appearance', run: () => ui.set('theme', ui.isDark ? 'light' : 'dark') },
    { id: 'calendar', label: 'Open calendar', group: 'Go to', icon: Calendar, hint: 'G C', run: () => (ui.view = 'calendar') },
    { id: 'inbox', label: 'Inbox', group: 'Go to', icon: Inbox, hint: 'G I', run: () => mail.setMailbox({ kind: 'unified', role: 'inbox' }) },
    { id: 'starred', label: 'Starred', group: 'Go to', icon: Star, hint: 'G S', run: () => mail.setMailbox({ kind: 'starred' }) },
    { id: 'snoozed', label: 'Snoozed', group: 'Go to', icon: Clock, run: () => mail.setMailbox({ kind: 'snoozed' }) },
    { id: 'drafts', label: 'Drafts', group: 'Go to', icon: File, hint: 'G D', run: () => mail.setMailbox({ kind: 'unified', role: 'drafts' }) },
    { id: 'sent', label: 'Sent', group: 'Go to', icon: Send, hint: 'G T', run: () => mail.setMailbox({ kind: 'unified', role: 'sent' }) },
    { id: 'all', label: 'All Mail', group: 'Go to', icon: Mails, hint: 'G A', run: () => mail.setMailbox({ kind: 'everything' }) },
    { id: 'settings', label: 'Settings', group: 'Go to', icon: Settings, hint: 'Ctrl ,', run: () => ui.open('settings') },
    { id: 'ai-settings', label: 'AI settings', group: 'Go to', icon: Sparkles, keywords: 'openrouter openai model key', run: () => ui.open('settings', 'ai') },
    { id: 'shortcuts', label: 'Keyboard shortcuts', group: 'Help', icon: Keyboard, hint: '?', run: () => ui.open('shortcuts') },
    { id: 'new-event', label: 'New event', group: 'Calendar', icon: CalendarPlus, keywords: 'meeting appointment schedule', run: () => { ui.view = 'calendar'; cal.newEvent(); } },
    { id: 'cal-today', label: 'Calendar: today', group: 'Calendar', icon: Calendar, searchOnly: true, run: () => { ui.view = 'calendar'; cal.today(); } },
    ...(['day', 'week', 'month', 'agenda'] as CalView[]).map((v) => ({
      id: `cal-${v}`,
      label: `Calendar: ${v} view`,
      group: 'Calendar',
      icon: Calendar,
      searchOnly: true,
      run: () => { ui.view = 'calendar'; cal.setView(v); },
    })),
    { id: 'cal-settings', label: 'Calendar settings', group: 'Go to', icon: Settings, keywords: 'week start reminder default', searchOnly: true, run: () => ui.open('settings', 'calendar') },
  ];

  const selectionCommands = $derived<Command[]>(
    mail.targets().length
      ? [
          { id: 'archive', label: 'Archive conversation', group: 'Conversation', icon: Archive, hint: 'E', run: () => mail.archive() },
          { id: 'trash', label: 'Delete conversation', group: 'Conversation', icon: Trash, hint: '#', run: () => mail.trash() },
          { id: 'star', label: 'Star / unstar', group: 'Conversation', icon: Star, hint: 'S', run: () => mail.toggleStar() },
          { id: 'unread', label: 'Mark as read / unread', group: 'Conversation', icon: Mail, hint: 'U', run: () => mail.toggleRead() },
        ]
      : [],
  );

  const folderCommands = $derived<Command[]>(
    mail.folders
      .filter((f) => f.selectable || f.kind === 'label')
      .map((f) => {
        const a = mail.accountById.get(f.accountId);
        const name = f.role ? roleLabels[f.role] : f.path;
        return {
          id: `folder-${f.id}`,
          label: mail.multiAccount ? `${name} — ${a?.label ?? a?.email ?? ''}` : name,
          group: 'Folders',
          icon: f.kind === 'label' ? Tag : Folder,
          run: () => mail.setMailbox({ kind: 'folder', id: f.id }),
        };
      }),
  );

  function score(c: Command, query: string): number {
    const hay = `${c.label} ${c.keywords ?? ''}`.toLowerCase();
    const s = query.toLowerCase();
    if (hay.startsWith(s)) return 3;
    if (hay.split(/\s+/).some((w) => w.startsWith(s))) return 2;
    if (hay.includes(s)) return 1;
    // Subsequence match.
    let i = 0;
    for (const ch of hay) if (ch === s[i]) i++;
    return i === s.length ? 0.5 : 0;
  }

  const commands = $derived.by(() => {
    const all = [...selectionCommands, ...base, ...folderCommands];
    if (!q.trim()) return [...selectionCommands, ...base.filter((c) => !c.searchOnly)];
    return all
      .map((c) => ({ c, s: score(c, q.trim()) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 12)
      .map((x) => x.c);
  });

  // Upcoming (and recent) events, searchable by title and place.
  let upcoming = $state<Occurrence[]>([]);
  const events = $derived.by(() => {
    const text = q.trim().toLowerCase();
    if (text.length < 2) return [];
    const now = Date.now();
    return upcoming
      .filter((o) => o.title.toLowerCase().includes(text) || o.location?.toLowerCase().includes(text))
      .sort((a, b) => Number(a.end < now) - Number(b.end < now) || Math.abs(a.start - now) - Math.abs(b.start - now))
      // One row per series: its nearest occurrence.
      .filter((o, i, list) => list.findIndex((x) => x.eventId === o.eventId) === i)
      .slice(0, 4);
  });

  type Row = { kind: 'cmd'; cmd: Command } | { kind: 'event'; event: Occurrence } | { kind: 'thread'; thread: ThreadSummary } | { kind: 'ask' };
  const rows = $derived<Row[]>([
    ...commands.map((cmd) => ({ kind: 'cmd' as const, cmd })),
    ...events.map((event) => ({ kind: 'event' as const, event })),
    ...threads.map((thread) => ({ kind: 'thread' as const, thread })),
    ...(q.trim().length > 3 && ui.settings.aiEnabled ? [{ kind: 'ask' as const }] : []),
  ]);

  let timer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const text = q.trim();
    hi = 0;
    if (timer) clearTimeout(timer);
    if (text.length < 2) {
      threads = [];
      return;
    }
    timer = setTimeout(async () => {
      const page = await api.threads({ mailbox: { kind: 'everything' }, search: text, offset: 0, limit: 6 });
      if (q.trim() === text) threads = page.items;
    }, 90);
  });

  onMount(() => {
    input?.focus();
    const now = Date.now();
    api.calendar
      .occurrences(now - 30 * 86_400_000, now + 180 * 86_400_000)
      .then((list) => (upcoming = list))
      .catch(() => {});
  });

  function run(row: Row | undefined) {
    if (!row) return;
    ui.close();
    if (row.kind === 'cmd') row.cmd.run();
    else if (row.kind === 'event') {
      ui.view = 'calendar';
      cal.view = 'day';
      cal.goto(row.event.start);
    } else if (row.kind === 'thread') {
      mail.setSearch(q.trim());
      mail.select(row.thread.id);
    } else {
      ui.askAi = q.trim();
      ui.open('ask');
    }
  }

  function eventWhen(o: Occurrence): string {
    const d = new Date(o.allDay && o.startDate ? `${o.startDate}T00:00:00` : o.start);
    const day = d.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' });
    return o.allDay ? day : `${day}, ${d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}`;
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      hi = Math.min(rows.length - 1, hi + 1);
      listEl?.querySelector(`[data-i="${hi}"]`)?.scrollIntoView({ block: 'nearest' });
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      hi = Math.max(0, hi - 1);
      listEl?.querySelector(`[data-i="${hi}"]`)?.scrollIntoView({ block: 'nearest' });
    } else if (e.key === 'Enter') {
      e.preventDefault();
      run(rows[hi]);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      ui.close();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="palette" role="dialog" aria-label="Command palette" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key}>
    <div class="search">
      <Search size={18} />
      <input bind:this={input} bind:value={q} placeholder="Type a command, folder or search…" spellcheck="false" />
      <span class="kbd">Esc</span>
    </div>
    <div class="list" bind:this={listEl}>
      {#each rows as row, i (row.kind === 'cmd' ? row.cmd.id : row.kind === 'thread' ? `t${row.thread.id}` : row.kind === 'event' ? `e${row.event.key}` : 'ask')}
        {#if row.kind === 'cmd' && (i === 0 || rows[i - 1].kind !== 'cmd' || (rows[i - 1] as { cmd: Command }).cmd.group !== row.cmd.group)}
          <div class="group">{row.cmd.group}</div>
        {:else if row.kind === 'event' && rows[i - 1]?.kind !== 'event'}
          <div class="group">Events</div>
        {:else if row.kind === 'thread' && rows[i - 1]?.kind !== 'thread'}
          <div class="group">Messages</div>
        {/if}
        <button class="row" class:hi={i === hi} data-i={i} onmouseenter={() => (hi = i)} onclick={() => run(row)}>
          {#if row.kind === 'cmd'}
            <span class="ico"><row.cmd.icon size={16} /></span>
            <span class="label">{row.cmd.label}</span>
            {#if row.cmd.hint}<span class="hint">{row.cmd.hint}</span>{/if}
          {:else if row.kind === 'event'}
            <span class="ico"><span class="dot" style:background={row.event.color}></span></span>
            <span class="label truncate">{row.event.title}{#if row.event.location}<span class="sub">&nbsp;· {row.event.location}</span>{/if}</span>
            <span class="hint">{eventWhen(row.event)}</span>
          {:else if row.kind === 'thread'}
            <span class="ico"><Mail size={16} /></span>
            <span class="label truncate"><b>{displayName(row.thread.participants[row.thread.participants.length - 1])}</b> — {row.thread.subject}</span>
            <span class="hint">{listDate(row.thread.lastDate)}</span>
          {:else}
            <span class="ico ai"><Sparkles size={16} /></span>
            <span class="label">Ask AI: <i>“{q.trim()}”</i></span>
            <span class="hint">Enter</span>
          {/if}
        </button>
      {:else}
        <div class="none">No matches</div>
      {/each}
    </div>
  </div>
</div>

<style>
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }
  .sub {
    color: var(--text-3);
  }
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1500;
    background: var(--overlay);
    display: flex;
    justify-content: center;
    padding-top: 12vh;
    animation: fade-in var(--dur-fast);
  }
  .palette {
    width: min(640px, 92vw);
    max-height: 66vh;
    display: flex;
    flex-direction: column;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: pop-in var(--dur) var(--ease-out);
    align-self: flex-start;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 16px;
    height: 56px;
    border-bottom: 1px solid var(--border);
    color: var(--text-3);
  }
  .search input {
    flex: 1;
    border: none;
    background: none;
    font-size: var(--text-lg);
    color: var(--text);
  }
  .list {
    overflow-y: auto;
    padding: 6px;
  }
  .group {
    padding: 10px 10px 4px;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    height: 40px;
    padding: 0 10px;
    border-radius: 9px;
    text-align: left;
  }
  .row.hi {
    background: var(--bg-selected);
  }
  .ico {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 8px;
    background: var(--bg-active);
    color: var(--text-2);
    flex-shrink: 0;
  }
  .row.hi .ico {
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .ico.ai {
    color: var(--accent-text);
  }
  .label {
    flex: 1;
    min-width: 0;
    font-weight: 500;
  }
  .label b {
    font-weight: 650;
  }
  .hint {
    font-size: var(--text-sm);
    color: var(--text-3);
    flex-shrink: 0;
  }
  .none {
    padding: 24px;
    text-align: center;
    color: var(--text-3);
  }
</style>

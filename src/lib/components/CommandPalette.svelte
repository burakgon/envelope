<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { ContactSummary, Occurrence, Task, ThreadSummary } from '../api';
  import Avatar from './Avatar.svelte';
  import { compose } from '../stores/compose.svelte';
  import { mail, roleLabel } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { people } from '../stores/contacts.svelte';
  import { listName, smartViews, tasks } from '../stores/tasks.svelte';
  import type { CalView } from '../stores/calendar.svelte';
  import { listDate, displayName, dateFormat, timeOf } from '../utils/format';
  import { dueLabel } from '../utils/due';
  import { intlLocale, t, tParts, type MessageKey } from '../i18n';
  import {
    Archive, BellOff, BookUser, Calendar, CalendarPlus, Compose, UserPlus, Folder, Inbox, Keyboard, Mail, Moon, Refresh, Search, Settings, Sparkles, Star, Sun, Tag, Trash, Clock, Send, File, Mails,
    ListChecks, ListPlus,
    Check, AlarmClock,
    ShieldAlert, ShieldCheck,
    Ban,
  } from '../icons';
  import { blockSender, senderOf } from '../stores/rules.svelte';

  type Group = 'actions' | 'goTo' | 'help' | 'calendar' | 'conversation' | 'folders';

  const groupLabels: Record<Group, MessageKey> = {
    actions: 'palette.groupActions',
    goTo: 'palette.groupGoTo',
    help: 'palette.groupHelp',
    calendar: 'palette.groupCalendar',
    conversation: 'palette.groupConversation',
    folders: 'palette.groupFolders',
  };

  interface Command {
    id: string;
    label: string;
    group: Group;
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

  const viewLabels: Record<CalView, MessageKey> = {
    day: 'palette.calendarDayView',
    workweek: 'palette.calendarWorkWeekView',
    week: 'palette.calendarWeekView',
    month: 'palette.calendarMonthView',
    agenda: 'palette.calendarAgendaView',
  };

  // Keywords: synonyms in the interface language, then the English ones so English searches work everywhere.
  const base = $derived<Command[]>([
    { id: 'compose', label: t('palette.newMessage'), group: 'actions', icon: Compose, hint: 'C', run: () => compose.new() },
    { id: 'search', label: t('palette.searchMail'), group: 'actions', icon: Search, hint: '/', run: () => (ui.focusSearch++) },
    { id: 'sync', label: t('palette.syncAll'), group: 'actions', icon: Refresh, run: () => api.syncNow(null) },
    { id: 'read-all', label: t('palette.markAllRead'), group: 'actions', icon: Mail, run: () => mail.markAllRead() },
    { id: 'theme', label: ui.isDark ? t('palette.lightTheme') : t('palette.darkTheme'), group: 'actions', icon: ui.isDark ? Sun : Moon, keywords: `${t('palette.kw.theme')} dark light theme appearance`, run: () => ui.set('theme', ui.isDark ? 'light' : 'dark') },
    { id: 'calendar', label: t('palette.openCalendar'), group: 'goTo', icon: Calendar, hint: 'G C', run: () => (ui.view = 'calendar') },
    { id: 'inbox', label: t('mailbox.inbox'), group: 'goTo', icon: Inbox, hint: 'G I', run: () => mail.setMailbox({ kind: 'unified', role: 'inbox' }) },
    { id: 'starred', label: t('mailbox.starred'), group: 'goTo', icon: Star, hint: 'G S', run: () => mail.setMailbox({ kind: 'starred' }) },
    { id: 'snoozed', label: t('mailbox.snoozed'), group: 'goTo', icon: Clock, run: () => mail.setMailbox({ kind: 'snoozed' }) },
    { id: 'drafts', label: t('mailbox.drafts'), group: 'goTo', icon: File, hint: 'G D', run: () => mail.setMailbox({ kind: 'unified', role: 'drafts' }) },
    { id: 'sent', label: t('mailbox.sent'), group: 'goTo', icon: Send, hint: 'G T', run: () => mail.setMailbox({ kind: 'unified', role: 'sent' }) },
    { id: 'all', label: t('mailbox.all'), group: 'goTo', icon: Mails, hint: 'G A', run: () => mail.setMailbox({ kind: 'everything' }) },
    { id: 'settings', label: t('palette.settings'), group: 'goTo', icon: Settings, hint: `${t('keys.ctrl')} ,`, run: () => ui.open('settings') },
    { id: 'ai-settings', label: t('palette.aiSettings'), group: 'goTo', icon: Sparkles, keywords: `${t('palette.kw.aiSettings')} openrouter openai model key`, run: () => ui.open('settings', 'ai') },
    { id: 'shortcuts', label: t('palette.shortcuts'), group: 'help', icon: Keyboard, hint: '?', run: () => ui.open('shortcuts') },
    { id: 'contacts', label: t('palette.contacts'), group: 'goTo', icon: BookUser, hint: 'G P', keywords: `${t('palette.kw.contacts')} people address book`, run: () => (ui.view = 'contacts') },
    { id: 'new-contact', label: t('palette.newContact'), group: 'actions', icon: UserPlus, searchOnly: true, keywords: `${t('palette.kw.newContact')} person add address book`, run: () => { ui.view = 'contacts'; people.init().then(() => people.newContact()); } },
    { id: 'new-event', label: t('palette.newEvent'), group: 'calendar', icon: CalendarPlus, keywords: `${t('palette.kw.newEvent')} meeting appointment schedule`, run: () => { ui.view = 'calendar'; cal.newEvent(); } },
    { id: 'cal-today', label: t('palette.calendarToday'), group: 'calendar', icon: Calendar, searchOnly: true, run: () => { ui.view = 'calendar'; cal.today(); } },
    ...(['day', 'workweek', 'week', 'month', 'agenda'] as CalView[]).map((v): Command => ({
      id: `cal-${v}`,
      label: t(viewLabels[v]),
      group: 'calendar',
      icon: Calendar,
      searchOnly: true,
      run: () => { ui.view = 'calendar'; cal.setView(v); },
    })),
    { id: 'cal-settings', label: t('palette.calendarSettings'), group: 'goTo', icon: Settings, keywords: `${t('palette.kw.calendarSettings')} week start reminder default`, searchOnly: true, run: () => ui.open('settings', 'calendar') },
    { id: 'tasks', label: t('tasks.palette.openTasks'), group: 'goTo', icon: ListChecks, hint: 'G K', keywords: `${t('tasks.palette.kwTasks')} todo to-do reminders checklist`, run: () => (ui.view = 'tasks') },
    { id: 'new-task', label: t('tasks.palette.newTask'), group: 'actions', icon: ListPlus, searchOnly: true, keywords: `${t('tasks.palette.kwNewTask')} todo to-do add reminder`, run: () => tasks.newTask() },
    ...smartViews.map((v): Command => ({
      id: `tasks-${v}`,
      label: t('tasks.palette.view', { view: t(`tasks.view.${v}`) }),
      group: 'goTo',
      icon: ListChecks,
      searchOnly: true,
      keywords: `${t('tasks.palette.kwTasks')} todo`,
      run: () => tasks.setView(v),
    })),
  ]);

  const selectionCommands = $derived<Command[]>(
    mail.targets().length
      ? [
          { id: 'archive', label: t('palette.archiveConversation'), group: 'conversation', icon: Archive, hint: 'E', run: () => mail.archive() },
          { id: 'trash', label: t('palette.deleteConversation'), group: 'conversation', icon: Trash, hint: '#', run: () => mail.trash() },
          { id: 'star', label: t('palette.toggleStar'), group: 'conversation', icon: Star, hint: 'S', run: () => mail.toggleStar() },
          { id: 'unread', label: t('palette.toggleRead'), group: 'conversation', icon: Mail, hint: 'U', run: () => mail.toggleRead() },
          { id: 'mute', label: t('palette.toggleMute'), group: 'conversation', icon: BellOff, hint: 'M', run: () => mail.toggleMute() },
          { id: 'add-task', label: t('tasks.palette.addToTasks'), group: 'conversation', icon: ListPlus, hint: 'T', run: () => tasks.addFromMail(mail.targets()) },
          mail.isSpam()
            ? { id: 'not-spam', label: t('palette.notSpam'), group: 'conversation', icon: ShieldCheck, hint: '!', keywords: t('palette.kw.notSpam'), run: () => mail.notSpam() }
            : { id: 'spam', label: t('palette.reportSpam'), group: 'conversation', icon: ShieldAlert, hint: '!', keywords: t('palette.kw.reportSpam'), run: () => mail.moveTo({ kind: 'junk' }) },
          ...blockCommand(),
          ...(mail.inFollowUp
            ? [
                { id: 'follow-up-done', label: t('palette.followUpDone'), group: 'conversation', icon: Check, run: () => mail.followUpDone() } satisfies Command,
                { id: 'follow-up-later', label: t('palette.followUpTomorrow'), group: 'conversation', icon: AlarmClock, run: () => mail.followUpLater(tomorrowMorning()) } satisfies Command,
              ]
            : []),
        ]
      : [],
  );

  /** "Block …" for the one conversation selected. */
  function blockCommand(): Command[] {
    const ids = mail.targets();
    const thread = ids.length === 1 ? (mail.items.find((th) => th.id === ids[0]) ?? (mail.detail?.thread.id === ids[0] ? mail.detail.thread : undefined)) : undefined;
    const sender = thread ? senderOf(thread) : null;
    if (!sender) return [];
    return [{ id: 'block', label: t('palette.blockSender', { name: displayName(sender) }), group: 'conversation', icon: Ban, searchOnly: true, keywords: t('palette.kw.blockSender'), run: () => blockSender(sender) }];
  }

  function tomorrowMorning(): number {
    const d = new Date();
    d.setDate(d.getDate() + 1);
    d.setHours(8, 0, 0, 0);
    return d.getTime();
  }

  const folderCommands = $derived<Command[]>(
    mail.folders
      .filter((f) => f.selectable || f.kind === 'label')
      .map((f) => {
        const a = mail.accountById.get(f.accountId);
        const name = f.role ? roleLabel(f.role) : f.path;
        return {
          id: `folder-${f.id}`,
          label: mail.multiAccount ? `${name} — ${a?.label ?? a?.email ?? ''}` : name,
          group: 'folders',
          icon: f.kind === 'label' ? Tag : Folder,
          run: () => mail.setMailbox({ kind: 'folder', id: f.id }),
        };
      }),
  );

  /** Lowercase without accents ("Ç" → "c", "ı" → "i", "ł" → "l"), so a search matches however it is typed. */
  function fold(text: string): string {
    return text
      .toLocaleLowerCase(intlLocale())
      .normalize('NFD')
      .replace(/[\u0300-\u036f]/g, '')
      .replace(/ı/g, 'i')
      .replace(/ł/g, 'l');
  }

  function score(c: Command, query: string): number {
    const hay = fold(`${c.label} ${c.keywords ?? ''}`);
    const s = fold(query);
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
    const text = fold(q.trim());
    if (text.length < 2) return [];
    const now = Date.now();
    return upcoming
      .filter((o) => fold(o.title).includes(text) || (o.location != null && fold(o.location).includes(text)))
      .sort((a, b) => Number(a.end < now) - Number(b.end < now) || Math.abs(a.start - now) - Math.abs(b.start - now))
      // One row per series: its nearest occurrence.
      .filter((o, i, list) => list.findIndex((x) => x.eventId === o.eventId) === i)
      .slice(0, 4);
  });

  // Open tasks, by title.
  const foundTasks = $derived.by(() => {
    const text = fold(q.trim());
    if (text.length < 2) return [];
    return tasks.all.filter((x) => fold(x.title).includes(text)).slice(0, 4);
  });

  // People from the address books.
  let found = $state<ContactSummary[]>([]);
  let contactTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const text = q.trim();
    if (contactTimer) clearTimeout(contactTimer);
    if (text.length < 2) {
      found = [];
      return;
    }
    contactTimer = setTimeout(async () => {
      try {
        const page = await api.contacts.list(text, null, 0, 3);
        if (q.trim() === text) found = page.items;
      } catch {
        found = [];
      }
    }, 70);
  });

  type Row =
    | { kind: 'cmd'; cmd: Command }
    | { kind: 'person'; person: ContactSummary }
    | { kind: 'event'; event: Occurrence }
    | { kind: 'task'; task: Task }
    | { kind: 'thread'; thread: ThreadSummary }
    | { kind: 'ask' };
  const rows = $derived<Row[]>([
    ...commands.map((cmd) => ({ kind: 'cmd' as const, cmd })),
    ...found.map((person) => ({ kind: 'person' as const, person })),
    ...events.map((event) => ({ kind: 'event' as const, event })),
    ...foundTasks.map((task) => ({ kind: 'task' as const, task })),
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
    else if (row.kind === 'person') {
      const p = row.person;
      if (p.email) compose.new({ to: [{ name: p.name, email: p.email }] });
      else {
        ui.view = 'contacts';
        people.select(p.id);
      }
    } else if (row.kind === 'event') {
      ui.view = 'calendar';
      cal.view = 'day';
      cal.goto(row.event.start);
    } else if (row.kind === 'task') {
      tasks.reveal(row.task.id);
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
    const day = dateFormat({ weekday: 'short', month: 'short', day: 'numeric' }).format(d);
    return o.allDay ? day : t('time.dateAt', { date: day, time: timeOf(d) });
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
  <div class="palette" role="dialog" aria-label={t('palette.title')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key}>
    <div class="search">
      <Search size={18} />
      <input bind:this={input} bind:value={q} placeholder={t('palette.placeholder')} spellcheck="false" />
      <span class="kbd">{t('keys.esc')}</span>
    </div>
    <div class="list" bind:this={listEl}>
      {#each rows as row, i (row.kind === 'cmd' ? row.cmd.id : row.kind === 'thread' ? `t${row.thread.id}` : row.kind === 'event' ? `e${row.event.key}` : row.kind === 'person' ? `p${row.person.id}` : row.kind === 'task' ? `k${row.task.id}` : 'ask')}
        {#if row.kind === 'cmd' && (i === 0 || rows[i - 1].kind !== 'cmd' || (rows[i - 1] as { cmd: Command }).cmd.group !== row.cmd.group)}
          <div class="group">{t(groupLabels[row.cmd.group])}</div>
        {:else if row.kind === 'person' && rows[i - 1]?.kind !== 'person'}
          <div class="group">{t('palette.groupPeople')}</div>
        {:else if row.kind === 'event' && rows[i - 1]?.kind !== 'event'}
          <div class="group">{t('palette.groupEvents')}</div>
        {:else if row.kind === 'task' && rows[i - 1]?.kind !== 'task'}
          <div class="group">{t('tasks.palette.groupTasks')}</div>
        {:else if row.kind === 'thread' && rows[i - 1]?.kind !== 'thread'}
          <div class="group">{t('palette.groupMessages')}</div>
        {/if}
        <button class="row" class:hi={i === hi} data-i={i} onmouseenter={() => (hi = i)} onclick={() => run(row)}>
          {#if row.kind === 'cmd'}
            <span class="ico"><row.cmd.icon size={16} /></span>
            <span class="label truncate">{row.cmd.label}</span>
            {#if row.cmd.hint}<span class="hint">{row.cmd.hint}</span>{/if}
          {:else if row.kind === 'person'}
            <span class="ico"><Avatar person={{ name: row.person.name, email: row.person.email ?? row.person.name }} size={22} /></span>
            <span class="label truncate">{row.person.email ? t('palette.writeTo', { name: row.person.name }) : row.person.name}{#if row.person.email}<span class="sub">&nbsp;· {row.person.email}</span>{/if}</span>
            <span
              class="hint open-contact"
              role="button"
              tabindex="-1"
              onkeydown={() => {}}
              onclick={(e) => {
                e.stopPropagation();
                ui.close();
                ui.view = 'contacts';
                people.init().then(() => people.select(row.person.id));
              }}>{t('common.open')}</span>
          {:else if row.kind === 'event'}
            <span class="ico"><span class="dot" style:background={row.event.color}></span></span>
            <span class="label truncate">{row.event.title}{#if row.event.location}<span class="sub">&nbsp;· {row.event.location}</span>{/if}</span>
            <span class="hint">{eventWhen(row.event)}</span>
          {:else if row.kind === 'task'}
            {@const list = tasks.listById.get(row.task.listId)}
            <span class="ico"><span class="ring" style:--c={list?.color ?? 'var(--text-3)'}></span></span>
            <span class="label truncate">{row.task.title}{#if list}<span class="sub">&nbsp;· {listName(list)}</span>{/if}</span>
            {#if row.task.dueDate}<span class="hint">{dueLabel(row.task)}</span>{/if}
          {:else if row.kind === 'thread'}
            <span class="ico"><Mail size={16} /></span>
            <span class="label truncate"><b>{displayName(row.thread.participants[row.thread.participants.length - 1])}</b> — {row.thread.subject}</span>
            <span class="hint">{listDate(row.thread.lastDate)}</span>
          {:else}
            <span class="ico ai"><Sparkles size={16} /></span>
            <span class="label truncate">{#each tParts('palette.askAi') as part, j (j)}{#if typeof part === 'string'}{part}{:else}<i>{q.trim()}</i>{/if}{/each}</span>
            <span class="hint">{t('keys.enter')}</span>
          {/if}
        </button>
      {:else}
        <div class="none">{t('palette.noMatches')}</div>
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
  .ring {
    width: 13px;
    height: 13px;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1.5px var(--c);
  }
  .sub {
    color: var(--text-3);
  }
  .open-contact {
    padding: 2px 8px;
    border-radius: 6px;
  }
  .open-contact:hover {
    background: var(--bg-active);
    color: var(--text);
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

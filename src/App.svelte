<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api } from './lib/api';
  import { rawMessages } from './lib/i18n';
  import { compose } from './lib/stores/compose.svelte';
  import { mail } from './lib/stores/mail.svelte';
  import { vips } from './lib/stores/vips.svelte';
  import { toasts } from './lib/stores/toasts.svelte';
  import { ui } from './lib/stores/ui.svelte';
  import { ai } from './lib/stores/ai.svelte';
  import { cal } from './lib/stores/calendar.svelte';
  import { autoReplies } from './lib/stores/autoReplies.svelte';
  import FileViewer from './lib/components/FileViewer.svelte';
  import SubscribeDialog from './lib/components/calendar/SubscribeDialog.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import ThreadList from './lib/components/ThreadList.svelte';
  import Reader from './lib/components/Reader.svelte';
  import Composer from './lib/components/Composer.svelte';
  import Toasts from './lib/components/Toasts.svelte';
  import CommandPalette from './lib/components/CommandPalette.svelte';
  import ShortcutsHelp from './lib/components/ShortcutsHelp.svelte';
  import SnoozeMenu from './lib/components/SnoozeMenu.svelte';
  import MoveMenu from './lib/components/MoveMenu.svelte';
  import Settings from './lib/components/settings/Settings.svelte';
  import PassphraseDialog from './lib/components/PassphraseDialog.svelte';
  import { pgp } from './lib/stores/pgp.svelte';
  import Onboarding from './lib/components/Onboarding.svelte';
  import AskAi from './lib/components/AskAi.svelte';
  import SubscriptionsDialog from './lib/components/SubscriptionsDialog.svelte';
  import CalendarView from './lib/components/calendar/CalendarView.svelte';
  import EventEditor from './lib/components/calendar/EventEditor.svelte';
  import ContactsView from './lib/components/contacts/ContactsView.svelte';
  import TasksView from './lib/components/tasks/TasksView.svelte';
  import { people } from './lib/stores/contacts.svelte';
  import { tasks } from './lib/stores/tasks.svelte';
  import ScopeDialog from './lib/components/calendar/ScopeDialog.svelte';
  import QuestionDialog from './lib/components/QuestionDialog.svelte';
  import { dialogs } from './lib/stores/dialogs.svelte';

  let booted = $state(false);
  /** Message and calendar files handed to the app, shown one after another. */
  let openedFiles = $state<string[]>([]);
  let kbMenu = $state<{ kind: 'snooze' | 'move' | 'label'; ids: number[] } | null>(null);
  let list: ReturnType<typeof ThreadList> | undefined = $state();
  let reader: ReturnType<typeof Reader> | undefined = $state();
  /** A conversation is open (or opening): with one pane at a time, it takes the window. */
  const reading = $derived(mail.detail != null || mail.detailLoading);
  let gPending = false;
  let gTimer: ReturnType<typeof setTimeout> | null = null;

  // Theme, accent and density on the root element.
  $effect(() => {
    const root = document.documentElement;
    root.dataset.theme = ui.isDark ? 'dark' : 'light';
    root.dataset.density = ui.settings.density;
    root.style.setProperty('--accent-h', String(ui.settings.accentHue));
  });

  $effect(() => {
    api.setBadge(mail.inboxUnread);
  });

  // Notifications and the tray menu are drawn by the desktop shell: hand it their
  // texts in the interface language, again whenever the language changes.
  $effect(() => {
    api.setNativeStrings(rawMessages('native.')).catch(() => {});
  });

  onMount(() => {
    (async () => {
      await ui.load();
      await mail.init();
      vips.load();
      await ai.load();
      booted = true;
      cal.init();
      tasks.init();
      // Whether any account is sending automatic replies; not urgent.
      setTimeout(() => autoReplies.refresh(mail.accounts), 4000);
      if (mail.accounts.length === 0) ui.open('onboarding');
      // Not an animation frame: the window is still hidden, and hidden views
      // get none, so the window would never be shown.
      await tick();
      api.ready();
    })().catch((e) => {
      booted = true;
      toasts.error(String(e?.message ?? e));
      api.ready();
    });
    const offMailto = api.onMailto((url) => compose.fromMailto(url));
    const offFile = api.onOpenFile((path) => (openedFiles = [...openedFiles, path]));
    const offWebcal = api.onWebcal((url) => (cal.subscribing = url));
    const offOpen = api.onOpenThread((id) => {
      ui.view = 'mail';
      if (id != null) {
        mail.setMailbox({ kind: 'unified', role: 'inbox' });
        mail.select(id);
      }
    });
    const offTask = api.onOpenTask((id) => tasks.reveal(id));
    const offResize = api.window.onResized(async () => (ui.maximized = await api.window.isMaximized()));
    return () => {
      offMailto();
      offFile();
      offWebcal();
      offOpen();
      offTask();
      offResize();
    };
  });

  /** The newest message of the open conversation that is not a draft (replies go to it). */
  function lastMessageId(): number | null {
    const m = mail.detail?.messages ?? [];
    return ([...m].reverse().find((x) => !x.flags.draft) ?? m[m.length - 1])?.id ?? null;
  }

  /**
   * After j or k from outside the list (nothing focused, or the conversation), the list takes the
   * focus: screen readers then say which conversation the selection moved to.
   */
  function focusList() {
    const now = document.activeElement;
    if (now && now !== document.body && !now.closest('.reader')) return;
    document.querySelector<HTMLElement>('.list .viewport')?.focus({ preventScroll: true });
  }

  function startG() {
    gPending = true;
    if (gTimer) clearTimeout(gTimer);
    gTimer = setTimeout(() => (gPending = false), 1200);
  }

  function onTasksKey(e: KeyboardEvent): void {
    const key = e.key;
    // Enter and Space on a focused button press that button.
    if ((key === 'Enter' || key === ' ') && (e.target as HTMLElement | null)?.closest?.('button, a, select')) return;
    const task = tasks.target();
    const actions: Record<string, () => void> = {
      g: startG,
      c: () => tasks.newTask(),
      n: () => tasks.newTask(),
      j: () => tasks.step(1),
      ArrowDown: () => tasks.step(1),
      k: () => tasks.step(-1),
      ArrowUp: () => tasks.step(-1),
      x: () => task && tasks.complete(task),
      ' ': () => task && tasks.complete(task),
      Enter: () => openTask(),
      o: () => openTask(),
      s: () => task && tasks.toggleImportant(task),
      '#': () => task && tasks.remove(task),
      Delete: () => task && tasks.remove(task),
      Escape: () => (tasks.openId != null ? tasks.closeDetail() : (tasks.selectedId = null)),
      z: () => toasts.undoLast(),
      '/': () => ui.open('palette'),
      '?': () => ui.open('shortcuts'),
    };
    const fn = actions[key];
    if (fn) {
      e.preventDefault();
      fn();
    }
  }

  /** Opens the selected task; with it open already, its title is edited. */
  function openTask() {
    const id = tasks.selectedId ?? tasks.visible[0]?.id;
    if (id == null) return;
    if (tasks.openId === id) document.querySelector<HTMLElement>('.detail textarea.title')?.focus();
    else tasks.openTask(id);
  }

  function onKey(e: KeyboardEvent) {
    // A question on screen has the keyboard until it is answered.
    if (dialogs.current) return;
    const target = e.target as HTMLElement | null;
    const typing = !!target?.closest?.('input, textarea, select, [contenteditable="true"]');
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key;

    if (mod && key.toLowerCase() === 'k' && !target?.closest?.('[contenteditable="true"]')) {
      e.preventDefault();
      ui.overlay === 'palette' ? ui.close() : ui.open('palette');
      return;
    }
    if (mod && key === ',') {
      e.preventDefault();
      ui.open('settings');
      return;
    }
    if (mod && key.toLowerCase() === 'n') {
      e.preventDefault();
      compose.new();
      return;
    }
    if (typing || e.defaultPrevented) return;
    if (cal.editor || cal.scopeAsk) return;
    if (ui.overlay) {
      if (key === 'Escape') ui.close();
      return;
    }
    // The whole sidebar over a narrow window has the keyboard until it closes.
    if (ui.railOpen) {
      if (key === 'Escape') ui.railOpen = false;
      return;
    }
    if (mod && key.toLowerCase() === 'a' && ui.view === 'mail') {
      e.preventDefault();
      mail.items.forEach((t) => t.id > 0 && mail.checked.add(t.id));
      return;
    }
    if (e.altKey && !mod && ui.view === 'tasks' && (key === 'ArrowUp' || key === 'ArrowDown')) {
      e.preventDefault();
      tasks.nudge(key === 'ArrowUp' ? -1 : 1);
      return;
    }
    if (mod || e.altKey) return;

    if (gPending) {
      gPending = false;
      const go: Record<string, () => void> = {
        i: () => mail.setMailbox({ kind: 'unified', role: 'inbox' }),
        s: () => mail.setMailbox({ kind: 'starred' }),
        d: () => mail.setMailbox({ kind: 'unified', role: 'drafts' }),
        t: () => mail.setMailbox({ kind: 'unified', role: 'sent' }),
        a: () => mail.setMailbox({ kind: 'everything' }),
        c: () => (ui.view = 'calendar'),
        m: () => (ui.view = 'mail'),
        p: () => (ui.view = 'contacts'),
        k: () => (ui.view = 'tasks'),
      };
      go[key.toLowerCase()]?.();
      return;
    }

    if (ui.view === 'tasks') return onTasksKey(e);

    if (ui.view === 'contacts') {
      if (people.editing) return;
      if (key === 'g') {
        startG();
      } else if (key === 'c' || key === 'n') {
        e.preventDefault();
        people.newContact();
      } else if (key === '/') {
        e.preventDefault();
        ui.focusSearch++;
      } else if (key === 'j' || key === 'ArrowDown') {
        e.preventDefault();
        people.step(1);
      } else if (key === 'k' || key === 'ArrowUp') {
        e.preventDefault();
        people.step(-1);
      } else if (key === 'e' && people.detail) {
        // The editor focuses the first name: the key must not be typed into it.
        e.preventDefault();
        people.edit();
      } else if (key === '?') ui.open('shortcuts');
      return;
    }

    if (ui.view === 'calendar') {
      if (cal.editor || cal.scopeAsk) return;
      const actions: Record<string, () => void> = {
        g: startG,
        c: () => cal.newEvent(),
        t: () => cal.today(),
        d: () => cal.setView('day'),
        x: () => cal.setView('workweek'),
        w: () => cal.setView('week'),
        m: () => cal.setView('month'),
        a: () => cal.setView('agenda'),
        j: () => cal.step(1),
        n: () => cal.step(1),
        ArrowRight: () => cal.step(1),
        k: () => cal.step(-1),
        p: () => cal.step(-1),
        ArrowLeft: () => cal.step(-1),
        '/': () => ui.open('palette'),
        '?': () => ui.open('shortcuts'),
      };
      const fn = actions[key];
      if (fn) {
        e.preventDefault();
        fn();
      }
      return;
    }

    const ids = mail.targets();
    // Space and Enter on a focused button press it.
    if ((key === ' ' || key === 'Enter') && target?.closest?.('button, a, select')) return;
    switch (key) {
      case 'j':
      case 'ArrowDown':
        e.preventDefault();
        mail.move(1);
        focusList();
        break;
      case 'k':
      case 'ArrowUp':
        e.preventDefault();
        mail.move(-1);
        focusList();
        break;
      case 'Enter':
      case 'o': {
        // In Drafts, a draft opens in a composer.
        const row = mail.items.find((x) => x.id === mail.selectedId);
        if (row) compose.openRow(row);
        else if (mail.selectedId != null) mail.openThread(mail.selectedId);
        else mail.move(0);
        break;
      }
      case 'e':
      case 'y':
        mail.archive();
        break;
      case '#':
      case 'Delete':
        mail.trash();
        break;
      case 's':
        if (ids.length) mail.toggleStar(ids);
        break;
      case 'u':
      case 'U':
        if (ids.length) mail.toggleRead(ids);
        break;
      case 'h':
      case 'b':
        if (ids.length) kbMenu = { kind: 'snooze', ids };
        break;
      case 'm':
        if (ids.length) mail.toggleMute(ids);
        break;
      case 't':
        if (ids.length) tasks.addFromMail(ids);
        break;
      case 'v':
      case 'l':
        // The menu's filter takes the focus: the key must not be typed into it.
        e.preventDefault();
        if (ids.length) kbMenu = { kind: key === 'v' ? 'move' : 'label', ids };
        break;
      case '!':
        if (ids.length) mail.toggleSpam(ids);
        break;
      case 'x':
        if (mail.selectedId != null) mail.toggleCheck(mail.selectedId);
        break;
      case 'z':
        toasts.undoLast();
        break;
      case 'c':
        e.preventDefault();
        compose.new();
        break;
      case 'r':
      case 'a':
      case 'f': {
        const id = lastMessageId();
        if (id != null) {
          e.preventDefault();
          compose.reply(id, key === 'r' ? 'reply' : key === 'a' ? 'replyAll' : 'forward', key !== 'f');
        }
        break;
      }
      case '/':
        e.preventDefault();
        ui.focusSearch++;
        break;
      case '?':
        ui.open('shortcuts');
        break;
      case 'g':
        startG();
        break;
      case '1':
      case '2':
      case '3':
      case '4':
        if (mail.splitActive) mail.setCategory([ 'people', 'newsletters', 'notifications', null ][Number(key) - 1]);
        break;
      case ' ':
      case 'PageDown':
      case 'PageUp': {
        // Reading: a page further (back with Shift+Space); Space at the end goes on to the next unread conversation.
        if (!mail.detail) break;
        e.preventDefault();
        const down = key === 'PageDown' || (key === ' ' && !e.shiftKey);
        if (!reader?.page(down ? 1 : -1) && key === ' ' && down) mail.nextUnread();
        break;
      }
      case 'Home':
      case 'End':
        if (!mail.detail) break;
        e.preventDefault();
        reader?.edge(key === 'End' ? 1 : -1);
        break;
      case 'n':
      case 'p':
        reader?.step(key === 'n' ? 1 : -1);
        break;
      case ';':
        reader?.expandAll();
        break;
      case 'Backspace':
        // Back to the list, with one pane at a time.
        if (ui.single && reading) {
          e.preventDefault();
          mail.closeThread();
        }
        break;
      case 'F10':
      case 'ContextMenu':
        // The context menu of the selected conversation, as a right click opens it.
        if (key === 'F10' && !e.shiftKey) break;
        e.preventDefault();
        list?.openRowMenu();
        break;
      case 'Escape':
        // A reply under the conversation closes first (its draft is kept).
        if (reader?.closeReply()) break;
        if (ui.single && reading) mail.closeThread();
        else if (mail.checked.size) mail.checked.clear();
        else if (mail.selectedId != null) mail.select(null);
        break;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="app" class:booted>
  {#if ui.rail}
    <Sidebar rail />
  {:else if !ui.sidebarCollapsed}
    <Sidebar />
  {/if}
  <main>
    {#if ui.view === 'mail'}
      <!-- One pane at a time in a narrow window: the list, or the conversation open from it. -->
      {#if !ui.single || !reading}<ThreadList bind:this={list} />{/if}
      {#if !ui.single || reading}<Reader bind:this={reader} />{/if}
    {:else if ui.view === 'calendar'}
      <CalendarView />
    {:else if ui.view === 'tasks'}
      <TasksView />
    {:else}
      <ContactsView />
    {/if}
  </main>

  <div class="dock">
    {#each compose.floating as c (c.key)}
      <Composer state={c} />
    {/each}
  </div>
</div>

{#if ui.overlay === 'palette'}<CommandPalette {reader} />{/if}
{#if ui.overlay === 'shortcuts'}<ShortcutsHelp />{/if}
{#if ui.overlay === 'settings'}<Settings />{/if}
{#if ui.overlay === 'onboarding' || ui.overlay === 'addAccount'}<Onboarding first={ui.overlay === 'onboarding'} />{/if}
{#if ui.overlay === 'ask'}<AskAi />{/if}
{#if ui.overlay === 'subscriptions'}<SubscriptionsDialog />{/if}
{#if cal.editor}<EventEditor />{/if}
{#if cal.subscribing !== null}<SubscribeDialog initial={cal.subscribing} onclose={() => (cal.subscribing = null)} />{/if}
{#if openedFiles.length}{#key openedFiles[0]}<FileViewer path={openedFiles[0]} onclose={() => (openedFiles = openedFiles.slice(1))} />{/key}{/if}
{#if cal.scopeAsk}<ScopeDialog />{/if}
{#if pgp.ask}<PassphraseDialog />{/if}
{#if dialogs.current}{#key dialogs.current}<QuestionDialog />{/key}{/if}

{#if kbMenu}
  {@const ids = kbMenu.ids}
  {#if kbMenu.kind === 'snooze'}
    <SnoozeMenu x={innerWidth / 2} y={innerHeight * 0.22} align="start" onclose={() => (kbMenu = null)} onpick={(ts) => { mail.snooze(ts, ids); kbMenu = null; }} />
  {:else}
    <MoveMenu x={innerWidth / 2 - 130} y={innerHeight * 0.22} {ids} labels={kbMenu.kind === 'label'} onclose={() => (kbMenu = null)} />
  {/if}
{/if}

<Toasts />

<style>
  .app {
    display: flex;
    height: 100vh;
    overflow: hidden;
    opacity: 0;
    transition: opacity 160ms var(--ease);
  }
  .app.booted {
    opacity: 1;
  }
  main {
    flex: 1;
    min-width: 0;
    display: flex;
    height: 100%;
  }
  .dock {
    position: fixed;
    right: 18px;
    bottom: 0;
    display: flex;
    align-items: flex-end;
    gap: 12px;
    z-index: 700;
    pointer-events: none;
  }
</style>

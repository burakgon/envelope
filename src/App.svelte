<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from './lib/api';
  import { compose } from './lib/stores/compose.svelte';
  import { mail } from './lib/stores/mail.svelte';
  import { toasts } from './lib/stores/toasts.svelte';
  import { ui } from './lib/stores/ui.svelte';
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
  import Onboarding from './lib/components/Onboarding.svelte';
  import AskAi from './lib/components/AskAi.svelte';
  import CalendarView from './lib/components/calendar/CalendarView.svelte';

  let booted = $state(false);
  let kbMenu = $state<{ kind: 'snooze' | 'move' | 'label'; ids: number[] } | null>(null);
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

  onMount(() => {
    (async () => {
      await ui.load();
      await mail.init();
      booted = true;
      if (mail.accounts.length === 0) ui.open('onboarding');
      requestAnimationFrame(() => api.ready());
    })().catch((e) => {
      booted = true;
      toasts.error(String(e?.message ?? e));
      api.ready();
    });
    const offMailto = api.onMailto((url) => compose.fromMailto(url));
    const offOpen = api.onOpenThread((id) => {
      ui.view = 'mail';
      if (id != null) {
        mail.setMailbox({ kind: 'unified', role: 'inbox' });
        mail.select(id);
      }
    });
    const offResize = api.window.onResized(async () => (ui.maximized = await api.window.isMaximized()));
    return () => {
      offMailto();
      offOpen();
      offResize();
    };
  });

  function lastMessageId(): number | null {
    const m = mail.detail?.messages;
    return m && m.length ? m[m.length - 1].id : null;
  }

  function onKey(e: KeyboardEvent) {
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
    if (ui.overlay) {
      if (key === 'Escape') ui.close();
      return;
    }
    if (mod && key.toLowerCase() === 'a' && ui.view === 'mail') {
      e.preventDefault();
      mail.items.forEach((t) => mail.checked.add(t.id));
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
      };
      go[key.toLowerCase()]?.();
      return;
    }

    if (ui.view === 'calendar') {
      if (key === 'g') {
        gPending = true;
        if (gTimer) clearTimeout(gTimer);
        gTimer = setTimeout(() => (gPending = false), 1200);
      } else if (key === 'c') compose.new();
      return;
    }

    const ids = mail.targets();
    switch (key) {
      case 'j':
      case 'ArrowDown':
        e.preventDefault();
        mail.move(1);
        break;
      case 'k':
      case 'ArrowUp':
        e.preventDefault();
        mail.move(-1);
        break;
      case 'Enter':
      case 'o':
        if (mail.selectedId != null) mail.openThread(mail.selectedId);
        else mail.move(0);
        break;
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
      case 'v':
        if (ids.length) kbMenu = { kind: 'move', ids };
        break;
      case 'l':
        if (ids.length) kbMenu = { kind: 'label', ids };
        break;
      case '!':
        if (ids.length) mail.moveTo({ kind: 'junk' }, ids);
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
        gPending = true;
        if (gTimer) clearTimeout(gTimer);
        gTimer = setTimeout(() => (gPending = false), 1200);
        break;
      case '1':
      case '2':
      case '3':
      case '4':
        if (mail.splitActive) mail.setCategory([ 'people', 'newsletters', 'notifications', null ][Number(key) - 1]);
        break;
      case 'Escape':
        if (mail.checked.size) mail.checked.clear();
        else if (mail.selectedId != null) mail.select(null);
        break;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="app" class:booted>
  {#if !ui.sidebarCollapsed}
    <Sidebar />
  {/if}
  <main>
    {#if ui.view === 'mail'}
      <ThreadList />
      <Reader />
    {:else}
      <CalendarView />
    {/if}
  </main>

  <div class="dock">
    {#each compose.floating as c (c.key)}
      <Composer state={c} />
    {/each}
  </div>
</div>

{#if ui.overlay === 'palette'}<CommandPalette />{/if}
{#if ui.overlay === 'shortcuts'}<ShortcutsHelp />{/if}
{#if ui.overlay === 'settings'}<Settings />{/if}
{#if ui.overlay === 'onboarding' || ui.overlay === 'addAccount'}<Onboarding first={ui.overlay === 'onboarding'} />{/if}
{#if ui.overlay === 'ask'}<AskAi />{/if}

{#if kbMenu}
  {@const ids = kbMenu.ids}
  {#if kbMenu.kind === 'snooze'}
    <SnoozeMenu x={innerWidth / 2} y={innerHeight * 0.22} align="start" onclose={() => (kbMenu = null)} onpick={(ts) => { kbMenu = null; mail.snooze(ts, ids); }} />
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

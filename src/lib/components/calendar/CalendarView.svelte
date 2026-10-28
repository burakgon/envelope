<script lang="ts">
  import { onMount } from 'svelte';
  import type { Occurrence } from '../../api';
  import { cal, addDays, startOfDay } from '../../stores/calendar.svelte';
  import type { CalView } from '../../stores/calendar.svelte';
  import { ui } from '../../stores/ui.svelte';
  import WindowControls from '../WindowControls.svelte';
  import TimeGrid from './TimeGrid.svelte';
  import MonthGrid from './MonthGrid.svelte';
  import Agenda from './Agenda.svelte';
  import EventPopover from './EventPopover.svelte';
  import { ChevronLeft, ChevronRight, PanelLeft, Plus, Spinner } from '../../icons';

  let popover = $state.raw<{ key: string; rect: DOMRect } | null>(null);
  // Follows reloads (e.g. after an RSVP) and goes away with the event.
  const popOcc = $derived(popover ? (cal.occurrences.find((x) => x.key === popover!.key) ?? null) : null);

  onMount(() => {
    cal.init();
  });

  const days = $derived.by(() => {
    const [from, to] = cal.range;
    const n = Math.round((to - from) / 86_400_000);
    return Array.from({ length: n }, (_, i) => addDays(startOfDay(from), i));
  });

  const views: { id: CalView; label: string; key: string }[] = [
    { id: 'day', label: 'Day', key: 'D' },
    { id: 'week', label: 'Week', key: 'W' },
    { id: 'month', label: 'Month', key: 'M' },
    { id: 'agenda', label: 'Agenda', key: 'A' },
  ];

  function open(o: Occurrence, rect: DOMRect) {
    popover = { key: o.key, rect };
  }

  let wheelLock = 0;
  function onWheel(e: WheelEvent) {
    // Horizontal swipes page through time in the week and day views.
    if (cal.view !== 'week' && cal.view !== 'day') return;
    if (Math.abs(e.deltaX) < 30 || Math.abs(e.deltaX) < Math.abs(e.deltaY) * 2) return;
    const now = Date.now();
    if (now - wheelLock < 600) return;
    wheelLock = now;
    cal.step(e.deltaX > 0 ? 1 : -1);
  }
</script>

<section class="calendar" onwheel={onWheel}>
  <header data-tauri-drag-region>
    {#if ui.sidebarCollapsed}
      <button class="icon-btn" title="Show sidebar" onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
    {/if}
    <button class="btn sm today" onclick={() => cal.today()} title="Today (T)">Today</button>
    <div class="nav">
      <button class="icon-btn" aria-label="Previous" title="Previous (K)" onclick={() => cal.step(-1)}><ChevronLeft size={18} /></button>
      <button class="icon-btn" aria-label="Next" title="Next (J)" onclick={() => cal.step(1)}><ChevronRight size={18} /></button>
    </div>
    <h1 data-tauri-drag-region>{cal.title}</h1>
    {#if cal.loading}<span class="spin"><Spinner size={14} /></span>{/if}
    <span class="grow" data-tauri-drag-region></span>
    <div class="seg" role="tablist">
      {#each views as v (v.id)}
        <button role="tab" aria-selected={cal.view === v.id} class:on={cal.view === v.id} title="{v.label} ({v.key})" onclick={() => cal.setView(v.id)}>{v.label}</button>
      {/each}
    </div>
    <button class="btn primary sm new" onclick={() => cal.newEvent()} title="New event (C)"><Plus size={15} strokeWidth={2.4} />Event</button>
    <WindowControls />
  </header>

  {#key cal.view}
    <div class="body">
      {#if cal.view === 'day' || cal.view === 'week'}
        <TimeGrid {days} onopen={open} />
      {:else if cal.view === 'month'}
        <MonthGrid onopen={open} />
      {:else}
        <Agenda onopen={open} />
      {/if}
    </div>
  {/key}
</section>

{#if popover && popOcc}
  {#key popover.key}
    <EventPopover occurrence={popOcc} rect={popover.rect} onclose={() => (popover = null)} />
  {/key}
{/if}

<style>
  .calendar {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--header-h);
    padding: 0 10px 0 18px;
    flex-shrink: 0;
  }
  .today {
    padding: 0 14px;
  }
  .nav {
    display: flex;
  }
  h1 {
    margin: 0 0 0 4px;
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
    white-space: nowrap;
  }
  .spin {
    display: grid;
    color: var(--text-3);
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .seg {
    display: flex;
    padding: 3px;
    gap: 2px;
    border-radius: 10px;
    background: var(--bg-active);
  }
  .seg button {
    height: 26px;
    padding: 0 12px;
    border-radius: 7px;
    font-weight: 500;
    font-size: var(--text-sm);
    color: var(--text-2);
    transition: background var(--dur), color var(--dur), box-shadow var(--dur);
  }
  .seg button:hover {
    color: var(--text);
  }
  .seg button.on {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .new {
    gap: 5px;
    margin-left: 4px;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    animation: fade-in var(--dur-fast);
  }
</style>

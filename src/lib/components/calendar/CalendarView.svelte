<script lang="ts">
  import { onMount } from 'svelte';
  import type { Occurrence } from '../../api';
  import { cal, addDays, startOfDay } from '../../stores/calendar.svelte';
  import { BIRTHDAYS } from '../../utils/birthdays';
  import { isoWeek, weekOfRow } from '../../utils/weeks';
  import type { CalView } from '../../stores/calendar.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { t, type MessageKey } from '../../i18n';
  import WindowControls from '../WindowControls.svelte';
  import TimeGrid from './TimeGrid.svelte';
  import MonthGrid from './MonthGrid.svelte';
  import Agenda from './Agenda.svelte';
  import EventPopover from './EventPopover.svelte';
  import BirthdayPopover from './BirthdayPopover.svelte';
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

  const views: { id: CalView; label: MessageKey; tooltip: MessageKey; key: string }[] = [
    { id: 'day', label: 'calendar.view.day', tooltip: 'calendar.view.dayTooltip', key: 'D' },
    { id: 'workweek', label: 'calendar.view.workWeek', tooltip: 'calendar.view.workWeekTooltip', key: 'X' },
    { id: 'week', label: 'calendar.view.week', tooltip: 'calendar.view.weekTooltip', key: 'W' },
    { id: 'month', label: 'calendar.view.month', tooltip: 'calendar.view.monthTooltip', key: 'M' },
    { id: 'agenda', label: 'calendar.view.agenda', tooltip: 'calendar.view.agendaTooltip', key: 'A' },
  ];

  function open(o: Occurrence, rect: DOMRect) {
    popover = { key: o.key, rect };
  }

  const timeGrid = $derived(cal.view === 'day' || cal.view === 'workweek' || cal.view === 'week');
  /** The number of the week on screen, in the day and week views when week numbers are on. */
  const week = $derived(
    ui.settings.calendarWeekNumbers && timeGrid ? (cal.view === 'day' ? isoWeek(cal.range[0]) : weekOfRow(new Date(cal.range[0]))) : null,
  );

  let wheelLock = 0;
  function onWheel(e: WheelEvent) {
    // Horizontal swipes page through time in the week and day views.
    if (!timeGrid) return;
    if (Math.abs(e.deltaX) < 30 || Math.abs(e.deltaX) < Math.abs(e.deltaY) * 2) return;
    const now = Date.now();
    if (now - wheelLock < 600) return;
    wheelLock = now;
    cal.step(e.deltaX > 0 ? 1 : -1);
  }
</script>

<section class="calendar" onwheel={onWheel}>
  <header data-tauri-drag-region>
    {#if ui.sidebarHidden}
      <button class="icon-btn" title={t('calendar.header.showSidebar')} onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
    {/if}
    <button class="btn sm today" onclick={() => cal.today()} title={t('calendar.header.todayTooltip', { key: 'T' })}>{t('time.today')}</button>
    <div class="nav">
      <button class="icon-btn" aria-label={t('calendar.header.previousPeriod')} title={t('calendar.header.previousPeriodTooltip', { key: 'K' })} onclick={() => cal.step(-1)}><ChevronLeft size={18} /></button>
      <button class="icon-btn" aria-label={t('calendar.header.nextPeriod')} title={t('calendar.header.nextPeriodTooltip', { key: 'J' })} onclick={() => cal.step(1)}><ChevronRight size={18} /></button>
    </div>
    <h1 data-tauri-drag-region title={cal.title}>{cal.title}</h1>
    {#if week != null}<span class="week-no">{t('calendar.weekNumber', { week })}</span>{/if}
    {#if cal.loading}<span class="spin"><Spinner size={14} /></span>{/if}
    <span class="grow" data-tauri-drag-region></span>
    <div class="seg" role="tablist">
      {#each views as v (v.id)}
        <button role="tab" aria-selected={cal.view === v.id} class:on={cal.view === v.id} title={t(v.tooltip, { key: v.key })} onclick={() => cal.setView(v.id)}>{t(v.label)}</button>
      {/each}
    </div>
    <button class="btn primary sm new" onclick={() => cal.newEvent()} title={t('calendar.header.newEventTooltip', { key: 'C' })}><Plus size={15} strokeWidth={2.4} />{t('calendar.header.newEventButton')}</button>
    <WindowControls />
  </header>

  {#key cal.view}
    <div class="body">
      {#if timeGrid}
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
    {#if popOcc.calendarId === BIRTHDAYS}
      <BirthdayPopover occurrence={popOcc} rect={popover.rect} onclose={() => (popover = null)} />
    {:else}
      <EventPopover occurrence={popOcc} rect={popover.rect} onclose={() => (popover = null)} />
    {/if}
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
  .week-no {
    flex-shrink: 0;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--text-2);
    background: var(--bg-active);
  }
  h1 {
    min-width: 0;
    margin: 0 0 0 4px;
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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
    flex-shrink: 0;
    padding: 3px;
    gap: 2px;
    border-radius: 10px;
    background: var(--bg-active);
  }
  .seg button {
    height: 26px;
    padding: 0 12px;
    border-radius: 7px;
    white-space: nowrap;
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

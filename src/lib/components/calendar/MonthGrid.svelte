<script lang="ts">
  import type { Occurrence, Task } from '../../api';
  import { cal, DAY, addDays, sameDay, span, startOfWeek } from '../../stores/calendar.svelte';
  import { weekOfRow } from '../../utils/weeks';
  import { ui } from '../../stores/ui.svelte';
  import { t } from '../../i18n';
  import { dateFormat, timeOf } from '../../utils/format';
  import { dayDate, dueDay } from '../../utils/due';
  import TaskChip from './TaskChip.svelte';

  let { onopen }: { onopen: (o: Occurrence, rect: DOMRect) => void } = $props();

  const first = $derived(startOfWeek(new Date(new Date(cal.focus).getFullYear(), new Date(cal.focus).getMonth(), 1), cal.weekStart));
  const days = $derived(Array.from({ length: 42 }, (_, i) => addDays(first, i)));
  const month = $derived(new Date(cal.focus).getMonth());
  const weekdays = $derived(days.slice(0, 7).map((d) => dateFormat({ weekday: 'short' }).format(d)));

  const byDay = $derived.by(() => {
    const map = new Map<number, Occurrence[]>();
    for (const o of cal.occurrences) {
      const s = span(o);
      for (let day = new Date(s.start).setHours(0, 0, 0, 0); day < s.end; day = addDays(day, 1).getTime()) {
        const list = map.get(day) ?? [];
        list.push(o);
        map.set(day, list);
      }
    }
    for (const list of map.values()) list.sort((a, b) => Number(b.allDay) - Number(a.allDay) || a.start - b.start);
    return map;
  });

  const tasksByDay = $derived.by(() => {
    const map = new Map<number, Task[]>();
    for (const task of cal.dueTasks) {
      const key = dueDay(task);
      if (!key) continue;
      const day = dayDate(key).getTime();
      map.set(day, [...(map.get(day) ?? []), task]);
    }
    return map;
  });

  type Item = { o: Occurrence; task?: undefined } | { task: Task; o?: undefined };
  /** A day's all-day events, then its tasks, then the rest. */
  function itemsOf(day: number): Item[] {
    const list = byDay.get(day) ?? [];
    return [
      ...list.filter((o) => o.allDay).map((o) => ({ o })),
      ...(tasksByDay.get(day) ?? []).map((task) => ({ task })),
      ...list.filter((o) => !o.allDay).map((o) => ({ o })),
    ];
  }

  let dragging = $state<Occurrence | null>(null);
  let over = $state<number | null>(null);

  function drop(day: Date) {
    const o = dragging;
    dragging = null;
    over = null;
    if (!o) return;
    const s = span(o);
    const fromDay = new Date(s.start).setHours(0, 0, 0, 0);
    const delta = Math.round((day.getTime() - fromDay) / DAY);
    if (delta === 0) return;
    cal.move(o, addDays(o.allDay ? s.start : o.start, delta).getTime(), addDays(o.allDay ? s.end : o.end, delta).getTime());
  }
</script>

<div class="month">
  <div class="wd">
    {#each weekdays as w, i (i)}<div>{w}</div>{/each}
  </div>
  <div class="cells">
    {#each days as d, i (d.getTime())}
      {@const list = itemsOf(d.getTime())}
      {@const today = sameDay(d, Date.now())}
      <div
        class="cell"
        class:other={d.getMonth() !== month}
        class:over={over === d.getTime()}
        role="gridcell"
        tabindex="-1"
        ondblclick={() => cal.newEvent(new Date(d.getFullYear(), d.getMonth(), d.getDate(), 9).getTime())}
        ondragover={(e) => { e.preventDefault(); over = d.getTime(); }}
        ondragleave={() => (over = null)}
        ondrop={() => drop(d)}
      >
        <button class="num" class:today onclick={() => { cal.view = 'day'; cal.goto(d.getTime()); }}>{d.getDate()}</button>
        {#if ui.settings.calendarWeekNumbers && i % 7 === 0}
          {@const week = weekOfRow(d)}
          <span class="wk" title={t('calendar.weekNumber', { week })}>{week}</span>
        {/if}
        <div class="events">
          {#each list.slice(0, 4) as it (it.o ? it.o.key : `task-${it.task.id}`)}
            {#if it.task}
              <TaskChip task={it.task} variant="month" />
            {:else}
              {@const o = it.o}
              <button
                class="item"
                class:allday={o.allDay}
                class:declined={o.myStatus === 'declined'}
                style:--c={o.color}
                draggable={!o.readOnly}
                ondragstart={() => (dragging = o)}
                onclick={(e) => onopen(o, (e.currentTarget as HTMLElement).getBoundingClientRect())}
              >
                {#if !o.allDay}<span class="dot"></span><span class="t">{timeOf(o.start)}</span>{/if}
                <span class="title">{o.title}</span>
              </button>
            {/if}
          {/each}
          {#if list.length > 4}
            {@const more = t('calendar.monthGrid.more', { count: list.length - 4 })}
            <button class="more" title={more} onclick={() => { cal.view = 'day'; cal.goto(d.getTime()); }}>{more}</button>
          {/if}
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .month {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 0 10px 10px;
  }
  .wd {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    padding: 4px 0 6px;
  }
  .wd div {
    min-width: 0;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-3);
    padding-left: 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cells {
    flex: 1;
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    grid-template-rows: repeat(6, 1fr);
    border-top: 1px solid var(--border);
    border-left: 1px solid var(--border);
    border-radius: 12px;
    overflow: hidden;
    min-height: 0;
  }
  .cell {
    position: relative;
    border-right: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    padding: 4px;
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .cell.other {
    background: var(--bg-sunken);
  }
  .cell.other .num {
    color: var(--text-4);
  }
  .cell.over {
    background: var(--accent-softer);
  }
  /* The week's number, in the corner of its first day. */
  .wk {
    position: absolute;
    top: 8px;
    right: 8px;
    font-size: 10.5px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
  }
  .num {
    align-self: flex-start;
    min-width: 24px;
    height: 24px;
    padding: 0 6px;
    border-radius: 12px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .num:hover {
    background: var(--bg-hover);
  }
  .num.today {
    background: var(--accent-solid);
    color: var(--accent-fg);
  }
  .events {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-height: 0;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 21px;
    padding: 0 6px;
    border-radius: 6px;
    font-size: 11.5px;
    text-align: left;
    color: var(--text);
    min-width: 0;
  }
  .item:hover {
    background: var(--bg-hover);
  }
  .item.allday {
    font-weight: 600;
    color: color-mix(in oklab, var(--c) 55%, var(--text));
    background: color-mix(in oklab, var(--c) 18%, var(--bg));
  }
  .item.declined {
    opacity: 0.5;
    text-decoration: line-through;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
    flex-shrink: 0;
  }
  .t {
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }
  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .more {
    min-width: 0;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-3);
    text-align: left;
    padding: 0 6px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .more:hover {
    color: var(--text);
  }
</style>

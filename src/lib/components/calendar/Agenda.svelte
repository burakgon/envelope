<script lang="ts">
  import type { Occurrence, Task } from '../../api';
  import { cal, sameDay, span, startOfDay } from '../../stores/calendar.svelte';
  import { MapPin, Video } from '../../icons';
  import { api } from '../../api';
  import { t } from '../../i18n';
  import { dateFormat, timeOf } from '../../utils/format';
  import TaskChip from './TaskChip.svelte';

  let { onopen }: { onopen: (o: Occurrence, rect: DOMRect) => void } = $props();

  // Each day: its all-day events, then the tasks due, then the rest.
  const groups = $derived.by(() => {
    const map = new Map<number, { events: Occurrence[]; tasks: Task[] }>();
    const [from, to] = cal.range;
    const at = (day: number) => {
      let g = map.get(day);
      if (!g) map.set(day, (g = { events: [], tasks: [] }));
      return g;
    };
    for (const o of cal.occurrences) {
      const s = span(o);
      const day = Math.max(startOfDay(s.start).getTime(), from);
      if (day >= to || s.end <= from) continue;
      at(day).events.push(o);
    }
    for (const task of cal.dueTasks) {
      const day = task.dueDate ? new Date(`${task.dueDate}T00:00:00`).getTime() : NaN;
      if (day >= from && day < to) at(day).tasks.push(task);
    }
    return [...map.entries()].sort((a, b) => a[0] - b[0]);
  });
</script>

{#snippet event(o: Occurrence)}
  <button class="row" style:--c={o.color} onclick={(e) => onopen(o, (e.currentTarget as HTMLElement).getBoundingClientRect())}>
    <span class="bar"></span>
    <span class="when">{o.allDay ? t('calendar.agenda.allDay') : t('calendar.event.timeRange', { start: timeOf(o.start), end: timeOf(o.end) })}</span>
    <span class="title" class:declined={o.myStatus === 'declined'}>{o.title}</span>
    {#if o.location}<span class="loc"><MapPin size={12} />{o.location}</span>{/if}
    {#if o.conferenceUrl}
      <span class="join" role="button" tabindex="0" onclick={(e) => { e.stopPropagation(); api.openUrl(o.conferenceUrl!); }} onkeydown={() => {}}><Video size={13} />{t('calendar.agenda.join')}</span>
    {/if}
  </button>
{/snippet}

<div class="agenda">
  {#each groups as [day, g] (day)}
    <section>
      <div class="day" class:today={sameDay(day, Date.now())}>
        <span class="n">{new Date(day).getDate()}</span>
        <span class="w">{dateFormat({ weekday: 'long' }).format(day)}<small>{dateFormat({ month: 'long', year: 'numeric' }).format(day)}</small></span>
      </div>
      <div class="items">
        {#each g.events.filter((o) => o.allDay) as o (o.key)}{@render event(o)}{/each}
        {#each g.tasks as task (task.id)}<TaskChip {task} variant="agenda" />{/each}
        {#each g.events.filter((o) => !o.allDay) as o (o.key)}{@render event(o)}{/each}
      </div>
    </section>
  {:else}
    <div class="empty">{t('calendar.agenda.empty')}</div>
  {/each}
</div>

<style>
  .agenda {
    flex: 1;
    overflow-y: auto;
    padding: 4px 24px 40px;
  }
  section {
    display: flex;
    gap: 18px;
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
  }
  .day {
    width: 150px;
    flex-shrink: 0;
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .n {
    font-size: 26px;
    font-weight: 700;
    line-height: 1;
    width: 40px;
    text-align: center;
  }
  .today .n {
    color: var(--accent-text);
  }
  .w {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow-wrap: break-word;
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .w small {
    font-weight: 500;
    color: var(--text-3);
  }
  .items {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 38px;
    padding: 0 10px;
    border-radius: 10px;
    text-align: left;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .bar {
    width: 4px;
    height: 22px;
    border-radius: 2px;
    background: var(--c);
    flex-shrink: 0;
  }
  .when {
    width: 140px;
    flex-shrink: 0;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
    font-size: var(--text-sm);
  }
  .title {
    flex: 1;
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title.declined {
    text-decoration: line-through;
    opacity: 0.6;
  }
  .loc {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--text-3);
    font-size: var(--text-sm);
    max-width: 220px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .join {
    display: inline-flex;
    align-items: center;
    flex-shrink: 0;
    white-space: nowrap;
    gap: 5px;
    height: 26px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .empty {
    padding: 60px;
    text-align: center;
    color: var(--text-3);
  }
</style>

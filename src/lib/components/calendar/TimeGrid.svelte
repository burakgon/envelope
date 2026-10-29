<script lang="ts">
  import { onMount } from 'svelte';
  import type { Occurrence, Task } from '../../api';
  import { cal, DAY, addDays, sameDay, span, startOfDay } from '../../stores/calendar.svelte';
  import { t } from '../../i18n';
  import { dateFormat, timeOf } from '../../utils/format';
  import EventChip from './EventChip.svelte';
  import TaskChip from './TaskChip.svelte';

  let { days, onopen }: { days: Date[]; onopen: (o: Occurrence, rect: DOMRect) => void } = $props();

  const HOUR = 52;
  const SNAP = 15;
  const pxPerMin = HOUR / 60;

  let scroller: HTMLDivElement | undefined = $state();
  let cols: HTMLDivElement | undefined = $state();
  let now = $state(Date.now());

  const dayStarts = $derived(days.map((d) => startOfDay(d).getTime()));
  const rangeStart = $derived(dayStarts[0]);
  const rangeEnd = $derived(addDays(days[days.length - 1], 1).getTime());

  // All-day and multi-day events go into the lane above the grid, and tasks on their day.
  type LaneItem = ({ o: Occurrence; task?: undefined } | { task: Task; o?: undefined }) & { first: number; last: number };
  const laneItems = $derived.by(() => {
    const items: LaneItem[] = cal.occurrences
      .filter((o) => {
        const s = span(o);
        return (o.allDay || s.end - s.start >= DAY) && s.end > rangeStart && s.start < rangeEnd;
      })
      .map((o) => {
        const s = span(o);
        const first = Math.max(0, dayStarts.findIndex((d) => d + DAY > s.start));
        let last = dayStarts.findIndex((d) => d >= s.end) - 1;
        if (last < 0) last = days.length - 1;
        return { o, first, last: Math.max(first, last) };
      })
      .sort((a, b) => a.first - b.first || b.last - b.first - (a.last - a.first));
    for (const task of cal.dueTasks) {
      const day = task.dueDate ? dayStarts.indexOf(new Date(`${task.dueDate}T00:00:00`).getTime()) : -1;
      if (day >= 0) items.push({ task, first: day, last: day });
    }
    const rows: number[][] = [];
    const placed: (LaneItem & { row: number })[] = [];
    for (const it of items) {
      let row = rows.findIndex((r) => r.every((c) => c < it.first || c > it.last));
      if (row < 0) {
        rows.push([]);
        row = rows.length - 1;
      }
      for (let c = it.first; c <= it.last; c++) rows[row].push(c);
      placed.push({ ...it, row });
    }
    return { placed, rows: rows.length };
  });
  // A day column has room for more of them than a week's.
  const laneMax = $derived(days.length === 1 ? 8 : 4);
  /** Rows shown; when some do not fit, the last row says how many more each day has. */
  const laneShown = $derived(laneItems.rows > laneMax ? laneMax - 1 : laneMax);
  const laneMore = $derived(days.map((_, i) => laneItems.placed.filter((p) => p.row >= laneShown && p.first <= i && i <= p.last).length));

  interface Placed {
    o: Occurrence;
    top: number;
    height: number;
    col: number;
    cols: number;
  }

  function layoutDay(dayStart: number): Placed[] {
    const dayEnd = dayStart + DAY;
    const evs = cal.occurrences
      .filter((o) => !o.allDay && o.end - o.start < DAY && o.end > dayStart && o.start < dayEnd)
      .map((o) => ({ o, s: Math.max(o.start, dayStart), e: Math.min(Math.max(o.end, o.start + 15 * 60_000), dayEnd) }))
      .sort((a, b) => a.s - b.s || b.e - a.e);
    const out: Placed[] = [];
    let cluster: { item: (typeof evs)[number]; col: number }[] = [];
    let columns: number[] = [];
    let clusterEnd = 0;
    const flush = () => {
      for (const c of cluster) {
        out.push({
          o: c.item.o,
          top: ((c.item.s - dayStart) / 60_000) * pxPerMin,
          height: Math.max(20, ((c.item.e - c.item.s) / 60_000) * pxPerMin - 2),
          col: c.col,
          cols: columns.length,
        });
      }
      cluster = [];
      columns = [];
    };
    for (const it of evs) {
      if (cluster.length && it.s >= clusterEnd) flush();
      let col = columns.findIndex((end) => end <= it.s);
      if (col < 0) {
        columns.push(it.e);
        col = columns.length - 1;
      } else columns[col] = it.e;
      cluster.push({ item: it, col });
      clusterEnd = Math.max(clusterEnd, it.e);
    }
    flush();
    return out;
  }

  const laidOut = $derived(dayStarts.map((d) => layoutDay(d)));

  onMount(() => {
    const first = cal.occurrences.filter((o) => !o.allDay).map((o) => new Date(o.start).getHours());
    const todayVisible = dayStarts.some((d) => sameDay(d, Date.now()));
    const h = todayVisible ? Math.max(0, new Date().getHours() - 2) : Math.min(8, ...first, 8);
    if (scroller) scroller.scrollTop = h * HOUR;
    const timer = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(timer);
  });

  // ----- pointer interactions -----------------------------------------------------

  type Drag =
    | { kind: 'create'; day: number; a: number; b: number }
    | { kind: 'move'; o: Occurrence; dayOffset: number; minutes: number; startX: number; startY: number; moved: boolean; locked: boolean; rect: DOMRect }
    | { kind: 'resize'; o: Occurrence; minutes: number; startY: number };
  let drag = $state<Drag | null>(null);

  function minutesAt(clientY: number): number {
    const r = cols!.getBoundingClientRect();
    const m = (clientY - r.top) / pxPerMin;
    return Math.max(0, Math.min(24 * 60, Math.round(m / SNAP) * SNAP));
  }

  function dayAt(clientX: number): number {
    const r = cols!.getBoundingClientRect();
    const w = r.width / days.length;
    return Math.max(0, Math.min(days.length - 1, Math.floor((clientX - r.left) / w)));
  }

  function onGridDown(e: PointerEvent, day: number) {
    if (e.button !== 0 || (e.target as HTMLElement).closest('.ev')) return;
    const m = minutesAt(e.clientY);
    drag = { kind: 'create', day, a: m, b: m + 30 };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onEventDown(e: PointerEvent, o: Occurrence, resize = false) {
    if (e.button !== 0 || (resize && o.readOnly)) return;
    e.stopPropagation();
    const target = (e.currentTarget as HTMLElement).closest('.ev') as HTMLElement;
    if (resize) drag = { kind: 'resize', o, minutes: 0, startY: e.clientY };
    else
      drag = {
        kind: 'move', o, dayOffset: 0, minutes: 0, startX: e.clientX, startY: e.clientY, moved: false, locked: o.readOnly,
        rect: target.getBoundingClientRect(),
      };
    cols?.setPointerCapture(e.pointerId);
  }

  function onMove(e: PointerEvent) {
    if (!drag) return;
    if (drag.kind === 'create') {
      const m = minutesAt(e.clientY);
      drag.b = m === drag.a ? drag.a + SNAP : m;
    } else if (drag.kind === 'move') {
      const dy = e.clientY - drag.startY;
      const dx = e.clientX - drag.startX;
      if (drag.locked || (!drag.moved && Math.hypot(dx, dy) < 5)) return;
      drag.moved = true;
      drag.minutes = Math.round(dy / pxPerMin / SNAP) * SNAP;
      const start = drag.o.start;
      const startDay = dayStarts.findIndex((d) => d <= start && start < d + DAY);
      drag.dayOffset = dayAt(e.clientX) - Math.max(0, startDay);
    } else {
      drag.minutes = Math.round((e.clientY - drag.startY) / pxPerMin / SNAP) * SNAP;
    }
  }

  function onUp() {
    const d = drag;
    drag = null;
    if (!d) return;
    if (d.kind === 'create') {
      const [a, b] = d.a <= d.b ? [d.a, d.b] : [d.b, d.a];
      const base = dayStarts[d.day];
      cal.newEvent(base + a * 60_000, base + Math.max(b, a + SNAP) * 60_000);
    } else if (d.kind === 'move') {
      if (!d.moved) return onopen(d.o, d.rect);
      const delta = d.minutes * 60_000;
      const ns = addDays(d.o.start + delta, d.dayOffset).getTime();
      cal.move(d.o, ns, ns + (d.o.end - d.o.start));
    } else {
      const ne = Math.max(d.o.start + SNAP * 60_000, d.o.end + d.minutes * 60_000);
      if (ne !== d.o.end) cal.move(d.o, d.o.start, ne);
    }
  }

  function preview(p: Placed, dayIndex: number): { top: number; height: number; day: number } {
    const d = drag;
    if (d && d.kind === 'move' && d.moved && d.o.key === p.o.key) {
      return { top: p.top + d.minutes * pxPerMin, height: p.height, day: dayIndex + d.dayOffset };
    }
    if (d && d.kind === 'resize' && d.o.key === p.o.key) {
      return { top: p.top, height: Math.max(SNAP * pxPerMin, p.height + d.minutes * pxPerMin), day: dayIndex };
    }
    return { top: p.top, height: p.height, day: dayIndex };
  }

  const hours = Array.from({ length: 24 }, (_, i) => i);
  const fmtHour = (h: number) => dateFormat({ hour: 'numeric' }).format(new Date(2000, 0, 1, h));
  const fmtTime = (m: number) => timeOf(new Date(2000, 0, 1, Math.floor(m / 60), m % 60));
</script>

<div class="grid-wrap" style:--n={days.length}>
  <div class="head">
    <div class="gutter"></div>
    {#each days as d, i (i)}
      {@const today = sameDay(d, now)}
      <button class="dayhead" class:today onclick={() => { cal.view = 'day'; cal.goto(d.getTime()); }}>
        <span class="dow">{dateFormat({ weekday: 'short' }).format(d)}</span>
        <span class="num">{d.getDate()}</span>
      </button>
    {/each}
  </div>

  {#if laneItems.rows > 0}
    <div class="lane" style:height="{Math.min(laneItems.rows, laneMax) * 26 + 6}px">
      <div class="gutter lane-label" title={t('calendar.timeGrid.allDay')}>{t('calendar.timeGrid.allDay')}</div>
      <div class="lane-body">
        {#each laneItems.placed.filter((p) => p.row < laneShown) as p (p.o ? p.o.key : `task-${p.task.id}`)}
          {#if p.task}
            <div class="lane-task" style:left="calc({p.first} * 100% / var(--n) + 2px)" style:width="calc(100% / var(--n) - 4px)" style:top="{p.row * 26 + 3}px">
              <TaskChip task={p.task} variant="lane" />
            </div>
          {:else}
            {@const o = p.o}
            <button
              class="lane-ev"
              style:left="calc({p.first} * 100% / var(--n) + 2px)"
              style:width="calc({p.last - p.first + 1} * 100% / var(--n) - 4px)"
              style:top="{p.row * 26 + 3}px"
              style:--c={o.color}
              onclick={(e) => onopen(o, (e.currentTarget as HTMLElement).getBoundingClientRect())}
            >
              {o.title}
            </button>
          {/if}
        {/each}
        {#each laneMore as more, i (i)}
          {#if more > 0}
            {@const label = t('calendar.monthGrid.more', { count: more })}
            <button
              class="lane-more"
              style:left="calc({i} * 100% / var(--n) + 2px)"
              style:width="calc(100% / var(--n) - 4px)"
              style:top="{laneShown * 26 + 3}px"
              title={label}
              onclick={() => { cal.view = 'day'; cal.goto(days[i].getTime()); }}>{label}</button>
          {/if}
        {/each}
      </div>
    </div>
  {/if}

  <div class="scroll" bind:this={scroller}>
    <div class="body" style:height="{24 * HOUR}px">
      <div class="gutter hours">
        {#each hours as h (h)}
          <div class="hour-label" style:top="{h * HOUR}px">{h === 0 ? '' : fmtHour(h)}</div>
        {/each}
      </div>
      <div class="cols" bind:this={cols} onpointermove={onMove} onpointerup={onUp} role="grid" tabindex="-1">
        {#each hours as h (h)}
          <div class="hline" style:top="{h * HOUR}px"></div>
          <div class="hline half" style:top="{h * HOUR + HOUR / 2}px"></div>
        {/each}
        {#each days as d, i (i)}
          {@const today = sameDay(d, now)}
          <div
            class="col"
            class:today
            class:weekend={d.getDay() === 0 || d.getDay() === 6}
            style:left="calc({i} * 100% / var(--n))"
            onpointerdown={(e) => onGridDown(e, i)}
            role="gridcell"
            tabindex="-1"
          ></div>
          {#if today}
            {@const mins = (now - startOfDay(now).getTime()) / 60_000}
            <div class="now" style:left="calc({i} * 100% / var(--n))" style:top="{mins * pxPerMin}px"><span></span></div>
          {/if}
        {/each}
        {#each laidOut as dayItems, i (i)}
          {#each dayItems as p (p.o.key)}
            {@const pv = preview(p, i)}
            <div
              class="ev"
              class:dragging={drag && drag.kind !== 'create' && drag.o.key === p.o.key}
              style:left="calc({pv.day} * 100% / var(--n) + {p.col} * (100% / var(--n) - 8px) / {p.cols} + 2px)"
              style:width="calc((100% / var(--n) - 8px) / {p.cols} - 2px + {p.col === p.cols - 1 ? 4 : 0}px)"
              style:top="{pv.top}px"
              style:height="{pv.height}px"
            >
              <EventChip occurrence={p.o} compact={pv.height < 34} onpointerdown={(e) => onEventDown(e, p.o)} />
              {#if !p.o.readOnly}
                <div class="resize" role="separator" aria-orientation="horizontal" onpointerdown={(e) => onEventDown(e, p.o, true)}></div>
              {/if}
            </div>
          {/each}
        {/each}
        {#if drag?.kind === 'create'}
          {@const a = Math.min(drag.a, drag.b)}
          {@const b = Math.max(drag.a, drag.b)}
          <div class="selection" style:left="calc({drag.day} * 100% / var(--n) + 2px)" style:top="{a * pxPerMin}px" style:height="{Math.max(SNAP, b - a) * pxPerMin}px">
            <span>{t('calendar.event.timeRange', { start: fmtTime(a), end: fmtTime(Math.max(b, a + SNAP)) })}</span>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .grid-wrap {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    --gutter: 58px;
  }
  .head {
    display: flex;
    border-bottom: 1px solid var(--border);
    padding-right: 10px;
  }
  .gutter {
    width: var(--gutter);
    flex-shrink: 0;
  }
  .dayhead {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: 6px 0 8px;
    color: var(--text-2);
  }
  .dow {
    max-width: 100%;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    font-size: 18px;
    font-weight: 600;
    color: var(--text);
    transition: background var(--dur-fast);
  }
  .dayhead:hover .num {
    background: var(--bg-hover);
  }
  .dayhead.today .num {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .dayhead.today .dow {
    color: var(--accent-text);
  }
  .lane {
    display: flex;
    border-bottom: 1px solid var(--border);
    padding-right: 10px;
    flex-shrink: 0;
  }
  .lane-label {
    font-size: 10.5px;
    line-height: 1.2;
    color: var(--text-3);
    text-align: right;
    padding: 6px 8px 0 0;
    overflow: hidden;
    overflow-wrap: break-word;
    hyphens: auto;
  }
  .lane-body {
    position: relative;
    flex: 1;
  }
  .lane-ev {
    position: absolute;
    height: 23px;
    padding: 0 8px;
    border-radius: 6px;
    text-align: left;
    font-size: 12px;
    font-weight: 600;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: color-mix(in oklab, var(--c) 72%, var(--text));
    background: color-mix(in oklab, var(--c) 18%, var(--bg));
    border-left: 3px solid var(--c);
  }
  .lane-ev:hover {
    background: color-mix(in oklab, var(--c) 26%, var(--bg));
  }
  .lane-task {
    position: absolute;
  }
  .lane-more {
    position: absolute;
    height: 23px;
    padding: 0 8px;
    border-radius: 6px;
    text-align: left;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-3);
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .lane-more:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
  }
  .body {
    position: relative;
    display: flex;
  }
  .hours {
    position: relative;
  }
  .hour-label {
    position: absolute;
    right: 10px;
    transform: translateY(-7px);
    font-size: 11px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .cols {
    position: relative;
    flex: 1;
    margin-right: 10px;
    touch-action: none;
  }
  .hline {
    position: absolute;
    left: 0;
    right: 0;
    border-top: 1px solid var(--border);
    pointer-events: none;
  }
  .hline.half {
    border-top-style: dashed;
    opacity: 0.45;
  }
  .col {
    position: absolute;
    top: 0;
    bottom: 0;
    width: calc(100% / var(--n));
    border-left: 1px solid var(--border);
  }
  .col.weekend {
    background: color-mix(in oklab, var(--bg-sunken) 60%, transparent);
  }
  .col.today {
    background: color-mix(in oklab, var(--accent) 3%, transparent);
  }
  .now {
    position: absolute;
    width: calc(100% / var(--n));
    height: 2px;
    background: var(--red);
    z-index: 4;
    pointer-events: none;
  }
  .now span {
    position: absolute;
    left: -5px;
    top: -4px;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--red);
  }
  .ev {
    position: absolute;
    z-index: 2;
    transition: top 120ms var(--ease), height 120ms var(--ease);
  }
  .ev.dragging {
    z-index: 5;
    opacity: 0.9;
    transition: none;
    filter: drop-shadow(0 8px 16px rgb(0 0 0 / 0.2));
  }
  .resize {
    position: absolute;
    left: 0;
    right: 0;
    bottom: -2px;
    height: 7px;
    cursor: ns-resize;
  }
  .selection {
    position: absolute;
    width: calc(100% / var(--n) - 6px);
    border-radius: 8px;
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1.5px var(--accent);
    z-index: 3;
    pointer-events: none;
    padding: 4px 8px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--accent-text);
  }
</style>

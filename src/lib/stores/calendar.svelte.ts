import { api } from '../api';
import type { CalendarInfo, EditScope, EventInput, Occurrence } from '../api';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';

export type CalView = 'day' | 'week' | 'month' | 'agenda';

export const DAY = 86_400_000;

export function startOfDay(t: number | Date): Date {
  const d = new Date(t);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

export function addDays(t: number | Date, n: number): Date {
  const d = new Date(t);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes());
}

export function startOfWeek(t: number | Date, weekStart: number): Date {
  const d = startOfDay(t);
  const diff = (d.getDay() - weekStart + 7) % 7;
  return addDays(d, -diff);
}

export function sameDay(a: number | Date, b: number | Date): boolean {
  const x = new Date(a);
  const y = new Date(b);
  return x.getFullYear() === y.getFullYear() && x.getMonth() === y.getMonth() && x.getDate() === y.getDate();
}

export function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** Local start/end of an occurrence (all-day dates are interpreted locally). */
export function span(o: Occurrence): { start: number; end: number } {
  if (o.allDay && o.startDate) {
    const s = new Date(`${o.startDate}T00:00:00`).getTime();
    const e = o.endDate ? new Date(`${o.endDate}T00:00:00`).getTime() : s + DAY;
    return { start: s, end: Math.max(e, s + DAY) };
  }
  return { start: o.start, end: Math.max(o.end, o.start + 15 * 60_000) };
}

export interface EditorState {
  eventId: number | null;
  recurrenceId: Occurrence['recurrenceId'];
  isRecurring: boolean;
  input: EventInput;
}

class Cal {
  calendars = $state<CalendarInfo[]>([]);
  view = $state<CalView>('week');
  focus = $state(startOfDay(new Date()).getTime());
  occurrences = $state<Occurrence[]>([]);
  loading = $state(false);
  editor = $state<EditorState | null>(null);
  scopeAsk = $state<{ title: string; resolve: (s: EditScope | null) => void } | null>(null);
  private seq = 0;
  private started = false;

  weekStart = $derived(ui.settings.weekStart ?? 1);

  range = $derived.by((): [number, number] => {
    const f = new Date(this.focus);
    switch (this.view) {
      case 'day':
        return [startOfDay(f).getTime(), addDays(startOfDay(f), 1).getTime()];
      case 'week': {
        const s = startOfWeek(f, this.weekStart);
        return [s.getTime(), addDays(s, 7).getTime()];
      }
      case 'month': {
        const first = new Date(f.getFullYear(), f.getMonth(), 1);
        const s = startOfWeek(first, this.weekStart);
        return [s.getTime(), addDays(s, 42).getTime()];
      }
      case 'agenda':
        return [startOfDay(f).getTime(), addDays(startOfDay(f), 45).getTime()];
    }
  });

  title = $derived.by(() => {
    const f = new Date(this.focus);
    if (this.view === 'month') return f.toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
    if (this.view === 'day') return f.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric', year: 'numeric' });
    const [a, b] = this.range;
    const s = new Date(a);
    const e = new Date(b - DAY);
    if (this.view === 'agenda') return 'Upcoming';
    if (s.getMonth() === e.getMonth()) return s.toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
    if (s.getFullYear() === e.getFullYear())
      return `${s.toLocaleDateString(undefined, { month: 'short' })} – ${e.toLocaleDateString(undefined, { month: 'short', year: 'numeric' })}`;
    return `${s.toLocaleDateString(undefined, { month: 'short', year: 'numeric' })} – ${e.toLocaleDateString(undefined, { month: 'short', year: 'numeric' })}`;
  });

  calendarById = $derived(new Map(this.calendars.map((c) => [c.id, c])));
  writable = $derived(this.calendars.filter((c) => !c.readOnly));

  async init() {
    if (this.started) return;
    this.started = true;
    api.onEvent((e) => {
      if (e.type === 'calendarChanged' || e.type === 'accountsChanged') {
        this.loadCalendars();
        this.load();
      }
    });
    await this.loadCalendars();
    await this.load();
  }

  async loadCalendars() {
    try {
      this.calendars = await api.calendar.list();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async load() {
    const seq = ++this.seq;
    const [from, to] = this.range;
    this.loading = true;
    try {
      const list = await api.calendar.occurrences(from - DAY, to + DAY);
      if (seq === this.seq) this.occurrences = list;
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      if (seq === this.seq) this.loading = false;
    }
  }

  setView(v: CalView) {
    this.view = v;
    this.load();
  }

  goto(t: number) {
    this.focus = startOfDay(t).getTime();
    this.load();
  }

  today() {
    this.goto(Date.now());
  }

  step(dir: 1 | -1) {
    const f = new Date(this.focus);
    switch (this.view) {
      case 'day':
        this.goto(addDays(f, dir).getTime());
        break;
      case 'week':
        this.goto(addDays(f, 7 * dir).getTime());
        break;
      case 'month':
        this.goto(new Date(f.getFullYear(), f.getMonth() + dir, 1).getTime());
        break;
      case 'agenda':
        this.goto(addDays(f, 30 * dir).getTime());
        break;
    }
  }

  defaultCalendar(): CalendarInfo | undefined {
    return this.writable.find((c) => c.isDefault && c.visible) ?? this.writable.find((c) => c.visible) ?? this.writable[0];
  }

  /** Opens the editor for a new event. */
  newEvent(start?: number, end?: number, allDay = false) {
    const cal = this.defaultCalendar();
    if (!cal) {
      toasts.error('There is no calendar you can add events to.');
      return;
    }
    const s = start ?? (() => {
      const d = new Date();
      d.setMinutes(d.getMinutes() < 30 ? 30 : 60, 0, 0);
      return d.getTime();
    })();
    const e = end ?? s + 60 * 60_000;
    this.editor = {
      eventId: null,
      recurrenceId: null,
      isRecurring: false,
      input: {
        calendarId: cal.id,
        summary: '',
        description: '',
        location: '',
        allDay,
        start: s,
        end: e,
        startDate: allDay ? ymd(new Date(s)) : null,
        endDate: allDay ? ymd(new Date(e)) : null,
        rrule: null,
        attendees: [],
        alarms: [10],
        transparent: false,
        addConference: false,
      },
    };
  }

  async edit(o: Occurrence) {
    try {
      const d = await api.calendar.detail(o.eventId, o.recurrenceId);
      const sp = span(o);
      this.editor = {
        eventId: o.eventId,
        recurrenceId: o.recurrenceId,
        isRecurring: o.isRecurring,
        input: {
          calendarId: d.calendar.id,
          summary: d.event.summary,
          description: d.event.description ?? '',
          location: d.event.location ?? '',
          allDay: o.allDay,
          start: sp.start,
          end: o.allDay ? sp.end : o.end,
          startDate: o.startDate,
          endDate: o.endDate,
          rrule: d.event.rrule,
          attendees: d.event.attendees.map((a) => ({ email: a.email, name: a.name })),
          alarms: d.event.alarms.map((a) => a.minutesBefore),
          color: d.event.color,
          transparent: d.event.transparency === 'transparent',
          addConference: false,
        },
      };
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Asks which occurrences an edit applies to (for recurring events). */
  askScope(title: string): Promise<EditScope | null> {
    return new Promise((resolve) => {
      this.scopeAsk = {
        title,
        resolve: (s) => {
          this.scopeAsk = null;
          resolve(s);
        },
      };
    });
  }

  /** Optimistically moves an occurrence (and, for a series, the affected siblings), then saves. */
  async move(o: Occurrence, start: number, end: number) {
    const prev = this.occurrences;
    const base = o.allDay ? span(o) : { start: o.start, end: o.end };
    const ds = start - base.start;
    const de = end - base.end;
    const shift = (x: Occurrence): Occurrence => {
      if (!x.allDay) return { ...x, start: x.start + ds, end: x.end + de };
      const sp = span(x);
      return {
        ...x,
        start: x.start + ds,
        end: x.end + de,
        startDate: ymd(addDays(sp.start, Math.round(ds / DAY))),
        endDate: ymd(addDays(sp.end, Math.round(de / DAY))),
      };
    };
    // Keep the event where it was dropped while asking about the rest of the series.
    this.occurrences = prev.map((x) => (x.key === o.key ? shift(x) : x));
    let scope: EditScope = 'all';
    if (o.isRecurring) {
      const s = await this.askScope('Move recurring event');
      if (!s) {
        this.occurrences = prev;
        return;
      }
      scope = s;
      if (scope !== 'this') {
        this.occurrences = prev.map((x) =>
          x.key === o.key || (x.eventId === o.eventId && (scope === 'all' || x.start >= o.start)) ? shift(x) : x,
        );
      }
    }
    try {
      await api.calendar.move(o.eventId, o.recurrenceId, scope, start, end);
      this.load();
    } catch (e) {
      this.occurrences = prev;
      toasts.error(`Couldn't move “${o.title}”: ${errorMessage(e)}`);
    }
  }

  async remove(o: Occurrence) {
    let scope: EditScope = 'all';
    if (o.isRecurring) {
      const s = await this.askScope('Delete recurring event');
      if (!s) return;
      scope = s;
    }
    const prev = this.occurrences;
    this.occurrences = this.occurrences.filter((x) =>
      scope === 'this' ? x.key !== o.key : scope === 'following' ? !(x.eventId === o.eventId && x.start >= o.start) : x.eventId !== o.eventId,
    );
    try {
      await api.calendar.remove(o.eventId, o.recurrenceId, scope);
      toasts.show('Event deleted');
      this.load();
    } catch (e) {
      this.occurrences = prev;
      toasts.error(errorMessage(e));
    }
  }

  async save(state: EditorState, scope: EditScope = 'all') {
    try {
      if (state.eventId == null) {
        await api.calendar.create($state.snapshot(state.input) as EventInput);
        toasts.show('Event created', { kind: 'success' });
      } else {
        await api.calendar.update(state.eventId, state.recurrenceId, scope, $state.snapshot(state.input) as EventInput);
        toasts.show('Event saved', { kind: 'success' });
      }
      this.editor = null;
      this.load();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async setVisible(c: CalendarInfo, visible: boolean) {
    c.visible = visible;
    await api.calendar.setVisible(c.id, visible);
    this.load();
  }
}

export const cal = new Cal();

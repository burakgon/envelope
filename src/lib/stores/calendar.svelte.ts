import { api } from '../api';
import type { CalendarInfo, EditScope, EventInput, ExtractedEvent, Occurrence } from '../api';
import { t } from '../i18n';
import { dateFormat } from '../utils/format';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';

export type CalView = 'day' | 'week' | 'month' | 'agenda';

export const DAY = 86_400_000;

export function startOfDay(at: number | Date): Date {
  const d = new Date(at);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

export function addDays(at: number | Date, n: number): Date {
  const d = new Date(at);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes());
}

export function startOfWeek(at: number | Date, weekStart: number): Date {
  const d = startOfDay(at);
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
  /** The address being subscribed to (from a `webcal:` link or the sidebar); null when no dialog is open. */
  subscribing = $state<string | null>(null);
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
    if (this.view === 'month') return dateFormat({ month: 'long', year: 'numeric' }).format(f);
    if (this.view === 'day') return dateFormat({ weekday: 'long', month: 'long', day: 'numeric', year: 'numeric' }).format(f);
    const [a, b] = this.range;
    const s = new Date(a);
    const e = new Date(b - DAY);
    if (this.view === 'agenda') return t('calendar.header.upcoming');
    if (s.getMonth() === e.getMonth()) return dateFormat({ month: 'long', year: 'numeric' }).format(s);
    // A week across two months: "Oct – Nov 2026", "Dec 2026 – Jan 2027", in the interface language.
    return dateFormat({ month: 'short', year: 'numeric' }).formatRange(s, e);
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
      if (seq === this.seq) this.occurrences = ui.settings.showDeclined ? list : list.filter((o) => o.myStatus !== 'declined');
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

  goto(at: number) {
    this.focus = startOfDay(at).getTime();
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
    const chosen = ui.settings.defaultCalendarId;
    return (
      this.writable.find((c) => c.id === chosen) ??
      this.writable.find((c) => c.isDefault && c.visible) ??
      this.writable.find((c) => c.visible) ??
      this.writable[0]
    );
  }

  /** Opens the editor for a new event. */
  async newEvent(start?: number, end?: number, allDay = false) {
    if (!this.calendars.length) await this.loadCalendars();
    const cal = this.defaultCalendar();
    if (!cal) {
      toasts.error(t('calendar.toast.noWritableCalendar'));
      return;
    }
    const s = start ?? (() => {
      const d = new Date();
      d.setMinutes(d.getMinutes() < 30 ? 30 : 60, 0, 0);
      return d.getTime();
    })();
    const e = end ?? s + ui.settings.defaultEventMinutes * 60_000;
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
        alarms: ui.settings.defaultAlarm >= 0 ? [ui.settings.defaultAlarm] : [],
        transparent: false,
        addConference: false,
      },
    };
  }

  /** Opens the editor prefilled with an event the AI found in a message. */
  async fromExtracted(e: ExtractedEvent) {
    if (!this.calendars.length) await this.loadCalendars();
    const c = this.defaultCalendar();
    if (!c) {
      toasts.error(t('calendar.toast.noWritableCalendar'));
      return;
    }
    let start: number;
    let end: number;
    let startDate: string | null = null;
    let endDate: string | null = null;
    if (e.all_day) {
      startDate = e.start.slice(0, 10);
      const last = (e.end ?? e.start).slice(0, 10);
      endDate = ymd(addDays(new Date(`${last < startDate ? startDate : last}T00:00:00`), 1));
      start = new Date(`${startDate}T00:00:00`).getTime();
      end = new Date(`${endDate}T00:00:00`).getTime();
    } else {
      start = new Date(e.start).getTime();
      end = e.end ? new Date(e.end).getTime() : NaN;
      if (!(end > start)) end = start + ui.settings.defaultEventMinutes * 60_000;
    }
    if (Number.isNaN(start)) {
      toasts.error(t('calendar.toast.unreadableDate'));
      return;
    }
    this.editor = {
      eventId: null,
      recurrenceId: null,
      isRecurring: false,
      input: {
        calendarId: c.id,
        summary: e.title,
        description: e.description ?? '',
        location: e.location ?? '',
        allDay: e.all_day,
        start,
        end,
        startDate,
        endDate,
        rrule: null,
        attendees: [],
        alarms: ui.settings.defaultAlarm >= 0 ? [ui.settings.defaultAlarm] : [],
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
      const s = await this.askScope(t('calendar.scope.moveTitle'));
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
      toasts.error(t('calendar.toast.moveFailed', { title: o.title, error: errorMessage(e) }));
    }
  }

  async remove(o: Occurrence) {
    let scope: EditScope = 'all';
    if (o.isRecurring) {
      const s = await this.askScope(t('calendar.scope.deleteTitle'));
      if (!s) return;
      scope = s;
    }
    const prev = this.occurrences;
    this.occurrences = this.occurrences.filter((x) =>
      scope === 'this' ? x.key !== o.key : scope === 'following' ? !(x.eventId === o.eventId && x.start >= o.start) : x.eventId !== o.eventId,
    );
    try {
      await api.calendar.remove(o.eventId, o.recurrenceId, scope);
      toasts.show(t('calendar.toast.deleted'));
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
        toasts.show(t('calendar.toast.created'), { kind: 'success' });
      } else {
        await api.calendar.update(state.eventId, state.recurrenceId, scope, $state.snapshot(state.input) as EventInput);
        toasts.show(t('calendar.toast.saved'), { kind: 'success' });
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

// In-memory calendar for the demo.

import type * as T from './types';

const DAY = 86_400_000;

function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

const today = startOfDay(new Date());
const monday = new Date(today.getTime() - ((today.getDay() + 6) % 7) * DAY);

export const calendars: T.CalendarInfo[] = [
  { id: 1, accountId: 1, kind: 'google', name: 'Work', color: '#6d5efc', readOnly: false, isDefault: true, visible: true },
  { id: 2, accountId: 2, kind: 'caldav', name: 'Personal', color: '#0ea5e9', readOnly: false, isDefault: true, visible: true },
  { id: 3, accountId: 2, kind: 'caldav', name: 'Family', color: '#f59e0b', readOnly: false, isDefault: false, visible: true },
  { id: 4, accountId: 1, kind: 'google', name: 'Holidays', color: '#10b981', readOnly: true, isDefault: false, visible: true },
];

interface Series {
  id: number;
  calendarId: number;
  title: string;
  /** Days of week (0 = Sunday) for weekly series; empty = single event. */
  days: number[];
  interval?: number;
  start: Date;
  minutes: number;
  allDay?: boolean;
  location?: string;
  conference?: string;
  attendees?: T.CalAttendee[];
  myStatus?: T.PartStat;
  description?: string;
  exdates: Set<number>;
  overrides: Map<number, { start: number; end: number }>;
}

const team: T.CalAttendee[] = [
  { email: 'priya@lumen.dev', name: 'Priya Raman', partstat: 'accepted', role: 'chair', rsvp: false },
  { email: 'daniel@lumen.dev', name: 'Daniel Okafor', partstat: 'accepted', role: 'required', rsvp: true },
  { email: 'alex@lumen.dev', name: 'Alex Morgan', partstat: 'accepted', role: 'required', rsvp: true },
  { email: 'sofia@lumen.dev', name: 'Sofia Martínez', partstat: 'tentative', role: 'required', rsvp: true },
];

let seq = 1;
const at = (base: Date, days: number, h: number, m = 0) => new Date(base.getFullYear(), base.getMonth(), base.getDate() + days, h, m);
function S(p: Partial<Series> & Pick<Series, 'calendarId' | 'title' | 'start' | 'minutes'>): Series {
  return { id: seq++, days: [], exdates: new Set(), overrides: new Map(), ...p };
}

export const series: Series[] = [
  S({ calendarId: 1, title: 'Daily standup', days: [1, 2, 3, 4, 5], start: at(monday, 0, 9, 30), minutes: 15, conference: 'https://meet.lumen.dev/standup', attendees: team }),
  S({ calendarId: 1, title: 'Product sync', days: [4], start: at(monday, 3, 10), minutes: 45, location: 'Room Galata', conference: 'https://meet.lumen.dev/product-sync', attendees: team, myStatus: 'needsAction', description: 'Roadmap, launches and blockers.' }),
  S({ calendarId: 1, title: '1:1 with Daniel', days: [2], interval: 2, start: at(monday, 1, 14), minutes: 30, location: 'Room Bosphorus' }),
  S({ calendarId: 1, title: 'Focus time', days: [3], start: at(monday, 2, 13), minutes: 180 }),
  S({ calendarId: 1, title: 'Design review: onboarding', start: at(today, 0, 15), minutes: 60, location: 'Figtree board', attendees: [team[3], { email: 'maya@northwind.design', name: 'Maya Chen', partstat: 'accepted', role: 'required', rsvp: true }] }),
  S({ calendarId: 1, title: 'Roadmap final review', start: at(today, 1, 10), minutes: 45, conference: 'https://meet.lumen.dev/roadmap-review', attendees: team, myStatus: 'needsAction' }),
  S({ calendarId: 1, title: 'Interview: Senior Rust Engineer', start: at(today, 2, 11), minutes: 60, conference: 'https://meet.lumen.dev/interview-7' }),
  S({ calendarId: 1, title: 'Q4 planning', start: at(monday, 4, 11), minutes: 90, location: 'Big room', attendees: team }),
  S({ calendarId: 1, title: 'Lunch & learn: Rust async', start: at(monday, 2, 12), minutes: 60, location: 'Kitchen' }),
  S({ calendarId: 1, title: 'Team offsite — Cappadocia', start: at(today, 24, 0), minutes: 3 * 24 * 60, allDay: true, location: 'Göreme' }),
  S({ calendarId: 2, title: 'Gym', days: [1, 3, 5], start: at(monday, 0, 18, 30), minutes: 60, location: 'Pulse Fitness' }),
  S({ calendarId: 2, title: 'Coffee with Elif ☕', start: at(monday, 5, 11), minutes: 90, location: 'Moda Cd. No: 12, Kadıköy' }),
  S({ calendarId: 2, title: 'Apartment viewing', start: at(monday, 5, 14), minutes: 60, location: 'Rosenthaler Str., Berlin' }),
  S({ calendarId: 2, title: 'Flight IST → BER', start: at(today, 16, 9, 40), minutes: 135, location: 'Istanbul Airport (IST)', description: 'Seat 12A · Booking SKY7Q2' }),
  S({ calendarId: 2, title: 'Dentist', start: at(today, 6, 16), minutes: 45, location: 'Nişantaşı' }),
  S({ calendarId: 3, title: 'Dinner at Mom’s', start: at(monday, 6, 19), minutes: 120 }),
  S({ calendarId: 3, title: 'Mom’s birthday 🎂', start: at(today, 9, 0), minutes: 24 * 60, allDay: true }),
  S({ calendarId: 4, title: 'Republic Day', start: new Date(today.getFullYear(), 9, 29), minutes: 24 * 60, allDay: true }),
];

// Recurring series began a while ago, so earlier weeks aren't empty.
for (const s of series) {
  if (s.days.length) s.start = at(s.start, -70, s.start.getHours(), s.start.getMinutes());
}

function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

export function occurrences(from: number, to: number): T.Occurrence[] {
  const out: T.Occurrence[] = [];
  for (const s of series) {
    const cal = calendars.find((c) => c.id === s.calendarId)!;
    if (!cal.visible) continue;
    const push = (start: number, isRecurring: boolean) => {
      if (s.exdates.has(start)) return;
      const ov = s.overrides.get(start);
      const st = ov?.start ?? start;
      const en = ov?.end ?? st + s.minutes * 60_000;
      if (en <= from || st >= to) return;
      out.push({
        key: `${s.id}:${start}`, eventId: s.id, calendarId: s.calendarId, uid: `demo-${s.id}`, title: s.title, start: st, end: en,
        allDay: !!s.allDay, startDate: s.allDay ? ymd(new Date(st)) : null, endDate: s.allDay ? ymd(new Date(en)) : null,
        location: s.location ?? null, color: cal.color, status: 'confirmed', transparent: false, isRecurring,
        recurrenceId: isRecurring ? { kind: 'utc', at: new Date(start).toISOString() } : null,
        myStatus: s.myStatus ?? (s.attendees ? 'accepted' : null), hasAttendees: !!s.attendees, conferenceUrl: s.conference ?? null, readOnly: cal.readOnly,
      });
    };
    if (s.days.length === 0) {
      push(s.start.getTime(), false);
      continue;
    }
    const interval = s.interval ?? 1;
    for (let t = startOfDay(new Date(Math.max(from, s.start.getTime()))).getTime(); t < to; t += DAY) {
      const d = new Date(t);
      if (!s.days.includes(d.getDay())) continue;
      const weeks = Math.floor((t - startOfDay(s.start).getTime()) / (7 * DAY));
      if (weeks % interval !== 0) continue;
      push(new Date(d.getFullYear(), d.getMonth(), d.getDate(), s.start.getHours(), s.start.getMinutes()).getTime(), true);
    }
  }
  return out.sort((a, b) => a.start - b.start);
}

export function detail(eventId: number, recurrenceId: T.EventTime | null): T.EventDetail {
  const s = series.find((x) => x.id === eventId)!;
  const cal = calendars.find((c) => c.id === s.calendarId)!;
  const start = recurrenceId && recurrenceId.kind === 'utc' ? Date.parse(recurrenceId.at) : s.start.getTime();
  const occ = occurrences(start - 1, start + 1).find((o) => o.eventId === eventId) ?? occurrences(start - DAY * 400, start + DAY * 400).find((o) => o.eventId === eventId)!;
  return {
    occurrence: occ,
    calendar: cal,
    organizerIsMe: !s.attendees || s.attendees[0]?.email === 'alex@lumen.dev',
    myEmail: 'alex@lumen.dev',
    event: {
      uid: `demo-${s.id}`, recurrenceId, summary: s.title, description: s.description ?? null, descriptionHtml: null, location: s.location ?? null, url: null,
      start: { kind: 'utc', at: new Date(occ.start).toISOString() }, end: { kind: 'utc', at: new Date(occ.end).toISOString() },
      rrule: s.days.length ? `FREQ=WEEKLY;${(s.interval ?? 1) > 1 ? `INTERVAL=${s.interval};` : ''}BYDAY=${s.days.map((d) => ['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'][d]).join(',')}` : null,
      status: 'confirmed', transparency: 'opaque',
      organizer: s.attendees ? { email: s.attendees[0].email, name: s.attendees[0].name } : null,
      attendees: s.attendees ?? [], alarms: [{ action: 'display', minutesBefore: 10, description: null }], color: null, conferenceUrl: s.conference ?? null,
    },
  };
}

export function create(input: T.EventInput): number {
  const start = input.allDay ? new Date(`${input.startDate}T00:00:00`) : new Date(input.start);
  const end = input.allDay ? new Date(`${input.endDate}T00:00:00`).getTime() : input.end;
  const s = S({ calendarId: input.calendarId, title: input.summary || '(No title)', start, minutes: Math.max(15, (end - start.getTime()) / 60_000), allDay: input.allDay, location: input.location ?? undefined, description: input.description ?? undefined });
  series.push(s);
  return s.id;
}

export function update(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, input: T.EventInput) {
  const s = series.find((x) => x.id === eventId);
  if (!s) return;
  s.title = input.summary || s.title;
  s.location = input.location ?? undefined;
  s.description = input.description ?? undefined;
  s.calendarId = input.calendarId;
  move(eventId, recurrenceId, scope, input.start, input.end);
}

export function move(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, start: number, end: number) {
  const s = series.find((x) => x.id === eventId);
  if (!s) return;
  const rid = recurrenceId && recurrenceId.kind === 'utc' ? Date.parse(recurrenceId.at) : null;
  if (rid != null && scope === 'this' && s.days.length) {
    s.overrides.set(rid, { start, end });
  } else if (rid != null && s.days.length) {
    const delta = start - rid;
    s.start = new Date(s.start.getTime() + delta);
    s.minutes = (end - start) / 60_000;
  } else {
    s.start = new Date(start);
    s.minutes = (end - start) / 60_000;
  }
}

export function remove(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope) {
  const i = series.findIndex((x) => x.id === eventId);
  if (i < 0) return;
  const s = series[i];
  const rid = recurrenceId && recurrenceId.kind === 'utc' ? Date.parse(recurrenceId.at) : null;
  if (rid != null && scope === 'this' && s.days.length) s.exdates.add(rid);
  else series.splice(i, 1);
}

export function respond(eventId: number, status: T.PartStat) {
  const s = series.find((x) => x.id === eventId);
  if (s) s.myStatus = status;
}

export function invite(): T.InviteInfo {
  const s = series.find((x) => x.title === 'Product sync')!;
  const next = occurrences(Date.now() - DAY, Date.now() + 8 * DAY).find((o) => o.eventId === s.id)!;
  return {
    method: 'REQUEST', uid: `demo-${s.id}`, summary: s.title, start: next.start, end: next.end, allDay: false,
    location: s.location ?? null, description: s.description ?? null, organizer: { name: 'Daniel Okafor', email: 'daniel@lumen.dev' },
    attendees: team.map((a) => ({ name: a.name, email: a.email })), myStatus: s.myStatus ?? 'needsAction', eventId: s.id, conferenceUrl: s.conference ?? null, recurring: true,
  };
}

/** Made-up but stable busy times for other people in the demo; the user's own come from the calendar. */
export function freeBusy(emails: string[], from: number, to: number, own: Set<string>): T.Availability[] {
  const day = 86_400_000;
  return emails.map((email) => {
    if (own.has(email.toLowerCase())) {
      const busy = occurrences(from, to)
        .filter((o) => !o.transparent && !o.allDay)
        .map((o) => ({ start: o.start, end: o.end, tentative: o.myStatus === 'tentative' || o.myStatus === 'needsAction' }));
      return { email, busy };
    }
    let seed = [...email].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
    const busy: T.Busy[] = [];
    const first = new Date(from);
    first.setHours(0, 0, 0, 0);
    for (let d = first.getTime(); d < to; d += day) {
      const weekday = new Date(d).getDay();
      if (weekday === 0 || weekday === 6) continue;
      for (let k = 0; k < 3; k++) {
        seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
        const start = d + (9 + (seed % 8)) * 3_600_000 + ((seed >> 4) % 2) * 1_800_000;
        busy.push({ start, end: start + (seed % 3 === 0 ? 60 : 30) * 60_000, tentative: seed % 5 === 0 });
      }
    }
    return { email, busy: busy.filter((b) => b.end > from && b.start < to).sort((a, b) => a.start - b.start) };
  });
}

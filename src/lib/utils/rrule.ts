// Repeat rules of events (RFC 5545 RRULE): as people read them, in the interface language, and
// as the custom repeat editor builds them. Rules are read on the clocks of the event's zone;
// `start` is the date of the first occurrence there ("2026-11-10").

import { intlLocale, t } from '../i18n';
import { dateFormat } from './format';
import { fromWallClock, wallClock } from './timezone';

// RRULE day codes in `Date.getDay()` order; 2 January 2000 was a Sunday.
export const DAY_CODES = ['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'];

export type Freq = 'DAILY' | 'WEEKLY' | 'MONTHLY' | 'YEARLY';

/** A rule's parts by name: `FREQ=WEEKLY;BYDAY=MO,WE` → `{ FREQ: 'WEEKLY', BYDAY: 'MO,WE' }`. */
export function ruleParts(rule: string): Record<string, string> {
  return Object.fromEntries(
    rule
      .replace(/^RRULE:/i, '')
      .split(';')
      .filter((p) => p.includes('='))
      .map((p) => {
        const [key, value = ''] = p.split('=');
        return [key.trim().toUpperCase(), value.trim().toUpperCase()];
      }),
  );
}

function joinParts(parts: Record<string, string>): string {
  return Object.entries(parts)
    .map(([k, v]) => `${k}=${v}`)
    .join(';');
}

/** "2026-11-10" as a date at local midnight, for its weekday, day and month. */
export function dayOf(ymd: string): Date {
  const [y, m, d] = ymd.split('-').map(Number);
  return new Date(y, m - 1, d);
}

function ymdOf(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

function weekdayName(code: string): string {
  const i = DAY_CODES.indexOf(code.slice(-2));
  return i < 0 ? code : dateFormat({ weekday: 'long' }).format(new Date(2000, 0, 2 + i));
}

/** A `BYDAY` entry: `2TU` → { nth: 2, code: 'TU' }, `FR` → { nth: null, code: 'FR' }. */
function byDay(entry: string): { nth: number | null; code: string } | null {
  const m = /^([+-]?\d{1,2})?(SU|MO|TU|WE|TH|FR|SA)$/.exec(entry);
  return m ? { nth: m[1] ? Number(m[1]) : null, code: m[2] } : null;
}

/** Which of its weekday in the month a date is: the 2nd Tuesday, and whether it is the last one. */
export function weekdayInMonth(ymd: string): { nth: number; last: boolean; code: string } {
  const d = dayOf(ymd);
  const days = new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate();
  return { nth: Math.ceil(d.getDate() / 7), last: d.getDate() + 7 > days, code: DAY_CODES[d.getDay()] };
}

/** "the second Tuesday", "the last Friday": `nth` is 1–5, or -1 for the last. */
function nthWeekday(nth: number, code: string): string {
  return t('calendar.recurrence.nthWeekday', { nth: nth === -1 ? 'last' : nth, day: code, weekday: weekdayName(code) });
}

/** A month's name as it reads with a day, "November" (genitive in Polish, "listopada"). */
function monthName(month: number): string {
  const date = new Date(2000, month - 1, 1);
  const name = dateFormat({ month: 'long', day: 'numeric' })
    .formatToParts(date)
    .find((p) => p.type === 'month')?.value;
  // Japanese and Chinese number their months; a month of its own reads "11月".
  return name && !/^\d+$/.test(name) ? name : dateFormat({ month: 'long' }).format(date);
}

const monthDay = (month: number, day: number) => dateFormat({ month: 'long', day: 'numeric' }).format(new Date(2000, month - 1, day));

/** An UNTIL value (`20261231`, `20261231T235959Z` or floating) as a date on the clocks of `zone`. */
export function untilDay(value: string, zone: string): string | null {
  const m = /^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})(Z?))?$/.exec(value);
  if (!m) return null;
  const [, y, mo, d, h, mi, s, z] = m;
  if (h == null || !z) return `${y}-${mo}-${d}`;
  return wallClock(Date.UTC(+y, +mo - 1, +d, +h, +mi, +s), zone).date;
}

/** The rule without its end (COUNT, UNTIL), in words; null when it has no frequency we know. */
function describeRepeat(p: Record<string, string>, count: number, start: string | null): string | null {
  const days = (p.BYDAY ?? '').split(',').filter(Boolean).map(byDay);
  const monthDays = (p.BYMONTHDAY ?? '').split(',').filter(Boolean).map(Number);
  const months = (p.BYMONTH ?? '').split(',').filter(Boolean).map(Number);
  const plain = !p.BYSETPOS && !p.BYYEARDAY && !p.BYWEEKNO;
  const first = start ? dayOf(start) : null;
  switch (p.FREQ) {
    case 'DAILY':
      return t('calendar.recurrence.daily', { count });
    case 'WEEKLY': {
      const codes = days.flatMap((d) => (d ? [d.code] : []));
      if (!codes.length && first) codes.push(DAY_CODES[first.getDay()]);
      if (count === 1 && codes.length === 5 && !codes.includes('SA') && !codes.includes('SU')) return t('calendar.recurrence.everyWeekday');
      if (!codes.length) return t('calendar.recurrence.weekly', { count });
      // "Monday, Wednesday" in English, "Montag und Mittwoch" in German…
      const list = new Intl.ListFormat(intlLocale(), { style: 'narrow', type: 'conjunction' }).format(codes.map(weekdayName));
      return t('calendar.recurrence.weeklyOn', { count, days: list });
    }
    case 'MONTHLY': {
      if (!plain || months.length) break;
      const [day] = days;
      if (days.length === 1 && day?.nth != null && !monthDays.length && (day.nth === -1 || (day.nth >= 1 && day.nth <= 5))) {
        return t('calendar.recurrence.monthlyOnWeekday', { count, which: nthWeekday(day.nth, day.code) });
      }
      if (days.length) break;
      if (monthDays.length === 1 && monthDays[0] === -1) return t('calendar.recurrence.monthlyOnLastDay', { count });
      const n = monthDays.length ? monthDays[0] : first?.getDate();
      if (monthDays.length <= 1 && n != null && n >= 1 && n <= 31) return t('calendar.recurrence.monthlyOnDay', { count, day: n });
      break;
    }
    case 'YEARLY': {
      if (!plain || months.length > 1) break;
      const month = months[0] ?? (first ? first.getMonth() + 1 : null);
      const [day] = days;
      if (days.length === 1 && day?.nth != null && months.length === 1 && !monthDays.length && (day.nth === -1 || (day.nth >= 1 && day.nth <= 5))) {
        return t('calendar.recurrence.yearlyOnWeekday', { count, which: nthWeekday(day.nth, day.code), month: monthName(months[0]) });
      }
      if (days.length || monthDays.length > 1 || month == null) break;
      const n = monthDays.length ? monthDays[0] : first?.getDate();
      if (n != null && n >= 1 && n <= 31) return t('calendar.recurrence.yearlyOn', { count, date: monthDay(month, n) });
      break;
    }
    default:
      return null;
  }
  return p.FREQ === 'MONTHLY' ? t('calendar.recurrence.monthly', { count }) : t('calendar.recurrence.yearly', { count });
}

/**
 * "Weekly on Monday and Wednesday", "Monthly on the second Tuesday, 5 times", "Annually on
 * November 10, until Dec 31, 2030"… for a repeat rule. `start` (the first occurrence's date)
 * names the day of rules that take it from there; `zone` reads a UTC end date.
 */
export function describeRule(rule: string | null | undefined, start: string | null = null, zone = 'UTC'): string | null {
  if (!rule) return null;
  const parts = ruleParts(rule);
  const interval = Number(parts.INTERVAL ?? 1);
  const text = describeRepeat(parts, interval > 1 ? interval : 1, start);
  if (text == null) return t('calendar.recurrence.repeats');
  const times = Number(parts.COUNT);
  if (times > 0) return t('calendar.recurrence.times', { rule: text, count: times });
  const until = parts.UNTIL ? untilDay(parts.UNTIL, zone) : null;
  if (until) {
    const date = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(dayOf(until));
    return t('calendar.recurrence.until', { rule: text, date });
  }
  return text;
}

// ----- The custom repeat editor -------------------------------------------------------

/** What the custom repeat editor shows. */
export interface RepeatChoice {
  freq: Freq;
  interval: number;
  /** Weekly: day codes. */
  days: string[];
  /** Monthly: on the start's day of the month (`day`), or on an ordinal weekday (`2TU`, `-1FR`). */
  monthly: string;
  end: 'never' | 'until' | 'count';
  /** Last day ("2027-03-31"). */
  until: string;
  count: number;
  /** The week start of a rule made elsewhere (Outlook writes `WKST=SU`), kept as it was. */
  wkst: string | null;
}

/** "Monthly on day 10", "… on the second Tuesday", "… on the last Tuesday": what a monthly rule can repeat on, from its start. */
export function monthlyChoices(start: string): { value: string; label: string }[] {
  const { nth, last, code } = weekdayInMonth(start);
  const out = [{ value: 'day', label: t('calendar.recurrence.monthlyOnDay', { count: 1, day: dayOf(start).getDate() }) }];
  if (nth <= 4) out.push({ value: `${nth}${code}`, label: t('calendar.recurrence.monthlyOnWeekday', { count: 1, which: nthWeekday(nth, code) }) });
  if (last) out.push({ value: `-1${code}`, label: t('calendar.recurrence.monthlyOnWeekday', { count: 1, which: nthWeekday(-1, code) }) });
  return out;
}

function addMonths(ymd: string, months: number): string {
  const d = dayOf(ymd);
  return ymdOf(new Date(d.getFullYear(), d.getMonth() + months, d.getDate()));
}

/** What the custom editor starts with for `rule` (or a new rule) of an event starting on `start`. */
export function repeatChoice(rule: string | null, start: string, zone: string): RepeatChoice {
  const p = ruleParts(rule ?? '');
  const freq: Freq = (['DAILY', 'WEEKLY', 'MONTHLY', 'YEARLY'] as const).find((f) => f === p.FREQ) ?? 'WEEKLY';
  const days = (p.BYDAY ?? '').split(',').flatMap((e) => {
    const d = byDay(e);
    return d ? [d] : [];
  });
  const until = p.UNTIL ? untilDay(p.UNTIL, zone) : null;
  const ordinal = days.length === 1 && days[0].nth != null ? `${days[0].nth}${days[0].code}` : null;
  const later = { DAILY: 1, WEEKLY: 3, MONTHLY: 12, YEARLY: 60 }[freq];
  return {
    freq,
    interval: Math.max(1, Number(p.INTERVAL ?? 1) || 1),
    days: freq === 'WEEKLY' && days.length ? days.map((d) => d.code) : [DAY_CODES[dayOf(start).getDay()]],
    monthly: freq === 'MONTHLY' && ordinal && monthlyChoices(start).some((c) => c.value === ordinal) ? ordinal : 'day',
    end: Number(p.COUNT) > 0 ? 'count' : until ? 'until' : 'never',
    until: until ?? addMonths(start, later),
    count: Number(p.COUNT) > 0 ? Number(p.COUNT) : 10,
    wkst: p.WKST && p.WKST !== 'MO' ? p.WKST : null,
  };
}

/** The rule for a choice of the custom editor; an end date is written as a date (see `untilFor`). */
export function buildRule(c: RepeatChoice): string {
  const parts: Record<string, string> = { FREQ: c.freq };
  if (c.interval > 1) parts.INTERVAL = String(Math.round(c.interval));
  if (c.freq === 'WEEKLY') {
    // Monday first, as RFC 5545 weeks start.
    parts.BYDAY = [...c.days].sort((a, b) => ((DAY_CODES.indexOf(a) + 6) % 7) - ((DAY_CODES.indexOf(b) + 6) % 7)).join(',');
  }
  if (c.freq === 'MONTHLY' && c.monthly !== 'day') parts.BYDAY = c.monthly;
  if (c.wkst) parts.WKST = c.wkst;
  if (c.end === 'count') parts.COUNT = String(Math.max(1, Math.round(c.count)));
  if (c.end === 'until') parts.UNTIL = c.until.replace(/-/g, '');
  return joinParts(parts);
}

/**
 * The rule as an event of this kind stores it: RFC 5545 wants the end of a timed series as a
 * UTC time (the end of the last day on the event's clocks) and of an all-day one as a date.
 */
export function untilFor(rule: string, allDay: boolean, zone: string): string {
  const p = ruleParts(rule);
  if (!p.UNTIL) return rule;
  const day = untilDay(p.UNTIL, zone);
  if (!day) return rule;
  const dated = /^\d{8}$/.test(p.UNTIL);
  if (allDay && !dated) p.UNTIL = day.replace(/-/g, '');
  else if (!allDay && dated) {
    const end = new Date(fromWallClock(day, '23:59:59', zone)).toISOString();
    p.UNTIL = `${end.slice(0, 19).replace(/[-:]/g, '')}Z`;
  } else return rule;
  return joinParts(p);
}

/** The first day on or after `start` a weekly rule repeats on (where a series really begins). */
export function firstDay(rule: string, start: string): string {
  const p = ruleParts(rule);
  const days = (p.BYDAY ?? '').split(',').flatMap((e) => {
    const d = byDay(e);
    return d && d.nth == null ? [d.code] : [];
  });
  if (p.FREQ !== 'WEEKLY' || !days.length) return start;
  const d = dayOf(start);
  for (let i = 0; i < 7; i++) {
    const day = new Date(d.getFullYear(), d.getMonth(), d.getDate() + i);
    if (days.includes(DAY_CODES[day.getDay()])) return ymdOf(day);
  }
  return start;
}

/**
 * The rule after the event moved from `from` to `to`: parts that named the old start's
 * weekday, day or month name the new one's ("Weekly on Tuesday" becomes "… on Wednesday").
 */
export function followStart(rule: string, from: string, to: string): string {
  if (!rule || from === to) return rule;
  const p = ruleParts(rule);
  const a = weekdayInMonth(from);
  const b = weekdayInMonth(to);
  const old = dayOf(from);
  const now = dayOf(to);
  if (p.FREQ === 'WEEKLY' && p.BYDAY === a.code) p.BYDAY = b.code;
  else if ((p.FREQ === 'MONTHLY' || p.FREQ === 'YEARLY') && p.BYDAY && !p.BYDAY.includes(',')) {
    const d = byDay(p.BYDAY);
    if (d?.code === a.code && (d.nth === a.nth || (d.nth === -1 && a.last))) {
      // The last one stays the last if it can; a fifth one is always the last.
      const last = d.nth === -1 ? b.last : b.nth > 4;
      p.BYDAY = `${last ? -1 : b.nth}${b.code}`;
    }
  }
  if (p.BYMONTHDAY === String(old.getDate())) p.BYMONTHDAY = String(now.getDate());
  if (p.FREQ === 'YEARLY' && p.BYMONTH === String(old.getMonth() + 1)) p.BYMONTH = String(now.getMonth() + 1);
  return joinParts(p);
}

// ----- What calendars can store -------------------------------------------------------

/** The name people know a Microsoft calendar by. */
function service(kind: string): string {
  return kind === 'ews' ? 'Exchange' : 'Outlook';
}

/**
 * Why calendars of `kind` cannot store `rule`, or null. Exchange (and Outlook through Microsoft
 * Graph) keep repeats as patterns of their own, which have no room for some rules.
 */
export function ruleProblem(rule: string | null, kind: string): string | null {
  if (!rule || (kind !== 'ews' && kind !== 'graph')) return null;
  const p = ruleParts(rule);
  if (kind === 'ews' && p.FREQ === 'YEARLY' && Number(p.INTERVAL ?? 1) > 1) {
    return t('calendar.recurrence.everyFewYears', { service: service(kind) });
  }
  const list = (key: string) => (p[key] ?? '').split(',').filter(Boolean);
  const positions = [1, 2, 3, 4, -1];
  const days = list('BYDAY').map(byDay);
  const ordinals = days.flatMap((d) => (d?.nth != null ? [d.nth] : []));
  const monthDays = list('BYMONTHDAY').map(Number);
  const setPos = list('BYSETPOS').map(Number);
  const fits =
    ['DAILY', 'WEEKLY', 'MONTHLY', 'YEARLY'].includes(p.FREQ) &&
    !['BYWEEKNO', 'BYYEARDAY', 'BYHOUR', 'BYMINUTE', 'BYSECOND'].some((k) => k in p) &&
    !days.includes(null) &&
    list('BYMONTH').length <= 1 &&
    monthDays.length <= 1 &&
    monthDays.every((d) => (d >= 1 && d <= 31) || (kind === 'graph' && d === -1)) &&
    (ordinals.length === 0 || (ordinals.length === 1 && days.length === 1 && positions.includes(ordinals[0]))) &&
    setPos.length <= 1 &&
    setPos.every((n) => positions.includes(n));
  return fits ? null : t('calendar.recurrence.notSupported', { service: service(kind) });
}

/** A word on months a monthly rule's day is missing from: skipped, or moved to the last day by Exchange. */
export function ruleNote(rule: string | null, start: string, kind: string): string | null {
  if (!rule) return null;
  const p = ruleParts(rule);
  if (p.FREQ !== 'MONTHLY' || p.BYDAY || p.BYSETPOS || (p.BYMONTHDAY ?? '').includes(',')) return null;
  const day = p.BYMONTHDAY ? Number(p.BYMONTHDAY) : dayOf(start).getDate();
  if (day < 29) return null;
  return kind === 'ews' || kind === 'graph'
    ? t('calendar.recurrence.shortMonthsLastDay', { day, service: service(kind) })
    : t('calendar.recurrence.shortMonthsSkipped', { day });
}

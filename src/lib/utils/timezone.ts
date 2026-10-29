// IANA time zones: wall-clock times in a zone, and the zones' names, in the interface language.

import { intlLocale } from '../i18n';

const parts = new Map<string, Intl.DateTimeFormat>();

/** A formatter giving the wall-clock fields of an instant in `tz`. */
function fieldsFormat(tz: string): Intl.DateTimeFormat {
  let f = parts.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat('en-US', {
      timeZone: tz,
      hourCycle: 'h23',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
    parts.set(tz, f);
  }
  return f;
}

/** The zone of this computer ("Europe/Istanbul"); UTC when the system does not say. */
export function localZone(): string {
  const tz = new Intl.DateTimeFormat().resolvedOptions().timeZone;
  return tz && tz !== 'Etc/Unknown' ? tz : 'UTC';
}

/** Whether a zone name is one this computer knows. */
export function isZone(tz: string | null | undefined): tz is string {
  if (!tz) return false;
  try {
    fieldsFormat(tz);
    return true;
  } catch {
    return false;
  }
}

/** The same zone under its canonical name ("Asia/Kolkata" and "Asia/Calcutta" are one). */
export function canonicalZone(tz: string): string {
  try {
    return new Intl.DateTimeFormat('en-US', { timeZone: tz }).resolvedOptions().timeZone;
  } catch {
    return tz;
  }
}

export function sameZone(a: string, b: string): boolean {
  return a === b || canonicalZone(a) === canonicalZone(b);
}

/** Year, month (1–12), day, hour, minute and second of `ms` on the clocks of `tz`. */
function fields(ms: number, tz: string): number[] {
  const out = [0, 0, 0, 0, 0, 0];
  const index: Record<string, number> = { year: 0, month: 1, day: 2, hour: 3, minute: 4, second: 5 };
  for (const p of fieldsFormat(tz).formatToParts(ms)) {
    if (p.type in index) out[index[p.type]] = Number(p.value);
  }
  return out;
}

/** How far ahead of UTC the clocks of `tz` are at `ms`, in minutes. */
export function offsetMinutes(ms: number, tz: string): number {
  const [y, mo, d, h, mi, s] = fields(ms, tz);
  return Math.round((Date.UTC(y, mo - 1, d, h, mi, s) - Math.floor(ms / 1000) * 1000) / 60_000);
}

const pad = (n: number) => String(n).padStart(2, '0');

/** `ms` as a date ("2026-11-10") and time ("10:00") on the clocks of `tz`. */
export function wallClock(ms: number, tz: string): { date: string; time: string } {
  const [y, mo, d, h, mi] = fields(ms, tz);
  return { date: `${y}-${pad(mo)}-${pad(d)}`, time: `${pad(h)}:${pad(mi)}` };
}

/**
 * The instant at which the clocks of `tz` show `date` ("2026-11-10") and `time` ("10:00" or
 * "23:59:59"). A time shown twice when the clocks go back is the first one; a time skipped
 * when they go forward is read with the offset from before, so it lands as much later. The
 * engine reads times the same way.
 */
export function fromWallClock(date: string, time: string, tz: string): number {
  const [y, mo, d] = date.split('-').map(Number);
  const [h = 0, mi = 0, s = 0] = time.split(':').map(Number);
  const local = Date.UTC(y, mo - 1, d, h, mi, s);
  // The offsets half a day before and after are those on both sides of a DST change.
  const before = offsetMinutes(local - 43_200_000, tz);
  const after = offsetMinutes(local + 43_200_000, tz);
  for (const offset of [before, after]) {
    const at = local - offset * 60_000;
    if (offsetMinutes(at, tz) === offset) return at;
  }
  return local - before * 60_000;
}

const names = new Map<string, Intl.DateTimeFormat>();

/** A zone's name in the interface language: "Eastern Time" (`longGeneric`), "GMT-5" (`shortOffset`), "EST" (`short`). */
export function zoneName(tz: string, style: 'longGeneric' | 'shortOffset' | 'short' = 'longGeneric', at = Date.now()): string {
  const key = `${intlLocale()} ${tz} ${style}`;
  let f = names.get(key);
  if (!f) {
    try {
      f = new Intl.DateTimeFormat(intlLocale(), { timeZone: tz, timeZoneName: style });
    } catch {
      return tz;
    }
    names.set(key, f);
  }
  return f.formatToParts(at).find((p) => p.type === 'timeZoneName')?.value ?? tz;
}

/** A zone without a place: UTC, a fixed offset ("Etc/GMT+3", three hours behind UTC) or an old alias ("EST5EDT"). */
function placeless(tz: string): boolean {
  return !tz.includes('/') || tz.startsWith('Etc/');
}

/** The city a zone is named after ("America/Argentina/Buenos_Aires" → "Buenos Aires"); a zone without one by its offset. */
export function zoneCity(tz: string): string {
  if (placeless(tz)) return tz === 'UTC' ? 'UTC' : zoneName(tz, 'shortOffset');
  return (tz.split('/').pop() ?? tz).replace(/_/g, ' ');
}

/** "New York (GMT-5)", or "UTC". */
export function zoneLabel(tz: string): string {
  return placeless(tz) ? zoneCity(tz) : `${zoneCity(tz)} (${zoneName(tz, 'shortOffset')})`;
}

export interface ZoneChoice {
  id: string;
  city: string;
  /** "Eastern Time", in the interface language. */
  name: string;
  /** "GMT-5". */
  offset: string;
  minutes: number;
  /** Lower-cased text a search looks in. */
  search: string;
}

let choices: { locale: string; list: ZoneChoice[] } | null = null;

/**
 * Every place this computer knows the zone of, west to east, and UTC; the computer's own zone
 * is there even if it is an alias. Fixed offsets, which some systems list as well, are left out.
 */
export function zoneChoices(): ZoneChoice[] {
  const locale = intlLocale();
  if (choices?.locale === locale) return choices.list;
  const now = Date.now();
  const known = typeof Intl.supportedValuesOf === 'function' ? Intl.supportedValuesOf('timeZone') : [];
  const ids = new Set(known.filter((id) => !placeless(id)));
  ids.add(localZone());
  ids.add('UTC');
  const list = [...ids]
    .filter(isZone)
    .map((id): ZoneChoice => {
      const city = zoneCity(id);
      const name = zoneName(id, 'longGeneric', now);
      const offset = zoneName(id, 'shortOffset', now);
      return { id, city, name, offset, minutes: offsetMinutes(now, id), search: `${city} ${id} ${name} ${offset}`.toLowerCase() };
    })
    .sort((a, b) => a.minutes - b.minutes || a.city.localeCompare(b.city, locale));
  choices = { locale, list };
  return list;
}

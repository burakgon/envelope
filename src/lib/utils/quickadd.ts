// Understands a due date, a time and `!` (important) in a task typed into quick add, in the
// interface language: "Call Maya tomorrow 3pm !", "Maya anrufen morgen 15 Uhr !", "明日15時にマヤに電話 !".
// The words come from Intl (relative days, weekday names, the clock's day periods and hour
// words), so every language Intl knows works without a word list of its own.

import { addDays, atTime, dayKey } from './due';

export interface QuickAdd {
  /** What remains as the title. */
  title: string;
  /** The understood due day, and time of day in minutes after midnight. */
  due: { date: string; minutes: number | null } | null;
  important: boolean;
}

export interface QuickAddOptions {
  locale: string;
  /** Look for a time of day; lists that keep dates only leave times in the title. */
  times: boolean;
  /** Look for `!`; lists without importance leave it in the title. */
  importance: boolean;
  /** What the user dismissed stays in the title. */
  ignore?: { due?: boolean; important?: boolean };
  /** First day of the week, for "next week" (0 = Sunday). */
  weekStart: number;
  /** Words that may come before a date or time ("on", "at") and go with it. */
  fillers: string[];
  /** How the language writes an hour besides what Intl knows, with `{n}` for it: "{n}h", "ore {n}". */
  hours: string[];
  now?: number;
}

interface Span {
  start: number;
  end: number;
}

interface Phrase {
  re: RegExp;
  /** The day it means, from today. */
  resolve: (today: string, weekStart: number) => string;
}

interface Grammar {
  /** Words are separated by spaces (not so in Japanese and Chinese). */
  spaced: boolean;
  phrases: Phrase[];
  times: RegExp[];
  /** The afternoon's day period where it comes before the hour ("午後", "下午", "ÖS"). */
  pm: string | null;
  /** Follows a number when it is an hour ("Uhr", "h", "時"). */
  hourWord: RegExp | null;
  numericDate: { re: RegExp; monthFirst: boolean } | null;
}

const grammars = new Map<string, Grammar>();

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** A literal phrase: either apostrophe, any run of spaces. */
function literal(text: string, spaced: boolean): string {
  return escape(text.normalize('NFC').trim())
    .replace(/['’]/g, "['’]")
    .replace(/\s+/g, spaced ? '\\s+' : '\\s*');
}

/**
 * Whole words where words have spaces, and numbers not cut out of longer ones. The left
 * boundary is a group (1) rather than a lookbehind, for older WebKit.
 */
function bounded(source: string, spaced: boolean, numeric = false): RegExp {
  if (spaced) return new RegExp(`(^|[^\\p{L}\\p{N}])(${source})(?![\\p{L}\\p{N}])`, 'giu');
  return new RegExp(`(^|[^\\p{N}])(${source})${numeric ? '(?!\\p{N})' : ''}`, 'giu');
}

function grammar(locale: string, hours: string[]): Grammar {
  const key = `${locale} ${hours.join('|')}`;
  const cached = grammars.get(key);
  if (cached) return cached;
  const lang = new Intl.Locale(locale).language;
  const spaced = !['ja', 'zh'].includes(lang);
  const phrases: Phrase[] = [];
  const add = (text: string, resolve: Phrase['resolve']) => {
    if (text.trim()) phrases.push({ re: bounded(literal(text, spaced), spaced, /\d/.test(text)), resolve });
  };

  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  for (let n = 0; n <= 14; n++) add(rtf.format(n, 'day'), (today) => addDays(today, n));
  add(rtf.format(1, 'week'), (today, weekStart) => addDays(today, 7 - ((new Date(`${today}T00:00:00`).getDay() - weekStart + 7) % 7)));
  for (let i = 0; i < 7; i++) {
    // 2 January 2000 was a Sunday. A weekday means the next one: a week ahead on that very day.
    const sample = new Date(2000, 0, 2 + i);
    const next = (today: string) => addDays(today, (i - new Date(`${today}T00:00:00`).getDay() + 7) % 7 || 7);
    const long = new Intl.DateTimeFormat(locale, { weekday: 'long' }).format(sample);
    add(long, next);
    // Chinese writes "周五" more than "星期五"; Japanese "金曜" as well as "金曜日".
    if (lang === 'zh') add(new Intl.DateTimeFormat(locale, { weekday: 'short' }).format(sample), next);
    if (lang === 'ja' && long.endsWith('日')) add(long.slice(0, -1), next);
  }

  // The clock as the language writes it: day periods ("PM", "午後") and hour words ("Uhr", "時").
  const parts = (d: Date, o: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(locale, o).formatToParts(d);
  const afternoon = parts(new Date(2000, 0, 1, 15), { hour: 'numeric', hour12: true });
  const morning = parts(new Date(2000, 0, 1, 9), { hour: 'numeric', hour12: true });
  const am = morning.find((p) => p.type === 'dayPeriod')?.value;
  const pm = afternoon.find((p) => p.type === 'dayPeriod')?.value;
  const periodFirst = afternoon.findIndex((p) => p.type === 'dayPeriod') < afternoon.findIndex((p) => p.type === 'hour');
  // Hours written with words: Intl's ("15 Uhr", "15 h", "15時") and the translation's ("{n}h", "ore {n}").
  const h23 = parts(new Date(2000, 0, 1, 15), { hour: 'numeric', hourCycle: 'h23' });
  const word = h23.slice(h23.findIndex((p) => p.type === 'hour') + 1).map((p) => p.value).join('').trim();
  const patterns = [...new Set([...hours, ...(word ? [`{n}${word}`] : [])].map((p) => p.trim()))]
    .map((p) => p.split('{n}'))
    .filter((p) => p.length === 2 && (p[0].trim() || p[1].trim()));
  const after = [...new Set(patterns.map(([, a]) => a.trim()).filter(Boolean))].map((w) => literal(w, spaced));
  const hourWord = after.length ? `(?:${after.join('|')})` : null;
  // Minutes after an hour word: "15h30", "15時30分".
  const minutes = '(?:\\s*(?<m2>\\d{1,2})分?)?';
  const ampm = '(?<ap>[ap])\\.?\\s?m\\.?';
  const times = [
    `(?<h>\\d{1,2})[:.](?<m>\\d{2})(?:\\s*${ampm})?${hourWord ? `(?:\\s*${hourWord})?` : ''}`,
    `(?<h>\\d{1,2})\\s*${ampm}`,
    ...patterns.map(([b, a]) => `${b.trim() ? `${literal(b, spaced)}\\s*` : ''}(?<h>\\d{1,2})(?:[:.](?<m>\\d{2}))?${a.trim() ? `\\s*${literal(a, spaced)}${minutes}` : ''}`),
  ];
  const localPeriod = periodFirst && am && pm && !/^[ap]\.?\s?m/i.test(pm);
  if (localPeriod) {
    times.push(`(?<pre>${literal(am, spaced)}|${literal(pm, spaced)})\\s*(?<h>\\d{1,2})(?:[:.](?<m>\\d{2})|\\s*${hourWord ?? '(?!)'}${minutes})?`);
  }

  // Day and month as numbers, in the language's order and with its separator ("11/6", "6.11.").
  const order = parts(new Date(2000, 10, 6), { month: 'numeric', day: 'numeric' });
  const sep = order.find((p) => p.type === 'literal')?.value.trim();
  const monthFirst = order.findIndex((p) => p.type === 'month') < order.findIndex((p) => p.type === 'day');
  const numericDate = sep?.length === 1 ? { re: bounded(`(?<a>\\d{1,2})${escape(sep)}(?<b>\\d{1,2})${escape(sep)}?`, spaced, true), monthFirst } : null;

  const g: Grammar = {
    spaced,
    phrases,
    times: times.map((s) => bounded(s, spaced, true)),
    pm: localPeriod ? pm!.toLocaleLowerCase(locale) : null,
    hourWord: hourWord ? new RegExp(`^\\s*${hourWord}`, 'iu') : null,
    numericDate,
  };
  grammars.set(key, g);
  return g;
}

interface Hit extends Span {
  groups: Record<string, string | undefined>;
}

/** The next match of a regex from `from`, as a span without its left boundary. */
function next(re: RegExp, text: string, from = 0): Hit | null {
  re.lastIndex = from;
  const m = re.exec(text);
  if (!m) return null;
  const start = m.index + m[1].length;
  return { start, end: start + m[2].length, groups: m.groups ?? {} };
}

const earlier = (a: Span, b: Span | null) => !b || a.start < b.start || (a.start === b.start && a.end > b.end);

/** Minutes after midnight, or null when it is no time of day (25:00, 13pm). */
function minutesOf(g: Record<string, string | undefined>, pm: string | null, locale: string): number | null {
  let h = Number(g.h);
  const min = Number(g.m ?? g.m2 ?? 0);
  if (min > 59) return null;
  const period = g.ap ? g.ap.toLowerCase() === 'p' : g.pre ? g.pre.toLocaleLowerCase(locale) === pm : null;
  if (period != null) {
    if (h < 1 || h > 12) return null;
    h = (h % 12) + (period ? 12 : 0);
  } else if (h > 23) return null;
  return h * 60 + min;
}

export function parseQuickAdd(text: string, o: QuickAddOptions): QuickAdd {
  const g = grammar(o.locale, o.hours);
  const now = o.now ?? Date.now();
  const today = dayKey(now);
  const spans: Span[] = [];
  let date: string | null = null;
  let minutes: number | null = null;

  if (!o.ignore?.due) {
    // The earliest date (the longest of several starting together: "übermorgen" over "morgen").
    let best: (Span & { date: string }) | null = null;
    for (const p of g.phrases) {
      const hit = next(p.re, text);
      if (hit && earlier(hit, best)) best = { ...hit, date: p.resolve(today, o.weekStart) };
    }
    if (g.numericDate) {
      const { re, monthFirst } = g.numericDate;
      for (let hit = next(re, text); hit; hit = next(re, text, hit.end)) {
        const [a, b] = [Number(hit.groups.a), Number(hit.groups.b)];
        const [month, day] = monthFirst ? [a, b] : [b, a];
        const year = new Date(now).getFullYear();
        const valid = (d: Date) => d.getMonth() === month - 1 && d.getDate() === day;
        // "15.30" and "15.11 Uhr" are times.
        if (!valid(new Date(year, month - 1, day)) || g.hourWord?.test(text.slice(hit.end))) continue;
        let d = new Date(year, month - 1, day);
        if (dayKey(d) < today) d = new Date(year + 1, month - 1, day);
        if (valid(d) && earlier(hit, best)) best = { start: hit.start, end: hit.end, date: dayKey(d) };
        break;
      }
    }
    if (best) {
      date = best.date;
      spans.push(best);
    }
    if (o.times) {
      let found: (Span & { minutes: number }) | null = null;
      for (const re of g.times) {
        for (let hit = next(re, text); hit; hit = next(re, text, hit.end)) {
          const m = best && hit.start < best.end && best.start < hit.end ? null : minutesOf(hit.groups, g.pm, o.locale);
          if (m == null) continue;
          if (earlier(hit, found)) found = { start: hit.start, end: hit.end, minutes: m };
          break;
        }
      }
      if (found) {
        minutes = found.minutes;
        spans.push(found);
        // Only a time: today if it is still ahead, else tomorrow.
        if (!date) date = atTime(today, minutes) > now ? today : addDays(today, 1);
      }
    }
  }

  let important = false;
  if (o.importance && !o.ignore?.important) {
    const hit = next(/(^|\s)([!！]+)(?=\s|$)/gu, text);
    if (hit) {
      important = true;
      spans.push(hit);
    }
  }

  // Words like "on" and "at" go with the date or time they introduce (and particles after them in Japanese).
  const fillers = o.fillers.map((f) => f.trim()).filter(Boolean).sort((a, b) => b.length - a.length);
  const cuts = spans.map((s) => ({ ...s }));
  for (const s of cuts) {
    if (/^[!！]/.test(text.slice(s.start, s.end))) continue;
    const before = text.slice(0, s.start);
    for (const f of fillers) {
      const m = new RegExp(`(^|${g.spaced ? '\\s' : ''})(${literal(f, g.spaced)})\\s*$`, 'iu').exec(before);
      if (m) {
        s.start = m.index + m[1].length;
        break;
      }
    }
    if (g.spaced) continue;
    for (const f of fillers) {
      const m = new RegExp(`^\\s*${literal(f, false)}`, 'iu').exec(text.slice(s.end));
      if (m) {
        s.end += m[0].length;
        break;
      }
    }
  }
  cuts.sort((a, b) => a.start - b.start);
  const merged: Span[] = [];
  for (const c of cuts) {
    const last = merged[merged.length - 1];
    if (last && c.start <= last.end) last.end = Math.max(last.end, c.end);
    else merged.push(c);
  }
  let title = text;
  for (const s of [...merged].reverse()) title = `${title.slice(0, s.start)}${g.spaced ? ' ' : ''}${title.slice(s.end)}`;
  title = title.replace(/\s+/g, ' ').replace(/^[\s,;:–—-]+|[\s,;:–—-]+$/g, '');
  // "Tomorrow call Maya" → "Call Maya".
  const lead = text.trimStart().charAt(0);
  if (merged[0]?.start === text.length - text.trimStart().length && lead !== lead.toLocaleLowerCase(o.locale)) {
    title = title.charAt(0).toLocaleUpperCase(o.locale) + title.slice(1);
  }

  return { title, due: date ? { date, minutes } : null, important };
}

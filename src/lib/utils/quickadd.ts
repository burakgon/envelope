// Understands a due date, a time and `!` (important) in a task typed into quick add, in the
// interface language: "Call Maya tomorrow 3pm !", "Maya anrufen morgen 15 Uhr !", "明日15時にマヤに電話 !".
// The words come from Intl (relative days, weekday names, the clock's day periods and hour
// words), so every language Intl knows works without a word list of its own; the translations
// add what Intl does not know ("on", "next Friday", "um 9.05").

import { addDays, atTime, dayDate, dayKey } from './due';

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
  /** Words that come before a time of day ("at", "um", "o"): "9.05" after them is a time, not a date or a number. */
  timeWords?: string[];
  /** More names of the weekdays, Monday first, several for a day split by "/": "środę/środa", "sexta", "venerdi". */
  weekdays?: string[];
  /** The coming weekday as the language says it, with `{day}` for the weekday: "this {day}", "{day} prochain". */
  thisWeekday?: string[];
  /** The weekday in next week: "next {day}", "{day} nächster Woche". */
  nextWeekday?: string[];
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
  /** A shortened weekday name that is an ordinary word too (Portuguese "segunda" is "second"): a date only where one fits. */
  loose?: boolean;
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
  /** Text that ends with a word coming before a time of day ("um", "o", "saat"). */
  timeBefore: RegExp | null;
  /** Numbers have a decimal point, so "12.50" alone is no time. */
  decimalPoint: boolean;
  /** Text that ends, or starts, with a word like "on" or "at", around a loose weekday name. */
  fillerBefore: RegExp | null;
  fillerAfter: RegExp | null;
}

const grammars = new Map<string, Grammar>();

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** A literal phrase: either apostrophe, a hyphen or not ("après demain"), any run of spaces. */
function literal(text: string, spaced: boolean): string {
  return escape(text.normalize('NFC').trim())
    .replace(/['’]/g, "['’]")
    .replace(/-/g, '[-‐\\s]?')
    .replace(/\s+/g, spaced ? '\\s+' : '\\s*');
}

/**
 * Whole words where words have spaces, and numbers not cut out of longer ones. The left
 * boundary is a group (1) rather than a lookbehind, for older WebKit. A `word` is no date
 * when it is a possessive ("today's agenda").
 */
function bounded(source: string, spaced: boolean, numeric = false, word = false): RegExp {
  if (spaced) return new RegExp(`(^|[^\\p{L}\\p{N}])(${source})(?![\\p{L}\\p{N}]${word ? "|['’]\\p{L}" : ''})`, 'giu');
  return new RegExp(`(^|[^\\p{N}])(${source})${numeric ? '(?!\\p{N})' : ''}`, 'giu');
}

const clean = (words: string[] | undefined) => (words ?? []).map((w) => w.trim()).filter(Boolean);

/** Any of several words, the longest first. */
const anyOf = (words: string[], spaced: boolean) => `(?:${[...new Set(words)].sort((a, b) => b.length - a.length).map((w) => literal(w, spaced)).join('|')})`;

/** The weekday of a day key (0 = Sunday). */
const weekday = (key: string) => dayDate(key).getDay();

/** The first day of next week. */
const nextWeek = (today: string, weekStart: number) => addDays(today, 7 - ((weekday(today) - weekStart + 7) % 7));

function grammar(o: QuickAddOptions): Grammar {
  const key = JSON.stringify([o.locale, o.fillers, o.hours, o.timeWords, o.weekdays, o.thisWeekday, o.nextWeekday]);
  const cached = grammars.get(key);
  if (cached) return cached;
  const locale = o.locale;
  const lang = new Intl.Locale(locale).language;
  const spaced = !['ja', 'zh'].includes(lang);
  const phrases: Phrase[] = [];
  const add = (source: string, resolve: Phrase['resolve'], numeric = false, loose = false) =>
    phrases.push({ re: bounded(source, spaced, numeric, true), resolve, loose });
  const say = (text: string, resolve: Phrase['resolve']) => {
    if (text.trim()) add(literal(text, spaced), resolve, /\d/.test(text));
  };

  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  for (let n = 0; n <= 14; n++) say(rtf.format(n, 'day'), (today) => addDays(today, n));
  say(rtf.format(1, 'week'), nextWeek);

  // Weekdays, alone or with words around them ("next {day}", "{day} prochain").
  const around = (pattern: string, day: string) => {
    const [before, after] = pattern.split('{day}');
    const gap = spaced ? '\\s+' : '\\s*';
    return `${before.trim() ? literal(before, spaced) + gap : ''}${day}${after.trim() ? gap + literal(after, spaced) : ''}`;
  };
  const withDay = (patterns: string[] | undefined) => clean(patterns).filter((p) => p.split('{day}').length === 2);
  const thisPatterns = withDay(o.thisWeekday);
  const nextPatterns = withDay(o.nextWeekday);
  const extra = clean(o.weekdays);
  const long = new Intl.DateTimeFormat(locale, { weekday: 'long' });
  const short = new Intl.DateTimeFormat(locale, { weekday: 'short' });
  for (let i = 0; i < 7; i++) {
    // 2 January 2000 was a Sunday.
    const sample = new Date(2000, 0, 2 + i);
    const name = long.format(sample);
    const names = [name];
    // Chinese writes "周五" more than "星期五"; Japanese "金曜" as well as "金曜日".
    if (lang === 'zh') names.push(short.format(sample));
    if (lang === 'ja' && name.endsWith('日')) names.push(name.slice(0, -1));
    // The translation's own names (Monday first): "środę" (on Wednesday), "venerdi", "sexta".
    const loose: string[] = [];
    for (const w of clean((extra[(i + 6) % 7] ?? '').split('/'))) {
      const shortened = spaced && w.length < name.length && name.toLocaleLowerCase(locale).startsWith(w.toLocaleLowerCase(locale));
      (shortened ? loose : names).push(w);
    }
    // A weekday means the next one: a week ahead on that very day. "This Friday" says the same.
    const ahead = (today: string) => addDays(today, (i - weekday(today) + 7) % 7 || 7);
    // "Next Friday" is the Friday of next week.
    const inNextWeek = (today: string, weekStart: number) => addDays(nextWeek(today, weekStart), (i - weekStart + 7) % 7);
    add(anyOf(names, spaced), ahead);
    if (loose.length) add(anyOf(loose, spaced), ahead, false, true);
    const day = anyOf([...names, ...loose], spaced);
    for (const p of thisPatterns) add(around(p, day), ahead);
    for (const p of nextPatterns) add(around(p, day), inNextWeek);
  }

  // The clock as the language writes it: day periods ("PM", "午後") and hour words ("Uhr", "時").
  const parts = (d: Date, opts: Intl.DateTimeFormatOptions) => new Intl.DateTimeFormat(locale, opts).formatToParts(d);
  const afternoon = parts(new Date(2000, 0, 1, 15), { hour: 'numeric', hour12: true });
  const morning = parts(new Date(2000, 0, 1, 9), { hour: 'numeric', hour12: true });
  const am = morning.find((p) => p.type === 'dayPeriod')?.value;
  const pm = afternoon.find((p) => p.type === 'dayPeriod')?.value;
  const periodFirst = afternoon.findIndex((p) => p.type === 'dayPeriod') < afternoon.findIndex((p) => p.type === 'hour');
  // "3pm" where people write it: in English, and where the clock has twelve hours ("3 p. m." in
  // Mexico). Not in German, whose "5 am Montag" is "5 on Monday".
  const latinPeriods = !!pm && /^[ap]\.?\s?m\.?$/i.test(pm);
  const twelveHours = /^h1[12]$/.test(new Intl.DateTimeFormat(locale, { hour: 'numeric' }).resolvedOptions().hourCycle ?? '');
  const ampmOn = latinPeriods && (lang === 'en' || twelveHours);
  // Hours written with words: Intl's ("15 Uhr", "15 h", "15時") and the translation's ("{n}h", "ore {n}").
  const h23 = parts(new Date(2000, 0, 1, 15), { hour: 'numeric', hourCycle: 'h23' });
  const word = h23.slice(h23.findIndex((p) => p.type === 'hour') + 1).map((p) => p.value).join('').trim();
  const patterns = [...new Set([...o.hours, ...(word ? [`{n}${word}`] : [])].map((p) => p.trim()))]
    .map((p) => p.split('{n}'))
    .filter((p) => p.length === 2 && (p[0].trim() || p[1].trim()));
  const after = [...new Set(patterns.map(([, a]) => a.trim()).filter(Boolean))].map((w) => literal(w, spaced));
  const hourWord = after.length ? `(?:${after.join('|')})` : null;
  // Minutes after an hour word: "15h30", "15 Uhr 30" and "15時30分", but not the 12 of
  // "20h 12 personnes" (spaced unlike the hour).
  const minutes = '(?:\\k<sp>(?<m2>\\d{1,2})(?!\\p{N})分?|\\s*(?<m3>\\d{1,2})分)?';
  const ampm = '(?<ap>[ap])\\.?\\s?m\\.?';
  const times = [
    // "15:30", and "15.30" (see `parseQuickAdd`).
    `(?<h>\\d{1,2})(?<sep>[:.])(?<m>\\d{2})${ampmOn ? `(?:\\s*${ampm})?` : ''}${hourWord ? `(?:\\s*(?<hw>${hourWord}))?` : ''}`,
    ...(ampmOn ? [`(?<h>\\d{1,2})\\s*${ampm}`] : []),
    // "15 Uhr", "ore 15", "a las 3 p. m.".
    ...patterns.map(
      ([b, a]) =>
        `${b.trim() ? `${literal(b, spaced)}\\s*` : ''}(?<h>\\d{1,2})(?:[:.](?<m>\\d{2}))?${a.trim() ? `(?<sp>\\s*)${literal(a, spaced)}${minutes}` : ampmOn ? `(?:\\s*${ampm})?` : ''}`,
    ),
  ];
  const localPeriod = periodFirst && am && pm && !/^[ap]\.?\s?m/i.test(pm);
  if (localPeriod) {
    times.push(`(?<pre>${literal(am, spaced)}|${literal(pm, spaced)})\\s*(?<h>\\d{1,2})(?:[:.](?<m>\\d{2})|(?<sp>\\s*)${hourWord ?? '(?!)'}${minutes})?`);
  }
  // Words before a time: the translation's ("at", "um", "o") and those that make a number an hour ("ore", "saat").
  const before = [...clean(o.timeWords), ...patterns.map(([b]) => b.trim()).filter(Boolean)];
  const timeBefore = before.length ? new RegExp(`${spaced ? '(?:^|[^\\p{L}\\p{N}])' : ''}${anyOf(before, spaced)}\\s*$`, 'iu') : null;
  const fillers = clean(o.fillers);
  const fillerBefore = fillers.length ? new RegExp(`${spaced ? '(?:^|[^\\p{L}\\p{N}])' : ''}${anyOf(fillers, spaced)}\\s*$`, 'iu') : null;
  const fillerAfter = fillers.length || before.length ? new RegExp(`^\\s*${anyOf([...fillers, ...before], spaced)}(?![\\p{L}\\p{N}])`, 'iu') : null;

  // Day and month as numbers, in the language's order and with its separators: those of day and
  // month alone and of its short dates ("11/6", "6.11.", and "6.11.2026" in Turkish, which also writes "6/11").
  const order = parts(new Date(2000, 10, 6), { month: 'numeric', day: 'numeric' });
  const monthFirst = order.findIndex((p) => p.type === 'month') < order.findIndex((p) => p.type === 'day');
  const separator = (ps: Intl.DateTimeFormatPart[]) => {
    const i = ps.findIndex((p) => p.type === 'day' || p.type === 'month');
    return ps[i + 1]?.type === 'literal' ? ps[i + 1].value.trim() : '';
  };
  const seps = [...new Set([separator(order), separator(parts(new Date(2000, 10, 6), { dateStyle: 'short' }))])].filter((s) => s.length === 1 && !/[\p{L}\p{N}]/u.test(s));
  const numericDate = seps.length
    ? {
        re: bounded(`(?<a>\\d{1,2})(?<s>${seps.map(escape).join('|')})(?<b>\\d{1,2})(?:\\k<s>(?<y>\\d{4}|\\d{2})(?!\\p{N})|\\k<s>)?`, spaced, true),
        monthFirst,
      }
    : null;

  const g: Grammar = {
    spaced,
    phrases,
    times: times.map((s) => bounded(s, spaced, true)),
    pm: localPeriod ? pm!.toLocaleLowerCase(locale) : null,
    hourWord: hourWord ? new RegExp(`^\\s*${hourWord}`, 'iu') : null,
    numericDate,
    timeBefore,
    decimalPoint: new Intl.NumberFormat(locale).formatToParts(1.5).find((p) => p.type === 'decimal')?.value === '.',
    fillerBefore,
    fillerAfter,
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

/** A loose weekday name is a day after "on", or before "at", a number or nothing more: "na segunda", "sexta às 15h", not "segunda via". */
function fits(g: Grammar, text: string, at: Span): boolean {
  const after = text.slice(at.end);
  return !/^\s*\p{L}/u.test(after) || !!g.fillerAfter?.test(after) || !!g.fillerBefore?.test(text.slice(0, at.start));
}

/** Part of a longer number: "1.10.2", "2.3.15". */
const inNumber = (text: string, at: Span) => /\d[.,:/]$/.test(text.slice(0, at.start)) || /^[.,:/]\d/.test(text.slice(at.end));

/** Minutes after midnight, or null when it is no time of day (25:00, 13pm). */
function minutesOf(g: Record<string, string | undefined>, pm: string | null, locale: string): number | null {
  let h = Number(g.h);
  const min = Number(g.m ?? g.m2 ?? g.m3 ?? 0);
  if (min > 59) return null;
  const period = g.ap ? g.ap.toLowerCase() === 'p' : g.pre ? g.pre.toLocaleLowerCase(locale) === pm : null;
  if (period != null) {
    if (h < 1 || h > 12) return null;
    h = (h % 12) + (period ? 12 : 0);
  } else if (h > 23) return null;
  return h * 60 + min;
}

export function parseQuickAdd(text: string, o: QuickAddOptions): QuickAdd {
  const g = grammar(o);
  const now = o.now ?? Date.now();
  const today = dayKey(now);
  const spans: Span[] = [];
  let date: string | null = null;
  let minutes: number | null = null;

  if (!o.ignore?.due) {
    // The earliest date (the longest of several starting together: "übermorgen" over "morgen").
    let best: (Span & { date: string }) | null = null;
    for (const p of g.phrases) {
      for (let hit = next(p.re, text); hit; hit = next(p.re, text, hit.end)) {
        if (p.loose && !fits(g, text, hit)) continue;
        if (earlier(hit, best)) best = { ...hit, date: p.resolve(today, o.weekStart) };
        break;
      }
    }
    if (g.numericDate) {
      const { re, monthFirst } = g.numericDate;
      for (let hit = next(re, text); hit; hit = next(re, text, hit.end)) {
        const before = text.slice(0, hit.start);
        const after = text.slice(hit.end);
        // Part of a longer number, or a time: "15.11 Uhr", "um 9.05".
        if (inNumber(text, hit) || g.hourWord?.test(after) || g.timeBefore?.test(before)) continue;
        const [a, b] = [Number(hit.groups.a), Number(hit.groups.b)];
        const [month, day] = monthFirst ? [a, b] : [b, a];
        const y = hit.groups.y;
        const year = y ? Number(y) + (y.length === 2 ? 2000 : 0) : new Date(now).getFullYear();
        const valid = (d: Date) => d.getMonth() === month - 1 && d.getDate() === day;
        let d = new Date(year, month - 1, day);
        if (!valid(d)) continue;
        // Without a year, a day already past is next year's.
        if (!y && dayKey(d) < today) d = new Date(year + 1, month - 1, day);
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
          if ((best && hit.start < best.end && best.start < hit.end) || inNumber(text, hit)) continue;
          // Where numbers have a decimal point, one with a dot is a price, a version or just a number
          // ("12.50", "1.10", "3.14") unless a word for time goes with it: "at 9.05", "9.05pm".
          const { sep, ap, hw } = hit.groups;
          if (sep === '.' && g.decimalPoint && !ap && !hw && !g.timeBefore?.test(text.slice(0, hit.start))) continue;
          const m = minutesOf(hit.groups, g.pm, o.locale);
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
  const fillers = clean(o.fillers).sort((a, b) => b.length - a.length);
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

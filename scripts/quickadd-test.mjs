#!/usr/bin/env node
// Tests how quick add reads dates, times and `!` in the interface languages, and due days around
// changes of the clocks. Runs the interface's own modules in Node, which strips their types; the
// i18n module (Svelte state) is replaced by a stand-in that only knows the formatting locale.
//
//   npm run test:quickadd

import { registerHooks } from 'node:module';
import { readFileSync } from 'node:fs';

// Berlin changes its clocks on 29 March and 25 October 2026.
process.env.TZ = 'Europe/Berlin';
if (new Date(2026, 0, 1).getTimezoneOffset() !== -60) throw new Error('could not switch to Berlin time');

const i18n = `data:text/javascript,${encodeURIComponent(
  "export const intlLocale = () => globalThis.locale ?? 'en'; export const t = (key, p = {}) => (key === 'time.dateAt' ? `${p.date}, ${p.time}` : key);",
)}`;
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === '../i18n') return { url: i18n, shortCircuit: true };
    // The interface imports its modules without the `.ts`.
    if (specifier.startsWith('.') && !/\.[cm]?[jt]s$/.test(specifier)) {
      try {
        return next(`${specifier}.ts`, context);
      } catch {
        /* not a TypeScript module */
      }
    }
    return next(specifier, context);
  },
});

const src = new URL('../src/lib/', import.meta.url);
const { parseQuickAdd } = await import(new URL('utils/quickadd.ts', src).href);
const due = await import(new URL('utils/due.ts', src).href);

const catalogs = new Map();
/** A word list of the language's catalog, as QuickAdd.svelte reads it. */
function words(locale, key) {
  const lang = locale.startsWith('pt') ? 'pt-BR' : locale.startsWith('zh') ? 'zh-CN' : locale.split('-')[0];
  if (!catalogs.has(lang)) catalogs.set(lang, JSON.parse(readFileSync(new URL(`i18n/locales/${lang}.json`, src), 'utf8')));
  return (catalogs.get(lang)[`tasks.quickAdd.${key}`] ?? '').split(',').map((w) => w.trim()).filter(Boolean);
}

// Wednesday 4 November 2026, 10:00; weeks start on Monday.
const NOW = new Date(2026, 10, 4, 10, 0).getTime();

function parse(locale, text, { now = NOW, times = true, importance = true } = {}) {
  globalThis.locale = locale;
  return parseQuickAdd(text, {
    locale,
    times,
    importance,
    weekStart: 1,
    now,
    fillers: words(locale, 'fillers'),
    hours: words(locale, 'hours'),
    timeWords: words(locale, 'timeWords'),
    weekdays: words(locale, 'weekdays'),
    thisWeekday: words(locale, 'thisWeekday'),
    nextWeekday: words(locale, 'nextWeekday'),
  });
}

const pad = (n) => String(n).padStart(2, '0');
const show = (r) => `"${r.title}" ${r.due ? `${r.due.date}${r.due.minutes != null ? ` ${pad(Math.floor(r.due.minutes / 60))}:${pad(r.due.minutes % 60)}` : ''}` : 'no date'}${r.important ? ' !' : ''}`;

// [locale, typed, title, due ("YYYY-MM-DD" or "YYYY-MM-DD HH:MM", null for none), options]
// From Wednesday 4 November: tomorrow is the 5th, Friday the 6th, "next Friday" the 13th.
const cases = [
  ['en', 'Call Maya tomorrow 3pm !', 'Call Maya', '2026-11-05 15:00', { important: true }],
  ['en', 'Review today’s agenda', 'Review today’s agenda', null],
  ["en", "Prepare Friday's meeting", "Prepare Friday's meeting", null],
  ['en', 'Pay $12.50 to John', 'Pay $12.50 to John', null],
  ['en', 'Upgrade to 1.10', 'Upgrade to 1.10', null],
  ['en', 'pi is 3.14', 'pi is 3.14', null],
  ['en', 'Standup at 9.30', 'Standup', '2026-11-05 09:30'],
  ['en', 'Standup 9:30', 'Standup', '2026-11-05 09:30'],
  ['en', 'Dinner 7.30pm', 'Dinner', '2026-11-04 19:30'],
  ['en', 'Dentist 11/6/2026', 'Dentist', '2026-11-06'],
  ['en', 'Dentist 11/6/27', 'Dentist', '2027-11-06'],
  ['en', 'Install 1.10.2 tomorrow', 'Install 1.10.2', '2026-11-05'],
  ['en', 'Next Friday call Bob', 'Call Bob', '2026-11-13'],
  ['en', 'Call Bob this Friday', 'Call Bob', '2026-11-06'],
  ['en', 'Call Bob Friday', 'Call Bob', '2026-11-06'],
  ['en', 'Call Bob next Monday', 'Call Bob', '2026-11-09'],
  ['en', 'Report due by next Monday', 'Report due', '2026-11-09'],
  ['en', 'Finish this tomorrow', 'Finish this', '2026-11-05'],
  ['en-GB', 'Call Maya at 3pm', 'Call Maya', '2026-11-04 15:00'],
  ['en', 'Call Maya tomorrow 3pm', 'Call Maya 3pm', '2026-11-05', { times: false }],
  ['en', 'Buy milk !', 'Buy milk !', null, { importance: false }],

  ['de', 'Zahnarzt um 9.05', 'Zahnarzt', '2026-11-05 09:05'],
  ['de', 'Zahnarzt 9.05', 'Zahnarzt', '2027-05-09'],
  ['de', 'Seite 5 am Montag lesen', 'Seite 5 lesen', '2026-11-09'],
  ['de', 'Zahnarzt am 6.11.2026', 'Zahnarzt', '2026-11-06'],
  ['de', 'Zahnarzt am 6.11.', 'Zahnarzt', '2026-11-06'],
  ['de', 'Zahnarzt am nächsten Freitag', 'Zahnarzt', '2026-11-06'],
  ['de', 'Zahnarzt Freitag nächster Woche', 'Zahnarzt', '2026-11-13'],
  ['de', 'Zahnarzt 15.30 Uhr', 'Zahnarzt', '2026-11-04 15:30'],
  ['de', 'Zahnarzt 15.30', 'Zahnarzt', '2026-11-04 15:30'],
  ['de', 'Version 1.10.2 installieren', 'Version 1.10.2 installieren', null],
  ['de', 'Maya anrufen morgen 15 Uhr 30 !', 'Maya anrufen', '2026-11-05 15:30', { important: true }],

  ['pl', 'Dentysta o 9.05', 'Dentysta', '2026-11-05 09:05'],
  ['pl', 'Dentysta 6.11', 'Dentysta', '2026-11-06'],
  ['pl', 'Dentysta w ten piątek', 'Dentysta', '2026-11-06'],
  ['pl', 'Dentysta w następny piątek', 'Dentysta', '2026-11-13'],
  ['pl', 'Dentysta w przyszłą środę', 'Dentysta', '2026-11-11'],
  ['pl', 'Dentysta w sobotę', 'Dentysta', '2026-11-07'],
  ['pl', 'Dentysta w niedzielę', 'Dentysta', '2026-11-08'],

  ['tr', 'Fatura 15.11', 'Fatura', '2026-11-15'],
  ['tr', 'Doğum günü 6.11', 'Doğum günü', '2026-11-06'],
  ['tr', 'Toplantı 6.11.2026 saat 14:00', 'Toplantı', '2026-11-06 14:00'],
  ['tr', 'Dişçi saat 9.05', 'Dişçi', '2026-11-05 09:05'],
  ['tr', 'Toplantı 15.30', 'Toplantı', '2026-11-04 15:30'],
  ['tr', 'Dişçi bu cuma', 'Dişçi', '2026-11-06'],
  ['tr', 'Dişçi gelecek cuma', 'Dişçi', '2026-11-13'],
  ['tr', 'Cuma günü toplantı', 'Toplantı', '2026-11-06'],
  ['tr', 'Cumartesi pazar', 'Pazar', '2026-11-07'],

  ['fr', 'Dîner à 20h 12 personnes', 'Dîner 12 personnes', '2026-11-04 20:00'],
  ['fr', 'Dîner à 20h12', 'Dîner', '2026-11-04 20:12'],
  ['fr', 'Réunion 20 h 30', 'Réunion', '2026-11-04 20:30'],
  ['fr', 'Dentiste vendredi prochain', 'Dentiste', '2026-11-06'],
  ['fr', 'Dentiste ce vendredi', 'Dentiste', '2026-11-06'],
  ['fr', 'Appeler Paul après demain', 'Appeler Paul', '2026-11-06'],

  ['it', 'Dentista alle 15', 'Dentista', '2026-11-04 15:00'],
  ['it', 'Dentista alle 15.30', 'Dentista', '2026-11-04 15:30'],
  ['it', 'Dentista venerdì prossimo', 'Dentista', '2026-11-06'],
  ['it', 'Dentista venerdi', 'Dentista', '2026-11-06'],

  ['es', 'Dentista el próximo viernes', 'Dentista', '2026-11-06'],
  ['es', 'Dentista el viernes que viene', 'Dentista', '2026-11-06'],
  ['es', 'Dentista mañana a las 10', 'Dentista', '2026-11-05 10:00'],
  ['es-MX', 'Dentista mañana a las 3 p. m.', 'Dentista', '2026-11-05 15:00'],

  ['pt-BR', 'Dentista na próxima sexta', 'Dentista', '2026-11-06'],
  ['pt-BR', 'Dentista sexta', 'Dentista', '2026-11-06'],
  ['pt-BR', 'Reunião segunda às 10h', 'Reunião', '2026-11-09 10:00'],
  ['pt-BR', 'Reunião sexta feira', 'Reunião', '2026-11-06'],
  ['pt-BR', 'Pedir segunda via do boleto', 'Pedir segunda via do boleto', null],

  ['ja', '明日15時にマヤに電話 !', 'マヤに電話', '2026-11-05 15:00', { important: true }],
  ['ja', '来週の金曜日に歯医者', '歯医者', '2026-11-13'],
  ['ja', '金曜15時30分 歯医者', '歯医者', '2026-11-06 15:30'],
  ['zh-CN', '下周五开会', '开会', '2026-11-13'],
  ['zh-CN', '明天下午3点开会', '开会', '2026-11-05 15:00'],
];

let failed = 0;
function check(name, got, want) {
  if (got === want) return;
  failed++;
  console.log(`FAIL ${name}\n     got  ${got}\n     want ${want}`);
}

for (const [locale, text, title, when, opts = {}] of cases) {
  const r = parse(locale, text, opts);
  const want = show({ title, due: when && { date: when.slice(0, 10), minutes: when.length > 10 ? Number(when.slice(11, 13)) * 60 + Number(when.slice(14)) : null }, important: !!opts.important });
  check(`[${locale}] ${text}`, show(r), want);
}

// Days of 25 and 23 hours.
const at = (...a) => new Date(...a).getTime();
check('the end of 25 October (25 hours)', due.dueMoment({ dueDate: '2026-10-25', dueAt: null }), at(2026, 9, 25, 23, 59, 59, 999));
check('the end of 29 March (23 hours)', due.dueMoment({ dueDate: '2026-03-29', dueAt: null }), at(2026, 2, 29, 23, 59, 59, 999));
check('due on 25 October, at 23:30 that day', due.dueState({ dueDate: '2026-10-25', dueAt: null, completedAt: null }, at(2026, 9, 25, 23, 30)), 'today');
check('due on 29 March, at 00:30 the next day', due.dueState({ dueDate: '2026-03-29', dueAt: null, completedAt: null }, at(2026, 2, 30, 0, 30)), 'overdue');
check('"23:30" at 23:00 on 25 October', show(parse('en', 'Call at 23:30', { now: at(2026, 9, 25, 23, 0) })), show({ title: 'Call', due: { date: '2026-10-25', minutes: 1410 } }));
check('"23:30" at 23:45 on 29 March', show(parse('en', 'Call at 23:30', { now: at(2026, 2, 29, 23, 45) })), show({ title: 'Call', due: { date: '2026-03-30', minutes: 1410 } }));

// A time set in another time zone: the day is where the time falls here.
globalThis.locale = 'en';
const lateEvening = { dueDate: '2026-11-06', dueAt: at(2026, 10, 5, 22, 0) };
check('the day of a time set further east', due.dueDay(lateEvening), '2026-11-05');
check('its label', due.dueLabel(lateEvening, 'day', NOW), 'Tomorrow');

if (failed) {
  console.log(`\n${failed} check(s) failed`);
  process.exit(1);
}
console.log(`ok   ${cases.length} quick add cases, due days around clock changes`);

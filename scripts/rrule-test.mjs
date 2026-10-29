#!/usr/bin/env node
// Tests the event editor's repeat rules and time zones: rules in words in the interface
// languages, what the custom repeat editor builds and reads back, end dates in a zone, rules
// following a moved start, what Exchange cannot store, and wall-clock times across clock
// changes. Runs the interface's own modules in Node, which strips their types; the i18n module
// (Svelte state) is replaced by a stand-in that formats the real catalogs.
//
//   npm run test:rrule

import { registerHooks } from 'node:module';
import { readFileSync } from 'node:fs';

const src = new URL('../src/lib/', import.meta.url);
const message = new URL('i18n/message.ts', src).href;
const i18n = `data:text/javascript,${encodeURIComponent(
  `import { format } from '${message}';
  export const intlLocale = () => globalThis.locale ?? 'en';
  export const t = (key, params = {}) => {
    const lang = globalThis.lang ?? 'en';
    return format(globalThis.catalog(lang)[key] ?? globalThis.catalog('en')[key] ?? key, params, lang, intlLocale());
  };`,
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

const catalogs = new Map();
globalThis.catalog = (lang) => {
  if (!catalogs.has(lang)) catalogs.set(lang, JSON.parse(readFileSync(new URL(`i18n/locales/${lang}.json`, src), 'utf8')));
  return catalogs.get(lang);
};
const rrule = await import(new URL('utils/rrule.ts', src).href);
const tz = await import(new URL('utils/timezone.ts', src).href);

let failed = 0;
let checks = 0;
function check(name, got, want) {
  checks++;
  if (JSON.stringify(got) === JSON.stringify(want)) return;
  failed++;
  console.log(`FAIL ${name}\n     got  ${JSON.stringify(got)}\n     want ${JSON.stringify(want)}`);
}
function inLanguage(lang, locale = lang) {
  globalThis.lang = lang;
  globalThis.locale = locale;
}

// Tuesday 10 November 2026 is the month's second Tuesday; Friday 27 November its fourth and last Friday.
const NY = 'America/New_York';
const words = [
  ['FREQ=DAILY', null, 'Daily'],
  ['FREQ=DAILY;INTERVAL=3', null, 'Every 3 days'],
  ['FREQ=WEEKLY;BYDAY=MO,WE,FR', null, 'Weekly on Monday, Wednesday, Friday'],
  ['FREQ=WEEKLY', '2026-11-10', 'Weekly on Tuesday'],
  ['FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR', null, 'Every weekday'],
  ['FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TU,WE,TH,FR', null, 'Every 2 weeks on Monday, Tuesday, Wednesday, Thursday, Friday'],
  ['FREQ=MONTHLY', '2026-11-10', 'Monthly on day 10'],
  ['FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=31', '2026-10-31', 'Every 3 months on day 31'],
  ['FREQ=MONTHLY;BYDAY=2TU;COUNT=5', '2026-11-10', 'Monthly on the second Tuesday, 5 times'],
  ['FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR', '2026-11-27', 'Every 2 months on the last Friday'],
  ['FREQ=MONTHLY;BYMONTHDAY=-1', '2026-11-30', 'Monthly on the last day'],
  ['FREQ=YEARLY', '2026-11-10', 'Annually on November 10'],
  ['FREQ=YEARLY;INTERVAL=2;UNTIL=20301110', '2026-11-10', 'Every 2 years on November 10, until Nov 10, 2030'],
  ['FREQ=YEARLY;BYMONTH=11;BYDAY=4TH', '2026-11-26', 'Annually on the fourth Thursday of November'],
  // Read where the series is planned: the end of 31 December in New York is 1 January in UTC.
  ['FREQ=WEEKLY;BYDAY=TU;UNTIL=20270101T045959Z', '2026-11-10', 'Weekly on Tuesday, until Dec 31, 2026', NY],
  // Rules the editor does not make still read as their frequency.
  ['FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1', '2026-11-02', 'Monthly'],
  ['FREQ=HOURLY', null, 'Repeats'],
];
inLanguage('en');
for (const [rule, start, want, zone] of words) check(`[en] ${rule}`, rrule.describeRule(rule, start, zone), want);

// The ordinal weekdays agree with their weekday where a language needs it.
const languages = [
  ['de', 'Monatlich am zweiten Dienstag, 5-mal', 'Alle 2 Monate am letzten Freitag', 'Jährlich am vierten Donnerstag im November'],
  ['es', 'Cada mes el segundo martes, 5 veces', 'Cada 2 meses el último viernes', 'Cada año el cuarto jueves de noviembre'],
  ['fr', 'Tous les mois le deuxième mardi, 5 fois', 'Tous les 2 mois le dernier vendredi', 'Tous les ans en novembre, le quatrième jeudi'],
  ['it', 'Ogni mese il secondo martedì, 5 volte', 'Ogni 2 mesi l’ultimo venerdì', 'Ogni anno il quarto giovedì di novembre'],
  ['ja', '毎月第2火曜日、5 回', '2 か月ごとの最終金曜日', '毎年11月の第4木曜日'],
  ['pl', 'Co miesiąc: drugi wtorek, 5 razy', 'Co 2 miesiące: ostatni piątek', 'Co roku: czwarty czwartek listopada'],
  ['pt-BR', 'Mensalmente na segunda terça-feira, 5 vezes', 'A cada 2 meses, na última sexta-feira', 'Anualmente na quarta quinta-feira de novembro'],
  ['tr', 'Her ayın ikinci Salı günü, 5 kez', '2 ayda bir, ayın son Cuma günü', 'Her yıl Kasım ayının dördüncü Perşembe günü'],
  ['zh-CN', '每月的第二个星期二，共 5 次', '每 2 个月的最后一个星期五', '每年十一月的第四个星期四'],
];
for (const [lang, second, last, yearly] of languages) {
  inLanguage(lang);
  check(`[${lang}] the second Tuesday`, rrule.describeRule('FREQ=MONTHLY;BYDAY=2TU;COUNT=5', '2026-11-10'), second);
  check(`[${lang}] the last Friday`, rrule.describeRule('FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR', '2026-11-27'), last);
  check(`[${lang}] the fourth Thursday of November`, rrule.describeRule('FREQ=YEARLY;BYMONTH=11;BYDAY=4TH', '2026-11-26'), yearly);
}
inLanguage('pl');
check('[pl] a Sunday is feminine', rrule.describeRule('FREQ=MONTHLY;BYDAY=-1SU', '2026-11-29'), 'Co miesiąc: ostatnia niedziela');
check('[pl] every 5 months', rrule.describeRule('FREQ=MONTHLY;INTERVAL=5;BYDAY=1WE', '2026-11-04'), 'Co 5 miesięcy: pierwsza środa');
inLanguage('it');
check('[it] a Sunday is feminine', rrule.describeRule('FREQ=MONTHLY;BYDAY=1SU', '2026-11-01'), 'Ogni mese la prima domenica');
inLanguage('pt-BR');
check('[pt-BR] Saturdays are masculine', rrule.describeRule('FREQ=MONTHLY;BYDAY=-1SA', '2026-11-28'), 'Mensalmente no último sábado');

// What a monthly rule can follow, from its start.
inLanguage('en');
check('monthly choices on the second Tuesday', rrule.monthlyChoices('2026-11-10').map((c) => c.value), ['day', '2TU']);
check('monthly choices on the fourth and last Friday', rrule.monthlyChoices('2026-11-27').map((c) => c.value), ['day', '4FR', '-1FR']);
check('monthly choices on a fifth Monday', rrule.monthlyChoices('2026-11-30').map((c) => c.value), ['day', '-1MO']);

// The custom editor reads a rule and builds it back the same.
for (const [rule, start] of [
  ['FREQ=MONTHLY;BYDAY=2TU;COUNT=5', '2026-11-10'],
  ['FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR', '2026-11-27'],
  ['FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20261231', '2026-11-02'],
  ['FREQ=WEEKLY;BYDAY=MO;WKST=SU', '2026-11-02'],
  ['FREQ=DAILY;INTERVAL=4;COUNT=12', '2026-11-02'],
  ['FREQ=YEARLY;INTERVAL=2', '2026-11-10'],
]) {
  check(`custom editor: ${rule}`, rrule.buildRule(rrule.repeatChoice(rule, start, 'UTC')), rule);
}
const fresh = rrule.repeatChoice(null, '2026-11-10', 'UTC');
check('a new custom repeat: weekly on the start day', rrule.buildRule(fresh), 'FREQ=WEEKLY;BYDAY=TU');
check('weekdays are written Monday first', rrule.buildRule({ ...fresh, days: ['SU', 'FR', 'MO'] }), 'FREQ=WEEKLY;BYDAY=MO,FR,SU');

// End dates: a UTC time for a timed series (the end of the day where it is planned), a date for an all-day one.
check('until, timed, New York', rrule.untilFor('FREQ=WEEKLY;BYDAY=TU;UNTIL=20261231', false, NY), 'FREQ=WEEKLY;BYDAY=TU;UNTIL=20270101T045959Z');
check('until, timed, Tokyo', rrule.untilFor('FREQ=DAILY;UNTIL=20261231', false, 'Asia/Tokyo'), 'FREQ=DAILY;UNTIL=20261231T145959Z');
check('until, all day', rrule.untilFor('FREQ=WEEKLY;BYDAY=TU;UNTIL=20270101T045959Z', true, NY), 'FREQ=WEEKLY;BYDAY=TU;UNTIL=20261231');
check('until, already right', rrule.untilFor('FREQ=DAILY;COUNT=3', false, NY), 'FREQ=DAILY;COUNT=3');

// A rule naming the start's day follows it when the start moves.
check('weekly follows the weekday', rrule.followStart('FREQ=WEEKLY;BYDAY=TU', '2026-11-10', '2026-11-11'), 'FREQ=WEEKLY;BYDAY=WE');
check('weekly on several days stays', rrule.followStart('FREQ=WEEKLY;BYDAY=TU,TH', '2026-11-10', '2026-11-11'), 'FREQ=WEEKLY;BYDAY=TU,TH');
check('the second Tuesday becomes the third Wednesday', rrule.followStart('FREQ=MONTHLY;BYDAY=2TU;COUNT=5', '2026-11-10', '2026-11-18'), 'FREQ=MONTHLY;BYDAY=3WE;COUNT=5');
check('the last Friday stays the last', rrule.followStart('FREQ=MONTHLY;BYDAY=-1FR', '2026-11-27', '2026-12-25'), 'FREQ=MONTHLY;BYDAY=-1FR');
check('the last Friday, moved earlier', rrule.followStart('FREQ=MONTHLY;BYDAY=-1FR', '2026-11-27', '2026-11-20'), 'FREQ=MONTHLY;BYDAY=3FR');
check('a fifth weekday is the last', rrule.followStart('FREQ=MONTHLY;BYDAY=4MO', '2026-11-23', '2026-11-30'), 'FREQ=MONTHLY;BYDAY=-1MO');
check('a day of the month follows', rrule.followStart('FREQ=MONTHLY;BYMONTHDAY=10', '2026-11-10', '2026-11-12'), 'FREQ=MONTHLY;BYMONTHDAY=12');
check('another weekday is left alone', rrule.followStart('FREQ=WEEKLY;BYDAY=MO', '2026-11-10', '2026-11-11'), 'FREQ=WEEKLY;BYDAY=MO');

// A weekly series begins on its first day.
check('weekdays from a Saturday', rrule.firstDay('FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR', '2026-11-14'), '2026-11-16');
check('a matching day stays', rrule.firstDay('FREQ=WEEKLY;BYDAY=TU', '2026-11-10'), '2026-11-10');
check('monthly rules stay', rrule.firstDay('FREQ=MONTHLY;BYDAY=2TU', '2026-11-11'), '2026-11-11');

// What Exchange and Outlook cannot store, and how they treat a day 31.
check('Exchange: every 2 years', rrule.ruleProblem('FREQ=YEARLY;INTERVAL=2', 'ews'), 'Exchange can repeat an event every year, but not every few years.');
check('Outlook: every 2 years', rrule.ruleProblem('FREQ=YEARLY;INTERVAL=2', 'graph'), null);
check('Outlook: a fifth Tuesday', rrule.ruleProblem('FREQ=MONTHLY;BYDAY=5TU', 'graph'), 'Outlook can’t repeat an event this way.');
check('Exchange: the last day', rrule.ruleProblem('FREQ=MONTHLY;BYMONTHDAY=-1', 'ews'), 'Exchange can’t repeat an event this way.');
check('Outlook: the last day', rrule.ruleProblem('FREQ=MONTHLY;BYMONTHDAY=-1', 'graph'), null);
check('Exchange: the editor’s rules', ['FREQ=MONTHLY;BYDAY=2TU;COUNT=5', 'FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR', 'FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;UNTIL=20270101T045959Z', 'FREQ=DAILY;INTERVAL=3'].map((r) => rrule.ruleProblem(r, 'ews')), [null, null, null, null]);
check('CalDAV stores anything', rrule.ruleProblem('FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=-2', 'caldav'), null);
check('day 31 on Google', rrule.ruleNote('FREQ=MONTHLY', '2026-10-31', 'google'), 'Months without day 31 are skipped.');
check('day 31 on Exchange', rrule.ruleNote('FREQ=MONTHLY;BYMONTHDAY=31', '2026-10-31', 'ews'), 'In months without day 31, Exchange moves it to the last day.');
check('day 10 needs no word', rrule.ruleNote('FREQ=MONTHLY', '2026-11-10', 'ews'), null);

// Wall-clock times in a zone, across its clock changes (New York: 8 March and 1 November 2026).
const utc = (ms) => new Date(ms).toISOString().slice(0, 16);
check('10:00 in New York in winter', utc(tz.fromWallClock('2026-11-10', '10:00', NY)), '2026-11-10T15:00');
check('10:00 in New York in summer', utc(tz.fromWallClock('2026-07-10', '10:00', NY)), '2026-07-10T14:00');
check('02:30 skipped in March: as much later', utc(tz.fromWallClock('2026-03-08', '02:30', NY)), '2026-03-08T07:30');
check('01:30 twice in November: the first', utc(tz.fromWallClock('2026-11-01', '01:30', NY)), '2026-11-01T05:30');
check('the end of a day', utc(tz.fromWallClock('2026-12-31', '23:59:59', NY)), '2027-01-01T04:59');
check('half-hour zones', utc(tz.fromWallClock('2026-11-10', '10:00', 'Asia/Kolkata')), '2026-11-10T04:30');
check('back to the wall clock', tz.wallClock(Date.UTC(2026, 10, 10, 15, 0), NY), { date: '2026-11-10', time: '10:00' });
check('offset in winter', tz.offsetMinutes(Date.UTC(2026, 0, 1), NY), -300);
check('one zone, two names', tz.sameZone('Asia/Kolkata', 'Asia/Calcutta'), true);
check('a name nobody knows', tz.isZone('Nowhere/Town'), false);
inLanguage('en', 'en-US');
check('zone names', [tz.zoneName(NY, 'longGeneric'), tz.zoneName('Asia/Tokyo', 'shortOffset')], ['Eastern Time', 'GMT+9']);
check('zone labels', ['America/Argentina/Buenos_Aires', 'UTC', 'Etc/GMT+12'].map(tz.zoneLabel), ['Buenos Aires (GMT-3)', 'UTC', 'GMT-12']);
check('the list has places and UTC, no fixed offsets', tz.zoneChoices().filter((z) => z.id.startsWith('Etc/') || !z.id.includes('/')).map((z) => z.id), ['UTC']);

if (failed) {
  console.log(`\n${failed} of ${checks} check(s) failed`);
  process.exit(1);
}
console.log(`ok   ${checks} repeat rule and time zone checks`);

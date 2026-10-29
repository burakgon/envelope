// Repeat rules of events (RFC 5545 RRULE) as people read them, in the interface language.

import { intlLocale, t } from '../i18n';
import { dateFormat } from './format';

// RRULE day codes in `Date.getDay()` order; 2 January 2000 was a Sunday.
const DAY_CODES = ['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'];

function weekdayName(code: string): string {
  const i = DAY_CODES.indexOf(code.slice(-2));
  return i < 0 ? code : dateFormat({ weekday: 'long' }).format(new Date(2000, 0, 2 + i));
}

/** An UNTIL value (`20261231`, `20261231T235959Z` or floating) as a moment. */
function untilDate(value: string): Date | null {
  const m = /^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})(Z?))?$/.exec(value);
  if (!m) return null;
  const [, y, mo, d, h, mi, s, z] = m;
  if (h == null) return new Date(+y, +mo - 1, +d);
  return z ? new Date(Date.UTC(+y, +mo - 1, +d, +h, +mi, +s)) : new Date(+y, +mo - 1, +d, +h, +mi, +s);
}

/** "Weekly on Monday and Wednesday", "Every 2 months, 10 times"… for a repeat rule. */
export function describeRule(rule: string | null | undefined): string | null {
  if (!rule) return null;
  const parts: Record<string, string> = Object.fromEntries(
    rule
      .replace(/^RRULE:/i, '')
      .split(';')
      .map((p) => {
        const [key, value = ''] = p.split('=');
        return [key.toUpperCase(), value];
      }),
  );
  const interval = Number(parts.INTERVAL ?? 1);
  const count = interval > 1 ? interval : 1;
  let text: string;
  switch (parts.FREQ) {
    case 'DAILY':
      text = t('calendar.recurrence.daily', { count });
      break;
    case 'WEEKLY': {
      const by: string[] = (parts.BYDAY ?? '').split(',').filter(Boolean);
      if (count === 1 && by.length === 5 && !by.includes('SA') && !by.includes('SU')) {
        text = t('calendar.recurrence.everyWeekday');
      } else if (!by.length) {
        text = t('calendar.recurrence.weekly', { count });
      } else {
        // "Monday, Wednesday" in English, "Montag und Mittwoch" in German…
        const days = new Intl.ListFormat(intlLocale(), { style: 'narrow', type: 'conjunction' }).format(by.map(weekdayName));
        text = t('calendar.recurrence.weeklyOn', { count, days });
      }
      break;
    }
    case 'MONTHLY':
      text = t('calendar.recurrence.monthly', { count });
      break;
    case 'YEARLY':
      text = t('calendar.recurrence.yearly', { count });
      break;
    default:
      return t('calendar.recurrence.repeats');
  }
  const times = Number(parts.COUNT);
  if (times > 0) return t('calendar.recurrence.times', { rule: text, count: times });
  const until = parts.UNTIL ? untilDate(parts.UNTIL) : null;
  if (until) {
    const date = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(until);
    return t('calendar.recurrence.until', { rule: text, date });
  }
  return text;
}

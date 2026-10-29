// Due dates of tasks: local days as `YYYY-MM-DD` keys, and how they read in the interface language.

import type { Task } from '../api';
import { intlLocale, t } from '../i18n';
import { dateFormat, timeOf } from './format';

/** The local day of a moment, `YYYY-MM-DD`. */
export function dayKey(at: number | Date): string {
  const d = new Date(at);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** Local midnight of a day key. */
export function dayDate(key: string): Date {
  return new Date(`${key}T00:00:00`);
}

export function addDays(key: string, n: number): string {
  const d = dayDate(key);
  return dayKey(new Date(d.getFullYear(), d.getMonth(), d.getDate() + n));
}

/** Days from today to a day key (negative in the past). */
export function daysFromToday(key: string, now = Date.now()): number {
  const today = dayDate(dayKey(now));
  return Math.round((dayDate(key).getTime() - today.getTime()) / 86_400_000);
}

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();

/** "today", "tomorrow", "übermorgen"… in the interface language. */
export function relativeDay(n: number): string {
  const locale = intlLocale();
  let f = relativeFormats.get(locale);
  if (!f) relativeFormats.set(locale, (f = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })));
  return f.format(n, 'day');
}

/** The first letter upper-cased by the rules of the interface language ("heute" → "Heute"). */
export function capitalize(text: string): string {
  return text.charAt(0).toLocaleUpperCase(intlLocale()) + text.slice(1);
}

/** A day as people say it: "Yesterday", "Today", "Tomorrow", "Saturday", "Mon, Nov 9". */
export function dayLabel(key: string, now = Date.now()): string {
  const n = daysFromToday(key, now);
  if (n >= -1 && n <= 1) return capitalize(relativeDay(n));
  const d = dayDate(key);
  if (n > 1 && n < 7) return capitalize(dateFormat({ weekday: 'long' }).format(d));
  const year = d.getFullYear() === new Date(now).getFullYear() ? undefined : 'numeric';
  return dateFormat({ weekday: 'short', month: 'short', day: 'numeric', year }).format(d);
}

/**
 * The local day a task is due. A time decides it when there is one: the stored day is the one
 * where the time was set, and another time zone may put that moment on the day before or after.
 */
export function dueDay(task: Pick<Task, 'dueDate' | 'dueAt'>): string | null {
  return task.dueAt != null ? dayKey(task.dueAt) : task.dueDate;
}

/** When a task is due: "Tomorrow, 3:00 PM"; with `time` only the time when there is one ("3:00 PM"). */
export function dueLabel(task: Pick<Task, 'dueDate' | 'dueAt'>, only: 'day' | 'time' | 'both' = 'both', now = Date.now()): string {
  const key = dueDay(task);
  if (!key) return '';
  const time = task.dueAt != null ? timeOf(task.dueAt) : null;
  if (only === 'time') return time ?? '';
  const day = dayLabel(key, now);
  if (only === 'day' || !time) return day;
  return t('time.dateAt', { date: day, time });
}

/** The moment a task is late after: its time, or the end of its day (not always 24 hours after its start). */
export function dueMoment(task: Pick<Task, 'dueDate' | 'dueAt'>): number | null {
  if (!task.dueDate) return null;
  return task.dueAt ?? dayDate(addDays(task.dueDate, 1)).getTime() - 1;
}

/** How a due date stands: late, due today, or later. */
export function dueState(task: Pick<Task, 'dueDate' | 'dueAt' | 'completedAt'>, now = Date.now()): 'overdue' | 'today' | 'later' | null {
  const day = dueDay(task);
  if (!day || task.completedAt != null) return null;
  if ((dueMoment(task) ?? Infinity) < now) return 'overdue';
  return day === dayKey(now) ? 'today' : 'later';
}

/** A time of day on a day key, in ms (minutes after midnight, as the clock reads that day). */
export function atTime(key: string, minutes: number): number {
  const d = dayDate(key);
  d.setHours(Math.floor(minutes / 60), minutes % 60, 0, 0);
  return d.getTime();
}

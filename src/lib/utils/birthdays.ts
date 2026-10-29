// The birthdays of the address books as all-day entries of a calendar of their own.

import type { Birthday, Occurrence } from '../api';
import { t } from '../i18n';

/** The calendar birthdays appear in. */
export const BIRTHDAYS = -1;
export const BIRTHDAY_COLOR = '#e3508f';

const ymd = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;

/** Birthdays in [from, to) as all-day occurrences; February 29 falls on the 28th in other years. */
export function birthdayOccurrences(list: Birthday[], from: number, to: number): Occurrence[] {
  const out: Occurrence[] = [];
  const last = new Date(to).getFullYear();
  for (const b of list) {
    const m = /^(\d{4}|-)-(\d{2})-(\d{2})$/.exec(b.date);
    if (!m) continue;
    const [month, date] = [Number(m[2]) - 1, Number(m[3])];
    for (let year = new Date(from).getFullYear(); year <= last; year++) {
      let day = new Date(year, month, date);
      if (day.getMonth() !== month) day = new Date(year, month + 1, 0);
      if (day.getTime() < from || day.getTime() >= to) continue;
      const next = new Date(year, day.getMonth(), day.getDate() + 1);
      out.push({
        key: `birthday-${b.contactId}-${year}`,
        eventId: -b.contactId,
        calendarId: BIRTHDAYS,
        uid: `birthday-${b.contactId}`,
        title: t('birthdays.title', { name: b.name }),
        start: day.getTime(),
        end: next.getTime(),
        allDay: true,
        startDate: ymd(day),
        endDate: ymd(next),
        location: null,
        color: BIRTHDAY_COLOR,
        status: 'confirmed',
        transparent: true,
        isRecurring: false,
        recurrenceId: null,
        myStatus: null,
        hasAttendees: false,
        conferenceUrl: null,
        readOnly: true,
      });
    }
  }
  return out;
}

/** The age someone turns on a birthday in `year`, when the year they were born is known. */
export function ageIn(b: Birthday, year: number): number | null {
  const born = /^\d{4}/.test(b.date) ? Number(b.date.slice(0, 4)) : null;
  return born != null && year > born ? year - born : null;
}

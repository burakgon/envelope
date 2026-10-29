// Week numbers, as ISO 8601 counts them: weeks start on Monday, and week 1 is the one with January 4.

const DAY = 86_400_000;

function day(at: number | Date, plus = 0): Date {
  const d = new Date(at);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + plus);
}

/** The number of the week holding a day. */
export function isoWeek(at: number | Date): number {
  const d = day(at);
  // The Thursday of the same Monday-to-Sunday week decides the year it counts in.
  const thursday = day(d, 3 - ((d.getDay() + 6) % 7));
  const jan4 = new Date(thursday.getFullYear(), 0, 4);
  const firstThursday = day(jan4, 3 - ((jan4.getDay() + 6) % 7));
  return 1 + Math.round((thursday.getTime() - firstThursday.getTime()) / (7 * DAY));
}

/** The number of a row of seven days starting at `first`: that of the Monday in it. */
export function weekOfRow(first: number | Date): number {
  const d = new Date(first);
  return isoWeek(day(d, (8 - d.getDay()) % 7));
}

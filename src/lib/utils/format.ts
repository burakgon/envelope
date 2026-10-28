import type { Address } from '../api';

const DAY = 86_400_000;

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const weekdayFmt = new Intl.DateTimeFormat(undefined, { weekday: 'short' });
const monthDayFmt = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const fullDateFmt = new Intl.DateTimeFormat(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
const longFmt = new Intl.DateTimeFormat(undefined, { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' });

/** Compact timestamp for lists: 14:05, Mon, Oct 3, 3 Oct 2024. */
export function listDate(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const today = startOfDay(new Date(now));
  if (ms >= today) return timeFmt.format(d);
  if (ms >= today - 6 * DAY) return weekdayFmt.format(d);
  if (d.getFullYear() === new Date(now).getFullYear()) return monthDayFmt.format(d);
  return fullDateFmt.format(d);
}

/** Message header timestamp: "Today, 14:05", "Yesterday, 09:12", "Mon, Oct 3, 14:05". */
export function messageDate(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const today = startOfDay(new Date(now));
  const t = timeFmt.format(d);
  if (ms >= today) return `Today, ${t}`;
  if (ms >= today - DAY) return `Yesterday, ${t}`;
  if (d.getFullYear() === new Date(now).getFullYear()) return `${weekdayFmt.format(d)}, ${monthDayFmt.format(d)}, ${t}`;
  return `${fullDateFmt.format(d)}, ${t}`;
}

export function longDate(ms: number): string {
  return longFmt.format(new Date(ms));
}

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
export function relative(ms: number, now = Date.now()): string {
  const diff = ms - now;
  const abs = Math.abs(diff);
  if (abs < 60_000) return 'just now';
  if (abs < 3_600_000) return rtf.format(Math.round(diff / 60_000), 'minute');
  if (abs < DAY) return rtf.format(Math.round(diff / 3_600_000), 'hour');
  if (abs < 7 * DAY) return rtf.format(Math.round(diff / DAY), 'day');
  return monthDayFmt.format(new Date(ms));
}

export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function displayName(a: Address | null | undefined): string {
  if (!a) return 'Unknown';
  const n = a.name?.trim();
  if (n && !n.includes('@')) return n;
  return a.email;
}

export function firstName(a: Address | null | undefined): string {
  const n = displayName(a);
  if (n.includes('@')) return n.split('@')[0];
  return n.split(/\s+/)[0];
}

export function initials(a: Address | null | undefined): string {
  const n = displayName(a);
  if (n.includes('@')) return n[0]?.toUpperCase() ?? '?';
  const parts = n.split(/\s+/).filter((p) => /\p{L}/u.test(p[0] ?? ''));
  if (parts.length === 0) return n[0]?.toUpperCase() ?? '?';
  return ((parts[0][0] ?? '') + (parts.length > 1 ? parts[parts.length - 1][0] : '')).toUpperCase();
}

/** Stable, pleasant avatar hue for an address. */
export function avatarHue(key: string): number {
  let h = 0;
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) >>> 0;
  return h % 360;
}

/** "Maya, Daniel, me (4)" style participant summary. */
export function participantSummary(people: Address[], meEmails: Set<string>, count: number): string {
  const names = people.map((p) => (meEmails.has(p.email.toLowerCase()) ? 'me' : firstName(p)));
  let text: string;
  if (names.length === 0) text = 'Unknown';
  else if (names.length === 1) text = people.length === 1 && names[0] !== 'me' ? displayName(people[0]) : names[0];
  else if (names.length <= 3) text = names.join(', ');
  else text = `${names[0]} … ${names.slice(-2).join(', ')}`;
  return count > 1 ? `${text}` : text;
}

export function plural(n: number, word: string, pluralWord = `${word}s`): string {
  return `${n} ${n === 1 ? word : pluralWord}`;
}

export function fileIconKind(mime: string, filename: string): 'image' | 'pdf' | 'doc' | 'sheet' | 'slides' | 'archive' | 'audio' | 'video' | 'calendar' | 'code' | 'file' {
  const ext = filename.split('.').pop()?.toLowerCase() ?? '';
  if (mime.startsWith('image/')) return 'image';
  if (mime === 'application/pdf' || ext === 'pdf') return 'pdf';
  if (mime.startsWith('audio/')) return 'audio';
  if (mime.startsWith('video/')) return 'video';
  if (mime === 'text/calendar' || ext === 'ics') return 'calendar';
  if (['xls', 'xlsx', 'ods', 'csv', 'numbers'].includes(ext)) return 'sheet';
  if (['ppt', 'pptx', 'odp', 'key'].includes(ext)) return 'slides';
  if (['doc', 'docx', 'odt', 'rtf', 'pages', 'txt', 'md'].includes(ext)) return 'doc';
  if (['zip', 'tar', 'gz', 'rar', '7z', 'xz', 'zst'].includes(ext)) return 'archive';
  if (['js', 'ts', 'rs', 'py', 'json', 'html', 'css', 'go', 'c', 'cpp', 'sh'].includes(ext)) return 'code';
  return 'file';
}

export const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
export const modKey = isMac ? '⌘' : 'Ctrl';

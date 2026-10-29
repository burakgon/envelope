import type { Address } from '../api';
import { intlLocale, t } from '../i18n';

const DAY = 86_400_000;

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

const formats = new Map<string, Intl.DateTimeFormat>();
const paddedHours = new Map<string, boolean>();

/** Whether the locale writes hours with two digits in its own short time format ("08:05" in Turkish or German, "8:05" in English or Spanish). */
function padsHours(locale: string): boolean {
  let pads = paddedHours.get(locale);
  if (pads == null) {
    const hour = new Intl.DateTimeFormat(locale, { timeStyle: 'short' }).formatToParts(new Date(2000, 0, 1, 8, 5)).find((p) => p.type === 'hour');
    paddedHours.set(locale, (pads = hour?.value.length === 2));
  }
  return pads;
}

/**
 * A cached `Intl.DateTimeFormat` for the interface language. `hour: 'numeric'`
 * follows the language's habit of padding hours, so times read "08:05" in German.
 */
export function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const locale = intlLocale();
  if (options.hour === 'numeric' && options.minute && padsHours(locale)) options = { ...options, hour: '2-digit' };
  const key = `${locale} ${JSON.stringify(options)}`;
  let f = formats.get(key);
  if (!f) formats.set(key, (f = new Intl.DateTimeFormat(locale, options)));
  return f;
}

const TIME: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' };
const WEEKDAY: Intl.DateTimeFormatOptions = { weekday: 'short' };
const MONTH_DAY: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric' };
const FULL_DATE: Intl.DateTimeFormatOptions = { year: 'numeric', month: 'short', day: 'numeric' };
const LONG: Intl.DateTimeFormatOptions = { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' };

/** Clock time in the interface language: "14:05", "2:05 PM". */
export function timeOf(ms: number | Date): string {
  return dateFormat(TIME).format(ms);
}

/** Compact timestamp for lists: 14:05, Mon, Oct 3, 3 Oct 2024. */
export function listDate(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const today = startOfDay(new Date(now));
  if (ms >= today) return dateFormat(TIME).format(d);
  if (ms >= today - 6 * DAY) return dateFormat(WEEKDAY).format(d);
  if (d.getFullYear() === new Date(now).getFullYear()) return dateFormat(MONTH_DAY).format(d);
  return dateFormat(FULL_DATE).format(d);
}

/** Message header timestamp: "Today, 14:05", "Yesterday, 09:12", "Mon, Oct 3, 14:05". */
export function messageDate(ms: number, now = Date.now()): string {
  const d = new Date(ms);
  const today = startOfDay(new Date(now));
  const time = dateFormat(TIME).format(d);
  if (ms >= today) return t('time.todayAt', { time });
  if (ms >= today - DAY) return t('time.yesterdayAt', { time });
  if (d.getFullYear() === new Date(now).getFullYear()) {
    return t('time.dateAt', { date: dateFormat({ ...WEEKDAY, ...MONTH_DAY }).format(d), time });
  }
  return t('time.dateAt', { date: dateFormat(FULL_DATE).format(d), time });
}

export function longDate(ms: number): string {
  return dateFormat(LONG).format(new Date(ms));
}

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();

export function relative(ms: number, now = Date.now()): string {
  const locale = intlLocale();
  let rtf = relativeFormats.get(locale);
  if (!rtf) relativeFormats.set(locale, (rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })));
  const diff = ms - now;
  const abs = Math.abs(diff);
  if (abs < 60_000) return t('time.justNow');
  if (abs < 3_600_000) return rtf.format(Math.round(diff / 60_000), 'minute');
  if (abs < DAY) return rtf.format(Math.round(diff / 3_600_000), 'hour');
  if (abs < 7 * DAY) return rtf.format(Math.round(diff / DAY), 'day');
  return dateFormat(MONTH_DAY).format(new Date(ms));
}

const sizeFormats = new Map<string, Intl.NumberFormat>();

/** "512 B", "48 KB", "1.5 MB" with the units and separators of the interface language. */
export function fileSize(bytes: number): string {
  const step = bytes < 1024 ? 0 : bytes < 1024 ** 2 ? 1 : bytes < 1024 ** 3 ? 2 : 3;
  const unit = ['byte', 'kilobyte', 'megabyte', 'gigabyte'][step];
  const digits = step >= 2 ? 1 : 0;
  const locale = intlLocale();
  const key = `${locale} ${unit}`;
  let f = sizeFormats.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, { style: 'unit', unit, unitDisplay: 'short', minimumFractionDigits: digits, maximumFractionDigits: digits });
    sizeFormats.set(key, f);
  }
  return f.format(step === 1 ? Math.round(bytes / 1024) : bytes / 1024 ** step);
}

export function displayName(a: Address | null | undefined): string {
  if (!a) return t('common.unknown');
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
  const me = t('common.me');
  const names = people.map((p) => (meEmails.has(p.email.toLowerCase()) ? me : firstName(p)));
  let text: string;
  if (names.length === 0) text = t('common.unknown');
  else if (names.length === 1) text = people.length === 1 && names[0] !== me ? displayName(people[0]) : names[0];
  else if (names.length <= 3) text = names.join(', ');
  else text = `${names[0]} … ${names.slice(-2).join(', ')}`;
  return count > 1 ? `${text}` : text;
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
/** The label of the shortcut modifier key: ⌘ on macOS, "Ctrl" (or "Strg", …) elsewhere. */
export function modKey(): string {
  return isMac ? '⌘' : t('keys.ctrl');
}

/**
 * A key that belongs to an input method (Japanese, Chinese…) rather than the app: Enter that
 * picks the text. WebKit may end the composition first, and the keydown then says 229.
 */
export function composing(e: KeyboardEvent): boolean {
  return e.isComposing || e.keyCode === 229;
}

/** The preview the engine stores for encrypted mail it keeps no decrypted copy of (crypto/mod.rs). */
const ENCRYPTED_SNIPPET = 'Encrypted message';

/** A message or conversation preview, in the interface language. */
export function previewText(snippet: string): string {
  return snippet === ENCRYPTED_SNIPPET ? t('crypto.encryptedPreview') : snippet;
}

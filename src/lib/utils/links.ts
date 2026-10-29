// Where links in messages go: the address shown while a link is pointed at,
// and whether the link's text names another site than the one it opens.
// No DOM here, so that `npm run test:mail` can run it in Node.

/** File names that look like domains ("report.pdf") but aren't. */
const FILE_ENDINGS = new Set([
  'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'txt', 'csv', 'rtf', 'zip', 'rar', 'gz', 'tar', '7z',
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'heic', 'mp3', 'mp4', 'mov', 'avi', 'wav', 'exe', 'dmg', 'msi', 'apk', 'deb', 'rpm',
  'html', 'htm', 'php', 'asp', 'aspx', 'js', 'json', 'xml', 'ics', 'vcf', 'eml', 'md',
]);

/** The host a link opens, lowercase and without "www."; null for other kinds of links. */
export function hostOf(href: string): string | null {
  try {
    const url = new URL(href);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
    return url.hostname.toLowerCase().replace(/^www\./, '').replace(/\.$/, '') || null;
  } catch {
    return null;
  }
}

/**
 * The site a link's text names, if the text is an address: "https://paypal.com/login",
 * "www.paypal.com" or "paypal.com". Ordinary words ("Read more", "report.pdf") name none.
 */
export function namedHost(text: string): string | null {
  const s = text.trim().replace(/^<|>$/g, '');
  if (!s || /\s/.test(s)) return null;
  const withScheme = /^https?:\/\//i.test(s);
  const m = /^(?:https?:\/\/)?(?:[^\s@/]+@)?((?:[a-z0-9¡-￿](?:[a-z0-9¡-￿-]{0,61}[a-z0-9¡-￿])?\.)+([a-z¡-￿]{2,24}))(?::\d+)?(?:[/?#][^\s]*)?$/i.exec(s);
  if (!m) return null;
  const host = m[1].toLowerCase().replace(/^www\./, '');
  const ending = m[2].toLowerCase();
  if (!withScheme && !/^www\./i.test(s) && FILE_ENDINGS.has(ending)) return null;
  try {
    // The same form as the target's host (international names in punycode).
    return new URL(`http://${host}`).hostname.replace(/\.$/, '');
  } catch {
    return host;
  }
}

/** Whether two hosts are the same site: the same, or one inside the other ("mail.paypal.com" and "paypal.com"). */
export function sameSite(a: string, b: string): boolean {
  return a === b || a.endsWith(`.${b}`) || b.endsWith(`.${a}`);
}

export interface LinkTarget {
  /** What kind of link: a web page, an address to write to, a phone number, a calendar. */
  kind: 'web' | 'mail' | 'phone' | 'calendar' | 'other';
  /** The address as it is shown. */
  href: string;
  /** The web page's host. */
  host: string | null;
  /** The site the link's text names instead of `host`, when it names another one. */
  claimed: string | null;
}

/** Where a link with `text` goes; null for links within the message. */
export function linkTarget(href: string, text: string): LinkTarget | null {
  const h = href.trim();
  if (!h || h.startsWith('#')) return null;
  if (/^mailto:/i.test(h)) return { kind: 'mail', href: decodeURIComponent(h.slice(7).split('?')[0]), host: null, claimed: null };
  if (/^tel:/i.test(h)) return { kind: 'phone', href: decodeURIComponent(h.slice(4)), host: null, claimed: null };
  if (/^webcals?:/i.test(h)) return { kind: 'calendar', href: h, host: null, claimed: null };
  const host = hostOf(h);
  if (!host) return { kind: 'other', href: h, host: null, claimed: null };
  const named = namedHost(text);
  return { kind: 'web', href: h, host, claimed: named && !sameSite(named, host) ? named : null };
}

/** An address shortened in the middle to `max` characters, keeping its start and end. */
export function shorten(s: string, max = 80): string {
  if (s.length <= max) return s;
  const head = Math.ceil((max - 1) * 0.65);
  return `${s.slice(0, head)}…${s.slice(s.length - (max - 1 - head))}`;
}

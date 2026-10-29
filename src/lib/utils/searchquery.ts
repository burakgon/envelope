// Search queries in the engine's language (Gmail-like operators: crates/envelope-core/src/search.rs),
// built from the search options and completed as they are typed.

/** What the search options ask for. */
export interface SearchOptions {
  from: string;
  to: string;
  subject: string;
  words: string;
  attachment: boolean;
  unread: boolean;
  starred: boolean;
  /** From a VIP. */
  vip: boolean;
  /** `7d`, `1m`, `1y` (newer than), or '' for any time. */
  within: string;
  /** A folder's path to search in, or '' for all mail. */
  folder: string;
  /** The account the folder is in, by address; '' for every account's. */
  account: string;
}

export const noOptions: SearchOptions = { from: '', to: '', subject: '', words: '', attachment: false, unread: false, starred: false, vip: false, within: '', folder: '', account: '' };

/** A value as an operator takes it: quoted when it holds spaces or quotes. */
function value(text: string): string {
  const v = text.trim();
  return /[\s"]/.test(v) ? `"${v.replace(/"/g, '')}"` : v;
}

/** The query for a set of options: `from:maya has:attachment newer_than:30d budget`. */
export function buildQuery(o: SearchOptions): string {
  const parts: string[] = [];
  if (o.from.trim()) parts.push(`from:${value(o.from)}`);
  if (o.to.trim()) parts.push(`to:${value(o.to)}`);
  if (o.subject.trim()) parts.push(`subject:${value(o.subject)}`);
  if (o.attachment) parts.push('has:attachment');
  if (o.unread) parts.push('is:unread');
  if (o.starred) parts.push('is:starred');
  if (o.vip) parts.push('is:vip');
  if (o.within) parts.push(`newer_than:${o.within}`);
  if (o.folder.trim()) parts.push(`in:${value(o.folder)}`);
  if (o.account.trim()) parts.push(`account:${value(o.account)}`);
  if (o.words.trim()) parts.push(o.words.trim());
  return parts.join(' ');
}

/** The options a query asks for, the reverse of `buildQuery`: what they can't hold goes into the words. */
export function parseQuery(query: string): SearchOptions {
  const o: SearchOptions = { ...noOptions };
  const words: string[] = [];
  for (const m of query.matchAll(/(-?)(\w+):("[^"]*"|\S+)|"[^"]*"|\S+/g)) {
    const [token, not, key, raw] = m;
    const v = raw?.replace(/^"|"$/g, '') ?? '';
    const k = key?.toLowerCase();
    const set = (field: 'from' | 'to' | 'subject' | 'folder' | 'account') => {
      if (o[field]) return false;
      o[field] = v;
      return true;
    };
    const placed =
      !not &&
      ((k === 'from' && set('from')) ||
        (k === 'to' && set('to')) ||
        (k === 'subject' && set('subject')) ||
        (k === 'in' && set('folder')) ||
        (k === 'account' && set('account')) ||
        (k === 'has' && v.toLowerCase() === 'attachment' && (o.attachment = true)) ||
        (k === 'is' && v.toLowerCase() === 'unread' && (o.unread = true)) ||
        (k === 'is' && v.toLowerCase() === 'starred' && (o.starred = true)) ||
        (k === 'is' && v.toLowerCase() === 'vip' && (o.vip = true)) ||
        (k === 'newer_than' && ['7d', '1m', '1y'].includes(v) && !o.within && (o.within = v)));
    if (!placed) words.push(token);
  }
  o.words = words.join(' ');
  return o;
}

/** An address operator being typed at the end of a query (`… from:ma`): its key and what follows it. */
export function addressAtEnd(query: string): { key: 'from' | 'to' | 'cc'; text: string } | null {
  const m = /(?:^|\s)-?(from|to|cc):([^\s"]*)$/i.exec(query);
  return m ? { key: m[1].toLowerCase() as 'from' | 'to' | 'cc', text: m[2] } : null;
}

/** The query with the address operator at its end completed to `address`, and a space to go on typing. */
export function completeAddress(query: string, address: string): string {
  return `${query.replace(/(from|to|cc):[^\s"]*$/i, (_, key: string) => `${key}:${value(address)}`)} `;
}

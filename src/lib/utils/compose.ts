// Checks before sending: addresses, and attachments the text talks about.
// No DOM here, so that `npm run test:mail` can run it in Node.

/** What an e-mail address looks like: something@domain.tld. */
export const EMAIL = /^[^\s@<>()]+@[^\s@<>()]+\.[^\s@<>()]+$/;

/** Whether an address can be sent to at all (the engine refuses others). */
export function sendable(email: string): boolean {
  return /^[^\s@]+@[^\s@]+$/.test(email.trim());
}

/** The editable part of a message, without quoted mail and the signature. */
export function ownText(html: string): string {
  const cut = html.search(/<div class="envelope-(?:quote|forward)"|<blockquote\b|<div class="gmail_quote"|<div id="divRplyFwdMsg"/i);
  const mine = cut >= 0 ? html.slice(0, cut) : html;
  return mine
    // The signature closes what was written.
    .replace(/<div class="envelope-signature">[\s\S]*$/i, '')
    .replace(/<(br|\/p|\/div|\/li|\/h[1-6])\b[^>]*>/gi, '\n')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&nbsp;/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .split('\n')
    // Quoted lines of a plain-text reply.
    .filter((line) => !/^\s*>/.test(line))
    .join('\n');
}

/** Words of a catalog list: "attached, attachment, see attached". */
export function wordList(...lists: string[]): string[] {
  const words = lists.flatMap((l) => l.split(',')).map((w) => w.trim().toLowerCase()).filter(Boolean);
  return [...new Set(words)];
}

const CJK = /[぀-ヿ㐀-鿿豈-﫿]/;

function escape(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * The first word of `words` the text of `html` uses outside quoted mail and the signature,
 * as a whole word ("attached", not "unattached"); a trailing `*` matches any ending
 * ("anhäng*"). Words in Chinese or Japanese match anywhere, as those languages don't
 * separate words.
 */
export function mentionedAttachment(html: string, words: string[]): string | null {
  const text = ownText(html).toLowerCase();
  if (!text.trim()) return null;
  // Longer phrases first: "in allegato" says more than "allegato".
  for (const raw of [...words].sort((a, b) => b.length - a.length)) {
    const prefix = raw.endsWith('*');
    const word = (prefix ? raw.slice(0, -1) : raw).trim();
    if (!word) continue;
    if (CJK.test(word)) {
      if (text.includes(word)) return word;
      continue;
    }
    const re = new RegExp(`(?<![\\p{L}\\p{N}])${escape(word).replace(/\s+/g, '\\s+')}${prefix ? '' : '(?![\\p{L}\\p{N}])'}`, 'u');
    if (re.test(text)) return word;
  }
  return null;
}

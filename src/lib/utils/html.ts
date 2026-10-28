// Email HTML preparation: sanitizing, privacy protection and presentation.

import DOMPurify from 'dompurify';

export interface PreparedEmail {
  html: string;
  blockedRemote: number;
  trackers: number;
  /** The email paints its own backgrounds/colors (newsletters, receipts). */
  designed: boolean;
}

export interface PrepareOptions {
  allowRemote: boolean;
  inlineUrl: (cid: string) => string;
  dark: boolean;
  darkenDesigned: boolean;
  collapseQuotes: boolean;
}

const TRACKER_HOSTS = [
  'list-manage.com/track', 'mailchimp.com/track', 'sendgrid.net/wf/open', 'mandrillapp.com/track', 'open.convertkit',
  'track.hubspot', 'hubspotlinks', 't.co/i/', 'pixel.', 'mailtrack.io', 'mixmax.com/api/track', 'yesware.com/t',
  'streak.com/pixel', 'bananatag', 'getnotify.com', 'superhuman.com', 'mailstat.us', 'email.mg.', '/open.aspx',
  'tracking.', 'trk.', '/wf/open', 'beacon', 'emltrk', 'facebook.com/tr', 'google-analytics.com/collect',
];

function isTracker(img: Element): boolean {
  const src = img.getAttribute('src') ?? '';
  const w = img.getAttribute('width');
  const h = img.getAttribute('height');
  const style = (img.getAttribute('style') ?? '').replace(/\s/g, '').toLowerCase();
  const tiny = (w === '1' || w === '0') && (h === '1' || h === '0');
  const hidden = style.includes('display:none') || style.includes('width:1px;height:1px') || style.includes('visibility:hidden');
  const known = TRACKER_HOSTS.some((t) => src.toLowerCase().includes(t));
  return tiny || (hidden && /^https?:/i.test(src)) || (known && (tiny || !w));
}

const REMOTE = /^(https?:)?\/\//i;
const CSS_URL = /url\(\s*(['"]?)(https?:)?\/\/[^)]*\1\s*\)/gi;

export function prepareHtml(raw: string, opts: PrepareOptions): PreparedEmail {
  let blocked = 0;
  let trackers = 0;

  DOMPurify.removeAllHooks();
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    const el = node as Element;
    if (!el.getAttribute) return;
    const tag = el.tagName;
    if (tag === 'IMG') {
      if (isTracker(el)) {
        trackers++;
        el.remove();
        return;
      }
      const src = el.getAttribute('src') ?? '';
      if (src.toLowerCase().startsWith('cid:')) {
        el.setAttribute('src', opts.inlineUrl(src.slice(4)));
      } else if (REMOTE.test(src) && !opts.allowRemote) {
        blocked++;
        el.setAttribute('data-blocked-src', src);
        el.removeAttribute('src');
        el.removeAttribute('srcset');
        el.classList.add('env-blocked');
      }
      el.setAttribute('loading', 'lazy');
    }
    if (el.hasAttribute('background')) {
      const bg = el.getAttribute('background') ?? '';
      if (REMOTE.test(bg) && !opts.allowRemote) {
        blocked++;
        el.removeAttribute('background');
      }
    }
    const style = el.getAttribute('style');
    if (style && !opts.allowRemote && CSS_URL.test(style)) {
      blocked++;
      el.setAttribute('style', style.replace(CSS_URL, 'none'));
    }
    if (tag === 'A') {
      el.setAttribute('target', '_blank');
      el.setAttribute('rel', 'noopener noreferrer');
    }
    if (tag === 'LINK') {
      if (!opts.allowRemote) {
        blocked++;
        el.remove();
      }
    }
  });
  DOMPurify.addHook('uponSanitizeElement', (node, data) => {
    if (data.tagName === 'style' && !opts.allowRemote && node.textContent && CSS_URL.test(node.textContent)) {
      blocked++;
      node.textContent = node.textContent.replace(CSS_URL, 'none');
    }
  });

  const doc = DOMPurify.sanitize(raw, {
    WHOLE_DOCUMENT: true,
    RETURN_DOM: true,
    ADD_TAGS: ['style', 'link', 'center', 'font'],
    ADD_ATTR: ['target', 'bgcolor', 'background', 'align', 'valign', 'cellpadding', 'cellspacing', 'border', 'color', 'face', 'size', 'width', 'height'],
    FORBID_TAGS: ['script', 'iframe', 'frame', 'frameset', 'object', 'embed', 'applet', 'base', 'meta', 'form', 'input', 'button', 'textarea', 'select', 'option', 'noscript', 'svg', 'math'],
    ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto|tel|cid|att|data|webcals?):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i,
  }) as HTMLElement;
  DOMPurify.removeAllHooks();

  const root = doc.ownerDocument ?? document;
  const html = doc.nodeName === 'HTML' ? doc : (root.documentElement as HTMLElement);
  const body = html.querySelector('body') ?? html;

  // Strip external stylesheets' links that survived sanitizing.
  if (!opts.allowRemote) html.querySelectorAll('link').forEach((l) => l.remove());

  const designed = isDesigned(html);

  if (opts.collapseQuotes) collapseQuotes(body as HTMLElement);

  const out = `<!DOCTYPE html><html><head><meta charset="utf-8">${cspMeta(opts.allowRemote)}<style>${baseCss(opts.dark, designed, opts.darkenDesigned)}</style>${[...html.querySelectorAll('style')].map((s) => s.outerHTML).join('')}</head><body class="${designed ? 'designed' : 'plain'}">${body.innerHTML}</body></html>`;
  return { html: out, blockedRemote: blocked, trackers, designed };
}

/**
 * Sanitizes a fragment of mail HTML for display inside the app itself (not in the
 * sandboxed message frame), e.g. the quoted original below a reply. Remote images
 * are dropped so that opening a reply doesn't ping trackers.
 */
export function sanitizeFragment(html: string): string {
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node.nodeName === 'IMG' && REMOTE.test(node.getAttribute('src') ?? '')) node.removeAttribute('src');
    if (node.nodeName === 'A') node.setAttribute('target', '_blank');
  });
  try {
    return DOMPurify.sanitize(html, {
      FORBID_TAGS: ['style', 'form', 'input', 'button', 'textarea', 'select', 'iframe', 'object', 'embed', 'link', 'meta'],
      FORBID_ATTR: ['srcset'],
    }) as string;
  } finally {
    DOMPurify.removeAllHooks();
  }
}

function cspMeta(allowRemote: boolean): string {
  const img = allowRemote ? 'data: att: http://att.localhost https: http:' : 'data: att: http://att.localhost';
  const font = allowRemote ? 'data: https:' : 'data:';
  return `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${img}; media-src ${img}; style-src 'unsafe-inline' ${allowRemote ? 'https:' : ''}; font-src ${font};">`;
}

function isDesigned(root: HTMLElement): boolean {
  if (root.querySelector('[bgcolor], [background]')) return true;
  const styles = [...root.querySelectorAll('style')].map((s) => s.textContent ?? '').join(' ');
  if (/background(-color)?\s*:/i.test(styles)) return true;
  let count = 0;
  for (const el of root.querySelectorAll<HTMLElement>('[style]')) {
    const s = el.getAttribute('style') ?? '';
    if (/background(-color)?\s*:\s*(?!transparent|none|inherit)/i.test(s)) count++;
    if (count >= 1) return true;
  }
  return false;
}

/** Hides quoted history behind a toggle (Gmail, Apple Mail, Outlook, Thunderbird styles). */
function collapseQuotes(body: HTMLElement) {
  const selectors = [
    '.gmail_quote',
    'blockquote[type="cite"]',
    '.envelope-quote',
    '#divRplyFwdMsg',
    '#appendonsend',
    '.moz-cite-prefix',
    'div[class*="yahoo_quoted"]',
    '.protonmail_quote',
  ];
  let target: Element | null = null;
  for (const sel of selectors) {
    const found = body.querySelector(sel);
    if (found) {
      target = found;
      break;
    }
  }
  if (!target) {
    // A trailing top-level blockquote is almost always quoted history.
    const last = [...body.children].reverse().find((c) => c.textContent?.trim());
    if (last?.tagName === 'BLOCKQUOTE') target = last;
  }
  if (!target || !target.parentElement) return;
  // Include the "On … wrote:" line right before the quote.
  const prev = target.previousElementSibling;
  const wrapper = body.ownerDocument.createElement('div');
  wrapper.className = 'env-quote';
  wrapper.hidden = true;
  if (target.id === 'divRplyFwdMsg' || target.id === 'appendonsend') {
    // Outlook: everything after the marker is history.
    let node: ChildNode | null = target;
    const parent = target.parentElement;
    const nodes: ChildNode[] = [];
    while (node) {
      nodes.push(node);
      node = node.nextSibling;
    }
    parent.insertBefore(wrapper, target);
    nodes.forEach((n) => wrapper.appendChild(n));
  } else {
    target.parentElement.insertBefore(wrapper, target);
    if (prev && /wrote:|yazdı:|schrieb|a écrit|escribió/i.test(prev.textContent ?? '') && (prev.textContent?.length ?? 0) < 300) {
      wrapper.appendChild(prev);
    }
    wrapper.appendChild(target);
  }
  const toggle = body.ownerDocument.createElement('button');
  toggle.className = 'env-quote-toggle';
  toggle.type = 'button';
  toggle.title = 'Show quoted text';
  toggle.textContent = '•••';
  wrapper.parentElement?.insertBefore(toggle, wrapper);
}

function baseCss(dark: boolean, designed: boolean, darken: boolean): string {
  const textColor = dark ? '#e6e6eb' : '#1d1e24';
  const linkColor = dark ? '#a99cff' : '#5b48e8';
  const quoteBorder = dark ? '#3a3b44' : '#d6d8de';
  const adaptPlain = !designed;
  // The document's color scheme must match the iframe element's, otherwise
  // engines paint an opaque canvas behind the message.
  const scheme = designed || !dark ? 'light' : 'dark';
  return `
:root{color-scheme:${scheme};}
html,body{margin:0;padding:0;}
body{font:14px/1.6 'Inter Variable',Inter,-apple-system,'Segoe UI',Roboto,'Noto Sans',sans-serif;word-wrap:break-word;overflow-wrap:anywhere;
  ${adaptPlain ? `color:${textColor};background:transparent;` : 'color:#1d1e24;background:#ffffff;'}
  -webkit-font-smoothing:antialiased;}
body.plain{padding:2px 0;}
body.designed{border-radius:10px;}
${designed && dark && darken ? 'html{filter:invert(.9) hue-rotate(180deg);} img,video,picture,[style*="background-image"]{filter:invert(1) hue-rotate(180deg);}' : ''}
a{color:${adaptPlain ? linkColor : '#3b5bdb'};}
img{max-width:100%;height:auto;}
table{max-width:100%;}
pre,code{white-space:pre-wrap;font-family:'JetBrains Mono',ui-monospace,monospace;font-size:12.5px;}
blockquote{margin:6px 0 6px 2px;padding-left:12px;border-left:2px solid ${quoteBorder};color:${adaptPlain ? (dark ? '#a0a3ae' : '#5d6170') : 'inherit'};}
.env-plain{white-space:pre-wrap;font:inherit;margin:0;}
.env-blocked{display:inline-block;min-width:24px;min-height:24px;background:${dark ? '#24252c' : '#eef0f3'};border-radius:6px;}
.env-quote-toggle{all:unset;cursor:pointer;display:inline-block;margin:8px 0 2px;padding:0 9px;height:18px;line-height:16px;border-radius:9px;
  font-size:13px;letter-spacing:1px;color:${dark ? '#a0a3ae' : '#6b6f7c'};background:${dark ? '#2a2b33' : '#eceef2'};}
.env-quote-toggle:hover{background:${dark ? '#34353e' : '#e1e4ea'};}
.env-quote[hidden]{display:none;}
`;
}

const URL_RE = /\b((?:https?:\/\/|www\.)[^\s<>"')\]]+[^\s<>"').,;:!?\]])/gi;
const EMAIL_RE = /\b([A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,})\b/gi;

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/** Plain text → HTML with links, and quoted lines as blockquotes. */
export function textToHtml(text: string): string {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const out: string[] = [];
  let quote: string[] = [];
  const flush = () => {
    if (quote.length) {
      out.push(`<blockquote type="cite">${textToHtml(quote.join('\n'))}</blockquote>`);
      quote = [];
    }
  };
  let para: string[] = [];
  const flushPara = () => {
    if (para.length) {
      out.push(`<div class="env-plain">${para.map(linkify).join('\n')}</div>`);
      para = [];
    }
  };
  for (const line of lines) {
    const m = /^\s*>\s?(.*)$/.exec(line);
    if (m) {
      flushPara();
      quote.push(m[1]);
    } else {
      flush();
      para.push(line);
    }
  }
  flushPara();
  flush();
  return out.join('');
}

function linkify(line: string): string {
  const escaped = escapeHtml(line);
  return escaped
    .replace(URL_RE, (url) => {
      const href = url.startsWith('www.') ? `https://${url}` : url;
      return `<a href="${href}">${url}</a>`;
    })
    .replace(EMAIL_RE, (e) => (escaped.includes(`href="mailto:${e}`) ? e : `<a href="mailto:${e}">${e}</a>`));
}

/** Plain-text excerpt of HTML (for AI context and previews). */
export function htmlToText(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  doc.querySelectorAll('style,script,head').forEach((n) => n.remove());
  return (doc.body?.innerText || doc.body?.textContent || '').replace(/\n{3,}/g, '\n\n').trim();
}

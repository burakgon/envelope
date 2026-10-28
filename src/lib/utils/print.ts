import { api } from '../api';
import type { Address, MessageView } from '../api';
import { prepareHtml, textToHtml } from './html';

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
const people = (list: Address[]) => list.map((a) => esc(a.name ? `${a.name} <${a.email}>` : a.email)).join(', ');

/** Prints one message (headers and body) through the system print dialog. */
export function printMessage(message: MessageView, allowRemote: boolean) {
  const body = message.body;
  const raw = body?.html?.trim() ? body.html : textToHtml(body?.text ?? message.snippet ?? '');
  const prepared = prepareHtml(raw, {
    allowRemote,
    inlineUrl: (cid) => api.inlineUrl(message.id, cid),
    dark: false,
    darkenDesigned: false,
    collapseQuotes: false,
  });
  const date = new Date(message.date).toLocaleString(undefined, { dateStyle: 'full', timeStyle: 'short' });
  const rows = [
    ['From', message.from ? people([message.from]) : ''],
    ['To', people(message.to)],
    ['Cc', people(message.cc)],
    ['Date', esc(date)],
  ].filter(([, v]) => v);
  const header = `<div class="env-print-head"><h1>${esc(message.subject || '(no subject)')}</h1><table>${rows
    .map(([k, v]) => `<tr><th>${k}</th><td>${v}</td></tr>`)
    .join('')}</table></div>`;
  const style = `<style>
    @page { margin: 16mm; }
    .env-print-head { font: 13px/1.45 Inter, system-ui, sans-serif; color: #111; border-bottom: 1px solid #ccc; padding-bottom: 10px; margin-bottom: 16px; }
    .env-print-head h1 { font-size: 19px; margin: 0 0 8px; }
    .env-print-head th { text-align: left; color: #666; font-weight: 500; padding-right: 12px; vertical-align: top; }
  </style>`;
  const html = prepared.html.includes('<body')
    ? prepared.html.replace(/<body([^>]*)>/i, `<body$1>${style}${header}`)
    : `${style}${header}${prepared.html}`;

  const frame = document.createElement('iframe');
  frame.style.cssText = 'position:fixed;right:0;bottom:0;width:0;height:0;border:0;visibility:hidden';
  frame.srcdoc = html;
  frame.onload = () => {
    // Give inline images a moment to decode before the print snapshot.
    setTimeout(() => {
      frame.contentWindow?.focus();
      frame.contentWindow?.print();
      setTimeout(() => frame.remove(), 1000);
    }, 250);
  };
  document.body.appendChild(frame);
}

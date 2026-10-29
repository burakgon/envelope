<script lang="ts" module>
  /** Heights messages were measured at, so that one opened again starts at its size. */
  const heights = new Map<string, number>();

  /**
   * A first guess at a message's height before its frame has laid it out, from its text: on
   * the low side, as growing once is less of a jump than shrinking. Quoted history is left
   * out, as it starts folded away.
   */
  function estimate(html: string | null, text: string | null, width: number): number {
    const perLine = Math.max(30, Math.floor(width / 7.6));
    let lines = 0;
    if (html) {
      const own = html.split(/<blockquote|class="(?:gmail_quote|env-quote|envelope-quote)"|id="divRplyFwdMsg"/i)[0];
      const breaks = (own.match(/<(?:p|div|br|li|tr|h[1-6])\b/gi) ?? []).length;
      const chars = own.replace(/<style[\s\S]*?<\/style>/gi, '').replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').length;
      lines = Math.ceil(chars / perLine) + Math.ceil(breaks * 0.6);
    } else if (text) {
      for (const line of text.split('\n')) {
        if (/^\s*>/.test(line)) continue;
        lines += Math.max(1, Math.ceil(line.length / perLine));
      }
    }
    return Math.round(Math.min(900, Math.max(40, lines * 22.4 * 0.85 + 8)));
  }
</script>

<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { api } from '../api';
  import type { MessageView } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { ui } from '../stores/ui.svelte';
  import { prepareHtml, textToHtml } from '../utils/html';
  import { t } from '../i18n';

  let {
    message,
    allowRemote,
    onstats,
    onimage,
  }: {
    message: MessageView;
    allowRemote: boolean;
    onstats?: (s: { blocked: number; trackers: number; designed: boolean }) => void;
    /** A picture in the message was clicked: all pictures, and the clicked one's index. */
    onimage?: (images: { src: string; filename: string }[], index: number) => void;
  } = $props();

  let frame: HTMLIFrameElement | undefined = $state();
  let observer: ResizeObserver | null = null;

  const prepared = $derived.by(() => {
    const body = message.body;
    const raw = body?.html?.trim() ? body.html : textToHtml(body?.text ?? message.snippet ?? '');
    return prepareHtml(raw, {
      allowRemote,
      inlineUrl: (cid) => api.inlineUrl(message.id, cid),
      dark: ui.isDark,
      darkenDesigned: ui.settings.darkEmails && ui.isDark,
      collapseQuotes: true,
    });
  });
  const key = $derived(`${message.id}:${prepared.html.length}`);
  let height = $state(untrack(() => heights.get(key) ?? estimate(message.body?.html ?? null, message.body?.text ?? message.snippet, 640)));
  let measured = false;

  $effect(() => {
    onstats?.({ blocked: prepared.blockedRemote, trackers: prepared.trackers, designed: prepared.designed });
  });

  function measure() {
    const doc = frame?.contentDocument;
    if (!doc?.documentElement || !doc.body) return;
    // The content's own height, which can be less than the frame's (a quote folded away
    // again): the document's scroll height never is.
    const h = Math.ceil(Math.max(doc.documentElement.getBoundingClientRect().height, doc.body.scrollHeight));
    if (h <= 0) return;
    measured = true;
    heights.set(key, h);
    if (Math.abs(h - height) > 1) height = h;
  }

  onMount(() => {
    // The real width makes a better guess, before the frame is drawn.
    if (!heights.has(key) && frame?.clientWidth) height = estimate(message.body?.html ?? null, message.body?.text ?? message.snippet, frame.clientWidth);
    // Measure as soon as the frame has laid the message out, without waiting for `load`.
    let tries = 0;
    const early = () => {
      if (measured || !frame || tries++ > 60) return;
      const doc = frame.contentDocument;
      if (doc?.body && doc.readyState !== 'loading' && doc.URL !== 'about:blank') measure();
      if (!measured) requestAnimationFrame(early);
    };
    requestAnimationFrame(early);
  });

  function onLoad() {
    const doc = frame?.contentDocument;
    if (!doc) return;
    measure();
    observer?.disconnect();
    observer = new ResizeObserver(measure);
    if (doc.body) observer.observe(doc.body);
    const zoomable = (img: HTMLImageElement) => {
      if (onimage && img.naturalWidth >= 120 && img.naturalHeight >= 80 && !img.closest('a')) img.style.cursor = 'zoom-in';
    };
    doc.querySelectorAll('img').forEach((img) => {
      if (img.complete) zoomable(img);
      img.addEventListener('load', () => (measure(), zoomable(img)), { once: true });
    });

    doc.addEventListener('click', (e) => {
      const target = e.target as Element | null;
      const toggle = target?.closest('.env-quote-toggle');
      if (toggle) {
        e.preventDefault();
        const quote = toggle.nextElementSibling as HTMLElement | null;
        if (quote) quote.hidden = !quote.hidden;
        requestAnimationFrame(measure);
        return;
      }
      const a = target?.closest('a') as HTMLAnchorElement | null;
      if (!a) {
        // Pictures (not icons, not links) open in the viewer.
        // The frame is another realm, so `instanceof HTMLImageElement` would be false here.
        const img = target?.tagName === 'IMG' ? (target as HTMLImageElement) : null;
        if (img && onimage && img.naturalWidth >= 120 && img.naturalHeight >= 80) {
          const pics = [...doc.images].filter((i) => i.naturalWidth >= 120 && i.naturalHeight >= 80 && !i.closest('a'));
          onimage(
            pics.map((i, n) => ({ src: i.currentSrc || i.src, filename: i.alt?.trim() || t('mailFrame.picture', { number: n + 1 }) })),
            Math.max(0, pics.indexOf(img)),
          );
        }
        return;
      }
      const href = a.getAttribute('href') ?? '';
      e.preventDefault();
      if (href.startsWith('#')) {
        doc.getElementById(href.slice(1))?.scrollIntoView();
        return;
      }
      if (/^mailto:/i.test(href)) compose.fromMailto(href);
      else if (/^webcals?:/i.test(href)) cal.subscribing = href;
      else if (/^(https?:|tel:)/i.test(href)) api.openUrl(href);
    });

    // Keep keyboard shortcuts working while the message has focus.
    doc.addEventListener('keydown', (e) => {
      const copy = new KeyboardEvent('keydown', {
        key: e.key,
        code: e.code,
        ctrlKey: e.ctrlKey,
        metaKey: e.metaKey,
        shiftKey: e.shiftKey,
        altKey: e.altKey,
        bubbles: true,
        cancelable: true,
      });
      if (!window.dispatchEvent(copy)) e.preventDefault();
    });
  }

  onDestroy(() => observer?.disconnect());
</script>

<!--
  WebKitGTK only runs the listeners we attach to the message document (links, quote toggles,
  pictures, shortcuts) when the sandbox allows scripts. The message itself still cannot run any:
  prepareHtml puts a `default-src 'none'` CSP at the top of the document and DOMPurify strips
  scripts and handlers.
-->
<iframe
  bind:this={frame}
  class="frame"
  class:designed={prepared.designed}
  title={message.subject}
  sandbox="allow-same-origin allow-scripts allow-popups allow-popups-to-escape-sandbox"
  srcdoc={prepared.html}
  style:height="{height}px"
  onload={onLoad}
></iframe>

<style>
  .frame {
    display: block;
    width: 100%;
    border: none;
    background: transparent;
  }
  .frame.designed {
    color-scheme: light;
    border-radius: 10px;
    background: #fff;
    box-shadow: 0 0 0 1px var(--border);
  }
</style>

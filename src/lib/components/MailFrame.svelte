<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api } from '../api';
  import type { MessageView } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { ui } from '../stores/ui.svelte';
  import { prepareHtml, textToHtml } from '../utils/html';

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
  let height = $state(80);
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

  $effect(() => {
    onstats?.({ blocked: prepared.blockedRemote, trackers: prepared.trackers, designed: prepared.designed });
  });

  function measure() {
    const doc = frame?.contentDocument;
    if (!doc?.documentElement) return;
    const h = Math.max(doc.documentElement.scrollHeight, doc.body?.scrollHeight ?? 0);
    if (h > 0 && Math.abs(h - height) > 1) height = h;
  }

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
            pics.map((i, n) => ({ src: i.currentSrc || i.src, filename: i.alt?.trim() || `Picture ${n + 1}` })),
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

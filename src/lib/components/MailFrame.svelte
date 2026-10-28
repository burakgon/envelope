<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api } from '../api';
  import type { MessageView } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { ui } from '../stores/ui.svelte';
  import { prepareHtml, textToHtml } from '../utils/html';

  let {
    message,
    allowRemote,
    onstats,
  }: {
    message: MessageView;
    allowRemote: boolean;
    onstats?: (s: { blocked: number; trackers: number; designed: boolean }) => void;
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
    doc.querySelectorAll('img').forEach((img) => img.addEventListener('load', measure, { once: true }));

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
      if (!a) return;
      const href = a.getAttribute('href') ?? '';
      e.preventDefault();
      if (href.startsWith('#')) {
        doc.getElementById(href.slice(1))?.scrollIntoView();
        return;
      }
      if (/^mailto:/i.test(href)) compose.fromMailto(href);
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

<iframe
  bind:this={frame}
  class="frame"
  class:designed={prepared.designed}
  title={message.subject}
  sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
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

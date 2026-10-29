<script lang="ts">
  import { marked } from 'marked';
  import DOMPurify from 'dompurify';
  import { api } from '../api';

  let { text, streaming = false, oncite }: { text: string; streaming?: boolean; oncite?: (n: number) => void } = $props();

  const html = $derived.by(() => {
    const raw = marked.parse(text, { async: false, gfm: true, breaks: true }) as string;
    const withCites = raw.replace(/\[(\d{1,2})\]/g, '<button class="cite" data-cite="$1">$1</button>');
    // Model output can echo text from untrusted mail: no images (they could leak data), no forms.
    return DOMPurify.sanitize(withCites, {
      ADD_ATTR: ['data-cite'],
      ADD_TAGS: ['button'],
      FORBID_TAGS: ['img', 'picture', 'video', 'audio', 'source', 'style', 'form', 'input', 'textarea', 'select', 'iframe'],
    });
  });

  function click(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest('[data-cite]');
    if (el) return oncite?.(Number(el.getAttribute('data-cite')));
    const a = (e.target as HTMLElement).closest('a[href]');
    if (a) {
      e.preventDefault();
      const href = a.getAttribute('href') ?? '';
      if (/^(https?:|mailto:)/i.test(href)) api.openUrl(href);
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="md selectable" class:streaming onclick={click}>{@html html}</div>

<style>
  .md {
    line-height: 1.6;
    font-size: var(--text-md);
  }
  .md :global(p) {
    margin: 0 0 0.6em;
  }
  .md :global(p:last-child) {
    margin-bottom: 0;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0.3em 0 0.6em;
    padding-left: 20px;
  }
  .md :global(li) {
    margin: 0.15em 0;
  }
  .md :global(strong) {
    font-weight: 650;
  }
  .md :global(a) {
    color: var(--accent-text);
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 12.5px;
    padding: 1px 5px;
    border-radius: 5px;
    background: var(--bg-active);
  }
  .md :global(.cite) {
    display: inline-grid;
    place-items: center;
    min-width: 17px;
    height: 17px;
    padding: 0 4px;
    margin: 0 1px;
    border-radius: 6px;
    font-size: 10.5px;
    font-weight: 700;
    vertical-align: 1px;
    color: var(--accent-text);
    background: var(--accent-soft);
    cursor: pointer;
  }
  .md :global(.cite:hover) {
    background: var(--accent-solid);
    color: var(--accent-fg);
  }
  .streaming :global(> :last-child)::after {
    content: '';
    display: inline-block;
    width: 7px;
    height: 14px;
    margin-left: 2px;
    vertical-align: -2px;
    border-radius: 2px;
    background: var(--accent);
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0;
    }
  }
</style>

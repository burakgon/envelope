<script lang="ts">
  import { api } from '../api';
  import type { AttachmentInfo } from '../api';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { fileIconKind, fileSize } from '../utils/format';
  import { Download, File, FileArchive, FileSheet, FileSlides, FileText, Image, Music, Video, Calendar, Code, Spinner } from '../icons';

  let { att, onpreview }: { att: AttachmentInfo; onpreview?: (att: AttachmentInfo) => void } = $props();
  let busy = $state(false);

  const kind = $derived(fileIconKind(att.mimeType, att.filename));
  const icons = { image: Image, pdf: FileText, doc: FileText, sheet: FileSheet, slides: FileSlides, archive: FileArchive, audio: Music, video: Video, calendar: Calendar, code: Code, file: File };
  const colors: Record<string, number> = { image: 200, pdf: 25, doc: 255, sheet: 150, slides: 45, archive: 300, audio: 330, video: 0, calendar: 280, code: 180, file: 260 };
  const Icon = $derived(icons[kind]);
  const ext = $derived((att.filename.split('.').pop() ?? '').toUpperCase().slice(0, 4));
  const thumb = $derived(kind === 'image' && !api.isDemo ? api.attachmentUrl(att.id) : null);

  async function open() {
    if (thumb && onpreview) return onpreview(att);
    busy = true;
    try {
      await api.attachmentOpen(att.id);
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  async function save(e: MouseEvent) {
    e.stopPropagation();
    const target = await api.pickSavePath(att.filename);
    if (!target) return;
    busy = true;
    try {
      await api.attachmentSave(att.id, target);
      toasts.show(`Saved ${att.filename}`, { kind: 'success', action: { label: 'Show', run: () => api.attachmentReveal(att.id) } });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }
</script>

<div class="tile" role="button" tabindex="0" onclick={open} onkeydown={(e) => e.key === 'Enter' && open()} title={att.filename}>
  {#if thumb}
    <img class="thumb" src={thumb} alt="" loading="lazy" />
  {:else}
    <span class="icon" style:--h={colors[kind]}>
      <Icon size={18} />
      {#if ext && kind !== 'image'}<span class="ext">{ext}</span>{/if}
    </span>
  {/if}
  <span class="meta">
    <span class="name truncate">{att.filename}</span>
    <span class="size">{fileSize(att.size)}</span>
  </span>
  <button class="dl" title="Save as…" onclick={save}>
    {#if busy}<span class="spin"><Spinner size={15} /></span>{:else}<Download size={15} />{/if}
  </button>
</div>

<style>
  .tile {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 220px;
    height: 52px;
    padding: 0 8px 0 8px;
    border-radius: 11px;
    background: var(--bg-elevated);
    box-shadow: 0 0 0 1px var(--border), var(--shadow-xs);
    transition: box-shadow var(--dur-fast), transform var(--dur-fast);
  }
  .tile:hover {
    box-shadow: 0 0 0 1px var(--border-strong), var(--shadow-md);
  }
  .tile:active {
    transform: scale(0.99);
  }
  .icon {
    position: relative;
    width: 36px;
    height: 36px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    flex-shrink: 0;
    color: oklch(0.55 0.17 var(--h));
    background: oklch(0.6 0.17 var(--h) / 0.12);
  }
  .ext {
    position: absolute;
    bottom: -3px;
    right: -4px;
    font-size: 8px;
    font-weight: 750;
    padding: 0 3px;
    border-radius: 4px;
    color: #fff;
    background: oklch(0.58 0.17 var(--h));
  }
  .thumb {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    object-fit: cover;
    flex-shrink: 0;
  }
  .meta {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name {
    font-weight: 550;
    font-size: var(--text-sm);
  }
  .size {
    font-size: 11px;
    color: var(--text-3);
  }
  .dl {
    width: 28px;
    height: 28px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    color: var(--text-3);
    opacity: 0;
    transition: opacity var(--dur-fast);
  }
  .tile:hover .dl {
    opacity: 1;
  }
  .dl:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .spin {
    display: grid;
    animation: spin 0.9s linear infinite;
  }
</style>

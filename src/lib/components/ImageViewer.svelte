<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { AttachmentInfo } from '../api';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { fileSize } from '../utils/format';
  import { t } from '../i18n';
  import { ChevronLeft, ChevronRight, Download, ExternalLink, X } from '../icons';

  type Item = AttachmentInfo | { id?: undefined; src: string; filename: string; size?: undefined };
  let { items, start = 0, onclose }: { items: Item[]; start?: number; onclose: () => void } = $props();

  let index = $state(0);
  let zoomed = $state(false);
  let root: HTMLDivElement | undefined = $state();
  const current = $derived(items[index]);

  onMount(() => {
    index = Math.min(Math.max(start, 0), items.length - 1);
    root?.focus();
  });

  function go(step: number) {
    if (items.length < 2) return;
    index = (index + step + items.length) % items.length;
    zoomed = false;
  }

  const src = (item: Item) => (item.id != null ? api.attachmentUrl(item.id) : item.src);

  async function save() {
    if (current.id == null) return;
    const id = current.id;
    const target = await api.pickSavePath(current.filename);
    if (!target) return;
    try {
      await api.attachmentSave(id, target);
      toasts.show(t('attachment.saved', { file: current.filename }), { kind: 'success', action: { label: t('attachment.showInFolder'), run: () => api.attachmentReveal(id) } });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  function key(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') onclose();
    else if (e.key === 'ArrowRight') go(1);
    else if (e.key === 'ArrowLeft') go(-1);
    else if (e.key === ' ' || e.key === 'z') {
      e.preventDefault();
      zoomed = !zoomed;
    }
  }
</script>

<div class="viewer" role="dialog" aria-label={current?.filename} tabindex="-1" bind:this={root} onkeydown={key}>
  <header>
    <div class="name">
      <b class="truncate">{current?.filename}</b>
      <span>{[current?.size != null ? fileSize(current.size) : '', items.length > 1 ? t('imageViewer.position', { current: index + 1, total: items.length }) : ''].filter(Boolean).join(' · ')}</span>
    </div>
    {#if current?.id != null}
      {@const id = current.id}
      <button class="tool" title={t('attachment.saveAs')} onclick={save}><Download size={18} /></button>
      <button class="tool" title={t('imageViewer.openWith')} onclick={() => api.attachmentOpen(id).catch((e) => toasts.error(errorMessage(e)))}><ExternalLink size={18} /></button>
    {/if}
    <button class="tool" title={t('imageViewer.close', { key: t('keys.esc') })} onclick={onclose}><X size={20} /></button>
  </header>

  <div class="stage" class:zoomed role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
    {#key current ? src(current) : ''}
      <button class="zoom" aria-label={zoomed ? t('imageViewer.fitToWindow') : t('imageViewer.actualSize')} onclick={() => (zoomed = !zoomed)}>
        <img src={src(current)} alt={current.filename} draggable="false" />
      </button>
    {/key}
  </div>

  {#if items.length > 1}
    <button class="nav prev" aria-label={t('imageViewer.previous')} onclick={() => go(-1)}><ChevronLeft size={26} /></button>
    <button class="nav next" aria-label={t('imageViewer.next')} onclick={() => go(1)}><ChevronRight size={26} /></button>
  {/if}
</div>

<style>
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 1600;
    display: flex;
    flex-direction: column;
    background: rgb(12 12 16 / 0.92);
    backdrop-filter: blur(6px);
    color: #f2f2f5;
    animation: fade-in var(--dur-fast);
    outline: none;
  }
  header {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 56px;
    padding: 0 12px 0 20px;
    flex-shrink: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name span {
    font-size: var(--text-sm);
    color: rgb(255 255 255 / 0.6);
  }
  .tool {
    width: 38px;
    height: 38px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    color: rgb(255 255 255 / 0.85);
  }
  .tool:hover {
    background: rgb(255 255 255 / 0.12);
    color: white;
  }
  .stage {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 8px 72px 32px;
    overflow: hidden;
  }
  .zoom {
    display: contents;
  }
  .stage img {
    max-width: 100%;
    max-height: calc(100vh - 100px);
    object-fit: contain;
    border-radius: 6px;
    box-shadow: 0 20px 60px rgb(0 0 0 / 0.5);
    cursor: zoom-in;
    animation: pop-in var(--dur) var(--ease-out);
  }
  .stage.zoomed {
    overflow: auto;
    place-items: start center;
  }
  .stage.zoomed img {
    max-width: none;
    max-height: none;
    cursor: zoom-out;
  }
  .nav {
    position: absolute;
    top: 50%;
    width: 48px;
    height: 48px;
    margin-top: -24px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: white;
    background: rgb(255 255 255 / 0.1);
    transition: background var(--dur-fast);
  }
  .nav:hover {
    background: rgb(255 255 255 / 0.2);
  }
  .prev {
    left: 14px;
  }
  .next {
    right: 14px;
  }
</style>

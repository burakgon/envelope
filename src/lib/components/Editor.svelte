<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { Editor } from '@tiptap/core';
  import StarterKit from '@tiptap/starter-kit';
  import Image from '@tiptap/extension-image';
  import { TextStyle, Color, BackgroundColor, FontSize } from '@tiptap/extension-text-style';
  import TextAlign from '@tiptap/extension-text-align';
  import { Placeholder } from '@tiptap/extensions';
  import {
    AlignCenter, AlignJustify, AlignLeft, AlignRight, Baseline, Bold, Code, FontSize as SizeIcon, Highlighter, ImagePlus, Italic,
    Link, List, ListOrdered, Quote, Redo, RemoveFormatting, Strikethrough, Type, Underline, Undo,
  } from '../icons';
  import { t } from '../i18n';
  import { dialogs } from '../stores/dialogs.svelte';
  import { toasts } from '../stores/toasts.svelte';
  import Popover from './Popover.svelte';
  import { Signature, signatureRange, focusAboveSignature, written } from './editor-signature';

  let {
    html,
    placeholder,
    onupdate,
    focus = 0,
    showToolbar = false,
    onkeysend,
    onpastefiles,
    editorRef = $bindable(),
  }: {
    html: string;
    placeholder?: string;
    onupdate: (html: string) => void;
    focus?: number;
    showToolbar?: boolean;
    onkeysend?: () => void;
    onpastefiles?: (files: File[]) => void;
    editorRef?: Editor | null;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let picker: HTMLInputElement | undefined = $state();
  let editor: Editor | null = null;
  let tick = $state(0);
  /** An open choice of colors, sizes or alignments, under its button. */
  let menu = $state<{ kind: 'color' | 'highlight' | 'size' | 'align'; x: number; y: number } | null>(null);
  const placeholderText = $derived(placeholder ?? t('editor.placeholder'));

  /** Pictures larger than this would make the message heavy to send and to open. */
  const MAX_PICTURE = 10 * 1024 * 1024;

  onMount(() => {
    editor = new Editor({
      element: el!,
      content: html,
      extensions: [
        StarterKit.configure({
          link: { openOnClick: false, autolink: true, linkOnPaste: true, HTMLAttributes: { rel: 'noopener noreferrer' } },
          heading: { levels: [1, 2, 3] },
        }),
        Image.configure({ inline: true, allowBase64: true }),
        TextStyle,
        Color,
        BackgroundColor,
        FontSize,
        TextAlign.configure({ types: ['heading', 'paragraph'] }),
        Signature,
        Placeholder.configure({ placeholder: () => placeholderText }),
      ],
      editorProps: {
        attributes: { class: 'prose', spellcheck: 'true' },
        handleKeyDown: (_view, event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
            event.preventDefault();
            onkeysend?.();
            return true;
          }
          if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
            event.preventDefault();
            setLink();
            return true;
          }
          return false;
        },
        handlePaste: (_view, event) => {
          const files = [...(event.clipboardData?.files ?? [])];
          if (files.length && onpastefiles) {
            const images = files.filter((f) => f.type.startsWith('image/'));
            const others = files.filter((f) => !f.type.startsWith('image/'));
            insertPictures(images);
            if (others.length) onpastefiles(others);
            return true;
          }
          return false;
        },
      },
      onUpdate: ({ editor }) => onupdate(editor.getHTML()),
      // Also while the composer is being taken down: removing the focused editor blurs it.
      onTransaction: () => untrack(() => tick++),
    });
    editorRef = editor;
  });

  onDestroy(() => editor?.destroy());

  // Redraw the placeholder when its text (or the interface language) changes.
  // Untracked: the transaction bumps `tick`, which must not re-run this effect.
  $effect(() => {
    void placeholderText;
    untrack(() => {
      if (editor && !editor.isDestroyed) editor.view.dispatch(editor.state.tr);
    });
  });

  $effect(() => {
    if (focus && editor) {
      // Put the caret at the start, above any signature or quote.
      setTimeout(() => editor?.chain().focus('start').run(), 30);
    }
  });

  /**
   * A click below the signature writes above it, at the end of the text. So does a click on the
   * signature while nothing is written yet: the empty message is where people click to begin,
   * and what they type must not join the signature's last line.
   */
  function onMouseDown(e: MouseEvent) {
    if (!editor || e.button !== 0 || e.shiftKey || e.detail > 1) return;
    const range = signatureRange(editor);
    if (!range) return;
    const pos = editor.view.posAtCoords({ left: e.clientX, top: e.clientY })?.pos ?? editor.state.doc.content.size;
    const below = pos >= range.to;
    const onSignature = pos > range.from && pos < range.to;
    if (!below && !(onSignature && !written(editor, 0, range.from))) return;
    e.preventDefault();
    focusAboveSignature(editor);
  }

  function insertPictures(files: File[]) {
    for (const file of files) {
      if (file.size > MAX_PICTURE) {
        toasts.error(t('composer.fileTooLarge', { name: file.name }));
        continue;
      }
      const reader = new FileReader();
      reader.onload = () => editor?.chain().focus().setImage({ src: String(reader.result), alt: file.name }).run();
      reader.readAsDataURL(file);
    }
  }

  async function setLink() {
    if (!editor) return;
    const prev = editor.getAttributes('link').href as string | undefined;
    const url = await dialogs.prompt({ title: t('editor.linkUrl'), value: prev ?? 'https://', type: 'url' });
    if (url === null || !editor || editor.isDestroyed) return;
    if (url === '' || url === 'https://') editor.chain().focus().extendMarkRange('link').unsetLink().run();
    else editor.chain().focus().extendMarkRange('link').setLink({ href: url }).run();
  }

  const is = (name: string | Record<string, unknown>, attrs?: Record<string, unknown>) => {
    void tick;
    if (typeof name !== 'string') return editor?.isActive(name) ?? false;
    return editor?.isActive(name, attrs) ?? false;
  };
  const style = (key: 'color' | 'backgroundColor' | 'fontSize'): string | null => {
    void tick;
    return (editor?.getAttributes('textStyle')[key] as string | undefined) ?? null;
  };

  /** "Ctrl+B" */
  const ctrl = (key: string) => `${t('keys.ctrl')}+${key}`;

  // Text colors that read on white (where most people read mail), and soft highlights.
  const colors = ['#d93025', '#e8710a', '#188038', '#1a73e8', '#9334e6', '#c2185b', '#5f6368'];
  const highlights = ['#fff59d', '#ffe0b2', '#c8e6c9', '#bbdefb', '#e1bee7', '#f8bbd0'];
  const sizes = $derived([
    { label: t('editor.sizeSmall'), value: '12px' },
    { label: t('editor.sizeNormal'), value: null },
    { label: t('editor.sizeLarge'), value: '18px' },
    { label: t('editor.sizeHuge'), value: '24px' },
  ]);
  const alignments = $derived([
    { icon: AlignLeft, label: t('editor.alignLeft'), value: 'left' },
    { icon: AlignCenter, label: t('editor.alignCenter'), value: 'center' },
    { icon: AlignRight, label: t('editor.alignRight'), value: 'right' },
    { icon: AlignJustify, label: t('editor.alignJustify'), value: 'justify' },
  ]);
  const alignment = $derived.by(() => {
    void tick;
    return alignments.find((a) => a.value !== 'left' && is({ textAlign: a.value })) ?? alignments[0];
  });

  function openMenu(kind: 'color' | 'highlight' | 'size' | 'align', e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = menu?.kind === kind ? null : { kind, x: r.left, y: r.bottom + 4 };
  }

  function apply(run: (e: Editor) => void) {
    menu = null;
    if (editor) run(editor);
  }

  const tools = $derived([
    { icon: Bold, title: t('editor.bold', { shortcut: ctrl('B') }), active: () => is('bold'), run: () => editor?.chain().focus().toggleBold().run() },
    { icon: Italic, title: t('editor.italic', { shortcut: ctrl('I') }), active: () => is('italic'), run: () => editor?.chain().focus().toggleItalic().run() },
    { icon: Underline, title: t('editor.underline', { shortcut: ctrl('U') }), active: () => is('underline'), run: () => editor?.chain().focus().toggleUnderline().run() },
    { icon: Strikethrough, title: t('editor.strikethrough'), active: () => is('strike'), run: () => editor?.chain().focus().toggleStrike().run() },
  ]);
  const blocks = $derived([
    { icon: Type, title: t('editor.heading'), active: () => is('heading'), run: () => editor?.chain().focus().toggleHeading({ level: 2 }).run() },
    { icon: Link, title: t('editor.link', { shortcut: ctrl('K') }), active: () => is('link'), run: setLink },
    { icon: List, title: t('editor.bulletedList'), active: () => is('bulletList'), run: () => editor?.chain().focus().toggleBulletList().run() },
    { icon: ListOrdered, title: t('editor.numberedList'), active: () => is('orderedList'), run: () => editor?.chain().focus().toggleOrderedList().run() },
    { icon: Quote, title: t('editor.quote'), active: () => is('blockquote'), run: () => editor?.chain().focus().toggleBlockquote().run() },
    { icon: Code, title: t('editor.code'), active: () => is('code'), run: () => editor?.chain().focus().toggleCode().run() },
  ]);
</script>

{#snippet button(icon: typeof Bold, title: string, active: boolean, run: (e: MouseEvent) => void, extra = '')}
  {@const Icon = icon}
  <button class="icon-btn sm {extra}" class:active {title} aria-label={title} onmousedown={(e) => e.preventDefault()} onclick={run}>
    <Icon size={15} />
  </button>
{/snippet}

{#if showToolbar}
  <div class="toolbar" role="toolbar" aria-label={t('composer.formatting')}>
    {#each tools as tool (tool.icon)}
      {@render button(tool.icon, tool.title, tool.active(), tool.run)}
    {/each}
    <span class="sep"></span>
    {@render button(SizeIcon, t('editor.fontSize'), !!style('fontSize'), (e) => openMenu('size', e))}
    <button class="icon-btn sm swatch-btn" class:active={!!style('color')} title={t('editor.textColor')} aria-label={t('editor.textColor')} onmousedown={(e) => e.preventDefault()} onclick={(e) => openMenu('color', e)}>
      <Baseline size={15} /><span class="bar" style:background={style('color') ?? 'currentColor'}></span>
    </button>
    <button class="icon-btn sm swatch-btn" class:active={!!style('backgroundColor')} title={t('editor.highlight')} aria-label={t('editor.highlight')} onmousedown={(e) => e.preventDefault()} onclick={(e) => openMenu('highlight', e)}>
      <Highlighter size={15} /><span class="bar" style:background={style('backgroundColor') ?? 'transparent'}></span>
    </button>
    <span class="sep"></span>
    {#each blocks as tool (tool.icon)}
      {@render button(tool.icon, tool.title, tool.active(), tool.run)}
    {/each}
    {@render button(alignment.icon, t('editor.alignment'), alignment.value !== 'left', (e) => openMenu('align', e))}
    <span class="sep"></span>
    {@render button(ImagePlus, t('editor.insertPicture'), false, () => picker?.click())}
    {@render button(RemoveFormatting, t('editor.clearFormatting'), false, () => editor?.chain().focus().unsetAllMarks().unsetTextAlign().run())}
    <span class="sep"></span>
    {@render button(Undo, t('common.undo'), false, () => editor?.chain().focus().undo().run())}
    {@render button(Redo, t('editor.redo'), false, () => editor?.chain().focus().redo().run())}
  </div>
  <input
    bind:this={picker}
    class="picker"
    type="file"
    accept="image/*"
    multiple
    tabindex="-1"
    aria-hidden="true"
    onchange={(e) => {
      const input = e.currentTarget;
      insertPictures([...(input.files ?? [])]);
      input.value = '';
    }}
  />
{/if}
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="editor" bind:this={el} onmousedown={onMouseDown}></div>

{#if menu}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={menu.kind === 'size' ? 170 : menu.kind === 'align' ? 128 : 248}>
    {#if menu.kind === 'color' || menu.kind === 'highlight'}
      {@const list = menu.kind === 'color' ? colors : highlights}
      {@const current = style(menu.kind === 'color' ? 'color' : 'backgroundColor')}
      <div class="menu-label">{menu.kind === 'color' ? t('editor.textColor') : t('editor.highlight')}</div>
      <div class="swatches">
        <button
          class="swatch none"
          class:on={!current}
          title={t('editor.noColor')}
          aria-label={t('editor.noColor')}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => apply((ed) => (menu?.kind === 'color' ? ed.chain().focus().unsetColor().run() : ed.chain().focus().unsetBackgroundColor().run()))}
        ></button>
        {#each list as c (c)}
          <button
            class="swatch"
            class:on={current?.toLowerCase() === c}
            style:--c={c}
            title={c}
            aria-label={c}
            onmousedown={(e) => e.preventDefault()}
            onclick={() => apply((ed) => (menu?.kind === 'color' ? ed.chain().focus().setColor(c).run() : ed.chain().focus().setBackgroundColor(c).run()))}
          ></button>
        {/each}
      </div>
    {:else if menu.kind === 'size'}
      {#each sizes as s (s.label)}
        <button
          class="menu-item size"
          class:on={(style('fontSize') ?? null) === s.value}
          style:font-size={s.value ?? 'var(--text-md)'}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => apply((ed) => (s.value ? ed.chain().focus().setFontSize(s.value).run() : ed.chain().focus().unsetFontSize().run()))}
        >{s.label}</button>
      {/each}
    {:else}
      <div class="aligns">
        {#each alignments as a (a.value)}
          {@render button(a.icon, a.label, alignment.value === a.value, () => apply((ed) => (a.value === 'left' ? ed.chain().focus().unsetTextAlign().run() : ed.chain().focus().setTextAlign(a.value).run())))}
        {/each}
      </div>
    {/if}
  </Popover>
{/if}

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 1px;
    padding: 4px;
    margin-bottom: 6px;
    border-radius: 10px;
    background: var(--bg-sunken);
  }
  .sep {
    width: 1px;
    height: 16px;
    margin: 0 4px;
    background: var(--border-strong);
  }
  .swatch-btn {
    position: relative;
  }
  .swatch-btn .bar {
    position: absolute;
    left: 7px;
    right: 7px;
    bottom: 3px;
    height: 3px;
    border-radius: 2px;
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .picker {
    display: none;
  }
  .swatches {
    display: grid;
    grid-template-columns: repeat(8, 22px);
    gap: 6px;
    padding: 4px 8px 8px;
  }
  .swatch {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--c);
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.12);
    transition: transform var(--dur-fast) var(--ease);
  }
  .swatch:hover {
    transform: scale(1.12);
  }
  .swatch.on {
    box-shadow: 0 0 0 2px var(--bg-elevated), 0 0 0 4px var(--accent);
  }
  .swatch.none {
    background:
      linear-gradient(135deg, transparent 46%, var(--red) 46%, var(--red) 54%, transparent 54%),
      var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .swatch.none.on {
    box-shadow: inset 0 0 0 1px var(--border-strong), 0 0 0 2px var(--bg-elevated), 0 0 0 4px var(--accent);
  }
  .menu-item.size {
    height: auto;
    min-height: 32px;
    line-height: 1.3;
  }
  .menu-item.size.on {
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .aligns {
    display: flex;
    gap: 2px;
    padding: 2px;
  }
  .editor {
    min-height: 120px;
    cursor: text;
  }
  .editor :global(.prose) {
    outline: none;
    font-size: var(--text-md);
    line-height: 1.6;
    min-height: 120px;
    padding: 4px 0;
    user-select: text;
    -webkit-user-select: text;
    word-wrap: break-word;
  }
  .editor :global(.prose p) {
    margin: 0 0 0.5em;
  }
  .editor :global(.prose blockquote) {
    margin: 0.5em 0;
    padding-left: 12px;
    border-left: 2px solid var(--border-strong);
    color: var(--text-2);
  }
  .editor :global(.prose a) {
    color: var(--accent-text);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .editor :global(.prose ul),
  .editor :global(.prose ol) {
    padding-left: 22px;
    margin: 0.3em 0;
  }
  .editor :global(.prose code) {
    font-family: var(--font-mono);
    font-size: 12.5px;
    padding: 1px 5px;
    border-radius: 5px;
    background: var(--bg-active);
  }
  .editor :global(.prose img) {
    max-width: 100%;
    border-radius: 8px;
  }
  .editor :global(.prose img.ProseMirror-selectednode) {
    outline: 2px solid var(--accent);
  }
  .editor :global(.prose h1),
  .editor :global(.prose h2),
  .editor :global(.prose h3) {
    margin: 0.6em 0 0.3em;
    line-height: 1.3;
  }
  /* Highlighted text without a color of its own stays dark on its light background in dark mode. */
  :global([data-theme='dark']) .editor :global(.prose span[style*='background-color']:not([style^='color']):not([style*=' color'])) {
    color: #17181c;
  }
  .editor :global(.prose p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    color: var(--text-3);
    pointer-events: none;
  }
  .editor :global(.envelope-signature),
  .editor :global(.prose .envelope-signature) {
    color: var(--text-2);
  }
  .editor :global(.prose .envelope-signature p) {
    margin: 0;
  }
</style>

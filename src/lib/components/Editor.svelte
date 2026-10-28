<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { Editor } from '@tiptap/core';
  import StarterKit from '@tiptap/starter-kit';
  import Image from '@tiptap/extension-image';
  import { Placeholder } from '@tiptap/extensions';
  import { Bold, Code, Italic, Link, List, ListOrdered, Quote, Strikethrough, Underline, Undo, Redo, Type } from '../icons';

  let {
    html,
    placeholder = 'Write your message…',
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
  let editor: Editor | null = null;
  let tick = $state(0);

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
        Placeholder.configure({ placeholder }),
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
            for (const img of images) {
              const reader = new FileReader();
              reader.onload = () => editor?.chain().focus().setImage({ src: String(reader.result) }).run();
              reader.readAsDataURL(img);
            }
            if (others.length) onpastefiles(others);
            return true;
          }
          return false;
        },
      },
      onUpdate: ({ editor }) => onupdate(editor.getHTML()),
      onTransaction: () => tick++,
    });
    editorRef = editor;
  });

  onDestroy(() => editor?.destroy());

  $effect(() => {
    if (focus && editor) {
      // Put the caret at the start, above any signature or quote.
      setTimeout(() => editor?.chain().focus('start').run(), 30);
    }
  });

  function setLink() {
    if (!editor) return;
    const prev = editor.getAttributes('link').href as string | undefined;
    const url = prompt('Link URL', prev ?? 'https://');
    if (url === null) return;
    if (url === '' || url === 'https://') editor.chain().focus().extendMarkRange('link').unsetLink().run();
    else editor.chain().focus().extendMarkRange('link').setLink({ href: url }).run();
  }

  const is = (name: string, attrs?: Record<string, unknown>) => {
    void tick;
    return editor?.isActive(name, attrs) ?? false;
  };

  const tools = [
    { icon: Bold, title: 'Bold (Ctrl+B)', active: () => is('bold'), run: () => editor?.chain().focus().toggleBold().run() },
    { icon: Italic, title: 'Italic (Ctrl+I)', active: () => is('italic'), run: () => editor?.chain().focus().toggleItalic().run() },
    { icon: Underline, title: 'Underline (Ctrl+U)', active: () => is('underline'), run: () => editor?.chain().focus().toggleUnderline().run() },
    { icon: Strikethrough, title: 'Strikethrough', active: () => is('strike'), run: () => editor?.chain().focus().toggleStrike().run() },
    { icon: Type, title: 'Heading', active: () => is('heading'), run: () => editor?.chain().focus().toggleHeading({ level: 2 }).run() },
    { icon: Link, title: 'Link (Ctrl+K)', active: () => is('link'), run: setLink },
    { icon: List, title: 'Bulleted list', active: () => is('bulletList'), run: () => editor?.chain().focus().toggleBulletList().run() },
    { icon: ListOrdered, title: 'Numbered list', active: () => is('orderedList'), run: () => editor?.chain().focus().toggleOrderedList().run() },
    { icon: Quote, title: 'Quote', active: () => is('blockquote'), run: () => editor?.chain().focus().toggleBlockquote().run() },
    { icon: Code, title: 'Code', active: () => is('code'), run: () => editor?.chain().focus().toggleCode().run() },
  ];
</script>

{#if showToolbar}
  <div class="toolbar" role="toolbar">
    {#each tools as t (t.title)}
      <button class="icon-btn sm" class:active={t.active()} title={t.title} onmousedown={(e) => e.preventDefault()} onclick={t.run}>
        <t.icon size={15} />
      </button>
    {/each}
    <span class="sep"></span>
    <button class="icon-btn sm" title="Undo" onmousedown={(e) => e.preventDefault()} onclick={() => editor?.chain().focus().undo().run()}><Undo size={15} /></button>
    <button class="icon-btn sm" title="Redo" onmousedown={(e) => e.preventDefault()} onclick={() => editor?.chain().focus().redo().run()}><Redo size={15} /></button>
  </div>
{/if}
<div class="editor" bind:this={el}></div>

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
  .editor :global(.prose h1),
  .editor :global(.prose h2),
  .editor :global(.prose h3) {
    margin: 0.6em 0 0.3em;
    line-height: 1.3;
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
</style>

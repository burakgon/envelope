<script lang="ts">
  // The menu of a folder or label in the sidebar (right-click, Shift+F10 or the Menu key), or of an
  // account's folder list (its heading): new folders, rename, delete, color, mark read, empty.
  import type { Account, Folder } from '../api';
  import Popover from './Popover.svelte';
  import { mail } from '../stores/mail.svelte';
  import {
    deleteFolder, emptyFolder, importInto, isCategory, isFixed, labelColors, newFolder, renameFolder, setFolderColor,
  } from '../stores/folders.svelte';
  import { Check, CheckCircle, FolderPlus, Pen, Plus, Trash, Upload, X } from '../icons';
  import { t } from '../i18n';

  let { x, y, account, folder, onclose }: { x: number; y: number; account: Account; folder: Folder | null; onclose: () => void } = $props();

  const gmail = $derived(account.kind === 'gmail');
  const label = $derived(folder?.kind === 'label');
  /** Folders can be made inside ordinary folders, Inbox and Archive (labels inside labels in Gmail). */
  const canHoldFolders = $derived(
    !!folder && !isCategory(folder) && (gmail ? folder.kind === 'label' && !folder.role : !folder.role || folder.role === 'inbox' || folder.role === 'archive'),
  );
  const empties = $derived(folder?.role === 'trash' || folder?.role === 'junk');
  let menu: HTMLDivElement | undefined = $state();

  $effect(() => {
    // Opened from the keyboard: arrows and Enter work from the first item.
    menu?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
  });

  /** Starts the action, then closes the menu (whose folder is gone once it is closed). */
  function run(fn: () => unknown) {
    fn();
    onclose();
  }

  function key(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    e.stopPropagation();
    const items = [...(menu?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length]?.focus();
  }
</script>

<Popover {x} {y} {onclose} width={236}>
  <div bind:this={menu} class="folder-menu" role="presentation" onkeydown={key}>
    <button class="menu-item" onclick={() => run(() => newFolder(account))}><Plus size={15} />{t(gmail ? 'folders.newLabel' : 'folders.newFolder')}</button>
    {#if folder && canHoldFolders}
      <button class="menu-item" onclick={() => run(() => newFolder(account, folder))}><FolderPlus size={15} />{t(gmail ? 'folders.newSublabel' : 'folders.newSubfolder')}</button>
    {/if}
    {#if folder}
      {#if !isFixed(folder) && !isCategory(folder)}
        <button class="menu-item" onclick={() => run(() => renameFolder(folder))}><Pen size={15} />{t('folders.renameItem')}</button>
      {/if}
      <div class="divider"></div>
      <button class="menu-item" disabled={folder.unreadCount === 0} onclick={() => run(() => mail.markAllRead(folder))}><CheckCircle size={15} />{t('threadList.markAllRead')}</button>
      <!-- Microsoft 365 takes no messages from apps. -->
      {#if account.kind !== 'microsoft' && !isCategory(folder) && folder.selectable}
        <button class="menu-item" onclick={() => run(() => importInto(folder))}><Upload size={15} />{t('folders.importMessages')}</button>
      {/if}
      {#if empties}
        <button class="menu-item danger" onclick={() => run(() => emptyFolder(folder))}>
          <Trash size={15} />{t(folder.role === 'trash' ? 'folders.emptyTrash' : 'folders.emptySpam')}
        </button>
      {/if}
      {#if label}
        <div class="menu-label">{t('folders.color')}</div>
        <div class="swatches" role="group" aria-label={t('folders.color')}>
          {#each labelColors as c (c.hex)}
            {@const name = t('folders.colorName', { color: c.name })}
            <button class="sw" style:background={c.hex} aria-label={name} title={name} aria-pressed={folder.color === c.hex} onclick={() => run(() => setFolderColor(folder, c.hex))}>
              {#if folder.color === c.hex}<Check size={12} strokeWidth={3} />{/if}
            </button>
          {/each}
          <button class="sw none" aria-label={t('folders.noColor')} title={t('folders.noColor')} aria-pressed={!folder.color} onclick={() => run(() => setFolderColor(folder, null))}><X size={12} /></button>
        </div>
      {/if}
      {#if !isFixed(folder)}
        <div class="divider"></div>
        <button class="menu-item danger" onclick={() => run(() => deleteFolder(folder))}><Trash size={15} />{t(label ? 'folders.deleteLabelItem' : 'folders.deleteFolderItem')}</button>
      {/if}
    {/if}
  </div>
</Popover>

<style>
  .folder-menu {
    display: flex;
    flex-direction: column;
  }
  .menu-item:disabled {
    opacity: 0.45;
    pointer-events: none;
  }
  .swatches {
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    gap: 6px;
    padding: 4px 10px 8px;
  }
  .sw {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 8px;
    color: #fff;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.08);
    transition: transform var(--dur-fast) var(--ease);
  }
  .sw:hover,
  .sw:focus-visible {
    transform: scale(1.12);
    outline: none;
  }
  .sw:focus-visible {
    box-shadow: 0 0 0 2px var(--bg-elevated), 0 0 0 4px var(--accent);
  }
  .sw.none {
    color: var(--text-3);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
</style>

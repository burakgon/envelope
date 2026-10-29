<script lang="ts">
  import Popover from './Popover.svelte';
  import { mail, roleLabel } from '../stores/mail.svelte';
  import { createAndLabel, createAndMove, moveTargetOf } from '../stores/folders.svelte';
  import { Check, Folder, Plus, Search, Tag } from '../icons';
  import { t } from '../i18n';

  let { x, y, ids, labels = false, onclose }: { x: number; y: number; ids: number[]; labels?: boolean; onclose: () => void } = $props();

  let q = $state('');
  let input: HTMLInputElement | undefined = $state();
  let hi = $state(0);

  /** The rows moved or labeled: in the list, or open in the reader (alone in a window of its own). */
  const rows = $derived([...mail.items, ...(mail.detail ? [mail.detail.thread] : [])].filter((th) => ids.includes(th.id)));
  const accountIds = $derived(new Set(rows.map((th) => th.accountId)));
  /** The one account of the conversations, where a missing folder or label can be made. */
  const account = $derived(accountIds.size === 1 ? mail.accountById.get([...accountIds][0]) : undefined);
  const newKind = $derived(labels || account?.kind === 'gmail' ? 'label' : 'folder');
  /** "Create ‘name’" when no folder (or label) of the account is called what was typed. */
  const canCreate = $derived(
    !!account &&
      !!q.trim() &&
      !(labels && account.kind === 'imap') &&
      !mail.folders.some((f) => f.accountId === account.id && f.kind === newKind && f.name.toLowerCase() === q.trim().toLowerCase()),
  );
  const options = $derived(
    mail.folders
      .filter((f) => accountIds.size === 0 || accountIds.has(f.accountId))
      .filter((f) => (labels ? f.kind === 'label' : f.selectable && f.role !== 'drafts' && f.role !== 'sent' && f.role !== 'starred' && f.role !== 'important'))
      .filter((f) => !q || f.path.toLowerCase().includes(q.toLowerCase()) || (f.role && roleLabel(f.role).toLowerCase().includes(q.toLowerCase())))
      .slice(0, 40),
  );
  /** The last row: "Create ‘name’" when it is offered. */
  const last = $derived(options.length - (canCreate ? 0 : 1));

  $effect(() => {
    input?.focus();
  });

  function pick(i: number) {
    // Read before closing: a menu opened from the keyboard has no props once it is closed.
    const [asLabels, targets, owner, name, f] = [labels, ids, account, q, options[i]];
    const create = i === options.length && canCreate;
    if (!create && !f) return;
    onclose();
    if (create && owner) return asLabels ? createAndLabel(owner, name, targets) : createAndMove(owner, name, targets);
    if (!f) return;
    if (asLabels) return mail.toggleLabel(f.id, targets);
    mail.moveTo(moveTargetOf(f), targets, f.role ? roleLabel(f.role) : f.name);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      hi = Math.min(last, hi + 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      hi = Math.max(0, hi - 1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(hi);
    }
  }
</script>

<Popover {x} {y} {onclose} width={260}>
  <div class="search">
    <Search size={14} />
    <input bind:this={input} bind:value={q} oninput={() => (hi = 0)} onkeydown={key} placeholder={labels ? t('moveMenu.labelAs') : t('moveMenu.moveTo')} />
  </div>
  <div class="items">
    {#each options as f, i (f.id)}
      {@const has = labels && rows.length > 0 && rows.every((th) => th.folderIds.includes(f.id))}
      <button class="menu-item" class:highlighted={i === hi} onmouseenter={() => (hi = i)} onclick={() => pick(i)}>
        {#if f.kind === 'label'}
          <span class="sw" style:background={f.color ?? 'var(--text-3)'}></span>
        {:else}
          <Folder size={15} />
        {/if}
        <span class="truncate">{f.role ? roleLabel(f.role) : f.path}</span>
        {#if mail.multiAccount}<span class="hint truncate">{mail.accountById.get(f.accountId)?.label ?? ''}</span>{/if}
        {#if has}<Check size={14} />{/if}
      </button>
    {:else}
      {#if !canCreate}<div class="empty"><Tag size={14} /> {t('moveMenu.noMatches')}</div>{/if}
    {/each}
    {#if canCreate}
      <button class="menu-item create" class:highlighted={hi === options.length} onmouseenter={() => (hi = options.length)} onclick={() => pick(options.length)}>
        <Plus size={15} />
        <span class="truncate">{t(newKind === 'label' ? (account?.kind === 'gmail' ? 'folders.createLabelNamed' : 'folders.createCategoryNamed') : 'folders.createFolderNamed', { name: q.trim() })}</span>
      </button>
    {/if}
  </div>
</Popover>

<style>
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px 8px;
    color: var(--text-3);
    border-bottom: 1px solid var(--border);
    margin-bottom: 4px;
  }
  .search input {
    flex: 1;
    border: none;
    background: none;
    height: 26px;
  }
  .items {
    max-height: 320px;
    overflow-y: auto;
  }
  .sw {
    width: 9px;
    height: 9px;
    border-radius: 3px;
    margin: 0 3px;
  }
  .create {
    color: var(--accent-text);
    font-weight: 550;
  }
  .empty {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 10px;
    color: var(--text-3);
  }
</style>

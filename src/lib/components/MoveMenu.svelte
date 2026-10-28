<script lang="ts">
  import Popover from './Popover.svelte';
  import { mail, roleLabel } from '../stores/mail.svelte';
  import { Check, Folder, Search, Tag } from '../icons';
  import { t } from '../i18n';

  let { x, y, ids, labels = false, onclose }: { x: number; y: number; ids: number[]; labels?: boolean; onclose: () => void } = $props();

  let q = $state('');
  let input: HTMLInputElement | undefined = $state();
  let hi = $state(0);

  const accountIds = $derived(new Set(mail.items.filter((th) => ids.includes(th.id)).map((th) => th.accountId)));
  const options = $derived(
    mail.folders
      .filter((f) => accountIds.size === 0 || accountIds.has(f.accountId))
      .filter((f) => (labels ? f.kind === 'label' : f.selectable && f.role !== 'drafts' && f.role !== 'sent' && f.role !== 'starred' && f.role !== 'important'))
      .filter((f) => !q || f.path.toLowerCase().includes(q.toLowerCase()) || (f.role && roleLabel(f.role).toLowerCase().includes(q.toLowerCase())))
      .slice(0, 40),
  );

  $effect(() => {
    input?.focus();
  });

  function pick(i: number) {
    const f = options[i];
    if (!f) return;
    onclose();
    if (labels) return mail.toggleLabel(f.id, ids);
    const target =
      f.role === 'inbox' ? { kind: 'inbox' as const }
      : f.role === 'trash' ? { kind: 'trash' as const }
      : f.role === 'junk' ? { kind: 'junk' as const }
      : f.role === 'archive' || f.role === 'all' ? { kind: 'archive' as const }
      : { kind: 'folder' as const, id: f.id };
    mail.moveTo(target, ids, f.role ? roleLabel(f.role) : f.name);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      hi = Math.min(options.length - 1, hi + 1);
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
      {@const has = labels && mail.items.filter((th) => ids.includes(th.id)).every((th) => th.folderIds.includes(f.id))}
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
      <div class="empty"><Tag size={14} /> {t('moveMenu.noMatches')}</div>
    {/each}
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
  .empty {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 10px;
    color: var(--text-3);
  }
</style>

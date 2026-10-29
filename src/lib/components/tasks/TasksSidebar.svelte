<script lang="ts">
  import type { TaskList } from '../../api';
  import { listName, smartViews, tasks, type SmartView } from '../../stores/tasks.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { toasts } from '../../stores/toasts.svelte';
  import { t } from '../../i18n';
  import ListMenu from './ListMenu.svelte';
  import { CalendarDays, ChevronDown, Layers, More, Plus, Star, Sun } from '../../icons';

  const icons: Record<SmartView, typeof Sun> = { today: Sun, upcoming: CalendarDays, important: Star, all: Layers };

  let collapsed = $state<Record<string, boolean>>({});
  /** The list a dragged task is over. */
  let dropOn = $state<number | null>(null);

  function dragOver(e: DragEvent, l: TaskList) {
    if (!e.dataTransfer?.types.includes('application/x-envelope-task') || l.readOnly || tasks.isView({ list: l.id })) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    dropOn = l.id;
  }

  async function drop(e: DragEvent, l: TaskList) {
    dropOn = null;
    const id = Number(e.dataTransfer?.getData('application/x-envelope-task'));
    const task = tasks.items.find((x) => x.id === id);
    if (!task) return;
    e.preventDefault();
    if (await tasks.save(task, { listId: l.id })) toasts.show(t('tasks.toast.movedTo', { list: listName(l) }));
  }
  let menu = $state<{ list: TaskList | null; accountId: number | null; x: number; y: number; start: 'menu' | 'create' } | null>(null);

  // The lists of this computer, then each account's.
  const groups = $derived.by(() => {
    const out: { key: string; accountId: number | null; label: string; color: string | null; lists: TaskList[] }[] = [];
    for (const l of tasks.lists) {
      const key = l.accountId == null ? 'local' : String(l.accountId);
      let g = out.find((x) => x.key === key);
      if (!g) {
        const a = l.accountId != null ? mail.accountById.get(l.accountId) : null;
        g = { key, accountId: l.accountId, label: a ? (a.label ?? a.email) : t('tasks.sidebar.onThisComputer'), color: a?.color ?? null, lists: [] };
        out.push(g);
      }
      g.lists.push(l);
    }
    return out;
  });

  function at(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: r.left, y: r.bottom + 4 };
  }
</script>

<div class="group">
  {#each smartViews as v (v)}
    {@const Icon = icons[v]}
    <button class="item" class:active={tasks.isView(v)} aria-current={tasks.isView(v) ? 'page' : undefined} onclick={() => tasks.setView(v)}>
      <Icon size={16} /><span class="name">{t(`tasks.view.${v}`)}</span>
      {#if tasks.counts[v] > 0}<span class="count" class:strong={v === 'today'}>{tasks.counts[v]}</span>{/if}
    </button>
  {/each}
</div>

{#each groups as g (g.key)}
  {@const open = !collapsed[g.key]}
  <div class="group account">
    <div class="account-head">
      <button class="account-title" aria-expanded={open} onclick={() => (collapsed[g.key] = open)}>
        {#if g.color}<span class="dot" style:background={g.color}></span>{/if}
        <span class="label truncate" class:email={g.label.includes('@')} title={g.label}>{g.label}</span>
        <span class="chev" class:closed={!open}><ChevronDown size={13} /></span>
      </button>
      <button
        class="icon-btn sm add"
        title={t('tasks.listMenu.newListIn', { account: g.label })}
        aria-label={t('tasks.listMenu.newListIn', { account: g.label })}
        onclick={(e) => (menu = { list: null, accountId: g.accountId, ...at(e), start: 'create' })}><Plus size={14} /></button>
    </div>
    {#if open}
      {#each g.lists as l (l.id)}
        {@const active = tasks.isView({ list: l.id })}
        <div class="list-row" class:active>
          <button
            class="item"
            class:active
            class:drop={dropOn === l.id}
            aria-current={active ? 'page' : undefined}
            onclick={() => tasks.setView({ list: l.id })}
            ondragover={(e) => dragOver(e, l)}
            ondragleave={() => (dropOn = null)}
            ondrop={(e) => drop(e, l)}
          >
            <span class="label-dot" style:background={l.color}></span>
            <span class="name truncate" title={listName(l)}>{listName(l)}</span>
            {#if l.openCount > 0}<span class="count">{l.openCount}</span>{/if}
          </button>
          <button
            class="icon-btn sm more"
            title={t('tasks.listMenu.title', { list: listName(l) })}
            aria-label={t('tasks.listMenu.title', { list: listName(l) })}
            onclick={(e) => (menu = { list: l, accountId: l.accountId, ...at(e), start: 'menu' })}><More size={14} /></button>
        </div>
      {/each}
    {/if}
  </div>
{/each}

{#if menu}
  <ListMenu list={menu.list} accountId={menu.accountId} x={menu.x} y={menu.y} start={menu.start} onclose={() => (menu = null)} />
{/if}

<style>
  .group {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-bottom: 10px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 31px;
    padding: 0 10px;
    border-radius: 8px;
    color: var(--text-2);
    font-weight: 500;
    text-align: left;
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .item:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .item.active {
    background: var(--bg-selected);
    color: var(--accent-text);
  }
  .item.drop {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .item :global(svg) {
    flex-shrink: 0;
    opacity: 0.9;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
    font-weight: 500;
    transition: opacity var(--dur-fast);
  }
  .count.strong {
    color: var(--text);
    font-weight: 650;
  }
  .item.active .count {
    color: var(--accent-text);
  }
  .label-dot {
    width: 9px;
    height: 9px;
    margin: 0 3.5px;
    border-radius: 3px;
    flex-shrink: 0;
  }
  .account {
    margin-top: 6px;
  }
  .account-head {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: 4px;
  }
  .account-title {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 28px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-3);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .account-title:hover {
    color: var(--text-2);
  }
  .account-title .label.email {
    text-transform: none;
    letter-spacing: 0;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .chev {
    display: grid;
    transition: transform var(--dur) var(--ease);
    opacity: 0;
  }
  .account-title:hover .chev,
  .chev.closed {
    opacity: 1;
  }
  .chev.closed {
    transform: rotate(-90deg);
  }
  .add,
  .more {
    opacity: 0;
    color: var(--text-3);
  }
  .account-head:hover .add,
  .add:focus-visible {
    opacity: 1;
  }
  .list-row {
    position: relative;
  }
  .more {
    position: absolute;
    top: 2.5px;
    right: 4px;
  }
  .list-row:hover .more,
  .more:focus-visible {
    opacity: 1;
  }
  .list-row:hover .count {
    opacity: 0;
  }
  .list-row.active .more {
    color: var(--accent-text);
  }
</style>

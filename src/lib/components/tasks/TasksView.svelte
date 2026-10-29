<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { flip } from 'svelte/animate';
  import { cubicOut } from 'svelte/easing';
  import type { Task } from '../../api';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { t, type MessageKey } from '../../i18n';
  import { dateFormat } from '../../utils/format';
  import WindowControls from '../WindowControls.svelte';
  import QuickAdd from './QuickAdd.svelte';
  import TaskRow from './TaskRow.svelte';
  import TaskDetail from './TaskDetail.svelte';
  import ListMenu from './ListMenu.svelte';
  import { BrushCleaning, CalendarDays, CheckCircle, ChevronRight, Layers, More, PanelLeft, Star, Sun } from '../../icons';

  let rowsEl: HTMLDivElement | undefined = $state();
  let menuAt = $state<{ x: number; y: number; start: 'menu' | 'clear' } | null>(null);

  onMount(() => {
    tasks.init();
  });

  const list = $derived(tasks.currentList);
  const hasOpen = $derived(tasks.groups.some((g) => g.items.length > 0));
  const viewKey = $derived(typeof tasks.view === 'object' ? `l${tasks.view.list}` : tasks.view);
  // As in the sidebar: open tasks, subtasks included.
  const count = $derived(list ? list.openCount : typeof tasks.view === 'object' ? 0 : tasks.counts[tasks.view]);
  const subtitle = $derived.by(() => {
    if (list) {
      const a = list.accountId != null ? mail.accountById.get(list.accountId) : null;
      return a ? (a.label ?? a.email) : '';
    }
    return tasks.view === 'today' ? dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(tasks.now) : '';
  });

  // A task just added comes into view (at the end of a list, or wherever its date puts it).
  $effect(() => {
    const id = tasks.fresh;
    if (id == null || !tasks.visible.some((x) => x.id === id)) return;
    tick().then(() => document.getElementById(`task-${id}`)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' }));
  });

  // Keep the task the keyboard is on in view.
  $effect(() => {
    const id = tasks.selectedId;
    if (id == null) return;
    tick().then(() => document.getElementById(`task-${id}`)?.scrollIntoView({ block: 'nearest' }));
  });

  // A list's open tasks can be dragged into another order (and onto another list in the sidebar).
  let dragId = $state<number | null>(null);
  let dropAt = $state<{ id: number; after: boolean } | null>(null);
  const canDrag = $derived(!!list && !list.readOnly);

  function dragStart(e: DragEvent, task: Task) {
    dragId = task.id;
    e.dataTransfer?.setData('application/x-envelope-task', String(task.id));
    e.dataTransfer?.setData('text/plain', task.title);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function dragOver(e: DragEvent, task: Task) {
    if (dragId == null) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    dropAt = { id: task.id, after: e.clientY > r.top + r.height / 2 };
  }

  function drop(e: DragEvent) {
    e.preventDefault();
    const task = tasks.open.find((x) => x.id === dragId);
    const at = dropAt;
    dragEnd();
    if (!task || !at) return;
    const open = tasks.open;
    const i = open.findIndex((x) => x.id === at.id);
    const beforeId = at.after ? (open[i + 1]?.id ?? null) : at.id;
    // Dropped where it already is.
    const j = open.findIndex((x) => x.id === task.id);
    if (beforeId === task.id || beforeId === (open[j + 1]?.id ?? null)) return;
    tasks.reorder(task, beforeId);
  }

  function dragEnd() {
    dragId = null;
    dropAt = null;
  }

  // Rows come and go smoothly once a view is on screen (not when it loads).
  function enter(node: HTMLElement) {
    const h = node.offsetHeight;
    return { duration: 200, easing: cubicOut, css: (k: number) => `overflow: hidden; height: ${k * h}px; opacity: ${k}` };
  }
  function leave(node: HTMLElement) {
    const h = node.offsetHeight;
    return { duration: 240, easing: cubicOut, css: (k: number) => `overflow: hidden; height: ${k * h}px; opacity: ${k}; transform: translateX(${(1 - k) * 10}px)` };
  }

  const empty: Record<string, { title: MessageKey; body: MessageKey; icon: typeof Sun }> = {
    today: { title: 'tasks.empty.todayTitle', body: 'tasks.empty.todayBody', icon: Sun },
    upcoming: { title: 'tasks.empty.upcomingTitle', body: 'tasks.empty.upcomingBody', icon: CalendarDays },
    important: { title: 'tasks.empty.importantTitle', body: 'tasks.empty.importantBody', icon: Star },
    all: { title: 'tasks.empty.allTitle', body: 'tasks.empty.allBody', icon: Layers },
  };
  const emptyCopy = $derived(
    typeof tasks.view === 'object'
      ? tasks.done.length
        ? { title: 'tasks.empty.listDoneTitle' as MessageKey, body: 'tasks.empty.listDoneBody' as MessageKey, icon: CheckCircle }
        : { title: 'tasks.empty.listTitle' as MessageKey, body: 'tasks.empty.listBody' as MessageKey, icon: Layers }
      : empty[tasks.view],
  );
</script>

<section class="tasks" class:with-detail={tasks.openId != null}>
  <div class="main">
    <!-- The title lines up with the list; the window's own buttons stay at its edges. -->
    <header data-tauri-drag-region>
      <div class="edge" data-tauri-drag-region>
        {#if ui.sidebarCollapsed}
          <button class="icon-btn" title={t('tasks.showSidebar')} onclick={() => (ui.sidebarCollapsed = false)}><PanelLeft size={17} /></button>
        {/if}
      </div>
      <div class="bar" data-tauri-drag-region>
        <div class="title" data-tauri-drag-region>
          {#if list}<span class="list-dot" style:background={list.color}></span>{/if}
          <h1 data-tauri-drag-region>{tasks.title}</h1>
          {#if count > 0}<span class="count" data-tauri-drag-region>{count}</span>{/if}
          {#if subtitle}<span class="sub" data-tauri-drag-region>{subtitle}</span>{/if}
        </div>
        <span class="grow" data-tauri-drag-region></span>
        {#if list}
          <button
            class="icon-btn"
            title={t('tasks.listMenu.title', { list: listName(list) })}
            aria-label={t('tasks.listMenu.title', { list: listName(list) })}
            onclick={(e) => {
              const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
              menuAt = { x: r.right, y: r.bottom + 4, start: 'menu' };
            }}><More size={17} /></button>
        {/if}
      </div>
      <div class="edge end" data-tauri-drag-region>
        {#if tasks.openId == null}<WindowControls />{/if}
      </div>
    </header>

    <div class="scroll">
      <div class="inner">
        <QuickAdd />
        {#key viewKey}
          {#if !hasOpen && !tasks.loading}
            {@const Icon = emptyCopy.icon}
            <div class="empty" class:short={list && tasks.done.length}>
              <span class="art"><Icon size={26} strokeWidth={1.6} /></span>
              <h2>{t(emptyCopy.title)}</h2>
              <p>{t(emptyCopy.body, { key: 'T' })}</p>
            </div>
          {/if}
          {#if hasOpen || (list && tasks.done.length)}
            <div
              class="rows"
              bind:this={rowsEl}
              role="listbox"
              tabindex="0"
              aria-label={tasks.title}
              aria-activedescendant={tasks.selectedId != null ? `task-${tasks.selectedId}` : undefined}
            >
              {#each tasks.groups as g (g.key)}
                <div class="group" role="group" aria-labelledby={g.title ? `group-${g.key}` : undefined} out:leave in:enter>
                  {#if g.title}
                    <div class="ghead" class:overdue={g.tone === 'overdue'} class:today={g.tone === 'today'} id="group-{g.key}">
                      {#if g.color}<span class="gdot" style:background={g.color}></span>{/if}
                      <span class="gtitle">{g.title}</span>
                      {#if g.sub}<span class="gsub">{g.sub}</span>{/if}
                      <span class="gcount">{g.items.length}</span>
                    </div>
                  {/if}
                  {#each g.items as task (task.id)}
                    <div
                      class="slot"
                      class:dragging={dragId === task.id}
                      class:drop-before={dropAt?.id === task.id && !dropAt.after}
                      class:drop-after={dropAt?.id === task.id && dropAt.after}
                      role="presentation"
                      draggable={canDrag}
                      ondragstart={(e) => dragStart(e, task)}
                      ondragover={(e) => canDrag && dragOver(e, task)}
                      ondrop={drop}
                      ondragend={dragEnd}
                      animate:flip={{ duration: 220, easing: cubicOut }}
                      in:enter
                      out:leave
                    >
                      <TaskRow
                        {task}
                        list={tasks.listById.get(task.listId)}
                        selected={tasks.selectedId === task.id}
                        showList={tasks.view !== 'all' && !list}
                        due={list || tasks.view === 'all' || g.key === 'overdue' ? 'both' : 'time'}
                      />
                    </div>
                  {/each}
                </div>
              {/each}

              {#if list && tasks.done.length}
                <div class="completed">
                  <button class="ctoggle" aria-expanded={tasks.showCompleted} onclick={() => (tasks.showCompleted = !tasks.showCompleted)}>
                    <span class="chev" class:open={tasks.showCompleted}><ChevronRight size={14} /></span>
                    {t('tasks.completedCount', { count: tasks.done.length })}
                  </button>
                  {#if tasks.showCompleted && !list.readOnly}
                    <button
                      class="clear"
                      onclick={(e) => {
                        const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                        menuAt = { x: r.left, y: r.bottom + 4, start: 'clear' };
                      }}><BrushCleaning size={13} />{t('tasks.listMenu.clearCompleted')}</button>
                  {/if}
                </div>
                {#if tasks.showCompleted}
                  <div class="group" role="group" aria-label={t('tasks.completedCount', { count: tasks.done.length })}>
                    {#each tasks.done as task (task.id)}
                      <div class="slot" animate:flip={{ duration: 220, easing: cubicOut }} in:enter out:leave>
                        <TaskRow {task} {list} selected={tasks.selectedId === task.id} />
                      </div>
                    {/each}
                  </div>
                {/if}
              {/if}
            </div>
          {/if}
        {/key}
      </div>
    </div>
  </div>

  {#if tasks.openId != null}
    <TaskDetail />
  {/if}
</section>

{#if menuAt && list}
  <ListMenu {list} x={menuAt.x} y={menuAt.y} align={menuAt.start === 'menu' ? 'end' : 'start'} start={menuAt.start} showCompletedToggle onclose={() => (menuAt = null)} />
{/if}

<style>
  .tasks {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
    background: var(--bg);
    container-type: inline-size;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  header {
    display: grid;
    grid-template-columns: minmax(10px, 1fr) minmax(0, 760px) minmax(10px, 1fr);
    align-items: center;
    height: var(--header-h);
    flex-shrink: 0;
  }
  .edge {
    display: flex;
    align-items: center;
    align-self: stretch;
    min-width: 0;
    padding-left: 10px;
  }
  .edge.end {
    justify-content: flex-end;
    padding: 0 10px 0 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    align-self: stretch;
    padding: 0 18px 0 30px;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .list-dot {
    align-self: center;
    width: 11px;
    height: 11px;
    border-radius: 4px;
    flex-shrink: 0;
  }
  h1 {
    margin: 0;
    min-width: 0;
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .count {
    font-size: var(--text-sm);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .sub {
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--text-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
  }
  .inner {
    max-width: 760px;
    margin: 0 auto;
    padding: 4px 28px 48px;
  }
  .rows {
    margin-top: 10px;
  }
  .rows:focus-visible {
    outline: none;
  }
  .rows:focus-visible :global(.task.selected) {
    box-shadow: inset 0 0 0 1.5px var(--border-focus);
  }
  .group + .group {
    margin-top: 6px;
  }
  .slot {
    position: relative;
  }
  .slot.dragging {
    opacity: 0.4;
  }
  /* Where a dragged task will go. */
  .drop-before::before,
  .drop-after::after {
    content: '';
    position: absolute;
    left: 12px;
    right: 12px;
    height: 2px;
    border-radius: 1px;
    background: var(--accent);
    z-index: 1;
  }
  .drop-before::before {
    top: -1px;
  }
  .drop-after::after {
    bottom: -1px;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 14px 12px 6px;
    min-width: 0;
  }
  .gdot {
    align-self: center;
    width: 8px;
    height: 8px;
    border-radius: 3px;
    flex-shrink: 0;
  }
  .gtitle {
    font-size: var(--text-md);
    font-weight: 650;
    letter-spacing: -0.005em;
    white-space: nowrap;
  }
  .ghead.overdue .gtitle {
    color: var(--red);
  }
  .ghead.today .gtitle {
    color: var(--accent-text);
  }
  .gsub {
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--text-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .gcount {
    margin-left: auto;
    font-size: var(--text-sm);
    color: var(--text-4);
    font-variant-numeric: tabular-nums;
  }
  .completed {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    padding: 0 4px;
  }
  .ctoggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px 0 6px;
    border-radius: 8px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-3);
  }
  .ctoggle:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .clear {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 9px;
    border-radius: 8px;
    font-size: var(--text-sm);
    color: var(--text-3);
    animation: fade-in var(--dur);
  }
  .clear:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .chev {
    display: grid;
    transition: transform var(--dur) var(--ease);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: 72px 24px 40px;
    animation: fade-in var(--dur-slow) var(--ease);
  }
  .art {
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    margin-bottom: 14px;
    border-radius: 50%;
    color: var(--accent-text);
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1px var(--accent-soft);
  }
  .empty h2 {
    margin: 0 0 6px;
    font-size: var(--text-lg);
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  .empty p {
    margin: 0;
    max-width: 320px;
    color: var(--text-3);
    line-height: 1.5;
  }
  /* Above the completed tasks of the list. */
  .empty.short {
    padding: 44px 24px 28px;
  }
</style>

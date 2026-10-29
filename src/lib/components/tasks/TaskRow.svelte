<script lang="ts">
  import type { Task, TaskList } from '../../api';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { t } from '../../i18n';
  import { dueLabel, dueState } from '../../utils/due';
  import { AlignLeft, CloudAlert, CloudUpload, CornerDownRight, ListChecks, Mail, Repeat, Star } from '../../icons';
  import TaskCheck from './TaskCheck.svelte';

  let {
    task,
    list,
    selected = false,
    showList = false,
    due = 'both',
  }: {
    task: Task;
    list: TaskList | undefined;
    selected?: boolean;
    /** Name the list (in views that mix lists). */
    showList?: boolean;
    /** What of the due date to show: the day is the group's heading in date groups. */
    due?: 'both' | 'time' | 'none';
  } = $props();

  const completing = $derived(tasks.completing.has(task.id));
  const done = $derived(task.completedAt != null || completing);
  const state = $derived(dueState(task, tasks.now));
  const dueText = $derived(due === 'none' ? '' : dueLabel(task, due === 'time' ? 'time' : 'both', tasks.now));
  const parent = $derived(task.parentId != null ? (tasks.items.find((x) => x.id === task.parentId) ?? tasks.all.find((x) => x.id === task.parentId)) : undefined);
  const hasMeta = $derived(!!dueText || task.repeats || task.subtaskCount > 0 || !!task.source || !!task.notes.trim() || showList || !!parent || task.pending || !!task.error);
  const color = $derived(list?.color ?? 'var(--text-3)');
</script>

<div
  class="task"
  class:selected
  class:done
  class:completing
  class:fresh={tasks.fresh === task.id}
  id="task-{task.id}"
  role="option"
  aria-selected={selected}
  aria-checked={done}
  tabindex="-1"
  style:--c={color}
  onclick={() => tasks.openTask(task.id)}
  onkeydown={() => {}}
>
  <TaskCheck checked={done} {color} label={t(done ? 'tasks.row.reopen' : 'tasks.row.complete', { title: task.title })} onclick={() => tasks.complete(task)} />
  <div class="body">
    <span class="title"><span class="text">{task.title}</span></span>
    {#if hasMeta}
      <span class="meta">
        {#if parent}<span class="parent truncate"><CornerDownRight size={11} />{parent.title}</span>{/if}
        {#if dueText}<span class="due {state ?? ''}">{dueText}</span>{/if}
        {#if task.repeats}<span class="ico" title={t('tasks.row.repeats')}><Repeat size={11} /></span>{/if}
        {#if task.subtaskCount > 0}
          <span class="subs" title={t('tasks.row.subtasks', { done: task.subtasksDone, count: task.subtaskCount })}><ListChecks size={12} />{task.subtasksDone}/{task.subtaskCount}</span>
        {/if}
        {#if task.source}<span class="src truncate" title={task.source.subject}><Mail size={11} />{task.source.from ?? task.source.subject}</span>{/if}
        {#if task.notes.trim()}<span class="ico" title={t('tasks.row.hasNotes')}><AlignLeft size={11} /></span>{/if}
        {#if showList && list}<span class="list truncate"><span class="dot"></span>{listName(list)}</span>{/if}
        {#if task.error}
          <span class="sync error" title={t('tasks.row.notSaved', { error: task.error })}><CloudAlert size={12} /></span>
        {:else if task.pending}
          <span class="sync pending" title={t('tasks.row.saving')}><CloudUpload size={12} /></span>
        {/if}
      </span>
    {/if}
  </div>
  {#if list?.importance}
    <button
      class="star"
      class:on={task.important}
      tabindex="-1"
      aria-pressed={task.important}
      aria-label={t('tasks.row.important')}
      title={t('tasks.row.importantShortcut', { key: 'S' })}
      onclick={(e) => {
        e.stopPropagation();
        tasks.toggleImportant(task);
      }}
    >
      <Star size={15} fill={task.important ? 'currentColor' : 'none'} />
    </button>
  {/if}
</div>

<style>
  .task {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 12px;
    min-height: 40px;
    padding: 9px 8px 9px 12px;
    border-radius: 10px;
    transition: background var(--dur-fast) var(--ease);
  }
  .task:hover {
    background: var(--bg-hover);
  }
  .task.selected {
    background: var(--bg-selected);
  }
  /* Just added: a moment of color to find it by. */
  .task.fresh {
    animation: fresh 1.4s var(--ease) both;
  }
  @keyframes fresh {
    from,
    30% {
      background: var(--accent-soft);
    }
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 1px;
  }
  .title {
    display: flex;
    min-width: 0;
    font-size: var(--text-md);
    line-height: 20px;
    color: var(--text);
    transition: color var(--dur-slow) var(--ease) 120ms;
  }
  /* The line through a completed title is drawn from left to right. */
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: linear-gradient(currentColor, currentColor) no-repeat 0 55% / 0 1.5px;
    transition: background-size 260ms var(--ease-out) 90ms;
  }
  .done .title {
    color: var(--text-3);
  }
  .done .text {
    background-size: 100% 1.5px;
  }
  .meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    column-gap: 10px;
    row-gap: 2px;
    min-width: 0;
    font-size: var(--text-sm);
    line-height: 16px;
    color: var(--text-3);
  }
  .meta > span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .meta :global(svg) {
    flex-shrink: 0;
  }
  .due {
    font-variant-numeric: tabular-nums;
    font-weight: 500;
  }
  .due.overdue {
    color: var(--red);
  }
  .due.today {
    color: var(--accent-text);
  }
  .done .due {
    color: inherit;
  }
  .subs {
    font-variant-numeric: tabular-nums;
  }
  .src,
  .parent {
    max-width: 220px;
  }
  .list .dot {
    width: 7px;
    height: 7px;
    border-radius: 2px;
    background: var(--c);
    flex-shrink: 0;
  }
  .sync.error {
    color: var(--red);
  }
  /* Quick saves never show it: only a change still on its way after a moment does. */
  .sync.pending {
    color: var(--text-3);
    animation: fade-in var(--dur) 1.2s both;
  }
  .star {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    margin: -4px 0 -4px 0;
    border-radius: var(--r-sm);
    color: var(--text-3);
    flex-shrink: 0;
    opacity: 0;
    transition: opacity var(--dur-fast), color var(--dur-fast), background var(--dur-fast), transform var(--dur) var(--spring);
  }
  .task:hover .star,
  .task.selected .star,
  .star.on {
    opacity: 1;
  }
  .star:hover {
    color: var(--text-2);
    background: var(--bg-active);
  }
  .star.on {
    color: var(--star);
  }
  .star:active {
    transform: scale(0.85);
  }
  .done .star.on {
    opacity: 0.5;
  }
</style>

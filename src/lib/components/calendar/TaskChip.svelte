<script lang="ts">
  import type { Task } from '../../api';
  import { cal } from '../../stores/calendar.svelte';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { t } from '../../i18n';
  import { dueState } from '../../utils/due';
  import { timeOf } from '../../utils/format';
  import TaskCheck from '../tasks/TaskCheck.svelte';

  let { task, variant }: { task: Task; variant: 'month' | 'lane' | 'agenda' } = $props();

  const list = $derived(tasks.listById.get(task.listId));
  const color = $derived(list?.color ?? 'var(--text-3)');
  const done = $derived(tasks.completing.has(task.id));
  const late = $derived(dueState(task, tasks.now) === 'overdue');
  const time = $derived(task.dueAt != null ? timeOf(task.dueAt) : null);
  const label = $derived(`${task.title}${list ? ` · ${listName(list)}` : ''} — ${t('tasks.calendar.open')}`);
</script>

<!-- The circle completes the task; the rest opens it in Tasks. -->
<div class="tchip {variant}" class:done style:--c={color}>
  <TaskCheck checked={done} {color} size={variant === 'agenda' ? 18 : 13} label={t('tasks.row.complete', { title: task.title })} onclick={() => cal.completeTask(task)} />
  <button class="open" title={label} aria-label={label} onclick={() => tasks.reveal(task.id)}>
    {#if variant === 'agenda'}
      <span class="when" class:late>{time ?? t('tasks.calendar.due')}</span>
      <span class="title">{task.title}</span>
      {#if list}<span class="list"><span class="dot"></span>{listName(list)}</span>{/if}
    {:else}
      {#if time}<span class="t" class:late>{time}</span>{/if}
      <span class="title">{task.title}</span>
    {/if}
  </button>
</div>

<style>
  .tchip {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    color: var(--text);
    transition: background var(--dur-fast), opacity var(--dur-slow);
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: inherit;
    align-self: stretch;
    text-align: left;
  }
  .tchip :global(.check) {
    margin-top: 0;
  }
  .title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: linear-gradient(currentColor, currentColor) no-repeat 0 55% / 0 1px;
    transition: background-size 260ms var(--ease-out) 90ms, color var(--dur-slow);
  }
  .done .title {
    color: var(--text-3);
    background-size: 100% 1px;
  }
  .t,
  .when {
    flex-shrink: 0;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .late {
    color: var(--red);
  }
  .month {
    height: 21px;
    padding: 0 6px 0 5px;
    border-radius: 6px;
    font-size: 11.5px;
  }
  .month:hover,
  .agenda:hover {
    background: var(--bg-hover);
  }
  .lane {
    height: 23px;
    padding: 0 8px 0 6px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 550;
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--c) 40%, var(--border));
  }
  .lane:hover {
    background: color-mix(in oklab, var(--c) 10%, var(--bg-elevated));
  }
  /* The circle sits where an event's color bar is, so the times line up. */
  .agenda {
    gap: 12px;
    min-height: 38px;
    padding: 0 10px 0 3px;
    border-radius: 10px;
  }
  .agenda :global(.check) {
    margin-right: -7px;
  }
  .agenda .when {
    width: 140px;
    font-size: var(--text-sm);
  }
  .agenda .title {
    flex: 1;
    font-weight: 600;
  }
  .list {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 2px;
    background: var(--c);
  }
</style>

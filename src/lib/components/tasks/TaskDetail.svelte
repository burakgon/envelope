<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import type { Task } from '../../api';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { t } from '../../i18n';
  import { addDays, atTime, capitalize, dayKey, relativeDay } from '../../utils/due';
  import { messageDate } from '../../utils/format';
  import DatePicker from '../calendar/DatePicker.svelte';
  import TimePicker from '../calendar/TimePicker.svelte';
  import MiniMonth from '../calendar/MiniMonth.svelte';
  import Popover from '../Popover.svelte';
  import WindowControls from '../WindowControls.svelte';
  import TaskCheck from './TaskCheck.svelte';
  import { AlignLeft, ArrowRight, CalendarDays, Clock, CloudAlert, CornerDownRight, List, Mail, Plus, Repeat, Star, Trash, X } from '../../icons';

  const task = $derived(tasks.detail);
  const list = $derived(task ? tasks.listById.get(task.listId) : undefined);
  const readOnly = $derived(!!list?.readOnly);
  const done = $derived(!!task && (task.completedAt != null || tasks.completing.has(task.id)));
  const parent = $derived(task?.parentId != null ? (tasks.items.find((x) => x.id === task.parentId) ?? tasks.all.find((x) => x.id === task.parentId)) : undefined);
  const subsDone = $derived(tasks.subtasks.filter((s) => s.completedAt != null).length);
  // Microsoft To Do keeps subtasks as checklist items: a title and whether it is done.
  const bare = $derived(task?.parentId != null && list?.kind === 'graph');
  const today = $derived(dayKey(tasks.now));

  // What is being typed stays as typed; the rest follows the task.
  let title = $state('');
  let notes = $state('');
  let dueDate = $state('');
  let dueTime = $state('09:00');
  let newSub = $state('');
  let editing: 'title' | 'notes' | null = null;
  let shown: Task | null = null;
  let titleEl: HTMLTextAreaElement | undefined = $state();
  let notesEl: HTMLTextAreaElement | undefined = $state();
  let datePick = $state<{ x: number; y: number } | null>(null);

  const pad = (n: number) => String(n).padStart(2, '0');

  $effect(() => {
    const cur = task;
    if (!cur) return;
    // Moved to another list, a task may come back with a new id: it is still the one being edited.
    if (shown && shown.id !== cur.id && tasks.movedTo.get(shown.id) !== cur.id) {
      commit(shown);
      editing = null;
    }
    if (editing !== 'title') title = cur.title;
    if (editing !== 'notes') notes = cur.notes;
    dueDate = cur.dueDate ?? today;
    if (cur.dueAt != null) {
      const d = new Date(cur.dueAt);
      dueTime = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    }
    shown = cur;
    tick().then(() => [titleEl, notesEl].forEach(grow));
  });

  onDestroy(() => {
    if (shown) commit(shown);
  });

  /** The latest copy of a task (the panel may have moved on). */
  function latest(target: Task): Task {
    return tasks.detail?.id === target.id ? tasks.detail : (tasks.items.find((x) => x.id === target.id) ?? target);
  }

  function commit(target: Task) {
    const cur = latest(target);
    const patch: { title?: string; notes?: string } = {};
    if (title.trim() && title.trim() !== cur.title) patch.title = title.trim();
    if (notes !== cur.notes) patch.notes = notes;
    if (patch.title != null || patch.notes != null) tasks.save(cur, patch);
  }

  function grow(el: HTMLTextAreaElement | undefined) {
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  }

  function setDue(date: string | null, time: string | null = null) {
    if (!task) return;
    const [h, m] = (time ?? '').split(':').map(Number);
    tasks.save(task, { dueDate: date, dueAt: date && time ? atTime(date, h * 60 + m) : null });
  }

  function addTime() {
    // The next full hour on a day already under way, else nine in the morning.
    const now = new Date();
    dueTime = dueDate === today && now.getHours() >= 9 ? `${pad(Math.min(23, now.getHours() + 1))}:00` : '09:00';
    setDue(dueDate, dueTime);
  }

  function nextWeek(): string {
    const d = new Date(`${today}T00:00:00`);
    return addDays(today, 7 - ((d.getDay() - ui.settings.weekStart + 7) % 7));
  }

  function openSource() {
    const s = task?.source;
    if (!s) return;
    if (s.threadId != null) {
      mail.setMailbox({ kind: 'unified', role: 'inbox' });
      mail.select(s.threadId);
    } else {
      // The message is gone from this computer; look for the conversation.
      ui.view = 'mail';
      mail.setSearch(s.subject);
    }
  }

  async function addSubtask() {
    const value = newSub.trim();
    if (!value || !task) return;
    newSub = '';
    await tasks.addSubtask(task, value);
  }

  function key(e: KeyboardEvent) {
    const field = (e.target as HTMLElement).closest('input, textarea');
    if (e.key === 'Escape' && field) {
      // Leaving a field keeps what was typed and goes back to the list; a second Escape closes the panel.
      e.stopPropagation();
      (field as HTMLElement).blur();
      document.querySelector<HTMLElement>('.tasks .rows')?.focus();
    }
  }

  function close() {
    tasks.closeDetail();
    document.querySelector<HTMLElement>('.tasks .rows')?.focus();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="detail" aria-label={t('tasks.detail.label')} onkeydown={key}>
  <header data-tauri-drag-region>
    {#if task && parent}
      <button class="crumb" onclick={() => tasks.openTask(parent.id)} title={parent.title}>
        <CornerDownRight size={13} /><span class="truncate">{t('tasks.detail.inTask', { task: parent.title })}</span>
      </button>
    {:else if list}
      <span class="crumb static" data-tauri-drag-region><span class="dot" style:background={list.color}></span><span class="truncate">{listName(list)}</span></span>
    {/if}
    <span class="grow" data-tauri-drag-region></span>
    {#if task && !readOnly}
      <button class="icon-btn" title={t('tasks.detail.deleteShortcut', { key: '#' })} aria-label={t('tasks.detail.delete')} onclick={() => tasks.remove(task)}><Trash size={16} /></button>
    {/if}
    <button class="icon-btn" title={t('tasks.detail.close')} aria-label={t('tasks.detail.close')} onclick={close}><X size={17} /></button>
    <WindowControls />
  </header>

  {#if task}
    <div class="scroll">
      <div class="head">
        <TaskCheck checked={done} color={list?.color ?? 'var(--text-3)'} size={22} label={t(done ? 'tasks.row.reopen' : 'tasks.row.complete', { title: task.title })} onclick={() => tasks.complete(task)} />
        <textarea
          bind:this={titleEl}
          class="title"
          class:done
          rows="1"
          bind:value={title}
          aria-label={t('tasks.detail.title')}
          readonly={readOnly}
          spellcheck="true"
          oninput={() => grow(titleEl)}
          onfocus={() => (editing = 'title')}
          onblur={() => {
            editing = null;
            if (!title.trim()) title = task.title;
            commit(task);
          }}
          onkeydown={(e) => {
            if (e.key === 'Enter' && !e.isComposing) {
              e.preventDefault();
              titleEl?.blur();
            }
          }}
        ></textarea>
        {#if list?.importance}
          <button
            class="star"
            class:on={task.important}
            aria-pressed={task.important}
            aria-label={t('tasks.row.important')}
            title={t('tasks.row.importantShortcut', { key: 'S' })}
            disabled={readOnly}
            onclick={() => tasks.toggleImportant(task)}
          >
            <Star size={18} fill={task.important ? 'currentColor' : 'none'} />
          </button>
        {/if}
      </div>

      {#if task.error}
        <div class="status error" role="status"><CloudAlert size={15} /><span>{t('tasks.detail.notSaved', { error: task.error })}</span></div>
      {:else if task.pending}
        <div class="status pending" role="status"><span>{t('tasks.row.saving')}</span></div>
      {/if}
      {#if readOnly}
        <div class="status"><span>{t('tasks.detail.readOnly', { list: listName(list) })}</span></div>
      {/if}

      <div class="fields">
        {#if !bare}
        <div class="field">
          <span class="ico"><CalendarDays size={16} /></span>
          {#if task.dueDate}
            <div class="due">
              <DatePicker bind:value={dueDate} onchange={() => setDue(dueDate, task.dueAt != null ? dueTime : null)} />
              {#if list?.dueTimes}
                {#if task.dueAt != null}
                  <TimePicker bind:value={dueTime} onchange={() => setDue(dueDate, dueTime)} />
                  <button class="icon-btn sm" title={t('tasks.detail.removeTime')} aria-label={t('tasks.detail.removeTime')} onclick={() => setDue(dueDate)}><X size={13} /></button>
                {:else}
                  <button class="link" onclick={addTime}><Clock size={13} />{t('tasks.detail.addTime')}</button>
                {/if}
              {/if}
              <span class="grow"></span>
              <button class="link quiet" onclick={() => setDue(null)}>{t('tasks.detail.removeDue')}</button>
            </div>
          {:else}
            <div class="presets">
              <button class="preset" disabled={readOnly} onclick={() => setDue(today)}>{capitalize(relativeDay(0))}</button>
              <button class="preset" disabled={readOnly} onclick={() => setDue(addDays(today, 1))}>{capitalize(relativeDay(1))}</button>
              <button class="preset" disabled={readOnly} onclick={() => setDue(nextWeek())}>{t('tasks.detail.nextWeek')}</button>
              <button
                class="preset"
                disabled={readOnly}
                onclick={(e) => {
                  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                  datePick = { x: r.left, y: r.bottom + 4 };
                }}>{t('tasks.detail.pickDate')}</button>
            </div>
          {/if}
        </div>

        {/if}

        {#if task.repeats}
          <div class="field">
            <span class="ico"><Repeat size={16} /></span>
            <div class="text">
              <span>{t('tasks.detail.repeats')}</span>
              <!-- Only CalDAV moves a task on to its next date; elsewhere it is simply done. -->
              {#if list?.kind === 'caldav' && task.dueDate}<small>{t('tasks.detail.repeatsHint')}</small>{/if}
            </div>
          </div>
        {/if}

        {#if task.parentId == null}
          <div class="field">
            <span class="ico"><List size={16} /></span>
            <select
              class="input"
              aria-label={t('tasks.detail.list')}
              value={task.listId}
              disabled={readOnly}
              onchange={(e) => tasks.save(task, { listId: Number((e.currentTarget as HTMLSelectElement).value) })}
            >
              {#each tasks.lists.filter((l) => !l.readOnly || l.id === task.listId) as l (l.id)}
                <option value={l.id}>{listName(l)}{l.accountId != null ? ` — ${mail.accountById.get(l.accountId)?.label ?? mail.accountById.get(l.accountId)?.email ?? ''}` : ''}</option>
              {/each}
            </select>
          </div>
        {/if}

        {#if !bare}
        <div class="field notes">
          <span class="ico"><AlignLeft size={16} /></span>
          <textarea
            bind:this={notesEl}
            class="input area"
            rows="3"
            placeholder={t('tasks.detail.notesPlaceholder')}
            aria-label={t('tasks.detail.notes')}
            readonly={readOnly}
            bind:value={notes}
            oninput={() => grow(notesEl)}
            onfocus={() => (editing = 'notes')}
            onblur={() => {
              editing = null;
              commit(task);
            }}
          ></textarea>
        </div>
        {/if}
      </div>

      {#if list?.subtasks && task.parentId == null}
        <section class="subtasks" aria-label={t('tasks.detail.subtasks')}>
          <div class="sec-head">
            <span>{t('tasks.detail.subtasks')}</span>
            {#if tasks.subtasks.length}<span class="sec-count">{subsDone}/{tasks.subtasks.length}</span>{/if}
          </div>
          {#each tasks.subtasks as s (s.id)}
            <div class="sub" class:done={s.completedAt != null}>
              <TaskCheck
                size={16}
                checked={s.completedAt != null}
                color={list.color}
                label={t(s.completedAt != null ? 'tasks.row.reopen' : 'tasks.row.complete', { title: s.title })}
                onclick={() => tasks.setCompleted(s, s.completedAt == null)}
              />
              <input
                class="sub-title"
                value={s.title}
                title={s.title}
                aria-label={t('tasks.detail.subtaskTitle')}
                readonly={readOnly}
                onchange={(e) => {
                  const v = (e.currentTarget as HTMLInputElement).value.trim();
                  if (v && v !== s.title) tasks.save(s, { title: v });
                }}
                onkeydown={(e) => e.key === 'Enter' && !e.isComposing && (e.currentTarget as HTMLInputElement).blur()}
              />
              {#if !readOnly}
                <button class="icon-btn sm remove" title={t('tasks.detail.removeSubtask')} aria-label={t('tasks.detail.removeSubtask')} onclick={() => tasks.remove(s)}><X size={13} /></button>
              {/if}
            </div>
          {/each}
          {#if !readOnly}
            <div class="sub add">
              <span class="add-ico"><Plus size={13} strokeWidth={2.4} /></span>
              <input
                class="sub-title"
                placeholder={t('tasks.detail.addSubtask')}
                aria-label={t('tasks.detail.addSubtask')}
                bind:value={newSub}
                onkeydown={(e) => {
                  if (e.key === 'Enter' && !e.isComposing) {
                    e.preventDefault();
                    addSubtask();
                  }
                }}
              />
            </div>
          {/if}
        </section>
      {/if}

      {#if task.source}
        <button class="source" onclick={openSource} title={t('tasks.detail.openMessage')}>
          <span class="src-ico"><Mail size={16} /></span>
          <span class="src-txt">
            <small>{t('tasks.detail.fromMessage')}</small>
            <b class="truncate">{task.source.subject || t('common.noSubject')}</b>
            {#if task.source.from}<span class="truncate">{task.source.from}</span>{/if}
          </span>
          <ArrowRight size={15} />
        </button>
      {/if}

      <footer>
        {#if task.completedAt != null}
          <span>{t('tasks.detail.completedAt', { when: messageDate(task.completedAt) })}</span>
        {/if}
        <span>{t('tasks.detail.createdAt', { when: messageDate(task.createdAt) })}</span>
      </footer>
    </div>
  {/if}
</aside>

{#if datePick && task}
  <Popover x={datePick.x} y={datePick.y} onclose={() => (datePick = null)} width={250}>
    <MiniMonth
      value={Date.now()}
      onpick={(ms) => {
        datePick = null;
        setDue(dayKey(ms));
      }}
    />
  </Popover>
{/if}

<style>
  .detail {
    width: 400px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border-left: 1px solid var(--border);
    animation: panel-in var(--dur) var(--ease-out);
  }
  @keyframes panel-in {
    from {
      opacity: 0;
      transform: translateX(12px);
    }
  }
  /* In a narrow window the panel goes over the list. */
  @container (max-width: 820px) {
    .detail {
      position: absolute;
      top: 0;
      right: 0;
      bottom: 0;
      width: min(400px, 100%);
      z-index: 10;
      border-left: none;
      box-shadow: var(--shadow-lg);
    }
  }
  header {
    display: flex;
    align-items: center;
    gap: 2px;
    height: var(--header-h);
    padding: 0 10px 0 14px;
    flex-shrink: 0;
  }
  .crumb {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    border-radius: 8px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--text-2);
  }
  button.crumb:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .crumb.static {
    padding-left: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 3px;
    flex-shrink: 0;
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 4px 22px 28px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 6px 0 10px;
  }
  .head :global(.check) {
    margin-top: 5px;
  }
  .title {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    resize: none;
    overflow: hidden;
    padding: 2px 0;
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.015em;
    line-height: 1.3;
    color: var(--text);
  }
  .title.done {
    color: var(--text-3);
    text-decoration: line-through;
    text-decoration-thickness: 1.5px;
  }
  .star {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    margin-top: 1px;
    border-radius: var(--r);
    color: var(--text-3);
    flex-shrink: 0;
    transition: color var(--dur-fast), background var(--dur-fast), transform var(--dur) var(--spring);
  }
  .star:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .star.on {
    color: var(--star);
  }
  .star:active {
    transform: scale(0.85);
  }
  .status {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 0 0 10px 34px;
    padding: 8px 10px;
    border-radius: 10px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--text-2);
    background: var(--bg-sunken);
  }
  .status :global(svg) {
    flex-shrink: 0;
    margin-top: 1px;
  }
  .status.error {
    color: var(--red);
    background: var(--red-soft);
  }
  .status.pending {
    color: var(--text-3);
    background: none;
    padding: 0 0 4px;
    animation: fade-in var(--dur) 1.2s both;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 6px 0 4px;
  }
  .field {
    display: flex;
    align-items: flex-start;
    gap: 14px;
    min-height: 34px;
  }
  .ico {
    display: grid;
    place-items: center;
    width: 20px;
    height: 34px;
    flex-shrink: 0;
    color: var(--text-3);
  }
  .field > .input,
  .field > select {
    flex: 1;
    min-width: 0;
  }
  .due,
  .presets {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .preset {
    height: 30px;
    padding: 0 9px;
    border-radius: var(--r);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--text-2);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .preset:hover:not(:disabled) {
    color: var(--text);
    background: var(--bg-hover);
  }
  .preset:disabled {
    opacity: 0.5;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 30px;
    padding: 0 8px;
    border-radius: 7px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-text);
    white-space: nowrap;
  }
  .link:hover {
    background: var(--accent-softer);
  }
  .link.quiet {
    color: var(--text-3);
    font-weight: 500;
  }
  .link.quiet:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .text {
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-height: 34px;
    color: var(--text-2);
  }
  .text small {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .area {
    height: auto;
    min-height: 76px;
    padding: 8px 11px;
    line-height: 1.5;
    resize: none;
    overflow: hidden;
  }
  .subtasks {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .sec-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 0 0 6px;
    font-size: 11.5px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-3);
  }
  .sec-count {
    font-variant-numeric: tabular-nums;
    letter-spacing: 0;
  }
  .sub {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 34px;
    padding: 0 4px 0 2px;
    border-radius: 8px;
  }
  .sub:hover {
    background: var(--bg-hover);
  }
  .sub :global(.check) {
    margin: 0 0 0 4px;
  }
  .sub-title {
    flex: 1;
    min-width: 0;
    height: 30px;
    border: none;
    background: none;
    color: var(--text);
    text-overflow: ellipsis;
  }
  .sub.done .sub-title {
    color: var(--text-3);
    text-decoration: line-through;
  }
  .remove {
    opacity: 0;
  }
  .sub:hover .remove,
  .remove:focus-visible {
    opacity: 1;
  }
  .add-ico {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    margin-left: 4px;
    border-radius: 50%;
    color: var(--text-3);
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
  }
  .add .sub-title::placeholder {
    color: var(--text-3);
  }
  .source {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    margin-top: 18px;
    padding: 10px 12px;
    border-radius: 12px;
    text-align: left;
    color: var(--text-3);
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border);
    transition: background var(--dur-fast), box-shadow var(--dur-fast);
  }
  .source:hover {
    background: var(--bg-hover);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .src-ico {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 10px;
    flex-shrink: 0;
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .src-txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    line-height: 1.35;
  }
  .src-txt small {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .src-txt b {
    font-weight: 600;
    color: var(--text);
  }
  .src-txt span {
    font-size: var(--text-sm);
  }
  footer {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 22px;
    font-size: var(--text-sm);
    color: var(--text-4);
    text-align: center;
  }
</style>

<script lang="ts">
  import { tick } from 'svelte';
  import { slide } from 'svelte/transition';
  import type { TaskInput } from '../../api';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { intlLocale, t, type MessageKey } from '../../i18n';
  import { parseQuickAdd } from '../../utils/quickadd';
  import { addDays, atTime, dayKey, dueLabel, relativeDay } from '../../utils/due';
  import { timeOf } from '../../utils/format';
  import Popover from '../Popover.svelte';
  import { CalendarDays, Check, ChevronDown, Plus, Star, X } from '../../icons';

  let input: HTMLInputElement | undefined = $state();
  let box: HTMLDivElement | undefined = $state();
  let text = $state('');
  let focused = $state(false);
  /** Chips the user removed: what they stood for stays in the title. */
  let ignore = $state<{ due?: boolean; important?: boolean }>({});
  let busy = $state(false);
  let picker = $state<{ x: number; y: number } | null>(null);

  const list = $derived(tasks.addTarget);
  const words = (key: MessageKey) => t(key).split(',').map((w) => w.trim()).filter(Boolean);
  const parsed = $derived(
    parseQuickAdd(text, {
      locale: intlLocale(),
      times: !!list?.dueTimes,
      importance: !!list?.importance,
      ignore,
      weekStart: ui.settings.weekStart,
      fillers: words('tasks.quickAdd.fillers'),
      hours: words('tasks.quickAdd.hours'),
    }),
  );
  // New tasks are due today in Today, tomorrow in Upcoming, and important in Important.
  const today = $derived(dayKey(tasks.now));
  const implied = $derived(tasks.view === 'today' ? today : tasks.view === 'upcoming' ? addDays(today, 1) : null);
  const due = $derived(ignore.due ? null : (parsed.due ?? (implied ? { date: implied, minutes: null } : null)));
  const dueAt = $derived(due && due.minutes != null ? atTime(due.date, due.minutes) : null);
  const important = $derived(!!list?.importance && !ignore.important && (parsed.important || tasks.view === 'important'));
  const smart = $derived(typeof tasks.view !== 'object');
  const readOnly = $derived(!list || list.readOnly);
  const example = $derived(list?.dueTimes ? `${relativeDay(1)} ${timeOf(new Date(2000, 0, 1, 15))}` : relativeDay(1));
  const showChips = $derived((focused || !!text || !!picker) && !readOnly);

  $effect(() => {
    if (!text) ignore = {};
  });

  $effect(() => {
    if (tasks.focusAdd && input) {
      tasks.focusAdd = false;
      tick().then(() => input?.focus());
    }
  });

  async function submit() {
    const title = parsed.title.trim();
    if (!title || !list || busy) return;
    busy = true;
    const task: TaskInput = { listId: list.id, title, dueDate: due?.date ?? null, dueAt, important };
    const made = await tasks.add(task);
    busy = false;
    if (made) text = '';
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.isComposing) {
      e.preventDefault();
      submit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (text) text = '';
      else input?.blur();
    } else if (e.key === 'ArrowDown' && !text) {
      e.preventDefault();
      input?.blur();
      document.querySelector<HTMLElement>('.tasks .rows')?.focus();
      tasks.step(1);
    }
  }

  function accountOf(id: number | null): string {
    const a = id != null ? mail.accountById.get(id) : null;
    return a ? (a.label ?? a.email) : t('tasks.sidebar.onThisComputer');
  }
</script>

<!-- The chips stay while the focus is in the field or on one of them. -->
<div
  class="box"
  bind:this={box}
  onfocusin={() => (focused = true)}
  onfocusout={(e) => {
    if (!box?.contains(e.relatedTarget as Node | null)) focused = false;
  }}
>
<div class="quick" class:focused class:disabled={readOnly}>
  <span class="plus"><Plus size={14} strokeWidth={2.4} /></span>
  <input
    bind:this={input}
    bind:value={text}
    placeholder={readOnly ? t('tasks.quickAdd.readOnly', { list: listName(list) }) : t('tasks.quickAdd.placeholder')}
    aria-label={t('tasks.quickAdd.label')}
    aria-describedby="quick-chips"
    disabled={readOnly}
    spellcheck="true"
    onkeydown={key}
  />
  {#if text.trim()}<span class="kbd">{t('keys.enter')}</span>{/if}
</div>

{#if showChips}
  <!-- Mouse down on a chip must not take the focus from the field. -->
  <div class="chips" id="quick-chips" role="group" aria-live="polite" transition:slide={{ duration: 140 }} onpointerdown={(e) => e.preventDefault()}>
    {#if due}
      <span class="qchip due">
        <CalendarDays size={13} />
        <span class="when">{dueLabel({ dueDate: due.date, dueAt })}</span>
        <button aria-label={t('tasks.quickAdd.removeDue')} title={t('tasks.quickAdd.removeDue')} onclick={() => (ignore.due = true)}><X size={11} /></button>
      </span>
    {/if}
    {#if important}
      <span class="qchip important">
        <Star size={13} fill="currentColor" />
        <span>{t('tasks.quickAdd.important')}</span>
        <button aria-label={t('tasks.quickAdd.removeImportant')} title={t('tasks.quickAdd.removeImportant')} onclick={() => (ignore.important = true)}><X size={11} /></button>
      </span>
    {/if}
    {#if smart && list}
      <button
        class="qchip list"
        title={t('tasks.quickAdd.chooseList')}
        style:--c={list.color}
        onclick={(e) => {
          const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
          picker = picker ? null : { x: r.left, y: r.bottom + 4 };
        }}
      >
        <span class="dot"></span><span>{listName(list)}</span><ChevronDown size={12} />
      </button>
    {/if}
    {#if !text.trim() && !parsed.due}
      <span class="hint">{t(list?.importance ? 'tasks.quickAdd.hint' : 'tasks.quickAdd.hintNoImportance', { example })}</span>
    {/if}
  </div>
{/if}
</div>

{#if picker}
  <Popover x={picker.x} y={picker.y} onclose={() => (picker = null)} width={250}>
    <div class="menu-label">{t('tasks.quickAdd.addTo')}</div>
    {#each tasks.writable as l (l.id)}
      <button
        class="menu-item"
        onclick={() => {
          ui.set('defaultTaskListId', l.id);
          picker = null;
          input?.focus();
        }}
      >
        <span class="pick-dot" style:background={l.color}></span>
        <span class="pick-name truncate">{listName(l)}</span>
        <span class="hint">{#if l.id === list?.id}<Check size={14} />{:else}{accountOf(l.accountId)}{/if}</span>
      </button>
    {/each}
  </Popover>
{/if}

<style>
  .quick {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 42px;
    padding: 0 10px 0 12px;
    border-radius: 11px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    color: var(--text-3);
    transition: box-shadow var(--dur-fast), background var(--dur-fast);
  }
  .quick.focused {
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--accent), 0 0 0 3px var(--accent-soft);
  }
  .quick.disabled {
    opacity: 0.7;
  }
  /* Where a task's circle is, so the new one lines up with the others. */
  .plus {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    flex-shrink: 0;
    color: var(--accent-text);
    box-shadow: inset 0 0 0 1.5px currentColor;
    opacity: 0.85;
  }
  .focused .plus {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
    opacity: 1;
  }
  input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: none;
    background: none;
    font-size: var(--text-md);
    color: var(--text);
  }
  input::placeholder {
    color: var(--text-3);
  }
  .kbd {
    flex-shrink: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 8px 2px 2px 42px;
  }
  .qchip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 4px 0 8px;
    border-radius: var(--r-full);
    font-size: var(--text-sm);
    font-weight: 550;
    white-space: nowrap;
    animation: pop-in var(--dur) var(--ease-out);
  }
  .qchip button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    opacity: 0.7;
  }
  .qchip button:hover {
    opacity: 1;
    background: rgb(0 0 0 / 0.08);
  }
  .due {
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .when {
    font-variant-numeric: tabular-nums;
  }
  .important {
    color: color-mix(in oklab, var(--star) 70%, var(--text));
    background: color-mix(in oklab, var(--star) 16%, transparent);
  }
  .list {
    padding: 0 8px;
    color: var(--text-2);
    background: var(--bg-active);
  }
  .list:hover {
    color: var(--text);
  }
  .list .dot,
  .pick-dot {
    width: 8px;
    height: 8px;
    border-radius: 3px;
    background: var(--c);
    flex-shrink: 0;
  }
  .hint {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .chips .hint {
    margin-left: 2px;
  }
  .pick-name {
    min-width: 0;
  }
</style>

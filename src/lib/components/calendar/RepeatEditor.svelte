<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { t, tParts, type MessageKey } from '../../i18n';
  import { ui } from '../../stores/ui.svelte';
  import { dateFormat } from '../../utils/format';
  import { DAY_CODES, buildRule, describeRule, monthlyChoices, repeatChoice, ruleNote, ruleProblem, type Freq } from '../../utils/rrule';
  import DatePicker from './DatePicker.svelte';
  import { Info, Repeat, Warning } from '../../icons';
  import { modal } from '../../actions/modal';

  let {
    rule,
    start,
    zone,
    kind,
    onclose,
  }: {
    /** The rule being edited; null for an event that does not repeat yet. */
    rule: string | null;
    /** The date of the first occurrence ("2026-11-10"), on the event's clocks. */
    start: string;
    zone: string;
    /** The kind of the event's calendar, which may not store every rule. */
    kind: string;
    /** The new rule, or null when cancelled. */
    onclose: (rule: string | null) => void;
  } = $props();

  // The rule as the dialog opened with it; what it was given later does not change it.
  const initial = untrack(() => repeatChoice(rule, start, zone));
  let choice = $state(structuredClone(initial));
  let first: HTMLInputElement | undefined = $state();
  onMount(() => first?.focus());

  const built = $derived(buildRule(choice));
  const summary = $derived(describeRule(built, start, zone));
  const problem = $derived(ruleProblem(built, kind));
  const note = $derived(ruleNote(built, start, kind));

  const units: { freq: Freq; label: MessageKey }[] = [
    { freq: 'DAILY', label: 'calendar.recurrence.unitDay' },
    { freq: 'WEEKLY', label: 'calendar.recurrence.unitWeek' },
    { freq: 'MONTHLY', label: 'calendar.recurrence.unitMonth' },
    { freq: 'YEARLY', label: 'calendar.recurrence.unitYear' },
  ];
  // The days of the week from the one it starts on.
  const week = $derived(Array.from({ length: 7 }, (_, i) => DAY_CODES[(ui.settings.weekStart + i) % 7]));
  const dayName = (code: string, weekday: 'long' | 'narrow') => dateFormat({ weekday }).format(new Date(2000, 0, 2 + DAY_CODES.indexOf(code)));

  function toggle(code: string) {
    if (!choice.days.includes(code)) choice.days = [...choice.days, code];
    else if (choice.days.length > 1) choice.days = choice.days.filter((d) => d !== code);
  }

  function done() {
    if (problem) return;
    // Left as it was, the rule stays as written (an end time, a week start of its own…).
    const same = JSON.stringify($state.snapshot(choice)) === JSON.stringify(initial);
    onclose(same && rule ? rule : built);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose(null);
    } else if (e.key === 'Enter' && !(e.target as HTMLElement).closest('button, select')) {
      e.preventDefault();
      e.stopPropagation();
      done();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={() => onclose(null)}>
  <div class="dialog" role="dialog" aria-label={t('calendar.recurrence.customTitle')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} use:modal>
    <h3>{t('calendar.recurrence.customTitle')}</h3>

    <!-- The sentence labels the number in it. -->
    <label class="line">
      {#each tParts('calendar.recurrence.every') as p, i (i)}
        {#if typeof p === 'string'}
          <span>{p}</span>
        {:else if p.slot === 'interval'}
          <input bind:this={first} class="input num" type="number" min="1" max="99" bind:value={choice.interval} />
        {:else}
          <select class="input unit" aria-label={t('calendar.recurrence.repeats')} bind:value={choice.freq}>
            {#each units as u (u.freq)}<option value={u.freq}>{t(u.label, { count: choice.interval })}</option>{/each}
          </select>
        {/if}
      {/each}
    </label>

    {#if choice.freq === 'WEEKLY'}
      <div class="label">{t('calendar.recurrence.repeatOn')}</div>
      <div class="days" role="group" aria-label={t('calendar.recurrence.repeatOn')}>
        {#each week as code (code)}
          {@const on = choice.days.includes(code)}
          <button type="button" class="day" class:on aria-pressed={on} aria-label={dayName(code, 'long')} title={dayName(code, 'long')} onclick={() => toggle(code)}>
            {dayName(code, 'narrow')}
          </button>
        {/each}
      </div>
    {:else if choice.freq === 'MONTHLY'}
      <select class="input monthly" aria-label={t('calendar.recurrence.repeatOn')} bind:value={choice.monthly}>
        {#each monthlyChoices(start) as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
      </select>
    {/if}

    <div class="label">{t('calendar.recurrence.ends')}</div>
    <div class="ends" role="radiogroup" aria-label={t('calendar.recurrence.ends')}>
      <label><input type="radio" name="ends" value="never" bind:group={choice.end} />{t('calendar.recurrence.endsNever')}</label>
      <label>
        <input type="radio" name="ends" value="until" bind:group={choice.end} />
        {#each tParts('calendar.recurrence.endsOn') as p, i (i)}
          {#if typeof p === 'string'}<span>{p}</span>{:else}<DatePicker bind:value={choice.until} onchange={() => (choice.end = 'until')} />{/if}
        {/each}
      </label>
      <label>
        <input type="radio" name="ends" value="count" bind:group={choice.end} />
        {#each tParts('calendar.recurrence.endsAfter', { count: choice.count }) as p, i (i)}
          {#if typeof p === 'string'}<span>{p}</span>{:else}<input class="input num" type="number" min="1" max="999" bind:value={choice.count} onfocus={() => (choice.end = 'count')} />{/if}
        {/each}
      </label>
    </div>

    <div class="summary"><Repeat size={15} /><span>{summary}</span></div>
    {#if problem}
      <div class="problem" role="alert"><Warning size={15} /><span>{problem}</span></div>
    {:else if note}
      <div class="note"><Info size={15} /><span>{note}</span></div>
    {/if}

    <footer>
      <button class="btn" onclick={() => onclose(null)}>{t('common.cancel')}</button>
      <button class="btn primary" disabled={!!problem} onclick={done}>{t('common.ok')}</button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1400;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    width: min(420px, 94vw);
    max-height: 90vh;
    overflow-y: auto;
    padding: 20px 20px 16px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  h3 {
    margin: 0 0 16px;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .line,
  .ends label {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    font-weight: 500;
    color: var(--text-2);
  }
  .num {
    width: 64px;
  }
  .unit {
    width: auto;
    min-width: 110px;
  }
  .label {
    margin: 18px 0 8px;
  }
  .days {
    display: flex;
    gap: 6px;
  }
  .day {
    width: 34px;
    height: 34px;
    border-radius: 50%;
    font-weight: 600;
    font-size: var(--text-sm);
    color: var(--text-2);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .day:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .day.on {
    color: var(--accent-fg);
    background: var(--accent-solid);
    box-shadow: none;
  }
  .monthly {
    margin-top: 12px;
  }
  .ends {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .ends label {
    min-height: 38px;
    cursor: pointer;
  }
  .ends input[type='radio'] {
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    margin: 0 4px 0 0;
    accent-color: var(--accent);
  }
  .summary,
  .problem,
  .note {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    margin-top: 18px;
    padding: 10px 12px;
    border-radius: 10px;
    line-height: 1.4;
    font-weight: 500;
  }
  .summary {
    color: var(--text);
    background: var(--bg-sunken);
  }
  .problem,
  .note {
    margin-top: 8px;
    font-size: var(--text-sm);
    font-weight: 400;
    color: var(--text-2);
  }
  .problem {
    color: var(--red);
    background: color-mix(in oklab, var(--red) 10%, transparent);
  }
  .summary :global(svg),
  .problem :global(svg),
  .note :global(svg) {
    flex-shrink: 0;
    margin-top: 1px;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }
</style>

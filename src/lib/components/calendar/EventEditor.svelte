<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { EditScope } from '../../api';
  import { cal, ymd } from '../../stores/calendar.svelte';
  import { t } from '../../i18n';
  import { dateFormat } from '../../utils/format';
  import { DAY_CODES, dayOf, describeRule, firstDay, followStart, monthlyChoices, ruleNote, ruleProblem, untilFor } from '../../utils/rrule';
  import { fromWallClock, isZone, localZone, sameZone, wallClock } from '../../utils/timezone';
  import RecipientInput from '../RecipientInput.svelte';
  import Toggle from '../settings/Toggle.svelte';
  import DatePicker from './DatePicker.svelte';
  import TimePicker from './TimePicker.svelte';
  import TimeZonePicker from './TimeZonePicker.svelte';
  import RepeatEditor from './RepeatEditor.svelte';
  import AvailabilityStrip from './AvailabilityStrip.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { confirmDiscard } from '../../stores/dialogs.svelte';
  import { AlignLeft, Bell, Calendar, Clock, Info, MapPin, Repeat, Users, Video, Warning, X } from '../../icons';

  const st = cal.editor!;
  const input = st.input;

  let title: HTMLInputElement | undefined = $state();
  onMount(() => title?.focus());

  // Dates and times as typed: on the clocks of the event's zone (all-day dates are just dates).
  const here = localZone();
  let zone = $state(isZone(input.timezone) ? input.timezone : here);
  const toMs = (date: string, time: string) => fromWallClock(date, time, zone);
  const dateOf = (ms: number) => wallClock(ms, zone).date;
  const timeOf = (ms: number) => wallClock(ms, zone).time;
  const firstDate = input.allDay ? (input.startDate ?? ymd(new Date(input.start))) : dateOf(input.start);
  let startDate = $state(firstDate);
  let startTime = $state(timeOf(input.start));
  let endDate = $state(
    input.allDay
      ? ymd(new Date(new Date(`${input.endDate ?? ymd(new Date(input.end))}T00:00:00`).getTime() - 86_400_000))
      : dateOf(input.end),
  );
  let endTime = $state(timeOf(input.end));
  let prevStart = input.start;
  let prevStartDate = firstDate;

  const weekday = $derived(dateFormat({ weekday: 'long' }).format(dayOf(startDate)));
  const dayCode = $derived(DAY_CODES[dayOf(startDate).getDay()]);
  const CUSTOM = 'custom';
  const repeatOptions = $derived([
    { value: '', label: t('calendar.recurrence.none') },
    { value: 'FREQ=DAILY', label: t('calendar.recurrence.everyDay') },
    { value: 'FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR', label: t('calendar.recurrence.everyWeekday') },
    { value: `FREQ=WEEKLY;BYDAY=${dayCode}`, label: t('calendar.recurrence.weeklyOn', { count: 1, days: weekday }) },
    { value: `FREQ=WEEKLY;INTERVAL=2;BYDAY=${dayCode}`, label: t('calendar.recurrence.weeklyOn', { count: 2, days: weekday }) },
    ...monthlyChoices(startDate).map((o) => ({ value: o.value === 'day' ? 'FREQ=MONTHLY' : `FREQ=MONTHLY;BYDAY=${o.value}`, label: o.label })),
    { value: 'FREQ=YEARLY', label: t('calendar.recurrence.yearlyOn', { count: 1, date: dateFormat({ month: 'long', day: 'numeric' }).format(dayOf(startDate)) }) },
  ]);
  let repeat = $state(input.rrule ?? '');
  const customRepeat = $derived(!!repeat && !repeatOptions.some((o) => o.value === repeat));
  let customizing = $state(false);
  let repeatMenu: HTMLSelectElement | undefined = $state();
  const calendarKind = $derived(cal.calendarById.get(input.calendarId)?.kind ?? '');
  // A calendar that cannot store a new rule says so here, rather than changing it (one it holds already, it can).
  const openedWith = { rule: input.rrule ?? '', calendarId: input.calendarId };
  const repeatProblem = $derived(
    repeat === openedWith.rule && input.calendarId === openedWith.calendarId ? null : ruleProblem(repeat, calendarKind),
  );
  const repeatNote = $derived(ruleNote(repeat, startDate, calendarKind));

  function pickRepeat(select: HTMLSelectElement) {
    if (select.value === CUSTOM) {
      select.value = repeat;
      customizing = true;
    } else setRepeat(select.value);
  }

  /** A weekly series begins on its first day; the event moves there, as Exchange would move it. */
  function setRepeat(rule: string) {
    repeat = rule;
    const day = firstDay(rule, startDate);
    if (day === startDate) return;
    const shift = Math.round((dayOf(day).getTime() - dayOf(startDate).getTime()) / 86_400_000);
    const end = dayOf(endDate);
    end.setDate(end.getDate() + shift);
    startDate = day;
    endDate = ymd(end);
    prevStart = toMs(startDate, startTime);
    prevStartDate = startDate;
  }

  /** Another zone keeps the times as typed, now on its clocks; an end date stays the same day. */
  function onZoneChange(tz: string, before: string) {
    repeat = untilFor(repeat, true, before);
    zone = tz;
    prevStart = toMs(startDate, startTime);
  }

  const alarmChoices = [0, 5, 10, 15, 30, 60, 120, 1440];
  const alarmLabel = (m: number) =>
    m === 0
      ? t('calendar.reminder.atStart')
      : m < 60
        ? t('calendar.reminder.minutesBefore', { count: m })
        : m < 1440
          ? t('calendar.reminder.hoursBefore', { count: m / 60 })
          : t('calendar.reminder.daysBefore', { count: m / 1440 });

  // Moving the start keeps the event's length, and a repeat rule naming its day follows it.
  function onStartChange() {
    const s = toMs(startDate, startTime);
    const e = toMs(endDate, endTime);
    if (input.allDay) {
      const shift = Math.round((dayOf(startDate).getTime() - dayOf(prevStartDate).getTime()) / 86_400_000);
      const end = dayOf(endDate);
      end.setDate(end.getDate() + shift);
      endDate = ymd(end) < startDate ? startDate : ymd(end);
    } else {
      const next = s + Math.max(e - prevStart, 15 * 60_000);
      endDate = dateOf(next);
      endTime = timeOf(next);
    }
    repeat = followStart(repeat, prevStartDate, startDate);
    prevStart = s;
    prevStartDate = startDate;
  }

  function onEndChange() {
    if (endDate < startDate) endDate = startDate;
  }

  function collect() {
    input.rrule = repeat ? untilFor(repeat, input.allDay, zone) : null;
    if (input.allDay) {
      input.startDate = startDate;
      const end = dayOf(endDate < startDate ? startDate : endDate);
      end.setDate(end.getDate() + 1);
      input.endDate = ymd(end);
      input.start = dayOf(startDate).getTime();
      input.end = end.getTime();
    } else {
      input.start = toMs(startDate, startTime);
      input.end = toMs(endDate, endTime);
      input.startDate = null;
      input.endDate = null;
      input.timezone = zone;
    }
  }

  async function save() {
    if (repeatProblem) return;
    collect();
    if (!input.allDay && input.end <= input.start) {
      input.end = input.start + 30 * 60_000;
    }
    let scope: EditScope = 'all';
    if (st.eventId != null && st.isRecurring) {
      const s = await cal.askScope(t('calendar.scope.saveTitle'));
      if (!s) return;
      scope = s;
    }
    await cal.save(st, scope);
  }

  // What the editor opened with: closing it asks first once something was changed.
  const entered = () => JSON.stringify({ input, startDate, startTime, endDate, endTime, repeat, zone });
  const opened = entered();
  let asking = false;
  async function close() {
    if (asking) return;
    if (entered() !== opened) {
      asking = true;
      const discard = await confirmDiscard();
      asking = false;
      if (!discard) return;
    }
    if (cal.editor === st) cal.editor = null;
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      close();
    } else if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault();
      save();
    }
  }

  const calendarColor = $derived(cal.calendarById.get(input.calendarId)?.color ?? 'var(--accent)');
  // The meeting as currently entered, for the guests' availability.
  const slotStart = $derived(toMs(startDate, startTime));
  const slotEnd = $derived(toMs(endDate, endTime));
  const organizer = $derived.by(() => {
    const accountId = cal.calendarById.get(input.calendarId)?.accountId;
    return mail.accounts.find((a) => a.id === accountId)?.email ?? mail.accounts[0]?.email ?? null;
  });

  /** Moves the meeting to `at`, keeping its length. */
  function moveTo(at: number) {
    const length = slotEnd - slotStart;
    startDate = dateOf(at);
    startTime = timeOf(at);
    endDate = dateOf(at + length);
    endTime = timeOf(at + length);
    prevStartDate = startDate;
  }
</script>

<div class="backdrop" role="presentation" onclick={close}>
  <div class="editor" role="dialog" aria-label={t('calendar.editor.dialogLabel')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} style:--c={calendarColor}>
    <header>
      <span class="sw"></span>
      <input bind:this={title} class="title" placeholder={t('calendar.editor.titlePlaceholder')} bind:value={input.summary} />
      <button class="icon-btn" aria-label={t('common.close')} onclick={close}><X size={18} /></button>
    </header>

    <div class="rows">
      <div class="row">
        <Clock size={17} />
        <div class="when">
          <div class="dt">
            <DatePicker bind:value={startDate} onchange={onStartChange} />
            {#if !input.allDay}<TimePicker bind:value={startTime} onchange={onStartChange} />{/if}
            <span class="to">–</span>
            {#if !input.allDay}<TimePicker bind:value={endTime} after={endDate === startDate ? startTime : null} />{/if}
            <DatePicker bind:value={endDate} onchange={onEndChange} />
          </div>
          <div class="options">
            <label class="inline"><Toggle checked={input.allDay} onchange={(v) => (input.allDay = v)} /> {t('calendar.editor.allDay')}</label>
            {#if !input.allDay}
              <TimeZonePicker
                variant="link"
                value={zone}
                label={sameZone(zone, here) ? t('calendar.editor.timeZone') : null}
                onchange={(tz) => onZoneChange(tz, zone)}
              />
            {/if}
          </div>
        </div>
      </div>

      <div class="row">
        <Repeat size={17} />
        <div class="grow repeat">
          <select bind:this={repeatMenu} class="input" value={repeat} onchange={(e) => pickRepeat(e.currentTarget)}>
            {#each repeatOptions as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
            {#if customRepeat}<option value={repeat}>{describeRule(repeat, startDate, zone)}</option>{/if}
            <option value={CUSTOM}>{t('calendar.recurrence.custom')}</option>
          </select>
          {#if repeatProblem}
            <div class="hint problem" role="alert"><Warning size={14} /><span>{repeatProblem}</span></div>
          {:else if repeatNote}
            <div class="hint"><Info size={14} /><span>{repeatNote}</span></div>
          {/if}
        </div>
      </div>

      <div class="row">
        <Calendar size={17} />
        <select class="input" bind:value={input.calendarId}>
          {#each cal.writable as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
        </select>
      </div>

      <div class="row">
        <MapPin size={17} />
        <input class="input" placeholder={t('calendar.editor.locationPlaceholder')} bind:value={input.location} />
      </div>

      {#if st.eventId == null && ['google', 'graph'].includes(cal.calendarById.get(input.calendarId)?.kind ?? '')}
        <div class="row">
          <Video size={17} />
          <label class="inline">
            <Toggle checked={!!input.addConference} onchange={(v) => (input.addConference = v)} />
            {t('calendar.editor.addVideoCall', { service: cal.calendarById.get(input.calendarId)?.kind === 'google' ? 'Google Meet' : 'Microsoft Teams' })}
          </label>
        </div>
      {/if}

      <div class="row people">
        <Users size={17} />
        <div class="grow">
          <RecipientInput label={t('calendar.editor.guests')} bind:value={input.attendees} />
          {#if !input.allDay && input.attendees.length && slotEnd > slotStart}
            <AvailabilityStrip
              calendarId={input.calendarId}
              {organizer}
              attendees={input.attendees}
              start={slotStart}
              end={slotEnd}
              onpick={moveTo}
            />
          {/if}
        </div>
      </div>

      <div class="row">
        <Bell size={17} />
        <div class="alarms">
          {#each input.alarms as m, i (i)}
            <span class="alarm">{alarmLabel(m)}<button aria-label={t('calendar.editor.removeReminder')} onclick={() => (input.alarms = input.alarms.filter((_, j) => j !== i))}><X size={11} /></button></span>
          {/each}
          <select class="input add" onchange={(e) => { const v = Number((e.currentTarget as HTMLSelectElement).value); if (!Number.isNaN(v) && !input.alarms.includes(v)) input.alarms = [...input.alarms, v]; (e.currentTarget as HTMLSelectElement).value = ''; }}>
            <option value="">{t('calendar.editor.addReminder')}</option>
            {#each alarmChoices as m (m)}<option value={m}>{alarmLabel(m)}</option>{/each}
          </select>
        </div>
      </div>

      <div class="row">
        <AlignLeft size={17} />
        <textarea class="input area" rows="4" placeholder={t('calendar.editor.descriptionPlaceholder')} bind:value={input.description}></textarea>
      </div>
    </div>

    <footer>
      <label class="inline busy"><Toggle checked={!input.transparent} onchange={(v) => (input.transparent = !v)} /> {t('calendar.editor.showAsBusy')}</label>
      <span class="grow"></span>
      <button class="btn" onclick={close}>{t('common.cancel')}</button>
      <button class="btn primary" disabled={!!repeatProblem} onclick={save}>{st.eventId == null ? t('calendar.editor.create') : t('common.save')}</button>
    </footer>
  </div>
</div>

{#if customizing}
  <RepeatEditor
    rule={repeat || null}
    start={startDate}
    {zone}
    kind={calendarKind}
    onclose={(rule) => {
      customizing = false;
      tick().then(() => repeatMenu?.focus());
      if (rule != null) setRepeat(rule);
    }}
  />
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1300;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .editor {
    width: min(640px, 94vw);
    max-height: 90vh;
    overflow-y: auto;
    border-radius: 18px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 18px 18px 10px 22px;
  }
  .sw {
    width: 14px;
    height: 14px;
    border-radius: 5px;
    background: var(--c);
    flex-shrink: 0;
  }
  .title {
    flex: 1;
    border: none;
    background: none;
    font-size: var(--text-xl);
    font-weight: 650;
    height: 36px;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 6px 22px 10px;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: 14px;
    color: var(--text-3);
  }
  .row > :global(svg) {
    flex-shrink: 0;
    margin-top: 8px;
  }
  .row > .input,
  .row > select {
    flex: 1;
  }
  .when {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .dt {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .to {
    color: var(--text-3);
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    color: var(--text-2);
    font-weight: 500;
    min-height: 34px;
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 24px;
  }
  .repeat {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .hint {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: var(--text-sm);
    line-height: 1.4;
    color: var(--text-3);
  }
  .hint :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
  }
  .hint.problem {
    color: var(--red);
  }
  .people :global(.field) {
    border-bottom: none;
    background: var(--bg-input);
    border-radius: var(--r);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    padding: 2px 8px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .alarms {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .alarm {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 6px 0 11px;
    border-radius: 8px;
    font-weight: 500;
    color: var(--text);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .alarm button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    color: var(--text-3);
  }
  .add {
    width: auto;
  }
  .area {
    height: auto;
    padding: 8px 11px;
    resize: vertical;
  }
  footer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 12px 18px 18px 22px;
    border-top: 1px solid var(--border);
  }
</style>

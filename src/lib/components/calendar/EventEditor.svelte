<script lang="ts">
  import { onMount } from 'svelte';
  import type { EditScope } from '../../api';
  import { cal, ymd } from '../../stores/calendar.svelte';
  import { t } from '../../i18n';
  import { dateFormat } from '../../utils/format';
  import { describeRule } from '../../utils/rrule';
  import RecipientInput from '../RecipientInput.svelte';
  import Toggle from '../settings/Toggle.svelte';
  import DatePicker from './DatePicker.svelte';
  import TimePicker from './TimePicker.svelte';
  import AvailabilityStrip from './AvailabilityStrip.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { confirmDiscard } from '../../stores/dialogs.svelte';
  import { AlignLeft, Bell, Calendar, Clock, MapPin, Repeat, Users, Video, X } from '../../icons';

  const st = cal.editor!;
  const input = st.input;

  let title: HTMLInputElement | undefined = $state();
  onMount(() => title?.focus());

  // Local date/time fields.
  const pad = (n: number) => String(n).padStart(2, '0');
  const dateOf = (ms: number) => ymd(new Date(ms));
  const timeOf = (ms: number) => `${pad(new Date(ms).getHours())}:${pad(new Date(ms).getMinutes())}`;
  let startDate = $state(input.allDay ? (input.startDate ?? dateOf(input.start)) : dateOf(input.start));
  let startTime = $state(timeOf(input.start));
  let endDate = $state(
    input.allDay
      ? ymd(new Date(new Date(`${input.endDate ?? dateOf(input.end)}T00:00:00`).getTime() - 86_400_000))
      : dateOf(input.end),
  );
  let endTime = $state(timeOf(input.end));
  let prevStart = input.start;
  let prevStartDate = input.allDay ? (input.startDate ?? dateOf(input.start)) : dateOf(input.start);

  const weekday = $derived(dateFormat({ weekday: 'long' }).format(new Date(`${startDate}T00:00:00`)));
  const dayCode = $derived(['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'][new Date(`${startDate}T00:00:00`).getDay()]);
  const repeatOptions = $derived([
    { value: '', label: t('calendar.recurrence.none') },
    { value: 'FREQ=DAILY', label: t('calendar.recurrence.everyDay') },
    { value: 'FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR', label: t('calendar.recurrence.everyWeekday') },
    { value: `FREQ=WEEKLY;BYDAY=${dayCode}`, label: t('calendar.recurrence.weeklyOn', { count: 1, days: weekday }) },
    { value: `FREQ=WEEKLY;INTERVAL=2;BYDAY=${dayCode}`, label: t('calendar.recurrence.weeklyOn', { count: 2, days: weekday }) },
    { value: 'FREQ=MONTHLY', label: t('calendar.recurrence.monthlyOnDay', { day: new Date(`${startDate}T00:00:00`).getDate() }) },
    { value: 'FREQ=YEARLY', label: t('calendar.recurrence.everyYear') },
  ]);
  let repeat = $state(input.rrule ?? '');
  const customRepeat = $derived(!!repeat && !repeatOptions.some((o) => o.value === repeat));

  const alarmChoices = [0, 5, 10, 15, 30, 60, 120, 1440];
  const alarmLabel = (m: number) =>
    m === 0
      ? t('calendar.reminder.atStart')
      : m < 60
        ? t('calendar.reminder.minutesBefore', { count: m })
        : m < 1440
          ? t('calendar.reminder.hoursBefore', { count: m / 60 })
          : t('calendar.reminder.daysBefore', { count: m / 1440 });

  // Moving the start keeps the event's length.
  function onStartChange() {
    const s = new Date(`${startDate}T${startTime}`).getTime();
    const e = new Date(`${endDate}T${endTime}`).getTime();
    if (input.allDay) {
      const shift = Math.round((new Date(`${startDate}T00:00:00`).getTime() - new Date(`${prevStartDate}T00:00:00`).getTime()) / 86_400_000);
      const end = new Date(`${endDate}T00:00:00`);
      end.setDate(end.getDate() + shift);
      endDate = ymd(end) < startDate ? startDate : ymd(end);
    } else {
      const next = s + Math.max(e - prevStart, 15 * 60_000);
      endDate = dateOf(next);
      endTime = timeOf(next);
    }
    prevStart = s;
    prevStartDate = startDate;
  }

  function onEndChange() {
    if (endDate < startDate) endDate = startDate;
  }

  function collect() {
    input.rrule = repeat || null;
    if (input.allDay) {
      input.startDate = startDate;
      const end = new Date(`${endDate < startDate ? startDate : endDate}T00:00:00`);
      end.setDate(end.getDate() + 1);
      input.endDate = ymd(end);
      input.start = new Date(`${startDate}T00:00:00`).getTime();
      input.end = end.getTime();
    } else {
      input.start = new Date(`${startDate}T${startTime}`).getTime();
      input.end = new Date(`${endDate}T${endTime}`).getTime();
      input.startDate = null;
      input.endDate = null;
    }
  }

  async function save() {
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
  const entered = () => JSON.stringify({ input, startDate, startTime, endDate, endTime, repeat });
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
  const slotStart = $derived(new Date(`${startDate}T${startTime}`).getTime());
  const slotEnd = $derived(new Date(`${endDate}T${endTime}`).getTime());
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
          <label class="inline"><Toggle checked={input.allDay} onchange={(v) => (input.allDay = v)} /> {t('calendar.editor.allDay')}</label>
        </div>
      </div>

      <div class="row">
        <Repeat size={17} />
        <select class="input" bind:value={repeat}>
          {#each repeatOptions as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
          {#if customRepeat}<option value={repeat}>{describeRule(repeat)}</option>{/if}
        </select>
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
      <button class="btn primary" onclick={save}>{st.eventId == null ? t('calendar.editor.create') : t('common.save')}</button>
    </footer>
  </div>
</div>

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

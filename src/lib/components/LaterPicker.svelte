<script lang="ts">
  // A later day and time, picked with the calendar's own date and time pickers (in the interface
  // language's format): a custom snooze, reminder or send time.
  import { ymd } from '../stores/calendar.svelte';
  import DatePicker from './calendar/DatePicker.svelte';
  import TimePicker from './calendar/TimePicker.svelte';

  let { label, onpick }: { /** The button that takes the time ("Set", "Schedule"). */ label: string; onpick: (ts: number) => void } = $props();

  // From the next full hour.
  const next = new Date(Date.now() + 3600_000);
  next.setMinutes(0, 0, 0);
  let day = $state(ymd(next));
  let time = $state(`${String(next.getHours()).padStart(2, '0')}:00`);
  const at = $derived(new Date(`${day}T${time}:00`).getTime());
</script>

<div class="later">
  <DatePicker bind:value={day} />
  <TimePicker bind:value={time} />
  <button class="btn primary" disabled={!(at > Date.now())} onclick={() => onpick(at)}>{label}</button>
</div>

<style>
  /* The button goes below the fields when a translation leaves no room beside them. */
  .later {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 4px;
  }
  .btn {
    flex-shrink: 0;
    height: 34px;
    margin-left: auto;
  }
</style>

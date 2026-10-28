<script lang="ts">
  import Popover from './Popover.svelte';
  import { Clock, Moon, Sun, Calendar, CalendarDays } from '../icons';

  let { x, y, align = 'start', onpick, onclose }: { x: number; y: number; align?: 'start' | 'end'; onpick: (ts: number) => void; onclose: () => void } = $props();

  function at(daysFromNow: number, hour: number): Date {
    const d = new Date();
    d.setDate(d.getDate() + daysFromNow);
    d.setHours(hour, 0, 0, 0);
    return d;
  }

  const options = $derived.by(() => {
    const now = new Date();
    const list: { label: string; when: Date; icon: typeof Clock }[] = [];
    const later = new Date(now.getTime() + 3 * 3600_000);
    later.setMinutes(0, 0, 0);
    if (later.getDate() === now.getDate()) list.push({ label: 'Later today', when: later, icon: Clock });
    if (now.getHours() < 18) list.push({ label: 'This evening', when: at(0, 19), icon: Moon });
    list.push({ label: 'Tomorrow', when: at(1, 8), icon: Sun });
    const day = now.getDay();
    if (day !== 6 && day !== 0) list.push({ label: 'This weekend', when: at(6 - day, 9), icon: Calendar });
    list.push({ label: 'Next week', when: at(((8 - day) % 7) || 7, 8), icon: CalendarDays });
    return list;
  });

  const fmt = new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' });
  let custom = $state('');

  function pickCustom() {
    if (!custom) return;
    const ts = new Date(custom).getTime();
    if (ts > Date.now()) onpick(ts);
  }
</script>

<Popover {x} {y} {align} {onclose} width={260}>
  <div class="menu-label">Snooze until</div>
  {#each options as o (o.label)}
    <button class="menu-item" onclick={() => onpick(o.when.getTime())}>
      <o.icon size={15} />
      {o.label}
      <span class="hint">{fmt.format(o.when)}</span>
    </button>
  {/each}
  <div class="divider"></div>
  <div class="custom">
    <input class="input" type="datetime-local" bind:value={custom} />
    <button class="btn sm primary" disabled={!custom} onclick={pickCustom}>Set</button>
  </div>
</Popover>

<style>
  .custom {
    display: flex;
    gap: 6px;
    padding: 4px;
  }
  .custom .input {
    height: 28px;
    font-size: var(--text-sm);
  }
</style>

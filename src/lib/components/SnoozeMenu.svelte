<script lang="ts">
  import Popover from './Popover.svelte';
  import LaterPicker from './LaterPicker.svelte';
  import { Clock, Moon, Sun, Calendar, CalendarDays } from '../icons';
  import { t } from '../i18n';
  import { dateFormat } from '../utils/format';

  let {
    x,
    y,
    align = 'start',
    title,
    onpick,
    onclose,
  }: { x: number; y: number; align?: 'start' | 'end'; /** The heading (else "Snooze until"). */ title?: string; onpick: (ts: number) => void; onclose: () => void } = $props();

  function at(daysFromNow: number, hour: number): Date {
    const d = new Date();
    d.setDate(d.getDate() + daysFromNow);
    d.setHours(hour, 0, 0, 0);
    return d;
  }

  const options = $derived.by(() => {
    const now = new Date();
    const list: { id: string; label: string; when: Date; icon: typeof Clock }[] = [];
    const later = new Date(now.getTime() + 3 * 3600_000);
    later.setMinutes(0, 0, 0);
    if (later.getDate() === now.getDate()) list.push({ id: 'later', label: t('snooze.laterToday'), when: later, icon: Clock });
    if (now.getHours() < 18) list.push({ id: 'evening', label: t('snooze.thisEvening'), when: at(0, 19), icon: Moon });
    list.push({ id: 'tomorrow', label: t('snooze.tomorrow'), when: at(1, 8), icon: Sun });
    const day = now.getDay();
    if (day !== 6 && day !== 0) list.push({ id: 'weekend', label: t('snooze.thisWeekend'), when: at(6 - day, 9), icon: Calendar });
    list.push({ id: 'week', label: t('snooze.nextWeek'), when: at(((8 - day) % 7) || 7, 8), icon: CalendarDays });
    return list;
  });

  const fmt = $derived(dateFormat({ weekday: 'short', hour: 'numeric', minute: '2-digit' }));
</script>

<Popover {x} {y} {align} {onclose} width={290}>
  <div class="menu-label">{title ?? t('snooze.until')}</div>
  {#each options as o (o.id)}
    <button class="menu-item" onclick={() => onpick(o.when.getTime())}>
      <o.icon size={15} />
      {o.label}
      <span class="hint">{fmt.format(o.when)}</span>
    </button>
  {/each}
  <div class="divider"></div>
  <LaterPicker label={t('snooze.set')} {onpick} />
</Popover>

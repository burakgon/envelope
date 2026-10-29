<script lang="ts">
  import { api } from '../../api';
  import type { CalendarInfo } from '../../api';
  import { cal, addDays, span } from '../../stores/calendar.svelte';
  import { BIRTHDAY_COLOR } from '../../utils/birthdays';
  import { ui } from '../../stores/ui.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { t } from '../../i18n';
  import Popover from '../Popover.svelte';
  import MiniMonth from './MiniMonth.svelte';
  import { Check, ChevronDown, Palette, Plus, Refresh, Trash } from '../../icons';

  const swatches = ['#4f7cff', '#2bb3a3', '#34a853', '#a3c23a', '#f4b400', '#f08a24', '#e8453c', '#e3508f', '#9b59d0', '#6d6f78'];

  let collapsed = $state<Record<string, boolean>>({});
  let colorFor = $state<{ c: CalendarInfo; x: number; y: number } | null>(null);

  const groups = $derived.by(() => {
    const map = new Map<string, { key: string; label: string; list: CalendarInfo[] }>();
    for (const c of cal.calendars) {
      const account = c.accountId != null ? mail.accounts.find((a) => a.id === c.accountId) : null;
      const subscribed = c.kind === 'subscription';
      const key = account ? String(account.id) : subscribed ? 'subscribed' : 'local';
      const label = account ? (account.label ?? account.email) : subscribed ? t('calendar.sidebar.subscribed') : t('calendar.sidebar.onThisComputer');
      if (!map.has(key)) map.set(key, { key, label, list: [] });
      map.get(key)!.list.push(c);
    }
    return [...map.values()];
  });

  const busy = $derived.by(() => {
    const set = new Set<number>();
    for (const o of cal.occurrences) {
      const s = span(o);
      for (let day = new Date(s.start).setHours(0, 0, 0, 0); day < s.end; day = addDays(day, 1).getTime()) set.add(day);
    }
    return set;
  });
  const range = $derived(cal.view === 'day' || cal.view === 'workweek' || cal.view === 'week' ? cal.range : [null, null]);

  async function refresh(c: CalendarInfo) {
    colorFor = null;
    try {
      await api.calendar.refresh(c.id);
      cal.load();
      toasts.show(t('calendar.sidebar.refreshed', { name: c.name }), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function unsubscribe(c: CalendarInfo) {
    colorFor = null;
    try {
      await api.calendar.unsubscribe(c.id);
      await cal.loadCalendars();
      cal.load();
      toasts.show(t('calendar.sidebar.unsubscribed', { name: c.name }));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function setColor(c: CalendarInfo, color: string) {
    colorFor = null;
    const prev = c.color;
    c.color = color;
    try {
      await api.calendar.setColor(c.id, color);
      cal.load();
    } catch (e) {
      c.color = prev;
      toasts.error(errorMessage(e));
    }
  }
</script>

<MiniMonth value={cal.focus} onpick={(day) => cal.goto(day)} from={range[0]} to={range[1]} marks={busy} />

{#each groups as g (g.key)}
  {@const open = !collapsed[g.key]}
  <div class="group">
    <button class="ghead" onclick={() => (collapsed[g.key] = open)}>
      <span class="truncate">{groups.length > 1 || g.key === 'local' ? g.label : t('calendar.sidebar.calendars')}</span>
      <span class="chev" class:closed={!open}><ChevronDown size={13} /></span>
    </button>
    {#if open}
      {#each g.list as c (c.id)}
        <div class="cal-row">
          <button class="toggle" role="checkbox" aria-checked={c.visible} onclick={() => cal.setVisible(c, !c.visible)}>
            <span class="box" class:on={c.visible} style:--c={c.color}>{#if c.visible}<Check size={11} strokeWidth={3} />{/if}</span>
            <span class="name truncate" title={c.name}>{c.name}</span>
          </button>
          <button
            class="icon-btn sm color"
            aria-label={t('calendar.sidebar.calendarColor')}
            onclick={(e) => {
              const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
              colorFor = { c, x: r.left, y: r.bottom + 4 };
            }}><Palette size={13} /></button>
        </div>
      {/each}
    {/if}
  </div>
{:else}
  <div class="none">{t('calendar.sidebar.empty')}</div>
{/each}

{#if cal.birthdays.length}
  {@const open = !collapsed.contacts}
  {@const shown = ui.settings.calendarBirthdays}
  <div class="group">
    <button class="ghead" onclick={() => (collapsed.contacts = open)}>
      <span class="truncate">{t('sidebar.modeContacts')}</span>
      <span class="chev" class:closed={!open}><ChevronDown size={13} /></span>
    </button>
    {#if open}
      <div class="cal-row">
        <button
          class="toggle"
          role="checkbox"
          aria-checked={shown}
          onclick={() => {
            ui.set('calendarBirthdays', !shown);
            cal.load();
          }}
        >
          <span class="box" class:on={shown} style:--c={BIRTHDAY_COLOR}>{#if shown}<Check size={11} strokeWidth={3} />{/if}</span>
          <span class="name truncate">{t('birthdays.calendar')}</span>
        </button>
      </div>
    {/if}
  </div>
{/if}

<button class="subscribe" onclick={() => (cal.subscribing = '')}><Plus size={13} />{t('calendar.sidebar.subscribe')}</button>

{#if colorFor}
  {@const target = colorFor.c}
  <Popover x={colorFor.x} y={colorFor.y} onclose={() => (colorFor = null)}>
    <div class="swatches">
      {#each swatches as s (s)}
        <button class="sw" class:on={target.color.toLowerCase() === s} style:background={s} aria-label={s} onclick={() => setColor(target, s)}></button>
      {/each}
    </div>
    {#if target.kind === 'subscription'}
      <div class="menu-sep"></div>
      <button class="menu-item" onclick={() => refresh(target)}><Refresh size={14} />{t('calendar.sidebar.refreshNow')}</button>
      <button class="menu-item danger" onclick={() => unsubscribe(target)}><Trash size={14} />{t('calendar.sidebar.unsubscribe')}</button>
    {/if}
  </Popover>
{/if}

<style>
  .group {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-top: 6px;
  }
  .ghead {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 10px;
    font-size: 11.5px;
    font-weight: 650;
    color: var(--text-3);
    text-align: left;
  }
  .ghead span:first-child {
    flex: 1;
    min-width: 0;
  }
  .chev {
    display: grid;
    transition: transform var(--dur);
  }
  .chev.closed {
    transform: rotate(-90deg);
  }
  .cal-row {
    display: flex;
    align-items: center;
    border-radius: 8px;
  }
  .cal-row:hover {
    background: var(--bg-hover);
  }
  .toggle {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 31px;
    padding: 0 4px 0 10px;
    color: var(--text-2);
    font-weight: 500;
    text-align: left;
  }
  .cal-row:hover .toggle {
    color: var(--text);
  }
  .box {
    width: 16px;
    height: 16px;
    border-radius: 5px;
    display: grid;
    place-items: center;
    flex-shrink: 0;
    color: white;
    box-shadow: inset 0 0 0 1.5px var(--c);
    transition: background var(--dur-fast);
  }
  .box.on {
    background: var(--c);
  }
  .name {
    flex: 1;
  }
  .color {
    opacity: 0;
    margin-right: 4px;
  }
  .cal-row:hover .color,
  .color:focus-visible {
    opacity: 1;
  }
  .none {
    padding: 10px 12px;
    font-size: var(--text-sm);
    color: var(--text-3);
    line-height: 1.5;
  }
  .swatches {
    display: grid;
    grid-template-columns: repeat(5, 24px);
    gap: 8px;
    padding: 6px;
  }
  .sw {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    transition: transform var(--dur-fast);
  }
  .sw:hover {
    transform: scale(1.12);
  }
  .sw.on {
    box-shadow: 0 0 0 2px var(--bg-elevated), 0 0 0 4px var(--text-3);
  }
  .menu-sep {
    height: 1px;
    margin: 6px 4px;
    background: var(--border);
  }
  .subscribe {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 8px 6px 0;
    padding: 6px 8px;
    border-radius: 8px;
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .subscribe:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
</style>

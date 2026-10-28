<script lang="ts">
  import { api } from '../../api';
  import type { Address, Availability } from '../../api';
  import { Spinner } from '../../icons';
  import { displayName } from '../../utils/format';

  /**
   * When the guests (and the organizer) are busy on the meeting's day, with
   * the meeting drawn on top and a button that moves it to the next time
   * everyone is free.
   */
  let {
    calendarId,
    organizer,
    attendees,
    start,
    end,
    onpick,
  }: {
    calendarId: number;
    /** The address the meeting is sent from, if known. */
    organizer: string | null;
    attendees: Address[];
    start: number;
    end: number;
    /** Moves the meeting to start at this time (same length). */
    onpick: (start: number) => void;
  } = $props();

  const HOUR = 3_600_000;
  const DAY = 24 * HOUR;

  let rows = $state<Availability[]>([]);
  let loading = $state(false);
  let searching = $state(false);
  let notFound = $state(false);

  const people = $derived([...new Set([organizer, ...attendees.map((a) => a.email)].filter((e): e is string => !!e).map((e) => e.toLowerCase()))]);
  const dayStart = $derived.by(() => {
    const d = new Date(start);
    d.setHours(0, 0, 0, 0);
    return d.getTime();
  });
  // Working hours, widened to fit the meeting.
  const from = $derived(Math.min(dayStart + 7 * HOUR, Math.floor((start - dayStart) / HOUR) * HOUR + dayStart));
  const to = $derived(Math.max(dayStart + 20 * HOUR, Math.ceil((end - dayStart) / HOUR) * HOUR + dayStart));
  const span = $derived(to - from);
  const hours = $derived(Array.from({ length: Math.round(span / HOUR) + 1 }, (_, i) => from + i * HOUR));

  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const emails = people;
    const day = dayStart;
    const calendar = calendarId;
    clearTimeout(timer);
    notFound = false;
    if (!emails.length) {
      rows = [];
      return;
    }
    timer = setTimeout(async () => {
      loading = true;
      try {
        rows = await api.calendar.freeBusy(calendar, emails, day, day + DAY);
      } catch {
        rows = emails.map((email) => ({ email, busy: null }));
      } finally {
        loading = false;
      }
    }, 350);
    return () => clearTimeout(timer);
  });

  function label(email: string): string {
    if (email === organizer?.toLowerCase()) return 'You';
    const a = attendees.find((x) => x.email.toLowerCase() === email);
    return a ? displayName(a) : email;
  }

  function pct(ms: number): number {
    return Math.min(100, Math.max(0, ((ms - from) / span) * 100));
  }

  /** The first slot from the meeting's start on, within the next two weeks, when everyone who can be seen is free. */
  async function nextFree() {
    searching = true;
    notFound = false;
    try {
      const length = end - start;
      const windowEnd = dayStart + 14 * DAY;
      const answers = await api.calendar.freeBusy(calendarId, people, start, windowEnd);
      const busy = answers.flatMap((a) => a.busy ?? []);
      const step = 15 * 60_000;
      let t = Math.ceil((start + step) / step) * step;
      while (t + length <= windowEnd) {
        const d = new Date(t);
        const weekday = d.getDay();
        const minutes = d.getHours() * 60 + d.getMinutes();
        const endMinutes = minutes + length / 60_000;
        const working = weekday !== 0 && weekday !== 6 && minutes >= 8 * 60 && endMinutes <= 18 * 60;
        if (working && !busy.some((b) => b.start < t + length && b.end > t)) {
          onpick(t);
          return;
        }
        t += step;
      }
      notFound = true;
    } catch {
      notFound = true;
    } finally {
      searching = false;
    }
  }
</script>

{#if people.length > 1}
  <div class="availability">
    <div class="scale" aria-hidden="true">
      <span class="who"></span>
      <div class="track">
        {#each hours as h, i (h)}
          {#if i % 2 === 0 && i < hours.length - 1}
            <span class="tick" style:left="{pct(h)}%">{new Date(h).toLocaleTimeString(undefined, { hour: 'numeric' })}</span>
          {/if}
        {/each}
      </div>
    </div>
    {#each people as email (email)}
      {@const row = rows.find((r) => r.email.toLowerCase() === email)}
      <div class="row">
        <span class="who" title={email}>{label(email)}</span>
        <div class="track lane">
          {#each hours as h (h)}<span class="grid" style:left="{pct(h)}%"></span>{/each}
          {#if row?.busy}
            {#each row.busy as b (b.start)}
              {#if b.end > from && b.start < to}
                <span
                  class="busy"
                  class:tentative={b.tentative}
                  style:left="{pct(b.start)}%"
                  style:width="{pct(b.end) - pct(b.start)}%"
                  title={`${new Date(b.start).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })} – ${new Date(b.end).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}${b.tentative ? ' (tentative)' : ''}`}
                ></span>
              {/if}
            {/each}
          {:else if row && !loading}
            <span class="unknown">Calendar not shared</span>
          {/if}
          <span class="slot" style:left="{pct(start)}%" style:width="{pct(end) - pct(start)}%"></span>
        </div>
      </div>
    {/each}
    <div class="actions">
      {#if loading}<span class="spin"><Spinner size={13} /></span>{/if}
      {#if notFound}<span class="note">No time when everyone is free in the next two weeks.</span>{/if}
      <span class="grow"></span>
      <button class="btn sm" disabled={searching || loading} onclick={nextFree}>
        {#if searching}<span class="spin"><Spinner size={13} /></span>{/if}Next time everyone is free
      </button>
    </div>
  </div>
{/if}

<style>
  .availability {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
    padding: 10px 12px;
    border-radius: 12px;
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .scale,
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .who {
    width: 96px;
    flex-shrink: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .track {
    position: relative;
    flex: 1;
    height: 16px;
  }
  .lane {
    height: 22px;
    border-radius: 6px;
    background: var(--bg-hover);
    overflow: hidden;
  }
  .tick {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    font-size: var(--text-xs);
    color: var(--text-3);
    white-space: nowrap;
  }
  .grid {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border);
  }
  .busy {
    position: absolute;
    top: 3px;
    bottom: 3px;
    border-radius: 4px;
    background: color-mix(in oklab, var(--text-3) 55%, transparent);
  }
  .busy.tentative {
    background: repeating-linear-gradient(
      135deg,
      color-mix(in oklab, var(--text-3) 45%, transparent) 0 4px,
      transparent 4px 8px
    );
  }
  .slot {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: 6px;
    background: color-mix(in oklab, var(--accent) 22%, transparent);
    box-shadow: inset 0 0 0 1.5px var(--accent);
    pointer-events: none;
  }
  .unknown {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    padding-left: 8px;
    font-size: var(--text-xs);
    color: var(--text-3);
    font-style: italic;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }
  .note {
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .grow {
    flex: 1;
  }
  .spin {
    display: inline-grid;
    color: var(--text-3);
    animation: spin 1s linear infinite;
  }
</style>

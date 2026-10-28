<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { InviteInfo, MessageView, Occurrence, PartStat } from '../api';
  import { ui } from '../stores/ui.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { displayName } from '../utils/format';
  import { CalendarDays, CheckCircle, MapPin, Repeat, Users, Video, Warning } from '../icons';

  let { message }: { message: MessageView } = $props();

  let info = $state<InviteInfo | null>(null);
  let failed = $state(false);
  let busy = $state<PartStat | null>(null);
  let conflicts = $state<Occurrence[] | null>(null);

  const cancelled = $derived((info?.method ?? message.calendarMethod) === 'CANCEL');
  const fallbackTitle = $derived(message.subject.replace(/^(invitation|updated invitation|einladung|davet):\s*/i, ''));

  onMount(async () => {
    try {
      const i = await api.calendar.invite(message.id);
      info = i;
      if (i.method !== 'CANCEL') {
        const list = await api.calendar.occurrences(i.start - 86_400_000, i.end + 86_400_000);
        conflicts = list.filter(
          (o) => o.uid !== i.uid && !o.transparent && o.myStatus !== 'declined' && o.status !== 'cancelled' && !(o.allDay && !i.allDay) && o.start < i.end && o.end > i.start,
        );
      }
    } catch {
      failed = true;
    }
  });

  const date = $derived(info ? new Date(info.start) : null);
  const when = $derived.by(() => {
    if (!info) return '';
    const day = new Date(info.start).toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
    if (info.allDay) return `${day} · All day`;
    const t = (x: number) => new Date(x).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
    return `${day} · ${t(info.start)} – ${t(info.end)}`;
  });

  async function respond(status: PartStat) {
    if (!info || busy) return;
    busy = status;
    try {
      await api.calendar.respondInvite(message.id, status);
      info.myStatus = status;
      toasts.show(
        status === 'accepted' ? 'Accepted — added to your calendar' : status === 'tentative' ? 'Replied maybe' : 'Declined',
        { kind: 'success' },
      );
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = null;
    }
  }

  async function removeCancelled() {
    if (info?.eventId == null) return;
    try {
      await api.calendar.remove(info.eventId, null, 'all');
      info.eventId = null;
      toasts.show('Removed from your calendar');
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  function show() {
    if (!info) return;
    ui.view = 'calendar';
    cal.view = 'week';
    cal.goto(info.start);
  }

  const time = (t: number) => new Date(t).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
</script>

{#if info && !failed}
  <div class="invite" class:cancelled>
    <div class="top">
      <button class="date" onclick={show} title="Show in calendar">
        <span class="m">{date!.toLocaleDateString(undefined, { month: 'short' })}</span>
        <span class="d">{date!.getDate()}</span>
        <span class="w">{date!.toLocaleDateString(undefined, { weekday: 'short' })}</span>
      </button>
      <div class="text">
        <div class="kind">
          {cancelled ? 'Event cancelled' : 'Invitation'}{#if info.organizer}<span class="from"> · from {displayName(info.organizer)}</span>{/if}
        </div>
        <div class="title selectable">{info.summary || fallbackTitle}</div>
        <div class="line">{when}{#if info.recurring}<span class="rep"><Repeat size={12} />Repeats</span>{/if}</div>
        {#if info.location && info.location.trim() !== info.conferenceUrl}<div class="line"><MapPin size={13} /><span class="truncate">{info.location}</span></div>{/if}
        {#if info.attendees.length}
          <div class="line"><Users size={13} /><span class="truncate">{info.attendees.slice(0, 3).map((a) => displayName(a)).join(', ')}{info.attendees.length > 3 ? ` +${info.attendees.length - 3}` : ''}</span></div>
        {/if}
        {#if conflicts}
          {#if conflicts.length}
            <div class="line conflict"><Warning size={13} /><span class="truncate">Conflicts with {conflicts.slice(0, 2).map((c) => `${c.title}${c.allDay ? '' : ` (${time(c.start)})`}`).join(', ')}{conflicts.length > 2 ? ` +${conflicts.length - 2}` : ''}</span></div>
          {:else}
            <div class="line free"><CheckCircle size={13} /><span>You're free at this time</span></div>
          {/if}
        {/if}
      </div>
    </div>
    <div class="actions">
      {#if cancelled}
        {#if info.eventId != null}
          <button class="act" onclick={removeCancelled}>Remove from calendar</button>
        {:else}
          <span class="note">This event is no longer on your calendar.</span>
        {/if}
      {:else}
        <span class="going">Going?</span>
        <button class="act" class:on={info.myStatus === 'accepted'} disabled={!!busy} onclick={() => respond('accepted')}>Yes</button>
        <button class="act" class:on={info.myStatus === 'tentative'} disabled={!!busy} onclick={() => respond('tentative')}>Maybe</button>
        <button class="act" class:on={info.myStatus === 'declined'} disabled={!!busy} onclick={() => respond('declined')}>No</button>
        <span class="grow"></span>
        {#if info.conferenceUrl}
          <button class="act join" onclick={() => api.openUrl(info!.conferenceUrl!)}><Video size={14} />Join</button>
        {/if}
        <button class="act ghost" onclick={show}><CalendarDays size={14} />Calendar</button>
      {/if}
    </div>
  </div>
{:else}
  <div class="invite simple" class:cancelled>
    <div class="icon"><CalendarDays size={18} /></div>
    <div class="text">
      <div class="kind">{cancelled ? 'Event cancelled' : 'Invitation'}</div>
      <div class="title">{fallbackTitle}</div>
    </div>
  </div>
{/if}

<style>
  .invite {
    margin-top: 14px;
    border-radius: 14px;
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1px var(--accent-soft);
    overflow: hidden;
  }
  .invite.cancelled {
    background: var(--red-soft);
    box-shadow: none;
  }
  .top {
    display: flex;
    gap: 14px;
    padding: 14px 16px 12px;
  }
  .date {
    width: 56px;
    flex-shrink: 0;
    align-self: flex-start;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 6px 0 7px;
    border-radius: 12px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-sm);
    transition: transform var(--dur-fast) var(--ease);
  }
  .date:hover {
    transform: translateY(-1px);
  }
  .date .m {
    font-size: 10.5px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--accent-text);
  }
  .cancelled .date .m {
    color: var(--red);
  }
  .date .d {
    font-size: 22px;
    font-weight: 700;
    line-height: 1.1;
  }
  .date .w {
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-3);
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .kind {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent-text);
  }
  .from {
    margin-left: 4px;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
    color: var(--text-3);
  }
  .cancelled .kind {
    color: var(--red);
  }
  .title {
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: 1.3;
  }
  .cancelled .title {
    text-decoration: line-through;
    text-decoration-color: color-mix(in oklab, var(--red) 60%, transparent);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .line :global(svg) {
    flex-shrink: 0;
    color: var(--text-3);
  }
  .rep {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: 6px;
    color: var(--text-3);
  }
  .conflict,
  .conflict :global(svg) {
    color: var(--amber);
  }
  .free,
  .free :global(svg) {
    color: var(--green);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 10px 16px;
    border-top: 1px solid var(--accent-soft);
    background: color-mix(in oklab, var(--bg-elevated) 55%, transparent);
  }
  .cancelled .actions {
    border-top-color: color-mix(in oklab, var(--red) 18%, transparent);
  }
  .going {
    font-weight: 600;
    color: var(--text-2);
    margin-right: 4px;
  }
  .act {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 14px;
    border-radius: 9px;
    font-weight: 600;
    font-size: var(--text-sm);
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: background var(--dur-fast);
  }
  .act:hover {
    background: var(--bg-hover);
  }
  .act.on {
    background: var(--accent);
    color: var(--accent-fg);
    box-shadow: none;
  }
  .act.join {
    background: var(--accent);
    color: var(--accent-fg);
    box-shadow: none;
  }
  .act.ghost {
    background: none;
    box-shadow: none;
    color: var(--text-2);
  }
  .act.ghost:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .act:disabled {
    opacity: 0.6;
  }
  .grow {
    flex: 1;
  }
  .note {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .simple {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
  }
  .icon {
    width: 36px;
    height: 36px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .cancelled .icon {
    background: var(--red);
  }
  .simple .title {
    font-size: var(--text-md);
    font-weight: 600;
  }
</style>

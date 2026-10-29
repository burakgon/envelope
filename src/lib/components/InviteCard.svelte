<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { InviteInfo, MessageView, Occurrence, PartStat } from '../api';
  import { ui } from '../stores/ui.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dateFormat, displayName, timeOf } from '../utils/format';
  import { t } from '../i18n';
  import { CalendarDays, CheckCircle, Clock, MapPin, Repeat, Users, Video, Warning } from '../icons';
  import ProposeTime from './ProposeTime.svelte';

  let { message }: { message: MessageView } = $props();

  let info = $state<InviteInfo | null>(null);
  let failed = $state(false);
  let busy = $state<PartStat | null>(null);
  let conflicts = $state<Occurrence[] | null>(null);
  /** Where the popover proposing another time opens. */
  let proposing = $state<{ x: number; y: number } | null>(null);
  /** The time proposed here. */
  let proposed = $state<{ start: number; end: number } | null>(null);

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
  /** A time span as the card writes it: the day, and the times unless it is all day. */
  function span(start: number, end: number, allDay: boolean): string {
    const day = dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(start);
    if (allDay) return t('invite.whenAllDay', { date: day });
    return t('invite.whenTime', { date: day, start: timeOf(start), end: timeOf(end) });
  }
  const when = $derived(info ? span(info.start, info.end, info.allDay) : '');

  async function respond(status: PartStat) {
    if (!info || busy) return;
    busy = status;
    try {
      await api.calendar.respondInvite(message.id, status);
      info.myStatus = status;
      toasts.show(
        status === 'accepted' ? t('invite.accepted') : status === 'tentative' ? t('invite.repliedMaybe') : t('invite.declined'),
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
      toasts.show(t('invite.removed'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  function propose(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    proposing = proposing ? null : { x: r.left, y: r.bottom + 4 };
  }

  function show() {
    if (!info) return;
    ui.view = 'calendar';
    cal.view = 'week';
    cal.goto(info.start);
  }

  const conflictText = $derived.by(() => {
    if (!conflicts?.length) return '';
    const events = conflicts
      .slice(0, 2)
      .map((c) => (c.allDay ? c.title : t('invite.conflictingEventAt', { title: c.title, time: timeOf(c.start) })))
      .join(', ');
    return conflicts.length > 2 ? t('invite.conflictsWithMore', { events, count: conflicts.length - 2 }) : t('invite.conflictsWith', { events });
  });
</script>

{#if info && !failed}
  <div class="invite" class:cancelled>
    <div class="top">
      <button class="date" onclick={show} title={t('invite.showInCalendar')}>
        <span class="m">{dateFormat({ month: 'short' }).format(date!)}</span>
        <span class="d">{date!.getDate()}</span>
        <span class="w">{dateFormat({ weekday: 'short' }).format(date!)}</span>
      </button>
      <div class="text">
        <div class="kind">
          {cancelled ? t('invite.cancelled') : t('invite.invitation')}{#if info.organizer}<span class="from"> · {t('invite.fromOrganizer', { name: displayName(info.organizer) })}</span>{/if}
        </div>
        <div class="title selectable">{info.summary || fallbackTitle}</div>
        <div class="line">{when}{#if info.recurring}<span class="rep"><Repeat size={12} />{t('invite.repeats')}</span>{/if}</div>
        {#if info.location && info.location.trim() !== info.conferenceUrl}<div class="line"><MapPin size={13} /><span class="truncate">{info.location}</span></div>{/if}
        {#if info.attendees.length}
          <div class="line"><Users size={13} /><span class="truncate">{info.attendees.slice(0, 3).map((a) => displayName(a)).join(', ')}{info.attendees.length > 3 ? ` +${info.attendees.length - 3}` : ''}</span></div>
        {/if}
        {#if conflicts}
          {#if conflicts.length}
            <div class="line conflict"><Warning size={13} /><span class="truncate" title={conflictText}>{conflictText}</span></div>
          {:else}
            <div class="line free"><CheckCircle size={13} /><span>{t('invite.free')}</span></div>
          {/if}
        {/if}
        {#if proposed}
          <div class="line"><Clock size={13} /><span class="truncate">{t('invite.youProposed', { when: span(proposed.start, proposed.end, false) })}</span></div>
        {/if}
      </div>
    </div>
    <div class="actions">
      {#if cancelled}
        {#if info.eventId != null}
          <button class="act" onclick={removeCancelled}>{t('invite.removeFromCalendar')}</button>
        {:else}
          <span class="note">{t('invite.noLongerOnCalendar')}</span>
        {/if}
      {:else}
        <span class="going">{t('invite.going')}</span>
        <button class="act" class:on={info.myStatus === 'accepted'} disabled={!!busy} onclick={() => respond('accepted')}>{t('invite.rsvpYes')}</button>
        <button class="act" class:on={info.myStatus === 'tentative'} disabled={!!busy} onclick={() => respond('tentative')}>{t('invite.rsvpMaybe')}</button>
        <button class="act" class:on={info.myStatus === 'declined'} disabled={!!busy} onclick={() => respond('declined')}>{t('invite.rsvpNo')}</button>
        {#if info.canPropose}
          <button class="act ghost propose" class:open={!!proposing} disabled={!!busy} onclick={propose}><Clock size={14} />{t('invite.propose')}</button>
        {/if}
        <span class="grow"></span>
        {#if info.conferenceUrl}
          <button class="act join" onclick={() => api.openUrl(info!.conferenceUrl!)}><Video size={14} />{t('invite.join')}</button>
        {/if}
        <button class="act ghost" onclick={show}><CalendarDays size={14} />{t('invite.openCalendar')}</button>
      {/if}
    </div>
  </div>
{:else}
  <div class="invite simple" class:cancelled>
    <div class="icon"><CalendarDays size={18} /></div>
    <div class="text">
      <div class="kind">{cancelled ? t('invite.cancelled') : t('invite.invitation')}</div>
      <div class="title">{fallbackTitle}</div>
    </div>
  </div>
{/if}

{#if proposing && info}
  <ProposeTime
    messageId={message.id}
    {info}
    x={proposing.x}
    y={proposing.y}
    onclose={() => (proposing = null)}
    onsent={(status, start, end) => {
      info!.myStatus = status;
      proposed = { start, end };
      proposing = null;
    }}
  />
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
    color: var(--amber-text);
  }
  .free,
  .free :global(svg) {
    color: var(--green);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
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
    background: var(--accent-solid);
    color: var(--accent-fg);
    box-shadow: none;
  }
  .act.join {
    background: var(--accent-solid);
    color: var(--accent-fg);
    box-shadow: none;
  }
  .act.ghost {
    background: none;
    box-shadow: none;
    color: var(--text-2);
  }
  .act.ghost:hover,
  .act.ghost.open {
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
    background: var(--accent-solid);
  }
  .cancelled .icon {
    background: var(--red);
  }
  .simple .title {
    font-size: var(--text-md);
    font-weight: 600;
  }
</style>

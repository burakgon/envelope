<script lang="ts">
  // A new time an attendee proposed for a meeting the user organizes (an iTIP COUNTER): who, the
  // proposed time against the current one, their note and whether the user is free then, with
  // Accept (the event moves and every attendee gets the update) and Decline. A proposal that can't
  // be answered any more says why instead.
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { MessageView, Occurrence, ProposalInfo } from '../api';
  import { ui } from '../stores/ui.svelte';
  import { cal } from '../stores/calendar.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dateFormat, displayName, timeOf } from '../utils/format';
  import { t } from '../i18n';
  import { CalendarDays, CheckCircle, Clock, History, Warning } from '../icons';

  let { message }: { message: MessageView } = $props();

  let info = $state<ProposalInfo | null>(null);
  let failed = $state(false);
  let busy = $state<'accept' | 'decline' | null>(null);
  /** Declined here (nothing on the calendar keeps that). */
  let declined = $state(false);
  let conflicts = $state<Occurrence[] | null>(null);

  async function load() {
    try {
      const p = await api.calendar.proposal(message.id);
      info = p;
      conflicts = null;
      if (p.state === 'open') {
        const list = await api.calendar.occurrences(p.start - 86_400_000, p.end + 86_400_000);
        conflicts = list.filter(
          (o) => o.uid !== p.uid && !o.transparent && o.myStatus !== 'declined' && o.status !== 'cancelled' && !(o.allDay && !p.allDay) && o.start < p.end && o.end > p.start,
        );
      }
    } catch {
      failed = true;
    }
  }

  onMount(load);

  const name = $derived(info ? displayName(info.attendee) : '');
  const date = $derived(info ? new Date(info.start) : null);

  /** A time span as the card writes it: the day, and the times unless it is all day. */
  function span(start: number, end: number, allDay: boolean): string {
    const day = dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(start);
    if (allDay) return t('invite.whenAllDay', { date: day });
    return t('invite.whenTime', { date: day, start: timeOf(start), end: timeOf(end) });
  }

  const note = $derived.by(() => {
    if (!info) return '';
    if (declined) return t('invite.proposalYouDeclined');
    switch (info.state) {
      case 'current':
        return t('invite.proposalCurrent');
      case 'outdated':
        return t('invite.proposalOutdated');
      case 'gone':
        return t('invite.noLongerOnCalendar');
      case 'readOnly':
        return t('invite.proposalReadOnly');
      default:
        return '';
    }
  });

  async function accept() {
    if (!info || busy) return;
    busy = 'accept';
    try {
      await api.calendar.acceptProposal(message.id);
      toasts.show(t('invite.proposalAccepted'), { kind: 'success' });
      await load();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = null;
    }
  }

  async function decline() {
    if (!info || busy) return;
    busy = 'decline';
    try {
      await api.calendar.declineProposal(message.id);
      declined = true;
      toasts.show(t('invite.proposalDeclined', { name }), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = null;
    }
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
  <div class="proposal">
    <div class="top">
      <button class="date" onclick={show} title={t('invite.showInCalendar')}>
        <span class="m">{dateFormat({ month: 'short' }).format(date!)}</span>
        <span class="d">{date!.getDate()}</span>
        <span class="w">{dateFormat({ weekday: 'short' }).format(date!)}</span>
      </button>
      <div class="text">
        <div class="kind">{t('invite.newTimeProposed')}<span class="from"> · {t('invite.proposedBy', { name })}</span></div>
        <div class="title selectable">{info.summary || message.subject}</div>
        <div class="line new"><Clock size={13} /><span class="truncate">{t('invite.proposedTime', { when: span(info.start, info.end, info.allDay) })}</span></div>
        {#if info.currentStart != null && info.currentEnd != null && info.state !== 'current'}
          <div class="line"><History size={13} /><span class="truncate">{t('invite.currentTime', { when: span(info.currentStart, info.currentEnd, info.allDay) })}</span></div>
        {/if}
        {#if info.comment}
          <blockquote class="comment selectable">{info.comment}</blockquote>
        {/if}
        {#if conflicts && !declined}
          {#if conflicts.length}
            <div class="line conflict"><Warning size={13} /><span class="truncate" title={conflictText}>{conflictText}</span></div>
          {:else}
            <div class="line free"><CheckCircle size={13} /><span>{t('invite.free')}</span></div>
          {/if}
        {/if}
      </div>
    </div>
    <div class="actions">
      {#if info.state === 'open' && !declined}
        <button class="act primary" disabled={!!busy} onclick={accept}>{t('invite.acceptProposal')}</button>
        <button class="act" disabled={!!busy} onclick={decline}>{t('invite.declineProposal')}</button>
      {:else}
        <span class="note">{note}</span>
      {/if}
      <span class="grow"></span>
      <button class="act ghost" onclick={show}><CalendarDays size={14} />{t('invite.openCalendar')}</button>
    </div>
  </div>
{/if}

<style>
  .proposal {
    margin-top: 14px;
    border-radius: 14px;
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1px var(--accent-soft);
    overflow: hidden;
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
  .title {
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: 1.3;
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
  .line.new {
    font-weight: 600;
    color: var(--text);
  }
  .line.new :global(svg) {
    color: var(--accent-text);
  }
  .comment {
    margin: 4px 0 2px;
    padding: 2px 0 2px 10px;
    border-left: 2px solid var(--accent-soft);
    font-size: var(--text-sm);
    font-style: italic;
    color: var(--text-2);
    white-space: pre-line;
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
  .act.primary {
    background: var(--accent-solid);
    color: var(--accent-fg);
    box-shadow: none;
  }
  .act.primary:hover {
    background: var(--accent-solid-hover);
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
</style>

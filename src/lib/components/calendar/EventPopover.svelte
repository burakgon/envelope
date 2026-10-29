<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { CalAttendee, EventDetail, Occurrence, PartStat } from '../../api';
  import { cal, span } from '../../stores/calendar.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { t } from '../../i18n';
  import { dateFormat, timeOf } from '../../utils/format';
  import { describeRule } from '../../utils/rrule';
  import Popover from '../Popover.svelte';
  import Avatar from '../Avatar.svelte';
  import { AlignLeft, Calendar, Check, Clock, HelpCircle, MapPin, Pen, Repeat, Trash, Users, Video, X, XCircle } from '../../icons';

  let { occurrence: o, rect, onclose }: { occurrence: Occurrence; rect: DOMRect; onclose: () => void } = $props();

  let detail = $state<EventDetail | null>(null);
  let busy = $state(false);

  onMount(async () => {
    try {
      detail = await api.calendar.detail(o.eventId, o.recurrenceId);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  });

  const pos = $derived.by(() => {
    const w = 360;
    const right = rect.right + 10;
    if (right + w < innerWidth - 8) return { x: right, y: Math.max(8, rect.top - 10), align: 'start' as const };
    return { x: Math.max(8, rect.left - 10), y: Math.max(8, rect.top - 10), align: 'end' as const };
  });

  const when = $derived.by(() => {
    const s = span(o);
    const day = dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(s.start);
    if (o.allDay) {
      const days = Math.round((s.end - s.start) / 86_400_000);
      if (days <= 1) return t('calendar.popover.allDayOn', { date: day });
      const last = dateFormat({ weekday: 'short', month: 'short', day: 'numeric' }).format(s.end - 86_400_000);
      return t('calendar.popover.dateRange', { start: day, end: last });
    }
    return t('calendar.popover.dateTimeRange', { date: day, start: timeOf(o.start), end: timeOf(o.end) });
  });

  async function respond(status: PartStat) {
    busy = true;
    try {
      await api.calendar.respond(o.eventId, status);
      toasts.show(
        status === 'accepted' ? t('calendar.toast.rsvpAccepted') : status === 'declined' ? t('calendar.toast.rsvpDeclined') : t('calendar.toast.rsvpTentative'),
        { kind: 'success' },
      );
      onclose();
      cal.load();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  const statusIcon = { accepted: Check, declined: XCircle, tentative: HelpCircle, needsAction: Clock, delegated: Users };
  // The organizer leads the guest list, even when not listed as an attendee.
  const guests = $derived.by((): CalAttendee[] => {
    if (!detail) return [];
    const org = detail.event.organizer;
    const list = detail.event.attendees.filter((a) => a.email.toLowerCase() !== org?.email.toLowerCase());
    const own = detail.event.attendees.find((a) => a.email.toLowerCase() === org?.email.toLowerCase());
    if (!org || (!list.length && !own)) return list;
    return [own ?? { email: org.email, name: org.name, partstat: 'accepted', role: 'chair', rsvp: false }, ...list];
  });
  const me = $derived(detail?.myEmail?.toLowerCase());
  const canRespond = $derived(!!detail && detail.event.attendees.length > 0 && !detail.organizerIsMe);
</script>

<Popover x={pos.x} y={pos.y} align={pos.align} {onclose} width={360}>
  <div class="pop" style:--c={o.color}>
    <div class="top">
      <span class="sw"></span>
      <h3 class="selectable">{o.title}</h3>
      {#if !o.readOnly}
        <!-- Act before closing: closing clears the `occurrence` prop. -->
        <button class="icon-btn sm" title={t('common.edit')} onclick={() => { cal.edit(o); onclose(); }}><Pen size={14} /></button>
        <button class="icon-btn sm" title={t('common.delete')} onclick={() => { cal.remove(o); onclose(); }}><Trash size={14} /></button>
      {/if}
      <button class="icon-btn sm" title={t('common.close')} onclick={onclose}><X size={15} /></button>
    </div>
    <div class="line"><Clock size={15} /><span>{when}</span></div>
    {#if detail?.event.rrule}
      <div class="line"><Repeat size={15} /><span>{describeRule(detail.event.rrule)}</span></div>
    {/if}
    {#if o.location && o.location.trim() !== o.conferenceUrl}
      <div class="line"><MapPin size={15} />
        {#if /^https?:\/\//.test(o.location)}
          <button class="link" onclick={() => api.openUrl(o.location!)}>{o.location}</button>
        {:else}
          <span class="selectable">{o.location}</span>
        {/if}
      </div>
    {/if}
    {#if o.conferenceUrl}
      <button class="join" onclick={() => api.openUrl(o.conferenceUrl!)}><Video size={16} />{t('calendar.popover.joinMeeting')}</button>
    {/if}
    {#if detail}
      <div class="line"><Calendar size={15} /><span>{detail.calendar.name}</span></div>
      {#if guests.length}
        <div class="people">
          <div class="label"><Users size={14} />{t('calendar.popover.guestCount', { count: guests.length })}</div>
          {#each guests.slice(0, 8) as a (a.email)}
            {@const Icon = statusIcon[a.partstat] ?? Clock}
            <div class="person">
              <Avatar person={{ email: a.email, name: a.name }} size={24} />
              <span class="pname truncate">{a.email.toLowerCase() === me ? t('calendar.popover.guestIsYou', { name: a.name ?? a.email }) : (a.name ?? a.email)}</span>
              {#if detail.event.organizer?.email.toLowerCase() === a.email.toLowerCase()}<span class="org">{t('calendar.popover.organizer')}</span>{/if}
              <span class="st st-{a.partstat}"><Icon size={14} /></span>
            </div>
          {/each}
          {#if guests.length > 8}<div class="more">{t('calendar.popover.moreGuests', { count: guests.length - 8 })}</div>{/if}
        </div>
      {/if}
      {#if detail.event.description}
        <div class="line desc"><AlignLeft size={15} /><span class="selectable">{detail.event.description}</span></div>
      {/if}
    {/if}
    {#if canRespond}
      <div class="rsvp">
        <span>{t('calendar.rsvp.going')}</span>
        <button class:on={o.myStatus === 'accepted'} disabled={busy} onclick={() => respond('accepted')}>{t('calendar.rsvp.yes')}</button>
        <button class:on={o.myStatus === 'tentative'} disabled={busy} onclick={() => respond('tentative')}>{t('calendar.rsvp.maybe')}</button>
        <button class:on={o.myStatus === 'declined'} disabled={busy} onclick={() => respond('declined')}>{t('calendar.rsvp.no')}</button>
      </div>
    {/if}
  </div>
</Popover>

<style>
  .pop {
    padding: 8px 8px 6px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .top {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .sw {
    width: 12px;
    height: 12px;
    border-radius: 4px;
    background: var(--c);
    margin: 5px 4px 0 2px;
    flex-shrink: 0;
  }
  h3 {
    flex: 1;
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: 1.35;
  }
  .line {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    color: var(--text-2);
    line-height: 1.45;
  }
  .line :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
    color: var(--text-3);
  }
  .desc span {
    white-space: pre-wrap;
    max-height: 120px;
    overflow: auto;
    font-size: var(--text-sm);
  }
  .link {
    color: var(--accent-text);
    text-align: left;
    word-break: break-all;
  }
  .join {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 34px;
    border-radius: 10px;
    font-weight: 600;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .join:hover {
    background: var(--accent-hover);
  }
  .people {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 10px;
    background: var(--bg-sunken);
  }
  .label {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 650;
    color: var(--text-3);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: 2px;
  }
  .person {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
  }
  .pname {
    flex: 1;
    min-width: 0;
  }
  .org {
    flex-shrink: 0;
    white-space: nowrap;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-3);
  }
  .st {
    display: grid;
  }
  .st-accepted {
    color: var(--green);
  }
  .st-declined {
    color: var(--red);
  }
  .st-tentative {
    color: var(--amber);
  }
  .st-needsAction {
    color: var(--text-4);
  }
  .more {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .rsvp {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .rsvp span {
    flex: 1;
    min-width: 0;
    font-weight: 600;
    color: var(--text-2);
  }
  .rsvp button {
    flex-shrink: 0;
    white-space: nowrap;
    height: 28px;
    padding: 0 12px;
    border-radius: 8px;
    font-weight: 600;
    font-size: var(--text-sm);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .rsvp button:hover {
    background: var(--bg-hover);
  }
  .rsvp button.on {
    background: var(--accent);
    color: var(--accent-fg);
    box-shadow: none;
  }
</style>

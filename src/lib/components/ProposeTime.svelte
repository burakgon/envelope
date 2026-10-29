<script lang="ts">
  // Proposing another time for an invitation: a day, a start and an end (the invitation's to begin
  // with; the end keeps the length when the start moves) and a note. The proposal goes with the
  // answer "maybe", or "no" when the invitation was declined.
  import { untrack } from 'svelte';
  import { api } from '../api';
  import type { InviteInfo, PartStat } from '../api';
  import { ymd } from '../stores/calendar.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { displayName } from '../utils/format';
  import { fromWallClock, localZone, wallClock } from '../utils/timezone';
  import { t } from '../i18n';
  import Popover from './Popover.svelte';
  import DatePicker from './calendar/DatePicker.svelte';
  import TimePicker from './calendar/TimePicker.svelte';

  let {
    messageId,
    info,
    x,
    y,
    onclose,
    onsent,
  }: {
    messageId: number;
    info: InviteInfo;
    x: number;
    y: number;
    onclose: () => void;
    /** The proposal went out: the answer it carried and the time proposed. */
    onsent: (status: PartStat, start: number, end: number) => void;
  } = $props();

  // The invitation's time to begin with, on this computer's clocks.
  const zone = localZone();
  const initial = untrack(() => ({ start: wallClock(info.start, zone), end: wallClock(info.end, zone), length: info.end - info.start }));
  let day = $state(initial.start.date);
  let startTime = $state(initial.start.time);
  let endTime = $state(initial.end.time);
  let length = initial.length;
  let note = $state('');
  let busy = $state(false);

  const declined = $derived(info.myStatus === 'declined');
  const name = $derived(displayName(info.organizer));
  const start = $derived(fromWallClock(day, startTime, zone));
  /** An end at or before the start is on the next day. */
  const end = $derived.by(() => {
    const sameDay = fromWallClock(day, endTime, zone);
    if (sameDay > start) return sameDay;
    const next = new Date(`${day}T12:00:00`);
    next.setDate(next.getDate() + 1);
    return fromWallClock(ymd(next), endTime, zone);
  });

  /** The end follows the start, keeping the length. */
  function startMoved() {
    endTime = wallClock(start + length, zone).time;
  }

  function endMoved() {
    length = end - start;
  }

  async function send() {
    if (busy) return;
    busy = true;
    try {
      const status = await api.calendar.proposeTime(messageId, declined ? 'declined' : 'tentative', start, end, note.trim() || null);
      toasts.show(t('invite.proposeSent', { name }), { kind: 'success' });
      onsent(status, start, end);
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }
</script>

<Popover {x} {y} width={330} role="dialog" label={t('invite.proposeTitle')} {onclose}>
  <form
    class="propose"
    onsubmit={(e) => {
      e.preventDefault();
      send();
    }}
  >
    <div class="menu-label">{t('invite.proposeTitle')}</div>
    <div class="fields">
      <DatePicker bind:value={day} />
      <div class="times">
        <TimePicker bind:value={startTime} onchange={startMoved} />
        <span class="dash">–</span>
        <TimePicker bind:value={endTime} after={startTime} onchange={endMoved} />
      </div>
      <textarea class="input area" bind:value={note} rows="2" placeholder={t('invite.proposeNote')} aria-label={t('invite.proposeNote')}></textarea>
    </div>
    <p class="hint">{declined ? t('invite.proposeHintNo', { name }) : t('invite.proposeHintMaybe', { name })}</p>
    <div class="buttons">
      <button type="button" class="btn ghost" onclick={onclose}>{t('common.cancel')}</button>
      <button type="submit" class="btn primary" disabled={busy}>{t('invite.proposeSend')}</button>
    </div>
  </form>
</Popover>

<style>
  .propose {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 0 6px 6px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
  }
  .times {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .times :global(.field) {
    width: 104px;
  }
  .dash {
    color: var(--text-3);
  }
  .area {
    height: auto;
    padding: 8px 11px;
    resize: vertical;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
</style>

<script lang="ts">
  import type { MessageView } from '../api';
  import { CalendarDays } from '../icons';

  let { message }: { message: MessageView } = $props();
  const cancelled = $derived(message.calendarMethod === 'CANCEL');
  const title = $derived(message.subject.replace(/^(invitation|updated invitation|einladung|davet):\s*/i, ''));
</script>

<div class="invite" class:cancelled>
  <div class="cal">
    <CalendarDays size={18} />
  </div>
  <div class="text">
    <div class="kind">{cancelled ? 'Event cancelled' : 'Invitation'}</div>
    <div class="title">{title}</div>
  </div>
</div>

<style>
  .invite {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 14px;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1px var(--accent-soft);
  }
  .invite.cancelled {
    background: var(--red-soft);
    box-shadow: none;
  }
  .cal {
    width: 36px;
    height: 36px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .cancelled .cal {
    background: var(--red);
  }
  .kind {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--accent-text);
  }
  .cancelled .kind {
    color: var(--red);
  }
  .title {
    font-weight: 600;
  }
</style>

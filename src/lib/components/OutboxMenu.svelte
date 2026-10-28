<script lang="ts">
  import { api } from '../api';
  import type { OutboxItem } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dateFormat, displayName, timeOf } from '../utils/format';
  import Popover from './Popover.svelte';
  import { AlertCircle, Clock, Pen, Send, Spinner } from '../icons';
  import { t } from '../i18n';

  let { x, y, onclose }: { x: number; y: number; onclose: () => void } = $props();

  const items = $derived([...mail.outbox].filter((i) => i.state !== 'sent').sort((a, b) => a.sendAt - b.sendAt));

  function when(i: OutboxItem): string {
    const d = new Date(i.sendAt);
    const sameDay = d.toDateString() === new Date().toDateString();
    return sameDay ? timeOf(d) : dateFormat({ weekday: 'short', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(d);
  }

  async function edit(i: OutboxItem) {
    try {
      const draftId = await api.cancelSend(i.id);
      onclose();
      if (draftId != null) compose.openDraft(draftId);
      else toasts.show(t('outbox.alreadySent'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function sendNow(i: OutboxItem) {
    try {
      await api.retrySend(i.id);
      toasts.show(i.state === 'failed' ? t('outbox.retrying') : t('outbox.sendingNow'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
</script>

<Popover {x} {y} {onclose} width={360}>
  <div class="menu-label">{t('mailbox.outbox')}</div>
  {#each items as i (i.id)}
    {@const recipients = i.to.map((a) => displayName(a)).join(', ')}
    <div class="item" class:failed={i.state === 'failed'}>
      <span class="ico">
        {#if i.state === 'failed'}<AlertCircle size={16} />{:else if i.state === 'sending'}<span class="spin"><Spinner size={15} /></span>{:else}<Clock size={16} />{/if}
      </span>
      <div class="txt">
        <b class="truncate">{i.subject || t('common.noSubject')}</b>
        <span class="truncate">{recipients ? t('outbox.toRecipients', { recipients }) : t('outbox.toNobody')}</span>
        <span class="state truncate">
          {#if i.state === 'failed'}{i.error != null ? t('outbox.notSent', { error: i.error }) : t('outbox.notSentUnknown')}{:else if i.state === 'sending'}{t('outbox.sending')}{:else}{t('outbox.scheduledFor', { when: when(i) })}{/if}
        </span>
      </div>
      {#if i.state !== 'sending'}
        <div class="acts">
          <button class="icon-btn sm" title={i.state === 'failed' ? t('outbox.tryAgain') : t('outbox.sendNow')} onclick={() => sendNow(i)}><Send size={14} /></button>
          <button class="icon-btn sm" title={t('outbox.editBackToDrafts')} onclick={() => edit(i)}><Pen size={14} /></button>
        </div>
      {/if}
    </div>
  {:else}
    <div class="none">{t('outbox.empty')}</div>
  {/each}
</Popover>

<style>
  .item {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 8px 8px 8px 10px;
    border-radius: 9px;
  }
  .item:hover {
    background: var(--bg-hover);
  }
  .ico {
    display: grid;
    margin-top: 2px;
    color: var(--text-3);
  }
  .failed .ico,
  .failed .state {
    color: var(--red);
  }
  .txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .txt span {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .state {
    color: var(--text-2);
  }
  .acts {
    display: flex;
    gap: 2px;
  }
  .none {
    padding: 10px;
    color: var(--text-3);
    font-size: var(--text-sm);
  }
</style>

<script lang="ts">
  import type { MessageView, OpenedMessage } from '../api';
  import { pgp } from '../stores/pgp.svelte';
  import { t } from '../i18n';
  import { KeyRound, Lock, ShieldAlert } from '../icons';

  // Shown instead of the body of an encrypted message until it is decrypted.
  let { message, opened }: { message: MessageView; opened: OpenedMessage | null } = $props();

  const failed = $derived(pgp.failed[message.id] ?? null);
</script>

<div class="notice" class:warn={!!failed || (!!opened && opened.status !== 'locked')}>
  {#if failed}
    <ShieldAlert size={16} />
    <span class="txt">{failed}</span>
    <button class="btn sm" onclick={() => pgp.open(message.id, true)}>{t('crypto.notice.tryAgain')}</button>
  {:else if !opened}
    <Lock size={16} />
    <span class="txt">{t('crypto.notice.decrypting')}</span>
  {:else if opened.status === 'locked' && opened.lockedKey}
    <KeyRound size={16} />
    <span class="txt">
      {t('crypto.notice.lockedKey', { key: opened.lockedKey.userIds[0] ?? opened.lockedKey.fingerprint })}
      {#if opened.error}<b>{opened.error}.</b>{/if}
    </span>
    <button class="btn sm primary" onclick={() => pgp.unlock(opened.lockedKey!, opened.error)}>{t('crypto.notice.unlock')}</button>
  {:else}
    <ShieldAlert size={16} />
    <span class="txt">{opened.error ?? t('crypto.messageNotDecrypted')}</span>
    {#if opened.status === 'error'}<button class="btn sm" onclick={() => pgp.open(message.id, true)}>{t('crypto.notice.tryAgain')}</button>{/if}
  {/if}
</div>

<style>
  .notice {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--bg-sunken);
    color: var(--text-2);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .notice.warn {
    color: var(--amber);
    background: var(--amber-soft);
  }
  .notice :global(svg) {
    flex-shrink: 0;
  }
  .txt {
    flex: 1;
    min-width: 0;
  }
  .txt b {
    font-weight: 650;
  }
</style>

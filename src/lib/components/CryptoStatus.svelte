<script lang="ts">
  import { api } from '../api';
  import type { KeyInfo, KeySource, MessageView, OpenedMessage, SignatureView } from '../api';
  import { formatFingerprint, isEncrypted, keyName, pgp } from '../stores/pgp.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dialogs } from '../stores/dialogs.svelte';
  import { t, type MessageKey } from '../i18n';
  import { longDate } from '../utils/format';
  import Popover from './Popover.svelte';
  import { BadgeCheck, Lock, ShieldAlert, ShieldCheck, ShieldQuestion, ShieldX } from '../icons';

  let { message, opened }: { message: MessageView; opened: OpenedMessage | null } = $props();

  let details = $state<{ x: number; y: number } | null>(null);
  let busy = $state(false);

  const encrypted = $derived(isEncrypted(message));
  const decrypted = $derived(encrypted && opened?.status === 'ok');
  const sig = $derived<SignatureView | null>(opened?.signatures[0] ?? null);
  const sender = $derived(message.from?.email ?? '');

  type Tone = 'good' | 'warn' | 'bad' | 'plain';
  const signature = $derived.by((): { label: string; tone: Tone } | null => {
    if (!sig) return null;
    const name = sig.signer ? keyName(sig.signer) : '';
    if (sig.status === 'bad') return { label: t('crypto.status.badSignature'), tone: 'bad' };
    if (sig.status === 'unknownKey') return { label: t('crypto.status.signedUnknownKey'), tone: 'plain' };
    if (!sig.senderMatches) return { label: t('crypto.status.signedByNotSender', { name }), tone: 'warn' };
    if (sig.signer?.revoked) return { label: t('crypto.status.signedByKeyRevoked', { name }), tone: 'warn' };
    if (sig.signer?.expired) return { label: t('crypto.status.signedByKeyExpired', { name }), tone: 'warn' };
    if (sig.signer?.verified) return { label: t('crypto.status.signedByVerified', { name }), tone: 'good' };
    return { label: t('crypto.status.signedBy', { name }), tone: 'good' };
  });

  /** Where the signer's key came from (the "Found" row of the details). */
  const sources: Record<KeySource, MessageKey> = {
    own: 'crypto.status.sourceOwn',
    autocrypt: 'crypto.status.sourceAutocrypt',
    gossip: 'crypto.status.sourceGossip',
    wkd: 'crypto.status.sourceWkd',
    keyserver: 'crypto.status.sourceKeyserver',
    manual: 'crypto.status.sourceManual',
  };

  function sourceLabel(source: KeySource): string {
    const key: MessageKey | undefined = sources[source];
    return key ? t(key) : source;
  }

  function open(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    details = { x: r.left, y: r.bottom + 6 };
  }

  async function verify(key: KeyInfo) {
    const ok = await dialogs.confirm({
      title: t('crypto.status.verifyTitle', { name: keyName(key) }),
      message: t('crypto.status.verifyConfirm', { name: keyName(key) }),
      detail: formatFingerprint(key.fingerprint),
      confirmLabel: t('settings.encryption.itMatches'),
    });
    if (!ok) return;
    try {
      await api.crypto.setVerified(key.fingerprint, true);
      pgp.forget();
      await pgp.open(message.id, true);
      toasts.show(t('crypto.keyVerified'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Looks for the signer's key: their domain's key directory and keys.openpgp.org (asked for). */
  async function findKey() {
    if (!sender) return;
    busy = true;
    try {
      const found = await api.crypto.lookup([sender], true, true);
      if (found.length) {
        pgp.forget();
        await pgp.open(message.id, true);
        toasts.show(t('crypto.status.foundKeys', { count: found.length, email: sender }), { kind: 'success' });
      } else toasts.show(t('crypto.noKeyFound', { email: sender }));
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }
</script>

{#if decrypted || signature || (encrypted && opened && opened.status !== 'ok')}
  <div class="status">
    {#if decrypted}
      <button class="pill enc" onclick={open} title={t('crypto.status.encryptionDetails')}><Lock size={13} /><span class="truncate">{t('crypto.status.encrypted')}</span></button>
    {:else if encrypted && opened}
      <button
        class="pill warn"
        onclick={(e) => (opened.status === 'locked' && opened.lockedKey ? pgp.unlock(opened.lockedKey, opened.error) : open(e))}
        title={opened.error ?? ''}
      >
        <Lock size={13} /><span class="truncate">{opened.status === 'locked' ? t('crypto.status.encryptedLocked') : t('crypto.status.encryptedCantDecrypt')}</span>
      </button>
    {/if}
    {#if signature}
      <button class="pill {signature.tone}" onclick={open} title={t('crypto.status.signatureDetails')}>
        {#if signature.tone === 'good'}
          {#if sig?.signer?.verified}<BadgeCheck size={13} />{:else}<ShieldCheck size={13} />{/if}
        {:else if signature.tone === 'bad'}<ShieldX size={13} />
        {:else if signature.tone === 'warn'}<ShieldAlert size={13} />
        {:else}<ShieldQuestion size={13} />{/if}
        <span class="truncate">{signature.label}</span>
      </button>
    {/if}
    {#if opened?.partiallySigned}<span class="note">{t('crypto.status.partlySigned')}</span>{/if}
    {#if opened?.partiallyEncrypted}<span class="note">{t('crypto.status.partlyEncrypted')}</span>{/if}
  </div>
{/if}

{#if details && opened}
  <Popover x={details.x} y={details.y} onclose={() => (details = null)} width={360}>
    <div class="details">
      {#if opened.encrypted}
        <section>
          <h4><Lock size={14} />{opened.status === 'ok' ? t('crypto.status.encrypted') : t('crypto.status.encryptedNotDecrypted')}</h4>
          <p>
            {#if opened.status === 'ok'}{t('crypto.status.encryptedExplain')}
            {:else}{opened.error ?? t('crypto.messageNotDecrypted')}{/if}
          </p>
        </section>
      {/if}
      {#each opened.signatures as s, i (i)}
        <section>
          {#if s.status === 'good'}
            <h4 class:warn={!s.senderMatches}><ShieldCheck size={14} />{t('crypto.status.goodSignature')}</h4>
            <p>
              {s.created ? t('crypto.status.madeByOn', { name: keyName(s.signer), date: longDate(s.created) }) : t('crypto.status.madeBy', { name: keyName(s.signer) })}
              {#if !s.senderMatches}{t('crypto.status.notSendersKey', { email: sender })}{/if}
            </p>
          {:else if s.status === 'bad'}
            <h4 class="bad"><ShieldX size={14} />{t('crypto.status.badSignature')}</h4>
            <p>{t('crypto.status.badSignatureExplain')}</p>
          {:else}
            <h4><ShieldQuestion size={14} />{t('crypto.status.unknownKey')}</h4>
            <p>{s.issuer ? t('crypto.status.unknownKeyExplainIssuer', { fingerprint: formatFingerprint(s.issuer) }) : t('crypto.status.unknownKeyExplain')}</p>
            <button class="btn sm" disabled={busy} onclick={findKey}>{t('crypto.status.lookUpSenderKey')}</button>
          {/if}
          {#if s.signer}
            <div class="key">
              <div class="row"><span>{t('crypto.status.rowKey')}</span><b>{s.signer.userIds[0] ?? s.signer.emails[0]}</b></div>
              <div class="row"><span>{t('crypto.status.rowFingerprint')}</span><code class="selectable">{formatFingerprint(s.signer.fingerprint)}</code></div>
              <div class="row"><span>{t('crypto.status.rowFound')}</span>{sourceLabel(s.signer.source)}</div>
              <div class="row">
                <span>{t('crypto.status.rowVerified')}</span>
                {#if s.signer.verified}<b class="ok"><BadgeCheck size={13} />{t('crypto.status.fingerprintCompared')}</b>
                {:else}<button class="link" onclick={() => verify(s.signer!)}>{t('crypto.status.notVerifiedYet')}</button>{/if}
              </div>
            </div>
          {/if}
        </section>
      {/each}
      {#if opened.partiallySigned}
        <p class="note">{t('crypto.status.partlySignedExplain')}</p>
      {/if}
      {#if opened.partiallyEncrypted}
        <p class="note">{t('crypto.status.partlyEncryptedExplain')}</p>
      {/if}
    </div>
  </Popover>
{/if}

<style>
  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    height: 24px;
    padding: 0 9px;
    border-radius: 999px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--bg-active);
  }
  .pill:hover {
    filter: brightness(0.97);
  }
  .pill.enc,
  .pill.good {
    color: var(--green);
    background: var(--green-soft);
  }
  .pill.warn {
    color: var(--amber-text);
    background: var(--amber-soft);
  }
  .pill.bad {
    color: var(--red);
    background: var(--red-soft);
  }
  .note {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .details {
    display: grid;
    gap: 12px;
    padding: 10px 12px;
  }
  section {
    display: grid;
    gap: 6px;
  }
  section + section {
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }
  h4 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: var(--text-base);
    color: var(--green);
  }
  h4.warn {
    color: var(--amber-text);
  }
  h4.bad {
    color: var(--red);
  }
  p {
    margin: 0;
    color: var(--text-2);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  /* Label and value per row; the labels share a column as wide as the longest one. */
  .key {
    display: grid;
    grid-template-columns: minmax(78px, max-content) minmax(0, 1fr);
    align-items: baseline;
    justify-items: start;
    gap: 4px 8px;
    padding: 8px 10px;
    border-radius: 10px;
    background: var(--bg-sunken);
    font-size: var(--text-sm);
  }
  .row {
    display: contents;
  }
  .row > span {
    color: var(--text-3);
  }
  .row b {
    font-weight: 600;
    word-break: break-all;
  }
  code {
    font-family: var(--font-mono);
    font-size: 11.5px;
    word-break: break-word;
  }
  .ok {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--green);
  }
  /* A sentence: it wraps between words (user ids above break anywhere). */
  .row b.ok {
    word-break: normal;
  }
  .link {
    color: var(--accent-text);
    font-weight: 600;
    font-size: var(--text-sm);
  }
</style>

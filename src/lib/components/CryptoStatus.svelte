<script lang="ts">
  import { api } from '../api';
  import type { KeyInfo, KeySource, MessageView, OpenedMessage, SignatureView, SmimeSignature } from '../api';
  import { formatFingerprint, isEncrypted, keyName, pgp } from '../stores/pgp.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dialogs } from '../stores/dialogs.svelte';
  import { t, type MessageKey } from '../i18n';
  import { dateFormat, longDate } from '../utils/format';
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
    if (sig.smime) return smimeLabel(sig.smime);
    const name = sig.signer ? keyName(sig.signer) : '';
    if (sig.status === 'bad') return { label: t('crypto.status.badSignature'), tone: 'bad' };
    if (sig.status === 'unknownKey') return { label: t('crypto.status.signedUnknownKey'), tone: 'plain' };
    if (!sig.senderMatches) return { label: t('crypto.status.signedByNotSender', { name }), tone: 'warn' };
    if (sig.signer?.revoked) return { label: t('crypto.status.signedByKeyRevoked', { name }), tone: 'warn' };
    if (sig.signer?.expired) return { label: t('crypto.status.signedByKeyExpired', { name }), tone: 'warn' };
    if (sig.signer?.verified) return { label: t('crypto.status.signedByVerified', { name }), tone: 'good' };
    return { label: t('crypto.status.signedBy', { name }), tone: 'good' };
  });

  /** A day, as certificates are valid from and until one. */
  const day = (ms: number) => dateFormat({ year: 'numeric', month: 'long', day: 'numeric' }).format(ms);

  /** A signed message whose signature could not even be read. */
  const unreadable = $derived(!encrypted && opened?.status === 'error' && !sig);

  /** S/MIME: verified only when every check passed, else the first check that failed. */
  function smimeLabel(s: SmimeSignature): { label: string; tone: Tone } {
    const name = s.certificate?.name ?? '';
    switch (s.verdict) {
      case 'verified':
        return { label: t('crypto.status.smimeVerified', { name }), tone: 'good' };
      case 'bad':
        return { label: t('crypto.status.badSignature'), tone: 'bad' };
      case 'noCertificate':
        return { label: t('crypto.status.smimeNoCertificate'), tone: 'plain' };
      case 'unsupported':
        return { label: t('crypto.status.smimeUnsupported'), tone: 'plain' };
      case 'expired':
        return { label: t('crypto.status.smimeExpired', { name }), tone: 'warn' };
      case 'notYetValid':
        return { label: t('crypto.status.smimeNotYetValid', { name }), tone: 'warn' };
      case 'notForEmail':
        return { label: t('crypto.status.smimeNotForEmail', { name }), tone: 'warn' };
      case 'untrusted':
        return { label: t('crypto.status.smimeUntrusted', { name }), tone: 'warn' };
      case 'mismatch':
        return { label: t('crypto.status.smimeMismatch', { name }), tone: 'warn' };
    }
  }

  /** What the S/MIME result means, in a sentence. */
  function smimeExplain(s: SmimeSignature): string {
    const c = s.certificate;
    switch (s.verdict) {
      case 'verified':
        return t('crypto.status.smimeVerifiedExplain', { name: c?.name ?? '' });
      case 'bad':
        return s.reason === 'signature' ? t('crypto.status.smimeSignatureMismatch') : t('crypto.status.smimeContentChanged');
      case 'noCertificate':
        return t('crypto.status.smimeNoCertificateExplain');
      case 'unsupported':
        return t('crypto.status.smimeUnsupportedExplain');
      case 'expired':
        return t('crypto.status.smimeExpiredExplain', { date: day(c?.notAfter ?? 0) });
      case 'notYetValid':
        return t('crypto.status.smimeNotYetValidExplain', { date: day(c?.notBefore ?? 0) });
      case 'notForEmail':
        return t('crypto.status.smimeNotForEmailExplain');
      case 'untrusted':
        if (s.reason === 'issuerExpired') return t('crypto.status.smimeIssuerExpired', { issuer: s.detail ?? c?.issuer ?? '' });
        if (s.reason === 'invalid') return t('crypto.status.smimeChainInvalid');
        return t('crypto.status.smimeUnknownIssuer', { issuer: c?.issuer ?? '' });
      case 'mismatch':
        return t('crypto.status.smimeMismatchExplain', { emails: c?.emails.join(', ') ?? '', email: sender });
    }
  }

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

{#if decrypted || signature || unreadable || (encrypted && opened && opened.status !== 'ok')}
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
          {#if sig?.signer?.verified || sig?.smime?.verdict === 'verified'}<BadgeCheck size={13} />{:else}<ShieldCheck size={13} />{/if}
        {:else if signature.tone === 'bad'}<ShieldX size={13} />
        {:else if signature.tone === 'warn'}<ShieldAlert size={13} />
        {:else}<ShieldQuestion size={13} />{/if}
        <span class="truncate">{signature.label}</span>
      </button>
    {:else if unreadable}
      <button class="pill" onclick={open} title={opened?.error ?? ''}><ShieldQuestion size={13} /><span class="truncate">{t('crypto.status.signatureUnreadable')}</span></button>
    {/if}
    {#if opened?.partiallySigned}<span class="note">{t('crypto.status.partlySigned')}</span>{/if}
    {#if opened?.partiallyEncrypted}<span class="note">{t('crypto.status.partlyEncrypted')}</span>{/if}
  </div>
{/if}

{#if details && opened}
  <Popover x={details.x} y={details.y} onclose={() => (details = null)} width={360} role="dialog" label={opened.encrypted ? t('crypto.status.encryptionDetails') : t('crypto.status.signatureDetails')}>
    <div class="details">
      {#if opened.encrypted}
        <section>
          <h4><Lock size={14} />{opened.status === 'ok' ? t('crypto.status.encrypted') : t('crypto.status.encryptedNotDecrypted')}</h4>
          <p>
            {#if opened.status === 'ok'}{t('crypto.status.encryptedExplain')}
            {:else if opened.status === 'noKey' && opened.kind.startsWith('smime-')}{t('crypto.notice.smimeNoKey')}
            {:else}{opened.error ?? t('crypto.messageNotDecrypted')}{/if}
          </p>
        </section>
      {/if}
      {#if unreadable}
        <section>
          <h4 class="warn"><ShieldQuestion size={14} />{t('crypto.status.signatureUnreadable')}</h4>
          <p class="tech selectable">{opened.error}</p>
        </section>
      {/if}
      {#each opened.signatures as s, i (i)}
        <section>
          {#if s.smime}
            {@const v = s.smime}
            {@const c = v.certificate}
            {@const label = smimeLabel(v)}
            <h4 class:warn={label.tone === 'warn' || label.tone === 'plain'} class:bad={label.tone === 'bad'}>
              {#if v.verdict === 'verified'}<BadgeCheck size={14} />{:else if label.tone === 'bad'}<ShieldX size={14} />{:else if label.tone === 'warn'}<ShieldAlert size={14} />{:else}<ShieldQuestion size={14} />{/if}{label.label}
            </h4>
            <p>{smimeExplain(v)}</p>
            {#if v.detail && (v.verdict === 'unsupported' || v.reason === 'invalid')}<p class="tech selectable">{v.detail}</p>{/if}
            {#if c}
              <div class="key">
                <div class="row"><span>{t('crypto.status.rowCertificate')}</span><b>{c.name}</b></div>
                {#if c.emails.length}<div class="row"><span>{t('crypto.status.rowAddress')}</span><span class="selectable">{c.emails.join(', ')}</span></div>{/if}
                <div class="row"><span>{t('crypto.status.rowIssuer')}</span><span>{v.chain.length ? v.chain.join(' › ') : c.issuer}</span></div>
                <div class="row"><span>{t('crypto.status.rowValid')}</span><span>{t('crypto.status.validFromTo', { from: day(c.notBefore), until: day(c.notAfter) })}</span></div>
                {#if s.created}<div class="row"><span>{t('crypto.status.rowSigned')}</span><span>{longDate(s.created)}</span></div>{/if}
                <div class="row"><span>{t('crypto.status.rowSerial')}</span><code class="selectable">{c.serial}</code></div>
                <div class="row"><span>{t('crypto.status.rowFingerprint')}</span><code class="selectable">{formatFingerprint(c.fingerprint)}</code></div>
                <div class="row"><span>{t('crypto.status.rowAlgorithm')}</span><span>{v.algorithm} · {c.algorithm}</span></div>
              </div>
            {/if}
          {:else if s.status === 'good'}
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
  /* Technical details, in English: the rule a chain breaks, an algorithm. */
  .tech {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--text-3);
    overflow-wrap: anywhere;
  }
</style>

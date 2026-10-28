<script lang="ts">
  import { api } from '../api';
  import type { KeyInfo, MessageView, OpenedMessage, SignatureView } from '../api';
  import { formatFingerprint, isEncrypted, keyName, pgp } from '../stores/pgp.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
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
    const who = sig.signer ? keyName(sig.signer) : '';
    if (sig.status === 'bad') return { label: 'Bad signature', tone: 'bad' };
    if (sig.status === 'unknownKey') return { label: 'Signed — unknown key', tone: 'plain' };
    if (!sig.senderMatches) return { label: `Signed by ${who} — not the sender's key`, tone: 'warn' };
    if (sig.signer?.revoked || sig.signer?.expired) return { label: `Signed by ${who} — key ${sig.signer.revoked ? 'revoked' : 'expired'}`, tone: 'warn' };
    if (sig.signer?.verified) return { label: `Signed by ${who} — verified`, tone: 'good' };
    return { label: `Signed by ${who}`, tone: 'good' };
  });

  const sources: Record<string, string> = {
    own: 'your key',
    autocrypt: 'from their mail (Autocrypt)',
    gossip: 'from a message to several people',
    wkd: "from their domain's key directory",
    keyserver: 'from keys.openpgp.org',
    manual: 'imported by you',
  };

  function open(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    details = { x: r.left, y: r.bottom + 6 };
  }

  async function verify(key: KeyInfo) {
    const ok = confirm(
      `Mark this key of ${keyName(key)} as verified?\n\n${formatFingerprint(key.fingerprint)}\n\nOnly do this after comparing the fingerprint with ${keyName(key)} — in person, on a call, or through another channel you trust.`,
    );
    if (!ok) return;
    try {
      await api.crypto.setVerified(key.fingerprint, true);
      pgp.forget();
      await pgp.open(message.id, true);
      toasts.show('Key verified', { kind: 'success' });
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
        toasts.show(`Found ${found.length === 1 ? 'a key' : `${found.length} keys`} for ${sender}`, { kind: 'success' });
      } else toasts.show(`No key found for ${sender}`);
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
      <button class="pill enc" onclick={open} title="Encryption details"><Lock size={13} />Encrypted</button>
    {:else if encrypted && opened}
      <button
        class="pill warn"
        onclick={(e) => (opened.status === 'locked' && opened.lockedKey ? pgp.unlock(opened.lockedKey, opened.error) : open(e))}
        title={opened.error ?? ''}
      >
        <Lock size={13} />{opened.status === 'locked' ? 'Encrypted — locked' : "Encrypted — can't decrypt"}
      </button>
    {/if}
    {#if signature}
      <button class="pill {signature.tone}" onclick={open} title="Signature details">
        {#if signature.tone === 'good'}
          {#if sig?.signer?.verified}<BadgeCheck size={13} />{:else}<ShieldCheck size={13} />{/if}
        {:else if signature.tone === 'bad'}<ShieldX size={13} />
        {:else if signature.tone === 'warn'}<ShieldAlert size={13} />
        {:else}<ShieldQuestion size={13} />{/if}
        <span class="truncate">{signature.label}</span>
      </button>
    {/if}
    {#if opened?.partiallySigned}<span class="note">Only part of this message is signed</span>{/if}
    {#if opened?.partiallyEncrypted}<span class="note">Only part of this message is encrypted</span>{/if}
  </div>
{/if}

{#if details && opened}
  <Popover x={details.x} y={details.y} onclose={() => (details = null)} width={360}>
    <div class="details">
      {#if opened.encrypted}
        <section>
          <h4><Lock size={14} />{opened.status === 'ok' ? 'Encrypted' : 'Encrypted — not decrypted'}</h4>
          <p>
            {#if opened.status === 'ok'}Only the recipients could read this message on its way to you. It is decrypted on this device.
            {:else}{opened.error ?? 'This message could not be decrypted.'}{/if}
          </p>
        </section>
      {/if}
      {#each opened.signatures as s, i (i)}
        <section>
          {#if s.status === 'good'}
            <h4 class:warn={!s.senderMatches}><ShieldCheck size={14} />Good signature</h4>
            <p>
              Made by {keyName(s.signer)}{s.created ? ` on ${longDate(s.created)}` : ''}.
              {#if !s.senderMatches}The key does not belong to {sender}: someone else may have sent this.{/if}
            </p>
          {:else if s.status === 'bad'}
            <h4 class="bad"><ShieldX size={14} />Bad signature</h4>
            <p>The message was changed after it was signed, or the signature is forged. Don't trust its content.</p>
          {:else}
            <h4><ShieldQuestion size={14} />Unknown key</h4>
            <p>Signed with a key Envelope doesn't have{s.issuer ? ` (${formatFingerprint(s.issuer)})` : ''}, so the signature can't be checked.</p>
            <button class="btn sm" disabled={busy} onclick={findKey}>Look up the sender's key</button>
          {/if}
          {#if s.signer}
            <div class="key">
              <div class="row"><span>Key</span><b>{s.signer.userIds[0] ?? s.signer.emails[0]}</b></div>
              <div class="row"><span>Fingerprint</span><code class="selectable">{formatFingerprint(s.signer.fingerprint)}</code></div>
              <div class="row"><span>Found</span>{sources[s.signer.source] ?? s.signer.source}</div>
              <div class="row">
                <span>Verified</span>
                {#if s.signer.verified}<b class="ok"><BadgeCheck size={13} />You compared the fingerprint</b>
                {:else}<button class="link" onclick={() => verify(s.signer!)}>Not yet — verify…</button>{/if}
              </div>
            </div>
          {/if}
        </section>
      {/each}
      {#if opened.partiallySigned}
        <p class="note">Only part of this message is signed: text added later (for example a mailing list footer) isn't covered.</p>
      {/if}
      {#if opened.partiallyEncrypted}
        <p class="note">This message also has unencrypted parts. Anyone could have added them; only the encrypted part is trustworthy.</p>
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
    color: var(--amber);
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
    color: var(--amber);
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
  .key {
    display: grid;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 10px;
    background: var(--bg-sunken);
    font-size: var(--text-sm);
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .row > span {
    width: 78px;
    flex-shrink: 0;
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
  .link {
    color: var(--accent-text);
    font-weight: 600;
    font-size: var(--text-sm);
  }
</style>

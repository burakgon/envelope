<script lang="ts">
  import { untrack } from 'svelte';
  import { api } from '../api';
  import type { ComposeCrypto } from '../api';
  import type { ComposerState } from '../stores/compose.svelte';
  import { keyName, pgp } from '../stores/pgp.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { t } from '../i18n';
  import Popover from './Popover.svelte';
  import Toggle from './settings/Toggle.svelte';
  import { BadgeCheck, Check, KeyRound, Lock, LockOpen, Search, Warning } from '../icons';

  // The lock in the composer: encryption and signing for this message.
  let { state: c, onchange }: { state: ComposerState; onchange: () => void } = $props();

  let status = $state<ComposeCrypto | null>(null);
  let menu = $state<{ x: number; y: number } | null>(null);
  let busy = $state(false);
  // Until the user flips a switch, Envelope decides (a reply to an encrypted
  // message starts out encrypted and stays so).
  let autoEncrypt = untrack(() => c.data.encrypt == null);
  let autoSign = untrack(() => c.data.sign == null);

  const emails = $derived([...c.data.to, ...c.data.cc, ...c.data.bcc].map((a) => a.email.trim().toLowerCase()).filter((e) => e.includes('@')));
  const key = $derived(`${c.data.accountId}|${c.data.identityId ?? ''}|${c.data.from?.email ?? ''}|${emails.join(',')}`);

  const encryptOn = $derived(!!c.data.encrypt);
  const signOn = $derived(!!c.data.sign);
  const blocked = $derived(encryptOn && !!status && !status.canEncrypt);

  async function refresh(): Promise<ComposeCrypto | null> {
    try {
      const s = await api.crypto.composeStatus(c.data.accountId, c.data.identityId ?? null, c.data.from ?? null, emails);
      status = s;
      decide(s);
      return s;
    } catch {
      return null;
    }
  }

  // Automatic choices are stored as `true` or left unset, so a reopened
  // draft keeps deciding by itself (an encrypted draft stays encrypted).
  function decide(s: ComposeCrypto) {
    let touched = false;
    if (autoEncrypt) {
      const on = s.canEncrypt && s.settings.encryptWhenPossible ? true : null;
      if ((c.data.encrypt ?? null) !== on) {
        c.data.encrypt = on;
        touched = true;
      }
    }
    if (autoSign) {
      const on = s.canSign && (s.settings.signByDefault || !!c.data.encrypt) ? true : null;
      if ((c.data.sign ?? null) !== on) {
        c.data.sign = on;
        touched = true;
      }
    }
    if (touched) onchange();
  }

  let looked = new Set<string>();
  $effect(() => {
    void key;
    const timer = setTimeout(async () => {
      const s = await refresh();
      // Missing keys may be published by the recipients' domains.
      if (!s?.settings.wkdLookup) return;
      const fresh = s.missing.filter((e) => !looked.has(e));
      if (!fresh.length) return;
      fresh.forEach((e) => looked.add(e));
      const found = await api.crypto.lookup(fresh, false, false).catch(() => []);
      if (found.length) refresh();
    }, 300);
    return () => clearTimeout(timer);
  });

  function setEncrypt(on: boolean) {
    autoEncrypt = false;
    c.data.encrypt = on;
    if (autoSign && status) c.data.sign = status.canSign && (status.settings.signByDefault || on) ? true : null;
    onchange();
  }

  function setSign(on: boolean) {
    autoSign = false;
    c.data.sign = on;
    onchange();
  }

  async function createKey() {
    if (!status) return;
    busy = true;
    try {
      const identity = await api.identities().then((l) => l.find((i) => i.email.toLowerCase() === status!.from));
      await api.crypto.generateKey(status.from, identity?.name ?? '', null);
      await refresh();
      toasts.show(t('crypto.keyCreated', { email: status.from }), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  /** Looks the missing keys up in the recipients' key directories, and on keys.openpgp.org when asked. */
  async function findKeys(keyserver: boolean) {
    if (!status?.missing.length) return;
    busy = true;
    try {
      const found = await api.crypto.lookup(status.missing, keyserver, true);
      const s = await refresh();
      if (!found.length) toasts.show(keyserver ? t('crypto.composer.noKeysOnKeyserver') : t('crypto.composer.noKeysFromDomains'));
      else if (s?.missing.length === 0) toasts.show(t('crypto.composer.foundKeysForEveryone'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  /** Called before sending: settles the switches and unlocks the key if needed. */
  export async function prepareSend(): Promise<boolean> {
    const s = status ?? (await refresh());
    if (!s) return !c.data.encrypt && !c.data.sign;
    if (c.data.encrypt && !s.canEncrypt) {
      toasts.error(
        s.ownKey
          ? t('crypto.composer.cantEncryptMissingKeys', { addresses: s.missing.join(', ') })
          : t('crypto.composer.createKeyToEncrypt', { email: s.from }),
      );
      return false;
    }
    if (c.data.sign && !s.ownKey) {
      toasts.error(t('crypto.composer.createKeyToSign', { email: s.from }));
      return false;
    }
    if (c.data.sign && s.ownLocked && s.ownKey) {
      if (!(await pgp.unlock(s.ownKey))) return false;
      await refresh();
    }
    return true;
  }

  function openMenu(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = { x: r.left, y: r.top - 8 };
    refresh();
  }
</script>

<button
  class="icon-btn lock"
  class:on={encryptOn && !blocked}
  class:warn={blocked}
  title={blocked
    ? t('crypto.composer.lockBlocked')
    : encryptOn
      ? t('crypto.composer.lockEncrypted')
      : status?.canEncrypt
        ? t('crypto.composer.lockNotEncrypted')
        : t('crypto.composer.lockEncryption')}
  onclick={openMenu}
>
  {#if encryptOn}<Lock size={17} />{:else}<LockOpen size={17} />{/if}
</button>

{#if menu}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={330}>
    <div class="panel">
      <div class="row">
        <div class="txt">
          <b>{t('crypto.composer.encrypt')}</b>
          <span>
            {#if !status}{t('crypto.composer.checkingKeys')}
            {:else if !status.ownKey}{t('crypto.composer.needOwnKey', { email: status.from })}
            {:else if !emails.length}{t('crypto.composer.addRecipientsFirst')}
            {:else if status.canEncrypt}{t('crypto.composer.onlyRecipientsCanRead')}
            {:else}{t('crypto.composer.notEveryRecipientHasKey')}{/if}
          </span>
        </div>
        <Toggle label={t('crypto.composer.encrypt')} checked={encryptOn} onchange={(v) => setEncrypt(v)} />
      </div>
      <div class="row">
        <div class="txt">
          <b>{t('crypto.composer.sign')}</b>
          <span>{status?.ownKey ? t('crypto.composer.signHint') : t('crypto.composer.signNeedsKey')}</span>
        </div>
        <Toggle label={t('crypto.composer.sign')} checked={signOn} onchange={(v) => setSign(v)} />
      </div>
      {#if status && !status.ownKey}
        {@const label = t('crypto.composer.createKeyFor', { email: status.from })}
        <button class="btn sm" disabled={busy} onclick={createKey} title={label}><KeyRound size={14} /><span class="truncate">{label}</span></button>
      {/if}
      {#if status?.recipients.length}
        <div class="list">
          {#each status.recipients as r (r.email)}
            <div class="who" class:missing={!r.key}>
              {#if r.key}
                {#if r.key.verified}<BadgeCheck size={14} />{:else}<Check size={14} />{/if}
              {:else}<Warning size={14} />{/if}
              <span class="truncate">{r.email}</span>
              <span class="what" title={r.key ? keyName(r.key) : ''}>{r.key ? t(r.key.verified ? 'crypto.composer.recipientVerifiedKey' : 'crypto.composer.recipientHasKey') : t('crypto.composer.recipientNoKey')}</span>
            </div>
          {/each}
        </div>
        {#if status.missing.length}
          <div class="find">
            <button class="btn sm" disabled={busy} onclick={() => findKeys(false)}><Search size={14} />{t('crypto.composer.lookUpKeys')}</button>
            <button class="btn sm" disabled={busy} onclick={() => findKeys(true)} title={t('crypto.composer.searchKeyserverHint')}>{t('crypto.composer.searchKeyserver')}</button>
          </div>
        {/if}
      {/if}
    </div>
  </Popover>
{/if}

<style>
  .lock.on {
    color: var(--green);
  }
  .lock.warn {
    color: var(--amber);
  }
  /* A single column as wide as the panel: long addresses and labels shorten or wrap instead of widening it. */
  .panel {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
    padding: 10px 12px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .txt {
    flex: 1;
    display: grid;
    gap: 1px;
  }
  .txt b {
    font-weight: 600;
  }
  .txt span {
    color: var(--text-3);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .list {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 2px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 26px;
    font-size: var(--text-sm);
    color: var(--green);
  }
  .who span {
    color: var(--text);
  }
  .who.missing {
    color: var(--amber);
  }
  .who .what {
    margin-left: auto;
    flex-shrink: 0;
    color: var(--text-3);
  }
  .find {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
</style>

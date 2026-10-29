<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { CryptoSettings, KeyInfo, KeySource, OwnKey } from '../../api';
  import { formatFingerprint, keyName, pgp } from '../../stores/pgp.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { dialogs } from '../../stores/dialogs.svelte';
  import { t, type MessageKey } from '../../i18n';
  import { longDate } from '../../utils/format';
  import Toggle from './Toggle.svelte';
  import { BadgeCheck, Copy, Download, KeyRound, Lock, Plus, Search, Trash, Upload, Warning } from '../../icons';

  let settings = $state<CryptoSettings | null>(null);
  let own = $state<OwnKey[]>([]);
  let others = $state<KeyInfo[]>([]);
  /** Secret keys no address uses (replaced, or imported for another address). */
  let spare = $state<KeyInfo[]>([]);
  let query = $state('');
  let busy = $state(false);
  /** Import form: for one of the user's addresses (`email`) or for others' keys. */
  let importing = $state<{ email: string | null; text: string; passphrase: string } | null>(null);
  /** Secret key export: the key, and an optional passphrase for the copy. */
  let exporting = $state<{ key: KeyInfo; passphrase: string } | null>(null);
  let creating = $state<{ email: string; name: string; passphrase: string; expert: boolean } | null>(null);
  let verifying = $state<KeyInfo | null>(null);
  let finding = $state('');

  /** Where a key came from, as a badge (keys.openpgp.org is shown by name). */
  const sources: Partial<Record<KeySource, MessageKey>> = {
    own: 'settings.encryption.sourceOwn',
    autocrypt: 'settings.encryption.sourceAutocrypt',
    gossip: 'settings.encryption.sourceGossip',
    wkd: 'settings.encryption.sourceWkd',
    manual: 'settings.encryption.sourceManual',
  };

  function sourceLabel(source: KeySource): string {
    if (source === 'keyserver') return 'keys.openpgp.org';
    const key = sources[source];
    return key ? t(key) : source;
  }

  async function load() {
    try {
      let all: KeyInfo[];
      [settings, own, others, all] = await Promise.all([api.crypto.settings(), api.crypto.ownKeys(), api.crypto.keys(query), api.crypto.keys('')]);
      others = others.filter((k) => !k.secret);
      spare = all.filter((k) => k.secret && !own.some((o) => o.key?.fingerprint === k.fingerprint));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
  onMount(load);

  async function searchKeys() {
    try {
      others = (await api.crypto.keys(query)).filter((k) => !k.secret);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function set<K extends keyof CryptoSettings>(key: K, value: CryptoSettings[K]) {
    if (!settings) return;
    if (key === 'keepDecrypted' && !value) {
      const ok = await dialogs.confirm({
        title: t('settings.encryption.forgetDecryptedTitle'),
        message: t('settings.encryption.forgetDecryptedConfirm'),
        confirmLabel: t('settings.encryption.forgetNow'),
      });
      if (!ok || !settings) return;
    }
    const next = { ...settings, [key]: value };
    try {
      await api.crypto.setSettings(next);
      settings = next;
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function run(task: () => Promise<unknown>, done?: string) {
    busy = true;
    try {
      await task();
      if (done) toasts.show(done, { kind: 'success' });
      pgp.forget();
      await load();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = false;
    }
  }

  function create() {
    if (!creating) return;
    const c = creating;
    run(async () => {
      await api.crypto.generateKey(c.email, c.name, c.passphrase || null);
      creating = null;
    }, t('crypto.keyCreated', { email: c.email }));
  }

  function importedToast(keys: KeyInfo[]) {
    toasts.show(t('settings.encryption.imported', { count: keys.length, name: keyName(keys[0]) }), { kind: 'success' });
  }

  function importKeys() {
    if (!importing) return;
    const i = importing;
    run(async () => {
      const keys = await api.crypto.importText(i.text, i.passphrase || null, i.email);
      importing = null;
      importedToast(keys);
    });
  }

  async function importFile() {
    const i = importing;
    const [path] = await api.pickFiles();
    if (!path) return;
    run(async () => {
      const keys = await api.crypto.importFile(path, i?.passphrase || null, i?.email ?? null);
      importing = null;
      importedToast(keys);
    });
  }

  async function copyPublic(key: KeyInfo) {
    try {
      await navigator.clipboard.writeText(await api.crypto.exportPublic(key.fingerprint));
      toasts.show(t('settings.encryption.publicKeyCopied'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function savePublic(key: KeyInfo) {
    const target = await api.pickSavePath(`${key.emails[0] ?? key.fingerprint.slice(-16)}.asc`);
    if (!target) return;
    run(() => api.crypto.exportFile(key.fingerprint, false, null, target), t('settings.encryption.publicKeySaved'));
  }

  async function saveSecret() {
    if (!exporting) return;
    const x = exporting;
    const target = await api.pickSavePath(`${x.key.emails[0] ?? x.key.fingerprint.slice(-16)}-secret.asc`);
    if (!target) return;
    run(async () => {
      await api.crypto.exportFile(x.key.fingerprint, true, x.passphrase || null, target);
      exporting = null;
    }, t('settings.encryption.secretKeySaved'));
  }

  async function remove(key: KeyInfo) {
    const ok = await dialogs.confirm({
      ...(key.secret
        ? { title: t('settings.encryption.deleteOwnKeyTitle'), message: t('settings.encryption.deleteOwnKeyConfirm') }
        : { title: t('settings.encryption.deleteKeyConfirm', { name: keyName(key) }) }),
      detail: formatFingerprint(key.fingerprint),
      confirmLabel: t('settings.encryption.deleteKey'),
      danger: true,
    });
    if (!ok) return;
    run(() => api.crypto.deleteKey(key.fingerprint), t('settings.encryption.keyDeleted'));
  }

  function verify(key: KeyInfo, verified: boolean) {
    run(async () => {
      await api.crypto.setVerified(key.fingerprint, verified);
      verifying = null;
    }, verified ? t('crypto.keyVerified') : undefined);
  }

  function find() {
    const email = finding.trim();
    if (!email.includes('@')) return;
    run(async () => {
      const found = await api.crypto.lookup([email], true, true);
      if (!found.length) throw { message: t('crypto.noKeyFound', { email }) };
      finding = '';
      toasts.show(t('settings.encryption.keyFound', { email }), { kind: 'success' });
    });
  }

  function lockNow() {
    run(() => api.crypto.lock(), t('settings.encryption.passphrasesForgotten'));
  }
</script>

<h2>{t('settings.tab.encryption')}</h2>
<p class="desc">{t('settings.encryption.desc')}</p>

<h3>{t('settings.encryption.yourKeys')}</h3>
<div class="list">
  {#each own as o (o.email)}
    <div class="item">
      <div class="head">
        <div class="who">
          <b>{o.name || o.email}</b>
          <span>{o.email}</span>
        </div>
        {#if o.key}
          <span class="badge" class:warn={o.key.expired || o.key.revoked}>{o.key.revoked ? t('settings.encryption.revoked') : o.key.expired ? t('settings.encryption.expired') : o.key.algorithm}</span>
          {#if o.key.protected}<span class="badge" title={o.unlocked ? t('settings.encryption.unlockedUntilQuit') : t('settings.encryption.needsPassphrase')}><Lock size={11} />{t('settings.encryption.passphraseBadge')}</span>{/if}
        {:else}
          <span class="badge muted">{t('settings.encryption.noKey')}</span>
        {/if}
      </div>
      {#if o.key}
        <code class="fp selectable">{formatFingerprint(o.key.fingerprint)}</code>
        <div class="meta">
          {o.key.expiresAt
            ? t('settings.encryption.createdExpires', { created: longDate(o.key.createdAt), expires: longDate(o.key.expiresAt) })
            : t('settings.encryption.created', { created: longDate(o.key.createdAt) })}
        </div>
        <div class="btns">
          <button class="btn sm" onclick={() => copyPublic(o.key!)}><Copy size={13} />{t('settings.encryption.copyPublicKey')}</button>
          <button class="btn sm" onclick={() => savePublic(o.key!)}><Download size={13} />{t('settings.encryption.savePublicKey')}</button>
          <button class="btn sm" onclick={() => (exporting = { key: o.key!, passphrase: '' })}><KeyRound size={13} />{t('settings.encryption.exportSecretKey')}</button>
          <button class="btn sm" onclick={() => (importing = { email: o.email, text: '', passphrase: '' })}><Upload size={13} />{t('settings.encryption.replaceKey')}</button>
          <span class="grow"></span>
          <button class="icon-btn" title={t('settings.encryption.deleteKey')} onclick={() => remove(o.key!)}><Trash size={15} /></button>
        </div>
      {:else}
        <div class="btns">
          <button class="btn sm primary" disabled={busy} onclick={() => (creating = { email: o.email, name: o.name, passphrase: '', expert: false })}><Plus size={13} />{t('settings.encryption.createKey')}</button>
          <button class="btn sm" onclick={() => (importing = { email: o.email, text: '', passphrase: '' })}><Upload size={13} />{t('settings.encryption.importExistingKey')}</button>
        </div>
      {/if}
      {#if creating?.email === o.email}
        <div class="form">
          <p class="note">{t('settings.encryption.newKeyNote')}</p>
          {#if creating.expert}
            <label class="label" for="pgp-new-pass">{t('settings.encryption.newKeyPassphrase')}</label>
            <input id="pgp-new-pass" class="input" type="password" bind:value={creating.passphrase} placeholder={t('settings.encryption.newKeyPassphraseHint')} />
          {:else}
            <button class="link" onclick={() => (creating!.expert = true)}>{t('settings.encryption.protectWithPassphrase')}</button>
          {/if}
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (creating = null)}>{t('common.cancel')}</button>
            <button class="btn sm primary" disabled={busy} onclick={create}>{t('settings.encryption.createKey')}</button>
          </div>
        </div>
      {/if}
      {#if importing && importing.email === o.email}
        <div class="form">
          <label class="label" for="pgp-import">{t('settings.encryption.secretKeyField')}</label>
          <textarea id="pgp-import" class="input mono" rows="5" bind:value={importing.text} placeholder="-----BEGIN PGP PRIVATE KEY BLOCK-----"></textarea>
          <label class="label" for="pgp-import-pass">{t('settings.encryption.importPassphrase')}</label>
          <input id="pgp-import-pass" class="input" type="password" bind:value={importing.passphrase} />
          <div class="btns">
            <button class="btn sm" disabled={busy} onclick={importFile}><Upload size={13} />{t('settings.encryption.chooseFile')}</button>
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (importing = null)}>{t('common.cancel')}</button>
            <button class="btn sm primary" disabled={busy || !importing.text.trim()} onclick={importKeys}>{t('settings.encryption.import')}</button>
          </div>
        </div>
      {/if}
      {#if exporting && exporting.key.fingerprint === o.key?.fingerprint}
        <div class="form warnbox">
          <p class="note"><Warning size={14} />{t('settings.encryption.exportWarning')}</p>
          {#if !exporting.key.protected}
            <label class="label" for="pgp-export-pass">{t('settings.encryption.exportPassphrase')}</label>
            <input id="pgp-export-pass" class="input" type="password" bind:value={exporting.passphrase} />
          {/if}
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (exporting = null)}>{t('common.cancel')}</button>
            <button class="btn sm primary" disabled={busy} onclick={saveSecret}>{t('settings.encryption.saveSecretKey')}</button>
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>

{#if spare.length}
  <h4>{t('settings.encryption.otherOwnKeys')}</h4>
  <p class="desc small">{t('settings.encryption.otherOwnKeysDesc')}</p>
  <div class="list">
    {#each spare as k (k.fingerprint)}
      <div class="item">
        <div class="head">
          <div class="who">
            <b>{keyName(k)}</b>
            <span class="truncate">{k.emails.join(', ')}</span>
          </div>
          <span class="badge">{k.algorithm}</span>
        </div>
        <code class="fp selectable">{formatFingerprint(k.fingerprint)}</code>
        <div class="btns">
          <select
            class="input sel"
            value=""
            onchange={(e) => {
              const email = (e.currentTarget as HTMLSelectElement).value;
              if (email) run(() => api.crypto.setOwnKey(email, k.fingerprint), t('settings.encryption.nowSendingFrom', { email }));
            }}
          >
            <option value="">{t('settings.encryption.useFor')}</option>
            {#each own as o (o.email)}<option value={o.email}>{o.email}</option>{/each}
          </select>
          <button class="btn sm" onclick={() => (exporting = { key: k, passphrase: '' })}><KeyRound size={13} />{t('settings.encryption.exportSecretKey')}</button>
          <span class="grow"></span>
          <button class="icon-btn" title={t('settings.encryption.deleteKey')} onclick={() => remove(k)}><Trash size={15} /></button>
        </div>
        {#if exporting?.key.fingerprint === k.fingerprint}
          <div class="form warnbox">
            <p class="note"><Warning size={14} />{t('settings.encryption.exportWarning')}</p>
            {#if !exporting.key.protected}
              <label class="label" for="pgp-export-spare">{t('settings.encryption.exportPassphrase')}</label>
              <input id="pgp-export-spare" class="input" type="password" bind:value={exporting.passphrase} />
            {/if}
            <div class="btns">
              <span class="grow"></span>
              <button class="btn sm" onclick={() => (exporting = null)}>{t('common.cancel')}</button>
              <button class="btn sm primary" disabled={busy} onclick={saveSecret}>{t('settings.encryption.saveSecretKey')}</button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<h3>{t('settings.encryption.preferences')}</h3>
{#if settings}
  <div class="group">
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.encryptWhenPossible')}</b><span>{t('settings.encryption.encryptWhenPossibleDesc')}</span></div>
      <Toggle checked={settings.encryptWhenPossible} onchange={(v) => set('encryptWhenPossible', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.signByDefault')}</b><span>{t('settings.encryption.signByDefaultDesc')}</span></div>
      <Toggle checked={settings.signByDefault} onchange={(v) => set('signByDefault', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.sendAutocrypt')}</b><span>{t('settings.encryption.sendAutocryptDesc')}</span></div>
      <Toggle checked={settings.sendAutocrypt} onchange={(v) => set('sendAutocrypt', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.protectSubject')}</b><span>{t('settings.encryption.protectSubjectDesc')}</span></div>
      <Toggle checked={settings.protectSubject} onchange={(v) => set('protectSubject', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.wkdLookup')}</b><span>{t('settings.encryption.wkdLookupDesc')}</span></div>
      <Toggle checked={settings.wkdLookup} onchange={(v) => set('wkdLookup', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.keepDecrypted')}</b><span>{t('settings.encryption.keepDecryptedDesc')}</span></div>
      <Toggle checked={settings.keepDecrypted} onchange={(v) => set('keepDecrypted', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>{t('settings.encryption.passphrases')}</b><span>{t('settings.encryption.passphrasesDesc')}</span></div>
      <button class="btn sm" disabled={busy} onclick={lockNow}><Lock size={13} />{t('settings.encryption.forgetNow')}</button>
    </div>
  </div>
{/if}

<h3>{t('settings.encryption.othersKeys')}</h3>
<div class="tools">
  <div class="search">
    <Search size={14} />
    <input class="bare" placeholder={t('settings.encryption.searchPlaceholder')} bind:value={query} oninput={searchKeys} />
  </div>
  <button class="btn sm" onclick={() => (importing = { email: null, text: '', passphrase: '' })}><Upload size={13} />{t('settings.encryption.importOthersKeys')}</button>
</div>
{#if importing && importing.email === null}
  <div class="form card">
    <label class="label" for="pgp-import-pub">{t('settings.encryption.publicKeysField')}</label>
    <textarea id="pgp-import-pub" class="input mono" rows="5" bind:value={importing.text} placeholder="-----BEGIN PGP PUBLIC KEY BLOCK-----"></textarea>
    <div class="btns">
      <button class="btn sm" disabled={busy} onclick={importFile}><Upload size={13} />{t('settings.encryption.chooseFile')}</button>
      <span class="grow"></span>
      <button class="btn sm" onclick={() => (importing = null)}>{t('common.cancel')}</button>
      <button class="btn sm primary" disabled={busy || !importing.text.trim()} onclick={importKeys}>{t('settings.encryption.import')}</button>
    </div>
  </div>
{/if}
<div class="find">
  <input class="input" placeholder={t('settings.encryption.findPlaceholder')} bind:value={finding} onkeydown={(e) => e.key === 'Enter' && find()} />
  <button class="btn sm" disabled={busy || !finding.includes('@')} onclick={find} title={t('settings.encryption.findHint')}>{t('settings.encryption.find')}</button>
</div>
<div class="list">
  {#each others as k (k.fingerprint)}
    <div class="item">
      <div class="head">
        <div class="who">
          <b>{keyName(k)}</b>
          <span class="truncate">{k.emails.join(', ')}</span>
        </div>
        {#if k.verified}<span class="badge good"><BadgeCheck size={11} />{t('settings.encryption.verified')}</span>{/if}
        {#if k.revoked || k.expired || !k.canEncrypt}<span class="badge warn">{k.revoked ? t('settings.encryption.revoked') : k.expired ? t('settings.encryption.expired') : t('settings.encryption.cannotEncrypt')}</span>{/if}
        <span class="badge muted">{sourceLabel(k.source)}</span>
      </div>
      <code class="fp selectable">{formatFingerprint(k.fingerprint)}</code>
      <div class="btns">
        {#if k.verified}
          <button class="btn sm" onclick={() => verify(k, false)}>{t('settings.encryption.unmarkVerified')}</button>
        {:else}
          <button class="btn sm" onclick={() => (verifying = k)}><BadgeCheck size={13} />{t('settings.encryption.verify')}</button>
        {/if}
        <button class="btn sm" onclick={() => copyPublic(k)}><Copy size={13} />{t('common.copy')}</button>
        <span class="grow"></span>
        <button class="icon-btn" title={t('settings.encryption.deleteKey')} onclick={() => remove(k)}><Trash size={15} /></button>
      </div>
      {#if verifying?.fingerprint === k.fingerprint}
        <div class="form">
          <p class="note">{t('settings.encryption.verifyNote', { name: keyName(k) })}</p>
          <code class="fp big selectable">{formatFingerprint(k.fingerprint)}</code>
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (verifying = null)}>{t('common.cancel')}</button>
            <button class="btn sm primary" onclick={() => verify(k, true)}>{t('settings.encryption.itMatches')}</button>
          </div>
        </div>
      {/if}
    </div>
  {:else}
    <p class="desc">{query ? t('settings.encryption.noMatchingKeys') : t('settings.encryption.noKeysYet')}</p>
  {/each}
</div>

<style>
  h2 {
    margin: 0 0 8px;
    font-size: var(--text-xl);
    font-weight: 680;
  }
  h3 {
    margin: 22px 0 10px;
    font-size: var(--text-md);
  }
  h4 {
    margin: 16px 0 4px;
    font-size: var(--text-base);
  }
  .desc.small {
    font-size: var(--text-sm);
    margin-bottom: 8px;
  }
  .sel {
    width: auto;
    height: 28px;
    font-size: var(--text-sm);
  }
  .desc {
    color: var(--text-2);
    line-height: 1.55;
    margin: 0 0 16px;
  }
  .group {
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 16px;
  }
  .row + .row {
    border-top: 1px solid var(--border);
  }
  .txt {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .txt b {
    font-weight: 600;
  }
  .txt span {
    color: var(--text-3);
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .list {
    display: grid;
    gap: 8px;
  }
  .item,
  .card {
    display: grid;
    gap: 8px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .card {
    margin-bottom: 8px;
  }
  /* Badges move below the name when they don't fit beside it. */
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .who {
    flex: 1 1 10em;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .who b {
    font-weight: 650;
  }
  .who span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    height: 20px;
    padding: 0 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--bg-active);
  }
  .badge.good {
    color: var(--green);
    background: var(--green-soft);
  }
  .badge.warn {
    color: var(--amber);
    background: var(--amber-soft);
  }
  .badge.muted {
    color: var(--text-3);
  }
  .fp {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-2);
    word-break: break-word;
  }
  .fp.big {
    font-size: 14px;
    letter-spacing: 0.02em;
    color: var(--text);
  }
  .meta {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .btns {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .grow {
    flex: 1;
  }
  .form {
    display: grid;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }
  .warnbox .note {
    color: var(--amber);
  }
  .note {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    margin: 0;
    color: var(--text-2);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .note :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
    height: auto;
    padding: 8px 10px;
    resize: vertical;
  }
  .link {
    justify-self: start;
    color: var(--accent-text);
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .tools {
    display: flex;
    gap: 8px;
    margin-bottom: 8px;
  }
  .search {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 10px;
    border-radius: 9px;
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    color: var(--text-3);
  }
  .bare {
    flex: 1;
    border: none;
    background: none;
    height: 100%;
    color: var(--text);
  }
  .find {
    display: flex;
    gap: 8px;
    margin-bottom: 10px;
  }
  .find .input {
    height: 32px;
  }
</style>

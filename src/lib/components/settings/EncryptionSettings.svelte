<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { CryptoSettings, KeyInfo, OwnKey } from '../../api';
  import { formatFingerprint, keyName, pgp } from '../../stores/pgp.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
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

  const sources: Record<string, string> = {
    own: 'Yours',
    autocrypt: 'From their mail',
    gossip: 'From a group message',
    wkd: 'From their domain',
    keyserver: 'keys.openpgp.org',
    manual: 'Imported',
  };

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
    if (key === 'keepDecrypted' && !value && !confirm('Forget the decrypted copies kept on this device? Encrypted messages stay readable; they are decrypted again when you open them.')) return;
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
    }, `Created a key for ${c.email}`);
  }

  function importKeys() {
    if (!importing) return;
    const i = importing;
    run(async () => {
      const keys = await api.crypto.importText(i.text, i.passphrase || null, i.email);
      importing = null;
      toasts.show(keys.length === 1 ? `Imported ${keyName(keys[0])}` : `Imported ${keys.length} keys`, { kind: 'success' });
    });
  }

  async function importFile() {
    const i = importing;
    const [path] = await api.pickFiles();
    if (!path) return;
    run(async () => {
      const keys = await api.crypto.importFile(path, i?.passphrase || null, i?.email ?? null);
      importing = null;
      toasts.show(keys.length === 1 ? `Imported ${keyName(keys[0])}` : `Imported ${keys.length} keys`, { kind: 'success' });
    });
  }

  async function copyPublic(key: KeyInfo) {
    try {
      await navigator.clipboard.writeText(await api.crypto.exportPublic(key.fingerprint));
      toasts.show('Public key copied');
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function savePublic(key: KeyInfo) {
    const target = await api.pickSavePath(`${key.emails[0] ?? key.fingerprint.slice(-16)}.asc`);
    if (!target) return;
    run(() => api.crypto.exportFile(key.fingerprint, false, null, target), 'Public key saved');
  }

  async function saveSecret() {
    if (!exporting) return;
    const x = exporting;
    const target = await api.pickSavePath(`${x.key.emails[0] ?? x.key.fingerprint.slice(-16)}-secret.asc`);
    if (!target) return;
    run(async () => {
      await api.crypto.exportFile(x.key.fingerprint, true, x.passphrase || null, target);
      exporting = null;
    }, 'Secret key saved');
  }

  function remove(key: KeyInfo) {
    const msg = key.secret
      ? `Delete your key ${formatFingerprint(key.fingerprint)}?\n\nMessages encrypted to it can no longer be read unless you have a copy of the secret key elsewhere. Export it first if unsure.`
      : `Delete the key of ${keyName(key)}?`;
    if (!confirm(msg)) return;
    run(() => api.crypto.deleteKey(key.fingerprint), 'Key deleted');
  }

  function verify(key: KeyInfo, verified: boolean) {
    run(async () => {
      await api.crypto.setVerified(key.fingerprint, verified);
      verifying = null;
    }, verified ? 'Key verified' : undefined);
  }

  function find() {
    const email = finding.trim();
    if (!email.includes('@')) return;
    run(async () => {
      const found = await api.crypto.lookup([email], true, true);
      if (!found.length) throw { message: `No key found for ${email}` };
      finding = '';
      toasts.show(`Found a key for ${email}`, { kind: 'success' });
    });
  }

  function lockNow() {
    run(() => api.crypto.lock(), 'Passphrases forgotten');
  }
</script>

<h2>Encryption</h2>
<p class="desc">
  Envelope encrypts and signs mail with OpenPGP. It sends your public key along with your messages and picks up the keys of people who do the same
  (Autocrypt), so encryption is one click away whenever everyone in a conversation has a key.
</p>

<h3>Your keys</h3>
<div class="list">
  {#each own as o (o.email)}
    <div class="item">
      <div class="head">
        <div class="who">
          <b>{o.name || o.email}</b>
          <span>{o.email}</span>
        </div>
        {#if o.key}
          <span class="badge" class:warn={o.key.expired || o.key.revoked}>{o.key.revoked ? 'Revoked' : o.key.expired ? 'Expired' : o.key.algorithm}</span>
          {#if o.key.protected}<span class="badge" title={o.unlocked ? 'Unlocked until you quit' : 'Needs its passphrase'}><Lock size={11} />Passphrase</span>{/if}
        {:else}
          <span class="badge muted">No key</span>
        {/if}
      </div>
      {#if o.key}
        <code class="fp selectable">{formatFingerprint(o.key.fingerprint)}</code>
        <div class="meta">Created {longDate(o.key.createdAt)}{o.key.expiresAt ? ` · expires ${longDate(o.key.expiresAt)}` : ''}</div>
        <div class="btns">
          <button class="btn sm" onclick={() => copyPublic(o.key!)}><Copy size={13} />Copy public key</button>
          <button class="btn sm" onclick={() => savePublic(o.key!)}><Download size={13} />Save public key</button>
          <button class="btn sm" onclick={() => (exporting = { key: o.key!, passphrase: '' })}><KeyRound size={13} />Export secret key…</button>
          <button class="btn sm" onclick={() => (importing = { email: o.email, text: '', passphrase: '' })}><Upload size={13} />Replace…</button>
          <span class="grow"></span>
          <button class="icon-btn" title="Delete key" onclick={() => remove(o.key!)}><Trash size={15} /></button>
        </div>
      {:else}
        <div class="btns">
          <button class="btn sm primary" disabled={busy} onclick={() => (creating = { email: o.email, name: o.name, passphrase: '', expert: false })}><Plus size={13} />Create key</button>
          <button class="btn sm" onclick={() => (importing = { email: o.email, text: '', passphrase: '' })}><Upload size={13} />Import existing key…</button>
        </div>
      {/if}
      {#if creating?.email === o.email}
        <div class="form">
          <p class="note">A modern key (Ed25519 with a Curve25519 encryption key). It is stored in your system keyring.</p>
          {#if creating.expert}
            <label class="label" for="pgp-new-pass">Passphrase (optional)</label>
            <input id="pgp-new-pass" class="input" type="password" bind:value={creating.passphrase} placeholder="Asked once per session when set" />
          {:else}
            <button class="link" onclick={() => (creating!.expert = true)}>Protect it with a passphrase…</button>
          {/if}
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (creating = null)}>Cancel</button>
            <button class="btn sm primary" disabled={busy} onclick={create}>Create key</button>
          </div>
        </div>
      {/if}
      {#if importing && importing.email === o.email}
        <div class="form">
          <label class="label" for="pgp-import">Secret key (ASCII armored), or choose a file</label>
          <textarea id="pgp-import" class="input mono" rows="5" bind:value={importing.text} placeholder="-----BEGIN PGP PRIVATE KEY BLOCK-----"></textarea>
          <label class="label" for="pgp-import-pass">Passphrase of the key (if it has one)</label>
          <input id="pgp-import-pass" class="input" type="password" bind:value={importing.passphrase} />
          <div class="btns">
            <button class="btn sm" disabled={busy} onclick={importFile}><Upload size={13} />Choose file…</button>
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (importing = null)}>Cancel</button>
            <button class="btn sm primary" disabled={busy || !importing.text.trim()} onclick={importKeys}>Import</button>
          </div>
        </div>
      {/if}
      {#if exporting && exporting.key.fingerprint === o.key?.fingerprint}
        <div class="form warnbox">
          <p class="note"><Warning size={14} />Your secret key lets anyone read your encrypted mail and sign as you. Keep the file somewhere safe and never send it to anyone.</p>
          {#if !exporting.key.protected}
            <label class="label" for="pgp-export-pass">Protect the file with a passphrase (recommended)</label>
            <input id="pgp-export-pass" class="input" type="password" bind:value={exporting.passphrase} />
          {/if}
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (exporting = null)}>Cancel</button>
            <button class="btn sm primary" disabled={busy} onclick={saveSecret}>Save secret key…</button>
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>

{#if spare.length}
  <h4>Other keys of yours</h4>
  <p class="desc small">Not used for sending, but still used to read mail encrypted to them.</p>
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
              if (email) run(() => api.crypto.setOwnKey(email, k.fingerprint), `Now sending from ${email} with this key`);
            }}
          >
            <option value="">Use for…</option>
            {#each own as o (o.email)}<option value={o.email}>{o.email}</option>{/each}
          </select>
          <button class="btn sm" onclick={() => (exporting = { key: k, passphrase: '' })}><KeyRound size={13} />Export secret key…</button>
          <span class="grow"></span>
          <button class="icon-btn" title="Delete key" onclick={() => remove(k)}><Trash size={15} /></button>
        </div>
        {#if exporting?.key.fingerprint === k.fingerprint}
          <div class="form warnbox">
            <p class="note"><Warning size={14} />Your secret key lets anyone read your encrypted mail and sign as you. Keep the file somewhere safe and never send it to anyone.</p>
            {#if !exporting.key.protected}
              <label class="label" for="pgp-export-spare">Protect the file with a passphrase (recommended)</label>
              <input id="pgp-export-spare" class="input" type="password" bind:value={exporting.passphrase} />
            {/if}
            <div class="btns">
              <span class="grow"></span>
              <button class="btn sm" onclick={() => (exporting = null)}>Cancel</button>
              <button class="btn sm primary" disabled={busy} onclick={saveSecret}>Save secret key…</button>
            </div>
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<h3>Preferences</h3>
{#if settings}
  <div class="group">
    <div class="row">
      <div class="txt"><b>Encrypt when possible</b><span>Turn on encryption automatically when every recipient has a key, and ask others to encrypt to you.</span></div>
      <Toggle checked={settings.encryptWhenPossible} onchange={(v) => set('encryptWhenPossible', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Sign messages by default</b><span>Recipients can check that a message is from you. Encrypted messages are signed either way.</span></div>
      <Toggle checked={settings.signByDefault} onchange={(v) => set('signByDefault', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Send my public key with messages</b><span>An Autocrypt header lets other apps encrypt to you. Envelope creates a key for an address when it has none.</span></div>
      <Toggle checked={settings.sendAutocrypt} onchange={(v) => set('sendAutocrypt', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Hide subjects of encrypted messages</b><span>The subject travels inside the encryption; servers and onlookers see “...”.</span></div>
      <Toggle checked={settings.protectSubject} onchange={(v) => set('protectSubject', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Look up keys automatically</b><span>While you write, ask the recipient's own mail domain (Web Key Directory) for a missing key. No other server is contacted.</span></div>
      <Toggle checked={settings.wkdLookup} onchange={(v) => set('wkdLookup', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Remember decrypted messages</b><span>Keep decrypted text on this device so encrypted mail can be searched and shows a preview. Off: it is decrypted each time you open it and only kept in memory.</span></div>
      <Toggle checked={settings.keepDecrypted} onchange={(v) => set('keepDecrypted', v)} />
    </div>
    <div class="row">
      <div class="txt"><b>Passphrases</b><span>Passphrases you enter are remembered until you quit Envelope.</span></div>
      <button class="btn sm" disabled={busy} onclick={lockNow}><Lock size={13} />Forget now</button>
    </div>
  </div>
{/if}

<h3>Other people's keys</h3>
<div class="tools">
  <div class="search">
    <Search size={14} />
    <input class="bare" placeholder="Search by name, address or fingerprint" bind:value={query} oninput={searchKeys} />
  </div>
  <button class="btn sm" onclick={() => (importing = { email: null, text: '', passphrase: '' })}><Upload size={13} />Import…</button>
</div>
{#if importing && importing.email === null}
  <div class="form card">
    <label class="label" for="pgp-import-pub">Public keys (ASCII armored), or choose a file</label>
    <textarea id="pgp-import-pub" class="input mono" rows="5" bind:value={importing.text} placeholder="-----BEGIN PGP PUBLIC KEY BLOCK-----"></textarea>
    <div class="btns">
      <button class="btn sm" disabled={busy} onclick={importFile}><Upload size={13} />Choose file…</button>
      <span class="grow"></span>
      <button class="btn sm" onclick={() => (importing = null)}>Cancel</button>
      <button class="btn sm primary" disabled={busy || !importing.text.trim()} onclick={importKeys}>Import</button>
    </div>
  </div>
{/if}
<div class="find">
  <input class="input" placeholder="Find a key by e-mail address" bind:value={finding} onkeydown={(e) => e.key === 'Enter' && find()} />
  <button class="btn sm" disabled={busy || !finding.includes('@')} onclick={find} title="Asks the address's domain and keys.openpgp.org">Find</button>
</div>
<div class="list">
  {#each others as k (k.fingerprint)}
    <div class="item">
      <div class="head">
        <div class="who">
          <b>{keyName(k)}</b>
          <span class="truncate">{k.emails.join(', ')}</span>
        </div>
        {#if k.verified}<span class="badge good"><BadgeCheck size={11} />Verified</span>{/if}
        {#if k.revoked || k.expired || !k.canEncrypt}<span class="badge warn">{k.revoked ? 'Revoked' : k.expired ? 'Expired' : 'Cannot encrypt'}</span>{/if}
        <span class="badge muted">{sources[k.source] ?? k.source}</span>
      </div>
      <code class="fp selectable">{formatFingerprint(k.fingerprint)}</code>
      <div class="btns">
        {#if k.verified}
          <button class="btn sm" onclick={() => verify(k, false)}>Unmark verified</button>
        {:else}
          <button class="btn sm" onclick={() => (verifying = k)}><BadgeCheck size={13} />Verify…</button>
        {/if}
        <button class="btn sm" onclick={() => copyPublic(k)}><Copy size={13} />Copy</button>
        <span class="grow"></span>
        <button class="icon-btn" title="Delete key" onclick={() => remove(k)}><Trash size={15} /></button>
      </div>
      {#if verifying?.fingerprint === k.fingerprint}
        <div class="form">
          <p class="note">
            Ask {keyName(k)} to read out their fingerprint (in person, on a call, or through a channel you trust) and compare it with this one. If every
            group matches, messages from this key really come from them.
          </p>
          <code class="fp big selectable">{formatFingerprint(k.fingerprint)}</code>
          <div class="btns">
            <span class="grow"></span>
            <button class="btn sm" onclick={() => (verifying = null)}>Cancel</button>
            <button class="btn sm primary" onclick={() => verify(k, true)}>It matches</button>
          </div>
        </div>
      {/if}
    </div>
  {:else}
    <p class="desc">{query ? 'No matching keys.' : 'No keys yet. They arrive with mail from people who use encryption, or you can import or find them.'}</p>
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
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .who {
    flex: 1;
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

<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api } from '../../api';
  import type { Account, AccountConfig, AccountPatch, ExchangeAuth, Security, ServerConfig } from '../../api';
  import { t } from '../../i18n';
  import { Check, Settings, Spinner } from '../../icons';
  import { confirmDiscard } from '../../stores/dialogs.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { modal } from '../../actions/modal';

  let { account, onclose }: { account: Account; onclose: () => void } = $props();

  const blank = (port: number): ServerConfig => ({ host: '', port, security: 'tls', username: '' });
  const servers = (c: AccountConfig) => JSON.stringify([c.imap, c.smtp, c.exchange, c.acceptInvalidCerts]);

  // A copy of the account as the dialog opened, with empty server fields to fill in; the
  // account itself changes once saved.
  const initial: AccountConfig = untrack(() => {
    const c = structuredClone($state.snapshot(account.config));
    c.notify ??= 'all';
    // Unset, the engine keeps a year.
    c.bodySyncDays ??= 365;
    if (account.kind === 'imap') {
      c.imap ??= blank(993);
      c.smtp ??= blank(465);
    }
    return c;
  });
  const openedName = untrack(() => account.name);
  const original = JSON.stringify({ name: openedName, config: initial });
  let name = $state(openedName);
  let config = $state<AccountConfig>(structuredClone(initial));
  let password = $state('');
  let busy = $state<'test' | 'save' | null>(null);
  let failure = $state<string | null>(null);
  /** The settings Envelope last signed in with, as text: a test passed for exactly these. */
  let tested = $state<string | null>(null);

  // Gmail and Microsoft 365 sign in on their own pages: only the name and what to keep apply.
  const withServers = $derived(account.kind === 'imap' || account.kind === 'exchange');
  const changed = $derived(JSON.stringify({ name, config }) !== original || password !== '');
  /** The connection itself changes: Envelope signs in with it before it replaces a working one. */
  const reconnects = $derived(servers(config) !== servers(initial) || password !== '');
  const current = $derived(servers(config) + password);

  let first: HTMLInputElement | undefined = $state();
  onMount(() => first?.focus());

  /** The settings to save or try: an outgoing server left empty is none. */
  function settings(): AccountConfig {
    const c = $state.snapshot(config);
    if (c.smtp && !c.smtp.host.trim()) c.smtp = null;
    return c;
  }

  const keep = [
    { days: 30, label: 'accountSettings.downloadMonth' },
    { days: 90, label: 'accountSettings.downloadThreeMonths' },
    { days: 365, label: 'accountSettings.downloadYear' },
    { days: 0, label: 'accountSettings.downloadAll' },
  ] as const;

  // Switching security moves a standard port to its counterpart; a custom port stays.
  function securityPort(server: ServerConfig, smtp: boolean, sec: Security) {
    if (!smtp && [993, 143].includes(server.port)) server.port = sec === 'tls' ? 993 : 143;
    if (smtp && [465, 587, 25].includes(server.port)) server.port = sec === 'tls' ? 465 : 587;
  }

  async function test(): Promise<boolean> {
    busy = 'test';
    failure = null;
    try {
      await api.verifyAccount(account.id, settings(), password || null);
      tested = current;
      return true;
    } catch (e) {
      failure = errorMessage(e);
      return false;
    } finally {
      busy = null;
    }
  }

  /** Saves; a changed connection is tried first unless `anyway`. */
  async function save(anyway = false) {
    if (busy) return;
    if (withServers && reconnects && !anyway && tested !== current && !(await test())) return;
    busy = 'save';
    try {
      const patch: AccountPatch = { config: settings() };
      if (name.trim() && name !== openedName) patch.name = name.trim();
      if (password) patch.password = password;
      await api.updateAccount(account.id, patch);
      await mail.loadAccounts();
      toasts.show(t('accountSettings.saved'), { kind: 'success' });
      onclose();
    } catch (e) {
      failure = errorMessage(e);
    } finally {
      busy = null;
    }
  }

  async function close() {
    if (changed && !(await confirmDiscard())) return;
    onclose();
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      close();
    }
  }
</script>

{#snippet server(s: ServerConfig, smtp: boolean)}
  <div class="srv">
    <span class="srv-title">{smtp ? t('onboarding.outgoingServer') : t('onboarding.incomingServer')}</span>
    <input class="input" placeholder={smtp ? 'smtp.example.com' : 'imap.example.com'} aria-label={smtp ? t('onboarding.outgoingServer') : t('onboarding.incomingServer')} bind:value={s.host} spellcheck="false" />
    <input class="input port" type="number" aria-label={t('accountSettings.port')} bind:value={s.port} />
    <select class="input sec" aria-label={t('accountSettings.security')} bind:value={s.security} onchange={() => securityPort(s, smtp, s.security)}>
      <option value="tls">SSL/TLS</option><option value="startTls">STARTTLS</option><option value="none">{t('onboarding.securityNone')}</option>
    </select>
    <input class="input" placeholder={t('onboarding.usernamePlaceholder')} aria-label={t('onboarding.usernamePlaceholder')} bind:value={s.username} spellcheck="false" />
  </div>
{/snippet}

<div class="backdrop" role="presentation" onclick={close}>
  <div class="dialog" role="dialog" aria-labelledby="acct-title" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} use:modal>
    <header>
      <span class="icon"><Settings size={18} /></span>
      <div>
        <h3 id="acct-title">{t('accountSettings.title')}</h3>
        <span class="sub">{account.email}</span>
      </div>
    </header>

    <div class="fields">
      <label class="label" for="acct-name">{t('onboarding.yourName')}</label>
      <input id="acct-name" class="input" bind:this={first} bind:value={name} autocomplete="name" />

      <label class="label" for="acct-notify">{t('accountSettings.notify')}</label>
      <select id="acct-notify" class="input" bind:value={config.notify}>
        <option value="all">{t('accountSettings.notifyAll')}</option>
        <option value="people">{t('accountSettings.notifyPeople')}</option>
        <option value="none">{t('accountSettings.notifyNone')}</option>
      </select>
      <span class="hint">{t('accountSettings.notifyDesc')}</span>

      <label class="label" for="acct-keep">{t('accountSettings.download')}</label>
      <select id="acct-keep" class="input" bind:value={config.bodySyncDays}>
        {#each keep as k (k.days)}<option value={k.days}>{t(k.label)}</option>{/each}
      </select>
      <span class="hint">{t('accountSettings.downloadDesc')}</span>

      {#if account.kind === 'imap'}
        <label class="label" for="acct-sent">{t('accountSettings.sentCopy')}</label>
        <select id="acct-sent" class="input" bind:value={config.saveSentCopy}>
          <option value={null}>{t('accountSettings.sentCopyAuto')}</option>
          <option value={true}>{t('accountSettings.sentCopyOn')}</option>
          <option value={false}>{t('accountSettings.sentCopyOff')}</option>
        </select>
        <span class="hint">{t('accountSettings.sentCopyDesc')}</span>
      {/if}
    </div>

    {#if withServers}
      <h4>{t('accountSettings.servers')}</h4>
      <div class="fields">
        {#if account.kind === 'imap' && config.imap && config.smtp}
          {@render server(config.imap, false)}
          {@render server(config.smtp, true)}
          <label class="label" for="acct-caldav">{t('onboarding.caldavLabel')}</label>
          <input id="acct-caldav" class="input" placeholder="https://caldav.example.com" value={config.caldavUrl ?? ''} oninput={(e) => (config.caldavUrl = (e.currentTarget as HTMLInputElement).value.trim() || null)} spellcheck="false" />
          <label class="label" for="acct-carddav">{t('accountSettings.carddav')}</label>
          <input id="acct-carddav" class="input" placeholder="https://carddav.example.com" value={config.carddavUrl ?? ''} oninput={(e) => (config.carddavUrl = (e.currentTarget as HTMLInputElement).value.trim() || null)} spellcheck="false" />
        {:else if account.kind === 'exchange' && config.exchange}
          <label class="label" for="acct-ews">{t('onboarding.serverLabel')}</label>
          <input id="acct-ews" class="input" bind:value={config.exchange.ewsUrl} placeholder="https://mail.company.com/EWS/Exchange.asmx" spellcheck="false" />
          <label class="label" for="acct-user">{t('onboarding.userNameLabel')}</label>
          <input id="acct-user" class="input" bind:value={config.exchange.username} placeholder={t('onboarding.userNamePlaceholder')} autocomplete="username" spellcheck="false" />
          <label class="label" for="acct-auth">{t('accountSettings.signInMethod')}</label>
          <select id="acct-auth" class="input" bind:value={config.exchange.auth}>
            {#each [['auto', t('accountSettings.authAuto')], ['ntlm', 'NTLM'], ['basic', 'Basic']] as [value, text] (value)}
              <option value={value as ExchangeAuth}>{text}</option>
            {/each}
          </select>
        {/if}
        <label class="label" for="acct-pass">{t('accountSettings.newPassword')}</label>
        <input id="acct-pass" class="input" type="password" bind:value={password} autocomplete="new-password" />
        <span class="hint">{t('accountSettings.newPasswordHint')}</span>
        <label class="check"><input type="checkbox" bind:checked={config.acceptInvalidCerts} /> {t('onboarding.acceptSelfSigned')}</label>
      </div>
    {/if}

    {#if failure}
      <p class="failure" role="alert">{failure}</p>
    {:else if tested === current}
      <p class="passed" role="status"><Check size={15} />{t('accountSettings.connected')}</p>
    {/if}

    <footer>
      {#if withServers}
        <button class="btn" disabled={!!busy} onclick={test}>
          {#if busy === 'test'}<span class="spin"><Spinner size={14} /></span>{/if}{t('accountSettings.test')}
        </button>
      {/if}
      <span class="grow"></span>
      <button class="btn" onclick={close}>{t('common.cancel')}</button>
      {#if failure && withServers && reconnects}
        <button class="btn" disabled={!!busy} onclick={() => save(true)}>{t('accountSettings.saveAnyway')}</button>
      {/if}
      <button class="btn primary" disabled={!!busy || !changed} onclick={() => save()}>
        {#if busy === 'save'}<span class="spin"><Spinner size={14} /></span>{/if}{t('common.save')}
      </button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1400;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    width: min(540px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow: auto;
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 12px;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 10px;
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  h3 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  h4 {
    margin: 20px 0 4px;
    font-size: var(--text-md);
    font-weight: 650;
  }
  .sub,
  .hint {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label {
    margin-top: 8px;
  }
  .srv {
    display: grid;
    grid-template-columns: 1fr 84px 120px;
    gap: 6px;
    margin-top: 8px;
  }
  .srv-title {
    grid-column: 1 / -1;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .srv input:last-child {
    grid-column: 1 / -1;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
  }
  .failure,
  .passed {
    margin: 16px 0 0;
    padding: 10px 12px;
    border-radius: 10px;
    line-height: 1.5;
  }
  .failure {
    color: var(--red);
    background: var(--red-soft);
  }
  .passed {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--green);
    background: var(--green-soft);
  }
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 18px;
  }
  .grow {
    flex: 1;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>

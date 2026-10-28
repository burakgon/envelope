<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api } from '../api';
  import type { AccountConfig, AppInfo, Discovery, OAuthProvider, Security, ServerConfig } from '../api';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage } from '../stores/toasts.svelte';
  import { t, tParts, type MessageKey } from '../i18n';
  import Logo from './Logo.svelte';
  import WindowControls from './WindowControls.svelte';
  import { ArrowLeft, ArrowRight, Check, ChevronDown, ExternalLink, Lock, Moon, Spinner, Sun, X, ShieldCheck, Sparkles, Zap } from '../icons';

  let { first = false }: { first?: boolean } = $props();

  type Step = 'welcome' | 'providers' | 'oauth' | 'password' | 'exchange' | 'done';
  let step = $state<Step>(untrack(() => (first ? 'welcome' : 'providers')));
  let info = $state<AppInfo | null>(null);
  let email = $state('');
  let name = $state('');
  let password = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let discovery = $state<Discovery | null>(null);
  let provider = $state('generic');
  let oauthProvider = $state<OAuthProvider>('google');
  let advanced = $state(false);
  let imap = $state<ServerConfig>({ host: '', port: 993, security: 'tls', username: '' });
  let smtp = $state<ServerConfig>({ host: '', port: 465, security: 'tls', username: '' });
  let caldav = $state('');
  let insecure = $state(false);
  let ewsUrl = $state('');
  let ewsUser = $state('');
  let added = $state<string | null>(null);
  /** The password step was reached through "Use an app password" for Google (its hint is ours, not discovery's). */
  let googleAppPassword = $state(false);

  onMount(async () => {
    info = await api.info().catch(() => null);
  });

  const providers = $derived([
    { id: 'gmail', name: 'Google', sub: 'Gmail & Workspace', mark: 'G', bg: 'linear-gradient(135deg,#4285f4,#34a853 55%,#fbbc05 80%,#ea4335)' },
    { id: 'outlook', name: 'Microsoft', sub: 'Outlook & Microsoft 365', mark: 'M', bg: 'linear-gradient(135deg,#0078d4,#28a8ea)' },
    { id: 'exchange', name: 'Exchange', sub: t('onboarding.providerExchangeSub'), mark: 'E', bg: 'linear-gradient(135deg,#123a73,#1c9bd6)' },
    { id: 'icloud', name: 'iCloud', sub: 'iCloud Mail', mark: '☁', bg: 'linear-gradient(135deg,#5ac8fa,#007aff)' },
    { id: 'yahoo', name: 'Yahoo', sub: 'Yahoo & AOL', mark: 'Y!', bg: 'linear-gradient(135deg,#7e1fff,#6001d2)' },
    { id: 'fastmail', name: 'Fastmail', sub: 'Fastmail', mark: 'F', bg: 'linear-gradient(135deg,#2268c5,#0b4ea2)' },
    { id: 'generic', name: t('onboarding.providerOther'), sub: 'IMAP / SMTP', mark: '@', bg: 'linear-gradient(135deg,#6b7280,#374151)' },
  ]);

  function close() {
    if (mail.accounts.length === 0 && !api.isDemo) return;
    ui.close();
  }

  function pick(id: string) {
    error = null;
    provider = id;
    if (id === 'gmail' || id === 'outlook') {
      oauthProvider = id === 'gmail' ? 'google' : 'microsoft';
      step = 'oauth';
    } else if (id === 'exchange') {
      step = 'exchange';
    } else {
      step = 'password';
      discovery = null;
      googleAppPassword = false;
    }
  }

  async function detect() {
    if (!email.includes('@')) return;
    busy = true;
    error = null;
    try {
      const d = await api.discover(email.trim());
      discovery = d;
      googleAppPassword = false;
      if (d.kind === 'gmail' && provider !== 'generic') return pick('gmail');
      if (d.kind === 'microsoft') return pick('outlook');
      if (d.kind === 'exchange') return pick('exchange');
      provider = d.provider;
      if (d.imap) imap = { ...d.imap };
      if (d.smtp) smtp = { ...d.smtp };
      caldav = d.caldavUrl ?? '';
      step = 'password';
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function startOAuth() {
    busy = true;
    error = null;
    try {
      const account = await api.addOAuthAccount(oauthProvider, email.trim() || null);
      added = account.email;
      await mail.loadAccounts();
      step = 'done';
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function useAppPassword() {
    provider = provider === 'outlook' ? 'outlook' : 'gmail';
    imap = provider === 'gmail' ? { host: 'imap.gmail.com', port: 993, security: 'tls', username: '' } : { host: 'outlook.office365.com', port: 993, security: 'tls', username: '' };
    smtp = provider === 'gmail' ? { host: 'smtp.gmail.com', port: 465, security: 'tls', username: '' } : { host: 'smtp.office365.com', port: 587, security: 'startTls', username: '' };
    discovery = { email, provider, providerName: provider === 'gmail' ? 'Gmail' : 'Outlook', kind: 'imap', auth: 'password', imap, smtp, caldavUrl: null, carddavUrl: null, passwordHint: null, passwordHelpUrl: provider === 'gmail' ? 'https://myaccount.google.com/apppasswords' : null };
    googleAppPassword = provider === 'gmail';
    step = 'password';
  }

  async function submitPassword() {
    if (!email.includes('@') || !password) {
      error = t('onboarding.errorMissingCredentials');
      return;
    }
    busy = true;
    error = null;
    try {
      if (!imap.host) {
        const d = await api.discover(email.trim());
        discovery = d;
        googleAppPassword = false;
        if (d.imap) imap = { ...d.imap };
        if (d.smtp) smtp = { ...d.smtp };
        caldav = caldav || d.caldavUrl || '';
      }
      const config: AccountConfig = {
        auth: 'password',
        imap: { ...imap },
        smtp: smtp.host ? { ...smtp } : null,
        caldavUrl: caldav.trim() || null,
        carddavUrl: discovery?.carddavUrl ?? null,
        acceptInvalidCerts: insecure,
        saveSentCopy: null,
        bodySyncDays: null,
      };
      const account = await api.addPasswordAccount({ email: email.trim(), name: name.trim(), provider: discovery?.provider ?? provider, password, config });
      added = account.email;
      await mail.loadAccounts();
      step = 'done';
    } catch (e) {
      error = errorMessage(e);
      advanced = advanced || /connect|host|server|timed out|network|tls/i.test(error);
    } finally {
      busy = false;
    }
  }

  async function submitExchange() {
    if (!email.includes('@') || !password) {
      error = t('onboarding.errorMissingCredentials');
      return;
    }
    busy = true;
    error = null;
    try {
      const account = await api.addExchangeAccount({
        email: email.trim(),
        name: name.trim(),
        password,
        ewsUrl: ewsUrl.trim() || null,
        username: ewsUser.trim() || null,
        acceptInvalidCerts: insecure,
      });
      added = account.email;
      await mail.loadAccounts();
      step = 'done';
    } catch (e) {
      error = errorMessage(e);
      // Server addresses, user names and certificates are set under Advanced.
      const kind = e && typeof e === 'object' && 'kind' in e ? (e as { kind: unknown }).kind : null;
      advanced = advanced || kind === 'notFound' || /advanced|server|address|certificate|connect|reach|found/i.test(error);
    } finally {
      busy = false;
    }
  }

  // Switching security moves a standard port to its counterpart; a custom port stays.
  function securityPort(kind: 'imap' | 'smtp', sec: Security) {
    if (kind === 'imap') {
      if ([993, 143].includes(imap.port)) imap.port = sec === 'tls' ? 993 : 143;
    } else if ([465, 587, 25].includes(smtp.port)) {
      smtp.port = sec === 'tls' ? 465 : 587;
    }
  }

  // Where to get an app password, for providers that need one when discovery brings no hint.
  const providerHints: Record<string, MessageKey> = {
    icloud: 'onboarding.hintIcloud',
    yahoo: 'onboarding.hintYahoo',
    fastmail: 'onboarding.hintFastmail',
  };
  const hint = $derived.by(() => {
    if (discovery?.passwordHint != null) return discovery.passwordHint;
    if (googleAppPassword) return t('onboarding.hintGoogleAppPassword');
    const key: MessageKey | undefined = providerHints[provider];
    return key ? t(key) : null;
  });
  const providerName = $derived(providers.find((p) => p.id === provider)?.name ?? discovery?.providerName ?? 'IMAP');
  const oauthConfigured = $derived(oauthProvider === 'google' ? info?.googleConfigured : info?.microsoftConfigured);
</script>

<div class="screen" class:first>
  <div class="drag" data-tauri-drag-region>
    <span class="grow" data-tauri-drag-region></span>
    {#if !first || mail.accounts.length}
      <button class="icon-btn" aria-label={t('common.close')} onclick={close}><X size={18} /></button>
    {/if}
    <WindowControls />
  </div>

  <div class="card">
    {#if step === 'welcome'}
      <div class="welcome">
        <div class="logo-wrap"><Logo size={84} /></div>
        <h1>{t('onboarding.welcomeTitle')}</h1>
        <p class="lead">{t('onboarding.welcomeLead')}</p>
        <div class="features">
          <div><Zap size={18} /><span><b>{t('onboarding.featureInstant')}</b>{t('onboarding.featureInstantDesc')}</span></div>
          <div><ShieldCheck size={18} /><span><b>{t('onboarding.featurePrivate')}</b>{t('onboarding.featurePrivateDesc')}</span></div>
          <div><Sparkles size={18} /><span><b>{t('onboarding.featureSmart')}</b>{t('onboarding.featureSmartDesc')}</span></div>
        </div>
        <button class="btn primary lg" onclick={() => (step = 'providers')}>{t('onboarding.getStarted')} <ArrowRight size={16} /></button>
      </div>
    {:else if step === 'providers'}
      <h2>{t('onboarding.addAccountTitle')}</h2>
      <p class="sub">{t('onboarding.addAccountSub')}</p>
      <form class="detect" onsubmit={(e) => { e.preventDefault(); detect(); }}>
        <input class="input lg" type="email" placeholder={t('onboarding.emailPlaceholder')} bind:value={email} autocomplete="email" spellcheck="false" />
        <button class="btn primary" type="submit" disabled={busy || !email.includes('@')}>
          {#if busy}<span class="spin"><Spinner size={15} /></span>{:else}{t('common.continue')}{/if}
        </button>
      </form>
      <div class="grid">
        {#each providers as p (p.id)}
          <button class="prov" onclick={() => pick(p.id)}>
            <span class="mark" style:background={p.bg}>{p.mark}</span>
            <span class="pname">{p.name}</span>
            <span class="psub">{p.sub}</span>
          </button>
        {/each}
      </div>
      {#if error}<p class="error">{error}</p>{/if}
    {:else if step === 'oauth'}
      <button class="back" onclick={() => (step = 'providers')}><ArrowLeft size={15} /> {t('common.back')}</button>
      <h2>{t('onboarding.oauthTitle', { provider: oauthProvider === 'google' ? 'Google' : 'Microsoft' })}</h2>
      <p class="sub">{t('onboarding.oauthIntro')}</p>
      <label class="label" for="ob-email">{t('onboarding.oauthEmailLabel')}</label>
      <input id="ob-email" class="input" type="email" placeholder={oauthProvider === 'google' ? t('onboarding.oauthEmailPlaceholderGoogle') : t('onboarding.oauthEmailPlaceholderMicrosoft')} bind:value={email} />
      {#if oauthConfigured === false}
        <div class="note">
          <Lock size={15} />
          <span>
            {#each tParts(oauthProvider === 'google' ? 'onboarding.oauthNoClientGoogle' : 'onboarding.oauthNoClientMicrosoft') as part, i (i)}{#if typeof part === 'string'}{part}{:else}<button class="inline-link" onclick={() => ui.open('settings', 'advanced')}>{t('settings.title')} → {t('settings.tab.advanced')}</button>{/if}{/each}
          </span>
        </div>
      {/if}
      <div class="actions">
        <button class="btn primary lg" disabled={busy} onclick={startOAuth}>
          {#if busy}<span class="spin"><Spinner size={16} /></span> {t('onboarding.oauthWaiting')}{:else}{t('onboarding.oauthContinue')} <ExternalLink size={15} />{/if}
        </button>
        {#if oauthProvider === 'google'}
          <button class="btn ghost" onclick={useAppPassword}>{t('onboarding.useAppPassword')}</button>
        {/if}
      </div>
      {#if error}<p class="error">{error}</p>{/if}
    {:else if step === 'password'}
      <button class="back" onclick={() => (step = 'providers')}><ArrowLeft size={15} /> {t('common.back')}</button>
      <h2>{provider === 'generic' ? t('onboarding.passwordTitleGeneric') : t('onboarding.passwordTitle', { provider: providerName })}</h2>
      {#if hint}
        <div class="note">
          <Lock size={15} />
          <span>{hint}
            {#if discovery?.passwordHelpUrl}<button class="inline-link" onclick={() => api.openUrl(discovery!.passwordHelpUrl!)}>{t('common.open')}</button>{/if}
          </span>
        </div>
      {/if}
      <form class="form" onsubmit={(e) => { e.preventDefault(); submitPassword(); }}>
        <div>
          <label class="label" for="pw-name">{t('onboarding.yourName')}</label>
          <input id="pw-name" class="input" bind:value={name} placeholder={t('onboarding.namePlaceholder')} autocomplete="name" />
        </div>
        <div>
          <label class="label" for="pw-email">{t('onboarding.emailLabel')}</label>
          <input id="pw-email" class="input" type="email" bind:value={email} placeholder={t('onboarding.emailPlaceholder')} onblur={() => !imap.host && email.includes('@') && detect()} spellcheck="false" />
        </div>
        <div>
          <label class="label" for="pw-pass">{provider === 'generic' ? t('onboarding.passwordLabel') : t('onboarding.appPasswordLabel')}</label>
          <input id="pw-pass" class="input" type="password" bind:value={password} autocomplete="current-password" />
        </div>

        <button type="button" class="adv-toggle" onclick={() => (advanced = !advanced)}>
          <span class="chev" class:open={advanced}><ChevronDown size={14} /></span> {t('onboarding.serverSettings')}
        </button>
        {#if advanced}
          <div class="adv">
            <div class="srv">
              <span class="srv-title">{t('onboarding.incomingServer')}</span>
              <input class="input" placeholder="imap.example.com" bind:value={imap.host} />
              <input class="input port" type="number" bind:value={imap.port} />
              <select class="input sec" bind:value={imap.security} onchange={() => securityPort('imap', imap.security)}>
                <option value="tls">SSL/TLS</option><option value="startTls">STARTTLS</option><option value="none">{t('onboarding.securityNone')}</option>
              </select>
              <input class="input" placeholder={t('onboarding.usernamePlaceholder')} bind:value={imap.username} />
            </div>
            <div class="srv">
              <span class="srv-title">{t('onboarding.outgoingServer')}</span>
              <input class="input" placeholder="smtp.example.com" bind:value={smtp.host} />
              <input class="input port" type="number" bind:value={smtp.port} />
              <select class="input sec" bind:value={smtp.security} onchange={() => securityPort('smtp', smtp.security)}>
                <option value="tls">SSL/TLS</option><option value="startTls">STARTTLS</option><option value="none">{t('onboarding.securityNone')}</option>
              </select>
              <input class="input" placeholder={t('onboarding.usernamePlaceholder')} bind:value={smtp.username} />
            </div>
            <div>
              <label class="label" for="pw-caldav">{t('onboarding.caldavLabel')}</label>
              <input id="pw-caldav" class="input" bind:value={caldav} placeholder="https://caldav.example.com" />
            </div>
            <label class="check"><input type="checkbox" bind:checked={insecure} /> {t('onboarding.acceptSelfSigned')}</label>
          </div>
        {/if}

        {#if error}<p class="error">{error}</p>{/if}
        <div class="actions">
          <button class="btn primary lg" type="submit" disabled={busy}>
            {#if busy}<span class="spin"><Spinner size={16} /></span> {t('onboarding.connecting')}{:else}{t('onboarding.addAccount')}{/if}
          </button>
        </div>
      </form>
    {:else if step === 'exchange'}
      <button class="back" onclick={() => (step = 'providers')}><ArrowLeft size={15} /> {t('common.back')}</button>
      <h2>{t('onboarding.exchangeTitle')}</h2>
      <div class="note">
        <Lock size={15} />
        <span>{t('onboarding.exchangeNote')}</span>
      </div>
      <form class="form" onsubmit={(e) => { e.preventDefault(); submitExchange(); }}>
        <div>
          <label class="label" for="ex-name">{t('onboarding.yourName')}</label>
          <input id="ex-name" class="input" bind:value={name} placeholder={t('onboarding.namePlaceholder')} autocomplete="name" />
        </div>
        <div>
          <label class="label" for="ex-email">{t('onboarding.workEmailLabel')}</label>
          <input id="ex-email" class="input" type="email" bind:value={email} placeholder={t('onboarding.workEmailPlaceholder')} autocomplete="email" spellcheck="false" />
        </div>
        <div>
          <label class="label" for="ex-pass">{t('onboarding.passwordLabel')}</label>
          <input id="ex-pass" class="input" type="password" bind:value={password} autocomplete="current-password" />
        </div>

        <button type="button" class="adv-toggle" onclick={() => (advanced = !advanced)}>
          <span class="chev" class:open={advanced}><ChevronDown size={14} /></span> {t('onboarding.advanced')}
        </button>
        {#if advanced}
          <div class="adv">
            <div>
              <label class="label" for="ex-url">{t('onboarding.serverLabel')}</label>
              <input id="ex-url" class="input" bind:value={ewsUrl} placeholder="mail.company.com" spellcheck="false" />
              <span class="field-hint">{t('onboarding.serverHint')}</span>
            </div>
            <div>
              <label class="label" for="ex-user">{t('onboarding.userNameLabel')}</label>
              <input id="ex-user" class="input" bind:value={ewsUser} placeholder={t('onboarding.userNamePlaceholder')} autocomplete="username" spellcheck="false" />
            </div>
            <label class="check"><input type="checkbox" bind:checked={insecure} /> {t('onboarding.acceptSelfSigned')}</label>
          </div>
        {/if}

        {#if error}<p class="error">{error}</p>{/if}
        <div class="actions">
          <button class="btn primary lg" type="submit" disabled={busy}>
            {#if busy}<span class="spin"><Spinner size={16} /></span> {ewsUrl.trim() ? t('onboarding.signingIn') : t('onboarding.findingServer')}{:else}{t('onboarding.addAccount')}{/if}
          </button>
        </div>
      </form>
    {:else}
      <div class="welcome">
        <div class="success"><Check size={34} strokeWidth={3} /></div>
        <h1>{t('onboarding.doneTitle')}</h1>
        <p class="lead">{t('onboarding.doneLead', { email: added ?? '' })}</p>
        <div class="themes">
          <button class:on={ui.settings.theme === 'light'} onclick={() => ui.set('theme', 'light')}><Sun size={16} /> {t('onboarding.themeLight')}</button>
          <button class:on={ui.settings.theme === 'dark'} onclick={() => ui.set('theme', 'dark')}><Moon size={16} /> {t('onboarding.themeDark')}</button>
          <button class:on={ui.settings.theme === 'system'} onclick={() => ui.set('theme', 'system')}>{t('onboarding.themeAuto')}</button>
        </div>
        <div class="actions center">
          <button class="btn ghost" onclick={() => { step = 'providers'; email = ''; password = ''; error = null; }}>{t('onboarding.addAnotherAccount')}</button>
          <button class="btn primary lg" onclick={() => ui.close()}>{t('onboarding.openEnvelope')} <ArrowRight size={16} /></button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .screen {
    position: fixed;
    inset: 0;
    z-index: 1200;
    display: grid;
    place-items: center;
    background: color-mix(in oklab, var(--overlay) 100%, transparent);
    animation: fade-in var(--dur);
    overflow: auto;
  }
  .screen.first {
    background:
      radial-gradient(1200px 600px at 15% -10%, oklch(0.75 0.14 var(--accent-h) / 0.35), transparent 60%),
      radial-gradient(900px 500px at 110% 110%, oklch(0.7 0.16 calc(var(--accent-h) + 60) / 0.25), transparent 60%),
      var(--bg-app);
  }
  .drag {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 52px;
    display: flex;
    align-items: center;
    padding: 0 12px;
    gap: 4px;
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .card {
    width: min(560px, 92vw);
    margin: 70px 0 40px;
    padding: 34px 38px 34px;
    border-radius: 22px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur-slow) var(--ease-out);
  }
  .welcome {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
  }
  .logo-wrap {
    filter: drop-shadow(0 14px 30px oklch(0.5 0.22 var(--accent-h) / 0.35));
    margin-bottom: 18px;
    animation: pop-in 600ms var(--spring);
  }
  h1 {
    margin: 0 0 8px;
    font-size: var(--text-2xl);
    font-weight: 720;
    letter-spacing: -0.025em;
  }
  h2 {
    margin: 0 0 6px;
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.02em;
  }
  .lead {
    margin: 0 0 22px;
    color: var(--text-2);
    font-size: var(--text-md);
    max-width: 400px;
    line-height: 1.55;
  }
  .sub {
    margin: 0 0 18px;
    color: var(--text-2);
    line-height: 1.55;
  }
  .features {
    display: grid;
    gap: 12px;
    text-align: left;
    margin: 4px 0 26px;
    width: 100%;
  }
  .features div {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    color: var(--accent-text);
  }
  .features span {
    color: var(--text-2);
    line-height: 1.45;
  }
  .features b {
    display: block;
    color: var(--text);
    font-weight: 650;
  }
  .detect {
    display: flex;
    gap: 8px;
    margin-bottom: 18px;
  }
  .detect .input {
    height: 40px;
    font-size: var(--text-md);
  }
  .detect .btn {
    height: 40px;
    padding: 0 18px;
    border-radius: 10px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 10px;
  }
  .prov {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 18px 8px 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    transition: transform var(--dur) var(--ease-out), box-shadow var(--dur-fast), background var(--dur-fast);
    /* Translated names and descriptions wrap inside the tile instead of overflowing it. */
    text-align: center;
    overflow-wrap: anywhere;
  }
  .prov:hover {
    transform: translateY(-2px);
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border-strong), var(--shadow-md);
  }
  /* A tile alone in the last row sits in the middle. */
  .prov:last-child:nth-child(3n + 1) {
    grid-column: 2;
  }
  .mark {
    width: 42px;
    height: 42px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    color: #fff;
    font-weight: 800;
    font-size: 18px;
    margin-bottom: 4px;
    box-shadow: 0 4px 12px rgb(0 0 0 / 0.15), inset 0 1px 0 rgb(255 255 255 / 0.25);
  }
  .pname {
    font-weight: 650;
  }
  .psub {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-3);
    font-weight: 500;
    margin-bottom: 14px;
  }
  .back:hover {
    color: var(--text);
  }
  .note {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 12px 14px;
    margin: 12px 0 16px;
    border-radius: 12px;
    background: var(--accent-softer);
    color: var(--text-2);
    line-height: 1.5;
  }
  .note :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
    color: var(--accent-text);
  }
  .inline-link {
    color: var(--accent-text);
    font-weight: 600;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .form {
    display: grid;
    gap: 14px;
  }
  .adv-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-2);
    font-weight: 550;
    justify-self: start;
  }
  .chev {
    display: grid;
    transform: rotate(-90deg);
    transition: transform var(--dur);
  }
  .chev.open {
    transform: none;
  }
  .adv {
    display: grid;
    gap: 14px;
    padding: 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    animation: fade-in var(--dur);
  }
  .srv {
    display: grid;
    grid-template-columns: 1fr 80px 120px;
    gap: 8px;
  }
  .srv .srv-title {
    grid-column: 1 / -1;
    font-weight: 600;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .srv input:last-child {
    grid-column: 1 / -1;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-2);
  }
  .field-hint {
    display: block;
    margin-top: 6px;
    font-size: 11.5px;
    color: var(--text-3);
    line-height: 1.45;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    margin-top: 18px;
  }
  .actions.center {
    justify-content: center;
  }
  .error {
    margin: 12px 0 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--red-soft);
    color: var(--red);
    line-height: 1.45;
    user-select: text;
  }
  .spin {
    display: grid;
    animation: spin 0.9s linear infinite;
  }
  .success {
    width: 72px;
    height: 72px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    margin-bottom: 18px;
    color: #fff;
    background: linear-gradient(135deg, var(--green), oklch(0.7 0.15 170));
    box-shadow: 0 12px 30px rgb(47 158 107 / 0.35);
    animation: pop-in 500ms var(--spring);
  }
  .themes {
    display: flex;
    gap: 6px;
    padding: 4px;
    border-radius: 12px;
    background: var(--bg-sunken);
  }
  .themes button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 14px;
    border-radius: 9px;
    font-weight: 550;
    color: var(--text-2);
  }
  .themes button.on {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
</style>

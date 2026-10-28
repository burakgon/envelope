<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { Account, AppInfo, Identity, OAuthClient } from '../../api';
  import { mail } from '../../stores/mail.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import AiSettings from './AiSettings.svelte';
  import EncryptionSettings from './EncryptionSettings.svelte';
  import AutoReplyDialog from './AutoReplyDialog.svelte';
  import { autoReplies } from '../../stores/autoReplies.svelte';
  import RulesSettings from './RulesSettings.svelte';
  import Toggle from './Toggle.svelte';
  import Logo from '../Logo.svelte';
  import {
    Away, Bell, Calendar, FileText, Filter, Info, Keyboard, Mail, Palette, Pen, Settings as Gear, ShieldCheck, Sparkles, Users, X, Plus, Trash, Moon, Sun, Lock, ExternalLink, Code,
  } from '../../icons';
  import { cal } from '../../stores/calendar.svelte';
  import type { Template } from '../../stores/ui.svelte';
  import { htmlToText } from '../../utils/html';
  import Editor from '../Editor.svelte';

  let tpl = $state<Template | null>(null);

  function saveTemplate() {
    if (!tpl || !tpl.name.trim()) return;
    const t = { ...tpl, id: tpl.id || crypto.randomUUID(), name: tpl.name.trim() };
    const list = ui.settings.templates;
    ui.set('templates', list.some((x) => x.id === t.id) ? list.map((x) => (x.id === t.id ? t : x)) : [...list, t]);
    tpl = null;
  }

  function deleteTemplate() {
    if (!tpl) return;
    const id = tpl.id;
    ui.set('templates', ui.settings.templates.filter((x) => x.id !== id));
    tpl = null;
  }

  const tabs = [
    { id: 'general', label: 'General', icon: Gear },
    { id: 'appearance', label: 'Appearance', icon: Palette },
    { id: 'accounts', label: 'Accounts', icon: Users },
    { id: 'signatures', label: 'Signatures', icon: Pen },
    { id: 'templates', label: 'Templates', icon: FileText },
    { id: 'rules', label: 'Rules', icon: Filter },
    { id: 'calendar', label: 'Calendar', icon: Calendar },
    { id: 'notifications', label: 'Notifications', icon: Bell },
    { id: 'privacy', label: 'Privacy', icon: ShieldCheck },
    { id: 'encryption', label: 'Encryption', icon: Lock },
    { id: 'ai', label: 'AI', icon: Sparkles },
    { id: 'shortcuts', label: 'Shortcuts', icon: Keyboard },
    { id: 'advanced', label: 'Advanced', icon: Code },
    { id: 'about', label: 'About', icon: Info },
  ];

  let info = $state<AppInfo | null>(null);
  let google = $state<OAuthClient>({ clientId: '', clientSecret: '' });
  let microsoft = $state<OAuthClient>({ clientId: '', tenant: '' });
  let editing = $state<Identity | null>(null);

  let autostart = $state(false);
  let autoReplyFor = $state<Account | null>(null);

  onMount(async () => {
    if (!cal.calendars.length) cal.loadCalendars();
    api.autostart.get().then((v) => (autostart = v)).catch(() => {});
    info = await api.info().catch(() => null);
    google = { clientSecret: '', ...(await api.oauthClient('google').catch(() => ({ clientId: '' }))) };
    microsoft = { tenant: '', ...(await api.oauthClient('microsoft').catch(() => ({ clientId: '' }))) };
  });

  const hues = [280, 250, 220, 195, 160, 140, 45, 25, 350, 320];

  async function saveAccount(a: Account, patch: Parameters<typeof api.updateAccount>[1]) {
    try {
      await api.updateAccount(a.id, patch);
      await mail.loadAccounts();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Provider (and, for Exchange, the server) shown under an account. */
  function accountKindLabel(a: Account): string {
    if (a.kind === 'exchange') {
      const url = a.config.exchange?.ewsUrl ?? '';
      let host = '';
      try {
        host = url ? new URL(url).host : '';
      } catch {
        host = url;
      }
      return host ? `Exchange · ${host}` : 'Exchange';
    }
    return a.provider === 'generic' ? 'IMAP' : a.provider;
  }

  async function removeAccount(a: Account) {
    if (!confirm(`Remove ${a.email}? Local copies of its mail will be deleted. Nothing is deleted on the server.`)) return;
    try {
      await api.removeAccount(a.id);
      await mail.loadAccounts();
      await mail.loadFolders();
      mail.refresh();
      toasts.show(`Removed ${a.email}`);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function reauth(a: Account) {
    try {
      if (a.config.auth === 'password') {
        const pw = prompt(`New password for ${a.email}`);
        if (!pw) return;
        await api.updateAccount(a.id, { password: pw });
      } else {
        await api.addOAuthAccount(a.config.auth === 'oAuthGoogle' ? 'google' : 'microsoft', a.email);
      }
      toasts.show('Signed in again', { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function saveIdentity() {
    if (!editing) return;
    try {
      await api.saveIdentity($state.snapshot(editing) as Identity);
      await mail.loadAccounts();
      editing = null;
      toasts.show('Saved', { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function saveOAuth() {
    try {
      await api.setOAuthClient('google', $state.snapshot(google));
      await api.setOAuthClient('microsoft', $state.snapshot(microsoft));
      toasts.show('Sign-in clients saved', { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  const shortcutList = [
    ['C', 'Compose'], ['R / A / F', 'Reply / Reply all / Forward'], ['E', 'Archive'], ['#', 'Delete'], ['S', 'Star'], ['U', 'Read / unread'],
    ['H', 'Snooze'], ['V', 'Move'], ['L', 'Label'], ['J / K', 'Next / previous'], ['/', 'Search'], ['Ctrl K', 'Command palette'], ['Z', 'Undo'], ['G I', 'Go to inbox'],
  ];
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="window" role="dialog" aria-label="Settings" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && ui.close()}>
    <nav>
      <div class="nav-title">Settings</div>
      {#each tabs as t (t.id)}
        <button class="tab" class:on={ui.settingsTab === t.id} onclick={() => (ui.settingsTab = t.id)}>
          <t.icon size={16} />{t.label}
        </button>
      {/each}
    </nav>
    <section class="content">
      <button class="icon-btn close" aria-label="Close settings" onclick={() => ui.close()}><X size={18} /></button>

      {#if ui.settingsTab === 'general'}
        <h2>General</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>Split inbox</b><span>Separate people from newsletters and notifications.</span></div>
            <Toggle checked={ui.settings.splitInbox} onchange={(v) => { ui.set('splitInbox', v); mail.refresh(); }} />
          </div>
          <div class="row">
            <div class="txt"><b>Mark as read</b><span>When to mark a conversation read after opening it.</span></div>
            <select class="input sel" value={String(ui.settings.markReadDelayMs)} onchange={(e) => ui.set('markReadDelayMs', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="0">Immediately</option>
              <option value="600">After a moment</option>
              <option value="3000">After 3 seconds</option>
              <option value="-1">Never automatically</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Undo send</b><span>How long you can cancel a message after sending.</span></div>
            <select class="input sel" value={String(ui.settings.undoSendSeconds)} onchange={(e) => ui.set('undoSendSeconds', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="0">Off</option>
              <option value="5">5 seconds</option>
              <option value="10">10 seconds</option>
              <option value="20">20 seconds</option>
              <option value="30">30 seconds</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Default reply</b><span>What R and the reply button do.</span></div>
            <select class="input sel" value={ui.settings.defaultReply} onchange={(e) => ui.set('defaultReply', (e.currentTarget as HTMLSelectElement).value as 'reply' | 'replyAll')}>
              <option value="reply">Reply</option>
              <option value="replyAll">Reply all</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Send and archive</b><span>Archive the conversation when you reply.</span></div>
            <Toggle checked={ui.settings.sendAndArchive} onchange={(v) => ui.set('sendAndArchive', v)} />
          </div>
        </div>
        <div class="group">
          {#if info?.trayAvailable}
            <div class="row">
              <div class="txt"><b>Keep running in the background</b><span>Closing the window leaves Envelope in the tray, so new mail and reminders still arrive.</span></div>
              <Toggle checked={ui.settings.closeToTray} onchange={(v) => ui.set('closeToTray', v)} />
            </div>
          {/if}
          <div class="row">
            <div class="txt"><b>Start when you log in</b><span>{info?.trayAvailable ? 'Envelope starts quietly in the tray.' : 'Envelope opens when your desktop session starts.'}</span></div>
            <Toggle
              checked={autostart}
              onchange={async (v) => {
                try {
                  await api.autostart.set(v);
                  autostart = v;
                  if (v && info?.trayAvailable && !ui.settings.closeToTray) ui.set('closeToTray', true);
                } catch (e) {
                  toasts.error(errorMessage(e));
                }
              }}
            />
          </div>
        </div>
      {:else if ui.settingsTab === 'rules'}
        <RulesSettings />
      {:else if ui.settingsTab === 'templates'}
        <h2>Templates</h2>
        <p class="desc">Messages you write once and insert from the composer with a click — for answers you send often.</p>
        <div class="accounts">
          {#each ui.settings.templates as t (t.id)}
            <div class="acct">
              <div class="acct-info">
                <b>{t.name}</b>
                <span class="acct-sub truncate">{t.subject || htmlToText(t.html).slice(0, 90)}</span>
              </div>
              <button class="btn sm" onclick={() => (tpl = { ...t })}>Edit</button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => (tpl = { id: '', name: '', subject: '', html: '' })}><Plus size={15} />New template</button>
        {#if tpl}
          <div class="editor-card">
            <div class="two">
              <div><label class="label" for="tpl-name">Name</label><input id="tpl-name" class="input" bind:value={tpl.name} placeholder="e.g. Meeting follow-up" /></div>
              <div><label class="label" for="tpl-subject">Subject (optional)</label><input id="tpl-subject" class="input" bind:value={tpl.subject} /></div>
            </div>
            <div>
              <span class="label">Message</span>
              {#key tpl.id}
                <div class="rich"><Editor html={tpl.html} onupdate={(h) => (tpl!.html = h)} showToolbar placeholder="Write the template…" /></div>
              {/key}
            </div>
            <div class="btns">
              {#if tpl.id}<button class="btn danger" onclick={deleteTemplate}>Delete</button>{/if}
              <span class="grow"></span>
              <button class="btn" onclick={() => (tpl = null)}>Cancel</button>
              <button class="btn primary" disabled={!tpl.name.trim()} onclick={saveTemplate}>Save</button>
            </div>
          </div>
        {/if}
      {:else if ui.settingsTab === 'calendar'}
        <h2>Calendar</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>Week starts on</b></div>
            <select class="input sel" value={String(ui.settings.weekStart)} onchange={(e) => { ui.set('weekStart', Number((e.currentTarget as HTMLSelectElement).value)); cal.load(); }}>
              <option value="1">Monday</option>
              <option value="0">Sunday</option>
              <option value="6">Saturday</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Default calendar</b><span>Where new events go.</span></div>
            <select
              class="input sel"
              value={String(cal.defaultCalendar()?.id ?? '')}
              onchange={(e) => ui.set('defaultCalendarId', Number((e.currentTarget as HTMLSelectElement).value) || null)}
            >
              {#each cal.writable as c (c.id)}<option value={String(c.id)}>{c.name}</option>{:else}<option value="">No calendars</option>{/each}
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Default event length</b></div>
            <select class="input sel" value={String(ui.settings.defaultEventMinutes)} onchange={(e) => ui.set('defaultEventMinutes', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="15">15 minutes</option>
              <option value="30">30 minutes</option>
              <option value="45">45 minutes</option>
              <option value="60">1 hour</option>
              <option value="90">1.5 hours</option>
              <option value="120">2 hours</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Default reminder</b><span>Added to new events.</span></div>
            <select class="input sel" value={String(ui.settings.defaultAlarm)} onchange={(e) => ui.set('defaultAlarm', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="-1">None</option>
              <option value="0">At start</option>
              <option value="5">5 minutes before</option>
              <option value="10">10 minutes before</option>
              <option value="15">15 minutes before</option>
              <option value="30">30 minutes before</option>
              <option value="60">1 hour before</option>
              <option value="1440">1 day before</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Show declined events</b></div>
            <Toggle checked={ui.settings.showDeclined} onchange={(v) => { ui.set('showDeclined', v); cal.load(); }} />
          </div>
          <div class="row">
            <div class="txt"><b>Event reminders</b><span>Notify before calendar events start.</span></div>
            <Toggle checked={ui.settings.calendarNotifications} onchange={(v) => ui.set('calendarNotifications', v)} />
          </div>
        </div>
      {:else if ui.settingsTab === 'appearance'}
        <h2>Appearance</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>Theme</b></div>
            <div class="seg">
              <button class:on={ui.settings.theme === 'light'} onclick={() => ui.set('theme', 'light')}><Sun size={15} />Light</button>
              <button class:on={ui.settings.theme === 'dark'} onclick={() => ui.set('theme', 'dark')}><Moon size={15} />Dark</button>
              <button class:on={ui.settings.theme === 'system'} onclick={() => ui.set('theme', 'system')}>System</button>
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>Accent color</b></div>
            <div class="swatches">
              {#each hues as h (h)}
                <button class="sw" class:on={ui.settings.accentHue === h} style:background="oklch(0.6 0.2 {h})" aria-label="Accent {h}" onclick={() => ui.set('accentHue', h)}></button>
              {/each}
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>Density</b><span>Compact shows more conversations at once.</span></div>
            <div class="seg">
              <button class:on={ui.settings.density === 'comfortable'} onclick={() => ui.set('density', 'comfortable')}>Comfortable</button>
              <button class:on={ui.settings.density === 'compact'} onclick={() => ui.set('density', 'compact')}>Compact</button>
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>Show avatars</b></div>
            <Toggle checked={ui.settings.showAvatars} onchange={(v) => ui.set('showAvatars', v)} />
          </div>
          <div class="row">
            <div class="txt"><b>Dark mode for designed emails</b><span>Invert newsletters and receipts in dark mode instead of showing them on white.</span></div>
            <Toggle checked={ui.settings.darkEmails} onchange={(v) => ui.set('darkEmails', v)} />
          </div>
          {#if !api.isDemo}
            <div class="row">
              <div class="txt"><b>Use system title bar</b><span>Show your desktop's window decorations instead of Envelope's.</span></div>
              <Toggle checked={ui.settings.nativeTitlebar} onchange={(v) => ui.set('nativeTitlebar', v)} />
            </div>
          {/if}
        </div>
      {:else if ui.settingsTab === 'accounts'}
        <h2>Accounts</h2>
        <div class="accounts">
          {#each mail.accounts as a (a.id)}
            <div class="acct">
              <input type="color" class="color" value={a.color} onchange={(e) => saveAccount(a, { color: (e.currentTarget as HTMLInputElement).value })} title="Account color" />
              <div class="acct-info">
                <input class="name-input" value={a.label ?? ''} placeholder={a.email} onchange={(e) => saveAccount(a, { label: (e.currentTarget as HTMLInputElement).value })} />
                <span class="acct-sub" title={a.config.exchange?.ewsUrl ?? undefined}>{a.email} · {accountKindLabel(a)} {a.sync.state === 'authError' ? '· sign-in needed' : ''}{autoReplies.on[a.id] ? ' · automatic replies on' : ''}</span>
              </div>
              <Toggle checked={a.enabled} onchange={(v) => saveAccount(a, { enabled: v })} />
              <button class="icon-btn" class:active={autoReplies.on[a.id]} title="Automatic replies" aria-label="Automatic replies" onclick={() => (autoReplyFor = a)}><Away size={16} /></button>
              <button class="btn sm" onclick={() => reauth(a)}><Lock size={13} />Sign in again</button>
              <button class="icon-btn" title="Remove account" onclick={() => removeAccount(a)}><Trash size={16} /></button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => ui.open('addAccount')}><Plus size={15} />Add account</button>
      {:else if ui.settingsTab === 'signatures'}
        <h2>Identities & signatures</h2>
        <p class="desc">Each address you send from can have its own name, reply-to and signature.</p>
        <div class="accounts">
          {#each mail.identities as i (i.id)}
            <div class="acct">
              <div class="acct-info">
                <b>{i.name} &lt;{i.email}&gt;</b>
                <span class="acct-sub">{i.signature ? 'Has a signature' : 'No signature'}{i.isDefault ? ' · default' : ''}</span>
              </div>
              <button class="btn sm" onclick={() => (editing = { ...i })}>Edit</button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => (editing = { id: 0, accountId: mail.accounts[0]?.id ?? 0, name: mail.accounts[0]?.name ?? '', email: '', replyTo: null, signature: '', isDefault: false })}>
          <Plus size={15} />Add alias
        </button>
        {#if editing}
          <div class="editor-card">
            <div class="two">
              <div><label class="label" for="id-name">Name</label><input id="id-name" class="input" bind:value={editing.name} /></div>
              <div><label class="label" for="id-email">E-mail</label><input id="id-email" class="input" bind:value={editing.email} disabled={editing.isDefault} /></div>
            </div>
            {#if editing.id === 0}
              <div>
                <label class="label" for="id-acct">Account</label>
                <select id="id-acct" class="input" bind:value={editing.accountId}>
                  {#each mail.accounts as a (a.id)}<option value={a.id}>{a.email}</option>{/each}
                </select>
              </div>
            {/if}
            <div><label class="label" for="id-rt">Reply-to (optional)</label><input id="id-rt" class="input" bind:value={editing.replyTo} /></div>
            <div>
              <span class="label">Signature</span>
              {#key editing.id}
                <div class="rich"><Editor html={editing.signature ?? ''} onupdate={(h) => (editing!.signature = h)} showToolbar placeholder="Your signature" /></div>
              {/key}
            </div>
            <div class="btns">
              {#if editing.id > 0 && !editing.isDefault}
                <button class="btn danger" onclick={async () => { await api.deleteIdentity(editing!.id); await mail.loadAccounts(); editing = null; }}>Delete</button>
              {/if}
              <span class="grow"></span>
              <button class="btn" onclick={() => (editing = null)}>Cancel</button>
              <button class="btn primary" onclick={saveIdentity}>Save</button>
            </div>
          </div>
        {/if}
      {:else if ui.settingsTab === 'notifications'}
        <h2>Notifications</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>New mail</b><span>Show a desktop notification when new mail arrives in an inbox.</span></div>
            <Toggle checked={ui.settings.notifications} onchange={(v) => ui.set('notifications', v)} />
          </div>
          <div class="row">
            <div class="txt"><b>Event reminders</b><span>Notify before calendar events start.</span></div>
            <Toggle checked={ui.settings.calendarNotifications} onchange={(v) => ui.set('calendarNotifications', v)} />
          </div>
        </div>
      {:else if ui.settingsTab === 'privacy'}
        <h2>Privacy</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>Remote images</b><span>Images loaded from the internet can tell senders when and where you read their mail.</span></div>
            <select class="input sel" value={ui.settings.loadRemoteImages} onchange={(e) => ui.set('loadRemoteImages', (e.currentTarget as HTMLSelectElement).value as 'ask' | 'always' | 'never')}>
              <option value="ask">Ask first</option>
              <option value="always">Always load</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>Tracking pixels</b><span>Invisible tracking images are always removed.</span></div>
            <span class="pill-ok"><ShieldCheck size={14} />Always on</span>
          </div>
          <div class="row">
            <div class="txt"><b>Trusted senders</b><span>{ui.settings.trustedSenders.length} senders can load remote images.</span></div>
            <button class="btn sm" disabled={!ui.settings.trustedSenders.length} onclick={() => ui.set('trustedSenders', [])}>Clear</button>
          </div>
        </div>
        <p class="desc">Envelope has no telemetry. Mail is stored only on this computer and your mail servers. AI features send only what you ask about to the provider you configure.</p>
      {:else if ui.settingsTab === 'encryption'}
        <EncryptionSettings />
      {:else if ui.settingsTab === 'ai'}
        <AiSettings />
      {:else if ui.settingsTab === 'shortcuts'}
        <h2>Keyboard shortcuts</h2>
        <div class="group">
          {#each shortcutList as [k, l] (k)}
            <div class="row"><div class="txt"><b>{l}</b></div><span class="kbd">{k}</span></div>
          {/each}
        </div>
        <button class="btn" onclick={() => ui.open('shortcuts')}><Keyboard size={15} />All shortcuts</button>
      {:else if ui.settingsTab === 'advanced'}
        <h2>Advanced</h2>
        <h3>Sign-in clients</h3>
        <p class="desc">
          Google and Microsoft sign-in need an OAuth client registered for your installation. Official builds ship with one; if yours doesn't,
          register a free “Desktop app” client and paste its details here.
          <button class="inline-link" onclick={() => api.openUrl('https://github.com/burakgon/envelope/blob/main/docs/OAUTH.md')}>How to <ExternalLink size={12} /></button>
        </p>
        <div class="editor-card">
          <b>Google</b>
          <div class="two">
            <div><label class="label" for="g-id">Client ID</label><input id="g-id" class="input" bind:value={google.clientId} spellcheck="false" /></div>
            <div><label class="label" for="g-secret">Client secret</label><input id="g-secret" class="input" bind:value={google.clientSecret} spellcheck="false" /></div>
          </div>
          <b>Microsoft</b>
          <div class="two">
            <div><label class="label" for="m-id">Application (client) ID</label><input id="m-id" class="input" bind:value={microsoft.clientId} spellcheck="false" /></div>
            <div><label class="label" for="m-tenant">Tenant</label><input id="m-tenant" class="input" bind:value={microsoft.tenant} placeholder="common" spellcheck="false" /></div>
          </div>
          <div class="btns"><span class="grow"></span><button class="btn primary" onclick={saveOAuth}>Save</button></div>
        </div>
        {#if info}
          <h3>Storage</h3>
          <p class="desc">Data folder: <code class="selectable">{info.dataDir}</code></p>
        {/if}
      {:else}
        <div class="about">
          <Logo size={72} />
          <h2>Envelope</h2>
          <p class="desc">Version {info?.version ?? '0.1.0'}</p>
          <p class="desc">A fast, beautiful mail and calendar client for Linux. Free and open source under the GPL-3.0 license.</p>
          <div class="btns center">
            <button class="btn" onclick={() => api.openUrl('https://github.com/burakgon/envelope')}><ExternalLink size={14} />GitHub</button>
            <button class="btn" onclick={() => api.openUrl('https://github.com/burakgon/envelope/issues/new')}><Mail size={14} />Report a problem</button>
          </div>
        </div>
      {/if}
    </section>
  </div>
</div>

{#if autoReplyFor}
  <AutoReplyDialog account={autoReplyFor} onclose={() => (autoReplyFor = null)} />
{/if}

<style>
  .rich {
    min-height: 140px;
    border-radius: var(--r);
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    padding: 2px 12px 8px;
  }
  .rich :global(.prose) {
    min-height: 100px;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1300;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .window {
    display: flex;
    width: min(980px, 94vw);
    height: min(720px, 90vh);
    border-radius: 18px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: pop-in var(--dur) var(--ease-out);
  }
  nav {
    width: 220px;
    flex-shrink: 0;
    padding: 18px 10px;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav-title {
    font-size: var(--text-lg);
    font-weight: 700;
    padding: 0 10px 12px;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 34px;
    padding: 0 10px;
    border-radius: 9px;
    color: var(--text-2);
    font-weight: 500;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab.on {
    background: var(--bg-selected);
    color: var(--accent-text);
  }
  .content {
    position: relative;
    flex: 1;
    overflow-y: auto;
    padding: 26px 34px 34px;
  }
  .close {
    position: absolute;
    top: 14px;
    right: 14px;
  }
  h2 {
    margin: 0 0 16px;
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.02em;
  }
  h3 {
    margin: 22px 0 8px;
    font-size: var(--text-md);
  }
  .desc {
    color: var(--text-2);
    line-height: 1.55;
    margin: 0 0 14px;
  }
  .group {
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    margin-bottom: 18px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 16px;
    min-height: 58px;
    padding: 10px 16px;
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
  .sel {
    width: auto;
    min-width: 170px;
  }
  .seg {
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: 10px;
    background: var(--bg-active);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 12px;
    border-radius: 8px;
    color: var(--text-2);
    font-weight: 500;
  }
  .seg button.on {
    background: var(--bg-elevated);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .swatches {
    display: flex;
    gap: 8px;
  }
  .sw {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.1);
    transition: transform var(--dur) var(--spring);
  }
  .sw:hover {
    transform: scale(1.12);
  }
  .sw.on {
    box-shadow: 0 0 0 2px var(--bg-elevated), 0 0 0 4px currentColor;
    color: var(--text-2);
  }
  .accounts {
    display: grid;
    gap: 8px;
    margin-bottom: 14px;
  }
  .acct {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .color {
    width: 26px;
    height: 26px;
    border: none;
    border-radius: 50%;
    padding: 0;
    background: none;
    overflow: hidden;
  }
  .color::-webkit-color-swatch-wrapper {
    padding: 0;
  }
  .color::-webkit-color-swatch {
    border: none;
    border-radius: 50%;
  }
  .acct-info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name-input {
    border: none;
    background: none;
    font-weight: 650;
    padding: 0;
    height: 22px;
  }
  .acct-sub {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .editor-card {
    display: grid;
    gap: 12px;
    padding: 16px;
    margin-top: 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .btns {
    display: flex;
    gap: 8px;
  }
  .btns.center {
    justify-content: center;
  }
  .grow {
    flex: 1;
  }
  .pill-ok {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    background: var(--green-soft);
    color: var(--green);
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .inline-link {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--accent-text);
    font-weight: 600;
  }
  code {
    font-family: var(--font-mono);
    font-size: 12px;
    padding: 2px 6px;
    border-radius: 6px;
    background: var(--bg-active);
  }
  .about {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding-top: 40px;
  }
  .about h2 {
    margin: 14px 0 4px;
  }
</style>

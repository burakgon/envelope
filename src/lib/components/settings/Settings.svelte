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
  import { dateFormat } from '../../utils/format';
  import Editor from '../Editor.svelte';
  import { languages, resolveLanguage, t, type LanguageSetting } from '../../i18n';

  let tpl = $state<Template | null>(null);

  function saveTemplate() {
    if (!tpl || !tpl.name.trim()) return;
    const tmpl = { ...tpl, id: tpl.id || crypto.randomUUID(), name: tpl.name.trim() };
    const list = ui.settings.templates;
    ui.set('templates', list.some((x) => x.id === tmpl.id) ? list.map((x) => (x.id === tmpl.id ? tmpl : x)) : [...list, tmpl]);
    tpl = null;
  }

  function deleteTemplate() {
    if (!tpl) return;
    const id = tpl.id;
    ui.set('templates', ui.settings.templates.filter((x) => x.id !== id));
    tpl = null;
  }

  const tabs = $derived([
    { id: 'general', label: t('settings.tab.general'), icon: Gear },
    { id: 'appearance', label: t('settings.tab.appearance'), icon: Palette },
    { id: 'accounts', label: t('settings.tab.accounts'), icon: Users },
    { id: 'signatures', label: t('settings.tab.signatures'), icon: Pen },
    { id: 'templates', label: t('settings.tab.templates'), icon: FileText },
    { id: 'rules', label: t('settings.tab.rules'), icon: Filter },
    { id: 'calendar', label: t('settings.tab.calendar'), icon: Calendar },
    { id: 'notifications', label: t('settings.tab.notifications'), icon: Bell },
    { id: 'privacy', label: t('settings.tab.privacy'), icon: ShieldCheck },
    { id: 'encryption', label: t('settings.tab.encryption'), icon: Lock },
    { id: 'ai', label: t('settings.tab.ai'), icon: Sparkles },
    { id: 'shortcuts', label: t('settings.tab.shortcuts'), icon: Keyboard },
    { id: 'advanced', label: t('settings.tab.advanced'), icon: Code },
    { id: 'about', label: t('settings.tab.about'), icon: Info },
  ]);

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
    if (!confirm(t('settings.accounts.removeConfirm', { email: a.email }))) return;
    try {
      await api.removeAccount(a.id);
      await mail.loadAccounts();
      await mail.loadFolders();
      mail.refresh();
      toasts.show(t('settings.accounts.removed', { email: a.email }));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function reauth(a: Account) {
    try {
      if (a.config.auth === 'password') {
        const pw = prompt(t('settings.accounts.newPassword', { email: a.email }));
        if (!pw) return;
        await api.updateAccount(a.id, { password: pw });
      } else {
        await api.addOAuthAccount(a.config.auth === 'oAuthGoogle' ? 'google' : 'microsoft', a.email);
      }
      toasts.show(t('settings.accounts.signedInAgain'), { kind: 'success' });
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
      toasts.show(t('common.saved'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function saveOAuth() {
    try {
      await api.setOAuthClient('google', $state.snapshot(google));
      await api.setOAuthClient('microsoft', $state.snapshot(microsoft));
      toasts.show(t('settings.advanced.oauthSaved'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  const shortcutList = $derived([
    ['C', t('settings.shortcuts.compose')], ['R / A / F', t('settings.shortcuts.reply')], ['E', t('settings.shortcuts.archive')], ['#', t('common.delete')], ['S', t('settings.shortcuts.star')], ['U', t('settings.shortcuts.readUnread')],
    ['H', t('settings.shortcuts.snooze')], ['V', t('settings.shortcuts.move')], ['L', t('settings.shortcuts.label')], ['J / K', t('settings.shortcuts.nextPrevious')], ['/', t('common.search')], [`${t('keys.ctrl')} K`, t('settings.shortcuts.commandPalette')], ['Z', t('common.undo')], ['G I', t('settings.shortcuts.goToInbox')],
  ]);
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="window" role="dialog" aria-label={t('settings.title')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && ui.close()}>
    <nav>
      <div class="nav-title">{t('settings.title')}</div>
      {#each tabs as tab (tab.id)}
        <button class="tab" class:on={ui.settingsTab === tab.id} title={tab.label} onclick={() => (ui.settingsTab = tab.id)}>
          <tab.icon size={16} /><span class="truncate">{tab.label}</span>
        </button>
      {/each}
    </nav>
    <section class="content">
      <button class="icon-btn close" aria-label={t('settings.close')} onclick={() => ui.close()}><X size={18} /></button>

      {#if ui.settingsTab === 'general'}
        <h2>{t('settings.tab.general')}</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>{t('settings.language')}</b><span>{t('settings.languageDesc')}</span></div>
            <select class="input sel" value={ui.settings.language} onchange={(e) => ui.set('language', (e.currentTarget as HTMLSelectElement).value as LanguageSetting)}>
              <option value="system">{t('settings.languageSystem', { language: languages.find((l) => l.code === resolveLanguage('system').lang)?.name ?? 'English' })}</option>
              {#each languages as l (l.code)}
                <option value={l.code} lang={l.code}>{l.name}</option>
              {/each}
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.general.splitInbox')}</b><span>{t('settings.general.splitInboxDesc')}</span></div>
            <Toggle checked={ui.settings.splitInbox} onchange={(v) => { ui.set('splitInbox', v); mail.refresh(); }} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.general.markRead')}</b><span>{t('settings.general.markReadDesc')}</span></div>
            <select class="input sel" value={String(ui.settings.markReadDelayMs)} onchange={(e) => ui.set('markReadDelayMs', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="0">{t('settings.general.markReadImmediately')}</option>
              <option value="600">{t('settings.general.markReadAfterMoment')}</option>
              <option value="3000">{t('settings.general.markReadAfterSeconds', { count: 3 })}</option>
              <option value="-1">{t('settings.general.markReadNever')}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.general.undoSend')}</b><span>{t('settings.general.undoSendDesc')}</span></div>
            <select class="input sel" value={String(ui.settings.undoSendSeconds)} onchange={(e) => ui.set('undoSendSeconds', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="0">{t('common.off')}</option>
              <option value="5">{t('settings.general.undoSendSeconds', { count: 5 })}</option>
              <option value="10">{t('settings.general.undoSendSeconds', { count: 10 })}</option>
              <option value="20">{t('settings.general.undoSendSeconds', { count: 20 })}</option>
              <option value="30">{t('settings.general.undoSendSeconds', { count: 30 })}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.general.defaultReply')}</b><span>{t('settings.general.defaultReplyDesc', { key: 'R' })}</span></div>
            <select class="input sel" value={ui.settings.defaultReply} onchange={(e) => ui.set('defaultReply', (e.currentTarget as HTMLSelectElement).value as 'reply' | 'replyAll')}>
              <option value="reply">{t('settings.general.reply')}</option>
              <option value="replyAll">{t('settings.general.replyAll')}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.general.sendAndArchive')}</b><span>{t('settings.general.sendAndArchiveDesc')}</span></div>
            <Toggle checked={ui.settings.sendAndArchive} onchange={(v) => ui.set('sendAndArchive', v)} />
          </div>
        </div>
        <div class="group">
          {#if info?.trayAvailable}
            <div class="row">
              <div class="txt"><b>{t('settings.general.closeToTray')}</b><span>{t('settings.general.closeToTrayDesc')}</span></div>
              <Toggle checked={ui.settings.closeToTray} onchange={(v) => ui.set('closeToTray', v)} />
            </div>
          {/if}
          <div class="row">
            <div class="txt"><b>{t('settings.general.autostart')}</b><span>{info?.trayAvailable ? t('settings.general.autostartTrayDesc') : t('settings.general.autostartDesc')}</span></div>
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
        <h2>{t('settings.tab.templates')}</h2>
        <p class="desc">{t('settings.templates.desc')}</p>
        <div class="accounts">
          {#each ui.settings.templates as tmpl (tmpl.id)}
            <div class="acct">
              <div class="acct-info">
                <b>{tmpl.name}</b>
                <span class="acct-sub truncate">{tmpl.subject || htmlToText(tmpl.html).slice(0, 90)}</span>
              </div>
              <button class="btn sm" onclick={() => (tpl = { ...tmpl })}>{t('common.edit')}</button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => (tpl = { id: '', name: '', subject: '', html: '' })}><Plus size={15} />{t('settings.templates.new')}</button>
        {#if tpl}
          <div class="editor-card">
            <div class="two">
              <div><label class="label" for="tpl-name">{t('settings.templates.name')}</label><input id="tpl-name" class="input" bind:value={tpl.name} placeholder={t('settings.templates.namePlaceholder')} /></div>
              <div><label class="label" for="tpl-subject">{t('settings.templates.subject')}</label><input id="tpl-subject" class="input" bind:value={tpl.subject} /></div>
            </div>
            <div>
              <span class="label">{t('settings.templates.message')}</span>
              {#key tpl.id}
                <div class="rich"><Editor html={tpl.html} onupdate={(h) => (tpl!.html = h)} showToolbar placeholder={t('settings.templates.messagePlaceholder')} /></div>
              {/key}
            </div>
            <div class="btns">
              {#if tpl.id}<button class="btn danger" onclick={deleteTemplate}>{t('common.delete')}</button>{/if}
              <span class="grow"></span>
              <button class="btn" onclick={() => (tpl = null)}>{t('common.cancel')}</button>
              <button class="btn primary" disabled={!tpl.name.trim()} onclick={saveTemplate}>{t('common.save')}</button>
            </div>
          </div>
        {/if}
      {:else if ui.settingsTab === 'calendar'}
        <h2>{t('settings.tab.calendar')}</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.weekStart')}</b></div>
            <select class="input sel" value={String(ui.settings.weekStart)} onchange={(e) => { ui.set('weekStart', Number((e.currentTarget as HTMLSelectElement).value)); cal.load(); }}>
              {#each [1, 0, 6] as day (day)}
                <!-- 2 January 2000 was a Sunday. -->
                <option value={String(day)}>{dateFormat({ weekday: 'long' }).format(new Date(2000, 0, 2 + day))}</option>
              {/each}
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.defaultCalendar')}</b><span>{t('settings.calendar.defaultCalendarDesc')}</span></div>
            <select
              class="input sel"
              value={String(cal.defaultCalendar()?.id ?? '')}
              onchange={(e) => ui.set('defaultCalendarId', Number((e.currentTarget as HTMLSelectElement).value) || null)}
            >
              {#each cal.writable as c (c.id)}<option value={String(c.id)}>{c.name}</option>{:else}<option value="">{t('settings.calendar.noCalendars')}</option>{/each}
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.eventLength')}</b></div>
            <select class="input sel" value={String(ui.settings.defaultEventMinutes)} onchange={(e) => ui.set('defaultEventMinutes', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="15">{t('settings.calendar.lengthMinutes', { count: 15 })}</option>
              <option value="30">{t('settings.calendar.lengthMinutes', { count: 30 })}</option>
              <option value="45">{t('settings.calendar.lengthMinutes', { count: 45 })}</option>
              <option value="60">{t('settings.calendar.lengthHours', { count: 1 })}</option>
              <option value="90">{t('settings.calendar.lengthHours', { count: 1.5 })}</option>
              <option value="120">{t('settings.calendar.lengthHours', { count: 2 })}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.reminder')}</b><span>{t('settings.calendar.reminderDesc')}</span></div>
            <select class="input sel" value={String(ui.settings.defaultAlarm)} onchange={(e) => ui.set('defaultAlarm', Number((e.currentTarget as HTMLSelectElement).value))}>
              <option value="-1">{t('common.none')}</option>
              <option value="0">{t('settings.calendar.reminderAtStart')}</option>
              <option value="5">{t('settings.calendar.reminderMinutes', { count: 5 })}</option>
              <option value="10">{t('settings.calendar.reminderMinutes', { count: 10 })}</option>
              <option value="15">{t('settings.calendar.reminderMinutes', { count: 15 })}</option>
              <option value="30">{t('settings.calendar.reminderMinutes', { count: 30 })}</option>
              <option value="60">{t('settings.calendar.reminderHours', { count: 1 })}</option>
              <option value="1440">{t('settings.calendar.reminderDays', { count: 1 })}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.showDeclined')}</b></div>
            <Toggle checked={ui.settings.showDeclined} onchange={(v) => { ui.set('showDeclined', v); cal.load(); }} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('tasks.settings.showInCalendar')}</b><span>{t('tasks.settings.showInCalendarDesc')}</span></div>
            <Toggle checked={ui.settings.calendarTasks} label={t('tasks.settings.showInCalendar')} onchange={(v) => { ui.set('calendarTasks', v); cal.loadTasks(); }} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.eventReminders')}</b><span>{t('settings.calendar.eventRemindersDesc')}</span></div>
            <Toggle checked={ui.settings.calendarNotifications} onchange={(v) => ui.set('calendarNotifications', v)} />
          </div>
        </div>
      {:else if ui.settingsTab === 'appearance'}
        <h2>{t('settings.tab.appearance')}</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>{t('settings.appearance.theme')}</b></div>
            <div class="seg">
              <button class:on={ui.settings.theme === 'light'} onclick={() => ui.set('theme', 'light')}><Sun size={15} />{t('settings.appearance.themeLight')}</button>
              <button class:on={ui.settings.theme === 'dark'} onclick={() => ui.set('theme', 'dark')}><Moon size={15} />{t('settings.appearance.themeDark')}</button>
              <button class:on={ui.settings.theme === 'system'} onclick={() => ui.set('theme', 'system')}>{t('settings.appearance.themeSystem')}</button>
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.appearance.accentColor')}</b></div>
            <div class="swatches">
              {#each hues as h (h)}
                <button class="sw" class:on={ui.settings.accentHue === h} style:background="oklch(0.6 0.2 {h})" aria-label={t('settings.appearance.accentSwatch', { hue: h })} onclick={() => ui.set('accentHue', h)}></button>
              {/each}
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.appearance.density')}</b><span>{t('settings.appearance.densityDesc')}</span></div>
            <div class="seg">
              <button class:on={ui.settings.density === 'comfortable'} onclick={() => ui.set('density', 'comfortable')}>{t('settings.appearance.densityComfortable')}</button>
              <button class:on={ui.settings.density === 'compact'} onclick={() => ui.set('density', 'compact')}>{t('settings.appearance.densityCompact')}</button>
            </div>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.appearance.showAvatars')}</b></div>
            <Toggle checked={ui.settings.showAvatars} onchange={(v) => ui.set('showAvatars', v)} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.appearance.darkEmails')}</b><span>{t('settings.appearance.darkEmailsDesc')}</span></div>
            <Toggle checked={ui.settings.darkEmails} onchange={(v) => ui.set('darkEmails', v)} />
          </div>
          {#if !api.isDemo}
            <div class="row">
              <div class="txt"><b>{t('settings.appearance.systemTitlebar')}</b><span>{t('settings.appearance.systemTitlebarDesc')}</span></div>
              <Toggle checked={ui.settings.nativeTitlebar} onchange={(v) => ui.set('nativeTitlebar', v)} />
            </div>
          {/if}
        </div>
      {:else if ui.settingsTab === 'accounts'}
        <h2>{t('settings.tab.accounts')}</h2>
        <div class="accounts">
          {#each mail.accounts as a (a.id)}
            <div class="acct">
              <input type="color" class="color" value={a.color} onchange={(e) => saveAccount(a, { color: (e.currentTarget as HTMLInputElement).value })} title={t('settings.accounts.color')} />
              <div class="acct-info">
                <input class="name-input" value={a.label ?? ''} placeholder={a.email} onchange={(e) => saveAccount(a, { label: (e.currentTarget as HTMLInputElement).value })} />
                <span class="acct-sub" title={a.config.exchange?.ewsUrl ?? undefined}>{a.email} · {accountKindLabel(a)} {a.sync.state === 'authError' ? `· ${t('settings.accounts.signInNeeded')}` : ''}{autoReplies.on[a.id] ? ` · ${t('settings.accounts.autoRepliesOn')}` : ''}</span>
              </div>
              <Toggle checked={a.enabled} onchange={(v) => saveAccount(a, { enabled: v })} />
              <button class="icon-btn" class:active={autoReplies.on[a.id]} title={t('autoReply.title')} aria-label={t('autoReply.title')} onclick={() => (autoReplyFor = a)}><Away size={16} /></button>
              <button class="btn sm" onclick={() => reauth(a)}><Lock size={13} />{t('settings.accounts.signInAgain')}</button>
              <button class="icon-btn" title={t('settings.accounts.remove')} onclick={() => removeAccount(a)}><Trash size={16} /></button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => ui.open('addAccount')}><Plus size={15} />{t('settings.accounts.add')}</button>
      {:else if ui.settingsTab === 'signatures'}
        <h2>{t('settings.signatures.title')}</h2>
        <p class="desc">{t('settings.signatures.desc')}</p>
        <div class="accounts">
          {#each mail.identities as i (i.id)}
            <div class="acct">
              <div class="acct-info">
                <b>{i.name} &lt;{i.email}&gt;</b>
                <span class="acct-sub">{i.signature ? t('settings.signatures.hasSignature') : t('settings.signatures.noSignature')}{i.isDefault ? ` · ${t('settings.signatures.default')}` : ''}</span>
              </div>
              <button class="btn sm" onclick={() => (editing = { ...i })}>{t('common.edit')}</button>
            </div>
          {/each}
        </div>
        <button class="btn" onclick={() => (editing = { id: 0, accountId: mail.accounts[0]?.id ?? 0, name: mail.accounts[0]?.name ?? '', email: '', replyTo: null, signature: '', isDefault: false })}>
          <Plus size={15} />{t('settings.signatures.addAlias')}
        </button>
        {#if editing}
          <div class="editor-card">
            <div class="two">
              <div><label class="label" for="id-name">{t('settings.signatures.name')}</label><input id="id-name" class="input" bind:value={editing.name} /></div>
              <div><label class="label" for="id-email">{t('settings.signatures.email')}</label><input id="id-email" class="input" bind:value={editing.email} disabled={editing.isDefault} /></div>
            </div>
            {#if editing.id === 0}
              <div>
                <label class="label" for="id-acct">{t('settings.signatures.account')}</label>
                <select id="id-acct" class="input" bind:value={editing.accountId}>
                  {#each mail.accounts as a (a.id)}<option value={a.id}>{a.email}</option>{/each}
                </select>
              </div>
            {/if}
            <div><label class="label" for="id-rt">{t('settings.signatures.replyTo')}</label><input id="id-rt" class="input" bind:value={editing.replyTo} /></div>
            <div>
              <span class="label">{t('settings.signatures.signature')}</span>
              {#key editing.id}
                <div class="rich"><Editor html={editing.signature ?? ''} onupdate={(h) => (editing!.signature = h)} showToolbar placeholder={t('settings.signatures.signaturePlaceholder')} /></div>
              {/key}
            </div>
            <div class="btns">
              {#if editing.id > 0 && !editing.isDefault}
                <button class="btn danger" onclick={async () => { await api.deleteIdentity(editing!.id); await mail.loadAccounts(); editing = null; }}>{t('common.delete')}</button>
              {/if}
              <span class="grow"></span>
              <button class="btn" onclick={() => (editing = null)}>{t('common.cancel')}</button>
              <button class="btn primary" onclick={saveIdentity}>{t('common.save')}</button>
            </div>
          </div>
        {/if}
      {:else if ui.settingsTab === 'notifications'}
        <h2>{t('settings.tab.notifications')}</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>{t('settings.notifications.newMail')}</b><span>{t('settings.notifications.newMailDesc')}</span></div>
            <Toggle checked={ui.settings.notifications} onchange={(v) => ui.set('notifications', v)} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.calendar.eventReminders')}</b><span>{t('settings.calendar.eventRemindersDesc')}</span></div>
            <Toggle checked={ui.settings.calendarNotifications} onchange={(v) => ui.set('calendarNotifications', v)} />
          </div>
          <div class="row">
            <div class="txt"><b>{t('tasks.settings.notifications')}</b><span>{t('tasks.settings.notificationsDesc')}</span></div>
            <Toggle checked={ui.settings.taskNotifications} label={t('tasks.settings.notifications')} onchange={(v) => ui.set('taskNotifications', v)} />
          </div>
        </div>
      {:else if ui.settingsTab === 'privacy'}
        <h2>{t('settings.tab.privacy')}</h2>
        <div class="group">
          <div class="row">
            <div class="txt"><b>{t('settings.privacy.remoteImages')}</b><span>{t('settings.privacy.remoteImagesDesc')}</span></div>
            <select class="input sel" value={ui.settings.loadRemoteImages} onchange={(e) => ui.set('loadRemoteImages', (e.currentTarget as HTMLSelectElement).value as 'ask' | 'always' | 'never')}>
              <option value="ask">{t('settings.privacy.askFirst')}</option>
              <option value="always">{t('settings.privacy.alwaysLoad')}</option>
            </select>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.privacy.trackingPixels')}</b><span>{t('settings.privacy.trackingPixelsDesc')}</span></div>
            <span class="pill-ok"><ShieldCheck size={14} />{t('settings.privacy.alwaysOn')}</span>
          </div>
          <div class="row">
            <div class="txt"><b>{t('settings.privacy.trustedSenders')}</b><span>{t('settings.privacy.trustedSendersDesc', { count: ui.settings.trustedSenders.length })}</span></div>
            <button class="btn sm" disabled={!ui.settings.trustedSenders.length} onclick={() => ui.set('trustedSenders', [])}>{t('common.clear')}</button>
          </div>
        </div>
        <p class="desc">{t('settings.privacy.noTelemetry')}</p>
      {:else if ui.settingsTab === 'encryption'}
        <EncryptionSettings />
      {:else if ui.settingsTab === 'ai'}
        <AiSettings />
      {:else if ui.settingsTab === 'shortcuts'}
        <h2>{t('settings.shortcuts.title')}</h2>
        <div class="group">
          {#each shortcutList as [k, l] (k)}
            <div class="row"><div class="txt"><b>{l}</b></div><span class="kbd">{k}</span></div>
          {/each}
        </div>
        <button class="btn" onclick={() => ui.open('shortcuts')}><Keyboard size={15} />{t('settings.shortcuts.all')}</button>
      {:else if ui.settingsTab === 'advanced'}
        <h2>{t('settings.tab.advanced')}</h2>
        <h3>{t('settings.advanced.signInClients')}</h3>
        <p class="desc">
          {t('settings.advanced.signInClientsDesc')}
          <button class="inline-link" onclick={() => api.openUrl('https://github.com/burakgon/envelope/blob/main/docs/OAUTH.md')}>{t('settings.advanced.howTo')} <ExternalLink size={12} /></button>
        </p>
        <div class="editor-card">
          <b>Google</b>
          <div class="two">
            <div><label class="label" for="g-id">{t('settings.advanced.clientId')}</label><input id="g-id" class="input" bind:value={google.clientId} spellcheck="false" /></div>
            <div><label class="label" for="g-secret">{t('settings.advanced.clientSecret')}</label><input id="g-secret" class="input" bind:value={google.clientSecret} spellcheck="false" /></div>
          </div>
          <b>Microsoft</b>
          <div class="two">
            <div><label class="label" for="m-id">{t('settings.advanced.applicationId')}</label><input id="m-id" class="input" bind:value={microsoft.clientId} spellcheck="false" /></div>
            <div><label class="label" for="m-tenant">{t('settings.advanced.tenant')}</label><input id="m-tenant" class="input" bind:value={microsoft.tenant} placeholder="common" spellcheck="false" /></div>
          </div>
          <div class="btns"><span class="grow"></span><button class="btn primary" onclick={saveOAuth}>{t('common.save')}</button></div>
        </div>
        {#if info}
          <h3>{t('settings.advanced.storage')}</h3>
          <p class="desc">{t('settings.advanced.dataFolder')} <code class="selectable">{info.dataDir}</code></p>
        {/if}
      {:else}
        <div class="about">
          <Logo size={72} />
          <h2>Envelope</h2>
          <p class="desc">{t('settings.about.version', { version: info?.version ?? '0.1.0' })}</p>
          <p class="desc">{t('settings.about.desc')}</p>
          <div class="btns center">
            <button class="btn" onclick={() => api.openUrl('https://github.com/burakgon/envelope')}><ExternalLink size={14} />GitHub</button>
            <button class="btn" onclick={() => api.openUrl('https://github.com/burakgon/envelope/issues/new')}><Mail size={14} />{t('settings.about.reportProblem')}</button>
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
    white-space: nowrap;
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

import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Api } from './index';
import type * as T from './types';
import { t } from '../i18n';

function subscribe<P>(event: string, cb: (payload: P) => void): () => void {
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  listen<P>(event, (e) => cb(e.payload)).then((u) => {
    if (cancelled) u();
    else unlisten = u;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export function createTauriApi(): Api {
  const win = getCurrentWindow();
  return {
    isDemo: false,
    ready: () => invoke('app_ready'),
    info: () => invoke('app_info'),
    autostart: {
      get: () => invoke('autostart_get'),
      set: (enabled) => invoke('autostart_set', { enabled }),
    },

    accounts: () => invoke('accounts'),
    discover: (email) => invoke('discover', { email }),
    addPasswordAccount: (input) => invoke('account_add_password', { input }),
    addExchangeAccount: (input) =>
      invoke('account_add_exchange', {
        email: input.email,
        name: input.name,
        password: input.password,
        ewsUrl: input.ewsUrl || null,
        username: input.username || null,
        acceptInvalidCerts: input.acceptInvalidCerts ?? false,
        auth: input.auth ?? null,
      }),
    addOAuthAccount: (provider, loginHint) => invoke('account_add_oauth', { provider, loginHint: loginHint ?? null }),
    updateAccount: (id, patch) => invoke('account_update', { id, patch }),
    removeAccount: (id) => invoke('account_remove', { id }),
    reorderAccounts: (ids) => invoke('accounts_reorder', { ids }),
    oauthClient: (provider) => invoke('oauth_client_get', { provider }),
    setOAuthClient: (provider, client) => invoke('oauth_client_set', { provider, client }),
    syncNow: (accountId) => invoke('sync_now', { accountId: accountId ?? null }),

    folders: () => invoke('folders'),
    threads: (query) => invoke('threads', { query }),
    thread: (id) => invoke('thread', { id }),
    loadBodies: (ids) => invoke('load_bodies', { ids }),
    setFlags: (threadIds, messageIds, change) => invoke('set_flags', { threadIds, messageIds, change }),
    markFolderRead: (folderId) => invoke('mark_folder_read', { folderId }),
    moveThreads: (threadIds, source, target) => invoke('move_threads', { threadIds, source, target }),
    undo: (token) => invoke('undo', { token }),
    deleteForever: (threadIds) => invoke('delete_forever', { threadIds }),
    snooze: (threadIds, until) => invoke('snooze', { threadIds, until }),
    muteThreads: (threadIds, muted) => invoke('mute_threads', { threadIds, muted }),
    setLabel: (threadIds, folderId, add) => invoke('set_label', { threadIds, folderId, add }),
    messageSource: (id) => invoke('message_source', { id }),
    unsubscribe: (messageId) => invoke('unsubscribe', { messageId }),

    drafts: () => invoke('drafts'),
    draft: (id) => invoke('draft', { id }),
    saveDraft: (id, data, upload) => invoke('save_draft', { id, data, upload }),
    deleteDraft: (id) => invoke('delete_draft', { id }),
    composeReply: (messageId, mode) => invoke('compose_reply', { messageId, mode }),
    send: (draftId, data, delayMs, sendAt) => invoke('send', { draftId, data, delayMs, sendAt }),
    cancelSend: (outboxId) => invoke('cancel_send', { outboxId }),
    retrySend: (outboxId) => invoke('retry_send', { outboxId }),
    outbox: () => invoke('outbox'),
    suggestContacts: (query) => invoke('suggest_contacts', { query }),
    identities: () => invoke('identities'),
    saveIdentity: (identity) => invoke('identity_save', { identity }),
    deleteIdentity: (id) => invoke('identity_delete', { id }),

    fileInfo: (paths) => invoke('file_info', { paths }),
    stashFile: (filename, data) => invoke('stash_file', { filename, data: Array.from(data) }),
    pickFiles: async () => {
      const r = await openDialog({ multiple: true, directory: false, title: t('composer.attachFiles') });
      if (!r) return [];
      return Array.isArray(r) ? r : [r];
    },
    pickSavePath: async (filename) => (await saveDialog({ defaultPath: filename })) ?? null,
    attachmentOpen: (id) => invoke('attachment_open', { id }),
    attachmentSave: (id, target) => invoke('attachment_save', { id, target }),
    attachmentReveal: (id) => invoke('attachment_reveal', { id }),
    attachmentUrl: (id) => `att://localhost/file/${id}`,
    inlineUrl: (messageId, cid) => `att://localhost/inline/${messageId}/${encodeURIComponent(cid)}`,

    settings: () => invoke('settings_get'),
    setSetting: (key, value) => invoke('settings_set', { key, value }),
    setDecorations: (native) => invoke('set_decorations', { native }),
    setBadge: (count) => invoke('set_unread_badge', { count }),
    setNativeStrings: (strings) => invoke('set_native_strings', { strings }),
    openUrl: (url) => openUrl(url),

    onEvent: (cb) => subscribe<T.EngineEvent>('engine', cb),
    onMailto: (cb) => subscribe<string>('mailto', cb),
    onWebcal: (cb) => subscribe<string>('webcal', cb),
    onOpenFile: (cb) => subscribe<string>('open-file', cb),
    onOpenThread: (cb) => subscribe<number | null>('open-thread', cb),
    onOpenTask: (cb) => subscribe<number>('open-task', cb),

    autoReply: {
      get: (accountId) => invoke('auto_reply_get', { accountId }),
      set: (accountId, reply) => invoke('auto_reply_set', { accountId, reply }),
    },
    files: {
      open: (path) => invoke('file_open', { path }),
      importEvents: (path, calendarId, uids) => invoke('file_import_events', { path, calendarId, uids: uids ?? null }),
      importContacts: (path, bookId, indices) => invoke('file_import_contacts', { path, bookId, indices: indices ?? null }),
      openPart: (path, partId) => invoke('file_part_open', { path, partId }),
      savePart: (path, partId, target) => invoke('file_part_save', { path, partId, target }),
    },
    crypto: {
      settings: () => invoke('crypto_settings_get'),
      setSettings: (settings) => invoke('crypto_settings_set', { settings }),
      ownKeys: () => invoke('crypto_own_keys'),
      generateKey: (email, name, passphrase) => invoke('crypto_generate_key', { email, name, passphrase: passphrase || null }),
      importText: (text, passphrase, ownEmail) => invoke('crypto_import_text', { text, passphrase: passphrase || null, ownEmail: ownEmail ?? null }),
      importFile: (path, passphrase, ownEmail) => invoke('crypto_import_file', { path, passphrase: passphrase || null, ownEmail: ownEmail ?? null }),
      exportPublic: (fingerprint) => invoke('crypto_export_public', { fingerprint }),
      exportSecret: (fingerprint, passphrase) => invoke('crypto_export_secret', { fingerprint, passphrase: passphrase || null }),
      exportFile: (fingerprint, secret, passphrase, path) => invoke('crypto_export_file', { fingerprint, secret, passphrase: passphrase || null, path }),
      deleteKey: (fingerprint) => invoke('crypto_delete_key', { fingerprint }),
      keys: (query) => invoke('crypto_keys', { query }),
      setVerified: (fingerprint, verified) => invoke('crypto_set_verified', { fingerprint, verified }),
      setOwnKey: (email, fingerprint) => invoke('crypto_set_own_key', { email, fingerprint }),
      unlock: (fingerprint, passphrase) => invoke('crypto_unlock', { fingerprint, passphrase }),
      lock: () => invoke('crypto_lock'),
      composeStatus: (accountId, identityId, from, recipients) => invoke('crypto_compose_status', { accountId, identityId, from, recipients }),
      lookup: (emails, keyserver, explicit) => invoke('crypto_lookup', { emails, keyserver, explicit }),
      open: (messageId) => invoke('crypto_open', { messageId }),
    },
    contacts: {
      books: () => invoke('contact_books'),
      list: (query, bookId, offset, limit) => invoke('contacts_list', { query, bookId, offset, limit }),
      get: (id) => invoke('contact_get', { id }),
      save: (contact) => invoke('contact_save', { contact }),
      remove: (id) => invoke('contact_delete', { id }),
    },
    tasks: {
      lists: () => invoke('task_lists'),
      query: (view, includeCompleted) => invoke('tasks_query', { view, includeCompleted }),
      due: (from, to) => invoke('tasks_due', { from, to }),
      get: (id) => invoke('task_get', { id }),
      subtasks: (id) => invoke('task_subtasks', { id }),
      create: (input) => invoke('task_create', { input }),
      update: (id, input) => invoke('task_update', { id, input }),
      complete: (id, completed) => invoke('task_complete', { id, completed }),
      reorder: (id, beforeId) => invoke('task_reorder', { id, beforeId: beforeId ?? null }),
      remove: (id) => invoke('task_delete', { id }),
      createList: (accountId, name) => invoke('task_list_create', { accountId: accountId ?? null, name }),
      renameList: (id, name) => invoke('task_list_rename', { id, name }),
      removeList: (id) => invoke('task_list_delete', { id }),
      clearCompleted: (listId) => invoke('tasks_clear_completed', { listId }),
    },
    calendar: {
      list: () => invoke('calendars'),
      setVisible: (id, visible) => invoke('calendar_set_visible', { id, visible }),
      setColor: (id, color) => invoke('calendar_set_color', { id, color }),
      subscribe: (url) => invoke('calendar_subscribe', { url }),
      refresh: (id) => invoke('calendar_refresh', { id }),
      unsubscribe: (id) => invoke('calendar_unsubscribe', { id }),
      occurrences: (from, to) => invoke('occurrences', { from, to }),
      detail: (eventId, recurrenceId) => invoke('event_detail', { eventId, recurrenceId }),
      create: (input) => invoke('event_create', { input }),
      update: (eventId, recurrenceId, scope, input) => invoke('event_update', { eventId, recurrenceId, scope, input }),
      move: (eventId, recurrenceId, scope, start, end) => invoke('event_move', { eventId, recurrenceId, scope, start, end }),
      remove: (eventId, recurrenceId, scope) => invoke('event_delete', { eventId, recurrenceId, scope }),
      respond: (eventId, status, comment) => invoke('event_respond', { eventId, status, comment: comment ?? null }),
      invite: (messageId) => invoke('invite_info', { messageId }),
      respondInvite: (messageId, status, comment) => invoke('invite_respond', { messageId, status, comment: comment ?? null }),
      freeBusy: (calendarId, emails, from, to) => invoke('free_busy', { calendarId, emails, from, to }),
    },

    ai: {
      config: () => invoke('ai_config'),
      setConfig: (settings, apiKey) => invoke('ai_set_config', { settings, apiKey: apiKey ?? null }),
      presets: () => invoke('ai_presets'),
      models: (settings, apiKey) => invoke('ai_models', { settings: settings ?? null, apiKey: apiKey ?? null }),
      test: (settings, apiKey) => invoke('ai_test', { settings, apiKey: apiKey ?? null }),
      run: (task, onEvent) => {
        const runId = `run-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        const channel = new Channel<T.AiEvent>();
        channel.onmessage = (e) => onEvent?.(e);
        const promise = invoke<string>('ai_run', { runId, task, onEvent: channel });
        return { promise, cancel: () => void invoke('ai_cancel', { runId }) };
      },
    },

    window: {
      minimize: () => win.minimize(),
      toggleMaximize: () => win.toggleMaximize(),
      close: () => win.close(),
      isMaximized: () => win.isMaximized(),
      onResized: (cb) => {
        let un: (() => void) | null = null;
        win.onResized(() => cb()).then((u) => (un = u));
        return () => un?.();
      },
    },
  };
}

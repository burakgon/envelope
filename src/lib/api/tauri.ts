import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Api } from './index';
import type * as T from './types';

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

    accounts: () => invoke('accounts'),
    discover: (email) => invoke('discover', { email }),
    addPasswordAccount: (input) => invoke('account_add_password', { input }),
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
      const r = await openDialog({ multiple: true, directory: false, title: 'Attach files' });
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
    openUrl: (url) => openUrl(url),

    onEvent: (cb) => subscribe<T.EngineEvent>('engine', cb),
    onMailto: (cb) => subscribe<string>('mailto', cb),
    onOpenThread: (cb) => subscribe<number | null>('open-thread', cb),

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

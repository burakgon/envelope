// In-memory backend for the browser demo and UI development.

import type { Api } from './index';
import type * as T from './types';
import * as data from './mockdata';
import { mockAiRun, mockPresets } from './mock-ai';
import * as cal from './mock-calendar';
import * as people from './mock-contacts';
import * as todo from './mock-tasks';
import { createMockCrypto } from './mock-crypto';
import { createMockDrafts } from './mock-drafts';

type Listener = (e: T.EngineEvent) => void;

export function createMockApi(): Api {
  const listeners = new Set<Listener>();
  const emit = (e: T.EngineEvent) => queueMicrotask(() => listeners.forEach((l) => l(e)));
  const tasksChanged = (listIds: number[]) => emit({ type: 'tasksChanged', listIds: [...new Set(listIds)] });
  todo.onSettled((listId) => tasksChanged([listId]));
  const threads = data.threads;
  const messages = data.messages;
  let autostart = false;
  const autoReplies = new Map<number, T.AutoReply>();
  let outboxSeq = 1;
  let sentSeq = 30_000;
  const outbox = new Map<number, { item: T.OutboxItem; draftId: number | null; data: T.DraftInput; timer: number }>();
  let identities: T.Identity[] = data.accounts.map((a) => ({
    id: a.id, accountId: a.id, name: a.name, email: a.email, replyTo: null,
    signature: a.id === 1 ? '<p>— Alex<br><span style="color:#888">Product Lead · Lumen</span></p>' : null, isDefault: true,
  }));

  let aiSettings: T.AiSettings = { enabled: true, provider: 'openrouter', baseUrl: 'https://openrouter.ai/api/v1', model: 'openrouter/auto', customInstructions: '', language: null };

  const delay = <V>(v: V, ms = 0): Promise<V> => new Promise((r) => setTimeout(() => r(structuredClone(v)), ms));

  function recount() {
    for (const f of data.folders) {
      const inFolder = threads.filter((t) => t.folderIds.includes(f.id) && !(f.role === 'inbox' && t.snoozedUntil));
      f.totalCount = inFolder.length;
      f.unreadCount = inFolder.filter((t) => t.unreadCount > 0).length;
    }
    for (const t of threads) {
      const ms = messages.filter((m) => m.threadId === t.id);
      t.unreadCount = ms.filter((m) => !m.flags.seen).length;
      t.flagged = ms.some((m) => m.flags.flagged);
      t.messageCount = ms.length;
      t.draftCount = ms.filter((m) => m.flags.draft).length;
      if (ms.length) {
        const last = ms[ms.length - 1];
        t.lastDate = last.date;
        t.snippet = last.snippet;
        t.lastSender = last.from;
      }
    }
  }
  recount();

  const drafts = createMockDrafts({
    identities: () => identities,
    changed: (threadIds) => changed(threadIds),
    queued: () => new Set([...outbox.values()].map((o) => o.draftId).filter((id): id is number => id != null)),
  });

  function changed(threadIds: number[]) {
    recount();
    const accountIds = new Set(threads.filter((t) => threadIds.includes(t.id)).map((t) => t.accountId));
    for (const a of accountIds.size ? accountIds : new Set([1])) emit({ type: 'mailChanged', accountId: a, folderIds: [], threadIds });
  }

  function matches(t: T.ThreadSummary, q: string): boolean {
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    const ms = messages.filter((m) => m.threadId === t.id);
    const hay = [t.subject, ...ms.map((m) => `${m.from?.name ?? ''} ${m.from?.email ?? ''} ${m.body?.text ?? ''} ${(m.body?.html ?? '').replace(/<[^>]+>/g, ' ')} ${m.attachments.map((a) => a.filename).join(' ')}`)].join(' ').toLowerCase();
    return terms.every((term) => {
      if (term === 'is:unread') return t.unreadCount > 0;
      if (term === 'is:starred') return t.flagged;
      if (term === 'has:attachment') return t.hasAttachments;
      if (term.startsWith('from:')) return ms.some((m) => `${m.from?.name} ${m.from?.email}`.toLowerCase().includes(term.slice(5)));
      return hay.includes(term.replace(/^-/, '')) !== term.startsWith('-');
    });
  }

  function list(q: T.ThreadQuery): T.ThreadPage {
    let items = threads.slice();
    const mb = q.mailbox;
    if (mb.kind === 'unified') {
      const ids = data.folders.filter((f) => f.role === mb.role).map((f) => f.id);
      items = items.filter((t) => t.folderIds.some((f) => ids.includes(f)));
      if (mb.role === 'inbox') items = items.filter((t) => !t.snoozedUntil);
    } else if (mb.kind === 'folder') {
      const f = data.folders.find((x) => x.id === mb.id);
      items = items.filter((t) => t.folderIds.includes(mb.id));
      if (f?.role === 'inbox') items = items.filter((t) => !t.snoozedUntil);
    } else if (mb.kind === 'starred') {
      items = items.filter((t) => t.flagged);
    } else if (mb.kind === 'snoozed') {
      items = items.filter((t) => t.snoozedUntil);
    } else if (mb.kind === 'followUp') {
      items = items.filter((t) => /contract renewal/i.test(t.subject)).slice(0, 1);
    } else {
      const trash = data.folders.filter((f) => f.role === 'trash' || f.role === 'junk').map((f) => f.id);
      items = items.filter((t) => !t.folderIds.some((f) => trash.includes(f)) && t.folderIds.length > 0);
    }
    if (q.filter?.unread) items = items.filter((t) => t.unreadCount > 0);
    if (q.filter?.starred) items = items.filter((t) => t.flagged);
    if (q.filter?.attachments) items = items.filter((t) => t.hasAttachments);
    if (q.filter?.category) items = items.filter((t) => t.category === q.filter!.category);
    if (q.search) items = items.filter((t) => matches(t, q.search!));
    items.sort((a, b) => b.lastDate - a.lastDate);
    return { total: items.length, offset: q.offset, items: items.slice(q.offset, q.offset + q.limit) };
  }

  function detail(id: number): T.ThreadDetail {
    const t = threads.find((x) => x.id === id);
    if (!t) throw { message: 'Conversation not found', kind: 'notFound' } satisfies T.ApiError;
    return { thread: t, messages: messages.filter((m) => m.threadId === id).sort((a, b) => a.date - b.date) };
  }

  function role(accountId: number, r: T.FolderRole) {
    return data.folders.find((f) => f.accountId === accountId && f.role === r);
  }

  const settingsKey = 'envelope.demo.settings';
  const loadSettings = (): Record<string, unknown> => {
    try {
      return JSON.parse(localStorage.getItem(settingsKey) ?? '{}');
    } catch {
      return {};
    }
  };

  const api: Api = {
    isDemo: true,
    ready: async () => {},
    info: async () => ({ version: '0.3.0', dataDir: '(demo)', googleConfigured: true, microsoftConfigured: true, trayAvailable: true }),
    autostart: {
      get: async () => autostart,
      set: async (enabled) => {
        autostart = enabled;
      },
    },

    accounts: () => delay(data.accounts),
    discover: async (email) => {
      const domain = email.split('@')[1] ?? '';
      const provider = /gmail/.test(domain) ? 'gmail' : /(outlook|hotmail|live)/.test(domain) ? 'outlook' : /icloud|me\.com/.test(domain) ? 'icloud' : /yahoo/.test(domain) ? 'yahoo' : 'generic';
      return delay({
        email, provider, providerName: provider === 'generic' ? 'IMAP' : provider[0].toUpperCase() + provider.slice(1),
        kind: provider === 'gmail' ? 'gmail' : provider === 'outlook' ? 'microsoft' : 'imap',
        auth: provider === 'gmail' ? 'oAuthGoogle' : provider === 'outlook' ? 'oAuthMicrosoft' : 'password',
        imap: { host: `imap.${domain}`, port: 993, security: 'tls', username: '' },
        smtp: { host: `smtp.${domain}`, port: 465, security: 'tls', username: '' },
        caldavUrl: null, carddavUrl: null,
        passwordHint: provider === 'icloud' ? 'iCloud needs an app-specific password.' : null, passwordHelpUrl: null,
      } satisfies T.Discovery, 400);
    },
    addPasswordAccount: async () => { throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' }; },
    addExchangeAccount: async () => {
      await delay(null, 900);
      throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' };
    },
    addOAuthAccount: async () => { throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' }; },
    updateAccount: async (id, patch) => {
      const a = data.accounts.find((x) => x.id === id)!;
      Object.assign(a, { ...patch, config: patch.config ?? a.config });
      emit({ type: 'accountsChanged' });
      return structuredClone(a);
    },
    removeAccount: async () => {},
    reorderAccounts: async () => {},
    oauthClient: async () => ({ clientId: '' }),
    setOAuthClient: async () => {},
    syncNow: async (accountId) => {
      for (const a of data.accounts.filter((x) => !accountId || x.id === accountId)) {
        emit({ type: 'syncStatus', accountId: a.id, sync: { state: 'syncing', folder: 'Inbox', progress: null } });
        setTimeout(() => emit({ type: 'syncStatus', accountId: a.id, sync: { state: 'idle' } }), 900);
      }
    },

    folders: () => delay(data.folders),
    threads: async (q) => structuredClone(list(q)),
    thread: async (id) => structuredClone(detail(id)),
    loadBodies: async (ids) => structuredClone(messages.filter((m) => ids.includes(m.id))),
    setFlags: async (threadIds, messageIds, change) => {
      const targets = messages.filter((m) => threadIds.includes(m.threadId) || messageIds.includes(m.id));
      for (const t of threadIds) {
        const ms = targets.filter((m) => m.threadId === t);
        if (change.seen != null) {
          if (change.seen) ms.forEach((m) => (m.flags.seen = true));
          else if (ms.length) ms[ms.length - 1].flags.seen = false;
        }
        if (change.flagged != null) {
          if (change.flagged) { if (ms.length) ms[ms.length - 1].flags.flagged = true; }
          else ms.forEach((m) => (m.flags.flagged = false));
        }
      }
      for (const m of targets.filter((x) => messageIds.includes(x.id))) {
        if (change.seen != null) m.flags.seen = change.seen;
        if (change.flagged != null) m.flags.flagged = change.flagged;
      }
      changed([...threadIds, ...targets.map((m) => m.threadId)]);
    },
    markFolderRead: async (folderId) => {
      const ids = threads.filter((t) => t.folderIds.includes(folderId)).map((t) => t.id);
      messages.filter((m) => ids.includes(m.threadId)).forEach((m) => (m.flags.seen = true));
      changed(ids);
    },
    moveThreads: async (threadIds, source, target) => {
      const token: T.UndoToken = { accountOps: [], locations: [], flags: [], snoozed: [] };
      for (const id of threadIds) {
        const t = threads.find((x) => x.id === id);
        if (!t) continue;
        token.locations.push([id, t.folderIds.map((f) => [f, null])]);
        token.snoozed.push([id, t.snoozedUntil]);
        t.snoozedUntil = null;
        const inbox = role(t.accountId, 'inbox')?.id;
        const src = source ?? inbox;
        let dest: number | undefined;
        if (target.kind === 'archive') dest = role(t.accountId, 'archive')?.id ?? role(t.accountId, 'all')?.id;
        else if (target.kind === 'trash') dest = role(t.accountId, 'trash')?.id;
        else if (target.kind === 'junk') dest = role(t.accountId, 'junk')?.id;
        else if (target.kind === 'inbox') dest = inbox;
        else dest = target.id;
        if (target.kind === 'trash' || target.kind === 'junk') t.folderIds = [];
        else t.folderIds = t.folderIds.filter((f) => f !== src);
        if (dest != null && !t.folderIds.includes(dest)) t.folderIds.push(dest);
        messages.filter((m) => m.threadId === id).forEach((m) => (m.folderIds = [...t.folderIds]));
      }
      changed(threadIds);
      return token;
    },
    undo: async (token) => {
      for (const [id, locs] of token.locations) {
        const t = threads.find((x) => x.id === id);
        if (t) t.folderIds = locs.map(([f]) => f);
      }
      for (const [id, s] of token.snoozed) {
        const t = threads.find((x) => x.id === id);
        if (t) t.snoozedUntil = s;
      }
      changed(token.locations.map(([id]) => id));
    },
    deleteForever: async (threadIds) => {
      for (const id of threadIds) {
        const i = threads.findIndex((t) => t.id === id);
        if (i >= 0) threads.splice(i, 1);
      }
      changed(threadIds);
    },
    snooze: async (threadIds, until) => {
      threads.filter((t) => threadIds.includes(t.id)).forEach((t) => (t.snoozedUntil = until));
      changed(threadIds);
    },
    muteThreads: async function (threadIds, muted) {
      threads.filter((t) => threadIds.includes(t.id)).forEach((t) => (t.muted = muted));
      if (!muted) {
        changed(threadIds);
        return [];
      }
      return [await this.moveThreads(threadIds, null, { kind: 'archive' })];
    },
    setLabel: async (threadIds, folderId, add) => {
      for (const t of threads.filter((x) => threadIds.includes(x.id))) {
        t.folderIds = add ? [...new Set([...t.folderIds, folderId])] : t.folderIds.filter((f) => f !== folderId);
      }
      changed(threadIds);
    },
    messageSource: async (id) => {
      const m = messages.find((x) => x.id === id)!;
      return `From: ${m.from?.name} <${m.from?.email}>\nTo: ${m.to.map((a) => a.email).join(', ')}\nSubject: ${m.subject}\nDate: ${new Date(m.date).toUTCString()}\nMessage-ID: <${m.messageId}>\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n${m.body?.text ?? m.snippet}`;
    },
    unsubscribe: async () => true,

    drafts: async () => drafts.list(),
    draft: async (id) => drafts.get(id),
    draftForMessage: async (messageId) => delay(drafts.forMessage(messageId), 120),
    saveDraft: async (id, d, upload) => drafts.save(id, d, upload),
    deleteDraft: async (id) => drafts.remove(id),
    composeReply: async (messageId, mode, quote) => {
      const m = messages.find((x) => x.id === messageId)!;
      const ident = identities.find((i) => i.accountId === m.accountId)!;
      const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
      const quoted = m.body?.html ?? esc(m.body?.text ?? '').replace(/\n/g, '<br>');
      const date = esc(quote?.when || new Date(m.date).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }));
      const sender = m.from ? (m.from.name ? `${esc(m.from.name)} &lt;${esc(m.from.email)}&gt;` : esc(m.from.email)) : '';
      const text = (s: string | null | undefined, english: string) => esc(s?.trim() ? s : english);
      const fwd = mode === 'forward';
      const header = fwd
        ? `<div class="envelope-forward">${text(quote?.forwarded, '---------- Forwarded message ---------')}<br>${text(quote?.from, 'From:')} ${sender}<br>${text(quote?.date, 'Date:')} ${date}<br>${text(quote?.subject, 'Subject:')} ${esc(m.subject)}<br>${text(quote?.to, 'To:')} ${esc(m.to.map((a) => a.email).join(', '))}<br><br>${quoted}</div>`
        : `<div class="envelope-quote"><div>${text(quote?.wrote, 'On {date}, {sender} wrote:').replace('{date}', date).replace('{sender}', sender)}</div><blockquote>${quoted}</blockquote></div>`;
      return {
        accountId: m.accountId, identityId: ident.id,
        to: fwd ? [] : [m.from ?? { email: '' }],
        cc: mode === 'replyAll' ? [...m.to, ...m.cc].filter((a) => a.email !== ident.email) : [],
        bcc: [],
        subject: `${fwd ? 'Fwd' : 'Re'}: ${m.subject.replace(/^(re|fwd):\s*/i, '')}`,
        html: `<p></p>${ident.signature ? `<div class="envelope-signature">${ident.signature}</div>` : ''}${header}`,
        attachments: fwd ? m.attachments.map((a) => ({ filename: a.filename, mimeType: a.mimeType, size: a.size, attachmentId: a.id })) : [],
        replyToMessageId: messageId, mode,
      };
    },
    send: async (draftId, d, delayMs, sendAt) => {
      const id = outboxSeq++;
      const item: T.OutboxItem = { id, accountId: d.accountId, subject: d.subject, to: d.to, sendAt: sendAt ?? Date.now() + delayMs, state: 'queued', error: null };
      const timer = window.setTimeout(() => {
        outbox.delete(id);
        if (draftId != null) drafts.remove(draftId);
        const sent = role(d.accountId, 'sent');
        const ident = identities.find((i) => i.accountId === d.accountId)!;
        const existing = d.replyToMessageId ? messages.find((m) => m.id === d.replyToMessageId) : null;
        const threadId = existing && d.mode !== 'forward' ? existing.threadId : threads.length + 1000;
        if (!threads.some((t) => t.id === threadId)) {
          threads.push({ id: threadId, accountId: d.accountId, subject: d.subject, snippet: '', lastDate: Date.now(), messageCount: 0, unreadCount: 0, flagged: false, hasAttachments: false, draftCount: 0, participants: [{ name: ident.name, email: ident.email }], lastSender: { name: ident.name, email: ident.email }, folderIds: sent ? [sent.id] : [], category: 'people', isInvite: false, muted: false, snoozedUntil: null });
        } else if (sent) {
          const t = threads.find((x) => x.id === threadId)!;
          if (!t.folderIds.includes(sent.id)) t.folderIds.push(sent.id);
        }
        const text = d.html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
        messages.push({
          id: sentSeq++, threadId, accountId: d.accountId, folderIds: sent ? [sent.id] : [], messageId: null, inReplyTo: null, references: [],
          subject: d.subject, from: { name: ident.name, email: ident.email }, to: d.to, cc: d.cc, bcc: d.bcc, replyTo: [], date: Date.now(),
          flags: { seen: true, flagged: false, answered: false, draft: false, forwarded: false }, snippet: text.slice(0, 160), size: d.html.length,
          hasAttachments: d.attachments.length > 0, attachments: [], body: { html: d.html, text: null }, unsubscribe: null, calendarMethod: null, isAuto: false,
        });
        changed([threadId]);
        emit({ type: 'outboxChanged' });
      }, Math.max(0, item.sendAt - Date.now()));
      outbox.set(id, { item, draftId, data: d, timer });
      emit({ type: 'outboxChanged' });
      return id;
    },
    cancelSend: async (outboxId) => {
      const o = outbox.get(outboxId);
      if (!o) return null;
      clearTimeout(o.timer);
      outbox.delete(outboxId);
      const nid = drafts.restore(o.draftId, o.data);
      emit({ type: 'outboxChanged' });
      return nid;
    },
    retrySend: async () => {},
    outbox: async () => [...outbox.values()].map((o) => o.item),
    suggestContacts: async (q) => {
      const s = q.toLowerCase();
      return data.contacts
        .filter((c) => c.email.toLowerCase().includes(s) || (c.name ?? '').toLowerCase().includes(s))
        .slice(0, 8)
        .map((c, i) => ({ name: c.name ?? null, email: c.email, score: 10 - i }));
    },
    identities: async () => structuredClone(identities),
    saveIdentity: async (i) => {
      if (i.id > 0) identities = identities.map((x) => (x.id === i.id ? i : x));
      else identities.push({ ...i, id: identities.length + 10 });
      emit({ type: 'accountsChanged' });
      return i.id;
    },
    deleteIdentity: async (id) => { identities = identities.filter((i) => i.id !== id); },

    fileInfo: async (paths) => paths.map((p) => ({ path: p, filename: p.split('/').pop() ?? p, mimeType: 'application/octet-stream', size: 1024 })),
    stashFile: async (filename, bytes) => ({ path: `/tmp/${filename}`, filename, mimeType: 'image/png', size: bytes.length }),
    pickFiles: async () => [],
    pickSavePath: async () => null,
    attachmentOpen: async () => {},
    attachmentSave: async () => {},
    attachmentReveal: async () => {},
    attachmentUrl: () => '',
    inlineUrl: () => '',

    // Quick add puts Alex's tasks in Reminders (iCloud), which keeps times and importance.
    settings: async () => ({ defaultTaskListId: 4, ...loadSettings() }),
    setSetting: async (key, value) => {
      const s = loadSettings();
      s[key] = value;
      localStorage.setItem(settingsKey, JSON.stringify(s));
    },
    setDecorations: async () => {},
    setBadge: async () => {},
    setNativeStrings: async () => {},
    openUrl: async (url) => { window.open(url, '_blank', 'noopener'); },

    onEvent: (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    onMailto: () => () => {},
    onWebcal: () => () => {},
    onOpenFile: () => () => {},
    onOpenThread: () => () => {},
    onOpenTask: () => () => {},

    autoReply: {
      get: async (accountId) => ({
        reply: structuredClone(
          autoReplies.get(accountId) ?? {
            enabled: false,
            start: null,
            end: null,
            subject: '',
            message: '',
            audience: 'everyone',
          },
        ),
        audiences: ['everyone', 'contacts', 'organization'],
        subject: true,
      }),
      set: async (accountId, reply) => {
        autoReplies.set(accountId, structuredClone(reply));
      },
    },
    files: {
      open: async () => {
        throw { message: 'Opening files needs the desktop app', kind: 'invalid' };
      },
      importEvents: async () => ({ added: 0, skipped: 0 }),
      importContacts: async () => ({ added: 0, skipped: 0 }),
      openPart: async () => {},
      savePart: async () => {},
    },

    crypto: createMockCrypto(() => identities),

    contacts: {
      books: async () => structuredClone(people.books),
      list: async (query, bookId, offset, limit) => people.list(query, bookId, offset, limit),
      get: async (id) => {
        const c = people.contacts.find((x) => x.id === id);
        if (!c) throw { message: 'Contact not found', kind: 'notFound' };
        return structuredClone(c);
      },
      save: async (contact) => people.save(structuredClone(contact)),
      remove: async (id) => people.remove(id),
    },

    tasks: {
      lists: async () => todo.taskLists(),
      query: async (view, includeCompleted) => todo.query(view, includeCompleted),
      due: async (from, to) => todo.due(from, to),
      get: async (id) => todo.get(id),
      subtasks: async (id) => todo.subtasks(id),
      create: async (input) => {
        const task = todo.create(input);
        tasksChanged([task.listId]);
        return task;
      },
      update: async (id, input) => {
        const before = todo.listIdOf(id);
        const task = todo.update(id, input);
        tasksChanged([before, task.listId]);
        return task;
      },
      complete: async (id, completed) => {
        const task = todo.complete(id, completed);
        tasksChanged([task.listId]);
        return task;
      },
      reorder: async (id, beforeId) => {
        todo.reorder(id, beforeId);
        tasksChanged([todo.listIdOf(id)]);
      },
      remove: async (id) => {
        const listId = todo.listIdOf(id);
        todo.remove(id);
        tasksChanged([listId]);
      },
      restore: async (id) => {
        const task = todo.restore(id);
        tasksChanged([task.listId]);
        return task;
      },
      createList: async (accountId, name) => {
        const list = todo.createList(accountId, name);
        tasksChanged([]);
        return list;
      },
      renameList: async (id, name) => {
        todo.renameList(id, name);
        tasksChanged([]);
      },
      removeList: async (id) => {
        todo.removeList(id);
        tasksChanged([]);
      },
      clearCompleted: async (listId) => {
        todo.clearCompleted(listId);
        tasksChanged([listId]);
      },
    },

    calendar: {
      list: async () => structuredClone(cal.calendars),
      setVisible: async (id, visible) => {
        const c = cal.calendars.find((x) => x.id === id);
        if (c) c.visible = visible;
        emit({ type: 'calendarChanged', calendarIds: [id] });
      },
      setColor: async (id, color) => {
        const c = cal.calendars.find((x) => x.id === id);
        if (c) c.color = color;
        emit({ type: 'calendarChanged', calendarIds: [id] });
      },
      subscribe: async (url) => {
        const name = (() => {
          try {
            return new URL(url.replace(/^webcals?:/, 'https:')).hostname;
          } catch {
            throw { message: 'This is not a calendar address.', kind: 'invalid' };
          }
        })();
        const added: T.CalendarInfo = {
          id: Math.max(...cal.calendars.map((c) => c.id)) + 1,
          accountId: null,
          kind: 'subscription',
          name,
          color: '#14b8a6',
          readOnly: true,
          isDefault: false,
          visible: true,
        };
        cal.calendars.push(added);
        emit({ type: 'calendarChanged', calendarIds: [added.id] });
        return structuredClone(added);
      },
      refresh: async () => {},
      unsubscribe: async (id) => {
        const i = cal.calendars.findIndex((c) => c.id === id);
        if (i >= 0) cal.calendars.splice(i, 1);
        emit({ type: 'calendarChanged', calendarIds: [id] });
      },
      occurrences: async (from, to) => cal.occurrences(from, to),
      detail: async (eventId, rid) => cal.detail(eventId, rid),
      create: async (input) => {
        const id = cal.create(input);
        emit({ type: 'calendarChanged', calendarIds: [input.calendarId] });
        return id;
      },
      update: async (id, rid, scope, input) => {
        cal.update(id, rid, scope, input);
        emit({ type: 'calendarChanged', calendarIds: [] });
      },
      move: async (id, rid, scope, start, end) => {
        cal.move(id, rid, scope, start, end);
        emit({ type: 'calendarChanged', calendarIds: [] });
      },
      remove: async (id, rid, scope) => {
        cal.remove(id, rid, scope);
        emit({ type: 'calendarChanged', calendarIds: [] });
      },
      respond: async (id, status) => {
        cal.respond(id, status);
        emit({ type: 'calendarChanged', calendarIds: [] });
      },
      invite: async () => cal.invite(),
      respondInvite: async (_m, status) => {
        cal.respond(cal.invite().eventId!, status);
        emit({ type: 'calendarChanged', calendarIds: [] });
      },
      freeBusy: async (_calendarId, emails, from, to) =>
        cal.freeBusy(emails, from, to, new Set(data.accounts.map((a) => a.email.toLowerCase()))),
    },

    ai: {
      config: async () => ({ ...aiSettings, hasKey: true, local: false }),
      setConfig: async (s) => { aiSettings = { ...s }; },
      presets: async () => mockPresets,
      models: async () => [
        { id: 'openrouter/auto', name: 'Auto (best for prompt)', contextLength: 2_000_000, inputPrice: null, outputPrice: null },
        { id: 'meta-llama/llama-4-maverick', name: 'Llama 4 Maverick', contextLength: 1_000_000, inputPrice: 0.15, outputPrice: 0.6 },
        { id: 'mistralai/mistral-medium-3', name: 'Mistral Medium 3', contextLength: 128_000, inputPrice: 0.4, outputPrice: 2 },
      ],
      test: async () => 'Connected · demo model answered in 210 ms',
      run: (task, onEvent) => mockAiRun(task, onEvent),
    },

    window: {
      minimize: async () => {},
      toggleMaximize: async () => {},
      close: async () => {},
      isMaximized: async () => false,
      onResized: () => () => {},
    },
  };
  return api;
}

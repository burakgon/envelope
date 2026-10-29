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
  // The demo's VIP: Maya, whose mail has a mailbox of its own.
  const vips = new Map<string, T.Vip>([['maya@northwind.design', { email: 'maya@northwind.design', name: 'Maya Chen' }]]);
  const fromVip = (m: T.MessageView) => vips.has(m.from?.email.toLowerCase() ?? '');
  let identities: T.Identity[] = data.accounts.map((a) => ({
    id: a.id, accountId: a.id, name: a.name, email: a.email, replyTo: null,
    signature: a.id === 1 ? '<p>— Alex<br><span style="color:#888">Product Lead · Lumen</span></p>' : null, isDefault: true,
  }));

  let aiSettings: T.AiSettings = { enabled: true, provider: 'openrouter', baseUrl: 'https://openrouter.ai/api/v1', model: 'openrouter/auto', customInstructions: '', language: null };

  const delay = <V>(v: V, ms = 0): Promise<V> => new Promise((r) => setTimeout(() => r(structuredClone(v)), ms));

  // Sync trouble to show (screenshots, tests): `sessionStorage['envelope.demo.sync']` maps account ids to states.
  try {
    const trouble = JSON.parse(sessionStorage.getItem('envelope.demo.sync') ?? '{}') as Record<string, T.SyncState>;
    for (const a of data.accounts) if (trouble[a.id]) a.sync = trouble[a.id];
  } catch {
    /* none */
  }

  function recount() {
    // Conversations first: the folders count their unread ones.
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
    for (const f of data.folders) {
      const inFolder = threads.filter((t) => t.folderIds.includes(f.id) && !(f.role === 'inbox' && t.snoozedUntil));
      f.totalCount = inFolder.length;
      f.unreadCount = inFolder.filter((t) => t.unreadCount > 0).length;
    }
  }
  recount();

  const drafts = createMockDrafts({
    identities: () => identities,
    changed: (threadIds) => changed(threadIds),
    queued: () => new Set([...outbox.values()].map((o) => o.draftId).filter((id): id is number => id != null)),
  });

  // ----- folders --------------------------------------------------------------------

  const invalid = (message: string): T.ApiError => ({ message, kind: 'invalid' });
  const folderById = (id: number) => {
    const f = data.folders.find((x) => x.id === id);
    if (!f) throw { message: 'Folder not found', kind: 'notFound' } satisfies T.ApiError;
    return f;
  };
  /** A folder and the folders inside it, parents first. */
  const subtree = (id: number): T.Folder[] => {
    const out = [folderById(id)];
    for (let i = 0; i < out.length; i++) out.push(...data.folders.filter((f) => f.parentId === out[i].id));
    return out;
  };
  const checkName = (accountId: number, parentId: number | null, kind: T.Folder['kind'], name: string, except?: number) => {
    if (!name.trim()) throw invalid('A folder needs a name');
    if (data.folders.some((f) => f.accountId === accountId && f.parentId === parentId && f.kind === kind && f.id !== except && f.name.toLowerCase() === name.trim().toLowerCase())) {
      throw invalid(`There's already a folder called “${name.trim()}” there`);
    }
  };
  /** Sets a folder's place and name, and the paths of the folders inside it. */
  const rehome = (f: T.Folder, name: string, parentId: number | null) => {
    const parent = parentId != null ? folderById(parentId) : null;
    const old = f.path;
    f.name = name;
    f.parentId = parentId;
    f.path = parent ? `${parent.path}/${name}` : name;
    for (const sub of subtree(f.id).slice(1)) sub.path = f.path + sub.path.slice(old.length);
  };
  const foldersChanged = (accountId: number, threadIds: number[] = []) => {
    emit({ type: 'foldersChanged', accountId });
    changed(threadIds);
  };

  // ----- follow-up reminders: the demo has one conversation waiting for a reply -------

  /** Conversations taken out of Follow up, and when they come back (null: done). */
  const followUpAway = new Map<number, number | null>();
  const inFollowUp = (t: T.ThreadSummary) => {
    if (!/contract renewal/i.test(t.subject)) return false;
    const away = followUpAway.get(t.id);
    return away === undefined || (away != null && away <= Date.now());
  };

  // ----- rules on existing mail -----------------------------------------------------

  type Rule = Parameters<Api['rules']['matches']>[0];
  function ruleMatches(rule: Rule, m: T.MessageView): boolean {
    if (!rule.conditions.length || (rule.accountId != null && rule.accountId !== m.accountId)) return false;
    const test = (c: Rule['conditions'][number]) => {
      const needle = c.value.trim().toLowerCase();
      if (!needle) return true;
      const check = (hay: string, email?: string) =>
        c.op === 'is' ? hay === needle || email === needle
        : c.op === 'endsWith' ? (email ?? hay).endsWith(needle) || hay.endsWith(needle)
        : hay.includes(needle);
      const addr = (a: T.Address) => check((a.name ? `${a.name} <${a.email}>` : a.email).toLowerCase(), a.email.toLowerCase());
      const from = () => !!m.from && addr(m.from);
      const to = () => [...m.to, ...m.cc].some(addr);
      const subject = () => check(m.subject.toLowerCase());
      const hit = c.field === 'from' ? from() : c.field === 'to' ? to() : c.field === 'subject' ? subject() : from() || to() || subject();
      return c.op === 'notContains' ? !hit : hit;
    };
    return rule.match === 'any' ? rule.conditions.some(test) : rule.conditions.every(test);
  }

  function changed(threadIds: number[]) {
    recount();
    const accountIds = new Set(threads.filter((t) => threadIds.includes(t.id)).map((t) => t.accountId));
    for (const a of accountIds.size ? accountIds : new Set([1])) emit({ type: 'mailChanged', accountId: a, folderIds: [], threadIds });
  }

  /** Milliseconds ago for `7d`, `2w`, `1m` (30 days), `1y` (365 days) or `12h`, as the engine counts them. */
  function ago(v: string): number | null {
    const m = /^(\d+)([hdwmy])$/.exec(v);
    if (!m) return null;
    const day = 86_400_000;
    return Number(m[1]) * { h: day / 24, d: day, w: 7 * day, m: 30 * day, y: 365 * day }[m[2] as 'd'];
  }

  /** Whether a conversation answers a query in the engine's words (crates/envelope-core/src/search.rs): one of its messages has to. */
  function matches(t: T.ThreadSummary, q: string): boolean {
    const terms = [...q.toLowerCase().matchAll(/(-?)(?:(\w+):)?(?:"([^"]*)"|(\S+))/g)].map(([, not, key, quoted, bare]) => ({ not: !!not, key, value: quoted ?? bare }));
    const who = (list: (T.Address | null)[]) => list.map((a) => `${a?.name ?? ''} ${a?.email ?? ''}`).join(' ').toLowerCase();
    const inFolder = (v: string) =>
      v === 'all' ||
      v === 'anywhere' ||
      t.folderIds.some((id) => {
        const f = data.folders.find((x) => x.id === id);
        return !!f && (f.role === v || f.name.toLowerCase() === v || f.path.toLowerCase() === v);
      });
    return messages
      .filter((m) => m.threadId === t.id)
      .some((m) => {
        const text = `${t.subject} ${who([m.from])} ${m.body?.text ?? ''} ${(m.body?.html ?? '').replace(/<[^>]+>/g, ' ')} ${m.attachments.map((a) => a.filename).join(' ')}`.toLowerCase();
        return terms.every(({ not, key, value }) => {
          const since = ago(value);
          const hit =
            key === 'is' ? (value === 'unread' ? !m.flags.seen : value === 'read' ? m.flags.seen : value === 'starred' ? m.flags.flagged || t.flagged : value.startsWith('vip') ? fromVip(m) : true)
            : key === 'has' ? !value.startsWith('attachment') || m.attachments.length > 0
            : key === 'from' ? who([m.from]).includes(value)
            : key === 'to' ? who(m.to).includes(value)
            : key === 'cc' ? who(m.cc).includes(value)
            : key === 'subject' ? m.subject.toLowerCase().includes(value)
            : key === 'in' || key === 'label' ? inFolder(value)
            : key === 'newer_than' ? since == null || m.date >= Date.now() - since
            : key === 'older_than' ? since == null || m.date < Date.now() - since
            : key === 'after' ? !(Date.parse(value) > m.date)
            : key === 'before' ? !(Date.parse(value) <= m.date)
            : text.includes(key ? `${key}:${value}` : value);
          return hit !== not;
        });
      });
  }

  function list(q: T.ThreadQuery): T.ThreadPage {
    const trash = data.folders.filter((f) => f.role === 'trash' || f.role === 'junk').map((f) => f.id);
    /** In Trash or Spam, or gone. */
    const trashed = (t: T.ThreadSummary) => !t.folderIds.length || t.folderIds.some((f) => trash.includes(f));
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
    } else if (mb.kind === 'vip') {
      items = items.filter((t) => !trashed(t) && messages.some((m) => m.threadId === t.id && fromVip(m)));
    } else if (mb.kind === 'snoozed') {
      items = items.filter((t) => t.snoozedUntil);
    } else if (mb.kind === 'followUp') {
      items = items.filter(inFollowUp).slice(0, 1);
    } else {
      items = items.filter((t) => !trashed(t));
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
    info: async () => ({ version: '0.4.0', dataDir: '(demo)', googleConfigured: true, microsoftConfigured: true, trayAvailable: true }),
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
    // The demo's computer has Thunderbird with an account not added yet.
    accountsElsewhere: () =>
      delay(
        data.accounts.some((a) => a.email === 'alex.morgan@posteo.test')
          ? []
          : [{ source: 'Thunderbird', email: 'alex.morgan@posteo.test', name: 'Alex Morgan', kind: 'imap', imap: { host: 'imap.posteo.test', port: 993, security: 'tls', username: 'alex.morgan@posteo.test' }, smtp: { host: 'smtp.posteo.test', port: 465, security: 'tls', username: 'alex.morgan@posteo.test' }, ewsUrl: null } satisfies T.FoundAccount],
      ),
    addPasswordAccount: async () => { throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' }; },
    addExchangeAccount: async () => {
      await delay(null, 900);
      throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' };
    },
    addOAuthAccount: async () => { throw { message: 'Adding accounts is disabled in the demo', kind: 'invalid' }; },
    // The demo's servers take any settings but a server named like a mistake.
    verifyAccount: async (_id, config) => {
      await delay(null, 500);
      if (config.imap?.host.includes('wrong')) throw { message: `Could not connect to the incoming (IMAP) server ${config.imap.host}:${config.imap.port}. Check the server name and port.`, kind: 'network' };
    },
    updateAccount: async (id, patch) => {
      const a = data.accounts.find((x) => x.id === id)!;
      const { password, ...rest } = patch;
      Object.assign(a, { ...rest, config: patch.config ?? a.config });
      // A new password signs the account in again.
      if (password && a.sync.state === 'authError') {
        a.sync = { state: 'idle' };
        emit({ type: 'syncStatus', accountId: id, sync: a.sync });
      }
      emit({ type: 'accountsChanged' });
      return structuredClone(a);
    },
    removeAccount: async () => {},
    reorderAccounts: async () => {},
    oauthClient: async () => ({ clientId: '' }),
    setOAuthClient: async () => {},
    syncNow: async (accountId) => {
      for (const a of data.accounts.filter((x) => !accountId || x.id === accountId)) {
        // An account in trouble (see `envelope.demo.sync`) stays in it, one failure more.
        const trouble = a.sync.state === 'error' || a.sync.state === 'offline' || a.sync.state === 'authError' ? a.sync : null;
        emit({ type: 'syncStatus', accountId: a.id, sync: trouble ? { state: 'connecting' } : { state: 'syncing', folder: 'Inbox', progress: null } });
        setTimeout(() => {
          if (trouble && trouble.state !== 'authError') a.sync = { ...trouble, failures: (trouble.failures ?? 0) + 1 };
          emit({ type: 'syncStatus', accountId: a.id, sync: trouble ? a.sync : { state: 'idle' } });
        }, 900);
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
    markRead: async (mailbox, filter, search) => {
      const ids = list({ mailbox, filter: { ...filter, unread: true }, search, offset: 0, limit: 100_000 }).items.map((t) => t.id);
      const unread = messages.filter((m) => ids.includes(m.threadId) && !m.flags.seen && !m.flags.draft);
      unread.forEach((m) => (m.flags.seen = true));
      changed(ids);
      return { threads: ids.length, messageIds: unread.map((m) => m.id) };
    },
    threadRefs: async (mailbox, filter, search) =>
      list({ mailbox, filter, search, offset: 0, limit: 100_000 }).items.map((t) => ({ id: t.id, accountId: t.accountId })),
    setFollowUp: async (threadIds, change) => {
      for (const id of threadIds) {
        if (change.kind === 'due') followUpAway.delete(id);
        else followUpAway.set(id, change.kind === 'remindAt' ? change.at : null);
      }
      changed(threadIds);
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
    folder: {
      import: async (id) => {
        for (const done of [25, 50]) {
          await delay(null, 250);
          emit({ type: 'importProgress', folderId: id, done });
        }
        return delay({ added: 62, skipped: 3, refused: 0 }, 250);
      },
      create: async (accountId, parentId, name, label, color) => {
        const account = data.accounts.find((a) => a.id === accountId);
        if (!account) throw invalid('No such account');
        if (label && account.kind === 'imap') throw invalid('This account has folders, not labels');
        const kind: T.Folder['kind'] = account.kind === 'gmail' || label ? 'label' : 'folder';
        if (parentId != null && account.kind === 'gmail' && folderById(parentId).kind !== 'label') throw invalid('Labels can only go inside other labels');
        checkName(accountId, parentId, kind, name);
        const f: T.Folder = {
          id: Math.max(...data.folders.map((x) => x.id)) + 1, accountId, name: name.trim(), path: name.trim(), parentId: null, role: null, kind,
          unreadCount: 0, totalCount: 0, color: kind === 'label' ? (color ?? null) : null, hidden: false, selectable: true, sortOrder: 0, delimiter: '/',
        };
        data.folders.push(f);
        rehome(f, f.name, parentId);
        foldersChanged(accountId);
        return structuredClone(f);
      },
      rename: async (id, name) => {
        const f = folderById(id);
        if (f.role) throw invalid('Special folders keep their names');
        checkName(f.accountId, f.parentId, f.kind, name, id);
        rehome(f, name.trim(), f.parentId);
        foldersChanged(f.accountId);
      },
      move: async (id, parentId) => {
        const f = folderById(id);
        if (f.role) throw invalid('Special folders stay where they are');
        if (parentId != null && subtree(id).some((x) => x.id === parentId)) throw invalid("A folder can't go inside itself");
        checkName(f.accountId, parentId, f.kind, f.name, id);
        rehome(f, f.name, parentId);
        foldersChanged(f.accountId);
      },
      remove: async (id) => {
        const f = folderById(id);
        if (f.role) throw invalid("Special folders can't be deleted");
        const gone = new Set(subtree(id).map((x) => x.id));
        const trash = role(f.accountId, 'trash');
        const touched: number[] = [];
        for (const t of threads.filter((x) => x.folderIds.some((g) => gone.has(g)))) {
          touched.push(t.id);
          t.folderIds = t.folderIds.filter((g) => !gone.has(g));
          // Mail in a deleted folder goes to Trash; a label is only taken off.
          if (f.kind === 'folder' && trash && !t.folderIds.length) t.folderIds.push(trash.id);
          messages.filter((m) => m.threadId === t.id).forEach((m) => (m.folderIds = [...t.folderIds]));
        }
        data.folders.splice(0, data.folders.length, ...data.folders.filter((x) => !gone.has(x.id)));
        foldersChanged(f.accountId, touched);
      },
      setColor: async (id, color) => {
        const f = folderById(id);
        f.color = color;
        emit({ type: 'foldersChanged', accountId: f.accountId });
      },
      empty: async (id) => {
        const f = folderById(id);
        if (f.role !== 'trash' && f.role !== 'junk') throw invalid('Only Trash and Spam can be emptied');
        const inside = new Set(subtree(id).map((x) => x.id));
        const ids = threads.filter((t) => t.folderIds.some((x) => inside.has(x))).map((t) => t.id);
        threads.splice(0, threads.length, ...threads.filter((t) => !ids.includes(t.id)));
        inside.delete(id);
        data.folders.splice(0, data.folders.length, ...data.folders.filter((x) => !inside.has(x.id)));
        foldersChanged(f.accountId, ids);
        return ids.length;
      },
    },
    rules: {
      fromSender: async (email, limit) => {
        const sender = email.trim().toLowerCase();
        const mine = new Set([...data.accounts.map((a) => a.email), ...identities.map((i) => i.email)].map((e) => e.toLowerCase()));
        const skip = new Set(data.folders.filter((f) => ['sent', 'drafts', 'trash', 'junk'].includes(f.role ?? '')).map((f) => f.id));
        const found = [...threads]
          .sort((a, b) => b.lastDate - a.lastDate)
          .filter((t) => {
            const from = messages.filter((m) => m.threadId === t.id).map((m) => m.from?.email.toLowerCase() ?? '');
            return from.includes(sender) && from.every((f) => f === sender || mine.has(f)) && t.folderIds.some((f) => !skip.has(f));
          })
          .map((t) => t.id);
        return { total: found.length, threadIds: found.slice(0, limit) };
      },
      matches: async (rule, limit) => {
        const skip = new Set(data.folders.filter((f) => ['sent', 'drafts', 'trash', 'junk'].includes(f.role ?? '')).map((f) => f.id));
        const mine = new Set([...data.accounts.map((a) => a.email), ...identities.map((i) => i.email)].map((e) => e.toLowerCase()));
        const found: number[] = [];
        for (const m of [...messages].sort((a, b) => b.date - a.date)) {
          const t = threads.find((x) => x.id === m.threadId);
          if (!t || found.includes(t.id) || m.flags.draft || !t.folderIds.some((f) => !skip.has(f))) continue;
          if (m.from && mine.has(m.from.email.toLowerCase())) continue;
          if (ruleMatches(rule, m)) found.push(t.id);
        }
        return { total: found.length, threadIds: found.slice(0, limit) };
      },
      apply: async (rule, threadIds) => {
        const undo: T.RuleUndo = { moves: [], read: [], starred: [], labeled: [] };
        const kinds = new Set(rule.actions.map((a) => a.kind));
        const theirs = (folderId: number) => threads.filter((t) => threadIds.includes(t.id) && t.accountId === folderById(folderId).accountId);
        if (kinds.has('read')) {
          const unread = messages.filter((m) => threadIds.includes(m.threadId) && !m.flags.seen && !m.flags.draft);
          unread.forEach((m) => (m.flags.seen = true));
          undo.read = unread.map((m) => m.id);
        }
        if (kinds.has('star')) {
          for (const id of threadIds) {
            const ms = messages.filter((m) => m.threadId === id);
            const last = ms[ms.length - 1];
            if (last && !ms.some((m) => m.flags.flagged)) {
              last.flags.flagged = true;
              undo.starred.push(last.id);
            }
          }
        }
        for (const a of rule.actions) {
          if (a.kind !== 'label') continue;
          const missing = theirs(a.folderId).filter((t) => !t.folderIds.includes(a.folderId));
          missing.forEach((t) => t.folderIds.push(a.folderId));
          if (missing.length) undo.labeled.push([a.folderId, missing.map((t) => t.id)]);
        }
        for (const a of rule.actions) {
          if (a.kind === 'move') undo.moves.push(await api.moveThreads(theirs(a.folderId).map((t) => t.id), null, { kind: 'folder', id: a.folderId }));
          else if (a.kind === 'archive') undo.moves.push(await api.moveThreads(threadIds, null, { kind: 'archive' }));
          else if (a.kind === 'spam') undo.moves.push(await api.moveThreads(threadIds, null, { kind: 'junk' }));
          else if (a.kind === 'trash') undo.moves.push(await api.moveThreads(threadIds, null, { kind: 'trash' }));
        }
        changed(threadIds);
        return undo;
      },
      undo: async (undo) => {
        for (const token of [...undo.moves].reverse()) await api.undo(token);
        const ids = new Set<number>();
        for (const [folderId, list] of undo.labeled) {
          for (const t of threads.filter((x) => list.includes(x.id))) {
            t.folderIds = t.folderIds.filter((f) => f !== folderId);
            ids.add(t.id);
          }
        }
        for (const m of messages) {
          if (undo.read.includes(m.id)) m.flags.seen = false;
          if (undo.starred.includes(m.id)) m.flags.flagged = false;
          if (undo.read.includes(m.id) || undo.starred.includes(m.id)) ids.add(m.threadId);
        }
        changed([...ids]);
      },
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
    vips: async () => [...vips.values()].sort((a, b) => (a.name ?? a.email).localeCompare(b.name ?? b.email)),
    setVip: async (email, name, on) => {
      const key = email.trim().toLowerCase();
      if (!key) return;
      if (on) vips.set(key, { email: key, name: name?.trim() || vips.get(key)?.name || null });
      else vips.delete(key);
    },
    saveIdentity: async (i) => {
      if (i.id > 0) identities = identities.map((x) => (x.id === i.id ? i : x));
      else identities.push({ ...i, id: identities.length + 10 });
      emit({ type: 'accountsChanged' });
      return i.id;
    },
    deleteIdentity: async (id) => { identities = identities.filter((i) => i.id !== id); },

    fileInfo: async (paths) => paths.map((p) => ({ path: p, filename: p.split('/').pop() ?? p, mimeType: 'application/octet-stream', size: 1024 })),
    stashFile: async (filename, bytes) => ({ path: `/tmp/${filename}`, filename, mimeType: 'image/png', size: bytes.length }),
    // Asked for anything but attachments: the demo's Downloads hold an archive to import.
    pickFiles: async (title) => (title ? delay(['/home/alex/Downloads/Archive.mbox'], 150) : []),
    pickSavePath: async () => null,
    // The demo has no file system: a folder is "picked" and the files "saved" there.
    pickFolder: async () => delay('/home/alex/Downloads', 150),
    // The demo's Downloads hold an address book exported from Gmail.
    pickFile: async () => delay('/home/alex/Downloads/contacts.csv', 150),
    attachmentOpen: async () => {},
    attachmentSave: async () => {},
    attachmentsSaveAll: async (ids, dir) => {
      const names = messages.flatMap((m) => m.attachments).filter((a) => ids.includes(a.id)).map((a) => a.filename);
      return delay(names.map((n) => `${dir}/${n}`), 400);
    },
    attachmentReveal: async () => {},
    revealPath: async () => {},
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
      open: async (path) => {
        if (path.endsWith('.csv')) {
          const contacts = people.csvContacts.map((c, index) => ({
            index,
            name: c.name,
            email: c.emails[0]?.value ?? null,
            organization: c.organization,
            exists: people.contacts.some((p) => p.emails.some((e) => c.emails.some((x) => x.value === e.value))),
          }));
          return delay({ kind: 'contacts', path, contacts } satisfies T.OpenedFile, 200);
        }
        throw { message: 'Opening files needs the desktop app', kind: 'invalid' };
      },
      importEvents: async () => ({ added: 0, skipped: 0 }),
      importContacts: async (path, bookId, indices) => {
        let added = 0;
        for (const [i, c] of people.csvContacts.entries()) {
          if (indices && !indices.includes(i)) continue;
          if (people.contacts.some((p) => p.emails.some((e) => c.emails.some((x) => x.value === e.value)))) continue;
          await people.save({ ...structuredClone(c), bookId });
          added++;
        }
        return delay({ added, skipped: (indices?.length ?? people.csvContacts.length) - added });
      },
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
      export: async (bookId) => delay(people.contacts.filter((c) => bookId == null || c.bookId === bookId).length),
      duplicates: async () => delay(people.duplicates()),
      merge: async (ids) => delay(people.merge(ids)),
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

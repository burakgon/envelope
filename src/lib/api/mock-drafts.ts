// Drafts in the demo: kept "on this computer", stored "on the server" as draft
// messages in Drafts when a composer closes, and made from such a message
// (written elsewhere) when it is opened. Mirrors the engine's mail_api drafts.

import type * as T from './types';
import * as data from './mockdata';

interface Kept {
  id: number;
  data: T.DraftInput;
  updatedAt: number;
  /** The draft message in Drafts that is this draft's server copy. */
  serverMessageId: number | null;
  /** `updatedAt` of the version the server copy holds. */
  syncedAt: number | null;
}

export function createMockDrafts(opts: {
  identities: () => T.Identity[];
  /** Message ids and threads changed: lists and counts are read again. */
  changed: (threadIds: number[]) => void;
  /** Drafts waiting in the outbox. */
  queued: () => Set<number>;
}) {
  const kept = new Map<number, Kept>();
  let seq = 1;
  let messageSeq = 20_000;
  let threadSeq = 5_000;
  const { threads, messages } = data;

  const drafts = (accountId: number) => data.folders.find((f) => f.accountId === accountId && f.role === 'drafts');
  const alive = (id: number | null) => id != null && messages.some((m) => m.id === id);

  function view(d: Kept): T.Draft {
    const serverMessageId = alive(d.serverMessageId) ? d.serverMessageId : null;
    const changedHere = d.syncedAt != null && d.updatedAt > d.syncedAt;
    return structuredClone({
      id: d.id,
      data: d.data,
      updatedAt: d.updatedAt,
      serverMessageId,
      localOnly: serverMessageId == null && (d.syncedAt == null || changedHere),
    });
  }

  /** Deletes a server copy, and its conversation once nothing else is in it. */
  function dropCopy(messageId: number | null): number[] {
    const i = messages.findIndex((m) => m.id === messageId);
    if (i < 0) return [];
    const [m] = messages.splice(i, 1);
    const t = threads.find((x) => x.id === m.threadId);
    if (t && !messages.some((x) => x.threadId === t.id)) threads.splice(threads.indexOf(t), 1);
    else if (t && !messages.some((x) => x.threadId === t.id && x.flags.draft)) {
      const folder = drafts(t.accountId)?.id;
      t.folderIds = t.folderIds.filter((f) => f !== folder);
    }
    return [m.threadId];
  }

  /** "Uploads" a draft: a draft message in Drafts replaces its old copy. */
  function upload(d: Kept): number[] {
    const changed = dropCopy(d.serverMessageId);
    const folder = drafts(d.data.accountId);
    if (!folder) return changed;
    const me = opts.identities().find((i) => i.id === d.data.identityId) ?? opts.identities().find((i) => i.accountId === d.data.accountId)!;
    const original = d.data.replyToMessageId != null ? messages.find((m) => m.id === d.data.replyToMessageId) : undefined;
    let thread = original && d.data.mode !== 'forward' ? threads.find((t) => t.id === original.threadId) : undefined;
    if (!thread) {
      thread = {
        id: threadSeq++, accountId: d.data.accountId, subject: d.data.subject, snippet: '', lastDate: Date.now(), messageCount: 0,
        unreadCount: 0, flagged: false, hasAttachments: false, draftCount: 0, participants: [{ name: me.name, email: me.email }],
        lastSender: { name: me.name, email: me.email }, folderIds: [], category: 'people', isInvite: false, muted: false, snoozedUntil: null,
      };
      threads.push(thread);
    }
    if (!thread.folderIds.includes(folder.id)) thread.folderIds.push(folder.id);
    const text = d.data.html.replace(/<div class="envelope-(quote|forward)">[\s\S]*$/, '').replace(/<[^>]+>/g, ' ').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim();
    const id = messageSeq++;
    messages.push({
      id, threadId: thread.id, accountId: d.data.accountId, folderIds: [...thread.folderIds], messageId: `draft-${id}@demo`, inReplyTo: null,
      references: [], subject: d.data.subject, from: { name: me.name, email: me.email }, to: d.data.to, cc: d.data.cc, bcc: d.data.bcc,
      replyTo: [], date: Date.now(), flags: { seen: true, flagged: false, answered: false, draft: true, forwarded: false },
      snippet: text.slice(0, 160), size: d.data.html.length, hasAttachments: d.data.attachments.length > 0,
      attachments: d.data.attachments.map((a, n) => ({ id: id * 10 + n, filename: a.filename, mimeType: a.mimeType, size: a.size, contentId: null, inline: false })),
      body: { html: d.data.html, text: null }, unsubscribe: null, calendarMethod: null, isAuto: false,
    });
    d.serverMessageId = id;
    d.syncedAt = d.updatedAt;
    return [...changed, thread.id];
  }

  /** A local draft made from a draft message written elsewhere. */
  function adopt(m: T.MessageView): Kept {
    const me = opts.identities().find((i) => i.accountId === m.accountId && i.email === m.from?.email);
    const earlier = messages
      .filter((x) => x.threadId === m.threadId && x.id !== m.id && !x.flags.draft)
      .sort((a, b) => b.date - a.date)[0];
    const now = Date.now();
    const d: Kept = {
      id: seq++,
      data: {
        accountId: m.accountId,
        identityId: me?.id ?? null,
        from: me ? null : m.from,
        to: structuredClone(m.to),
        cc: structuredClone(m.cc),
        bcc: structuredClone(m.bcc),
        subject: m.subject,
        html: m.body?.html ?? (m.body?.text ?? '').split(/\n{2,}/).map((p) => `<p>${p.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/\n/g, '<br>')}</p>`).join(''),
        attachments: m.attachments.filter((a) => !a.inline).map((a) => ({ filename: a.filename, mimeType: a.mimeType, size: a.size, path: `/demo/drafts/${a.filename}` })),
        replyToMessageId: earlier?.id ?? null,
        mode: earlier ? 'reply' : 'new',
      },
      updatedAt: now,
      serverMessageId: m.id,
      syncedAt: now,
    };
    kept.set(d.id, d);
    return d;
  }

  return {
    list: (): T.Draft[] => [...kept.values()].filter((d) => !opts.queued().has(d.id)).sort((a, b) => b.updatedAt - a.updatedAt).map(view),
    get: (id: number): T.Draft => {
      const d = kept.get(id);
      if (!d) throw { message: `Not found: draft ${id}`, kind: 'notFound' } satisfies T.ApiError;
      return view(d);
    },
    forMessage: (messageId: number): T.Draft => {
      const mine = [...kept.values()].find((d) => d.serverMessageId === messageId && !opts.queued().has(d.id));
      if (mine) return view(mine);
      const m = messages.find((x) => x.id === messageId);
      if (!m) throw { message: 'Conversation not found', kind: 'notFound' } satisfies T.ApiError;
      return view(adopt(m));
    },
    save: (id: number | null, input: T.DraftInput, store: boolean): number => {
      let d = id != null ? kept.get(id) : undefined;
      if (!d) {
        d = { id: seq++, data: input, updatedAt: Date.now(), serverMessageId: null, syncedAt: null };
        kept.set(d.id, d);
      }
      d.data = structuredClone(input);
      d.updatedAt = Math.max(Date.now(), d.updatedAt + 1);
      if (store) {
        const target = d;
        // Uploaded and synced back a moment later, as with a real server.
        setTimeout(() => {
          if (kept.get(target.id) === target) opts.changed(upload(target));
        }, 300);
      }
      return d.id;
    },
    /** Sent or discarded: the draft and its server copy go. */
    remove: (id: number) => {
      const d = kept.get(id);
      if (!d) return;
      kept.delete(id);
      const changed = dropCopy(d.serverMessageId);
      if (changed.length) opts.changed(changed);
    },
    /** A draft back from the outbox (sending was undone). */
    restore: (id: number | null, input: T.DraftInput): number => {
      const d = id != null ? kept.get(id) : undefined;
      if (d) {
        d.data = structuredClone(input);
        return d.id;
      }
      const fresh: Kept = { id: seq++, data: structuredClone(input), updatedAt: Date.now(), serverMessageId: null, syncedAt: null };
      kept.set(fresh.id, fresh);
      return fresh.id;
    },
  };
}

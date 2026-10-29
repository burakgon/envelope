import { SvelteSet } from 'svelte/reactivity';
import { api } from '../api';
import type {
  Account,
  Address,
  Draft,
  EngineEvent,
  Folder,
  FolderRole,
  Identity,
  ListFilter,
  Mailbox,
  MessageView,
  MoveTarget,
  OutboxItem,
  ThreadDetail,
  ThreadSummary,
} from '../api';
import { errorMessage, toasts } from './toasts.svelte';
import { dialogs } from './dialogs.svelte';
import { ui } from './ui.svelte';
import { t, type MessageKey } from '../i18n';
import { dateFormat } from '../utils/format';

const PAGE = 150;

export const roleOrder: FolderRole[] = ['inbox', 'starred', 'important', 'drafts', 'sent', 'archive', 'all', 'junk', 'trash'];

/** The interface name of a special folder ("Inbox", "Sent", …), whatever the server calls it. */
export function roleLabel(role: FolderRole): string {
  return t(`mailbox.${role}`);
}

/** Toasts after a move: [one conversation, several conversations]. */
const movedMessages: Record<MoveTarget['kind'], [MessageKey, MessageKey]> = {
  inbox: ['mail.movedToInbox', 'mail.movedToInboxCount'],
  archive: ['mail.archived', 'mail.archivedCount'],
  trash: ['mail.movedToTrash', 'mail.movedToTrashCount'],
  junk: ['mail.markedAsSpam', 'mail.markedAsSpamCount'],
  folder: ['mail.moved', 'mail.movedCount'],
};

/** A draft only on this computer as a row of Drafts (id: the draft's, negated). */
function localDraftRow(d: Draft, me: Address | null): ThreadSummary {
  const text = d.data.html
    .replace(/<div class="envelope-(?:quote|forward|signature)">[\s\S]*$/, '')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&nbsp;/g, ' ')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/\s+/g, ' ')
    .trim();
  const to = [...d.data.to, ...d.data.cc];
  return {
    id: -d.id,
    accountId: d.data.accountId,
    subject: d.data.subject,
    snippet: text.slice(0, 200),
    lastDate: d.updatedAt,
    messageCount: 1,
    unreadCount: 0,
    flagged: false,
    hasAttachments: d.data.attachments.length > 0,
    draftCount: 1,
    participants: to.length ? to : me ? [me] : [],
    lastSender: to[0] ?? me,
    folderIds: [],
    category: 'people',
    isInvite: false,
    muted: false,
    snoozedUntil: null,
    localDraftId: d.id,
  };
}

function debounce(fn: () => void, ms: number) {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(fn, ms);
  };
}

class Mail {
  accounts = $state<Account[]>([]);
  folders = $state<Folder[]>([]);
  identities = $state<Identity[]>([]);
  outbox = $state<OutboxItem[]>([]);
  /** Sent conversations still waiting for a reply after their reminder came due. */
  followUpCount = $state(0);

  mailbox = $state<Mailbox>({ kind: 'unified', role: 'inbox' });
  search = $state('');
  filter = $state<ListFilter>({});
  category = $state<string | null>('people');

  items = $state<ThreadSummary[]>([]);
  total = $state(0);
  loading = $state(true);
  ready = $state(false);

  selectedId = $state<number | null>(null);
  checked = new SvelteSet<number>();
  anchorId: number | null = null;

  detail = $state<ThreadDetail | null>(null);
  detailLoading = $state(false);

  private seq = 0;
  /** How often a conversation was selected: one opened during a folder change keeps it open. */
  private selections = 0;
  private markReadTimer: ReturnType<typeof setTimeout> | null = null;

  meEmails = $derived(new Set([...this.accounts.map((a) => a.email), ...this.identities.map((i) => i.email)].map((e) => e.toLowerCase())));
  accountById = $derived(new Map(this.accounts.map((a) => [a.id, a])));
  folderById = $derived(new Map(this.folders.map((f) => [f.id, f])));
  multiAccount = $derived(this.accounts.length > 1);

  isInboxView = $derived(
    !this.search &&
      ((this.mailbox.kind === 'unified' && this.mailbox.role === 'inbox') ||
        (this.mailbox.kind === 'folder' && this.folderById.get(this.mailbox.id)?.role === 'inbox')),
  );
  splitActive = $derived(this.isInboxView && ui.settings.splitInbox);

  /** Drafts are listed: drafts only on this computer join the server's. */
  inDrafts = $derived(
    !this.search &&
      ((this.mailbox.kind === 'unified' && this.mailbox.role === 'drafts') ||
        (this.mailbox.kind === 'folder' && this.folderById.get(this.mailbox.id)?.role === 'drafts')),
  );

  mailboxTitle = $derived.by(() => {
    if (this.search) return t('mailbox.search');
    const mb = this.mailbox;
    switch (mb.kind) {
      case 'unified':
        return mb.role === 'inbox' && this.multiAccount ? t('mailbox.allInboxes') : roleLabel(mb.role);
      case 'folder': {
        const f = this.folderById.get(mb.id);
        if (!f) return t('mailbox.folder');
        return f.role ? roleLabel(f.role) : f.name;
      }
      case 'starred':
        return t('mailbox.starred');
      case 'snoozed':
        return t('mailbox.snoozed');
      case 'followUp':
        return t('mailbox.followUp');
      case 'everything':
        return t('mailbox.all');
    }
  });

  mailboxSubtitle = $derived.by(() => {
    const mb = this.mailbox;
    if (mb.kind === 'folder' && this.multiAccount) {
      const f = this.folderById.get(mb.id);
      const a = f && this.accountById.get(f.accountId);
      return a ? (a.label ?? a.email) : '';
    }
    return '';
  });

  inboxUnread = $derived(this.folders.filter((f) => f.role === 'inbox').reduce((n, f) => n + f.unreadCount, 0));

  /** Unread counts per split-inbox category (inbox only). */
  categoryUnread = $state<Record<string, number>>({});

  private refreshSoon = debounce(() => this.refresh(), 60);
  private foldersSoon = debounce(() => this.loadFolders(), 120);

  async init() {
    await Promise.all([this.loadAccounts(), this.loadFolders()]);
    api.onEvent((e) => this.onEvent(e));
    await this.refresh();
    this.ready = true;
    this.loadOutbox();
    this.loadFollowUps();
  }

  private followUpsSoon = debounce(() => this.loadFollowUps(), 400);

  async loadFollowUps() {
    try {
      const page = await api.threads({ mailbox: { kind: 'followUp' }, offset: 0, limit: 1 });
      this.followUpCount = page.total;
    } catch {
      this.followUpCount = 0;
    }
  }

  async loadAccounts() {
    const [accounts, identities] = await Promise.all([api.accounts(), api.identities()]);
    this.accounts = accounts;
    this.identities = identities;
  }

  async loadFolders() {
    this.folders = await api.folders();
    api.setBadge(this.inboxUnread);
  }

  async loadOutbox() {
    this.outbox = await api.outbox();
  }

  private onEvent(e: EngineEvent) {
    switch (e.type) {
      case 'accountsChanged':
        this.loadAccounts();
        this.foldersSoon();
        this.refreshSoon();
        break;
      case 'foldersChanged':
        this.foldersSoon();
        break;
      case 'mailChanged':
        this.refreshSoon();
        this.foldersSoon();
        if (this.followUpCount) this.followUpsSoon();
        if (this.detail && e.threadIds.includes(this.detail.thread.id)) this.reloadDetail();
        break;
      case 'syncStatus': {
        const a = this.accounts.find((x) => x.id === e.accountId);
        if (a) a.sync = e.sync;
        break;
      }
      case 'outboxChanged':
        this.loadOutbox();
        break;
      case 'followUpDue':
        this.followUpsSoon();
        if (this.isMailbox({ kind: 'followUp' })) this.refreshSoon();
        break;
      case 'sendFailed':
        toasts.error(t('mail.sendFailed', { error: e.error }));
        break;
    }
  }

  // ----- list ------------------------------------------------------------------

  private query(offset: number, limit: number) {
    const filter: ListFilter = { ...this.filter };
    if (this.splitActive && this.category) filter.category = this.category;
    return api.threads({
      mailbox: this.search ? { kind: 'everything' } : this.mailbox,
      search: this.search || null,
      filter,
      offset,
      limit,
    });
  }

  /** A draft was saved or discarded here: Drafts, if on screen, lists it as it is now. */
  draftsChanged() {
    if (this.inDrafts) this.refreshSoon();
  }

  /** Drafts only on this computer that belong in the Drafts being viewed. */
  private async localDrafts(): Promise<ThreadSummary[]> {
    if (!this.inDrafts || this.filter.unread || this.filter.starred) return [];
    const mb = this.mailbox;
    const account = mb.kind === 'folder' ? this.folderById.get(mb.id)?.accountId : null;
    try {
      const drafts = await api.drafts();
      return drafts
        .filter((d) => d.localOnly && (account == null || d.data.accountId === account))
        .filter((d) => !this.filter.attachments || d.data.attachments.length > 0)
        .map((d) => {
          const me = this.identities.find((i) => i.id === d.data.identityId) ?? this.identities.find((i) => i.accountId === d.data.accountId);
          return localDraftRow(d, me ? { name: me.name, email: me.email } : null);
        });
    } catch {
      return [];
    }
  }

  async refresh() {
    const seq = ++this.seq;
    const want = Math.max(this.items.filter((t) => t.id > 0).length, PAGE);
    try {
      const [page, local] = await Promise.all([this.query(0, want), this.localDrafts()]);
      if (seq !== this.seq) return;
      this.items = local.length ? [...local, ...page.items].sort((a, b) => b.lastDate - a.lastDate) : page.items;
      this.total = page.total + local.length;
      this.loading = false;
      if (this.splitActive) this.loadCategoryCounts();
    } catch (e) {
      if (seq === this.seq) {
        this.loading = false;
        toasts.error(errorMessage(e));
      }
    }
  }

  private async loadCategoryCounts() {
    const counts: Record<string, number> = {};
    await Promise.all(
      ['people', 'newsletters', 'notifications'].map(async (c) => {
        const p = await api.threads({ mailbox: this.mailbox, filter: { category: c, unread: true }, offset: 0, limit: 1 });
        counts[c] = p.total;
      }),
    );
    this.categoryUnread = counts;
  }

  async loadMore() {
    if (this.items.length >= this.total) return;
    const seq = this.seq;
    const local = this.items.filter((t) => t.id < 0).length;
    const page = await this.query(this.items.length - local, PAGE);
    if (seq !== this.seq) return;
    const known = new Set(this.items.map((t) => t.id));
    this.items = [...this.items, ...page.items.filter((t) => !known.has(t.id))];
    this.total = page.total + local;
  }

  setMailbox(mb: Mailbox) {
    const selections = this.selections;
    this.mailbox = mb;
    this.search = '';
    this.filter = {};
    this.checked.clear();
    this.category = 'people';
    this.items = [];
    this.loading = true;
    ui.view = 'mail';
    this.refresh().then(() => this.closeUnlisted(selections));
  }

  /**
   * After going to another folder or inbox tab: the conversation that was open beside the list
   * closes unless the list has it (or another one was opened meanwhile).
   */
  private closeUnlisted(selections: number) {
    const id = this.selectedId;
    if (id == null || this.selections !== selections || this.loading) return;
    if (!this.items.some((t) => t.id === id)) this.select(null);
  }

  setSearch(q: string) {
    if (q === this.search) return;
    this.search = q;
    this.items = [];
    this.loading = true;
    this.checked.clear();
    this.refresh();
  }

  setCategory(c: string | null) {
    const selections = this.selections;
    this.category = c;
    this.items = [];
    this.loading = true;
    this.refresh().then(() => this.closeUnlisted(selections));
  }

  setFilter(f: ListFilter) {
    this.filter = f;
    this.items = [];
    this.loading = true;
    this.refresh();
  }

  isMailbox(mb: Mailbox): boolean {
    if (this.search) return false;
    return JSON.stringify(mb) === JSON.stringify(this.mailbox);
  }

  // ----- selection & reading ---------------------------------------------------

  select(id: number | null, opts: { open?: boolean } = {}) {
    this.selections++;
    this.selectedId = id;
    this.anchorId = id;
    // A draft only on this computer has no conversation to read: it opens in a composer.
    if (id != null && id < 0) this.detail = null;
    else if (id != null && opts.open !== false) this.openThread(id);
    if (id == null) this.detail = null;
  }

  toggleCheck(id: number) {
    if (this.checked.has(id)) this.checked.delete(id);
    else this.checked.add(id);
    this.anchorId = id;
  }

  checkRange(id: number) {
    const ids = this.items.map((t) => t.id);
    const a = ids.indexOf(this.anchorId ?? this.selectedId ?? id);
    const b = ids.indexOf(id);
    if (a < 0 || b < 0) return this.toggleCheck(id);
    const [from, to] = a < b ? [a, b] : [b, a];
    for (let i = from; i <= to; i++) this.checked.add(ids[i]);
  }

  /** Threads an action applies to: the checked ones, or the selected one (`local`: with drafts only on this computer). */
  targets(local = false): number[] {
    const ids = this.checked.size > 0 ? [...this.checked] : this.selectedId != null ? [this.selectedId] : [];
    return local ? ids : ids.filter((id) => id > 0);
  }

  move(delta: number) {
    if (this.items.length === 0) return;
    const i = this.items.findIndex((t) => t.id === this.selectedId);
    const next = i < 0 ? 0 : Math.min(this.items.length - 1, Math.max(0, i + delta));
    this.select(this.items[next].id);
    if (next > this.items.length - 30) this.loadMore();
  }

  /** Opens the next unread conversation below the selected one (Space at the end of a conversation). */
  nextUnread() {
    const i = this.items.findIndex((t) => t.id === this.selectedId);
    const next = this.items.slice(i + 1).find((t) => t.unreadCount > 0);
    if (next) this.select(next.id);
    else toasts.show(t('mail.noMoreUnread'));
  }

  async openThread(id: number) {
    if (this.markReadTimer) clearTimeout(this.markReadTimer);
    this.detailLoading = this.detail?.thread.id !== id;
    try {
      const d = await api.thread(id);
      if (this.selectedId !== id) return;
      this.detail = d;
      this.detailLoading = false;
      const missing = d.messages.filter((m) => !m.body).map((m) => m.id);
      if (missing.length) this.fetchBodies(id, missing);
      const delay = ui.settings.markReadDelayMs;
      if (d.thread.unreadCount > 0 && delay >= 0) {
        this.markReadTimer = setTimeout(() => {
          if (this.selectedId === id) this.setRead([id], true, true);
        }, delay);
      }
    } catch (e) {
      this.detailLoading = false;
      if (this.selectedId === id) toasts.error(errorMessage(e));
    }
  }

  private async fetchBodies(threadId: number, ids: number[]) {
    try {
      const updated = await api.loadBodies(ids);
      if (this.detail?.thread.id !== threadId) return;
      const byId = new Map(updated.map((m) => [m.id, m]));
      this.detail = { ...this.detail, messages: this.detail.messages.map((m) => byId.get(m.id) ?? m) };
    } catch (e) {
      toasts.error(t('mail.downloadFailed', { error: errorMessage(e) }));
    }
  }

  private async reloadDetail() {
    const id = this.detail?.thread.id;
    if (id == null) return;
    try {
      const d = await api.thread(id);
      if (this.detail?.thread.id !== id) return;
      // Keep already-downloaded bodies while the fresh copy has none.
      const old = new Map(this.detail.messages.map((m) => [m.id, m]));
      d.messages = d.messages.map((m) => (m.body ? m : { ...m, body: old.get(m.id)?.body ?? null }));
      this.detail = d;
    } catch (e) {
      // Gone (a draft saved again is a new message): not shown as if it were still there.
      if ((e as { kind?: string })?.kind === 'notFound' && this.detail?.thread.id === id) this.detail = null;
    }
  }

  // ----- actions -------------------------------------------------------------------

  /** Source folder of a thread for "move" style actions in the current view. */
  private sourceFor(t: ThreadSummary | undefined): number | null {
    if (this.search) return null;
    const mb = this.mailbox;
    if (mb.kind === 'folder') return mb.id;
    if (mb.kind === 'unified' && t) {
      return this.folders.find((f) => f.accountId === t.accountId && f.role === mb.role)?.id ?? null;
    }
    return null;
  }

  /** Removes threads from the visible list immediately and advances selection. */
  private removeLocally(ids: number[]) {
    const set = new Set(ids);
    const idx = this.items.findIndex((t) => t.id === this.selectedId);
    const wasSelected = this.selectedId != null && set.has(this.selectedId);
    this.items = this.items.filter((t) => !set.has(t.id));
    this.total = Math.max(0, this.total - ids.length);
    for (const id of ids) this.checked.delete(id);
    if (wasSelected) {
      const next = this.items[Math.min(idx, this.items.length - 1)];
      this.select(next ? next.id : null);
    }
  }

  /**
   * Moves threads; `folderName` names the destination in the confirmation ("Moved to Work"),
   * `messages` replaces it ([one conversation, several]).
   */
  async moveTo(target: MoveTarget, ids = this.targets(), folderName?: string, messages?: [MessageKey, MessageKey]) {
    if (ids.length === 0) return;
    const byThread = new Map(this.items.map((th) => [th.id, th]));
    // Group by source folder (per account in unified views).
    const groups = new Map<number | null, number[]>();
    for (const id of ids) {
      const s = this.sourceFor(byThread.get(id));
      groups.set(s, [...(groups.get(s) ?? []), id]);
    }
    this.removeLocally(ids);
    try {
      const tokens = await Promise.all([...groups].map(([source, tids]) => api.moveThreads(tids, source, target)));
      const count = ids.length;
      const message =
        folderName != null
          ? t(count > 1 ? 'mail.movedToFolderCount' : 'mail.movedToFolder', { folder: folderName, count })
          : t((messages ?? movedMessages[target.kind])[count > 1 ? 1 : 0], { count });
      toasts.undoable(message, async () => {
        for (const token of tokens) await api.undo(token);
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
    }
  }

  archive(ids?: number[]) {
    return this.moveTo({ kind: 'archive' }, ids);
  }

  trash(ids = this.targets(true)) {
    const local = ids.filter((id) => id < 0);
    if (local.length) this.discardLocal(local);
    ids = ids.filter((id) => id > 0);
    if (!ids.length) return;
    const mb = this.mailbox;
    const inTrash = !this.search && ((mb.kind === 'folder' && this.folderById.get(mb.id)?.role === 'trash') || (mb.kind === 'unified' && mb.role === 'trash'));
    if (inTrash) return this.deleteForever(ids);
    return this.moveTo({ kind: 'trash' }, ids);
  }

  /** Discards drafts only on this computer (rows of Drafts), with Undo. */
  private async discardLocal(rowIds: number[]) {
    const ids = this.items.filter((t) => rowIds.includes(t.id) && t.localDraftId != null).map((t) => t.localDraftId!);
    this.removeLocally(rowIds);
    try {
      const drafts = await Promise.all(ids.map((id) => api.draft(id)));
      await Promise.all(ids.map((id) => api.deleteDraft(id)));
      toasts.undoable(ids.length > 1 ? t('mail.draftsDiscarded', { count: ids.length }) : t('compose.draftDiscarded'), async () => {
        for (const d of drafts) await api.saveDraft(null, d.data, false);
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
    }
  }

  /** Asks before `count` conversations are deleted for good: from Trash, or by emptying Trash or Spam. */
  confirmDeleteForever(count: number) {
    return dialogs.confirm({
      title: t('mail.deleteForeverTitle', { count }),
      message: t('mail.deleteForeverMessage', { count }),
      confirmLabel: t('threadList.deleteForever'),
      danger: true,
    });
  }

  /** Deletes conversations for good (from Trash), once the question with their count is answered yes. */
  async deleteForever(ids = this.targets()) {
    if (!ids.length) return;
    const ok = await this.confirmDeleteForever(ids.length);
    if (!ok) return;
    this.removeLocally(ids);
    try {
      await api.deleteForever(ids);
      toasts.show(ids.length > 1 ? t('mail.deletedCount', { count: ids.length }) : t('mail.deletedForever'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async setRead(ids: number[], seen: boolean, silent = false) {
    if (!ids.length) return;
    for (const th of this.items) if (ids.includes(th.id)) th.unreadCount = seen ? 0 : Math.max(1, th.unreadCount);
    if (this.detail && ids.includes(this.detail.thread.id)) {
      this.detail.thread.unreadCount = seen ? 0 : 1;
      if (seen) for (const m of this.detail.messages) m.flags.seen = true;
    }
    try {
      await api.setFlags(ids, [], { seen });
      if (!silent && ids.length > 1) toasts.show(seen ? t('mail.markedRead') : t('mail.markedUnread'));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  toggleRead(ids = this.targets()) {
    const first = this.items.find((t) => t.id === ids[0]);
    const seen = (first?.unreadCount ?? 0) > 0;
    if (!seen && this.markReadTimer) clearTimeout(this.markReadTimer);
    return this.setRead(ids, seen);
  }

  async setStar(ids: number[], flagged: boolean) {
    if (!ids.length) return;
    for (const t of this.items) if (ids.includes(t.id)) t.flagged = flagged;
    if (this.detail && ids.includes(this.detail.thread.id)) this.detail.thread.flagged = flagged;
    try {
      await api.setFlags(ids, [], { flagged });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  toggleStar(ids = this.targets()) {
    const first = this.items.find((t) => t.id === ids[0]) ?? (this.detail?.thread.id === ids[0] ? this.detail.thread : undefined);
    return this.setStar(ids, !first?.flagged);
  }

  async snooze(until: number, ids = this.targets()) {
    if (!ids.length) return;
    this.removeLocally(ids);
    try {
      await api.snooze(ids, until);
      const when = dateFormat({ weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(until);
      toasts.undoable(t('mail.snoozedUntil', { when }), async () => {
        await api.snooze(ids, null);
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Mutes the conversations (archiving them from the inbox), or unmutes them when all are muted. */
  async toggleMute(ids = this.targets()) {
    if (!ids.length) return;
    const byId = new Map(this.items.map((t) => [t.id, t]));
    const allMuted = ids.every((id) => byId.get(id)?.muted);
    try {
      if (allMuted) {
        await api.muteThreads(ids, false);
        toasts.show(t('mail.unmuted'));
        this.refresh();
        return;
      }
      this.removeLocally(ids);
      const tokens = await api.muteThreads(ids, true);
      toasts.undoable(ids.length > 1 ? t('mail.mutedCount', { count: ids.length }) : t('mail.muted'), async () => {
        await api.muteThreads(ids, false);
        for (const token of tokens) await api.undo(token);
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
    }
  }

  async unsnooze(ids = this.targets()) {
    this.removeLocally(ids);
    await api.snooze(ids, null);
    toasts.show(t('mail.movedBackToInbox'));
  }

  async toggleLabel(folderId: number, ids = this.targets()) {
    const has = this.items.filter((th) => ids.includes(th.id)).every((th) => th.folderIds.includes(folderId));
    try {
      await api.setLabel(ids, folderId, !has);
      const name = this.folderById.get(folderId)?.name ?? '';
      toasts.show(has ? t('mail.labelRemoved', { name }) : t('mail.labelAdded', { name }));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async markMessageRead(m: MessageView, seen: boolean) {
    m.flags.seen = seen;
    await api.setFlags([], [m.id], { seen });
  }

  /**
   * Marks every unread conversation of the list as read, as the list shows it (mailbox, filter,
   * inbox tab and search), or of `folder` (from its menu in the sidebar). Undo marks the same
   * messages unread again.
   */
  async markAllRead(folder?: Folder) {
    const mailbox: Mailbox = folder ? { kind: 'folder', id: folder.id } : this.search ? { kind: 'everything' } : this.mailbox;
    const filter: ListFilter = folder ? {} : { ...this.filter };
    if (!folder && this.splitActive && this.category) filter.category = this.category;
    const search = folder ? null : this.search || null;
    try {
      const done = await api.markRead(mailbox, filter, search);
      if (!done.threads) {
        toasts.show(t('mail.nothingUnread'));
        return;
      }
      toasts.undoable(t('mail.markedReadCount', { count: done.threads }), async () => {
        await api.setFlags([], done.messageIds, { seen: false });
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Whether all these conversations are in Spam, where "Not spam" applies instead of "Report spam". */
  isSpam(ids = this.targets()): boolean {
    if (!ids.length) return false;
    const byId = new Map(this.items.map((th) => [th.id, th]));
    if (this.detail) byId.set(this.detail.thread.id, this.detail.thread);
    return ids.every((id) => byId.get(id)?.folderIds.some((f) => this.folderById.get(f)?.role === 'junk'));
  }

  /** Moves conversations out of Spam into the inbox; the server learns they are not spam. */
  notSpam(ids = this.targets()) {
    return this.moveTo({ kind: 'inbox' }, ids, undefined, ['mail.notSpam', 'mail.notSpamCount']);
  }

  /** Report spam, or "Not spam" for conversations already in Spam (the `!` key). */
  toggleSpam(ids = this.targets()) {
    return this.isSpam(ids) ? this.notSpam(ids) : this.moveTo({ kind: 'junk' }, ids);
  }

  /** The list is Follow up: sent conversations still waiting for a reply. */
  inFollowUp = $derived(this.mailbox.kind === 'followUp' && !this.search);

  /** Stops the follow-up reminders of conversations ("Done"). */
  async followUpDone(ids = this.targets()) {
    if (!ids.length) return;
    if (this.inFollowUp) this.removeLocally(ids);
    try {
      await api.setFollowUp(ids, { kind: 'done' });
      this.loadFollowUps();
      toasts.undoable(t('mail.followUpDone', { count: ids.length }), () => this.followUpBack(ids));
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
    }
  }

  /** Reminds again at `until` unless someone replies first. */
  async followUpLater(until: number, ids = this.targets()) {
    if (!ids.length) return;
    if (this.inFollowUp) this.removeLocally(ids);
    try {
      await api.setFollowUp(ids, { kind: 'remindAt', at: until });
      this.loadFollowUps();
      const when = dateFormat({ weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(until);
      toasts.undoable(t('mail.followUpLater', { when }), () => this.followUpBack(ids));
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
    }
  }

  private async followUpBack(ids: number[]) {
    await api.setFollowUp(ids, { kind: 'due' });
    this.loadFollowUps();
    this.refresh();
  }

  foldersOf(accountId: number): Folder[] {
    return this.folders.filter((f) => f.accountId === accountId && !f.hidden);
  }

  roleFolder(accountId: number, role: FolderRole): Folder | undefined {
    return this.folders.find((f) => f.accountId === accountId && f.role === role);
  }
}

export const mail = new Mail();

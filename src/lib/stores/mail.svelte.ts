import { SvelteSet } from 'svelte/reactivity';
import { api } from '../api';
import type {
  Account,
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
import { ui } from './ui.svelte';

const PAGE = 150;

export const roleOrder: FolderRole[] = ['inbox', 'starred', 'important', 'drafts', 'sent', 'archive', 'all', 'junk', 'trash'];

export const roleLabels: Record<FolderRole, string> = {
  inbox: 'Inbox',
  sent: 'Sent',
  drafts: 'Drafts',
  trash: 'Trash',
  junk: 'Spam',
  archive: 'Archive',
  all: 'All Mail',
  starred: 'Starred',
  important: 'Important',
};

function debounce(fn: () => void, ms: number) {
  let t: ReturnType<typeof setTimeout> | null = null;
  return () => {
    if (t) clearTimeout(t);
    t = setTimeout(fn, ms);
  };
}

class Mail {
  accounts = $state<Account[]>([]);
  folders = $state<Folder[]>([]);
  identities = $state<Identity[]>([]);
  outbox = $state<OutboxItem[]>([]);

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

  mailboxTitle = $derived.by(() => {
    if (this.search) return 'Search';
    const mb = this.mailbox;
    switch (mb.kind) {
      case 'unified':
        return mb.role === 'inbox' && this.multiAccount ? 'All Inboxes' : roleLabels[mb.role];
      case 'folder': {
        const f = this.folderById.get(mb.id);
        if (!f) return 'Folder';
        return f.role ? roleLabels[f.role] : f.name;
      }
      case 'starred':
        return 'Starred';
      case 'snoozed':
        return 'Snoozed';
      case 'everything':
        return 'All Mail';
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
      case 'sendFailed':
        toasts.error(`Message not sent: ${e.error}`);
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

  async refresh() {
    const seq = ++this.seq;
    const want = Math.max(this.items.length, PAGE);
    try {
      const page = await this.query(0, want);
      if (seq !== this.seq) return;
      this.items = page.items;
      this.total = page.total;
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
    const page = await this.query(this.items.length, PAGE);
    if (seq !== this.seq) return;
    const known = new Set(this.items.map((t) => t.id));
    this.items = [...this.items, ...page.items.filter((t) => !known.has(t.id))];
    this.total = page.total;
  }

  setMailbox(mb: Mailbox) {
    this.mailbox = mb;
    this.search = '';
    this.filter = {};
    this.checked.clear();
    this.category = 'people';
    this.items = [];
    this.loading = true;
    ui.view = 'mail';
    this.refresh();
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
    this.category = c;
    this.items = [];
    this.loading = true;
    this.refresh();
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
    this.selectedId = id;
    this.anchorId = id;
    if (id != null && opts.open !== false) this.openThread(id);
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

  /** Threads an action applies to: the checked ones, or the selected one. */
  targets(): number[] {
    if (this.checked.size > 0) return [...this.checked];
    return this.selectedId != null ? [this.selectedId] : [];
  }

  move(delta: number) {
    if (this.items.length === 0) return;
    const i = this.items.findIndex((t) => t.id === this.selectedId);
    const next = i < 0 ? 0 : Math.min(this.items.length - 1, Math.max(0, i + delta));
    this.select(this.items[next].id);
    if (next > this.items.length - 30) this.loadMore();
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
      toasts.error(`Could not download message: ${errorMessage(e)}`);
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
    } catch {
      /* thread was removed */
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

  async moveTo(target: MoveTarget, ids = this.targets(), label?: string) {
    if (ids.length === 0) return;
    const byThread = new Map(this.items.map((t) => [t.id, t]));
    // Group by source folder (per account in unified views).
    const groups = new Map<number | null, number[]>();
    for (const id of ids) {
      const s = this.sourceFor(byThread.get(id));
      groups.set(s, [...(groups.get(s) ?? []), id]);
    }
    this.removeLocally(ids);
    try {
      const tokens = await Promise.all([...groups].map(([source, tids]) => api.moveThreads(tids, source, target)));
      const verb = label ?? { inbox: 'Moved to Inbox', archive: 'Archived', trash: 'Moved to Trash', junk: 'Marked as spam', folder: 'Moved' }[target.kind];
      toasts.undoable(ids.length > 1 ? `${verb} · ${ids.length} conversations` : verb, async () => {
        for (const t of tokens) await api.undo(t);
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

  trash(ids?: number[]) {
    const inTrash = this.mailbox.kind === 'folder' && this.folderById.get(this.mailbox.id)?.role === 'trash';
    if (inTrash) return this.deleteForever(ids);
    return this.moveTo({ kind: 'trash' }, ids);
  }

  async deleteForever(ids = this.targets()) {
    if (!ids.length) return;
    this.removeLocally(ids);
    try {
      await api.deleteForever(ids);
      toasts.show(ids.length > 1 ? `Deleted ${ids.length} conversations` : 'Deleted forever');
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async setRead(ids: number[], seen: boolean, silent = false) {
    if (!ids.length) return;
    for (const t of this.items) if (ids.includes(t.id)) t.unreadCount = seen ? 0 : Math.max(1, t.unreadCount);
    if (this.detail && ids.includes(this.detail.thread.id)) {
      this.detail.thread.unreadCount = seen ? 0 : 1;
      if (seen) for (const m of this.detail.messages) m.flags.seen = true;
    }
    try {
      await api.setFlags(ids, [], { seen });
      if (!silent && ids.length > 1) toasts.show(seen ? 'Marked as read' : 'Marked as unread');
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
      const when = new Date(until).toLocaleString(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' });
      toasts.undoable(`Snoozed until ${when}`, async () => {
        await api.snooze(ids, null);
        this.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async unsnooze(ids = this.targets()) {
    this.removeLocally(ids);
    await api.snooze(ids, null);
    toasts.show('Moved back to Inbox');
  }

  async toggleLabel(folderId: number, ids = this.targets()) {
    const has = this.items.filter((t) => ids.includes(t.id)).every((t) => t.folderIds.includes(folderId));
    try {
      await api.setLabel(ids, folderId, !has);
      const f = this.folderById.get(folderId);
      toasts.show(`${has ? 'Removed' : 'Added'} label “${f?.name ?? ''}”`);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async markMessageRead(m: MessageView, seen: boolean) {
    m.flags.seen = seen;
    await api.setFlags([], [m.id], { seen });
  }

  async markAllRead() {
    const mb = this.mailbox;
    const ids = mb.kind === 'folder' ? [mb.id] : mb.kind === 'unified' ? this.folders.filter((f) => f.role === mb.role).map((f) => f.id) : [];
    for (const id of ids) await api.markFolderRead(id);
    toasts.show('All marked as read');
  }

  foldersOf(accountId: number): Folder[] {
    return this.folders.filter((f) => f.accountId === accountId && !f.hidden);
  }

  roleFolder(accountId: number, role: FolderRole): Folder | undefined {
    return this.folders.find((f) => f.accountId === accountId && f.role === role);
  }
}

export const mail = new Mail();

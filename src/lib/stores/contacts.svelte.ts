import { api } from '../api';
import type { Address, Contact, ContactBook, ContactGroup, ContactInput, ContactSummary, GroupMember } from '../api';
import { intlLocale, t, type MessageKey } from '../i18n';
import { errorMessage, toasts } from './toasts.svelte';
import { confirmDiscard, dialogs } from './dialogs.svelte';
import { compose } from './compose.svelte';

// Field labels the address books use. They are stored in English ("home", "mobile"…) and named in the interface language.
const fieldLabels = new Map<string, MessageKey>([
  ['home', 'contacts.labelHome'],
  ['work', 'contacts.labelWork'],
  ['mobile', 'contacts.labelMobile'],
  ['other', 'contacts.labelOther'],
  ['fax', 'contacts.labelFax'],
  ['pager', 'contacts.labelPager'],
  ['main', 'contacts.labelMain'],
  ['assistant', 'contacts.labelAssistant'],
]);

/** An address book's name; the one on this computer is named in the interface language. */
export function bookName(b: ContactBook): string {
  return b.kind === 'local' ? t('contacts.localBook') : b.name;
}

/** The interface name of an e-mail, phone or address label ("home" → "Home"); custom labels keep their wording, capitalized. */
export function fieldLabel(label: string | null): string {
  if (!label) return '';
  const key = fieldLabels.get(label.toLowerCase());
  return key ? t(key) : label.charAt(0).toLocaleUpperCase(intlLocale()) + label.slice(1);
}

/** Lower-cased and without accents, to compare and search names ("Öztürk" → "ozturk"). */
export const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

/** Whether groups can be made and changed in an address book: on this computer and on CardDAV servers. */
export const groupsEditable = (b: ContactBook | undefined) => !!b && !b.readOnly && (b.kind === 'local' || b.kind === 'carddav');

/** "New group in iCloud", or "New group on this computer". */
export const newGroupTitle = (b: ContactBook) => (b.kind === 'local' ? t('groups.newGroupHere') : t('groups.newGroupIn', { book: b.name }));

/** The addresses to write to for a group: each member's primary address, once; members without one are left out. */
export function groupAddresses(g: ContactGroup): Address[] {
  const seen = new Set<string>();
  const out: Address[] = [];
  for (const m of g.members) {
    const email = m.email?.trim();
    if (!email || seen.has(email.toLowerCase())) continue;
    seen.add(email.toLowerCase());
    out.push({ name: m.name !== email ? m.name : null, email });
  }
  return out;
}

class People {
  books = $state<ContactBook[]>([]);
  /** Address book filter; null = all. */
  bookId = $state<number | null>(null);
  groups = $state<ContactGroup[]>([]);
  /** The group whose people the list shows instead of an address book's; null = none. */
  groupId = $state<number | null>(null);
  group = $derived(this.groups.find((g) => g.id === this.groupId) ?? null);
  /** The group "Add people…" is choosing people for. */
  adding = $state<number | null>(null);
  query = $state('');
  items = $state<ContactSummary[]>([]);
  total = $state(0);
  loading = $state(false);
  selectedId = $state<number | null>(null);
  detail = $state<Contact | null>(null);
  editing = $state<ContactInput | null>(null);
  /** The contact as the editor opened it, to tell whether it was changed. */
  private opened = '';
  private seq = 0;
  private started = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

  writable = $derived(this.books.filter((b) => !b.readOnly));

  /** The people of the group shown that match the search. */
  members = $derived.by(() => {
    const g = this.group;
    if (!g) return [];
    const q = fold(this.query.trim());
    if (!q) return g.members;
    return g.members.filter((m) => [m.name, m.email ?? '', m.organization ?? ''].some((v) => fold(v).includes(q)));
  });

  async init() {
    if (this.started) return;
    this.started = true;
    api.onEvent((e) => {
      if (e.type === 'contactsChanged' || e.type === 'accountsChanged') {
        this.loadBooks();
        this.loadGroups();
        this.load();
        if (this.selectedId != null && !this.editing) this.select(this.selectedId);
      }
    });
    await Promise.all([this.loadBooks(), this.loadGroups()]);
    await this.load();
  }

  async loadBooks() {
    try {
      this.books = await api.contacts.books();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async loadGroups() {
    try {
      this.groups = await api.contacts.groups();
      // A group deleted elsewhere: back to its address book.
      if (this.groupId != null && !this.groups.some((g) => g.id === this.groupId)) this.groupId = null;
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async load() {
    const seq = ++this.seq;
    this.loading = true;
    try {
      const page = await api.contacts.list(this.query, this.bookId, 0, 5000);
      if (seq !== this.seq) return;
      this.items = page.items;
      this.total = page.total;
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      if (seq === this.seq) this.loading = false;
    }
  }

  search(q: string) {
    this.query = q;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => this.load(), 80);
  }

  setBook(id: number | null) {
    this.groupId = null;
    this.bookId = id;
    this.load();
  }

  /** Lists a group's people (its address book's contacts stay loaded for the rest). */
  setGroup(id: number) {
    const g = this.groups.find((x) => x.id === id);
    if (!g) return;
    this.groupId = id;
    if (this.bookId !== g.bookId) {
      this.bookId = g.bookId;
      this.load();
    }
  }

  /** The groups a contact is in. */
  groupsOf(contactId: number): ContactGroup[] {
    return this.groups.filter((g) => g.members.some((m) => m.contactId === contactId));
  }

  /** Why a name can't be a group's in an address book, or null. */
  private groupNameProblem(bookId: number, name: string, except?: number): string | null {
    const n = name.trim();
    if (!n) return t('groups.nameEmpty');
    const taken = this.groups.some((g) => g.bookId === bookId && g.id !== except && fold(g.name) === fold(n));
    return taken ? t('groups.nameTaken', { name: n }) : null;
  }

  /** Asks for a group's name until one fits (or the question is cancelled). */
  private async askGroupName(o: { title: string; confirmLabel: string; value?: string; check: (name: string) => string | null }): Promise<string | null> {
    let value = o.value ?? '';
    let message: string | undefined;
    for (;;) {
      const name = await dialogs.prompt({ title: o.title, message, value, confirmLabel: o.confirmLabel });
      if (name == null) return null;
      message = o.check(name) ?? undefined;
      if (!message) return name.trim();
      value = name;
    }
  }

  /**
   * Asks for a name and makes a group in an address book (the one given or shown, else the first that
   * can have groups), shows it and offers to add people to it.
   */
  async newGroup(bookId: number | null = null) {
    const book = this.books.find((b) => b.id === (bookId ?? this.bookId) && groupsEditable(b)) ?? this.books.find(groupsEditable);
    if (!book) {
      toasts.error(t('groups.noEditableBook'));
      return;
    }
    const name = await this.askGroupName({
      title: newGroupTitle(book),
      confirmLabel: t('groups.create'),
      check: (n) => this.groupNameProblem(book.id, n),
    });
    if (name == null) return;
    try {
      const id = await api.contacts.createGroup(book.id, name, []);
      await this.loadGroups();
      this.setGroup(id);
      toasts.show(t('groups.created', { name }), { kind: 'success' });
      this.adding = id;
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async renameGroup(g: ContactGroup) {
    const name = await this.askGroupName({
      title: t('groups.renameTitle', { name: g.name }),
      confirmLabel: t('groups.rename'),
      value: g.name,
      check: (n) => this.groupNameProblem(g.bookId, n, g.id),
    });
    if (name == null || name === g.name) return;
    try {
      await api.contacts.renameGroup(g.id, name);
      await this.loadGroups();
      toasts.show(t('groups.renamed', { name }), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Deletes a group after asking; the people in it stay. */
  async deleteGroup(g: ContactGroup) {
    const sure = await dialogs.confirm({
      title: t('groups.deleteTitle', { name: g.name }),
      message: t('groups.deleteMessage', { count: g.members.length }),
      confirmLabel: t('groups.delete'),
      danger: true,
    });
    if (!sure) return;
    try {
      await api.contacts.deleteGroup(g.id);
      if (this.groupId === g.id) this.setBook(g.bookId);
      await this.loadGroups();
      toasts.show(t('groups.deleted', { name: g.name }));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async addToGroup(g: ContactGroup, contactIds: number[]) {
    try {
      const count = await api.contacts.addToGroup(g.id, contactIds);
      await this.loadGroups();
      toasts.show(t('groups.added', { count, name: g.name }), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Takes someone out of a group, with Undo. */
  async removeFromGroup(g: ContactGroup, m: GroupMember) {
    const ids = m.contactId != null ? [m.contactId] : [];
    const emails = m.contactId == null && m.email ? [m.email] : [];
    try {
      await api.contacts.removeFromGroup(g.id, ids, emails);
      await this.loadGroups();
      toasts.undoable(t('groups.removed', { name: m.name, group: g.name }), () => {
        api.contacts
          .addToGroup(g.id, ids, emails)
          .then(() => this.loadGroups())
          .catch((e) => toasts.error(errorMessage(e)));
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** A new message to everyone in a group who has an address. */
  writeTo(g: ContactGroup) {
    const to = groupAddresses(g);
    if (!to.length) {
      toasts.error(t('groups.noAddresses'));
      return;
    }
    compose.new({ to });
  }

  /** Whether the editor has changes that weren't saved. */
  get changed(): boolean {
    return !!this.editing && JSON.stringify(this.editing) !== this.opened;
  }

  /** Closes the editor, asking first when that throws changes away; whether it closed. */
  async closeEditor(): Promise<boolean> {
    if (this.changed && !(await confirmDiscard())) return false;
    this.editing = null;
    return true;
  }

  async select(id: number | null) {
    if (!(await this.closeEditor())) return;
    this.selectedId = id;
    if (id == null) {
      this.detail = null;
      return;
    }
    try {
      const c = await api.contacts.get(id);
      if (this.selectedId === id) this.detail = c;
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** Moves the selection up or down the list (a group's people when a group is shown). */
  step(dir: 1 | -1) {
    const ids = this.group ? this.members.flatMap((m) => (m.contactId != null ? [m.contactId] : [])) : this.items.map((c) => c.id);
    if (!ids.length) return;
    const i = ids.indexOf(this.selectedId ?? -1);
    this.select(ids[Math.max(0, Math.min(ids.length - 1, i < 0 ? 0 : i + dir))]);
  }

  async newContact(prefill: Partial<ContactInput> = {}) {
    if (!(await this.closeEditor())) return;
    const book = this.writable.find((b) => b.id === this.bookId) ?? this.writable[0];
    if (!book) {
      toasts.error(t('contacts.noWritableBook'));
      return;
    }
    this.editing = {
      id: null,
      bookId: book.id,
      name: '',
      givenName: '',
      familyName: '',
      emails: [{ value: '', label: 'work' }],
      phones: [],
      addresses: [],
      urls: [],
      organization: '',
      title: '',
      birthday: null,
      notes: '',
      photo: null,
      ...prefill,
    };
    this.opened = JSON.stringify(this.editing);
  }

  edit() {
    if (!this.detail) return;
    this.editing = $state.snapshot(this.detail) as ContactInput;
    this.opened = JSON.stringify(this.editing);
  }

  async save() {
    const c = this.editing;
    if (!c) return;
    const clean = (list: { value: string; label: string | null }[]) => list.filter((x) => x.value.trim());
    const name = [c.givenName, c.familyName].filter((x) => x?.trim()).join(' ').trim() || c.name.trim() || c.organization?.trim() || c.emails[0]?.value.trim() || '';
    if (!name) {
      toasts.error(t('contacts.nameRequired'));
      return;
    }
    const input: ContactInput = {
      ...($state.snapshot(c) as ContactInput),
      name,
      emails: clean(c.emails),
      phones: clean(c.phones),
      addresses: clean(c.addresses),
      urls: c.urls.filter((u) => u.trim()),
    };
    // Made while a group of its address book is shown: it joins the group.
    const group = c.id == null && this.group && !this.group.readOnly && this.group.bookId === input.bookId ? this.group : null;
    try {
      const id = await api.contacts.save(input);
      this.editing = null;
      if (group) await api.contacts.addToGroup(group.id, [id]);
      toasts.show(c.id == null ? t('contacts.added') : t('contacts.saved'), { kind: 'success' });
      // Groups show their people's names and addresses too.
      await Promise.all([this.load(), this.loadGroups()]);
      await this.select(id);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async remove() {
    const c = this.detail;
    if (!c) return;
    const listed = () => (this.group ? this.members.flatMap((m) => (m.contactId != null ? [m.contactId] : [])) : this.items.map((x) => x.id));
    const i = listed().indexOf(c.id);
    try {
      await api.contacts.remove(c.id);
      toasts.show(t('contacts.deleted', { name: c.name }));
      await Promise.all([this.load(), this.loadGroups()]);
      const ids = listed();
      this.select(ids[Math.min(i, ids.length - 1)] ?? null);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
}

export const people = new People();

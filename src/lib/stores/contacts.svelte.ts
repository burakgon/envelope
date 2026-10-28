import { api } from '../api';
import type { Contact, ContactBook, ContactInput, ContactSummary } from '../api';
import { errorMessage, toasts } from './toasts.svelte';

class People {
  books = $state<ContactBook[]>([]);
  /** Address book filter; null = all. */
  bookId = $state<number | null>(null);
  query = $state('');
  items = $state<ContactSummary[]>([]);
  total = $state(0);
  loading = $state(false);
  selectedId = $state<number | null>(null);
  detail = $state<Contact | null>(null);
  editing = $state<ContactInput | null>(null);
  private seq = 0;
  private started = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

  writable = $derived(this.books.filter((b) => !b.readOnly));

  async init() {
    if (this.started) return;
    this.started = true;
    api.onEvent((e) => {
      if (e.type === 'contactsChanged' || e.type === 'accountsChanged') {
        this.loadBooks();
        this.load();
        if (this.selectedId != null && !this.editing) this.select(this.selectedId);
      }
    });
    await this.loadBooks();
    await this.load();
  }

  async loadBooks() {
    try {
      this.books = await api.contacts.books();
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
    this.bookId = id;
    this.load();
  }

  async select(id: number | null) {
    this.selectedId = id;
    this.editing = null;
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

  /** Moves the selection up or down the list. */
  step(dir: 1 | -1) {
    if (!this.items.length) return;
    const i = this.items.findIndex((c) => c.id === this.selectedId);
    const next = this.items[Math.max(0, Math.min(this.items.length - 1, i < 0 ? 0 : i + dir))];
    this.select(next.id);
  }

  newContact(prefill: Partial<ContactInput> = {}) {
    const book = this.writable.find((b) => b.id === this.bookId) ?? this.writable[0];
    if (!book) {
      toasts.error('There is no address book you can add contacts to.');
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
  }

  edit() {
    if (this.detail) this.editing = $state.snapshot(this.detail) as ContactInput;
  }

  async save() {
    const c = this.editing;
    if (!c) return;
    const clean = (list: { value: string; label: string | null }[]) => list.filter((x) => x.value.trim());
    const name = [c.givenName, c.familyName].filter((x) => x?.trim()).join(' ').trim() || c.name.trim() || c.organization?.trim() || c.emails[0]?.value.trim() || '';
    if (!name) {
      toasts.error('Add a name or an e-mail address.');
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
    try {
      const id = await api.contacts.save(input);
      this.editing = null;
      toasts.show(c.id == null ? 'Contact added' : 'Contact saved', { kind: 'success' });
      await this.load();
      await this.select(id);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async remove() {
    const c = this.detail;
    if (!c) return;
    const i = this.items.findIndex((x) => x.id === c.id);
    try {
      await api.contacts.remove(c.id);
      toasts.show(`Deleted ${c.name}`);
      await this.load();
      const next = this.items[Math.min(i, this.items.length - 1)];
      this.select(next?.id ?? null);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
}

export const people = new People();

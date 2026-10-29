// Demo address books and contacts for the browser demo.

import type * as T from './types';

export const books: T.ContactBook[] = [
  { id: 1, accountId: 1, kind: 'google', name: 'Work contacts', readOnly: false },
  { id: 2, accountId: 2, kind: 'carddav', name: 'iCloud', readOnly: false },
];

/** Mom's birthday: nine days from now, when the demo calendar has her birthday dinner. */
const momsBirthday = (() => {
  const d = new Date();
  d.setDate(d.getDate() + 9);
  return `1961-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
})();

let seq = 1;
const C = (bookId: number, name: string, email: string, more: Partial<T.Contact> = {}): T.Contact => {
  const [givenName, ...rest] = name.split(' ');
  return {
    id: seq++,
    bookId,
    name,
    givenName,
    familyName: rest.join(' ') || null,
    emails: [{ value: email, label: 'work' }],
    phones: [],
    addresses: [],
    urls: [],
    organization: null,
    title: null,
    birthday: null,
    notes: null,
    photo: null,
    ...more,
  };
};

export const contacts: T.Contact[] = [
  C(1, 'Priya Raman', 'priya@lumen.dev', { organization: 'Lumen', title: 'Head of Product', phones: [{ value: '+90 532 410 22 18', label: 'mobile' }], addresses: [{ value: 'Maslak Mah. Büyükdere Cd. 255\n34398 Sarıyer, İstanbul', label: 'work' }] }),
  C(1, 'Daniel Okafor', 'daniel@lumen.dev', { organization: 'Lumen', title: 'Engineering Manager', phones: [{ value: '+44 7700 900 461', label: 'mobile' }], birthday: '--03-14' }),
  C(1, 'Sofia Martínez', 'sofia@lumen.dev', { organization: 'Lumen', title: 'Senior Designer', urls: ['https://sofia.design'] }),
  C(1, 'Olivia Brooks', 'olivia@lumen.dev', { organization: 'Lumen', title: 'People & Operations' }),
  C(1, 'Maya Chen', 'maya@northwind.design', { organization: 'Northwind Design', title: 'Design Director', phones: [{ value: '+1 415 555 0142', label: 'work' }], notes: 'Met at Config. Prefers async feedback in Figtree comments.' }),
  C(1, 'Lukas Weber', 'lukas.weber@kraftwerk-studio.de', { organization: 'Kraftwerk Studio', title: 'Geschäftsführer', phones: [{ value: '+49 30 901820', label: 'work' }], addresses: [{ value: 'Rosenthaler Str. 40\n10178 Berlin', label: 'work' }] }),
  C(1, 'Kenji Tanaka', 'kenji@tanaka.jp', { organization: 'Tanaka & Co.', title: 'Partner' }),
  C(2, 'Elif Yılmaz', 'elif@yilmaz.family', { emails: [{ value: 'elif@yilmaz.family', label: 'home' }], phones: [{ value: '+90 555 210 44 90', label: 'mobile' }], birthday: '1992-06-21' }),
  C(2, 'Noah Williams', 'noah.w@fastmail.test', { emails: [{ value: 'noah.w@fastmail.test', label: 'home' }] }),
  C(2, 'Linda Morgan', 'linda.morgan@icloud.test', { emails: [{ value: 'linda.morgan@icloud.test', label: 'home' }], phones: [{ value: '+1 503 555 0199', label: 'home' }], notes: 'Mom 💛', birthday: momsBirthday }),
  C(2, 'Hannah Becker', 'hannah.becker@mail.test', { title: 'Landlord', phones: [{ value: '+49 151 2345 6789', label: 'mobile' }] }),
  C(2, 'Arjun Patel', 'arjun@patel.dev', { organization: 'Freelance', title: 'iOS developer' }),
  C(2, 'Zoe Garcia', 'zoe.garcia@umbrella.org', { organization: 'Umbrella', title: 'Researcher' }),
  C(1, 'Carlos Rossi', 'carlos@globex.com', { organization: 'Globex', title: 'Account Executive', phones: [{ value: '+39 02 555 0110', label: 'work' }] }),
  C(1, 'Aisha Haddad', 'aisha@initech.net', { organization: 'Initech', title: 'CTO' }),
  C(2, 'Ben Cohen', 'ben.cohen@hooli.xyz', { organization: 'Hooli' }),
  // In the address book twice, as often happens after an import.
  C(1, 'Dan Okafor', 'Daniel@lumen.dev', { phones: [{ value: '+44 7700 900461', label: 'mobile' }] }),
];

const summary = (c: T.Contact): T.ContactSummary => ({
  id: c.id,
  bookId: c.bookId,
  name: c.name,
  email: c.emails[0]?.value ?? null,
  organization: c.organization,
  photo: c.photo,
});

export function list(query: string, bookId: number | null, offset: number, limit: number): T.ContactPage {
  const q = query.trim().toLowerCase();
  const hits = contacts
    .filter((c) => bookId == null || c.bookId === bookId)
    .filter((c) => !q || [c.name, c.organization ?? '', ...c.emails.map((e) => e.value), ...c.phones.map((p) => p.value)].some((v) => v.toLowerCase().includes(q)))
    .sort((a, b) => a.name.localeCompare(b.name));
  return { total: hits.length, items: hits.slice(offset, offset + limit).map(summary) };
}

/** The people of the demo's contacts.csv, exported from Gmail; Maya Chen is in an address book already. */
export const csvContacts: Omit<T.ContactInput, 'bookId'>[] = [
  { id: null, name: 'Maya Chen', givenName: 'Maya', familyName: 'Chen', emails: [{ value: 'maya@northwind.design', label: 'work' }], phones: [], addresses: [], urls: [], organization: 'Northwind Design', title: null, birthday: null, notes: null, photo: null },
  { id: null, name: 'Jonas Lindqvist', givenName: 'Jonas', familyName: 'Lindqvist', emails: [{ value: 'jonas@fjord.example', label: 'work' }], phones: [{ value: '+46 70 123 45 67', label: 'mobile' }], addresses: [], urls: [], organization: 'Fjord Studio', title: 'Creative Director', birthday: null, notes: null, photo: null },
  { id: null, name: 'Amara Nwosu', givenName: 'Amara', familyName: 'Nwosu', emails: [{ value: 'amara@lagoslabs.example', label: 'work' }], phones: [], addresses: [], urls: ['https://lagoslabs.example'], organization: 'Lagos Labs', title: 'Founder', birthday: null, notes: null, photo: null },
];

/** How much a contact holds, to keep the fullest one of duplicates. */
const filled = (c: T.Contact) =>
  c.emails.length + c.phones.length + c.addresses.length + c.urls.length + [c.organization, c.title, c.birthday, c.notes, c.photo].filter(Boolean).length;

/** People in an address book more than once, by e-mail address: the fullest first. */
export function duplicates(): T.ContactSummary[][] {
  const groups = new Map<string, T.Contact[]>();
  for (const c of contacts) {
    for (const e of c.emails) {
      const key = `${c.bookId} ${e.value.toLowerCase()}`;
      const group = groups.get(key) ?? [];
      if (!group.includes(c)) group.push(c);
      groups.set(key, group);
    }
  }
  return [...groups.values()].filter((g) => g.length > 1).map((g) => [...g].sort((a, b) => filled(b) - filled(a)).map(summary));
}

/** Merges contacts into the first; returns how many went into it. */
export function merge(ids: number[]): number {
  const [keep, ...rest] = ids.map((id) => contacts.find((c) => c.id === id)).filter((c): c is T.Contact => !!c);
  if (!keep) return 0;
  for (const other of rest) {
    // The groups the duplicate was in have the contact kept instead.
    for (const g of groups) {
      if (!g.members.includes(other.id)) continue;
      g.members = g.members.filter((m) => m !== other.id);
      if (!g.members.includes(keep.id)) g.members.push(keep.id);
    }
    for (const e of other.emails) if (!keep.emails.some((m) => m.value.toLowerCase() === e.value.toLowerCase())) keep.emails.push(e);
    for (const p of other.phones) if (!keep.phones.some((m) => m.value.replace(/\D/g, '') === p.value.replace(/\D/g, ''))) keep.phones.push(p);
    keep.organization ??= other.organization;
    keep.title ??= other.title;
    keep.birthday ??= other.birthday;
    keep.notes ??= other.notes;
    keep.photo ??= other.photo;
    remove(other.id);
  }
  return rest.length;
}

export function save(input: T.ContactInput): number {
  if (input.id != null) {
    const i = contacts.findIndex((c) => c.id === input.id);
    if (i >= 0) contacts[i] = { ...input, id: input.id };
    return input.id;
  }
  const c = { ...input, id: seq++ };
  contacts.push(c);
  return c.id;
}

export function remove(id: number) {
  const i = contacts.findIndex((c) => c.id === id);
  if (i >= 0) contacts.splice(i, 1);
}

// ----- Groups ------------------------------------------------------------------

/** A demo group: its people by contact id, and members that are only an address. */
interface Group {
  id: number;
  bookId: number;
  name: string;
  members: number[];
  emails: string[];
}

const idOf = (name: string) => contacts.find((c) => c.name === name)?.id ?? 0;

// Design team is a label of the Google address book (read-only, as Google's labels are in
// Envelope); Family is a group of the iCloud one.
const groups: Group[] = [
  { id: 901, bookId: 1, name: 'Design team', members: ['Sofia Martínez', 'Priya Raman', 'Daniel Okafor', 'Maya Chen', 'Olivia Brooks'].map(idOf), emails: [] },
  { id: 902, bookId: 2, name: 'Family', members: ['Linda Morgan', 'Elif Yılmaz', 'Noah Williams'].map(idOf), emails: [] },
];
let groupSeq = 903;

/** Groups of address books on this computer and on CardDAV servers can be changed. */
const groupsEditable = (bookId: number) => {
  const b = books.find((x) => x.id === bookId);
  return !!b && !b.readOnly && (b.kind === 'local' || b.kind === 'carddav');
};

const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

function view(g: Group): T.ContactGroup {
  const people: T.GroupMember[] = g.members
    .map((id) => contacts.find((c) => c.id === id && c.bookId === g.bookId))
    .filter((c): c is T.Contact => !!c)
    .map((c) => ({ contactId: c.id, name: c.name, email: c.emails[0]?.value ?? null, organization: c.organization, photo: c.photo }));
  const addresses: T.GroupMember[] = g.emails.map((e) => ({ contactId: null, name: e, email: e, organization: null, photo: null }));
  return {
    id: g.id,
    bookId: g.bookId,
    name: g.name,
    readOnly: !groupsEditable(g.bookId),
    members: [...people, ...addresses].sort((a, b) => a.name.localeCompare(b.name)),
  };
}

function editable(id: number): Group {
  const g = groups.find((x) => x.id === id);
  if (!g) throw { message: 'Group not found', kind: 'notFound' };
  if (!groupsEditable(g.bookId)) throw { message: 'This group can’t be changed here.', kind: 'invalid' };
  return g;
}

function checkName(bookId: number, name: string, except?: number): string {
  const n = name.trim();
  if (!n) throw { message: 'Enter a name.', kind: 'invalid' };
  if (groups.some((g) => g.bookId === bookId && g.id !== except && fold(g.name) === fold(n))) {
    throw { message: `There’s already a group called “${n}” in this address book.`, kind: 'invalid' };
  }
  return n;
}

/** Every group, in the address books' order and by name. */
export function groupList(): T.ContactGroup[] {
  return books.flatMap((b) =>
    groups
      .filter((g) => g.bookId === b.id)
      .map(view)
      .sort((x, y) => x.name.localeCompare(y.name)),
  );
}

export function createGroup(bookId: number, name: string, ids: number[]): number {
  if (!groupsEditable(bookId)) throw { message: 'Groups can’t be added to this address book.', kind: 'invalid' };
  const g: Group = { id: groupSeq++, bookId, name: checkName(bookId, name), members: [], emails: [] };
  groups.push(g);
  addToGroup(g.id, ids, []);
  return g.id;
}

export function renameGroup(id: number, name: string) {
  const g = editable(id);
  g.name = checkName(g.bookId, name, id);
}

export function deleteGroup(id: number) {
  editable(id);
  groups.splice(
    groups.findIndex((g) => g.id === id),
    1,
  );
}

export function addToGroup(id: number, ids: number[], emails: string[]): number {
  const g = editable(id);
  let added = 0;
  for (const c of ids.map((i) => contacts.find((x) => x.id === i && x.bookId === g.bookId))) {
    if (c && !g.members.includes(c.id)) {
      g.members.push(c.id);
      added++;
    }
  }
  for (const e of emails) {
    if (!g.emails.some((x) => x.toLowerCase() === e.toLowerCase())) {
      g.emails.push(e);
      added++;
    }
  }
  return added;
}

export function removeFromGroup(id: number, ids: number[], emails: string[]): number {
  const g = editable(id);
  const before = g.members.length + g.emails.length;
  g.members = g.members.filter((m) => !ids.includes(m));
  g.emails = g.emails.filter((e) => !emails.some((x) => x.toLowerCase() === e.toLowerCase()));
  return before - g.members.length - g.emails.length;
}

/** Groups with a word of their name starting with `query`, with their members' addresses. */
export function suggestGroups(query: string): T.GroupSuggestion[] {
  const q = fold(query.trim());
  if (!q) return [];
  return groupList()
    .filter((g) => fold(g.name).startsWith(q) || fold(g.name).split(/[^\p{L}\p{N}]+/u).some((w) => w.startsWith(q)))
    .map((g) => ({
      id: g.id,
      name: g.name,
      count: g.members.length,
      addresses: g.members.filter((m) => m.email).map((m) => ({ name: m.contactId != null ? m.name : null, email: m.email! })),
    }))
    .filter((s) => s.addresses.length)
    .slice(0, 4);
}

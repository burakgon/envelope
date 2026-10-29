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

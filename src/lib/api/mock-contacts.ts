// Demo address books and contacts for the browser demo.

import type * as T from './types';

export const books: T.ContactBook[] = [
  { id: 1, accountId: 1, kind: 'google', name: 'Work contacts', readOnly: false },
  { id: 2, accountId: 2, kind: 'carddav', name: 'iCloud', readOnly: false },
];

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
  C(2, 'Linda Morgan', 'linda.morgan@icloud.test', { emails: [{ value: 'linda.morgan@icloud.test', label: 'home' }], phones: [{ value: '+1 503 555 0199', label: 'home' }], notes: 'Mom 💛', birthday: '1961-11-06' }),
  C(2, 'Hannah Becker', 'hannah.becker@mail.test', { title: 'Landlord', phones: [{ value: '+49 151 2345 6789', label: 'mobile' }] }),
  C(2, 'Arjun Patel', 'arjun@patel.dev', { organization: 'Freelance', title: 'iOS developer' }),
  C(2, 'Zoe Garcia', 'zoe.garcia@umbrella.org', { organization: 'Umbrella', title: 'Researcher' }),
  C(1, 'Carlos Rossi', 'carlos@globex.com', { organization: 'Globex', title: 'Account Executive', phones: [{ value: '+39 02 555 0110', label: 'work' }] }),
  C(1, 'Aisha Haddad', 'aisha@initech.net', { organization: 'Initech', title: 'CTO' }),
  C(2, 'Ben Cohen', 'ben.cohen@hooli.xyz', { organization: 'Hooli' }),
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

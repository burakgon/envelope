// Fictional sample mailbox used by the in-browser demo. All people,
// companies and products are made up.

import type { Account, Address, Folder, MessageView, ThreadSummary } from './types';

const now = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

export const me = { name: 'Alex Morgan', work: 'alex@lumen.dev', home: 'alex.morgan@icloud.com' };

export const accounts: Account[] = [
  {
    id: 1, kind: 'gmail', provider: 'gmail', email: me.work, name: me.name, label: 'Work', color: '#6d5efc',
    sortOrder: 0, enabled: true, hasCalendar: true, hasContacts: true, sync: { state: 'idle' },
    config: { auth: 'oAuthGoogle', imap: null, smtp: null, caldavUrl: null, carddavUrl: null, acceptInvalidCerts: false, saveSentCopy: null, bodySyncDays: null },
  },
  {
    id: 2, kind: 'imap', provider: 'icloud', email: me.home, name: me.name, label: 'Personal', color: '#0ea5e9',
    sortOrder: 1, enabled: true, hasCalendar: true, hasContacts: true, sync: { state: 'idle' },
    config: { auth: 'password', imap: null, smtp: null, caldavUrl: 'https://caldav.icloud.com', carddavUrl: null, acceptInvalidCerts: false, saveSentCopy: null, bodySyncDays: null },
  },
];

let folderId = 1;
function folder(accountId: number, name: string, role: Folder['role'], kind: Folder['kind'] = 'folder', color: string | null = null): Folder {
  return { id: folderId++, accountId, name, path: name, parentId: null, role, kind, unreadCount: 0, totalCount: 0, color, hidden: false, selectable: true, sortOrder: 0 };
}

export const folders: Folder[] = [
  folder(1, 'Inbox', 'inbox'),
  folder(1, 'Starred', 'starred'),
  folder(1, 'Sent', 'sent'),
  folder(1, 'Drafts', 'drafts'),
  folder(1, 'All Mail', 'all'),
  folder(1, 'Spam', 'junk'),
  folder(1, 'Trash', 'trash'),
  folder(1, 'Design', null, 'label', '#ec4899'),
  folder(1, 'Hiring', null, 'label', '#f59e0b'),
  folder(1, 'Releases', null, 'label', '#10b981'),
  folder(2, 'Inbox', 'inbox'),
  folder(2, 'Sent', 'sent'),
  folder(2, 'Drafts', 'drafts'),
  folder(2, 'Archive', 'archive'),
  folder(2, 'Junk', 'junk'),
  folder(2, 'Trash', 'trash'),
  folder(2, 'Travel', null),
  folder(2, 'Receipts', null),
];

export function folderOf(accountId: number, role: Folder['role'] | string): Folder {
  return folders.find((f) => f.accountId === accountId && (f.role === role || f.name === role))!;
}

const P: Record<string, Address> = {
  maya: { name: 'Maya Chen', email: 'maya@northwind.design' },
  daniel: { name: 'Daniel Okafor', email: 'daniel@lumen.dev' },
  priya: { name: 'Priya Raman', email: 'priya@lumen.dev' },
  sofia: { name: 'Sofia Martínez', email: 'sofia@lumen.dev' },
  olivia: { name: 'Olivia Brooks', email: 'olivia@lumen.dev' },
  kenji: { name: 'Kenji Tanaka', email: 'kenji@tanaka.jp' },
  elif: { name: 'Elif Yılmaz', email: 'elif@yilmaz.family' },
  lukas: { name: 'Lukas Weber', email: 'lukas.weber@kraftwerk-studio.de' },
  noah: { name: 'Noah Williams', email: 'noah.w@fastmail.test' },
  mom: { name: 'Mom', email: 'linda.morgan@icloud.test' },
  meWork: { name: me.name, email: me.work },
  meHome: { name: me.name, email: me.home },
  forge: { name: 'Gitforge', email: 'notifications@gitforge.test' },
  track: { name: 'Tracklane', email: 'updates@tracklane.test' },
  pay: { name: 'Paystream', email: 'receipts@paystream.test' },
  sky: { name: 'Skyway Airlines', email: 'bookings@skyway-air.test' },
  byte: { name: 'The Daily Byte', email: 'hello@dailybyte.test' },
  rust: { name: 'Rust Weekly', email: 'editor@rustweekly.test' },
  figtree: { name: 'Figtree', email: 'comments@figtree.test' },
  bank: { name: 'Northstar Bank', email: 'statements@northstar.test' },
  wave: { name: 'Soundwave', email: 'mixes@soundwave.test' },
  gym: { name: 'Pulse Fitness', email: 'hello@pulsefit.test' },
  landlord: { name: 'Hannah Becker', email: 'hannah.becker@mail.test' },
  security: { name: 'Lumen Account', email: 'security@lumen.dev' },
};

function card(inner: string, accent = '#6d5efc'): string {
  return `<div style="background:#f4f4f7;padding:28px 0;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#1f2330">
<div style="max-width:560px;margin:0 auto;background:#fff;border-radius:14px;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.06)">
<div style="height:6px;background:${accent}"></div><div style="padding:30px 36px;font-size:15px;line-height:1.6">${inner}</div></div></div>`;
}

function gradientImage(c1: string, c2: string, label: string): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="560" height="300"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${c1}"/><stop offset="1" stop-color="${c2}"/></linearGradient></defs><rect width="560" height="300" rx="14" fill="url(#g)"/><rect x="36" y="40" width="200" height="220" rx="14" fill="#fff" opacity=".92"/><rect x="56" y="64" width="120" height="14" rx="7" fill="#c9c4ff"/><rect x="56" y="92" width="160" height="10" rx="5" fill="#e6e3ff"/><rect x="56" y="112" width="140" height="10" rx="5" fill="#e6e3ff"/><rect x="56" y="200" width="160" height="36" rx="10" fill="${c1}"/><text x="262" y="140" font-family="Inter,Arial" font-size="22" font-weight="700" fill="#fff">${label}</text><text x="262" y="170" font-family="Inter,Arial" font-size="14" fill="#fff" opacity=".8">Mail that keeps up with you</text></svg>`;
  return `data:image/svg+xml;base64,${btoa(svg)}`;
}

interface MsgSeed {
  from: Address;
  to: Address[];
  cc?: Address[];
  at: number;
  text?: string;
  html?: string;
  seen?: boolean;
  flagged?: boolean;
  attachments?: { filename: string; mimeType: string; size: number }[];
  unsubscribe?: boolean;
  invite?: boolean;
  /** A draft stored on the server (written on another computer, say). */
  draft?: boolean;
}

interface ThreadSeed {
  account: number;
  folders: string[];
  subject: string;
  category?: 'people' | 'newsletters' | 'notifications';
  messages: MsgSeed[];
}

const work = (f: Address[]) => f;
const T = (account: number, folders: string[], subject: string, messages: MsgSeed[], category: ThreadSeed['category'] = 'people'): ThreadSeed => ({ account, folders, subject, messages, category });

const seeds: ThreadSeed[] = [
  T(1, ['inbox', 'Design'], 'New onboarding flow — first look', [
    {
      from: P.maya, to: [P.meWork], at: now - 14 * MIN, seen: false, flagged: true,
      html: `<p>Hey Alex!</p><p>Here's the first pass at the new onboarding. I simplified the account step to a single screen and added a provider picker with auto-detection — paste an address and we figure out the rest.</p>
<p><img src="${gradientImage('#6d5efc', '#ec4899', 'Welcome to Envelope')}" width="520" style="border-radius:12px;max-width:100%" alt="Onboarding preview"></p>
<p>Open questions:</p><ul><li>Do we show the AI step during onboarding or after the first sync?</li><li>Dark mode by default when the system theme is dark?</li></ul><p>Would love your notes before Friday 🙌</p><p>— Maya</p>`,
    },
  ]),
  T(1, ['inbox'], 'Q4 roadmap — final review', [
    { from: P.priya, to: [P.meWork, P.daniel], at: now - 2 * DAY - 5 * HOUR, text: 'Hi both,\n\nAttached is the draft Q4 roadmap. The big bets are offline search, the new composer and calendar sharing.\n\nCan we lock priorities by Thursday?\n\nPriya', attachments: [{ filename: 'Q4-Roadmap.pdf', mimeType: 'application/pdf', size: 482_113 }] },
    { from: P.daniel, to: [P.priya, P.meWork], at: now - 2 * DAY - 2 * HOUR, text: "Looks solid. I'd move calendar sharing to Q1 — the sync rewrite has to land first.\n\nDaniel" },
    { from: P.meWork, to: [P.priya, P.daniel], at: now - 1 * DAY - 20 * HOUR, text: "Agree with Daniel. Offline search is the one users keep asking for, let's make it the headline.\n\nAlex" },
    { from: P.priya, to: [P.meWork, P.daniel], at: now - 47 * MIN, seen: false, text: "Great — updated the doc with both of your notes. Final review tomorrow at 10:00, I'll send an invite.\n\nPriya" },
  ]),
  T(1, ['inbox'], 'Invitation: Product sync @ Thu 10:00', [
    { from: P.daniel, to: [P.meWork, P.priya, P.sofia], at: now - 2 * HOUR, seen: false, invite: true, text: 'Daniel Okafor has invited you to Product sync.\nThursday 10:00 – 10:45 · meet.lumen.dev/product-sync' },
  ]),
  T(1, ['inbox', 'Releases'], '[lumen/envelope] Release v0.1 checklist (#151)', [
    { from: P.forge, to: [P.meWork], at: now - 3 * HOUR, seen: false, text: "Sofia Martínez requested your review on this pull request.\n\n✓ Packaging for deb, rpm and AppImage\n✓ Flatpak manifest\n○ Release notes\n\n— View it on Gitforge" },
  ], 'notifications'),
  T(1, ['inbox', 'Hiring'], 'Interview loop for Senior Rust Engineer', [
    { from: P.daniel, to: [P.meWork, P.olivia], at: now - 5 * HOUR, text: "Hi Alex, Olivia —\n\nWe have three strong candidates for the Rust role. Could you each take one system design interview next week? I've put the scorecards in the hiring folder.\n\nThanks!\nDaniel", attachments: [{ filename: 'Scorecard.xlsx', mimeType: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', size: 38_220 }] },
  ]),
  T(1, ['inbox'], 'Offsite planning 🏔️', [
    { from: P.olivia, to: [P.meWork, P.daniel, P.priya, P.sofia], at: now - 8 * DAY, text: 'Team! Offsite is happening. Two options:\n\n1. Cappadocia — 3 days, hot air balloons\n2. Black Forest — cabins + hiking\n\nVote by Friday!' },
    { from: P.daniel, to: [P.olivia], at: now - 8 * DAY + 2 * HOUR, text: 'Cappadocia, obviously.' },
    { from: P.sofia, to: [P.olivia], at: now - 7 * DAY, text: "+1 Cappadocia! I can look into flights." },
    { from: P.meWork, to: [P.olivia], at: now - 7 * DAY + 3 * HOUR, text: 'Cappadocia from me too. Happy to organise the balloon booking.' },
    { from: P.priya, to: [P.olivia], at: now - 6 * DAY, text: "Black Forest has better wifi 😅 but I'm outvoted. Cappadocia it is!" },
    { from: P.olivia, to: [P.meWork, P.daniel, P.priya, P.sofia], at: now - 6 * HOUR, seen: false, text: 'Decided: Cappadocia, 21–23 November. Flights are booked — itinerary attached. 🎈', attachments: [{ filename: 'Cappadocia-itinerary.pdf', mimeType: 'application/pdf', size: 1_204_331 }] },
  ]),
  T(1, ['inbox', 'Design'], 'Design system tokens v2', [
    { from: P.sofia, to: [P.meWork], cc: [P.maya], at: now - 1 * DAY - 2 * HOUR, text: 'I pushed the new color tokens. Accent is now a single hue variable so themes stay consistent.' },
    { from: P.maya, to: [P.sofia, P.meWork], at: now - 1 * DAY, text: 'Love it. Dark mode contrast is way better.' },
    { from: P.meWork, to: [P.sofia, P.maya], at: now - 22 * HOUR, text: 'Merged. Thanks both — this is a big upgrade.' },
  ]),
  T(1, ['inbox'], '3 issues were assigned to you', [
    { from: P.track, to: [P.meWork], at: now - 9 * HOUR, text: 'ENV-412 Search: support larger: and smaller: operators\nENV-418 Calendar: drag to resize events\nENV-421 Composer: paste images inline', seen: true },
  ], 'notifications'),
  T(1, ['inbox'], 'Maya commented on “Composer — states”', [
    { from: P.figtree, to: [P.meWork], at: now - 10 * HOUR, text: '“Can we make the send button morph into the undo toast? Feels magical.” — Maya Chen', seen: true },
  ], 'notifications'),
  T(1, ['inbox'], 'Tokyo office visit — 東京オフィス', [
    { from: P.kenji, to: [P.meWork], at: now - 1 * DAY - 8 * HOUR, text: 'Hi Alex,\n\nLooking forward to your visit next month! The office is in the new Shibuya building, I booked the big meeting room for Tuesday.\n\nアレックスさん、お会いできるのを楽しみにしています。\n\nKenji', seen: true },
  ]),
  T(1, ['inbox'], 'Your receipt from Lumen Cloud #1043-2201', [
    { from: P.pay, to: [P.meWork], at: now - 3 * DAY, seen: true, html: card(`<p style="color:#6b7080;margin:0">Receipt #1043-2201</p><h1 style="margin:6px 0 4px;font-size:32px">$24.00</h1><p style="margin:0 0 18px">Lumen Cloud Pro · monthly</p><table width="100%" style="font-size:14px;border-collapse:collapse"><tr><td style="padding:8px 0;border-top:1px solid #eee">Pro plan</td><td align="right" style="border-top:1px solid #eee">$24.00</td></tr><tr><td style="padding:8px 0;border-top:1px solid #eee;color:#6b7080">Paid with</td><td align="right" style="border-top:1px solid #eee;color:#6b7080">Visa •••• 4242</td></tr></table>`, '#10b981') },
  ], 'notifications'),
  T(1, ['inbox'], 'New sign-in from Firefox on Linux', [
    { from: P.security, to: [P.meWork], at: now - 11 * HOUR, seen: true, text: 'We noticed a new sign-in to your Lumen account from Firefox on Linux (Istanbul, TR). If this was you, you can ignore this email.' },
  ], 'notifications'),
  T(1, ['inbox'], 'This week in Rust #612', [
    { from: P.rust, to: [P.meWork], at: now - 2 * DAY - 9 * HOUR, seen: true, unsubscribe: true, html: card(`<h2 style="margin:0 0 8px">This week in Rust #612</h2><p>Hello and welcome to another issue! This week: async closures everywhere, a faster linker, and why your SQLite queries might be faster than you think.</p><h3>Updates from the community</h3><ul><li>Writing a mail client in 2026</li><li>Zero-copy parsing with nom 8</li><li>GPU text rendering on Wayland</li></ul>`, '#f97316') },
  ], 'newsletters'),
  T(1, ['Sent'], 'Notes from today', [
    { from: P.meWork, to: [P.daniel], at: now - 4 * DAY, text: 'Hi Daniel,\n\nSharing my notes from the planning session. Happy to walk through them tomorrow.\n\nAlex' },
  ]),

  T(2, ['inbox'], 'Hafta sonu planı?', [
    { from: P.elif, to: [P.meHome], at: now - 1 * DAY - 6 * HOUR, text: "Selam Alex!\n\nBu hafta sonu Kadıköy'de yeni açılan kahveciye gidelim mi? Cumartesi 11 gibi uygun musun?\n\nSevgiler,\nElif" },
    { from: P.meHome, to: [P.elif], at: now - 1 * DAY - 4 * HOUR, text: 'Selam Elif, harika fikir! Cumartesi 11 bana uyar. Görüşürüz ☕' },
    { from: P.elif, to: [P.meHome], at: now - 36 * MIN, seen: false, text: 'Süper! Adresi atıyorum: Moda Cd. No: 12. Rezervasyon yaptım 🙂' },
  ]),
  T(2, ['inbox', 'Travel'], 'Your trip to Berlin is confirmed — SKY7Q2', [
    {
      from: P.sky, to: [P.meHome], at: now - 3 * HOUR - 20 * MIN, seen: false,
      html: card(`<h2 style="margin:0 0 4px">You're all set, Alex ✈️</h2><p style="color:#6b7080;margin:0 0 22px">Booking reference <b style="color:#1f2330">SKY7Q2</b></p>
<table width="100%" style="border-collapse:collapse;font-size:15px"><tr><td style="padding:14px 0;border-bottom:1px solid #eee"><b>IST → BER</b><br><span style="color:#6b7080">Thu, 14 Nov · 09:40 – 11:55</span></td><td align="right" style="border-bottom:1px solid #eee">Seat <b>12A</b></td></tr>
<tr><td style="padding:14px 0"><b>BER → IST</b><br><span style="color:#6b7080">Sun, 17 Nov · 18:20 – 22:30</span></td><td align="right">Seat <b>14C</b></td></tr></table>
<p style="margin:24px 0 0"><a href="https://skyway-air.test/manage/SKY7Q2" style="display:inline-block;background:#0ea5e9;color:#fff;padding:12px 20px;border-radius:10px;text-decoration:none;font-weight:600">Manage booking</a></p>`, '#0ea5e9'),
    },
  ], 'notifications'),
  T(2, ['inbox'], 'Photos from the weekend 📸', [
    { from: P.mom, to: [P.meHome], at: now - 5 * HOUR, seen: false, text: "Hi sweetie,\n\nHere are the photos from Sunday. Your dad insists the blurry one is art.\n\nLove,\nMom", attachments: [{ filename: 'IMG_2041.jpg', mimeType: 'image/jpeg', size: 2_834_112 }, { filename: 'IMG_2044.jpg', mimeType: 'image/jpeg', size: 3_101_455 }, { filename: 'IMG_2052.jpg', mimeType: 'image/jpeg', size: 2_455_870 }] },
  ]),
  T(2, ['inbox'], 'Angebot für das neue Projekt', [
    { from: P.lukas, to: [P.meHome], at: now - 4 * DAY, flagged: true, text: 'Hallo Alex,\n\nanbei unser Angebot für den Relaunch. Wir könnten Anfang Dezember starten.\n\nViele Grüße\nLukas Weber\nKraftwerk Studio', attachments: [{ filename: 'Angebot-Relaunch.pdf', mimeType: 'application/pdf', size: 233_910 }] },
  ]),
  T(2, ['inbox', 'Drafts'], 'Apartment viewing on Saturday', [
    { from: P.landlord, to: [P.meHome], at: now - 2 * DAY - 3 * HOUR, text: 'Hi Alex,\n\nThe apartment on Rosenthaler Str. is available for a viewing this Saturday at 14:00. Let me know if that works.\n\nBest,\nHannah' },
    { from: P.meHome, to: [P.landlord], at: now - 2 * DAY - 2 * HOUR, draft: true, html: '<p>Hi Hannah,</p><p>Saturday at 14:00 works for me. Could I bring a friend along?</p>' },
  ]),
  T(1, ['Drafts'], 'Talk outline for the meetup', [
    {
      from: P.meWork, to: [P.maya], cc: [P.sofia], at: now - 4 * HOUR, draft: true,
      html: '<p>Hi Maya,</p><p>Here is the outline for Thursday. I kept the demo short so there is time for questions.</p><ul><li>Why another mail client</li><li>Offline search</li><li>Live demo</li></ul>',
      attachments: [{ filename: 'talk-outline.pdf', mimeType: 'application/pdf', size: 184_220 }],
    },
  ]),
  T(2, ['inbox'], 'Re: Book recommendations', [
    { from: P.noah, to: [P.meHome], at: now - 6 * DAY, text: 'Ha, I knew you\'d like it. Next up: "The Pragmatic Programmer", then we argue about it over dinner.\n\nOn Mon, Alex Morgan wrote:\n> Finished "A Philosophy of Software Design" — what next?' },
  ]),
  T(2, ['inbox'], 'The Daily Byte: The quiet comeback of desktop apps', [
    { from: P.byte, to: [P.meHome], at: now - 7 * HOUR, seen: false, unsubscribe: true, html: card(`<p style="color:#b45309;font-weight:600;margin:0 0 6px;font-size:13px;letter-spacing:.06em">THE DAILY BYTE</p><h1 style="font-size:26px;line-height:1.25;margin:0 0 12px">The quiet comeback of desktop apps</h1><p>Native is back. We look at the tools making fast, beautiful desktop software fun to build again — and why Linux users are suddenly spoiled for choice.</p><p><a href="https://dailybyte.test/read" style="color:#b45309;font-weight:600">Read the full issue →</a></p>`, '#f59e0b') },
  ], 'newsletters'),
  // A link whose text names another site than the one it opens (the reader warns).
  T(2, ['inbox'], 'Action needed: confirm your account details', [
    {
      from: { name: 'Northstar Bank', email: 'service@northstar-verify.test' }, to: [P.meHome], at: now - 3 * DAY - 7 * HOUR, seen: true,
      html: card(`<h2 style="margin:0 0 8px">Please confirm your details</h2><p>We could not verify your account. Confirm your details within 24 hours to keep online banking.</p><p style="margin:22px 0 0"><a href="https://northstar-verify.test/login?id=8821" style="color:#0f766e;font-weight:600">https://northstar.test/secure</a></p>`, '#0f766e'),
    },
  ], 'notifications'),
  T(2, ['inbox'], 'Your October statement is ready', [
    { from: P.bank, to: [P.meHome], at: now - 3 * DAY - 4 * HOUR, seen: true, text: 'Your October statement for account ending 0918 is now available in online banking.' },
  ], 'notifications'),
  // Spam, to show "Not spam".
  T(2, ['junk'], 'You have been selected for a $500 gift card', [
    { from: { name: 'Rewards Center', email: 'prize@winner-rewards.test' }, to: [P.meHome], at: now - 9 * HOUR, seen: false, text: 'Congratulations! You have been selected. Claim your $500 gift card within 24 hours — supplies are limited.' },
  ], 'notifications'),
  T(1, ['junk'], 'Your wallet is eligible for 2,500 free tokens', [
    { from: { name: 'Token Bonus', email: 'airdrop@token-bonus.test' }, to: [P.meWork], at: now - 1 * DAY - 3 * HOUR, seen: false, text: 'Connect your wallet today to receive 2,500 tokens. This offer expires soon.' },
  ], 'notifications'),
  T(2, ['inbox'], 'Your Weekly Mix is here 🎧', [
    { from: P.wave, to: [P.meHome], at: now - 4 * DAY - 2 * HOUR, seen: true, unsubscribe: true, html: card(`<h2 style="margin:0 0 8px">Fresh picks for your week</h2><p>30 new songs, handpicked for you. Starting with a little synthwave for your Monday.</p>`, '#22c55e') },
  ], 'newsletters'),
  T(2, ['inbox'], 'Your membership renews soon', [
    { from: P.gym, to: [P.meHome], at: now - 5 * DAY, seen: true, text: 'Your Pulse Fitness membership renews on November 3. No action needed.' },
  ], 'notifications'),
];

// Long tail so lists scroll realistically.
const fillerSubjects = ['Weekly sync notes', 'Quick question about the API', 'Lunch next week?', 'Feedback on the landing copy', 'Release checklist', 'Conference talk proposal', 'Bug bash results', 'Invoice for September', 'Re: Contract renewal', 'Slides from the meetup'];
const fillerPeople = [P.maya, P.daniel, P.priya, P.sofia, P.olivia, P.noah, P.lukas, P.kenji];
for (let i = 0; i < 46; i++) {
  const who = fillerPeople[i % fillerPeople.length];
  const acct = i % 3 === 0 ? 2 : 1;
  seeds.push(T(acct, [i % 4 === 0 ? 'Archive' : 'inbox'], fillerSubjects[i % fillerSubjects.length], [
    { from: who, to: [acct === 1 ? P.meWork : P.meHome], at: now - (6 + i * 0.9) * DAY, seen: true, text: `Hi Alex,\n\n${fillerSubjects[i % fillerSubjects.length]} — details below. Let me know if anything is unclear.\n\nThanks!\n${who.name?.split(' ')[0]}` },
  ]));
}

export const threads: ThreadSummary[] = [];
export const messages: MessageView[] = [];

let tid = 1;
let mid = 1;
let aid = 1;
for (const s of seeds) {
  const id = tid++;
  const folderIds = s.folders
    .map((f) => (f === 'inbox' || f === 'Sent' || f === 'Archive' ? folderOf(s.account, f === 'inbox' ? 'inbox' : f === 'Sent' ? 'sent' : (s.account === 1 ? 'all' : 'archive')) : folderOf(s.account, f)))
    .filter(Boolean)
    .map((f) => f.id);
  if (s.account === 1) folderIds.push(folderOf(1, 'all').id);
  const msgs = s.messages.map((m) => {
    const flags = { seen: m.seen ?? true, flagged: m.flagged ?? false, answered: false, draft: m.draft ?? false, forwarded: false };
    const text = m.text ?? null;
    const view: MessageView = {
      id: mid++, threadId: id, accountId: s.account, folderIds: [...new Set(folderIds)], messageId: `${mid}@demo`, inReplyTo: null, references: [],
      subject: s.subject, from: m.from, to: m.to, cc: m.cc ?? [], bcc: [], replyTo: [], date: m.at, flags,
      snippet: (text ?? (m.html ?? '').replace(/<[^>]+>/g, ' ')).replace(/\s+/g, ' ').trim().slice(0, 180),
      size: 12_000, hasAttachments: !!m.attachments?.length,
      attachments: (m.attachments ?? []).map((a) => ({ id: aid++, filename: a.filename, mimeType: a.mimeType, size: a.size, contentId: null, inline: false })),
      body: { html: m.html ?? null, text },
      unsubscribe: m.unsubscribe ? { url: 'https://example.test/unsubscribe', mailto: null, oneClick: true } : null,
      calendarMethod: m.invite ? 'REQUEST' : null,
      isAuto: s.category === 'notifications',
    };
    return view;
  });
  messages.push(...msgs);
  const last = msgs[msgs.length - 1];
  const senders: Address[] = [];
  for (const m of msgs) if (m.from && !senders.some((x) => x.email === m.from!.email)) senders.push(m.from);
  threads.push({
    id, accountId: s.account, subject: s.subject, snippet: last.snippet, lastDate: last.date,
    messageCount: msgs.length, unreadCount: msgs.filter((m) => !m.flags.seen).length,
    flagged: msgs.some((m) => m.flags.flagged), hasAttachments: msgs.some((m) => m.hasAttachments),
    draftCount: msgs.filter((m) => m.flags.draft).length, participants: senders, lastSender: last.from, folderIds: [...new Set(folderIds)], category: s.category ?? 'people',
    isInvite: msgs.some((m) => m.calendarMethod === 'REQUEST'), muted: false, snoozedUntil: null,
  });
}

export const contacts: Address[] = Object.values(P).filter((p) => !p.email.includes('notifications') && !p.email.includes('noreply'));

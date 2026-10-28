// OpenPGP for the browser demo: a few made-up keys, no real cryptography.

import type { Api } from './index';
import type * as T from './types';
import * as data from './mockdata';

const DAY = 86_400_000;

function fingerprint(seed: string): string {
  let h = 0x811c9dc5;
  let out = '';
  for (let round = 0; out.length < 40; round++) {
    for (const ch of `${seed}:${round}`) h = Math.imul(h ^ ch.charCodeAt(0), 0x01000193) >>> 0;
    out += h.toString(16).toUpperCase().padStart(8, '0');
  }
  return out.slice(0, 40);
}

function key(email: string, name: string | null | undefined, source: T.KeySource, extra: Partial<T.KeyInfo> = {}): T.KeyInfo {
  const created = Date.now() - 400 * DAY;
  return {
    fingerprint: fingerprint(email + source),
    userIds: [name ? `${name} <${email}>` : `<${email}>`],
    emails: [email.toLowerCase()],
    algorithm: 'Ed25519',
    createdAt: created,
    expiresAt: null,
    revoked: false,
    expired: false,
    canEncrypt: true,
    canSign: true,
    secret: source === 'own',
    protected: false,
    source,
    verified: false,
    addedAt: created,
    updatedAt: created,
    ...extra,
  };
}

const ARMOR = (kind: string, fp: string) =>
  `-----BEGIN PGP ${kind} KEY BLOCK-----\n\nmDMEZdemo${fp.slice(0, 24)}\n=demo\n-----END PGP ${kind} KEY BLOCK-----\n`;

export function createMockCrypto(identities: () => T.Identity[]): Api['crypto'] {
  let settings: T.CryptoSettings = {
    encryptWhenPossible: false,
    signByDefault: false,
    sendAutocrypt: true,
    protectSubject: true,
    keepDecrypted: false,
    wkdLookup: true,
  };
  const own = new Map<string, T.KeyInfo>();
  own.set(data.me.work, key(data.me.work, data.me.name, 'own'));
  const peers: T.KeyInfo[] = data.contacts
    .slice(0, 4)
    .map((c, i) => key(c.email, c.name, i === 0 ? 'manual' : 'autocrypt', { verified: i === 0 }));
  const all = () => [...own.values(), ...peers];
  const find = (fp: string) => all().find((k) => k.fingerprint === fp);
  const forEmail = (email: string) => all().find((k) => k.emails.includes(email.toLowerCase()) && k.canEncrypt);
  const demo = (): never => {
    throw { message: 'Not available in the demo', kind: 'invalid' };
  };

  // A signed message from the first contact with a key, to show the badge.
  const signed = data.messages.find((m) => m.from && peers[0]?.emails.includes(m.from.email.toLowerCase()));
  if (signed) signed.crypto = 'signed';

  const clone = <V>(v: V): Promise<V> => Promise.resolve(structuredClone(v));

  return {
    settings: () => clone(settings),
    setSettings: async (s) => {
      settings = { ...s };
    },
    ownKeys: async () => {
      const out: T.OwnKey[] = [];
      for (const i of identities()) {
        const email = i.email.toLowerCase();
        const existing = out.find((o) => o.email === email);
        if (existing) existing.identityIds.push(i.id);
        else out.push({ email, name: i.name, identityIds: [i.id], key: own.get(email) ?? null, unlocked: true });
      }
      return structuredClone(out);
    },
    generateKey: async (email, name) => {
      const k = key(email, name, 'own', { createdAt: Date.now(), addedAt: Date.now(), updatedAt: Date.now() });
      own.set(email.toLowerCase(), k);
      return structuredClone(k);
    },
    importText: async (text) => {
      if (!text.includes('-----BEGIN PGP')) throw { message: 'No OpenPGP key found', kind: 'invalid' };
      const k = key(`imported${peers.length}@example.org`, 'Imported key', 'manual');
      peers.push(k);
      return [structuredClone(k)];
    },
    importFile: async () => demo(),
    exportPublic: async (fp) => ARMOR('PUBLIC', fp),
    exportSecret: async (fp) => ARMOR('PRIVATE', fp),
    exportFile: async () => {},
    deleteKey: async (fp) => {
      for (const [email, k] of own) if (k.fingerprint === fp) own.delete(email);
      const i = peers.findIndex((k) => k.fingerprint === fp);
      if (i >= 0) peers.splice(i, 1);
    },
    keys: async (query) => {
      const q = query.trim().toLowerCase();
      return structuredClone(
        all().filter((k) => !q || k.userIds.join(' ').toLowerCase().includes(q) || k.fingerprint.toLowerCase().includes(q.replace(/\s/g, ''))),
      );
    },
    setVerified: async (fp, verified) => {
      const k = find(fp);
      if (k) k.verified = verified;
    },
    setOwnKey: async (email, fp) => {
      const k = find(fp);
      if (k) own.set(email.toLowerCase(), k);
    },
    unlock: async () => {},
    lock: async () => {},
    composeStatus: async (accountId, identityId, from, recipients) => {
      const identity = identities().find((i) => i.id === identityId) ?? identities().find((i) => i.accountId === accountId);
      const sender = (from?.email ?? identity?.email ?? data.me.work).toLowerCase();
      const ownKey = own.get(sender) ?? null;
      const list: T.RecipientKey[] = [...new Set(recipients.map((r) => r.trim().toLowerCase()).filter((r) => r.includes('@')))].map((email) => ({
        email,
        key: forEmail(email) ?? null,
        preferEncrypt: false,
      }));
      const missing = list.filter((r) => !r.key).map((r) => r.email);
      return structuredClone({
        from: sender,
        ownKey,
        ownLocked: false,
        recipients: list,
        missing,
        canEncrypt: !!ownKey && list.length > 0 && missing.length === 0,
        canSign: !!ownKey,
        mutual: false,
        settings,
      });
    },
    lookup: async () => [],
    open: async (messageId) => {
      const m = data.messages.find((x) => x.id === messageId);
      if (!m?.crypto) throw { message: 'This message is neither encrypted nor signed.', kind: 'notFound' };
      const signer = m.from ? forEmail(m.from.email) ?? null : null;
      const opened: T.OpenedMessage = {
        messageId,
        kind: m.crypto,
        encrypted: m.crypto === 'encrypted' || m.crypto === 'inline-encrypted',
        status: 'ok',
        error: null,
        lockedKey: null,
        body: null,
        attachments: [],
        subject: null,
        signatures: [{ status: signer ? 'good' : 'unknownKey', signer, issuer: signer?.fingerprint ?? null, created: m.date, senderMatches: !!signer }],
        partiallySigned: false,
        partiallyEncrypted: false,
      };
      return structuredClone(opened);
    },
  };
}

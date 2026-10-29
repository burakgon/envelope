// OpenPGP and S/MIME for the browser demo: a few made-up keys and certificates, no real cryptography.

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

/** A made-up S/MIME certificate, issued by a made-up authority. */
function certificate(email: string, name: string, source: T.CertificateSource, issuer: string): T.CertificateInfo {
  const from = Date.now() - 200 * DAY;
  const fp = fingerprint(`${email}:smime`) + fingerprint(`${email}:sha256`).slice(0, 24);
  return {
    fingerprint: fp,
    name,
    subject: `CN=${name}`,
    emails: [email.toLowerCase()],
    issuer,
    issuerSubject: `CN=${issuer},O=${issuer.split(' ')[0]}`,
    serial: fp.slice(0, 16).match(/../g)!.join(':'),
    algorithm: 'RSA 3072',
    notBefore: from,
    notAfter: from + 730 * DAY,
    expired: false,
    notYetValid: false,
    canSign: true,
    canEncrypt: true,
    secret: source === 'own',
    source,
    addedAt: from,
    updatedAt: from,
  };
}

const PEM = (fp: string) => `-----BEGIN CERTIFICATE-----\nMIIDemo${fp.slice(0, 40)}\n-----END CERTIFICATE-----\n`;

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

  // S/MIME: a certificate of the user's, and a message signed with one, as companies send them.
  const certs: T.CertificateInfo[] = [certificate(data.me.work, data.me.name, 'own', 'Lumen Corporate CA')];
  const smimeSigned = data.messages.find((m) => m.subject.startsWith('Angebot'));
  if (smimeSigned?.from) {
    smimeSigned.crypto = 'smime-signed';
    certs.push(certificate(smimeSigned.from.email, smimeSigned.from.name ?? smimeSigned.from.email, 'message', 'Harbor Trust Email CA'));
  }
  const ownCert = (email: string) => certs.find((c) => c.secret && c.emails.includes(email.toLowerCase())) ?? null;
  const certFor = (email: string) => certs.find((c) => c.emails.includes(email.toLowerCase()) && c.canEncrypt) ?? null;

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
    composeStatus: async (accountId, identityId, from, recipients, replyTo) => {
      const identity = identities().find((i) => i.id === identityId) ?? identities().find((i) => i.accountId === accountId);
      const sender = (from?.email ?? identity?.email ?? data.me.work).toLowerCase();
      const ownKey = own.get(sender) ?? null;
      const list: T.RecipientKey[] = [...new Set(recipients.map((r) => r.trim().toLowerCase()).filter((r) => r.includes('@')))].map((email) => ({
        email,
        key: forEmail(email) ?? null,
        preferEncrypt: false,
      }));
      const missing = list.filter((r) => !r.key).map((r) => r.email);
      const myCert = ownCert(sender);
      const withCerts = list.map((r) => ({ email: r.email, certificate: certFor(r.email) }));
      const noCert = withCerts.filter((r) => !r.certificate).map((r) => r.email);
      const smime: T.SmimeCompose = { own: myCert, recipients: withCerts, missing: noCert, canEncrypt: !!myCert && list.length > 0 && noCert.length === 0, canSign: !!myCert };
      const original = replyTo ? data.messages.find((m) => m.id === replyTo) : null;
      const standard = myCert && (original?.crypto?.startsWith('smime-') || !ownKey) ? 'smime' : 'openpgp';
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
        smime,
        standard,
      } satisfies T.ComposeCrypto);
    },
    lookup: async () => [],
    open: async (messageId) => {
      const m = data.messages.find((x) => x.id === messageId);
      if (!m?.crypto) throw { message: 'This message is neither encrypted nor signed.', kind: 'notFound' };
      const signer = m.from ? forEmail(m.from.email) ?? null : null;
      const cert = m.from ? certs.find((c) => c.emails.includes(m.from!.email.toLowerCase())) ?? null : null;
      const smime: T.SmimeSignature | null =
        m.crypto === 'smime-signed'
          ? { verdict: cert ? 'verified' : 'noCertificate', reason: null, detail: null, certificate: cert, chain: cert ? [cert.issuer, 'Harbor Trust Root'] : [], algorithm: 'RSA, SHA-256' }
          : null;
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
        signatures: smime
          ? [{ status: cert ? 'good' : 'unknownKey', signer: null, issuer: null, created: m.date, senderMatches: !!cert, smime }]
          : [{ status: signer ? 'good' : 'unknownKey', signer, issuer: signer?.fingerprint ?? null, created: m.date, senderMatches: !!signer }],
        partiallySigned: false,
        partiallyEncrypted: false,
      };
      return structuredClone(opened);
    },
    smime: {
      certificates: async (query) => {
        const q = query.trim().toLowerCase();
        return structuredClone(certs.filter((c) => !q || c.name.toLowerCase().includes(q) || c.emails.some((e) => e.includes(q)) || c.issuer.toLowerCase().includes(q)));
      },
      importFile: async (_path, password, ownEmail) => {
        if (ownEmail) {
          // A .p12 file needs its password; any will do in the demo.
          if (!password) throw { message: 'Wrong password', kind: 'badPassword' } satisfies T.ApiError;
          const name = identities().find((i) => i.email.toLowerCase() === ownEmail.toLowerCase())?.name ?? ownEmail;
          const c = certificate(ownEmail, name, 'own', 'Harbor Trust Email CA');
          const kept = certs.filter((x) => !(x.secret && x.emails.includes(ownEmail.toLowerCase())));
          certs.length = 0;
          certs.push(...kept, c);
          certs.sort((a, b) => Number(b.secret) - Number(a.secret));
          return [structuredClone(c)];
        }
        const c = certificate(`imported${certs.length}@example.org`, 'Imported certificate', 'manual', 'Harbor Trust Email CA');
        certs.push(c);
        return [structuredClone(c)];
      },
      exportText: async (fp) => PEM(fp),
      exportFile: async () => {},
      remove: async (fp) => {
        const i = certs.findIndex((c) => c.fingerprint === fp);
        if (i >= 0) certs.splice(i, 1);
      },
    },
  };
}

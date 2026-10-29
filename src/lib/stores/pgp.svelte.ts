// OpenPGP and S/MIME in the interface: opened (decrypted or verified) messages
// and the passphrase prompt.

import { api } from '../api';
import type { KeyInfo, MessageView, OpenedMessage } from '../api';
import { errorMessage } from './toasts.svelte';

interface Ask {
  key: KeyInfo;
  error: string | null;
  resolve: (unlocked: boolean) => void;
}

/** Groups a fingerprint in fours for reading aloud and comparing. */
export function formatFingerprint(fp: string): string {
  return (fp.match(/.{1,4}/g) ?? [fp]).join(' ');
}

/** The name in a key's first user id (or its address). */
export function keyName(key: KeyInfo | null | undefined): string {
  const uid = key?.userIds[0] ?? '';
  const name = uid.replace(/<[^>]*>/, '').trim();
  return name || key?.emails[0] || (key ? formatFingerprint(key.fingerprint.slice(-16)) : '');
}

export function isEncrypted(m: Pick<MessageView, 'crypto'>): boolean {
  return m.crypto === 'encrypted' || m.crypto === 'inline-encrypted' || m.crypto === 'smime-encrypted';
}

/** The content is inside the protection (encrypted, or S/MIME opaque signed): its attachments come with opening it. */
export function isContained(m: Pick<MessageView, 'crypto'>): boolean {
  return isEncrypted(m) || m.crypto === 'smime-opaque';
}

class Pgp {
  /** Messages opened in this session, by id. */
  opened = $state<Record<number, OpenedMessage>>({});
  /** Messages that could not be opened at all, with the reason. */
  failed = $state<Record<number, string>>({});
  /** The passphrase prompt on screen. */
  ask = $state<Ask | null>(null);
  private pending = new Map<number, Promise<OpenedMessage | null>>();

  /** Decrypts and/or verifies a message (once per session). */
  open(messageId: number, again = false): Promise<OpenedMessage | null> {
    const known = this.opened[messageId];
    if (known && !again) return Promise.resolve(known);
    const running = this.pending.get(messageId);
    if (running) return running;
    delete this.failed[messageId];
    const p = api.crypto
      .open(messageId)
      .then((o) => {
        this.opened[messageId] = o;
        return o;
      })
      .catch((e) => {
        this.failed[messageId] = errorMessage(e);
        return null;
      })
      .finally(() => this.pending.delete(messageId));
    this.pending.set(messageId, p);
    return p;
  }

  /** Asks for a key's passphrase; resolves `true` once the key is unlocked. */
  unlock(key: KeyInfo, error: string | null = null): Promise<boolean> {
    this.ask?.resolve(false);
    return new Promise((resolve) => {
      this.ask = { key, error, resolve };
    });
  }

  async submit(passphrase: string) {
    const a = this.ask;
    if (!a) return;
    try {
      await api.crypto.unlock(a.key.fingerprint, passphrase);
    } catch (e) {
      this.ask = { ...a, error: errorMessage(e) };
      return;
    }
    this.ask = null;
    // Messages that waited for this passphrase open now.
    for (const [id, o] of Object.entries(this.opened)) if (o.status === 'locked') this.open(Number(id), true);
    a.resolve(true);
  }

  cancel() {
    const a = this.ask;
    this.ask = null;
    a?.resolve(false);
  }

  /** Keys changed: check every message again when it is shown. */
  forget() {
    this.opened = {};
    this.failed = {};
  }

  /** The real subject (protected headers) of the first opened message that has one. */
  subjectOf(ids: number[]): string | null {
    for (const id of ids) {
      const s = this.opened[id]?.subject;
      if (s) return s;
    }
    return null;
  }
}

export const pgp = new Pgp();

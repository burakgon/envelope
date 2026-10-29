// VIP senders: their conversations have a mailbox of their own, `is:vip` finds them, and an
// account can notify for their mail only. Kept by the engine (the vips table).

import { api } from '../api';
import type { Address, Vip } from '../api';
import { t } from '../i18n';
import { displayName } from '../utils/format';
import { mail } from './mail.svelte';
import { errorMessage, toasts } from './toasts.svelte';

class Vips {
  list = $state<Vip[]>([]);
  #emails = $derived(new Set(this.list.map((v) => v.email)));

  async load() {
    try {
      this.list = await api.vips();
    } catch {
      /* the list stays as it was */
    }
  }

  /** Whether mail from this address is a VIP's. */
  has(email: string | null | undefined): boolean {
    return !!email && this.#emails.has(email.trim().toLowerCase());
  }

  /** Makes a person a VIP, or no longer one, with Undo. */
  async set(person: Address, vip: boolean) {
    const name = person.name?.trim() || null;
    try {
      await api.setVip(person.email, name, vip);
      await this.changed();
      toasts.undoable(t(vip ? 'vip.added' : 'vip.removed', { name: displayName(person) }), async () => {
        await api.setVip(person.email, name, !vip);
        await this.changed();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  /** A contact's addresses, all VIPs or none, with Undo. */
  async setAll(name: string, emails: string[], vip: boolean) {
    const each = async (on: boolean) => {
      for (const email of emails) await api.setVip(email, name, on);
      await this.changed();
    };
    try {
      await each(vip);
      toasts.undoable(t(vip ? 'vip.added' : 'vip.removed', { name }), () => each(!vip));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  private async changed() {
    await this.load();
    if (mail.isMailbox({ kind: 'vip' }) || /\bis:vips?\b/i.test(mail.search)) await mail.refresh();
  }
}

export const vips = new Vips();

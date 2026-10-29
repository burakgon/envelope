// Accounts that need attention: signed out (the server wants a new sign-in), failing to sync, or
// unable to reach their server while the network is up. Shown above the message list and in the
// account's entry in Settings.

import { api } from '../api';
import type { Account } from '../api';
import { mail } from './mail.svelte';
import { dialogs } from './dialogs.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { t } from '../i18n';

/** Failed attempts in a row before an unreachable server is worth a banner (about 15 s of retries). */
const UNREACHABLE_AFTER = 3;

export type Problem = 'signIn' | 'error' | 'unreachable';

/** What the interface calls an account. */
export const accountName = (a: Account) => a.label ?? a.email;

/** The server an account syncs with, for "can't reach …". */
export function serverOf(a: Account): string {
  if (a.kind === 'exchange') {
    try {
      return new URL(a.config.exchange?.ewsUrl ?? '').host;
    } catch {
      return a.config.exchange?.ewsUrl ?? '';
    }
  }
  if (a.kind === 'microsoft') return 'Microsoft 365';
  return a.config.imap?.host ?? (a.kind === 'gmail' ? 'imap.gmail.com' : a.email.split('@')[1] ?? '');
}

class AccountHealth {
  /** Problems the user put away: account id → which (shown again once it changes or comes back). */
  dismissed = $state<Record<number, Problem>>({});
  /** Whether the computer has a network: when it has none, every account is offline, which says enough. */
  online = $state(typeof navigator === 'undefined' || navigator.onLine);

  constructor() {
    if (typeof window === 'undefined') return;
    window.addEventListener('online', () => (this.online = true));
    window.addEventListener('offline', () => (this.online = false));
  }

  /** What is wrong with an account, when it is worth telling. */
  problem(a: Account): Problem | null {
    const s = a.sync;
    if (!a.enabled) return null;
    if (s.state === 'authError') return 'signIn';
    if (s.state === 'error') return 'error';
    if (s.state === 'offline' && (s.failures ?? 0) >= UNREACHABLE_AFTER && this.online) return 'unreachable';
    return null;
  }

  /** Accounts whose problem is shown above the message list. */
  alerts = $derived(mail.accounts.filter((a) => {
    const p = this.problem(a);
    return p != null && this.dismissed[a.id] !== p;
  }));

  /** The banner's sentence. */
  text(a: Account): string {
    const name = accountName(a);
    switch (this.problem(a)) {
      case 'signIn':
        return t('accountAlert.signIn', { name });
      case 'unreachable':
        return t('accountAlert.unreachable', { name, server: serverOf(a) || name });
      default:
        return t('accountAlert.error', { name });
    }
  }

  dismiss(a: Account) {
    const p = this.problem(a);
    if (p) this.dismissed[a.id] = p;
  }

  /** Forgets dismissed problems of accounts that recovered, so they show if they come back. */
  forgetRecovered() {
    for (const a of mail.accounts) {
      if (this.dismissed[a.id] && this.problem(a) !== this.dismissed[a.id]) delete this.dismissed[a.id];
    }
  }

  /** Tries to sync the account again now. */
  retry(a: Account) {
    api.syncNow(a.id);
    toasts.show(t('accountAlert.retrying', { name: accountName(a) }));
  }

  /** The whole error message, with Retry. */
  async details(a: Account) {
    const message = a.sync.state === 'error' || a.sync.state === 'offline' || a.sync.state === 'authError' ? a.sync.message : '';
    const again = await dialogs.confirm({
      title: this.text(a),
      message: t(this.problem(a) === 'unreachable' ? 'accountAlert.unreachableHelp' : 'accountAlert.errorHelp'),
      detail: message || undefined,
      confirmLabel: t('accountAlert.retry'),
    });
    if (again) this.retry(a);
  }

  /** Signs an account in again: a new password, or Google's or Microsoft's sign-in in the browser. */
  async signIn(a: Account) {
    try {
      if (a.config.auth === 'password') {
        const pw = await dialogs.prompt({ title: t('settings.accounts.newPassword', { email: a.email }), type: 'password', confirmLabel: t('settings.accounts.signInAgain') });
        if (!pw) return;
        await api.updateAccount(a.id, { password: pw });
      } else {
        await api.addOAuthAccount(a.config.auth === 'oAuthGoogle' ? 'google' : 'microsoft', a.email);
      }
      await mail.loadAccounts();
      toasts.show(t('settings.accounts.signedInAgain'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
}

export const health = new AccountHealth();

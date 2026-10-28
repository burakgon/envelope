import { api } from '../api';
import type { Account } from '../api';

/** Which accounts are known to be sending automatic replies. */
class AutoReplies {
  on = $state<Record<number, boolean>>({});

  known(accountId: number, enabled: boolean) {
    this.on[accountId] = enabled;
  }

  /**
   * Asks the accounts with a settings API (Microsoft 365, Exchange, Gmail)
   * in the background. IMAP servers are asked only when the dialog opens:
   * ManageSieve can be slow to answer, or not answer at all.
   */
  async refresh(accounts: Account[]) {
    for (const a of accounts) {
      const viaApi = a.kind === 'microsoft' || a.kind === 'exchange' || (a.kind === 'gmail' && a.config.auth === 'oAuthGoogle');
      if (!a.enabled || !viaApi) continue;
      try {
        this.known(a.id, (await api.autoReply.get(a.id)).reply.enabled);
      } catch {
        // Not available for this account (for example a grant without the permission).
      }
    }
  }
}

export const autoReplies = new AutoReplies();

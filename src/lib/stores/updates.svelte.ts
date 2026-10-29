// Newer versions of Envelope: looked for a little after start and then once a day, unless turned off
// in Settings → About. Only GitHub's public list of releases is asked; nothing about the user goes out.

import { api } from '../api';
import type { Release } from '../api';
import { t } from '../i18n';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';

const DAY = 86_400_000;

class Updates {
  available = $state<Release | null>(null);
  checking = $state(false);
  private started = false;

  /** The notice for the sidebar: a newer version that was not put away. */
  notice = $derived(this.available && ui.settings.dismissedUpdate !== this.available.version ? this.available : null);

  start() {
    if (this.started) return;
    this.started = true;
    setTimeout(() => this.check(), 20_000);
    setInterval(() => this.check(), DAY);
  }

  /** Looks now; asked for by the user (`manual`), it says what it found. */
  async check(manual = false) {
    if ((!manual && !ui.settings.checkUpdates) || this.checking) return;
    this.checking = true;
    try {
      this.available = await api.newerRelease();
      if (manual) toasts.show(this.available ? t('updates.available', { version: this.available.version }) : t('updates.upToDate'), { kind: 'success' });
    } catch (e) {
      if (manual) toasts.error(t('updates.failed', { error: errorMessage(e) }));
    } finally {
      this.checking = false;
    }
  }
}

export const updates = new Updates();

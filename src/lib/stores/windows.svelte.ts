// Windows of their own (a conversation, a message being written) close with what they showed:
// archived, sent, put away. The toast about it, with its Undo, goes to the main window then, so
// what was done can still be undone there.

import { api } from '../api';
import type { HandOver, ToastStep } from '../api';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';
import { t } from '../i18n';

type Runner = (step: ToastStep) => Promise<void> | void;

class Windows {
  /** Work under way that may still give a toast: a window of its own waits for it before closing. */
  pending = $state(0);
  /** What runs the button of a toast, by the kind of its step (the mail and compose stores say). */
  private runners = new Map<ToastStep['kind'], Runner>();

  /** Says what runs the buttons of toasts of these kinds. */
  handle(kinds: ToastStep['kind'][], run: Runner) {
    for (const kind of kinds) this.runners.set(kind, run);
  }

  /** Keeps a window of its own open until `work` is done. */
  async hold<T>(work: () => Promise<T>): Promise<T> {
    this.pending++;
    try {
      return await work();
    } finally {
      this.pending--;
    }
  }

  /**
   * A toast about something done here, with Undo (or Open, for a draft put away). A window of
   * its own hands it to the main window, which shows it, when that is on screen; else it shows
   * here, and the window waits for it to go.
   */
  async toast(message: string, step: ToastStep, duration?: number) {
    if (ui.window !== 'main') {
      const handed = await api.windows.handOver({ message, step, duration }).catch(() => false);
      if (handed) return;
    }
    this.show({ message, step, duration });
  }

  /** Shows a toast of this window, or one another window handed over. */
  show({ message, step, duration }: HandOver) {
    const run = () => {
      const runner = this.runners.get(step.kind);
      Promise.resolve(runner?.(step)).catch((e) => toasts.error(errorMessage(e)));
    };
    if (step.kind === 'open') toasts.show(message, { action: { label: t('common.open'), run }, duration });
    else toasts.undoable(message, run, duration);
  }
}

export const windows = new Windows();

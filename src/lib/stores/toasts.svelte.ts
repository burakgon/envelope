import { t } from '../i18n';

export interface Toast {
  id: number;
  message: string;
  kind: 'info' | 'success' | 'error' | 'progress';
  /** `undo` marks the action that the `z` shortcut repeats. */
  action?: { label: string; run: () => void; key?: string; undo?: boolean };
  /** Milliseconds until the toast disappears (0 = sticky). */
  duration: number;
  createdAt: number;
}

let seq = 1;

class Toasts {
  items = $state<Toast[]>([]);
  /** The most recent undoable action (bound to the `z` shortcut). */
  lastUndo: (() => void) | null = null;

  show(message: string, opts: Partial<Omit<Toast, 'id' | 'message' | 'createdAt'>> = {}): number {
    const toast: Toast = {
      id: seq++,
      message,
      kind: opts.kind ?? 'info',
      action: opts.action,
      duration: opts.duration ?? (opts.action ? 6000 : 3500),
      createdAt: Date.now(),
    };
    // Keep the stack short: newest wins.
    this.items = [...this.items.slice(-2), toast];
    if (toast.duration > 0) setTimeout(() => this.dismiss(toast.id), toast.duration);
    if (opts.action?.undo) this.lastUndo = opts.action.run;
    return toast.id;
  }

  undoable(message: string, undo: () => void, duration = 6000): number {
    let used = false;
    const run = () => {
      if (used) return;
      used = true;
      this.dismiss(id);
      undo();
    };
    const id = this.show(message, { action: { label: t('common.undo'), run, key: 'Z', undo: true }, duration });
    this.lastUndo = run;
    return id;
  }

  error(message: string): number {
    return this.show(message, { kind: 'error', duration: 7000 });
  }

  update(id: number, patch: Partial<Toast>) {
    this.items = this.items.map((t) => (t.id === id ? { ...t, ...patch } : t));
    if (patch.duration && patch.duration > 0) setTimeout(() => this.dismiss(id), patch.duration);
  }

  dismiss(id: number) {
    this.items = this.items.filter((t) => t.id !== id);
  }

  undoLast() {
    const u = this.lastUndo;
    this.lastUndo = null;
    u?.();
  }
}

export const toasts = new Toasts();

export function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e && typeof e === 'object' && 'message' in e) return String((e as { message: unknown }).message);
  return t('common.somethingWentWrong');
}

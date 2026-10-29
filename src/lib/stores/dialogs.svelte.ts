// Questions asked in the app's own dialogs. The browser's confirm() and prompt() are bare system
// windows in WebKitGTK ("JavaScript - tauri://localhost"), and prompt() shows a password as typed.

export interface ConfirmOptions {
  /** The question. */
  title: string;
  /** What answering it does. */
  message?: string;
  /** Shown as code, such as a fingerprint to compare. */
  detail?: string;
  /** The button that answers yes (else "OK"). */
  confirmLabel?: string;
  /** The button that answers no (else "Cancel"). */
  cancelLabel?: string;
  /** Yes deletes or loses something: the button is red, and Cancel has the focus. */
  danger?: boolean;
}

export interface PromptOptions {
  title: string;
  message?: string;
  /** What the field starts with. */
  value?: string;
  /** A password is not shown as it is typed. */
  type?: 'text' | 'password' | 'url';
  confirmLabel?: string;
}

export type Ask =
  | (ConfirmOptions & { kind: 'confirm'; resolve: (ok: boolean) => void })
  | (ConfirmOptions & { kind: 'choose'; resolve: (answer: boolean | null) => void })
  | (PromptOptions & { kind: 'prompt'; resolve: (value: string | null) => void });

class Dialogs {
  /** Questions waiting to be answered; the first one is on screen. */
  queue = $state.raw<Ask[]>([]);
  current = $derived(this.queue[0] ?? null);

  /** Asks a yes-or-no question: true for yes. */
  confirm(options: ConfirmOptions): Promise<boolean> {
    return new Promise((resolve) => (this.queue = [...this.queue, { ...options, kind: 'confirm', resolve }]));
  }

  /**
   * Asks a question whose no is an answer of its own ("Attach" against "Send anyway"):
   * true for yes, false for no, null when the question is put away (Escape, a click outside).
   */
  choose(options: ConfirmOptions): Promise<boolean | null> {
    return new Promise((resolve) => (this.queue = [...this.queue, { ...options, kind: 'choose', resolve }]));
  }

  /** Asks for a text: what was typed, or null when cancelled. */
  prompt(options: PromptOptions): Promise<string | null> {
    return new Promise((resolve) => (this.queue = [...this.queue, { ...options, kind: 'prompt', resolve }]));
  }

  /** Answers the question on screen: yes (with what was typed) or no. */
  answer(yes: boolean, value = '') {
    const [ask, ...rest] = this.queue;
    if (!ask) return;
    this.queue = rest;
    if (ask.kind === 'prompt') ask.resolve(yes ? value : null);
    else ask.resolve(yes);
  }

  /** Puts the question on screen away without an answer: no, or null for `choose`. */
  dismiss() {
    const [ask, ...rest] = this.queue;
    if (!ask) return;
    this.queue = rest;
    if (ask.kind === 'choose') ask.resolve(null);
    else if (ask.kind === 'confirm') ask.resolve(false);
    else ask.resolve(null);
  }
}

export const dialogs = new Dialogs();

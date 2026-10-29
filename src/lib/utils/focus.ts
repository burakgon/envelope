// Keyboard focus inside dialogs and menus.

const FOCUSABLE = 'a[href], button, input, select, textarea, iframe, [tabindex], [contenteditable="true"]';

/** Where Tab stops inside `root`, in order: enabled, on screen and not taken out of the tab order. */
export function tabbables(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) =>
      el.tabIndex >= 0 &&
      !(el as HTMLButtonElement).disabled &&
      el.getClientRects().length > 0 &&
      getComputedStyle(el).visibility !== 'hidden',
  );
}

/** Tab and Shift+Tab go round inside `root` instead of leaving it. */
export function trapTab(e: KeyboardEvent, root: HTMLElement) {
  if (e.key !== 'Tab' || e.ctrlKey || e.altKey || e.metaKey || e.defaultPrevented) return;
  e.preventDefault();
  const stops = tabbables(root);
  if (!stops.length) return root.focus();
  const i = stops.indexOf(document.activeElement as HTMLElement);
  stops[e.shiftKey ? (i <= 0 ? stops.length - 1 : i - 1) : (i + 1) % stops.length].focus();
}

/** A field that takes typing (and the arrow keys) has the focus. */
export function isTyping(el: Element | null): boolean {
  return !!el?.closest?.('input:not([type="checkbox"]):not([type="radio"]), textarea, select, [contenteditable="true"]');
}

/**
 * Puts the focus back where it was once something that took it is gone, unless what closed it
 * put the focus somewhere on purpose (another dialog, a field). When that place is gone too (the
 * command palette that opened a dialog, a row that was deleted), the focus goes to the view's
 * main list (`data-focus-home`) rather than to the page.
 */
export function restoreFocus(before: Element | null) {
  queueMicrotask(() => {
    const now = document.activeElement;
    if (now && now !== document.body && now.isConnected) return;
    const to = before instanceof HTMLElement && before.isConnected ? before : document.querySelector<HTMLElement>('[data-focus-home]');
    to?.focus({ preventScroll: true });
  });
}

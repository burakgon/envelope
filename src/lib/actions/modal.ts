import { tick } from 'svelte';
import { on } from 'svelte/events';
import { restoreFocus, trapTab } from '../utils/focus';

/**
 * A modal dialog (`use:modal` on the element with role="dialog"): the focus moves into it, unless
 * the dialog put it on a field or button of its own, Tab and Shift+Tab go round inside it, and the
 * focus goes back where it was when it closes. Screen readers are told the rest of the window is out
 * of reach; the dialog is named by its title (aria-labelledby) or, without one, by aria-label.
 */
export function modal(node: HTMLElement) {
  let before: Element | null = document.activeElement;
  node.setAttribute('aria-modal', 'true');
  if (!node.hasAttribute('tabindex')) node.tabIndex = -1;

  tick().then(() => {
    // Opened as another dialog closed: where that one gave the focus back.
    if (!before?.isConnected || before === document.body || node.contains(before)) {
      const now = document.activeElement;
      before = now && !node.contains(now) ? now : null;
    }
    if (!node.contains(document.activeElement)) node.focus({ preventScroll: true });
  });

  // After the handlers inside the dialog: a field that takes Tab itself (picking a suggestion) keeps it.
  const off = on(node, 'keydown', (e) => trapTab(e as KeyboardEvent, node));

  return {
    destroy() {
      off();
      restoreFocus(before);
    },
  };
}

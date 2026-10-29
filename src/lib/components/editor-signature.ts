// The signature inside the composer's editor: a block of its own, so it can be
// swapped when the sender changes and the caret can be kept out of it.

import { Node, mergeAttributes, type Editor } from '@tiptap/core';
import { Selection } from '@tiptap/pm/state';

/** `<div class="envelope-signature">`, kept through editing instead of melting into the text. */
export const Signature = Node.create({
  name: 'signature',
  group: 'block',
  content: 'block+',
  defining: true,
  parseHTML: () => [{ tag: 'div.envelope-signature', priority: 60 }],
  renderHTML: ({ HTMLAttributes }) => ['div', mergeAttributes(HTMLAttributes, { class: 'envelope-signature' }), 0],
});

/** Where the signature is in the document. */
export function signatureRange(editor: Editor): { from: number; to: number } | null {
  let found: { from: number; to: number } | null = null;
  editor.state.doc.forEach((node, offset) => {
    if (!found && node.type.name === 'signature') found = { from: offset, to: offset + node.nodeSize };
  });
  return found;
}

/** Whether anything is written in the document between `from` and `to` (text or a picture). */
export function written(editor: Editor, from: number, to: number): boolean {
  let found = false;
  editor.state.doc.nodesBetween(from, to, (node) => {
    if (found) return false;
    if ((node.isText && node.text?.trim()) || node.type.name === 'image') found = true;
    return !found;
  });
  return found;
}

/**
 * Puts `html` in as the signature: in place of the one there, removing it without `html`, or
 * after what was written when there is none. What was written stays as it is, and so does
 * the caret.
 */
export function replaceSignature(editor: Editor, html: string | null) {
  const range = signatureRange(editor);
  const block = html?.trim() ? `<div class="envelope-signature">${html}</div>` : '';
  if (range && block) editor.chain().insertContentAt(range, block, { updateSelection: false }).run();
  else if (range) editor.chain().deleteRange(range).run();
  else if (block) {
    // After the last block with something in it (or the first line), not after empty lines at the end.
    let at = editor.state.doc.firstChild?.nodeSize ?? 0;
    editor.state.doc.forEach((node, offset) => {
      if (node.textContent.trim() || node.type.name !== 'paragraph') at = offset + node.nodeSize;
    });
    editor.chain().insertContentAt(at, block, { updateSelection: false }).run();
  }
}

/** Puts the caret at the end of what was written, above the signature (making room there if need be). */
export function focusAboveSignature(editor: Editor) {
  // Now, not on the next frame as the focus command does: what is typed right away goes here.
  editor.view.focus();
  const range = signatureRange(editor);
  if (!range) {
    editor.chain().setTextSelection(editor.state.doc.content.size).scrollIntoView().run();
    return;
  }
  const near = Selection.near(editor.state.doc.resolve(range.from), -1);
  if (near.from < range.from) {
    editor.chain().setTextSelection(near.from).scrollIntoView().run();
    return;
  }
  // The signature comes first: a line above it to write on.
  editor.chain().insertContentAt(range.from, '<p></p>').setTextSelection(range.from + 1).scrollIntoView().run();
}

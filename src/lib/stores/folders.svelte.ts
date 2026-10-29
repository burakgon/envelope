// Creating, renaming, nesting, coloring and deleting folders and labels, and emptying Trash and
// Spam: from the sidebar's folder menu, by dragging folders, and from the Move and Label menus.
// Changes to the folder tree need the server (and say so when it refuses); colors and emptying
// work offline.

import { api } from '../api';
import type { Account, Folder, MoveTarget } from '../api';
import { dialogs } from './dialogs.svelte';
import { mail, roleLabel } from './mail.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { t } from '../i18n';

/** Label colors: Outlook's category colors, so a category shows in Outlook as it was picked. */
export const labelColors = [
  { name: 'red', hex: '#e74856' },
  { name: 'orange', hex: '#ff8c00' },
  { name: 'yellow', hex: '#c19c00' },
  { name: 'green', hex: '#47b347' },
  { name: 'teal', hex: '#00b7c3' },
  { name: 'blue', hex: '#3a96dd' },
  { name: 'purple', hex: '#8764b8' },
  { name: 'cranberry', hex: '#c50f1f' },
  { name: 'brown', hex: '#ab7b4f' },
  { name: 'steel', hex: '#8ba3b6' },
] as const;

/** A folder's name as the interface shows it. */
export const folderName = (f: Folder) => (f.role ? roleLabel(f.role) : f.name);

/** Special folders (Inbox, Sent, Trash, …) keep their name and place. */
export const isFixed = (f: Folder) => f.role != null;

/** Outlook categories are labels that can't be renamed or nested. */
export const isCategory = (f: Folder) => f.kind === 'label' && mail.accountById.get(f.accountId)?.kind !== 'gmail';

/** A folder and the folders inside it, parents first. */
export function subtree(f: Folder): Folder[] {
  const out = [f];
  for (let i = 0; i < out.length; i++) out.push(...mail.folders.filter((x) => x.parentId === out[i].id));
  return out;
}

/** Conversations in a folder and the folders inside it. */
export const conversationCount = (f: Folder) => subtree(f).reduce((n, x) => n + x.totalCount, 0);

/** Where a folder may be dropped: `target` (null: the top of its account). */
export function canNest(f: Folder, target: Folder | null): boolean {
  if (isFixed(f) || isCategory(f)) return false;
  if (!target) return f.parentId != null;
  if (target.accountId !== f.accountId || target.id === f.parentId || isCategory(target)) return false;
  if (subtree(f).some((x) => x.id === target.id)) return false;
  // Into an ordinary folder of the same kind: Gmail labels inside labels, folders inside folders.
  return target.kind === f.kind && target.role == null;
}

/** Why a name can't be used there, or null. */
function problem(name: string, accountId: number, parentId: number | null, kind: Folder['kind'], delimiter: string | null, except?: number): string | null {
  const trimmed = name.trim();
  if (!trimmed) return t('folders.nameEmpty');
  if (delimiter && trimmed.includes(delimiter)) return t('folders.nameSeparator', { separator: delimiter });
  const taken = mail.folders.some(
    (f) => f.accountId === accountId && f.parentId === parentId && f.kind === kind && f.id !== except && f.name.toLowerCase() === trimmed.toLowerCase(),
  );
  return taken ? t('folders.nameTaken', { name: trimmed }) : null;
}

/** Asks for a name until one fits (or the question is cancelled). */
async function askName(o: { title: string; confirmLabel: string; value?: string; check: (name: string) => string | null }): Promise<string | null> {
  let value = o.value ?? '';
  let message: string | undefined;
  for (;;) {
    const name = await dialogs.prompt({ title: o.title, message, value, confirmLabel: o.confirmLabel });
    if (name == null) return null;
    message = o.check(name) ?? undefined;
    if (!message) return name.trim();
    value = name;
  }
}

const delimiterOf = (accountId: number, parent: Folder | null) =>
  parent?.delimiter ?? mail.folders.find((f) => f.accountId === accountId && f.delimiter)?.delimiter ?? null;

/** A color for a new label: the next of the palette. */
function nextColor(accountId: number): string {
  const labels = mail.folders.filter((f) => f.accountId === accountId && f.kind === 'label').length;
  return labelColors[labels % labelColors.length].hex;
}

/** Creates a folder (or label) on the server; reports what went wrong. */
async function create(account: Account, parent: Folder | null, name: string, label: boolean): Promise<Folder | null> {
  const asLabel = label || account.kind === 'gmail';
  const progress = toasts.show(t('folders.creating', { name }), { kind: 'progress', duration: 0 });
  try {
    const folder = await api.folder.create(account.id, parent?.id ?? null, name, label, asLabel ? nextColor(account.id) : null);
    await mail.loadFolders();
    toasts.dismiss(progress);
    return folder;
  } catch (e) {
    toasts.dismiss(progress);
    toasts.error(t('folders.createFailed', { name, error: errorMessage(e) }));
    return null;
  }
}

/** "New folder" (or label) at the top of an account or inside `parent`. */
export async function newFolder(account: Account, parent: Folder | null = null): Promise<Folder | null> {
  const gmail = account.kind === 'gmail';
  const kind: Folder['kind'] = gmail ? 'label' : 'folder';
  const name = await askName({
    title: parent
      ? t(gmail ? 'folders.newSublabelIn' : 'folders.newSubfolderIn', { parent: folderName(parent) })
      : t(gmail ? 'folders.newLabel' : 'folders.newFolder'),
    confirmLabel: t('folders.create'),
    check: (n) => problem(n, account.id, parent?.id ?? null, kind, delimiterOf(account.id, parent)),
  });
  if (name == null) return null;
  const folder = await create(account, parent, name, false);
  if (folder) toasts.show(t(gmail ? 'folders.labelCreated' : 'folders.folderCreated', { name }), { kind: 'success' });
  return folder;
}

export async function renameFolder(f: Folder) {
  const label = f.kind === 'label';
  const name = await askName({
    title: t(label ? 'folders.renameLabel' : 'folders.renameFolder', { name: f.name }),
    confirmLabel: t('folders.rename'),
    value: f.name,
    check: (n) => (n.trim() === f.name ? null : problem(n, f.accountId, f.parentId, f.kind, f.delimiter ?? null, f.id)),
  });
  if (name == null || name === f.name) return;
  try {
    await api.folder.rename(f.id, name);
    await mail.loadFolders();
    toasts.show(t('folders.renamed', { name }), { kind: 'success' });
  } catch (e) {
    toasts.error(t('folders.renameFailed', { name: f.name, error: errorMessage(e) }));
  }
}

/** Puts a folder inside another (or at the top with null), with Undo. */
export async function nestFolder(f: Folder, target: Folder | null) {
  if (!canNest(f, target)) return;
  const name = f.name;
  const before = f.parentId;
  if (problem(name, f.accountId, target?.id ?? null, f.kind, f.delimiter ?? null, f.id)) {
    toasts.error(t('folders.nameTaken', { name }));
    return;
  }
  try {
    await api.folder.move(f.id, target?.id ?? null);
    await mail.loadFolders();
    const message = target ? t('folders.movedInto', { name, parent: folderName(target) }) : t('folders.movedToTop', { name });
    toasts.undoable(message, async () => {
      try {
        await api.folder.move(f.id, before);
        await mail.loadFolders();
      } catch (e) {
        toasts.error(t('folders.moveFailed', { name, error: errorMessage(e) }));
      }
    });
  } catch (e) {
    toasts.error(t('folders.moveFailed', { name, error: errorMessage(e) }));
  }
}

/** Deletes a folder or label after asking, saying what happens to its conversations. */
export async function deleteFolder(f: Folder) {
  const account = mail.accountById.get(f.accountId);
  const count = conversationCount(f);
  const inside = subtree(f).length - 1;
  const gmail = account?.kind === 'gmail';
  const message = f.kind === 'label'
    ? t(gmail ? 'folders.deleteLabelMessage' : 'folders.deleteCategoryMessage', { count })
    : [t('folders.deleteFolderMessage', { count }), inside ? t('folders.deleteFolderInside', { count: inside }) : ''].filter(Boolean).join(' ');
  const sure = await dialogs.confirm({
    title: t(f.kind === 'label' ? (gmail ? 'folders.deleteLabelTitle' : 'folders.deleteCategoryTitle') : 'folders.deleteFolderTitle', { name: f.name }),
    message,
    confirmLabel: t('common.delete'),
    danger: true,
  });
  if (!sure) return;
  const mb = mail.mailbox;
  const shown = mb.kind === 'folder' && subtree(f).some((x) => x.id === mb.id);
  try {
    await api.folder.remove(f.id);
    if (shown) mail.setMailbox({ kind: 'unified', role: 'inbox' });
    await mail.loadFolders();
    mail.refresh();
    toasts.show(t('folders.deleted', { name: f.name }));
  } catch (e) {
    toasts.error(t('folders.deleteFailed', { name: f.name, error: errorMessage(e) }));
  }
}

export async function setFolderColor(f: Folder, color: string | null) {
  try {
    await api.folder.setColor(f.id, color);
    await mail.loadFolders();
  } catch (e) {
    toasts.error(errorMessage(e));
  }
}

/** Empties Trash or Spam for good after asking, with how many conversations go. */
/** Uploads the messages of mbox files (Thunderbird's local folders, archives) and saved messages into a folder. */
export async function importInto(f: Folder) {
  const folder = folderName(f);
  const paths = await api.pickFiles(t('folders.importTitle', { folder }));
  if (!paths.length) return;
  const progress = toasts.show(t('folders.importing', { folder }), { kind: 'progress', duration: 0 });
  const off = api.onEvent((e) => {
    if (e.type === 'importProgress' && e.folderId === f.id) toasts.update(progress, { message: t('folders.importingCount', { count: e.done, folder }) });
  });
  try {
    const done = await api.folder.import(f.id, paths);
    const parts = [t('folders.imported', { count: done.added, folder })];
    if (done.skipped) parts.push(t('folders.alreadyThere', { count: done.skipped }));
    if (done.refused) parts.push(t('folders.refusedByServer', { count: done.refused }));
    toasts.show(parts.join(' · '), { kind: 'success', duration: 7000 });
  } catch (e) {
    toasts.error(errorMessage(e));
  } finally {
    off();
    toasts.dismiss(progress);
  }
}

export async function emptyFolder(f: Folder) {
  // Counted afresh: the list may have just changed.
  await mail.loadFolders();
  const count = conversationCount(mail.folderById.get(f.id) ?? f);
  const trash = f.role === 'trash';
  if (!count) {
    toasts.show(t('folders.alreadyEmpty', { role: trash ? 'trash' : 'junk' }));
    return;
  }
  // The question deleting forever in Trash asks.
  if (!(await mail.confirmDeleteForever(count))) return;
  try {
    const gone = await api.folder.empty(f.id);
    if (mail.isMailbox({ kind: 'folder', id: f.id })) mail.select(null);
    await mail.loadFolders();
    mail.refresh();
    toasts.show(t('mail.deletedCount', { count: gone }));
  } catch (e) {
    toasts.error(errorMessage(e));
  }
}

/** Where moving into a folder takes conversations. */
export function moveTargetOf(f: Folder): MoveTarget {
  if (f.role === 'inbox') return { kind: 'inbox' };
  if (f.role === 'trash') return { kind: 'trash' };
  if (f.role === 'junk') return { kind: 'junk' };
  if (f.role === 'archive' || f.role === 'all') return { kind: 'archive' };
  return { kind: 'folder', id: f.id };
}

/** "Create ‘name’" in the Move menu: a new folder (a label in Gmail), then the move. */
export async function createAndMove(account: Account, name: string, ids: number[]) {
  const kind: Folder['kind'] = account.kind === 'gmail' ? 'label' : 'folder';
  const issue = problem(name, account.id, null, kind, delimiterOf(account.id, null));
  if (issue) return toasts.error(issue);
  const folder = await create(account, null, name.trim(), false);
  if (folder) await mail.moveTo(moveTargetOf(folder), ids, folder.name);
}

/** "Create ‘name’" in the Label menu: a new label (an Outlook category), then on the conversations. */
export async function createAndLabel(account: Account, name: string, ids: number[]) {
  const issue = problem(name, account.id, null, 'label', account.kind === 'gmail' ? delimiterOf(account.id, null) : null);
  if (issue) return toasts.error(issue);
  const folder = await create(account, null, name.trim(), true);
  if (!folder) return;
  try {
    await (mail.byMessage ? api.labelMessages(ids, folder.id, true) : api.setLabel(ids, folder.id, true));
    toasts.show(t('mail.labelAdded', { name: folder.name }));
  } catch (e) {
    toasts.error(errorMessage(e));
  }
}

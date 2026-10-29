// Recent and saved searches. Saved ones are listed in the sidebar; both are kept with the settings.

import { t } from '../i18n';
import { dialogs } from './dialogs.svelte';
import { mail } from './mail.svelte';
import { toasts } from './toasts.svelte';
import { ui, type SavedSearch } from './ui.svelte';

const RECENT = 8;

/** Remembers a search that was made, newest first. */
export function rememberSearch(query: string) {
  const q = query.trim();
  if (!q) return;
  const recent = [q, ...ui.settings.recentSearches.filter((r) => r !== q)].slice(0, RECENT);
  if (recent.join('\n') !== ui.settings.recentSearches.join('\n')) ui.set('recentSearches', recent);
}

export function forgetSearch(query: string) {
  ui.set(
    'recentSearches',
    ui.settings.recentSearches.filter((r) => r !== query),
  );
}

/** Keeps the search on screen in the sidebar, under a name asked for. */
export async function saveSearch(query = mail.search) {
  const q = query.trim();
  if (!q) return;
  const name = await dialogs.prompt({ title: t('search.saveTitle'), message: t('search.saveMessage'), value: q, confirmLabel: t('common.save') });
  if (!name?.trim()) return;
  const saved: SavedSearch = { id: crypto.randomUUID(), name: name.trim(), query: q };
  ui.set('savedSearches', [...ui.settings.savedSearches, saved]);
  toasts.show(t('search.saved', { name: saved.name }), { kind: 'success' });
}

export async function renameSearch(s: SavedSearch) {
  const name = await dialogs.prompt({ title: t('search.renameTitle'), value: s.name, confirmLabel: t('folders.rename') });
  if (!name?.trim() || name.trim() === s.name) return;
  ui.set(
    'savedSearches',
    ui.settings.savedSearches.map((x) => (x.id === s.id ? { ...x, name: name.trim() } : x)),
  );
}

export function deleteSearch(s: SavedSearch) {
  const before = ui.settings.savedSearches;
  ui.set(
    'savedSearches',
    before.filter((x) => x.id !== s.id),
  );
  toasts.undoable(t('search.deleted', { name: s.name }), () => ui.set('savedSearches', before));
}

/** Runs a saved or recent search. */
export function runSearch(query: string) {
  rememberSearch(query);
  mail.setSearch(query);
}

// Mail rules run on mail that is already here ("Run now", "Also apply to existing messages").

import { api } from '../api';
import type { Address, RuleUndo } from '../api';
import { ui, type MailRule } from './ui.svelte';
import { mail } from './mail.svelte';
import { dialogs } from './dialogs.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { displayName } from '../utils/format';
import { t } from '../i18n';

/** At most this many conversations (the newest) are changed by one run. */
export const RUN_LIMIT = 5000;
/** Conversations changed per request, so progress can be shown. */
const CHUNK = 200;

/** Whether running the rule changes mail already here ("don't notify" only matters as mail arrives). */
export const runsOnExisting = (rule: MailRule) => rule.actions.some((a) => a.kind !== 'mute');

/** Applies a rule to matching conversations in chunks; returns what undoes it. */
async function applyInChunks(rule: MailRule, ids: number[], progress: number): Promise<RuleUndo[]> {
  const undos: RuleUndo[] = [];
  for (let i = 0; i < ids.length; i += CHUNK) {
    undos.push(await api.rules.apply(rule, ids.slice(i, i + CHUNK)));
    if (ids.length > CHUNK) toasts.update(progress, { message: t('rules.progress', { name: rule.name, done: Math.min(i + CHUNK, ids.length), count: ids.length }) });
  }
  return undos;
}

async function undoAll(undos: RuleUndo[]) {
  for (const undo of [...undos].reverse()) await api.rules.undo(undo);
  mail.refresh();
}

/** Runs a rule on the mail already here: the newest matching conversations, with progress, a count and Undo. */
export async function runRule(rule: MailRule) {
  const progress = toasts.show(t('rules.running', { name: rule.name }), { kind: 'progress', duration: 0 });
  try {
    const found = await api.rules.matches(rule, RUN_LIMIT);
    if (!found.threadIds.length) {
      toasts.dismiss(progress);
      toasts.show(t('rules.noMatches', { name: rule.name }));
      return;
    }
    const undos = await applyInChunks(rule, found.threadIds, progress);
    toasts.dismiss(progress);
    mail.refresh();
    const count = found.threadIds.length;
    const message = found.total > count ? t('rules.appliedNewest', { name: rule.name, count, total: found.total }) : t('rules.applied', { name: rule.name, count });
    toasts.undoable(message, () => undoAll(undos), 10_000);
  } catch (e) {
    toasts.dismiss(progress);
    toasts.error(t('rules.runFailed', { name: rule.name, error: errorMessage(e) }));
  }
}

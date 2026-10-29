// Mail rules run on mail that is already here ("Run now", "Also apply to existing messages").
// Blocked senders: a "Blocked senders" rule sends their new mail to Spam, and blocking someone moves
// what they sent before there too.

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

// ----- blocked senders ----------------------------------------------------------------

/** The rule holding the blocked senders. */
export const BLOCKED_RULE = 'blocked-senders';

/** Addresses whose mail goes to Spam. */
export function blockedSenders(): string[] {
  const rule = ui.settings.rules.find((r) => r.id === BLOCKED_RULE);
  return rule ? rule.conditions.filter((c) => c.field === 'from' && c.op === 'is').map((c) => c.value) : [];
}

export const isBlocked = (email: string) => blockedSenders().includes(email.trim().toLowerCase());

/** A rule that sends one sender's mail to Spam. */
const blockRule = (emails: string[], name = ''): MailRule => ({
  id: BLOCKED_RULE,
  name,
  enabled: true,
  accountId: null,
  match: 'any',
  conditions: emails.map((value) => ({ field: 'from', op: 'is', value })),
  actions: [{ kind: 'spam' }],
  stop: true,
});

/** Who a conversation is from, for "Block sender": the newest sender who isn't the user. */
export function senderOf(thread: { lastSender: Address | null; participants: Address[] }): Address | null {
  const mine = (a: Address | null | undefined) => !a || mail.meEmails.has(a.email.toLowerCase());
  if (!mine(thread.lastSender)) return thread.lastSender;
  return [...thread.participants].reverse().find((p) => !mine(p)) ?? null;
}

/**
 * Blocks a sender after asking: their new mail goes to Spam (the "Blocked senders" rule, first among
 * the rules), and so do the conversations from them already here. Undo unblocks and moves them back.
 */
export async function blockSender(sender: Address) {
  const email = sender.email.trim().toLowerCase();
  const once = blockRule([email]);
  // Conversations they alone wrote: a group conversation they joined stays.
  const found = await api.rules.fromSender(email, RUN_LIMIT).catch(() => ({ total: 0, threadIds: [] as number[] }));
  const sure = await dialogs.confirm({
    title: t('blocking.title', { name: displayName(sender) }),
    message: t('blocking.message', { email, count: found.total }),
    confirmLabel: t('blocking.block'),
    danger: true,
  });
  if (!sure) return;
  const rules = ui.settings.rules;
  const current = rules.find((r) => r.id === BLOCKED_RULE);
  const updated = current
    ? rules.map((r) => (r.id === BLOCKED_RULE ? { ...r, conditions: [...r.conditions.filter((c) => c.value !== email), { field: 'from' as const, op: 'is' as const, value: email }] } : r))
    : [blockRule([email], t('blocking.ruleName')), ...rules];
  ui.set('rules', updated);
  let undos: RuleUndo[] = [];
  const progress = found.threadIds.length > CHUNK ? toasts.show(t('blocking.moving'), { kind: 'progress', duration: 0 }) : 0;
  try {
    undos = await applyInChunks(once, found.threadIds, progress);
  } catch (e) {
    toasts.error(errorMessage(e));
  } finally {
    if (progress) toasts.dismiss(progress);
  }
  mail.refresh();
  toasts.undoable(t('blocking.blocked', { email, count: found.threadIds.length }), async () => {
    unblock(email, true);
    await undoAll(undos);
  }, 10_000);
}

/** Lets a sender's mail into the inbox again. */
export function unblock(email: string, quiet = false) {
  const rules = ui.settings.rules;
  const rule = rules.find((r) => r.id === BLOCKED_RULE);
  if (!rule) return;
  const conditions = rule.conditions.filter((c) => !(c.field === 'from' && c.value === email));
  ui.set('rules', conditions.length ? rules.map((r) => (r.id === BLOCKED_RULE ? { ...r, conditions } : r)) : rules.filter((r) => r.id !== BLOCKED_RULE));
  if (!quiet) toasts.show(t('blocking.unblocked', { email }));
}

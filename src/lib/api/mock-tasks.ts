// Demo task lists and tasks, with the semantics of the engine (see the `tasks_*` commands).

import type * as T from './types';
import { accounts, messages, threads } from './mockdata';

const DAY = 86_400_000;

function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** Local midnight of a `YYYY-MM-DD` day. */
function dayStart(date: string): number {
  return new Date(`${date}T00:00:00`).getTime();
}

const today = new Date();
today.setHours(0, 0, 0, 0);
const day = (n: number) => ymd(new Date(today.getFullYear(), today.getMonth(), today.getDate() + n));
const at = (n: number, h: number, m = 0) => new Date(today.getFullYear(), today.getMonth(), today.getDate() + n, h, m).getTime();

const invalid = (message: string): T.ApiError => ({ message, kind: 'invalid' });
const notFound = (message: string): T.ApiError => ({ message, kind: 'notFound' });

/** What each kind of service stores. */
const features: Record<T.TaskListKind, Pick<T.TaskList, 'dueTimes' | 'importance' | 'subtasks' | 'reorder'>> = {
  local: { dueTimes: true, importance: true, subtasks: true, reorder: true },
  caldav: { dueTimes: true, importance: true, subtasks: true, reorder: true },
  google: { dueTimes: false, importance: false, subtasks: true, reorder: true },
  graph: { dueTimes: false, importance: true, subtasks: true, reorder: false },
  ews: { dueTimes: false, importance: true, subtasks: false, reorder: false },
};

type StoredList = Omit<T.TaskList, 'openCount' | 'completedCount'> & { sortOrder: number };

const L = (id: number, accountId: number | null, kind: T.TaskListKind, name: string, color: string, isDefault: boolean, sortOrder: number): StoredList => ({
  id, accountId, kind, name, color, readOnly: false, isDefault, sortOrder, ...features[kind],
});

const lists: StoredList[] = [
  L(1, null, 'local', 'Tasks', '#14b8a6', true, 0),
  L(2, 1, 'google', 'My Tasks', '#6d5efc', true, 0),
  L(3, 1, 'google', 'Launch v0.3', '#ec4899', false, 1),
  L(4, 2, 'caldav', 'Reminders', '#0ea5e9', true, 0),
  L(5, 2, 'caldav', 'Home', '#f59e0b', false, 1),
];

interface Stored extends Omit<T.Task, 'subtaskCount' | 'subtasksDone' | 'pending'> {
  position: number;
  /** Repeats every this many days (the demo's stand-in for an RRULE). */
  every: number | null;
  pendingUntil: number;
}

let seq = 1;
const tasks: Stored[] = [];

/** The latest message of a demo conversation, for tasks made from mail. */
function sourceOf(subject: string): T.TaskSource | null {
  const thread = threads.find((t) => t.subject === subject);
  const msgs = messages.filter((m) => m.threadId === thread?.id);
  const m = msgs[msgs.length - 1];
  if (!thread || !m) return null;
  return { messageId: m.messageId ?? `${m.id}@demo`, threadId: thread.id, accountId: thread.accountId, subject: thread.subject, from: m.from?.name || m.from?.email || null };
}

interface Seed {
  due?: number | [number, number, number?];
  important?: boolean;
  notes?: string;
  done?: number;
  every?: number;
  source?: string;
  /** Title, done, and the day it is due (CalDAV subtasks can have their own). */
  subtasks?: [string, boolean, number?][];
}

function add(listId: number, title: string, s: Seed = {}, parentId: number | null = null): Stored {
  const due = s.due;
  const dueAt = Array.isArray(due) ? at(due[0], due[1], due[2]) : null;
  const dueDate = Array.isArray(due) ? day(due[0]) : due != null ? day(due) : null;
  const siblings = tasks.filter((t) => t.listId === listId && t.parentId === parentId);
  const created = at(-12 - (seq % 9), 9 + (seq % 8));
  const task: Stored = {
    id: seq++, listId, parentId, title, notes: s.notes ?? '', dueDate, dueAt,
    completedAt: s.done != null ? at(-s.done, 17, 20) : null, important: !!s.important, repeats: s.every != null, every: s.every ?? null,
    source: s.source ? sourceOf(s.source) : null, createdAt: created, updatedAt: created, error: null,
    position: siblings.length, pendingUntil: 0,
  };
  tasks.push(task);
  for (const [subtitle, doneSub, dueSub] of s.subtasks ?? []) add(listId, subtitle, { done: doneSub ? 1 : undefined, due: dueSub }, task.id);
  return task;
}

// The local list.
add(1, 'Renew passport', { due: 12, important: true, notes: 'Book the appointment online and bring two photos.' });
add(1, 'Back up the laptop before the Berlin trip', { due: 14 });
add(1, 'Return the library books', { done: 1 });

// Work: Google Tasks (dates without times, no importance).
const daysToFriday = (5 - today.getDay() + 7) % 7;
add(2, 'Send onboarding notes to Maya', {
  due: 0,
  source: 'New onboarding flow — first look',
  notes: 'She would like them before Friday.',
  subtasks: [['Provider picker with auto-detection', true], ['AI step: during onboarding or after the first sync?', false], ['Dark mode by default?', false]],
});
add(2, 'Review the release v0.1 checklist (#151)', { due: -1 });
add(2, 'Prepare for the Q4 roadmap review', { due: 1, notes: 'Offline search is the headline; calendar sharing moves to Q1.' });
add(2, 'Pick a system design question for the Rust interviews', { due: 2 });
add(2, 'Write the weekly update', { due: daysToFriday });
add(2, 'Book the balloon ride for the offsite', { due: 7 });
add(2, 'Expense the Lumen Cloud receipt');
add(2, 'Merge the design tokens v2', { done: 1 });
add(2, 'Reply to Kenji about the Tokyo visit', { done: 2 });

add(3, 'Draft the release notes', { due: 3 });
add(3, 'Record the demo video', { due: 5 });
add(3, 'Update the screenshots in the README');
add(3, 'Ask Sofia for the new app icon', { done: 3 });

// Personal: CalDAV (times, importance and subtasks).
add(4, 'Reply to Hannah about the apartment viewing', { due: [-1, 12], important: true, source: 'Apartment viewing on Saturday' });
add(4, 'Call Mom about her birthday dinner', { due: [0, 18, 30], important: true });
add(4, 'Look over the Kraftwerk Studio offer', { due: 4 });
add(4, 'Buy a birthday present for Mom', {
  due: 6,
  important: true,
  subtasks: [['Order the photo book', true], ['Pick up the frame', false, 2], ['Wrap it', false]],
});
add(4, 'Online check-in for the Berlin flight', { due: [15, 9, 40], notes: 'Seat 12A · booking SKY7Q2' });
add(4, 'Pay the October credit card bill', { done: 3 });

add(5, 'Water the plants', { due: 0, every: 3 });
add(5, 'Pick up the dry cleaning', { due: [1, 17] });
add(5, 'Fix the balcony door');
add(5, 'Buy coffee beans');

// ----- reading -----------------------------------------------------------------

function listOf(id: number): StoredList {
  const l = lists.find((x) => x.id === id);
  if (!l) throw notFound('Task list not found');
  return l;
}

function find(id: number): Stored {
  const t = tasks.find((x) => x.id === id);
  if (!t) throw notFound('Task not found');
  return t;
}

/** Lists in display order: the local ones first, then by account. */
function ordered(): StoredList[] {
  const accountOrder = (id: number | null) => (id == null ? -1 : (accounts.find((a) => a.id === id)?.sortOrder ?? 99));
  return lists.slice().sort((a, b) => accountOrder(a.accountId) - accountOrder(b.accountId) || a.sortOrder - b.sortOrder || a.id - b.id);
}

function view(t: Stored): T.Task {
  const children = tasks.filter((c) => c.parentId === t.id);
  const { position: _p, every: _e, pendingUntil, ...rest } = t;
  return structuredClone({
    ...rest,
    subtaskCount: children.length,
    subtasksDone: children.filter((c) => c.completedAt != null).length,
    pending: pendingUntil > Date.now(),
  });
}

/** Due time for sorting: the moment, or the end of the day for tasks without a time. */
function dueKey(t: Stored): number {
  if (t.dueDate == null) return Infinity;
  return t.dueAt ?? dayStart(t.dueDate) + DAY - 1;
}

function byDue(a: Stored, b: Stored): number {
  return dueKey(a) - dueKey(b) || Number(b.important) - Number(a.important) || byListOrder(a, b);
}

function byListOrder(a: Stored, b: Stored): number {
  const order = ordered().map((l) => l.id);
  return order.indexOf(a.listId) - order.indexOf(b.listId) || a.position - b.position;
}

export function taskLists(): T.TaskList[] {
  return ordered().map(({ sortOrder: _s, ...l }) => ({
    ...l,
    openCount: tasks.filter((t) => t.listId === l.id && t.completedAt == null).length,
    completedCount: tasks.filter((t) => t.listId === l.id && t.parentId == null && t.completedAt != null).length,
  }));
}

export function query(v: T.TaskView, includeCompleted: boolean): T.Task[] {
  const open = tasks.filter((t) => t.completedAt == null);
  const todayKey = ymd(new Date());
  let out: Stored[];
  if (typeof v === 'object') {
    listOf(v.list);
    out = open.filter((t) => t.listId === v.list && t.parentId == null).sort((a, b) => a.position - b.position);
    if (includeCompleted) {
      const done = tasks
        .filter((t) => t.listId === v.list && t.parentId == null && t.completedAt != null)
        .sort((a, b) => b.completedAt! - a.completedAt!)
        .slice(0, 200);
      out = [...out, ...done];
    }
  } else if (v === 'today') {
    out = open.filter((t) => t.dueDate != null && t.dueDate <= todayKey).sort(byDue);
  } else if (v === 'upcoming') {
    out = open.filter((t) => t.dueDate != null && t.dueDate > todayKey).sort(byDue);
  } else if (v === 'important') {
    out = open.filter((t) => t.important).sort(byDue);
  } else {
    out = open.filter((t) => t.parentId == null).sort(byListOrder);
  }
  return out.map(view);
}

export function due(from: number, to: number): T.Task[] {
  return tasks
    .filter((t) => t.completedAt == null && t.dueDate != null)
    .filter((t) => (t.dueAt != null ? t.dueAt >= from && t.dueAt < to : dayStart(t.dueDate!) < to && dayStart(t.dueDate!) + DAY > from))
    .sort(byDue)
    .map(view);
}

export function get(id: number): T.Task {
  return view(find(id));
}

export function subtasks(id: number): T.Task[] {
  find(id);
  return tasks
    .filter((t) => t.parentId === id)
    .sort((a, b) => a.position - b.position)
    .map(view);
}

// ----- changing ----------------------------------------------------------------

/** Changes on a service's list stay pending for a moment, as if being saved there; `settle` reports when they are done. */
let settle: (listId: number) => void = () => {};
export function onSettled(cb: (listId: number) => void) {
  settle = cb;
}

function touch(t: Stored) {
  t.updatedAt = Date.now();
  if (listOf(t.listId).kind === 'local') return;
  t.pendingUntil = Date.now() + 900;
  setTimeout(() => settle(t.listId), 950);
}

function writable(listId: number): StoredList {
  const l = listOf(listId);
  if (l.readOnly) throw invalid('This list is read-only');
  return l;
}

/**
 * Applies an input with the rules of the target list: a time only where it keeps times, importance
 * only where it has it. A field left out keeps its value, `null` clears it; a day alone clears the time.
 */
function apply(t: Stored, input: T.TaskInput, list: StoredList) {
  t.title = input.title.trim() || t.title || 'Untitled';
  if (input.notes !== undefined) t.notes = input.notes;
  if (input.dueAt != null) {
    t.dueDate = ymd(new Date(input.dueAt));
    t.dueAt = input.dueAt;
  } else if (input.dueDate !== undefined) {
    t.dueDate = input.dueDate;
    t.dueAt = null;
  } else if (input.dueAt === null) {
    t.dueAt = null;
  }
  if (!list.dueTimes) t.dueAt = null;
  if (input.important !== undefined) t.important = !!input.important;
  if (!list.importance) t.important = false;
}

export function create(input: T.TaskInput): T.Task {
  const list = writable(input.listId);
  const parentId = input.parentId ?? null;
  if (parentId != null) {
    const parent = find(parentId);
    if (!list.subtasks) throw invalid('This list has no subtasks');
    if (parent.parentId != null) throw invalid('Subtasks cannot have subtasks');
  }
  // New tasks go to the end of their list, or of their parent's subtasks.
  const siblings = tasks.filter((t) => t.listId === input.listId && t.parentId === parentId);
  const position = Math.max(-1, ...siblings.map((s) => s.position)) + 1;
  const now = Date.now();
  const t: Stored = {
    id: seq++, listId: input.listId, parentId, title: '', notes: '', dueDate: null, dueAt: null, completedAt: null, important: false,
    repeats: false, every: null, source: null, createdAt: now, updatedAt: now, error: null, position, pendingUntil: 0,
  };
  apply(t, input, list);
  if (input.fromMessage != null) {
    const m = messages.find((x) => x.id === input.fromMessage);
    const thread = m && threads.find((x) => x.id === m.threadId);
    if (m) t.source = { messageId: m.messageId ?? `${m.id}@demo`, threadId: thread?.id ?? null, accountId: m.accountId, subject: thread?.subject ?? m.subject, from: m.from?.name || m.from?.email || null };
  }
  tasks.push(t);
  touch(t);
  return view(t);
}

export function update(id: number, input: T.TaskInput): T.Task {
  const t = find(id);
  writable(t.listId);
  const list = writable(input.listId);
  if (input.listId !== t.listId) {
    if (t.parentId != null) throw invalid('Subtasks stay in the list of their task');
    const target = tasks.filter((x) => x.listId === input.listId && x.parentId == null);
    t.position = Math.max(-1, ...target.map((x) => x.position)) + 1;
    const children = tasks.filter((x) => x.parentId === t.id);
    // Moved to another service (or account) the task is a new one there, with a new id.
    if (listOf(t.listId).kind !== 'local' || list.kind !== 'local') {
      t.id = seq++;
      for (const c of children) c.id = seq++;
    }
    for (const c of children) {
      c.parentId = t.id;
      c.listId = input.listId;
    }
    t.listId = input.listId;
    if (!list.subtasks) for (const c of children) tasks.splice(tasks.indexOf(c), 1);
  }
  apply(t, input, list);
  touch(t);
  return view(t);
}

/** The next due day of a repeating task: one period on, and after today. */
function advance(t: Stored) {
  if (!t.every || !t.dueDate) return;
  const todayKey = ymd(new Date());
  let start = dayStart(t.dueDate);
  const time = t.dueAt != null ? t.dueAt - start : null;
  do {
    const d = new Date(start);
    start = new Date(d.getFullYear(), d.getMonth(), d.getDate() + t.every).getTime();
  } while (ymd(new Date(start)) <= todayKey);
  t.dueDate = ymd(new Date(start));
  if (time != null) t.dueAt = start + time;
}

export function complete(id: number, completed: boolean): T.Task {
  const t = find(id);
  const list = writable(t.listId);
  // Only CalDAV keeps the rule that moves a task on; elsewhere it is simply done.
  if (completed && t.repeats && list.kind === 'caldav' && t.dueDate) advance(t);
  else t.completedAt = completed ? Date.now() : null;
  touch(t);
  return view(t);
}

export function reorder(id: number, beforeId: number | null) {
  const t = find(id);
  writable(t.listId);
  const siblings = tasks.filter((x) => x.listId === t.listId && x.parentId === t.parentId && x.id !== t.id).sort((a, b) => a.position - b.position);
  const i = beforeId == null ? siblings.length : siblings.findIndex((x) => x.id === beforeId);
  siblings.splice(i < 0 ? siblings.length : i, 0, t);
  siblings.forEach((x, n) => (x.position = n));
  touch(t);
}

export function remove(id: number) {
  const t = find(id);
  writable(t.listId);
  for (const x of tasks.filter((c) => c.id === id || c.parentId === id)) tasks.splice(tasks.indexOf(x), 1);
}

export function listIdOf(id: number): number {
  return find(id).listId;
}

const palette = ['#8b5cf6', '#10b981', '#ef4444', '#06b6d4', '#84cc16', '#f97316'];

export function createList(accountId: number | null, name: string): T.TaskList {
  const account = accountId == null ? null : accounts.find((a) => a.id === accountId);
  if (accountId != null && !account) throw notFound('Account not found');
  if (!name.trim()) throw invalid('A list needs a name');
  const kind: T.TaskListKind = !account ? 'local' : account.kind === 'gmail' ? 'google' : account.kind === 'microsoft' ? 'graph' : account.kind === 'exchange' ? 'ews' : 'caldav';
  const siblings = lists.filter((l) => l.accountId === accountId);
  const list = L(Math.max(...lists.map((l) => l.id)) + 1, accountId, kind, name.trim(), palette[lists.length % palette.length], false, siblings.length);
  lists.push(list);
  return taskLists().find((l) => l.id === list.id)!;
}

export function renameList(id: number, name: string) {
  const l = writable(id);
  if (!name.trim()) throw invalid('A list needs a name');
  l.name = name.trim();
}

export function removeList(id: number) {
  const l = writable(id);
  if (l.isDefault) throw invalid(l.kind === 'local' ? 'The local list cannot be deleted' : 'The default list of an account cannot be deleted');
  lists.splice(lists.indexOf(l), 1);
  for (const t of tasks.filter((x) => x.listId === id)) tasks.splice(tasks.indexOf(t), 1);
}

/** Deletes the completed top-level tasks of a list with their subtasks (completed subtasks of open tasks stay). */
export function clearCompleted(listId: number) {
  writable(listId);
  const done = new Set(tasks.filter((t) => t.listId === listId && t.parentId == null && t.completedAt != null).map((t) => t.id));
  for (const t of tasks.filter((x) => done.has(x.id) || (x.parentId != null && done.has(x.parentId)))) tasks.splice(tasks.indexOf(t), 1);
}

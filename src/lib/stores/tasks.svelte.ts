import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { api } from '../api';
import type { ApiError, Task, TaskInput, TaskList, TaskView } from '../api';
import { t } from '../i18n';
import { capitalize, dayDate, dayKey, daysFromToday, dueDay, dueLabel, relativeDay } from '../utils/due';
import { dateFormat } from '../utils/format';
import { mail } from './mail.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';

export type SmartView = 'today' | 'upcoming' | 'important' | 'all';
export const smartViews: SmartView[] = ['today', 'upcoming', 'important', 'all'];

/** How long a completed task shows its check and struck-through title before it moves. */
const COMPLETE_MS = 720;
/** How long the undo of a delete is offered (the engine keeps a deleted task 12 seconds). */
const UNDO_MS = 6000;

/** A list's name in the interface: the local list is named in the interface language. */
export function listName(l: TaskList | null | undefined): string {
  if (!l) return '';
  return l.kind === 'local' && l.isDefault ? t('tasks.localList') : l.name;
}

export interface TaskGroup {
  key: string;
  title: string;
  /** The date under a day's name, or the account of a list. */
  sub: string;
  tone: 'overdue' | 'today' | null;
  /** A list's color, when the group is a list. */
  color: string | null;
  items: Task[];
}

const sameView = (a: TaskView, b: TaskView) => (typeof a === 'object' && typeof b === 'object' ? a.list === b.list : a === b);

class Tasks {
  lists = $state<TaskList[]>([]);
  view = $state<TaskView>('today');
  /** The view's tasks in display order (with the completed ones after, for a list). */
  items = $state<Task[]>([]);
  counts = $state<Record<SmartView, number>>({ today: 0, upcoming: 0, important: 0, all: 0 });
  /** Open top-level tasks of every list (for the palette, and finding tasks made from mail). */
  all = $state<Task[]>([]);
  loading = $state(true);
  /** The task the keyboard acts on. */
  selectedId = $state<number | null>(null);
  /** The task in the detail panel, and its subtasks. */
  openId = $state<number | null>(null);
  detail = $state<Task | null>(null);
  subtasks = $state<Task[]>([]);
  /** The completed tasks of a list are shown. */
  showCompleted = $state(false);
  /** Tasks playing their completion. */
  completing = new SvelteSet<number>();
  /** Quick add should take the focus (once it is on screen). */
  focusAdd = $state(false);
  /** A task just added, shown and briefly highlighted. */
  fresh = $state<number | null>(null);
  /** Moved tasks get a new id on most services: the new id of an old one. */
  movedTo = new SvelteMap<number, number>();
  /** Ticks every minute, so "overdue" and the day groups follow the clock. */
  now = $state(Date.now());
  /** The day the views were read on: Today and Upcoming are read again when it is over. */
  private day = dayKey(Date.now());
  private seq = 0;
  private started = false;
  private stale = false;
  private timer: ReturnType<typeof setTimeout> | null = null;

  listById = $derived(new Map(this.lists.map((l) => [l.id, l])));
  writable = $derived(this.lists.filter((l) => !l.readOnly));
  currentList = $derived(typeof this.view === 'object' ? (this.listById.get(this.view.list) ?? null) : null);
  /** Where quick add puts tasks outside a list: the list chosen last, else an account's default list, else the local one. */
  defaultList = $derived(
    this.writable.find((l) => l.id === ui.settings.defaultTaskListId) ??
      this.writable.find((l) => l.isDefault && l.accountId != null) ??
      this.writable.find((l) => l.isDefault) ??
      this.writable[0] ??
      null,
  );
  addTarget = $derived(this.currentList ?? this.defaultList);
  /** Open tasks made from mail, by conversation. */
  linked = $derived(new Map(this.all.filter((x) => x.source?.threadId != null).map((x) => [x.source!.threadId!, x])));

  open = $derived(this.items.filter((x) => x.completedAt == null));
  done = $derived(this.items.filter((x) => x.completedAt != null));
  /**
   * Completed tasks of the list in view: all of them, though only the latest are loaded, or those
   * on screen while the list's count catches up with a change.
   */
  doneCount = $derived(Math.max(this.currentList?.completedCount ?? 0, this.done.length));

  groups = $derived.by((): TaskGroup[] => {
    const view = this.view;
    if (typeof view === 'object') return [{ key: 'list', title: '', sub: '', tone: null, color: null, items: this.open }];
    if (view === 'all') {
      const out: TaskGroup[] = [];
      for (const l of this.lists) {
        const items = this.open.filter((x) => x.listId === l.id);
        if (!items.length) continue;
        const account = l.accountId != null ? mail.accountById.get(l.accountId) : null;
        out.push({ key: `l${l.id}`, title: listName(l), sub: account ? (account.label ?? account.email) : '', tone: null, color: l.color, items });
      }
      return out;
    }
    const today = dayKey(this.now);
    const out: TaskGroup[] = [];
    for (const task of this.open) {
      const day = dueDay(task);
      const key = !day ? 'none' : day < today ? 'overdue' : day;
      let g = out.find((x) => x.key === key);
      if (!g) out.push((g = { key, ...this.dayHeading(key), color: null, items: [] }));
      g.items.push(task);
    }
    return out;
  });

  /** Tasks in the order they are shown, for moving through them with the keyboard. */
  visible = $derived([...this.groups.flatMap((g) => g.items), ...(this.currentList && this.showCompleted ? this.done : [])]);

  title = $derived.by(() => {
    const v = this.view;
    if (typeof v === 'object') return listName(this.currentList);
    return t(`tasks.view.${v}`);
  });

  private dayHeading(key: string): Pick<TaskGroup, 'title' | 'sub' | 'tone'> {
    if (key === 'none') return { title: t('tasks.group.noDate'), sub: '', tone: null };
    if (key === 'overdue') return { title: t('tasks.group.overdue'), sub: '', tone: 'overdue' };
    const n = daysFromToday(key, this.now);
    const d = dayDate(key);
    const full = dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(d);
    if (n <= 1) return { title: capitalize(relativeDay(n)), sub: full, tone: n === 0 ? 'today' : null };
    if (n < 7) return { title: capitalize(dateFormat({ weekday: 'long' }).format(d)), sub: dateFormat({ month: 'long', day: 'numeric' }).format(d), tone: null };
    const year = d.getFullYear() === new Date(this.now).getFullYear() ? undefined : 'numeric';
    return { title: capitalize(dateFormat({ weekday: 'long', month: 'long', day: 'numeric', year }).format(d)), sub: '', tone: null };
  }

  async init() {
    if (this.started) return;
    this.started = true;
    api.onEvent((e) => {
      if (e.type === 'tasksChanged' || e.type === 'accountsChanged') this.refreshSoon();
    });
    setInterval(() => this.tick(), 60_000);
    await this.refresh();
  }

  /** Follows the clock, and reads the views again on a new day (also on coming back to Tasks, in case the timer slept). */
  tick() {
    this.now = Date.now();
    if (this.started && dayKey(this.now) !== this.day) this.refresh();
  }

  refreshSoon() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => this.refresh(), 60);
  }

  async refresh() {
    // A completing task keeps its place until its animation is over.
    if (this.completing.size) {
      this.stale = true;
      return;
    }
    const seq = ++this.seq;
    const view = this.view;
    this.day = dayKey(Date.now());
    try {
      const [lists, items, ...smart] = await Promise.all([
        api.tasks.lists(),
        api.tasks.query(view, typeof view === 'object'),
        ...smartViews.map((v) => api.tasks.query(v, false)),
      ]);
      if (seq !== this.seq) return;
      this.lists = lists;
      if (typeof view === 'object' && !lists.some((l) => l.id === view.list)) {
        this.setView('today');
        return;
      }
      this.items = items;
      this.counts = { today: smart[0].length, upcoming: smart[1].length, important: smart[2].length, all: smart[3].length };
      this.all = smart[3];
      this.loading = false;
      if (this.openId != null) this.loadDetail(this.openId);
    } catch (e) {
      if (seq === this.seq) {
        this.loading = false;
        toasts.error(errorMessage(e));
      }
    }
  }

  private settle() {
    if (this.stale && !this.completing.size) {
      this.stale = false;
      this.refresh();
    }
  }

  setView(view: TaskView): Promise<void> {
    if (!sameView(view, this.view)) {
      this.view = view;
      this.items = [];
      this.loading = true;
      this.showCompleted = false;
    }
    this.selectedId = null;
    this.closeDetail();
    ui.view = 'tasks';
    return this.refresh();
  }

  isView(view: TaskView): boolean {
    return sameView(view, this.view);
  }

  /** Goes to Tasks with the cursor in quick add. */
  newTask() {
    ui.view = 'tasks';
    this.focusAdd = true;
  }

  // ----- selection and the detail panel ---------------------------------------------

  select(id: number | null) {
    this.selectedId = id;
    if (id != null && this.openId != null) this.openTask(id);
  }

  /** Moves the selection; an open detail panel follows it. */
  step(dir: 1 | -1) {
    const rows = this.visible;
    if (!rows.length) return;
    const i = rows.findIndex((x) => x.id === this.selectedId);
    const next = i < 0 ? (dir > 0 ? rows[0] : rows[rows.length - 1]) : rows[Math.max(0, Math.min(rows.length - 1, i + dir))];
    this.select(next.id);
  }

  selected(): Task | undefined {
    return this.items.find((x) => x.id === this.selectedId) ?? (this.detail?.id === this.selectedId ? this.detail : undefined);
  }

  /**
   * The task the keyboard completes, stars and deletes: the selected one, or the subtask in the
   * detail panel when it has no row of its own and its task's row is selected instead (as when
   * a notification or the calendar opened it).
   */
  target(): Task | undefined {
    const d = this.detail;
    if (d && this.openId === d.id && d.parentId != null && d.parentId === this.selectedId && !this.visible.some((x) => x.id === d.id)) return d;
    return this.selected();
  }

  async openTask(id: number) {
    this.openId = id;
    if (this.items.some((x) => x.id === id)) this.selectedId = id;
    if (this.detail?.id !== id) {
      this.detail = this.items.find((x) => x.id === id) ?? this.subtasks.find((x) => x.id === id) ?? null;
      this.subtasks = [];
    }
    await this.loadDetail(id);
  }

  private async loadDetail(id: number) {
    try {
      const [task, subs] = await Promise.all([api.tasks.get(id), api.tasks.subtasks(id)]);
      if (this.openId !== id) return;
      this.detail = task;
      this.subtasks = subs;
    } catch (e) {
      if (this.openId !== id) return;
      if ((e as ApiError)?.kind === 'notFound') this.closeDetail();
      else toasts.error(errorMessage(e));
    }
  }

  closeDetail() {
    this.openId = null;
    this.detail = null;
    this.subtasks = [];
  }

  /** Shows a task in Tasks mode, in the current view when it is there, else in its list. */
  async reveal(id: number) {
    ui.view = 'tasks';
    await this.init();
    let task: Task;
    try {
      task = await api.tasks.get(id);
    } catch (e) {
      toasts.error(errorMessage(e));
      return;
    }
    // A subtask is reached through its task's row where it has none of its own; both are in the same list.
    const shown = (id: number | null) => id != null && this.items.some((x) => x.id === id);
    if (!shown(task.id) && !shown(task.parentId)) {
      if (task.completedAt != null) this.showCompleted = true;
      await this.setView({ list: task.listId });
      if (task.completedAt != null) this.showCompleted = true;
    }
    this.selectedId = shown(task.id) ? task.id : (task.parentId ?? task.id);
    await this.openTask(task.id);
  }

  // ----- changes -------------------------------------------------------------------------

  /** Whether a task is part of a view, as the engine decides it. */
  belongs(task: Task, view: TaskView = this.view): boolean {
    if (typeof view === 'object') return task.listId === view.list && task.parentId == null;
    if (task.completedAt != null) return false;
    const today = dayKey(this.now);
    switch (view) {
      case 'today':
        return task.dueDate != null && task.dueDate <= today;
      case 'upcoming':
        return task.dueDate != null && task.dueDate > today;
      case 'important':
        return task.important;
      case 'all':
        return task.parentId == null;
    }
  }

  /** Shows a task's new state wherever it appears. */
  private put(task: Task) {
    const i = this.items.findIndex((x) => x.id === task.id);
    const keep = this.belongs(task);
    const nowDone = task.completedAt != null && i >= 0 && this.items[i].completedAt == null;
    if (keep && nowDone) {
      // Just completed: first among the completed ones, as the engine orders them.
      const rest = this.items.filter((x) => x.id !== task.id);
      const at = rest.findIndex((x) => x.completedAt != null);
      rest.splice(at < 0 ? rest.length : at, 0, task);
      this.items = rest;
    } else if (i >= 0) {
      if (keep) this.items[i] = task;
      else this.items.splice(i, 1);
    } else if (keep && typeof this.view === 'object') {
      // New in a list: after its open tasks, where the engine puts it. In the other views
      // the refresh that follows every change puts it in its place.
      const at = this.items.findIndex((x) => x.completedAt != null);
      this.items.splice(at < 0 ? this.items.length : at, 0, task);
    }
    if (this.detail?.id === task.id) this.detail = task;
    const s = this.subtasks.findIndex((x) => x.id === task.id);
    if (s >= 0) this.subtasks[s] = task;
  }

  private writableList(task: Task): boolean {
    const list = this.listById.get(task.listId);
    if (list?.readOnly) {
      toasts.show(t('tasks.toast.readOnly', { list: listName(list) }));
      return false;
    }
    return true;
  }

  private inputOf(task: Task): TaskInput {
    return { listId: task.listId, parentId: task.parentId, title: task.title, notes: task.notes, dueDate: task.dueDate, dueAt: task.dueAt, important: task.important };
  }

  /** Saves a change: shown at once, written in the background. */
  async save(task: Task, patch: Partial<TaskInput>): Promise<Task | null> {
    if (!this.writableList(task)) return null;
    const input: TaskInput = { ...this.inputOf(task), ...patch };
    const dueDate = input.dueAt != null ? dayKey(input.dueAt) : (input.dueDate ?? null);
    this.put({ ...task, listId: input.listId, title: input.title, notes: input.notes ?? '', dueDate, dueAt: input.dueAt ?? null, important: !!input.important });
    try {
      const saved = await api.tasks.update(task.id, input);
      if (saved.id !== task.id) this.renumber(task.id, saved.id);
      this.put(saved);
      return saved;
    } catch (e) {
      toasts.error(errorMessage(e));
      this.put(task);
      this.refreshSoon();
      return null;
    }
  }

  /** A moved task came back with a new id: the selection and the detail panel go with it. */
  private renumber(from: number, to: number) {
    this.movedTo.set(from, to);
    this.items = this.items.filter((x) => x.id !== from);
    if (this.selectedId === from) this.selectedId = to;
    if (this.openId === from) {
      this.openId = to;
      this.loadDetail(to);
    }
  }

  async toggleImportant(task: Task) {
    const list = this.listById.get(task.listId);
    if (list && !list.importance) {
      toasts.show(t('tasks.toast.noImportance', { list: listName(list) }));
      return;
    }
    await this.save(task, { important: !task.important });
  }

  /** Completes a task (or reopens a completed one): the check and the line through it play, then it moves. */
  async complete(task: Task) {
    if (this.completing.has(task.id) || !this.writableList(task)) return;
    if (task.completedAt != null) return this.setCompleted(task, false);
    const before = this.visible.filter((x) => x.completedAt == null);
    const index = before.findIndex((x) => x.id === task.id);
    this.completing.add(task.id);
    const [result] = await Promise.allSettled([api.tasks.complete(task.id, true), new Promise((r) => setTimeout(r, COMPLETE_MS))]);
    this.completing.delete(task.id);
    if (result.status === 'rejected') {
      toasts.error(errorMessage(result.reason));
      this.settle();
      return;
    }
    const updated = result.value as Task;
    this.put(updated);
    // The selection moves on to the next task.
    if (this.selectedId === task.id && !this.open.some((x) => x.id === task.id)) {
      const rest = before.filter((x) => x.id !== task.id);
      this.selectedId = rest[Math.min(index, rest.length - 1)]?.id ?? null;
    }
    if (updated.completedAt == null) {
      // A repeating task comes back with its next due date.
      toasts.undoable(t('tasks.toast.repeats', { when: dueLabel(updated, 'day') }), () => this.save(updated, { dueDate: task.dueDate, dueAt: task.dueAt }));
    } else {
      toasts.undoable(t('tasks.toast.completed'), () => this.setCompleted(updated, false));
    }
    this.stale = true;
    this.settle();
  }

  async setCompleted(task: Task, completed: boolean) {
    if (!this.writableList(task)) return;
    try {
      this.put(await api.tasks.complete(task.id, completed));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
    this.refreshSoon();
  }

  /** Moves a task in its list before another (null: after the last open one). */
  async reorder(task: Task, beforeId: number | null) {
    if (beforeId === task.id || !this.writableList(task)) return;
    const prev = this.items;
    const rest = prev.filter((x) => x.id !== task.id);
    const at = beforeId == null ? rest.findIndex((x) => x.completedAt != null) : rest.findIndex((x) => x.id === beforeId);
    rest.splice(at < 0 ? rest.length : at, 0, task);
    this.items = rest;
    try {
      await api.tasks.reorder(task.id, beforeId);
    } catch (e) {
      this.items = prev;
      toasts.error(errorMessage(e));
    }
  }

  /** Moves the selected task one place up or down in its list. */
  nudge(dir: 1 | -1) {
    const task = this.selected();
    if (!task || !this.currentList || task.completedAt != null) return;
    const open = this.open;
    const i = open.findIndex((x) => x.id === task.id);
    if (i < 0 || i + dir < 0 || i + dir >= open.length) return;
    this.reorder(task, dir < 0 ? open[i - 1].id : (open[i + 2]?.id ?? null));
  }

  /** Deletes a task and its subtasks at once; for a few seconds the engine can bring them back. */
  async remove(task: Task) {
    if (!this.writableList(task)) return;
    const rows = this.visible;
    const index = rows.findIndex((x) => x.id === task.id);
    // Its subtasks go with it, also where they have rows of their own (Today, Upcoming).
    const gone = (x: Task) => x.id === task.id || x.parentId === task.id;
    // A subtask's task counts one less ("1/2").
    const fewer = (x: Task): Task =>
      x.id === task.parentId ? { ...x, subtaskCount: x.subtaskCount - 1, subtasksDone: x.subtasksDone - (task.completedAt != null ? 1 : 0) } : x;
    this.items = this.items.filter((x) => !gone(x)).map(fewer);
    this.subtasks = this.subtasks.filter((x) => x.id !== task.id);
    if (this.detail) this.detail = fewer(this.detail);
    if (rows.some((x) => x.id === this.selectedId && gone(x))) {
      const rest = rows.filter((x) => !gone(x));
      this.selectedId = rest[Math.min(index, rest.length - 1)]?.id ?? null;
    }
    if (this.openId === task.id || (this.detail && gone(this.detail))) this.closeDetail();
    try {
      await api.tasks.remove(task.id);
    } catch (e) {
      toasts.error(errorMessage(e));
      this.refresh();
      return;
    }
    this.refreshSoon();
    toasts.undoable(t('tasks.toast.deleted'), () => this.restore(task.id), UNDO_MS);
  }

  /** Brings back a task deleted a moment ago, with the subtasks deleted with it. */
  private async restore(id: number) {
    try {
      await api.tasks.restore(id);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
    await this.refresh();
  }

  /** Adds a task from quick add; one that will not show in this view is announced. */
  async add(input: TaskInput): Promise<Task | null> {
    try {
      const task = await api.tasks.create(input);
      this.put(task);
      if (this.belongs(task)) {
        this.fresh = task.id;
        setTimeout(() => this.fresh === task.id && (this.fresh = null), 1600);
      } else {
        const list = this.listById.get(task.listId);
        const message = task.dueDate
          ? t('tasks.toast.addedToDue', { list: listName(list), when: dueLabel(task) })
          : t('tasks.toast.addedTo', { list: listName(list) });
        toasts.show(message, { action: { label: t('tasks.toast.show'), run: () => this.reveal(task.id) } });
      }
      return task;
    } catch (e) {
      toasts.error(errorMessage(e));
      return null;
    }
  }

  async addSubtask(parent: Task, title: string): Promise<Task | null> {
    try {
      const task = await api.tasks.create({ listId: parent.listId, parentId: parent.id, title });
      if (this.openId === parent.id) this.subtasks = [...this.subtasks, task];
      return task;
    } catch (e) {
      toasts.error(errorMessage(e));
      return null;
    }
  }

  /** The list a task made from an account's mail goes to: that account's own, else the default one. */
  listFor(accountId: number | null): TaskList | null {
    return this.writable.find((l) => l.accountId === accountId && l.isDefault) ?? this.defaultList;
  }

  /** Conversations being made into tasks (a second T while the first is saved adds nothing). */
  private adding = new Set<number>();

  /** Makes a task of each conversation: titled with the subject, linked to its latest message. */
  async addFromMail(threadIds: number[]) {
    await this.init();
    const made: Task[] = [];
    const existing: Task[] = [];
    const mine: number[] = [];
    try {
      for (const id of threadIds) {
        const linked = this.linked.get(id);
        if (linked) existing.push(linked);
        if (linked || this.adding.has(id)) continue;
        this.adding.add(id);
        mine.push(id);
        try {
          const d = mail.detail?.thread.id === id ? mail.detail : await api.thread(id);
          const list = this.listFor(d.thread.accountId);
          if (!list) {
            toasts.error(t('tasks.toast.noList'));
            break;
          }
          // Without the "Re:" and "Fwd:" (and their translations) replies and forwards put in front.
          const title = d.thread.subject.replace(/^((re|fwd?|aw|wg|tr|rv|enc|odp|ynt)\s*:\s*)+/i, '').trim() || t('common.noSubject');
          const last = d.messages[d.messages.length - 1];
          made.push(await api.tasks.create({ listId: list.id, title, fromMessage: last?.id ?? null }));
        } catch (e) {
          toasts.error(errorMessage(e));
        }
      }
      // Known as linked once the tasks are read again.
      await this.refresh();
    } finally {
      for (const id of mine) this.adding.delete(id);
    }
    const shown = made[0] ?? existing[0];
    if (!shown) return;
    const open = { label: t('common.open'), run: () => this.reveal(shown.id) };
    if (!made.length) toasts.show(t('tasks.toast.alreadyAdded'), { action: open });
    else if (made.length === 1) toasts.show(t('tasks.toast.addedTo', { list: listName(this.listById.get(made[0].listId)) }), { kind: 'success', action: open });
    else toasts.show(t('tasks.toast.fromMailCount', { count: made.length }), { kind: 'success', action: open });
  }

  async clearCompleted(list: TaskList) {
    const count = this.isView({ list: list.id }) ? this.doneCount : list.completedCount;
    try {
      await api.tasks.clearCompleted(list.id);
      this.items = this.items.filter((x) => x.completedAt == null);
      toasts.show(t('tasks.toast.cleared', { count }));
    } catch (e) {
      toasts.error(errorMessage(e));
    }
    this.refreshSoon();
  }

  // ----- lists -----------------------------------------------------------------------

  async createList(accountId: number | null, name: string): Promise<TaskList | null> {
    try {
      const list = await api.tasks.createList(accountId, name.trim());
      this.lists = [...this.lists, list];
      this.setView({ list: list.id });
      return list;
    } catch (e) {
      toasts.error(errorMessage(e));
      return null;
    }
  }

  async renameList(list: TaskList, name: string) {
    const prev = list.name;
    const i = this.lists.findIndex((l) => l.id === list.id);
    if (i >= 0) this.lists[i] = { ...list, name: name.trim() };
    try {
      await api.tasks.renameList(list.id, name.trim());
    } catch (e) {
      if (i >= 0) this.lists[i] = { ...list, name: prev };
      toasts.error(errorMessage(e));
    }
  }

  async removeList(list: TaskList) {
    try {
      await api.tasks.removeList(list.id);
      toasts.show(t('tasks.toast.listDeleted', { list: listName(list) }));
      if (this.isView({ list: list.id })) this.setView('today');
      else this.refresh();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
}

export const tasks = new Tasks();

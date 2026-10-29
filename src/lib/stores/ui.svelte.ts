import { MediaQuery } from 'svelte/reactivity';
import { api } from '../api';
import type { SortOrder } from '../api';
import { setLanguage, type LanguageSetting } from '../i18n';

export type Theme = 'system' | 'light' | 'dark';
export type Density = 'comfortable' | 'compact';
/** Where an open conversation goes: beside the list, below it, or in its place. */
export type ReadingPane = 'right' | 'bottom' | 'off';

/** A reusable message body (with an optional subject). */
export interface Template {
  id: string;
  name: string;
  subject: string;
  html: string;
}

/** What a rule looks at. */
export type RuleField = 'from' | 'to' | 'subject' | 'any';
export type RuleOp = 'contains' | 'notContains' | 'is' | 'endsWith';
export interface RuleCondition {
  field: RuleField;
  op: RuleOp;
  value: string;
}
export type RuleAction =
  | { kind: 'move'; folderId: number }
  | { kind: 'label'; folderId: number }
  | { kind: 'archive' }
  | { kind: 'trash' }
  | { kind: 'spam' }
  | { kind: 'read' }
  | { kind: 'star' }
  | { kind: 'mute' };
/** A filter applied by the sync engine to newly arrived mail (stored as `ui.rules`). */
export interface MailRule {
  id: string;
  name: string;
  enabled: boolean;
  /** Only for this account; null = all accounts. */
  accountId: number | null;
  match: 'all' | 'any';
  conditions: RuleCondition[];
  actions: RuleAction[];
  /** Don't run later rules on a message this one matched. */
  stop: boolean;
}

/** A search kept in the sidebar. */
export interface SavedSearch {
  id: string;
  name: string;
  query: string;
}

/** User preferences persisted through the backend settings store. */
export interface Settings {
  /** Interface language; 'system' follows the desktop's preferred languages. */
  language: LanguageSetting;
  theme: Theme;
  accentHue: number;
  density: Density;
  splitInbox: boolean;
  /** Replies are listed with what they answer; off, the list shows messages one by one. */
  conversationView: boolean;
  readingPane: ReadingPane;
  /** The order of each list not newest first, by `listKey`. */
  sortOrders: Record<string, SortOrder>;
  markReadDelayMs: number; // -1 = never automatically
  undoSendSeconds: number;
  loadRemoteImages: 'ask' | 'always' | 'never';
  trustedSenders: string[];
  defaultReply: 'reply' | 'replyAll';
  notifications: boolean;
  calendarNotifications: boolean;
  /** Notify when a task due at a time of day comes due. */
  taskNotifications: boolean;
  nativeTitlebar: boolean;
  showAvatars: boolean;
  previewLines: number;
  sendAndArchive: boolean;
  darkEmails: boolean;
  weekStart: number; // 0 = Sunday, 1 = Monday, 6 = Saturday
  defaultCalendarId: number | null;
  defaultEventMinutes: number;
  defaultAlarm: number; // minutes before; -1 = none
  showDeclined: boolean;
  /** Open tasks with a due date appear in the calendar. */
  calendarTasks: boolean;
  /** The day and week views show the hours of `secondTimeZone` too. */
  showSecondTimeZone: boolean;
  /** An IANA zone ("America/New_York"). */
  secondTimeZone: string;
  /** Where quick add puts tasks outside a list (the last list chosen there). */
  defaultTaskListId: number | null;
  templates: Template[];
  rules: MailRule[];
  /** New-mail notifications play the desktop's new-mail sound. */
  notificationSound: boolean;
  /** Envelope asks GitHub once a day whether a newer version is out. */
  checkUpdates: boolean;
  /** The newer version whose notice was put away. */
  dismissedUpdate: string;
  /** The calendar shows the birthdays from the address books. */
  calendarBirthdays: boolean;
  /** The calendar shows week numbers (ISO 8601). */
  calendarWeekNumbers: boolean;
  /** Senders unsubscribed from in Subscriptions, by address, with when. */
  unsubscribed: Record<string, number>;
  /** Searches kept in the sidebar. */
  savedSearches: SavedSearch[];
  /** The last searches made, newest first. */
  recentSearches: string[];
  /** Closing the window keeps Envelope running in the tray. */
  closeToTray: boolean;
  sidebarWidth: number;
  listWidth: number;
  /** The list's height with the reading pane below it. */
  listHeight: number;
  aiEnabled: boolean;
}

export const defaultSettings: Settings = {
  language: 'system',
  theme: 'system',
  accentHue: 280,
  density: 'comfortable',
  splitInbox: true,
  conversationView: true,
  readingPane: 'right',
  sortOrders: {},
  markReadDelayMs: 600,
  undoSendSeconds: 5,
  loadRemoteImages: 'ask',
  trustedSenders: [],
  defaultReply: 'reply',
  notifications: true,
  calendarNotifications: true,
  taskNotifications: true,
  nativeTitlebar: false,
  showAvatars: true,
  previewLines: 1,
  sendAndArchive: false,
  darkEmails: false,
  weekStart: 1,
  defaultCalendarId: null,
  defaultEventMinutes: 60,
  defaultAlarm: 10,
  showDeclined: true,
  calendarTasks: true,
  showSecondTimeZone: false,
  secondTimeZone: 'UTC',
  defaultTaskListId: null,
  templates: [],
  notificationSound: true,
  unsubscribed: {},
  checkUpdates: true,
  dismissedUpdate: '',
  calendarBirthdays: true,
  calendarWeekNumbers: false,
  savedSearches: [],
  recentSearches: [],
  rules: [],
  closeToTray: false,
  sidebarWidth: 236,
  listWidth: 392,
  listHeight: 320,
  aiEnabled: true,
};

export type View = 'mail' | 'calendar' | 'contacts' | 'tasks';
export type Overlay = null | 'settings' | 'palette' | 'shortcuts' | 'onboarding' | 'addAccount' | 'ask' | 'subscriptions';
/** A window: the main one, or one of its own for a conversation or a message being written. */
export type WindowKind = 'main' | 'thread' | 'compose';

/** Which window this is, from its address (`?window=thread&id=12`, see src-tauri/src/windows.rs). */
function windowOf(): { kind: WindowKind; id: number | null } {
  const params = new URLSearchParams(typeof location === 'undefined' ? '' : location.search);
  const kind = params.get('window');
  const id = Number(params.get('id'));
  return { kind: kind === 'thread' || kind === 'compose' ? kind : 'main', id: Number.isInteger(id) && id > 0 ? id : null };
}

class Ui {
  /** This window, and the conversation or draft it is for. */
  readonly window: WindowKind;
  readonly windowId: number | null;
  view = $state<View>('mail');
  overlay = $state<Overlay>(null);
  settingsTab = $state('general');
  /** A rule to open in the rules editor (from "Create rule…" on a message). */
  rulesDraft = $state<MailRule | null>(null);
  sidebarCollapsed = $state(false);
  /** In a narrow window (half a screen): the sidebar is a rail of icons, and opens over the rest on demand. */
  private wide = new MediaQuery('min-width: 1200px', true);
  /** Narrower still: the conversation list and the open conversation take turns instead of sharing the window. */
  private roomy = new MediaQuery('min-width: 1000px', true);
  /** The rail's full sidebar is open over the window. */
  railOpen = $state(false);
  settings = $state<Settings>({ ...defaultSettings });
  systemDark = $state(typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: dark)').matches);
  maximized = $state(false);
  focusSearch = $state(0);
  askAi = $state('');

  isDark = $derived(this.settings.theme === 'dark' || (this.settings.theme === 'system' && this.systemDark));
  /** Told of settings another window changed. */
  private listeners = new Set<(key: keyof Settings) => void>();

  constructor() {
    const w = windowOf();
    this.window = w.kind;
    this.windowId = w.id;
  }

  get rail(): boolean {
    return !this.wide.current;
  }

  /** One pane at a time: the list, or the conversation opened from it (with no reading pane, or a narrow window). */
  get single(): boolean {
    return !this.roomy.current || this.settings.readingPane === 'off';
  }

  /** The open conversation is below the list (of the main window). */
  get paneBelow(): boolean {
    return this.window === 'main' && !this.single && this.settings.readingPane === 'bottom';
  }

  /** Hidden by the person, so the views offer to show it again (the rail always shows). */
  get sidebarHidden(): boolean {
    return this.sidebarCollapsed && !this.rail;
  }

  async load() {
    try {
      const stored = await api.settings();
      this.settings = { ...defaultSettings, ...(stored as Partial<Settings>) };
    } catch {
      /* first run */
    }
    await setLanguage(this.settings.language);
    if (typeof matchMedia !== 'undefined') {
      matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => (this.systemDark = e.matches));
    }
  }

  set<K extends keyof Settings>(key: K, value: Settings[K]) {
    this.settings[key] = value;
    api.setSetting(key, value).catch(() => {});
    if (key === 'theme') localStorage.setItem('envelope.theme', String(value));
    if (key === 'accentHue') localStorage.setItem('envelope.accent', String(value));
    if (key === 'nativeTitlebar') api.setDecorations(Boolean(value));
    if (key === 'language') setLanguage(value as LanguageSetting);
  }

  /** A setting changed in another window: this one follows. */
  changedElsewhere(key: string, value: unknown) {
    if (!(key in defaultSettings) || JSON.stringify(this.settings[key as keyof Settings]) === JSON.stringify(value)) return;
    (this.settings as unknown as Record<string, unknown>)[key] = value;
    if (key === 'language') setLanguage(value as LanguageSetting);
    for (const listener of this.listeners) listener(key as keyof Settings);
  }

  /** Hears of settings changed in another window; returns how to stop. */
  onChangedElsewhere(listener: (key: keyof Settings) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  open(overlay: Overlay, tab?: string) {
    if (tab) this.settingsTab = tab;
    this.overlay = overlay;
  }

  close() {
    this.overlay = null;
  }
}

export const ui = new Ui();

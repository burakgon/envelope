import { api } from '../api';

export type Theme = 'system' | 'light' | 'dark';
export type Density = 'comfortable' | 'compact';

/** User preferences persisted through the backend settings store. */
export interface Settings {
  theme: Theme;
  accentHue: number;
  density: Density;
  splitInbox: boolean;
  conversationView: boolean;
  markReadDelayMs: number; // -1 = never automatically
  undoSendSeconds: number;
  loadRemoteImages: 'ask' | 'always' | 'never';
  trustedSenders: string[];
  defaultReply: 'reply' | 'replyAll';
  notifications: boolean;
  calendarNotifications: boolean;
  nativeTitlebar: boolean;
  showAvatars: boolean;
  previewLines: number;
  sendAndArchive: boolean;
  darkEmails: boolean;
  weekStart: number; // 0 = Sunday, 1 = Monday
  sidebarWidth: number;
  listWidth: number;
  aiEnabled: boolean;
}

export const defaultSettings: Settings = {
  theme: 'system',
  accentHue: 280,
  density: 'comfortable',
  splitInbox: true,
  conversationView: true,
  markReadDelayMs: 600,
  undoSendSeconds: 6,
  loadRemoteImages: 'ask',
  trustedSenders: [],
  defaultReply: 'reply',
  notifications: true,
  calendarNotifications: true,
  nativeTitlebar: false,
  showAvatars: true,
  previewLines: 1,
  sendAndArchive: false,
  darkEmails: false,
  weekStart: 1,
  sidebarWidth: 236,
  listWidth: 392,
  aiEnabled: true,
};

export type View = 'mail' | 'calendar';
export type Overlay = null | 'settings' | 'palette' | 'shortcuts' | 'onboarding' | 'addAccount' | 'ask';

class Ui {
  view = $state<View>('mail');
  overlay = $state<Overlay>(null);
  settingsTab = $state('general');
  sidebarCollapsed = $state(false);
  settings = $state<Settings>({ ...defaultSettings });
  systemDark = $state(typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: dark)').matches);
  maximized = $state(false);
  focusSearch = $state(0);
  askAi = $state('');

  isDark = $derived(this.settings.theme === 'dark' || (this.settings.theme === 'system' && this.systemDark));

  async load() {
    try {
      const stored = await api.settings();
      this.settings = { ...defaultSettings, ...(stored as Partial<Settings>) };
    } catch {
      /* first run */
    }
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

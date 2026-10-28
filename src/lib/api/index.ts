import type * as T from './types';

export * from './types';

/** Everything the UI can ask of the backend. */
export interface Api {
  readonly isDemo: boolean;
  ready(): Promise<void>;
  info(): Promise<T.AppInfo>;

  accounts(): Promise<T.Account[]>;
  discover(email: string): Promise<T.Discovery>;
  addPasswordAccount(input: T.NewAccount): Promise<T.Account>;
  /** Adds an on-premises Exchange account (finds the server when no URL is given). */
  addExchangeAccount(input: T.NewExchangeAccount): Promise<T.Account>;
  addOAuthAccount(provider: T.OAuthProvider, loginHint?: string | null): Promise<T.Account>;
  updateAccount(id: number, patch: T.AccountPatch): Promise<T.Account>;
  removeAccount(id: number): Promise<void>;
  reorderAccounts(ids: number[]): Promise<void>;
  oauthClient(provider: T.OAuthProvider): Promise<T.OAuthClient>;
  setOAuthClient(provider: T.OAuthProvider, client: T.OAuthClient): Promise<void>;
  syncNow(accountId?: number | null): Promise<void>;

  folders(): Promise<T.Folder[]>;
  threads(query: T.ThreadQuery): Promise<T.ThreadPage>;
  thread(id: number): Promise<T.ThreadDetail>;
  loadBodies(ids: number[]): Promise<T.MessageView[]>;
  setFlags(threadIds: number[], messageIds: number[], change: T.FlagChange): Promise<void>;
  markFolderRead(folderId: number): Promise<void>;
  moveThreads(threadIds: number[], source: number | null, target: T.MoveTarget): Promise<T.UndoToken>;
  undo(token: T.UndoToken): Promise<void>;
  deleteForever(threadIds: number[]): Promise<void>;
  snooze(threadIds: number[], until: number | null): Promise<void>;
  setLabel(threadIds: number[], folderId: number, add: boolean): Promise<void>;
  messageSource(id: number): Promise<string>;
  unsubscribe(messageId: number): Promise<boolean>;

  drafts(): Promise<T.Draft[]>;
  draft(id: number): Promise<T.Draft>;
  saveDraft(id: number | null, data: T.DraftInput, upload: boolean): Promise<number>;
  deleteDraft(id: number): Promise<void>;
  composeReply(messageId: number, mode: 'reply' | 'replyAll' | 'forward'): Promise<T.DraftInput>;
  send(draftId: number | null, data: T.DraftInput, delayMs: number, sendAt: number | null): Promise<number>;
  cancelSend(outboxId: number): Promise<number | null>;
  retrySend(outboxId: number): Promise<void>;
  outbox(): Promise<T.OutboxItem[]>;
  suggestContacts(query: string): Promise<T.ContactSuggestion[]>;
  identities(): Promise<T.Identity[]>;
  saveIdentity(identity: T.Identity): Promise<number>;
  deleteIdentity(id: number): Promise<void>;

  fileInfo(paths: string[]): Promise<T.FileInfo[]>;
  stashFile(filename: string, data: Uint8Array): Promise<T.FileInfo>;
  pickFiles(): Promise<string[]>;
  pickSavePath(filename: string): Promise<string | null>;
  attachmentOpen(id: number): Promise<void>;
  attachmentSave(id: number, target: string): Promise<void>;
  attachmentReveal(id: number): Promise<void>;
  attachmentUrl(id: number): string;
  inlineUrl(messageId: number, contentId: string): string;

  settings(): Promise<Record<string, unknown>>;
  setSetting(key: string, value: unknown): Promise<void>;
  setDecorations(native: boolean): Promise<void>;
  setBadge(count: number): Promise<void>;
  openUrl(url: string): Promise<void>;

  onEvent(cb: (e: T.EngineEvent) => void): () => void;
  onMailto(cb: (url: string) => void): () => void;
  onOpenThread(cb: (threadId: number | null) => void): () => void;

  calendar: {
    list(): Promise<T.CalendarInfo[]>;
    setVisible(id: number, visible: boolean): Promise<void>;
    setColor(id: number, color: string): Promise<void>;
    occurrences(from: number, to: number): Promise<T.Occurrence[]>;
    detail(eventId: number, recurrenceId: T.EventTime | null): Promise<T.EventDetail>;
    create(input: T.EventInput): Promise<number>;
    update(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, input: T.EventInput): Promise<void>;
    move(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, start: number, end: number): Promise<void>;
    remove(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope): Promise<void>;
    respond(eventId: number, status: T.PartStat, comment?: string | null): Promise<void>;
    invite(messageId: number): Promise<T.InviteInfo>;
    respondInvite(messageId: number, status: T.PartStat, comment?: string | null): Promise<void>;
  };

  contacts: {
    books(): Promise<T.ContactBook[]>;
    /** Contacts matching `query` (name, e-mail, organization, phone), sorted by name. */
    list(query: string, bookId: number | null, offset: number, limit: number): Promise<T.ContactPage>;
    get(id: number): Promise<T.Contact>;
    /** Creates (id null) or updates a contact; returns its id. */
    save(contact: T.ContactInput): Promise<number>;
    remove(id: number): Promise<void>;
  };

  ai: {
    config(): Promise<T.AiConfig>;
    setConfig(settings: T.AiSettings, apiKey?: string | null): Promise<void>;
    presets(): Promise<T.AiPreset[]>;
    models(settings?: T.AiSettings | null, apiKey?: string | null): Promise<T.AiModel[]>;
    test(settings: T.AiSettings, apiKey?: string | null): Promise<string>;
    run(task: T.AiTask, onEvent?: (e: T.AiEvent) => void): T.AiRun;
  };

  window: {
    minimize(): Promise<void>;
    toggleMaximize(): Promise<void>;
    close(): Promise<void>;
    isMaximized(): Promise<boolean>;
    onResized(cb: () => void): () => void;
  };
}

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function load(): Promise<Api> {
  if (inTauri && !__DEMO__) {
    const m = await import('./tauri');
    return m.createTauriApi();
  }
  const m = await import('./mock');
  return m.createMockApi();
}

export const apiPromise: Promise<Api> = load();

/** Resolved backend; set once `apiPromise` settles (before the app mounts). */
export let api: Api;
apiPromise.then((a) => (api = a));

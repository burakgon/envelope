import type * as T from './types';

export * from './types';

/** Everything the UI can ask of the backend. */
export interface Api {
  readonly isDemo: boolean;
  ready(): Promise<void>;
  info(): Promise<T.AppInfo>;
  /** Start Envelope (in the tray) when the user logs in. */
  autostart: {
    get(): Promise<boolean>;
    set(enabled: boolean): Promise<void>;
  };

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
  /** Mutes conversations (archiving them from the inbox) or unmutes them; muting returns the archive's undo tokens. */
  muteThreads(threadIds: number[], muted: boolean): Promise<T.UndoToken[]>;
  setLabel(threadIds: number[], folderId: number, add: boolean): Promise<void>;
  messageSource(id: number): Promise<string>;
  unsubscribe(messageId: number): Promise<boolean>;

  /** Drafts not waiting in the outbox, with their server copies. */
  drafts(): Promise<T.Draft[]>;
  draft(id: number): Promise<T.Draft>;
  /** The draft to edit for a draft message: its local draft, or one made from it (written elsewhere). */
  draftForMessage(messageId: number): Promise<T.Draft>;
  /** Saves a draft (a new one when `id` is gone); `upload` also stores it on the server. */
  saveDraft(id: number | null, data: T.DraftInput, upload: boolean): Promise<number>;
  /** Discards a draft and its server copy. */
  deleteDraft(id: number): Promise<void>;
  /** `quote`: the header above the quoted original, in the interface language. */
  composeReply(messageId: number, mode: 'reply' | 'replyAll' | 'forward', quote?: T.QuoteText): Promise<T.DraftInput>;
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
  /** Translations for texts shown outside the window (notifications, tray menu), keyed `native.*`. */
  setNativeStrings(strings: Record<string, string>): Promise<void>;
  openUrl(url: string): Promise<void>;

  onEvent(cb: (e: T.EngineEvent) => void): () => void;
  onMailto(cb: (url: string) => void): () => void;
  /** A `webcal:` link was opened with Envelope. */
  onWebcal(cb: (url: string) => void): () => void;
  /** A message or calendar file was opened with Envelope. */
  onOpenFile(cb: (path: string) => void): () => void;
  onOpenThread(cb: (threadId: number | null) => void): () => void;
  /** A task's notification was clicked. */
  onOpenTask(cb: (taskId: number) => void): () => void;

  calendar: {
    list(): Promise<T.CalendarInfo[]>;
    setVisible(id: number, visible: boolean): Promise<void>;
    setColor(id: number, color: string): Promise<void>;
    /** Subscribes to an iCalendar feed (`webcal:` or `https:`); read-only, refreshed every few hours. */
    subscribe(url: string): Promise<T.CalendarInfo>;
    /** Fetches a subscribed calendar now. */
    refresh(id: number): Promise<void>;
    unsubscribe(id: number): Promise<void>;
    occurrences(from: number, to: number): Promise<T.Occurrence[]>;
    detail(eventId: number, recurrenceId: T.EventTime | null): Promise<T.EventDetail>;
    create(input: T.EventInput): Promise<number>;
    update(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, input: T.EventInput): Promise<void>;
    move(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope, start: number, end: number): Promise<void>;
    remove(eventId: number, recurrenceId: T.EventTime | null, scope: T.EditScope): Promise<void>;
    respond(eventId: number, status: T.PartStat, comment?: string | null): Promise<void>;
    invite(messageId: number): Promise<T.InviteInfo>;
    respondInvite(messageId: number, status: T.PartStat, comment?: string | null): Promise<void>;
    /** When people are busy, asked through the service of the calendar the meeting goes on. */
    freeBusy(calendarId: number, emails: string[], from: number, to: number): Promise<T.Availability[]>;
  };

  /** Automatic replies ("out of office"). Fails for accounts whose server has none. */
  autoReply: {
    get(accountId: number): Promise<T.AutoReplyInfo>;
    set(accountId: number, reply: T.AutoReply): Promise<void>;
  };

  /** Saved messages (.eml) and calendar files (.ics) opened from outside. */
  files: {
    open(path: string): Promise<T.OpenedFile>;
    /** Adds the file's events (or only `uids`) to a calendar, skipping ones already there. */
    importEvents(path: string, calendarId: number, uids?: string[] | null): Promise<T.ImportResult>;
    /** Adds the file's contacts (or only those at `indices`) to an address book, skipping people already there. */
    importContacts(path: string, bookId: number, indices?: number[] | null): Promise<T.ImportResult>;
    openPart(path: string, partId: string): Promise<void>;
    savePart(path: string, partId: string, target: string): Promise<void>;
  };

  /** OpenPGP end-to-end encryption. */
  crypto: {
    settings(): Promise<T.CryptoSettings>;
    setSettings(settings: T.CryptoSettings): Promise<void>;
    /** Every sending address and its key. */
    ownKeys(): Promise<T.OwnKey[]>;
    generateKey(email: string, name: string, passphrase?: string | null): Promise<T.KeyInfo>;
    /** Imports armored keys (public or secret); `ownEmail` makes a secret key that address's key. */
    importText(text: string, passphrase?: string | null, ownEmail?: string | null): Promise<T.KeyInfo[]>;
    importFile(path: string, passphrase?: string | null, ownEmail?: string | null): Promise<T.KeyInfo[]>;
    exportPublic(fingerprint: string): Promise<string>;
    /** A passphrase protects the exported copy of a key that has none. */
    exportSecret(fingerprint: string, passphrase?: string | null): Promise<string>;
    exportFile(fingerprint: string, secret: boolean, passphrase: string | null, path: string): Promise<void>;
    deleteKey(fingerprint: string): Promise<void>;
    /** Keys matching an address, a name or a fingerprint. */
    keys(query: string): Promise<T.KeyInfo[]>;
    setVerified(fingerprint: string, verified: boolean): Promise<void>;
    setOwnKey(email: string, fingerprint: string): Promise<void>;
    /** Checks a passphrase and remembers it until Envelope quits. */
    unlock(fingerprint: string, passphrase: string): Promise<void>;
    /** Forgets passphrases and decrypted messages. */
    lock(): Promise<void>;
    composeStatus(accountId: number, identityId: number | null, from: T.Address | null, recipients: string[]): Promise<T.ComposeCrypto>;
    /** Looks keys up in the recipients' Web Key Directories (and on keys.openpgp.org with `keyserver`). */
    lookup(emails: string[], keyserver: boolean, explicit: boolean): Promise<T.KeyInfo[]>;
    /** Decrypts and/or verifies a message. */
    open(messageId: number): Promise<T.OpenedMessage>;
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

  tasks: {
    /** The local list first, then each account's lists in their order. */
    lists(): Promise<T.TaskList[]>;
    /** Tasks in display order; `includeCompleted` only applies to a list. */
    query(view: T.TaskView, includeCompleted: boolean): Promise<T.Task[]>;
    /** Open tasks due in [from, to) (ms), for the calendar. */
    due(from: number, to: number): Promise<T.Task[]>;
    get(id: number): Promise<T.Task>;
    /** A task's subtasks by position, completed ones included. */
    subtasks(id: number): Promise<T.Task[]>;
    create(input: T.TaskInput): Promise<T.Task>;
    /**
     * Saves a task (fields left out keep their value). A different `listId` moves it, and
     * unless both lists are local the moved task has a new id: go on with the one returned.
     */
    update(id: number, input: T.TaskInput): Promise<T.Task>;
    /** A repeating task with a due date on a CalDAV list comes back open with its next one. */
    complete(id: number, completed: boolean): Promise<T.Task>;
    /** Moves a task before another in its list (null: to the end). New tasks go to the end. */
    reorder(id: number, beforeId: number | null): Promise<void>;
    /** Deletes a task and its subtasks; for a few seconds `restore` can bring them back. */
    remove(id: number): Promise<void>;
    /** Brings back a task deleted with `remove`, with the subtasks deleted with it (fails once it is too late). */
    restore(id: number): Promise<T.Task>;
    createList(accountId: number | null, name: string): Promise<T.TaskList>;
    renameList(id: number, name: string): Promise<void>;
    removeList(id: number): Promise<void>;
    clearCompleted(listId: number): Promise<void>;
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

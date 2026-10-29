// Types shared with the Rust engine (see crates/envelope-core/src/model.rs).

export type AccountKind = 'imap' | 'gmail' | 'microsoft' | 'exchange';
export type Security = 'tls' | 'startTls' | 'none';
export type AuthMethod = 'password' | 'oAuthGoogle' | 'oAuthMicrosoft';

export interface ServerConfig {
  host: string;
  port: number;
  security: Security;
  username: string;
}

export interface AccountConfig {
  auth: AuthMethod;
  imap: ServerConfig | null;
  smtp: ServerConfig | null;
  caldavUrl: string | null;
  carddavUrl: string | null;
  acceptInvalidCerts: boolean;
  saveSentCopy: boolean | null;
  bodySyncDays: number | null;
  /** Exchange Web Services settings (Exchange accounts only). */
  exchange?: ExchangeConfig | null;
}

/** How an Exchange account signs in: NTLM with a Basic fallback, or one of them. */
export type ExchangeAuth = 'auto' | 'ntlm' | 'basic';

export interface ExchangeConfig {
  ewsUrl: string;
  /** Login name (`DOMAIN\user` or a UPN); empty means the e-mail address. */
  username: string;
  auth: ExchangeAuth;
  version: string | null;
}

/** Input for adding an on-premises Exchange account. */
export interface NewExchangeAccount {
  email: string;
  name: string;
  password: string;
  /** EWS URL or server name; found automatically when empty. */
  ewsUrl?: string | null;
  /** `DOMAIN\user` or a UPN, when it is not the e-mail address. */
  username?: string | null;
  acceptInvalidCerts?: boolean;
  auth?: ExchangeAuth;
}

export type SyncState =
  | { state: 'idle' }
  | { state: 'connecting' }
  | { state: 'syncing'; folder: string | null; progress: number | null }
  | { state: 'offline' }
  | { state: 'authError'; message: string }
  | { state: 'error'; message: string };

export interface Account {
  id: number;
  kind: AccountKind;
  provider: string;
  email: string;
  name: string;
  label: string | null;
  color: string;
  sortOrder: number;
  enabled: boolean;
  config: AccountConfig;
  sync: SyncState;
  hasCalendar: boolean;
  hasContacts: boolean;
}

export interface Identity {
  id: number;
  accountId: number;
  name: string;
  email: string;
  replyTo: string | null;
  signature: string | null;
  isDefault: boolean;
}

export type FolderRole = 'inbox' | 'sent' | 'drafts' | 'trash' | 'junk' | 'archive' | 'all' | 'starred' | 'important';

export interface Folder {
  id: number;
  accountId: number;
  name: string;
  path: string;
  parentId: number | null;
  role: FolderRole | null;
  kind: 'folder' | 'label';
  unreadCount: number;
  totalCount: number;
  color: string | null;
  hidden: boolean;
  selectable: boolean;
  sortOrder: number;
}

export interface Address {
  name?: string | null;
  email: string;
}

export interface Flags {
  seen: boolean;
  flagged: boolean;
  answered: boolean;
  draft: boolean;
  forwarded: boolean;
}

export interface ThreadSummary {
  /** Negative for a draft that is only on this computer (see `localDraftId`). */
  id: number;
  accountId: number;
  subject: string;
  snippet: string;
  lastDate: number;
  messageCount: number;
  unreadCount: number;
  flagged: boolean;
  hasAttachments: boolean;
  draftCount: number;
  participants: Address[];
  lastSender: Address | null;
  folderIds: number[];
  category: string | null;
  isInvite: boolean;
  /** Later replies skip the inbox and do not notify. */
  muted: boolean;
  snoozedUntil: number | null;
  /** Interface only: the row is a draft that is only on this computer. */
  localDraftId?: number;
}

export interface ThreadPage {
  total: number;
  offset: number;
  items: ThreadSummary[];
}

export type Mailbox =
  | { kind: 'unified'; role: FolderRole }
  | { kind: 'folder'; id: number }
  | { kind: 'starred' }
  | { kind: 'snoozed' }
  /** Sent conversations with a follow-up reminder that came due without a reply. */
  | { kind: 'followUp' }
  | { kind: 'everything' };

export interface ListFilter {
  unread?: boolean;
  starred?: boolean;
  attachments?: boolean;
  category?: string | null;
}

export interface ThreadQuery {
  mailbox: Mailbox;
  search?: string | null;
  filter?: ListFilter;
  offset: number;
  limit: number;
}

export interface AttachmentInfo {
  id: number;
  filename: string;
  mimeType: string;
  size: number;
  contentId: string | null;
  inline: boolean;
}

export interface UnsubscribeInfo {
  url: string | null;
  mailto: string | null;
  oneClick: boolean;
}

export interface MessageBody {
  html: string | null;
  text: string | null;
}

export interface MessageView {
  id: number;
  threadId: number;
  accountId: number;
  folderIds: number[];
  messageId: string | null;
  inReplyTo: string | null;
  references: string[];
  subject: string;
  from: Address | null;
  to: Address[];
  cc: Address[];
  bcc: Address[];
  replyTo: Address[];
  date: number;
  flags: Flags;
  snippet: string;
  size: number;
  hasAttachments: boolean;
  attachments: AttachmentInfo[];
  body: MessageBody | null;
  unsubscribe: UnsubscribeInfo | null;
  calendarMethod: string | null;
  isAuto: boolean;
  /** OpenPGP content, opened with `api.crypto.open`. */
  crypto?: CryptoKind | null;
}

export interface ThreadDetail {
  thread: ThreadSummary;
  messages: MessageView[];
}

export type MoveTarget =
  | { kind: 'inbox' }
  | { kind: 'archive' }
  | { kind: 'trash' }
  | { kind: 'junk' }
  | { kind: 'folder'; id: number };

export interface FlagChange {
  seen?: boolean | null;
  flagged?: boolean | null;
}

export interface UndoToken {
  accountOps: [number, number][];
  locations: [number, [number, number | null][]][];
  flags: [number, number][];
  snoozed: [number, number | null][];
}

export interface DraftAttachment {
  filename: string;
  mimeType: string;
  size: number;
  path?: string | null;
  attachmentId?: number | null;
  contentId?: string | null;
  inline?: boolean;
}

export interface DraftInput {
  accountId: number;
  identityId?: number | null;
  from?: Address | null;
  to: Address[];
  cc: Address[];
  bcc: Address[];
  subject: string;
  html: string;
  text?: string | null;
  attachments: DraftAttachment[];
  replyToMessageId?: number | null;
  mode?: 'new' | 'reply' | 'replyAll' | 'forward' | null;
  requestReadReceipt?: boolean;
  /** Remind me if nobody replies within this many days. */
  followUpDays?: number | null;
  /** Encrypt with OpenPGP (to every recipient and the sender). */
  encrypt?: boolean | null;
  /** Sign with the sender's OpenPGP key. */
  sign?: boolean | null;
}

export interface Draft {
  id: number;
  data: DraftInput;
  updatedAt: number;
  /** The synced message that is this draft's copy on the server. */
  serverMessageId: number | null;
  /** Only on this computer: never stored on the server, or changed here since and its server copy is gone. */
  localOnly: boolean;
}

export interface OutboxItem {
  id: number;
  accountId: number;
  subject: string;
  to: Address[];
  sendAt: number;
  state: 'queued' | 'sending' | 'failed' | 'sent';
  error: string | null;
}

// ----- Contacts ----------------------------------------------------------------

export interface ContactBook {
  id: number;
  accountId: number | null;
  /** `carddav`, `google`, `graph`, `ews` or `local`. */
  kind: string;
  name: string;
  readOnly: boolean;
}

export interface LabeledValue {
  value: string;
  /** `home`, `work`, `mobile`, `other`, or free text. */
  label: string | null;
}

export interface Contact {
  id: number;
  bookId: number;
  name: string;
  givenName: string | null;
  familyName: string | null;
  emails: LabeledValue[];
  phones: LabeledValue[];
  addresses: LabeledValue[];
  urls: string[];
  organization: string | null;
  title: string | null;
  /** `YYYY-MM-DD`, or `--MM-DD` when the year is unknown. */
  birthday: string | null;
  notes: string | null;
  /** A `data:` URL. */
  photo: string | null;
}

export type ContactInput = Omit<Contact, 'id'> & { id: number | null };

export interface ContactSummary {
  id: number;
  bookId: number;
  name: string;
  email: string | null;
  organization: string | null;
  photo: string | null;
}

export interface ContactPage {
  total: number;
  items: ContactSummary[];
}

export interface ContactSuggestion {
  name: string | null;
  email: string;
  score: number;
}

// ----- Tasks -------------------------------------------------------------------

export type TaskListKind = 'local' | 'caldav' | 'google' | 'graph' | 'ews';

export interface TaskList {
  id: number;
  accountId: number | null;
  kind: TaskListKind;
  name: string;
  /** `#rrggbb`. */
  color: string;
  readOnly: boolean;
  /** Where new tasks for this account go. */
  isDefault: boolean;
  /** Tasks not completed yet (subtasks included). */
  openCount: number;
  /** Completed top-level tasks: what `clearCompleted` deletes (with their subtasks). */
  completedCount: number;
  /** What the service stores: a time of day on due dates, importance, subtasks. */
  dueTimes: boolean;
  importance: boolean;
  subtasks: boolean;
  /** Tasks can be reordered (Microsoft To Do and Exchange keep their own order). */
  reorder: boolean;
}

/** The message a task was made from. */
export interface TaskSource {
  /** `Message-ID` header, without angle brackets. */
  messageId: string;
  /** The conversation, when the message is still in the database. */
  threadId: number | null;
  accountId: number | null;
  subject: string;
  /** Sender's name or address. */
  from: string | null;
}

export interface Task {
  id: number;
  listId: number;
  parentId: number | null;
  title: string;
  notes: string;
  /** The day the task is due (`YYYY-MM-DD`, local). */
  dueDate: string | null;
  /** When it is due, if at a time of day (ms since epoch); `dueDate` is then that local day. */
  dueAt: number | null;
  /** When it was completed (ms); null while open. */
  completedAt: number | null;
  important: boolean;
  /** Repeats (kept from the server); completing it moves it to its next due date. */
  repeats: boolean;
  source: TaskSource | null;
  /** Subtasks: how many, how many done (0 for subtasks themselves). */
  subtaskCount: number;
  subtasksDone: number;
  createdAt: number;
  updatedAt: number;
  /** Changed here and not on the server yet. */
  pending: boolean;
  /** Why saving it on the server failed last time. */
  error: string | null;
}

export type TaskView = { list: number } | 'today' | 'upcoming' | 'important' | 'all';

/** For `task_update` a field left out keeps its value and `null` clears it; `title` and `listId` are always needed. */
export interface TaskInput {
  listId: number;
  parentId?: number | null;
  title: string;
  notes?: string;
  dueDate?: string | null;
  dueAt?: number | null;
  important?: boolean;
  /** Database id of a message to link the task to (only when creating). */
  fromMessage?: number | null;
}

export interface Discovery {
  email: string;
  provider: string;
  providerName: string;
  kind: AccountKind;
  auth: AuthMethod;
  imap: ServerConfig | null;
  smtp: ServerConfig | null;
  caldavUrl: string | null;
  carddavUrl: string | null;
  passwordHint: string | null;
  passwordHelpUrl: string | null;
}

export interface NewAccount {
  email: string;
  name: string;
  provider: string;
  password: string;
  config: AccountConfig;
}

export interface AccountPatch {
  name?: string;
  label?: string;
  color?: string;
  enabled?: boolean;
  config?: AccountConfig;
  password?: string;
}

export interface FileInfo {
  path: string;
  filename: string;
  mimeType: string;
  size: number;
}

export type OAuthProvider = 'google' | 'microsoft';

export interface OAuthClient {
  clientId: string;
  clientSecret?: string | null;
  tenant?: string | null;
}

export interface AppInfo {
  version: string;
  dataDir: string;
  googleConfigured: boolean;
  microsoftConfigured: boolean;
  /** A tray icon can be shown (Linux needs an AppIndicator library). */
  trayAvailable?: boolean;
}

export type EngineEvent =
  | { type: 'accountsChanged' }
  | { type: 'foldersChanged'; accountId: number }
  | { type: 'mailChanged'; accountId: number; folderIds: number[]; threadIds: number[] }
  | { type: 'syncStatus'; accountId: number; sync: SyncState }
  | { type: 'newMail'; accountId: number; threads: ThreadSummary[] }
  | { type: 'calendarChanged'; calendarIds: number[] }
  | { type: 'contactsChanged'; bookIds: number[] }
  /** Tasks or lists changed; an empty `listIds` means the lists themselves changed. */
  | { type: 'tasksChanged'; listIds: number[] }
  /** A task due at a time of day came due (the desktop shell shows the notification). */
  | { type: 'taskDue'; taskId: number; title: string; dueAt: number }
  | { type: 'followUpDue'; threadId: number; subject: string }
  | { type: 'outboxChanged' }
  | { type: 'sendFailed'; outboxId: number; error: string }
  | { type: 'reminder'; eventId: number; title: string; start: number; location: string | null };

export interface ApiError {
  message: string;
  kind: 'auth' | 'notFound' | 'invalid' | 'network' | 'error';
}

// ----- AI --------------------------------------------------------------------

export interface AiSettings {
  enabled: boolean;
  provider: string;
  baseUrl: string;
  model: string;
  customInstructions: string;
  language: string | null;
}

export interface AiConfig extends AiSettings {
  hasKey: boolean;
  local: boolean;
}

export interface AiPreset {
  id: string;
  name: string;
  kind: string;
  baseUrl: string;
  requiresKey: boolean;
  keyUrl: string | null;
  defaultModel: string | null;
  local: boolean;
}

export interface AiModel {
  id: string;
  name: string;
  contextLength: number | null;
  inputPrice: number | null;
  outputPrice: number | null;
}

export type AiTask =
  | { kind: 'summarize'; threadId: number }
  | { kind: 'quickReplies'; threadId: number }
  | { kind: 'draftReply'; threadId: number; instruction: string }
  | { kind: 'compose'; instruction: string; recipients: string[] }
  | { kind: 'rewrite'; text: string; style: string }
  | { kind: 'translate'; text: string; language: string }
  | { kind: 'ask'; question: string }
  | { kind: 'searchQuery'; question: string }
  | { kind: 'extractEvents'; messageId: number };

export interface AiSource {
  index: number;
  messageId: number;
  threadId: number;
  subject: string;
  from: string;
  date: number;
}

export type AiEvent =
  | { type: 'delta'; text: string }
  | { type: 'sources'; sources: AiSource[] }
  | { type: 'done'; text: string };

export interface AiRun {
  promise: Promise<string>;
  cancel(): void;
}

// ----- Calendar ----------------------------------------------------------------

export type EventTime =
  | { kind: 'date'; date: string }
  | { kind: 'zoned'; local: string; tzid: string }
  | { kind: 'utc'; at: string }
  | { kind: 'floating'; local: string };

export type PartStat = 'needsAction' | 'accepted' | 'declined' | 'tentative' | 'delegated';
export type EventStatus = 'confirmed' | 'tentative' | 'cancelled';
export type EditScope = 'this' | 'following' | 'all';

export interface CalendarInfo {
  id: number;
  accountId: number | null;
  kind: string;
  name: string;
  color: string;
  readOnly: boolean;
  isDefault: boolean;
  visible: boolean;
}

export interface Occurrence {
  key: string;
  eventId: number;
  calendarId: number;
  uid: string;
  title: string;
  start: number;
  end: number;
  allDay: boolean;
  startDate: string | null;
  endDate: string | null;
  location: string | null;
  color: string;
  status: EventStatus;
  transparent: boolean;
  isRecurring: boolean;
  recurrenceId: EventTime | null;
  myStatus: PartStat | null;
  hasAttendees: boolean;
  conferenceUrl: string | null;
  readOnly: boolean;
}

export interface CalAttendee {
  email: string;
  name: string | null;
  partstat: PartStat;
  role: 'chair' | 'required' | 'optional' | 'nonParticipant';
  rsvp: boolean;
}

export interface CalEvent {
  uid: string;
  recurrenceId: EventTime | null;
  summary: string;
  description: string | null;
  descriptionHtml: string | null;
  location: string | null;
  url: string | null;
  start: EventTime;
  end: EventTime;
  rrule: string | null;
  status: EventStatus;
  transparency: 'opaque' | 'transparent';
  organizer: { email: string; name: string | null } | null;
  attendees: CalAttendee[];
  alarms: { action: string; minutesBefore: number; description: string | null }[];
  color: string | null;
  conferenceUrl: string | null;
}

export interface EventDetail {
  occurrence: Occurrence;
  event: CalEvent;
  calendar: CalendarInfo;
  organizerIsMe: boolean;
  myEmail: string | null;
}

export interface EventInput {
  calendarId: number;
  summary: string;
  description?: string | null;
  location?: string | null;
  allDay: boolean;
  start: number;
  end: number;
  startDate?: string | null;
  endDate?: string | null;
  timezone?: string | null;
  rrule?: string | null;
  attendees: Address[];
  alarms: number[];
  color?: string | null;
  transparent?: boolean;
  addConference?: boolean;
}

/** An event found in a message by the AI (`extractEvents`). */
export interface ExtractedEvent {
  title: string;
  /** `YYYY-MM-DD` for all-day events, otherwise an ISO date-time (with offset, or local). */
  start: string;
  /** For all-day events, the last day (inclusive). */
  end: string | null;
  all_day: boolean;
  location: string | null;
  description: string | null;
  confidence: number;
}

/** A busy period (unix ms). */
export interface Busy {
  start: number;
  end: number;
  /** Not yet answered or tentatively accepted. */
  tentative: boolean;
}

/** One person's availability; `busy` is null when their calendar cannot be seen. */
export interface Availability {
  email: string;
  busy: Busy[] | null;
}

/** Who gets an automatic reply. */
export type AutoReplyAudience = 'everyone' | 'contacts' | 'organization';

/** An account's automatic reply ("out of office"). */
export interface AutoReply {
  enabled: boolean;
  /** First and last day (inclusive), `YYYY-MM-DD`; both or neither. */
  start: string | null;
  end: string | null;
  subject: string;
  /** Plain text. */
  message: string;
  audience: AutoReplyAudience;
}

export interface AutoReplyInfo {
  reply: AutoReply;
  /** The audiences the account offers. */
  audiences: AutoReplyAudience[];
  /** The reply can have its own subject. */
  subject: boolean;
}

/** A saved message (.eml) opened from a file. Inline pictures are `data:` URLs. */
export interface FileMessage {
  path: string;
  subject: string;
  from: Address | null;
  to: Address[];
  cc: Address[];
  replyTo: Address[];
  date: number;
  html: string | null;
  text: string | null;
  attachments: { partId: string; filename: string; mimeType: string; size: number }[];
  /** The message was encrypted and is shown decrypted. */
  encrypted: boolean;
}

export interface FileEvent {
  uid: string;
  summary: string;
  start: number;
  end: number;
  allDay: boolean;
  location: string | null;
  recurring: boolean;
  /** Already on one of the user's calendars. */
  exists: boolean;
}

export interface FileContact {
  /** Position in the file, for choosing which to add. */
  index: number;
  name: string;
  email: string | null;
  organization: string | null;
  /** Someone with this address is already in an address book. */
  exists: boolean;
}

/** What a file opened with Envelope contains. */
export type OpenedFile =
  | ({ kind: 'message' } & FileMessage)
  | { kind: 'calendar'; path: string; name: string | null; events: FileEvent[] }
  | { kind: 'contacts'; path: string; contacts: FileContact[] };

export interface ImportResult {
  added: number;
  /** Events that were already on a calendar. */
  skipped: number;
}

export interface InviteInfo {
  method: string;
  uid: string;
  summary: string;
  start: number;
  end: number;
  allDay: boolean;
  location: string | null;
  description: string | null;
  organizer: Address | null;
  attendees: Address[];
  myStatus: PartStat | null;
  eventId: number | null;
  conferenceUrl: string | null;
  recurring: boolean;
}

// ----- OpenPGP -----------------------------------------------------------------

export type CryptoKind = 'encrypted' | 'signed' | 'inline-encrypted' | 'inline-signed';

/** Where a key came from. */
export type KeySource = 'own' | 'autocrypt' | 'gossip' | 'wkd' | 'keyserver' | 'manual';

export interface CryptoSettings {
  /** Encrypt automatically when every recipient has a key. */
  encryptWhenPossible: boolean;
  signByDefault: boolean;
  /** Send the public key with every message (Autocrypt). */
  sendAutocrypt: boolean;
  /** Replace the subject of encrypted messages with "...". */
  protectSubject: boolean;
  /** Keep decrypted copies on this device (search, previews). */
  keepDecrypted: boolean;
  /** Look up missing keys in the recipient's domain while composing. */
  wkdLookup: boolean;
}

export interface KeyInfo {
  /** Upper-case hex. */
  fingerprint: string;
  userIds: string[];
  emails: string[];
  algorithm: string;
  createdAt: number;
  expiresAt: number | null;
  revoked: boolean;
  expired: boolean;
  canEncrypt: boolean;
  canSign: boolean;
  /** One of the user's own keys (the secret key is in the keyring). */
  secret: boolean;
  /** The secret key is protected by a passphrase. */
  protected: boolean;
  source: KeySource;
  /** The user compared the fingerprint with its owner. */
  verified: boolean;
  addedAt: number;
  updatedAt: number;
}

/** An address the user sends from, and its key. */
export interface OwnKey {
  email: string;
  name: string;
  identityIds: number[];
  key: KeyInfo | null;
  /** No passphrase needed right now. */
  unlocked: boolean;
}

export interface RecipientKey {
  email: string;
  key: KeyInfo | null;
  /** The recipient asked for encrypted mail (Autocrypt). */
  preferEncrypt: boolean;
}

/** What the composer can do for a message. */
export interface ComposeCrypto {
  from: string;
  ownKey: KeyInfo | null;
  ownLocked: boolean;
  recipients: RecipientKey[];
  missing: string[];
  canEncrypt: boolean;
  canSign: boolean;
  /** Every recipient asked for encryption. */
  mutual: boolean;
  settings: CryptoSettings;
}

export interface SignatureView {
  status: 'good' | 'bad' | 'unknownKey';
  signer: KeyInfo | null;
  /** Fingerprint or key id named in the signature. */
  issuer: string | null;
  created: number | null;
  /** The signing key carries the sender's address. */
  senderMatches: boolean;
}

/** An encrypted or signed message, opened. */
export interface OpenedMessage {
  messageId: number;
  kind: CryptoKind;
  encrypted: boolean;
  status: 'ok' | 'locked' | 'noKey' | 'error';
  error: string | null;
  /** The key whose passphrase is needed. */
  lockedKey: KeyInfo | null;
  /** Decrypted content (null: the stored body is the content). */
  body: MessageBody | null;
  attachments: AttachmentInfo[];
  /** The real subject (protected headers). */
  subject: string | null;
  signatures: SignatureView[];
  /** The signature covers only part of the message. */
  partiallySigned: boolean;
  /** The message has unencrypted parts besides the encrypted one. */
  partiallyEncrypted: boolean;
}

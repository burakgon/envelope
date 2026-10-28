// Types shared with the Rust engine (see crates/envelope-core/src/model.rs).

export type AccountKind = 'imap' | 'gmail' | 'microsoft';
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
  snoozedUntil: number | null;
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
}

export interface Draft {
  id: number;
  data: DraftInput;
  updatedAt: number;
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

export interface ContactSuggestion {
  name: string | null;
  email: string;
  score: number;
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
}

export type EngineEvent =
  | { type: 'accountsChanged' }
  | { type: 'foldersChanged'; accountId: number }
  | { type: 'mailChanged'; accountId: number; folderIds: number[]; threadIds: number[] }
  | { type: 'syncStatus'; accountId: number; sync: SyncState }
  | { type: 'newMail'; accountId: number; threads: ThreadSummary[] }
  | { type: 'calendarChanged'; calendarIds: number[] }
  | { type: 'outboxChanged' }
  | { type: 'sendFailed'; outboxId: number; error: string }
  | { type: 'reminder'; eventId: number; title: string; start: number; location: string | null };

export interface ApiError {
  message: string;
  kind: 'auth' | 'notFound' | 'invalid' | 'network' | 'error';
}

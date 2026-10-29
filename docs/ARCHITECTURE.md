# How Envelope works

A tour of the moving parts, for anyone who wants to change them. The [README](../README.md#how-it-works) lists the crates; this document follows a message from the server to the screen and back.

## One process, two halves

Envelope is a single [Tauri 2](https://tauri.app) process. The Rust half owns everything that talks to servers or touches the disk; the Svelte half, rendered by WebKitGTK, owns the screen.

```
  Svelte 5 interface ──── commands (invoke) ────▶ src-tauri/src/commands.rs ──▶ envelope-core::Engine
         ▲                                                                              │
         └──────────────── "engine" events (EngineEvent) ◀──────────────────────────────┘
```

- **Commands** are thin: they unpack arguments, call the engine and turn its errors into messages people can read (`CmdError`).
- **Events** flow the other way. When sync stores mail, the engine emits `MailChanged`, `NewMail`, `CalendarChanged`, `ContactsChanged` and friends; the interface reloads only what changed.
- `src/lib/api` is the typed boundary. `tauri.ts` talks to the backend; `mock.ts` is a complete stand-in with demo data, which is how [the live demo](https://burakgon.github.io/envelope/) and `npm run demo` run without a backend.

## Storage

Everything the interface shows comes from one SQLite database (`envelope.db` in the data directory), opened in WAL mode with one writer and a small pool of readers, so reads never wait for sync.

- **Mail:** `accounts`, `folders`, `messages`, `message_folders` (a message can sit in several folders, as with Gmail labels), `threads` and `thread_folders`. Thread rows keep precomputed aggregates (unread count, participants, last date, category), so a message list is one indexed query, even with hundreds of thousands of messages.
- **Bodies** are compressed with zstd and indexed for full-text search with FTS5.
- **Calendars, contacts and tasks** have their own tables (`calendars`, `events`, `address_books`, `contacts`, `task_lists`, `tasks`); OpenPGP keys and per-message encryption state live in `crypto.sql`.
- **Schema changes** go in `db/mod.rs`: a list of migrations by version, and a list of columns added to existing tables (checked before they are added).

Attachments and inline pictures are fetched on demand, cached on disk, and served to the interface through the `att://` protocol (`src-tauri/src/protocol.rs`).

## Sync

Each account runs its own worker (`sync::run_account`), chosen by kind:

| Kind | Mail | Notes |
| --- | --- | --- |
| IMAP (including Gmail) | `sync/imap.rs` over `envelope-imap` | IDLE for push, CONDSTORE/QRESYNC for cheap resyncs, Gmail labels and thread ids when available |
| Microsoft 365 / Outlook.com | `sync/graph.rs` over `envelope-graph` | Delta queries per folder |
| Exchange (on-premises) | `sync/ews.rs` over `envelope-ews` | `SyncFolderItems`, with streaming notifications for push |

Calendars (`sync/calendar.rs`: CalDAV, Google, Graph, EWS), contacts (`sync/contacts.rs`: CardDAV, Google People, Graph, EWS), tasks (`sync/tasks/`: one `TaskService` per kind of server — CalDAV, Google Tasks, Microsoft To Do, EWS) and subscribed calendars (`sync/subscriptions.rs`) sync alongside. The outbox (`sync/outbox.rs`) sends queued mail when its undo delay or scheduled time has passed. A housekeeping loop brings snoozed conversations back and raises follow-up reminders.

New mail runs through the user's rules (`rules.rs`) before anything is notified; replies in muted conversations skip the inbox there too.

## Local first

Every action — archive, move, flag, delete, snooze — changes the database at once and records a **pending operation** for the account's worker to carry out on the server. Contacts and tasks work the same way with a per-row `local_state` (created, changed, deleted), keeping the server's copy so that fields Envelope does not edit survive the round trip. The interface never waits for the network, and works offline.

Actions return an **undo token** with the previous state. Undoing within a few seconds simply cancels the operation before it reaches the server; later, it is reverted with a new operation.

## Showing mail safely

Mail is hostile input. The interface sanitizes message HTML with DOMPurify (`src/lib/utils/html.ts`), blocks remote content until the user allows it, removes tracking pixels, and renders the result in a sandboxed iframe whose Content Security Policy is `default-src 'none'` — no script in a message can run, and nothing loads that was not allowed. Links open in the browser; `mailto:` and `webcal:` links stay in Envelope.

## Secrets

Passwords, OAuth tokens, AI API keys and OpenPGP secret keys go to the system keyring through [oo7](https://github.com/bilelmoussaoui/oo7): the Secret Service on the desktop, the Secret portal inside Flatpak. Only when no keyring is available do they fall back to a file readable by the user alone. Decrypted OpenPGP messages stay in memory unless the user asks for them to be kept for search (see [ENCRYPTION.md](ENCRYPTION.md)).

## The interface

Svelte 5 with runes. State lives in stores under `src/lib/stores` (`mail`, `ui`, `calendar`, `contacts`, `compose`, …), components under `src/lib/components`. Long lists are virtualized. Keyboard shortcuts are handled in `App.svelte`; the command palette (`CommandPalette.svelte`) reaches every action.

## Testing

- `cargo test --workspace` runs unit tests and integration tests against mock servers (wiremock) and temporary databases.
- `scripts/devserver/run.sh` starts a local Dovecot (IMAP, ManageSieve), an SMTP sink and Radicale (CalDAV/CardDAV) with seeded data; the `live_*` tests in `crates/envelope-core/tests` run against it with `ENVELOPE_TEST_IMAP=1`.
- `npm run check` type-checks the interface.

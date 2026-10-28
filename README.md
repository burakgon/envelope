<div align="center">

<img src="packaging/icons/envelope.svg" width="96" height="96" alt="Envelope logo">

# Envelope

**A fast, beautiful mail and calendar client for Linux.**

Gmail, Microsoft 365, Outlook.com, iCloud, Yahoo, Fastmail and any IMAP server — with a real calendar,<br>
instant offline search, a keyboard-first workflow and optional AI that runs on the provider *you* choose.

[![CI](https://github.com/burakgon/envelope/actions/workflows/ci.yml/badge.svg)](https://github.com/burakgon/envelope/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/license-GPL--3.0--or--later-6d5efc)](LICENSE)
[![Made with Rust](https://img.shields.io/badge/core-Rust-dea584)](crates)
[![Svelte 5](https://img.shields.io/badge/ui-Svelte%205-ff3e00)](src)

[**Try the live demo**](https://burakgon.github.io/envelope/) · [Install](#install) · [Features](#features) · [Build from source](#build-from-source) · [Contributing](CONTRIBUTING.md)

<br>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/inbox-dark.png">
  <img src="docs/screenshots/inbox-light.png" alt="Envelope showing a unified inbox with a conversation open" width="100%">
</picture>

</div>

## Why Envelope?

Desktop mail on Linux has long meant choosing between powerful-but-dated and pretty-but-slow. Envelope is an attempt at both: a native Rust engine that keeps your mail in a local SQLite database, and a carefully designed interface on top of it.

- **Instant.** Lists are read from a local database with precomputed thread aggregates, so opening a folder, switching tabs and searching take milliseconds — online or offline. Every action is optimistic and queued, so the UI never waits for the server.
- **Keyboard-first.** Gmail-style shortcuts (`j`/`k`, `e`, `#`, `r`, `c`, `g i` …) and a command palette (`Ctrl K`) that can do everything the mouse can.
- **A real calendar.** Day, week, month and agenda views, drag to create, move and resize, recurring events with "this / following / all" edits, invitations with RSVP and conflict detection, reminders — synced both ways with CalDAV, Google Calendar and Microsoft 365.
- **Private by design.** No telemetry, no proxy servers, no account with us. Your mail goes straight from your provider to your disk. Remote images and trackers are blocked until you allow them, and messages render in a locked-down sandbox.
- **AI you control.** Summaries, smart replies, writing help, translation and "ask your inbox" — using OpenRouter, OpenAI, Anthropic, Google Gemini, Groq, Mistral, a local Ollama or LM Studio model, or any OpenAI-compatible endpoint. Off until you turn it on, and your API keys live in the system keyring.

## Features

<table>
<tr>
<td width="50%"><img src="docs/screenshots/calendar-week.png" alt="Week view of the calendar"></td>
<td width="50%"><img src="docs/screenshots/calendar-month.png" alt="Month view of the calendar in dark mode"></td>
</tr>
<tr>
<td width="50%"><img src="docs/screenshots/ai-summary.png" alt="AI summary with suggested replies"></td>
<td width="50%"><img src="docs/screenshots/ai-ask.png" alt="Asking a question about your mail with cited sources"></td>
</tr>
<tr>
<td width="50%"><img src="docs/screenshots/compose.png" alt="Composer with recipient autocomplete"></td>
<td width="50%"><img src="docs/screenshots/invite.png" alt="An invitation card with RSVP and conflict check"></td>
</tr>
<tr>
<td width="50%"><img src="docs/screenshots/contacts.png" alt="Contacts with a contact card and recent conversations"></td>
<td width="50%"><img src="docs/screenshots/palette.png" alt="The command palette in dark mode"></td>
</tr>
</table>

### Mail
- **Unified inbox** across all accounts, plus per-account folders and Gmail labels (with colors), drag and drop to move or label.
- **Conversations** threaded the way you expect, with quoted text and signatures folded away.
- **Split inbox**: *People*, *Newsletters* and *Notifications* tabs, computed locally.
- **Instant full-text search** (SQLite FTS5) with operators — `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `before:`, `after:` … — and natural-language search through AI if enabled.
- **Composer** with rich text, inline images, attachments (drag and drop or paste), recipient autocomplete, spell checking, multiple identities and rich signatures, **templates**, **undo send**, **scheduled send** (with an outbox to review, send now or edit) and drafts that sync to the server.
- **Snooze**, star, archive, report spam, bulk actions and **undo** for everything.
- **Follow-up reminders**: get nudged when nobody replied to a message you sent.
- **Rules** that file, label, mark or silence new mail as it arrives — for every kind of account.
- **Safe HTML rendering** in a sandboxed frame with remote content blocking, tracking-pixel removal, dark-mode adaptation for plain emails and `cid:` inline images.
- **Offline-first**: read, search, triage and write without a connection; changes sync when you're back.
- **Push mail** via IMAP IDLE, CONDSTORE/QRESYNC for fast resyncs, Gmail extensions (labels, thread ids, `X-GM-RAW` search) when talking to Gmail.
- Desktop notifications, unread badge, `mailto:` handling.

### Calendar
- Day, week, month and agenda views; mini month; per-calendar visibility and colors.
- Drag to create, drag to move (across days), drag to resize; double-click in month view to add.
- Recurring events with editing scopes (*this event*, *this and following*, *all events*).
- Guests, RSVP, Google Meet / Teams links and one-click **Join**, reminders, busy/free.
- Invitations in mail (iTIP) become cards with **Yes / Maybe / No**, a conflict check against your calendar and replies sent to the organizer.
- Two-way sync: **CalDAV** (iCloud, Fastmail, Nextcloud, Radicale, …), **Google Calendar** and **Microsoft 365 / Outlook.com**.

### Contacts
- Address books synced both ways with **CardDAV**, **Google Contacts** and **Microsoft 365**, plus a local address book.
- Search, edit, photos, birthdays, and a contact card with your recent conversations; autocomplete in the composer draws on both your address books and the people you write to.

### AI (optional)
- Summarize a conversation, suggest replies, draft a reply from a one-line instruction, rewrite (shorter, friendlier, more formal …), translate.
- **Ask your inbox** — answers with citations to the messages they came from.
- Bring your own provider: OpenRouter, OpenAI, Anthropic, Google Gemini, Groq, Mistral, DeepSeek, Together, Fireworks, Cerebras, Perplexity, or local models through Ollama, LM Studio, vLLM and LiteLLM. Responses stream token by token and can be cancelled.

### Design
- Light and dark themes that follow the system, an accent color picker and compact/comfortable density.
- Custom title bar (or native, if you prefer), subtle animations and virtualized lists that stay fluid in very large mailboxes.

## Performance

Numbers from `cargo run --release -p envelope-core --example bench_sync` — a 50 000-message mailbox (16 600 conversations, generated by `scripts/devserver/seed_large.py`) served by a local Dovecot over TLS, on a 16-thread laptop:

| | |
| --- | --- |
| First screen of conversations after adding the account | 0.7 s |
| Complete initial sync of all 50 000 messages | 10 s |
| Open the inbox (first 100 conversations) | 1 ms |
| Page 100 conversations deep at position 10 000 | 2.4 ms |
| Full-text search | 1–3 ms |
| Open a conversation | 0.2 ms |

Real servers add network latency to the sync, but everything you click on is answered from the local database.

## Accounts

| Provider | Mail | Calendar | Sign-in |
| --- | --- | --- | --- |
| Gmail / Google Workspace | IMAP + Gmail extensions | Google Calendar API | Google sign-in (OAuth) |
| Microsoft 365 / Outlook.com / Hotmail | Microsoft Graph | Microsoft Graph | Microsoft sign-in (OAuth) |
| iCloud Mail | IMAP | CalDAV | App-specific password |
| Yahoo Mail | IMAP | CalDAV | App password |
| AOL | IMAP | — | App password |
| Fastmail | IMAP | CalDAV | App password |
| Any other provider | IMAP + SMTP (auto-detected) | CalDAV (optional) | Password |

Server settings are detected automatically from your address (provider presets, the Thunderbird ISP database, autoconfig and DNS SRV records). Proton Mail works through Proton Mail Bridge.

## Install

> Envelope is young — expect rough edges, and please [report them](https://github.com/burakgon/envelope/issues).

- **Debian / Ubuntu, Fedora / openSUSE, AppImage:** download the `.deb`, `.rpm` or `.AppImage` from the [latest release](https://github.com/burakgon/envelope/releases/latest).
- **Arch Linux:** build from the [PKGBUILD](packaging/aur/PKGBUILD) (`makepkg -si`); an AUR package is planned.
- **Flatpak:** a manifest lives in [`packaging/flatpak`](packaging/flatpak); Flathub submission is planned.

## Build from source

You need a recent stable Rust toolchain, Node.js 20+ and the WebKitGTK 4.1 development files.

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libssl-dev
# Fedora
sudo dnf install webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel openssl-devel
# Arch
sudo pacman -S --needed webkit2gtk-4.1 gtk3 libappindicator-gtk3 librsvg base-devel

git clone https://github.com/burakgon/envelope && cd envelope
npm ci
npm run tauri build      # bundles land in target/release/bundle/
npm run tauri dev        # or run in development mode with hot reload
```

Google and Microsoft sign-in need OAuth client ids, supplied at build time or run time — see [docs/OAUTH.md](docs/OAUTH.md). Password-based accounts (iCloud, Yahoo, Fastmail, any IMAP server) work without them.

## Keyboard shortcuts

| Keys | Action | Keys | Action |
| --- | --- | --- | --- |
| `j` / `k` | Next / previous conversation | `c` | Compose |
| `Enter` | Open | `r` / `a` / `f` | Reply / reply all / forward |
| `e` | Archive | `Ctrl Enter` | Send |
| `#` | Delete | `h` | Snooze |
| `s` | Star | `v` / `l` | Move / label |
| `u` | Mark read / unread | `z` | Undo |
| `/` | Search | `Ctrl K` | Command palette |
| `g` then `i` / `s` / `d` / `t` | Inbox / starred / drafts / sent | `g` then `c` | Calendar |
| `t` (calendar) | Today | `d` / `w` / `m` / `a` | Day / week / month / agenda |

Press `?` in the app for the full list.

## How it works

Envelope is a [Tauri 2](https://tauri.app) application: a Rust core and a Svelte 5 interface rendered by WebKitGTK.

| Crate | What it does |
| --- | --- |
| [`envelope-imap`](crates/envelope-imap) | Async IMAP client: TLS/STARTTLS, XOAUTH2, IDLE, CONDSTORE/QRESYNC, MOVE/UIDPLUS, Gmail extensions |
| [`envelope-core`](crates/envelope-core) | The engine: SQLite storage (WAL, FTS5, zstd-compressed bodies), threading, sync workers, offline operation queue, outbox, OAuth, autodiscovery |
| [`envelope-graph`](crates/envelope-graph) | Microsoft Graph client: mail, folders, delta queries, calendar, contacts, batching |
| [`envelope-google`](crates/envelope-google) | Google Calendar and People API client |
| [`envelope-dav`](crates/envelope-dav) | CalDAV / CardDAV client with discovery, sync-collection and multiget |
| [`envelope-ical`](crates/envelope-ical) | iCalendar parsing and writing, recurrence expansion, time zones, iTIP |
| [`envelope-ai`](crates/envelope-ai) | Streaming client for OpenAI-compatible, Anthropic and Gemini APIs |
| [`src-tauri`](src-tauri) | Desktop shell: commands, events, `att://` protocol for attachments, notifications |
| [`src`](src) | The Svelte 5 + TypeScript interface |

## Development

```sh
npm run demo                  # the full UI in a browser with realistic demo data, no backend needed
scripts/devserver/run.sh      # local Dovecot IMAP + SMTP sink + Radicale CalDAV with seeded mail and events
cargo test --workspace        # unit and integration tests
ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_imap --test live_calendar   # against the dev server
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the details.

## Roadmap

- Exchange on-premises (EWS)
- PGP / S/MIME
- Flathub and AUR packages

## License

Envelope is free software, released under the [GNU General Public License v3.0 or later](LICENSE).

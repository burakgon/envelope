# Changelog

All notable changes to Envelope are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [semantic versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] — 2026-10-29

### Added
- Mute conversations (M, the reader toolbar or the command palette): they are archived, and later replies skip the inbox without a notification.
- Automatic replies (out of office) for Microsoft 365, Exchange, Gmail and IMAP servers that offer ManageSieve: a message, an optional subject and first and last day, and who gets it. A banner in the sidebar shows while they are on.
- Exchange contacts can be added, changed and deleted (names, e-mail addresses, phone numbers, company, job title and birthday); fields Envelope does not show are left as they are.
- Calendars subscribed to by URL (`webcal:` and `https:` feeds, also from links in mail and on the web): read-only, refreshed every few hours and only stored again when the feed changed, with Refresh now and Unsubscribe in the sidebar.
- A scheduling assistant in the event editor: when the guests are busy that day (through Google, Microsoft 365 or Exchange, and your own calendars), and a button that moves the meeting to the next time everyone is free.
- Saved messages (`.eml`, decrypted when they are encrypted to one of your keys), calendar files (`.ics`) and contact cards (`.vcf`) open from the file manager: messages with their pictures and attachments, calendar files with a choice of events to add (without inviting anyone), contact cards with a choice of people to add to an address book; whatever is already there is marked.
- End-to-end encryption and signing with OpenPGP: keys created automatically per address (or imported), exchanged with Autocrypt headers and found in the recipient's Web Key Directory; PGP/MIME with protected subjects, Bcc recipients kept anonymous and drafts encrypted to yourself; decryption with passphrase prompts, signature status and fingerprint verification; an Encryption settings tab. See `docs/ENCRYPTION.md`.
- The interface in Deutsch, Español, Français, Italiano, Polski, Português (Brasil), Türkçe, 日本語 and 简体中文 besides English. It follows the system language by default and can be changed under Settings → General; dates and times follow the chosen language, and so do notifications and the tray menu.

### Fixed
- Archive on a new-mail notification, and the Archive action of mail rules, also took the conversation out of folders it was filed in; they now take it out of the inbox only.

## [0.1.1] — 2026-10-29

### Added
- Archive and Mark as Read buttons on new-mail notifications.
- Gmail send-as addresses become identities, with their names, reply-to addresses and signatures.

### Fixed
- The window could stay hidden at startup: it waited for an animation frame that a hidden window never gets.
- Starting at login showed the window when it had been open at the last exit, instead of staying in the tray.
- New-mail notifications for IMAP accounts did not name the sender, and could be suppressed while the window was hidden in the tray.
- The undo send setting showed no choice on a fresh install (the default is now 5 seconds).
- Flatpak: starting at login goes through the Background portal, and the tray icon shows from inside the sandbox.

## [0.1.0] — 2026-10-29

### Mail
- Accounts: Gmail and Google Workspace (Google sign-in), Microsoft 365 and Outlook.com (Microsoft sign-in, Microsoft Graph), on-premises Exchange 2013–SE (Exchange Web Services with NTLM or Basic and Autodiscover), iCloud, Yahoo, AOL, Fastmail and any IMAP/SMTP server, with automatic server detection.
- Unified inbox, conversations, split inbox (People, Newsletters, Notifications), Gmail labels and Outlook categories.
- Offline-first local database with instant full-text search and search operators.
- Composer with rich text, inline images, attachments, recipient autocomplete, spell checking, identities, rich signatures, templates, undo send, scheduled send and an outbox.
- Snooze, star, archive, spam, bulk actions, drag and drop, undo for everything.
- Mail rules that file, label, mark or silence new mail as it arrives.
- Follow-up reminders for sent messages nobody answered.
- Safe HTML rendering with remote-content and tracker blocking, an image viewer and printing.
- Push mail through IMAP IDLE; CONDSTORE/QRESYNC; large messages from Microsoft accounts.

### Calendar
- Day, week, month and agenda views with drag to create, move and resize; recurring events with this/following/all edits.
- Two-way sync with CalDAV, Google Calendar, Microsoft 365 and Exchange; reminders.
- Invitations in mail with RSVP, conflict check and join links; "Add to calendar" from any message with AI.

### Contacts
- Address books from CardDAV, Google, Microsoft 365 and Exchange, plus a local book; contact cards with recent conversations.

### AI (optional, bring your own provider)
- Summaries, suggested replies, drafting, rewriting, translation and answers about your mail with citations.
- OpenRouter, OpenAI, Anthropic, Google Gemini, Groq, Mistral, DeepSeek, Together, Fireworks, Cerebras, Perplexity, Ollama, LM Studio, vLLM, LiteLLM or any OpenAI-compatible endpoint.

### Platform
- Light and dark themes, accent colors, density, keyboard shortcuts and a command palette.
- Tray icon with the unread count, running in the background and starting at login (through the Background portal in Flatpak).
- Packages: .deb, .rpm, AppImage, a Flatpak manifest and a PKGBUILD.

[Unreleased]: https://github.com/burakgon/envelope/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/burakgon/envelope/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/burakgon/envelope/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/burakgon/envelope/releases/tag/v0.1.0

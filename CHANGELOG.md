# Changelog

All notable changes to Envelope are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [semantic versioning](https://semver.org/).

## [Unreleased]

### Added
- Search options under the search box (the sliders button beside it): from, to, subject, words, a date range, the folder on screen, and attachments, unread or starred. They write the query in the search language, so it can be read, changed and saved.
- Saved searches: Save search beside a search keeps it in the sidebar under a name, where a click runs it again; right-click, Shift+F10 or the Menu key renames or deletes it (with Undo).
- Recent searches below the empty search box, and people from the address books and recent mail after `from:`, `to:` or `cc:`; the Down arrow goes into them.
- Search results show the folder each conversation is in.
- VIPs: Add to VIPs in a message's menu, a conversation's right-click menu, the command palette or on a contact's page. Their conversations are gathered under VIPs in the sidebar, a crown marks their messages, `is:vip` (and From a VIP in the search options) finds them, and Settings → Notifications lists them with Remove.
- Notifications for mail from VIPs only, per account; with "Mail from people only", a VIP's mail notifies even when it was sorted as a newsletter.
- New-mail notifications play the desktop's new-mail sound (Settings → Notifications → Sound).

### Fixed
- A few German texts (blocked senders, account alerts) addressed the reader as "du" instead of "Sie" like the rest.

## [0.4.0] — 2026-10-29

### Added
- Drafts open in the composer: from Drafts, from a draft in a conversation (it opens under it), after a restart, and when they were written on another computer or phone, attachments included. Saving such a draft replaces its copy on the server, and sending or discarding it deletes that copy. Drafts kept only on this computer are listed in Drafts too, marked as such.
- Formatting in the composer: text color, highlight, size and alignment, clearing formatting, and inserting pictures. Pictures, pasted ones too, go along as inline parts that every mail program shows.
- Forward as attachment, in a message's menu and a conversation's context menu.
- Save all… for messages with several attachments: they are saved into a folder at once, and Show in folder opens it.
- Send asks first when an address doesn't look complete (Edit or Send anyway), and when the text mentions an attachment but nothing is attached (Attach or Send anyway), with the words for it in every interface language.
- Links in messages show where they go in a small pill while they are pointed at or focused, and warn when their text names another site than the one they open.
- Folder and label management for every kind of account: new folders and subfolders (labels in Gmail, categories in Outlook), rename, delete (asking first, with how many conversations), colors for labels, and nesting by dragging a folder onto another — from a menu on each folder in the sidebar (right-click, Shift+F10 or the Menu key) and a "+" beside each account's folders. The Move and Label menus offer "Create ‘name’" when no folder or label has the name typed.
- Empty Trash and Empty Spam, after a confirmation with the number of conversations.
- "Not spam" in Spam (reader toolbar, context menu, command palette, and `!`, which now toggles): the conversation goes back to the inbox, and the server learns it (`$NotJunk` on IMAP servers, the move itself for Gmail, Microsoft 365 and Exchange). Reporting spam on an IMAP server sets `$Junk`.
- Block sender (from a message's menu, a conversation's right-click menu or the command palette): a "Blocked senders" rule sends their new mail to Spam, and the conversations they wrote move there too. Settings → Privacy lists blocked senders with Unblock.
- Rules can run on the mail already here: "Also apply to existing messages" when saving one, and "Run now" beside each, with a count and Undo.
- Follow-up reminders can be marked Done or come back later ("Remind me again…").
- A banner above the message list when an account needs a new sign-in (with Sign in) or keeps failing to sync (with Retry and Details); the account's entry in Settings says the same.
- Exchange contacts keep postal addresses (work, home and other), a web site and notes, both ways; the contact editor shows these fields for Exchange address books too.
- Notifications per account (in its settings): all new mail in the inbox, mail from people only (no newsletters or automatic notifications), or none.
- Account settings after setup (the gear beside each account in Settings → Accounts): the sender name, how much mail is kept on this computer, whether a copy of sent mail is saved, and for IMAP and Exchange accounts the servers, user names, sign-in method, calendar and address book addresses, self-signed certificates and a new password. Test connection signs in with the changes; saving changed servers signs in first, so a typo can't replace working settings (Save anyway is offered).
- Contacts: Import contacts… and Export contacts… in the Contacts view's menu. Imports read vCard files and the CSV exports of Gmail, Outlook and Thunderbird (in UTF-8, UTF-16 or Windows-1252, with commas, semicolons or tabs), show who is in the file and leave out people already in an address book; exports write the address book shown, or all of them, as one vCard file with photos. Envelope also opens .csv contact lists from the file manager.
- Find duplicates… in the Contacts view's menu: people who are in an address book more than once (the same e-mail address, phone number however it's written, or full name) are listed with the fullest contact chosen to keep; merging adds what the others know to it — e-mail addresses, numbers, postal addresses, web sites, notes, a photo — and deletes them, on the server too.
- Import messages… in a folder's menu puts the mail of mbox files (Thunderbird's local folders, archives, Google Takeout) and saved messages (.eml) into that folder on the server, as read mail with its original date, leaving out messages the account has already; IMAP, Gmail and Exchange accounts (Microsoft 365 doesn't take messages from apps).
- Adding an account lists the accounts already set up on this computer in Thunderbird (also its Flatpak and Snap versions), Evolution, Geary and GNOME Online Accounts; choosing one fills in its address, name and servers, so only the password (or the Google or Microsoft sign-in) is left.
- Custom repeats in the event editor (Repeat → Custom…): every so many days, weeks, months or years; on several weekdays; monthly on a day of the month or on a weekday such as "the second Tuesday" or "the last Friday"; ending never, on a date or after a number of times. The rule reads in words as it is made, and in the event's popover, in every interface language. A calendar that cannot keep a rule says so instead of changing it (Exchange repeats yearly events only every year).
- A time zone for each event (Time zone, beside All day): times are typed in that zone, every kind of calendar keeps it, and the event's popover gives its times there when the zone is not this computer's.
- A second time zone in the day and week views (Settings → Calendar), with an hour column of its own.
- A work week view, Monday to Friday (`x`, or from the command palette).
- Select all (Ctrl+A, or the link in the selection bar) offers “Select all N conversations in this folder” — or of the search — when there are more than the list has loaded; archiving, deleting, moving, marking read and the other actions then take every one of them.
- Reading with the keyboard: Space and Page Down scroll the open conversation, and Space at its end opens the next unread one; Shift+Space and Page Up scroll back, Home and End go to its top and end; `n` and `p` go from message to message and `;` unfolds them all. In a reply under a conversation, Escape goes back to the conversation and a second Escape closes the reply, keeping the draft. The shortcuts sheet and the command palette list them.

### Changed
- Narrow windows: below 1200 pixels wide the sidebar is a rail of icons, and Show sidebar opens the whole of it over the window; below 1000 pixels (half of a 1920-pixel screen) the conversation list and the open conversation take turns, with a Back button, Escape or Backspace to return to the list. However wide the list is made, the conversation beside it keeps room to be read, and a toolbar short of room puts the rest of its buttons in a More menu instead of cutting them off. The calendar, tasks and contacts fit down to the smallest window, 780 pixels wide.
- The header above a quoted reply or forwarded message ("On …, … wrote:", "Forwarded message") and its date are written in the interface language.
- Choosing another From address swaps the signature for that address's and leaves what was written alone; clicking in an empty message writes above the signature, not after it.
- Deleting forever in Trash asks first, with the number of conversations; a draft thrown away with the composer's trash button comes back with Undo.
- Closing the event or contact editor with changes that weren't saved — Escape, Cancel, a click outside, or opening another contact — asks first (Discard or Keep editing) instead of throwing them away.
- The inline reply's pop-out button has a pop-out icon, and message frames start close to their size and fit their text as quotes are folded and unfolded.
- Repeating events with a rule of their own read in words in the event editor ("Weekly on Monday, Wednesday, Friday") instead of the raw rule, and say how often or until when they repeat.
- The agenda lists tasks due at a time of day among the events, in time order.
- Text that was too faint to read comfortably is darker: dates, previews and other secondary text, calendar events, labels, amber and red warnings, and the white text on buttons in the dark theme, which are a deeper shade of the accent color now. Each meets the WCAG AA contrast ratio for its size.
- A custom snooze, follow-up reminder or send-later time is picked with Envelope's own date and time pickers, written the way the interface language writes dates and times, instead of the browser's bare date field.
- Mark all as read acts on the list as shown (the inbox tab, filter or search), says how many conversations it marked, and can be undone.

### Fixed
- A short plain-text reply was hidden together with the quote below it.
- Messages Envelope saved into an Exchange folder from MIME could show as drafts.
- Old mail with 8-bit text and no declared character set showed replacement characters; it is read as Windows-1252, as in other mail programs.
- The address book on this computer was called "On this computer" in English in every interface language, and a contact in it said "In On this computer".
- An event's popover could run off the bottom of the window once its guests had loaded.
- An address still being typed when Send was pressed was left out.
- Account setup no longer keeps the password and servers of a provider you went back from: they are cleared, servers are detected again when the address moves to another domain, and Google or Microsoft sign-in is no longer offered an address of another provider.
- The `v` and `l` shortcuts no longer type their letter into the Move and Label menus.
- Events planned in a zone that is not the main one of its Windows zone (Rome, Toronto, Hong Kong, …) went to Exchange in UTC, so repeating ones moved by an hour when the clocks changed; a series ending on a date ended a day late west of UTC.
- Editing a Microsoft 365 event put it in the computer's time zone; it keeps its own now, and zones go to Microsoft 365 by their Windows names, which it knows for every zone.
- Menus — snooze, move, label, a message's More, templates, the context menus and the others — take the keyboard focus when they open, move with ↑, ↓, Home and End, choose with Enter, keep Tab inside, and close with Escape, putting the focus back where it was; keys pressed in them no longer reach the conversation list behind. Shift+F10 and the Menu key open the context menu of the selected conversation.
- Going to another folder or inbox tab closes the conversation that was open unless the new list has it, instead of leaving it beside an unrelated (even empty) list.
- The search box kept showing a search that going to another folder had ended; it now always shows the search the list is showing, also one started from the command palette or a contact.
- A message closed right after something was typed in it, before it was first saved, lost what was typed. Enter or Space on a focused button no longer also opens the selected conversation.
- `e` in Contacts no longer types an “e” into the first name it starts editing. Choosing a time in the snooze menu opened with `h` or a row's snooze button, and Edit or Discard in the menu of a draft kept only on this computer, no longer fail.
- Dialogs — account setup, Settings, the keyboard shortcuts, the command palette and the others — take the keyboard focus when they open, keep Tab inside, give the focus back when they close, and tell screen readers that they are modal and what they are called.
- Screen readers name every switch and dropdown in Settings, the fields of the rule and contact editors and of the server settings in account setup, and the composer's From and Subject, and hear which conversation is selected as J and K move through the list. The buttons on a row are reached with Tab on the selected row only, and show while they have the focus; whatever has the keyboard focus shows a ring.

## [0.3.0] — 2026-10-29

### Added
- Tasks: a Tasks view (`g k`) with Today, Upcoming, Important, All and the lists of each account; due dates and times, notes, subtasks and importance; quick add that understands dates, times and `!` in the interface language; tasks made from conversations (`t`, the reader toolbar or the command palette) that link back to them; tasks with a due date in the calendar (Settings → Calendar); a notification with Mark as Done when a task with a time comes due (Settings → Notifications). Synced both ways with CalDAV servers that keep tasks (Nextcloud, Radicale, …), Google Tasks, Microsoft To Do and Exchange, plus a list on this computer. Google and Microsoft accounts added before need to sign in again (Settings → Accounts) to show their tasks.

### Changed
- The calendar's all-day row shows "+N more" when a day has more items than fit.
- Questions — removing an account, sending without a subject, a link's address, a new password — are asked in Envelope's own dialogs instead of the system's bare ones, and passwords are no longer shown as they are typed.

### Fixed
- Ctrl+Enter in the message body could send a message twice.

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

[Unreleased]: https://github.com/burakgon/envelope/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/burakgon/envelope/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/burakgon/envelope/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/burakgon/envelope/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/burakgon/envelope/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/burakgon/envelope/releases/tag/v0.1.0

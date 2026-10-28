# Contributing to Envelope

Thanks for helping! Bug reports, design feedback, translations and code are all welcome. For anything bigger than a small fix, please open an issue first so we can agree on the approach.

## Getting set up

Requirements: a recent stable Rust toolchain, Node.js 20+, and the WebKitGTK 4.1 / GTK 3 development packages (see the README for distribution-specific commands).

```sh
npm ci
npm run tauri dev
```

`npm run tauri dev` starts Vite with hot reload for the interface and builds the Rust side in debug mode (dependencies are optimized even in debug builds, so syncing large mailboxes stays fast).

### Working on the interface without a backend

```sh
npm run demo        # http://localhost:1420 — realistic demo data, mocked API
```

The demo implements the whole API in TypeScript (`src/lib/api/mock*.ts`), including AI streaming and the calendar, so most UI work never needs a mail server. `npm run build:demo` produces the static build used for the live demo.

### A local mail and calendar server

`scripts/devserver/run.sh` starts Dovecot (IMAP with TLS and STARTTLS), a tiny SMTP sink that delivers into the IMAP mailboxes and Radicale (CalDAV/CardDAV), all as your user with data in `.devserver/`. It seeds a mailbox and two calendars for `alex@envelope.test` (password `password`). You'll need `dovecot`, `python3` with `aiosmtpd`, and `radicale` installed.

```sh
scripts/devserver/run.sh &
cargo run -p envelope-core --example dev_account -- /tmp/envelope-dev   # adds the dev account to a data directory
ENVELOPE_DATA_DIR=/tmp/envelope-dev/data ENVELOPE_CACHE_DIR=/tmp/envelope-dev/cache \
  ENVELOPE_SECRETS=file ENVELOPE_LOG=info npm run tauri dev
```

Useful environment variables:

| Variable | Effect |
| --- | --- |
| `ENVELOPE_DATA_DIR`, `ENVELOPE_CACHE_DIR` | Use separate directories instead of the XDG defaults |
| `ENVELOPE_SECRETS=file` | Store secrets in a file instead of the system keyring (development only) |
| `ENVELOPE_LOG` | Log filter, e.g. `info` or `envelope_core=debug,envelope_imap=trace` |
| `ENVELOPE_GOOGLE_CLIENT_ID`, `ENVELOPE_MICROSOFT_CLIENT_ID`, … | OAuth clients — see [docs/OAUTH.md](docs/OAUTH.md) |

## Tests and checks

```sh
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
npm run check                       # svelte-check / TypeScript

# end-to-end tests against the local server (resets .devserver state)
ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_imap --test live_calendar
```

Please make sure all of these pass before opening a pull request. New protocol code should come with tests — the crates use recorded server responses and `wiremock` so they run offline.

## Project layout

- `crates/` — the Rust libraries (protocol clients and the sync engine). Each crate is usable on its own and has its own docs.
- `src-tauri/` — the desktop shell: Tauri commands, events and the `att://` protocol.
- `src/` — the Svelte 5 + TypeScript interface. `src/lib/api` is the typed boundary to the backend (`tauri.ts`) and the demo (`mock.ts`); `src/lib/stores` hold app state; `src/lib/components` the UI.
- `packaging/` — icons, desktop entry, AppStream metadata, Flatpak and Arch packaging.
- `scripts/` — the development server, screenshots for the README.

## Guidelines

- **Fast paths stay fast.** Anything on the path from a keypress to pixels must not wait on the network. Write to the local database, update the UI optimistically and queue the server operation.
- **No telemetry, no third-party services** beyond the user's own mail, calendar and (opt-in) AI providers.
- Match the style of the code around you; keep comments for the *why*.
- Interface text is plain, friendly English in sentence case. Keyboard shortcuts should work wherever a mouse action exists.
- Commit messages: a short imperative summary line, then the details if needed.

## Reporting security issues

Please don't open public issues for security problems — see [SECURITY.md](SECURITY.md).

By contributing you agree that your contributions are licensed under the GPL-3.0-or-later, like the rest of the project.

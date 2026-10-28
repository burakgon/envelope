# Security policy

Envelope handles mail, calendars and account credentials, so we take security reports seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's [private vulnerability reporting](https://github.com/burakgon/envelope/security/advisories/new) instead, with:

- a description of the problem and its impact,
- steps to reproduce (a sample message that triggers it is ideal),
- the Envelope version and your distribution.

You'll get an acknowledgement within a few days. Once a fix is available we publish an advisory and credit you, unless you prefer otherwise.

## Scope

Particularly interesting areas:

- escaping the HTML mail sandbox, running script from a message, or loading remote content without consent;
- leaking credentials, OAuth tokens or message content to anyone other than the user's own providers;
- TLS or certificate validation problems in the IMAP, SMTP, DAV or HTTP clients;
- parsing bugs (MIME, iCalendar, vCard, IMAP responses) that crash the app or corrupt data.

## Supported versions

Only the latest release receives security fixes.

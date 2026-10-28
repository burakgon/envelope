# Signing in with Google and Microsoft

Gmail and Microsoft accounts are added through the provider's own sign-in page (OAuth 2.0 with PKCE). For that, the provider has to know the app: Envelope needs an **OAuth client id** for Google and one for Microsoft.

Official release builds ship with client ids. If you build Envelope yourself, register your own clients as described below — it takes about ten minutes and costs nothing. Accounts that use a password or an app password (iCloud, Yahoo, Fastmail, any IMAP server) do not need any of this.

Client ids are read from environment variables, either **at build time** (baked into the binary) or **at run time** (overriding the built-in value):

| Variable | Used for |
| --- | --- |
| `ENVELOPE_GOOGLE_CLIENT_ID` | Google sign-in |
| `ENVELOPE_GOOGLE_CLIENT_SECRET` | Google sign-in (desktop clients get a "secret" that is not confidential; Google still requires it) |
| `ENVELOPE_MICROSOFT_CLIENT_ID` | Microsoft sign-in |
| `ENVELOPE_MICROSOFT_TENANT` | Optional: `common` (default — work, school and personal accounts), `organizations`, `consumers` or your tenant id |

```sh
# bake them into a build
ENVELOPE_GOOGLE_CLIENT_ID=… ENVELOPE_GOOGLE_CLIENT_SECRET=… ENVELOPE_MICROSOFT_CLIENT_ID=… npm run tauri build

# or provide them when starting the app
ENVELOPE_MICROSOFT_CLIENT_ID=… envelope
```

Envelope receives the sign-in result on a temporary loopback address (`http://127.0.0.1:<random port>` for Google, `http://localhost:<random port>` for Microsoft), so no web server or redirect domain is involved, and tokens are stored in the system keyring (Secret Service or KWallet; the portal keyring inside Flatpak). Only if no keyring is available do they fall back to a file readable by your user alone.

## Google

1. Open the [Google Cloud console](https://console.cloud.google.com/) and create a project (or pick an existing one).
2. **APIs & Services → Library**: enable the **Google Calendar API** and the **People API**. (Mail uses IMAP and SMTP, which need no API to be enabled.)
3. **APIs & Services → OAuth consent screen** (Google Auth Platform):
   - User type **External** (or **Internal** for a Workspace-only setup).
   - Add these scopes under **Data access**:
     - `https://mail.google.com/`
     - `https://www.googleapis.com/auth/calendar`
     - `https://www.googleapis.com/auth/contacts.readonly`
     - `https://www.googleapis.com/auth/contacts.other.readonly`
     - `openid`, `email`, `profile`
   - While the app is in *Testing*, add the Google accounts you want to use under **Audience → Test users**. (`https://mail.google.com/` is a restricted scope; publishing an app with it for everyone requires Google's verification.)
4. **Clients → Create client**, application type **Desktop app**. Copy the client id and client secret into `ENVELOPE_GOOGLE_CLIENT_ID` and `ENVELOPE_GOOGLE_CLIENT_SECRET`.

## Microsoft (Outlook.com, Hotmail, Microsoft 365)

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com/) → **App registrations → New registration**.
   - Name: anything, e.g. *Envelope*.
   - Supported account types: **Accounts in any organizational directory and personal Microsoft accounts** (or narrower, matching `ENVELOPE_MICROSOFT_TENANT`).
   - Redirect URI: platform **Public client/native (mobile & desktop)**, URI `http://localhost`.
2. In the new registration, **Authentication**: make sure `http://localhost` is listed under *Mobile and desktop applications* and set **Allow public client flows** to *Yes*. No client secret is needed.
3. **API permissions → Add a permission → Microsoft Graph → Delegated permissions**, add:
   `offline_access`, `openid`, `email`, `profile`, `User.Read`, `Mail.ReadWrite`, `Mail.Send`, `MailboxSettings.ReadWrite`, `Calendars.ReadWrite`, `Contacts.Read`, `People.Read`.
   For work or school tenants that require it, an administrator can **Grant admin consent** here.
4. Copy the **Application (client) id** from the overview page into `ENVELOPE_MICROSOFT_CLIENT_ID`.

## Troubleshooting

- *"No Google/Microsoft OAuth client is configured"* — the client id variable was empty both at build time and at run time.
- *Google: "Access blocked: app has not completed the Google verification process"* — add your account as a test user (step 3), or use an app password with IMAP instead.
- *Microsoft: "AADSTS50011: redirect URI mismatch"* — the registration is missing the `http://localhost` redirect under *Mobile and desktop applications*.
- *Microsoft: "Need admin approval"* — your organization restricts user consent; ask an administrator to grant consent for the permissions above.
- The browser shows "connection refused" after signing in — the sign-in took longer than ten minutes, or another program is filtering loopback connections. Try again.

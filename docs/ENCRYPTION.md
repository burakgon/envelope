# End-to-end encryption (OpenPGP)

Envelope encrypts and signs mail with OpenPGP, the standard used by Thunderbird, GnuPG, K-9 Mail, Delta Chat, Proton and Mailvelope. The goal is that it **just works** for people who never heard of keys — in the spirit of [Autocrypt](https://autocrypt.org/) — while giving experts full control in **Settings → Encryption**.

The cryptography comes from [rPGP](https://github.com/rpgp/rpgp) (the pure Rust `pgp` crate, MIT/Apache-2.0). The OpenPGP layer of Envelope lives in [`crates/envelope-pgp`](../crates/envelope-pgp) (keys, PGP/MIME, Autocrypt, Web Key Directory; no network, disk or keyring access) and the engine side in [`crates/envelope-core/src/crypto`](../crates/envelope-core/src/crypto).

## How it works for most people

1. The first time you send from an address, Envelope creates a key for it (Ed25519 for signing with a Curve25519 encryption subkey — the shape GnuPG creates by default) and attaches the public key to your messages in an `Autocrypt` header.
2. When mail arrives from someone whose app does the same (Thunderbird, Delta Chat, K-9, another Envelope …), their key is remembered.
3. When everyone you write to has a key, the lock in the composer can be turned on. With **Encrypt when possible** it turns on by itself; when you reply to an encrypted message it starts on.
4. Encrypted mail you receive is decrypted when you open it. The message shows **Encrypted** and who signed it.

## Keys

| What | Where |
| --- | --- |
| Your secret keys | The system keyring (Secret Service / KWallet, the portal keyring inside Flatpak), one item per key named `pgp:secret:<fingerprint>`, ASCII armored and exactly as generated or imported (protected by its passphrase if it has one). Like account passwords, they fall back to an owner-only file in the data folder only on systems without any keyring. Secret keys are never written to the database. |
| Public keys | The `pgp_keys` table: the certificate, its user ids and addresses, key ids, algorithm, creation and expiry, revocation, where it came from (`own`, `autocrypt`, `gossip`, `wkd`, `keyserver`, `manual`) and whether you verified it. |
| Which key each of your addresses uses | `pgp_own` (address → fingerprint). Keys belong to addresses, not accounts, so removing an account keeps them. |
| Autocrypt state of your correspondents | `pgp_peers`: last seen, the date and key of their newest `Autocrypt` header, `prefer-encrypt`, gossip. |

- **Generate, import, export, delete** per address in Settings → Encryption. Imports take armored or binary data, public or secret, one key or many, pasted or from a file; the passphrase of a protected secret key is checked on import. Exporting a secret key warns first and offers to protect the exported copy with a passphrase.
- **Passphrases** are asked for when a protected key is first needed (to read a message or to sign) and kept in memory until Envelope quits (or *Forget now*). Keys Envelope creates have no passphrase by default: the keyring already protects them at rest; experts can set one when creating a key.
- **Verification**: compare a fingerprint with its owner (in person, on a call) and mark the key verified. Verified keys are preferred when sending, and signatures from them show *verified*.

### Where other people's keys come from

- **Autocrypt headers** on incoming mail, following the [Level 1 rules](https://autocrypt.org/level1.html): the header's `addr` must match `From`, messages with several headers are ignored, a newer message's key replaces an older one, and `prefer-encrypt=mutual` is recorded. Messages from your own addresses and in Junk are ignored.
- **Autocrypt-Gossip** inside encrypted messages to several people, accepted only from messages whose signature checks out against the sender.
- **Web Key Directory** (WKD): while you compose to someone without a key, Envelope asks *their own domain* (`openpgpkey.<domain>` first, then `<domain>`) — at most once an hour per address, and only with *Look up keys automatically* on. No other server is contacted silently.
- **keys.openpgp.org**: only when you ask (*Search keys.openpgp.org* in the composer, *Find* in settings, *Look up the sender's key* on a message).
- **Manual import**.

When several keys carry an address, Envelope encrypts to the one you verified, else one you imported, else the newest Autocrypt key, else one from a key directory, else a gossiped one — and only to keys that are valid (not expired or revoked, with a valid encryption subkey).

## Sending

- **PGP/MIME** ([RFC 3156](https://www.rfc-editor.org/rfc/rfc3156)): signed and encrypted mail is one OpenPGP message (signature inside the encryption, as Thunderbird and Enigmail do) in `multipart/encrypted` with `application/pgp-encrypted` and `application/octet-stream` parts; signed-only mail is `multipart/signed` with an `application/pgp-signature` part (`micalg` matching the hash). Messages use SEIPD v1 with AES-256 (or what every recipient's key prefers), so GnuPG 2.2/2.4 reads them.
- Mail is **encrypted to every recipient and to the sender**, so the copy in Sent stays readable.
- **Protected headers** (`protected-headers="v1"`, RFC 9788): Subject, From, To, Cc, Reply-To, Date, Message-ID and the threading headers are copied into the encrypted part; with *Hide subjects of encrypted messages* (on by default) the visible subject becomes `...`. Envelope shows the real subject once the message is decrypted.
- **Bcc**: Bcc recipients are encrypted to as *anonymous recipients* (their key ids are left out of the message, like `gpg --hidden-recipient` and Thunderbird), so To and Cc recipients cannot tell who they are — only that someone else could decrypt the message. Bcc addresses never appear in the headers.
- **Autocrypt header** with your (minimized) public key on every message you send, unless *Send my public key with messages* is off. `prefer-encrypt=mutual` is set when *Encrypt when possible* is on. Encrypted messages to several people carry `Autocrypt-Gossip` headers inside the encryption.
- **Signing** defaults to *Sign messages by default*, and is on for encrypted messages.

### Drafts

Drafts of encrypted messages are **encrypted to your own key before they are stored on the server** (nothing is signed; protected headers hide the subject), so the server never sees their content, and you can still open them in another OpenPGP client that has your key. If the sending address has no key yet, an encrypted draft is not uploaded and stays on this device. The local draft (in Envelope's database, like every draft) is not encrypted.

## Receiving

- Envelope recognizes PGP/MIME encrypted and signed messages (also a `multipart/signed` part followed by a mailing list footer, and sign-then-encrypt nesting) and **inline PGP** (`-----BEGIN PGP MESSAGE-----` and cleartext signed `-----BEGIN PGP SIGNED MESSAGE-----` text). PGP/MIME messages are always downloaded whole.
- Encrypted messages are stored **as received** (the ciphertext, compressed). The message list shows `...` and *Encrypted message* until they are opened.
- Opening decrypts with your secret keys (asking for a passphrase when a key has one), checks signatures against known keys and shows a status on the message:
  - **Encrypted**
  - **Signed by *name* — verified**: a good signature by a key you verified
  - **Signed by *name***: a good signature by a key carrying the sender's address
  - **Signed by *name* — not the sender's key**, or *key expired / revoked*
  - **Signed — unknown key**: Envelope has no key to check it (one click looks it up)
  - **Bad signature**: the message was changed after signing, or the signature is forged

  Clicking the status shows the signer's key, fingerprint, where the key came from and lets you verify it.
- Decrypted content goes through the same sanitizing renderer as all mail (remote content blocked, scripts removed, sandboxed frame). Attachments inside the encryption are decrypted when you open them, into `~/.cache/envelope/decrypted`, which is emptied every time Envelope starts.

### Decrypted text, caching and search

By default, **decrypted content is kept only in memory** for the session: it is not written to the database and not indexed, so encrypted mail cannot be searched and shows no preview, and quitting Envelope leaves no plaintext behind (except attachments you opened, until the next start).

With **Remember decrypted messages** (Settings → Encryption, off by default), the decrypted text and the real subject are stored in Envelope's local database like other mail, so encrypted messages get previews and can be searched; new encrypted mail is then decrypted in the background when no passphrase is needed. The database is not encrypted, so this is a trade-off for a device you trust. Turning the setting off removes every stored decrypted copy.

## Interoperability

The tests decrypt and verify messages made by GnuPG 2.4 shaped like Thunderbird, Enigmail and Mutt mail (`crates/envelope-pgp/tests/fixtures`, regenerated by `generate.py`), and check that GnuPG decrypts and verifies what Envelope writes (when `gpg` is installed). `ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_pgp` sends an encrypted, signed message with an attachment to yourself through the development server and opens it.

## Not (yet) supported

- S/MIME.
- Editing keys: changing expiry, adding user ids or subkeys, revoking (use GnuPG and import the result).
- Certifications between keys (web of trust); verification is a local mark.
- The Autocrypt Setup Message for moving a key between devices (export and import instead).
- Encrypted or signed messages larger than about 3 MB through Microsoft 365 accounts (Microsoft Graph needs large messages split up, which would undo the encryption).
- Invitations inside encrypted messages, and protected headers for signed-only mail.

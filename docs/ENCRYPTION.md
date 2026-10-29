# End-to-end encryption (OpenPGP and S/MIME)

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

## S/MIME

S/MIME is what Outlook, Apple Mail and most companies use. Instead of keys people exchange with each other, an authority — the company's own, or a public certificate authority — issues a **certificate** that vouches for an address. Envelope reads and sends S/MIME next to OpenPGP, and S/MIME messages go through the same steps: recognized when they are downloaded, opened (checked, decrypted) when they are shown, protected when they are sent.

The S/MIME layer is [`crates/envelope-smime`](../crates/envelope-smime) — certificates, PKCS #12, certificate chains, CMS and S/MIME messages, on the RustCrypto crates (`x509-cert`, `cms`, `pkcs12`, `der`, `rsa`, `p256`, `p384`, `sha1`, `sha2`, `aes`, `cbc`, `aes-gcm`, `aes-kw`, `des` and `rc2` for reading old files, all MIT/Apache-2.0), without network, disk or keyring access — and the engine side is [`crypto/smime.rs`](../crates/envelope-core/src/crypto/smime.rs). CMS and PKCS #12 data are read as BER, the way Thunderbird, Windows and `openssl -stream` write them (indefinite lengths, strings in pieces).

### Certificates

| What | Where |
| --- | --- |
| Your private keys | The system keyring, one item per certificate named `smime:secret:<fingerprint>` (the certificate's SHA-256), as unencrypted PKCS #8: the keyring protects it at rest, as it does the OpenPGP keys Envelope creates. The password of the `.p12` file is only used to import it. Never in the database. |
| Certificates | The `smime_certs` table: the certificate (DER), its addresses (and the address it was imported for), whether it is an authority's, whether its private key is in the keyring, and where it came from (`own`, `message`, `manual`, or `chain` for authorities). |

- **Your certificate**: Settings → Encryption → S/MIME → *Import certificate…* takes a `.p12` or `.pfx` file, as Windows, macOS, browsers, Thunderbird and OpenSSL export them, and asks for its password in Envelope's own dialog (again while it is wrong). Both kinds of these files are read: PBES2 with AES (OpenSSL 3, recent Windows) and the older SHA-1 with 3DES or RC2 (older Windows and OpenSSL); the password is checked against the file's MAC. The authorities' certificates in the file are kept, to be sent along with signatures. Companies that issue separate certificates for signing and for encryption are covered: the one that may sign signs, the one that may receive encrypted mail is the one others encrypt to. *Save certificate…* exports the certificate (PEM, or DER for `.cer` and `.der` files) — never the private key: keep the `.p12` file for that.
- **Other people's certificates** arrive with their signed mail, as Autocrypt keys do: when a signed message *verifies* (below), the signer's certificate is kept, with the certificate they ask to be encrypted to (`SMIMEEncryptionKeyPreference`, or Outlook's attribute for it) and the authorities in between — not from messages in Junk, and not for your own addresses. They can be imported by hand too (PEM, DER, PKCS #7 `.p7b`).
- When several certificates carry an address, mail is encrypted to one you imported, else the newest from their mail; it must be valid and meant for encrypting mail (key usage and extended key usage).

### When a signature is verified

A signature shows **Signed by *name* — verified** only when every check passes. The checks run in this order, and the message shows the first that failed, saying which:

1. **The signature fits the message**: the digest in the signed attributes matches the content, the content type matches, and the signature over the attributes is valid for the certificate's key — RSA (PKCS #1 v1.5 and PSS) or ECDSA on P-256 and P-384, with SHA-224 to SHA-512; SHA-1 only when reading old mail. Otherwise **Bad signature**: the message was changed after it was signed, or the signature doesn't match the certificate it names.
2. **The certificate is there**: in the message, or kept from earlier mail. Otherwise *Signed — certificate missing*.
3. **The certificate is valid now**: otherwise *certificate expired* or *not valid yet*.
4. **It may sign e-mail**: when it restricts them, its key usage includes digital signatures (or non-repudiation) and its extended key usage e-mail protection (RFC 8550). Otherwise *certificate not for e-mail*.
5. **An authority this computer trusts vouches for it**: a chain leads from the certificate through the authorities in the message (or kept from earlier mail) to a trusted root. Trusted are the authorities TLS trusts on this computer — the system certificate store as `rustls-native-certs` reads it (`SSL_CERT_FILE` and `SSL_CERT_DIR`, else the distribution's bundle) — and the distribution's bundle of authorities trusted for e-mail where it has one (p11-kit's `email-ca-bundle.pem`, on Arch and Fedora for example). Every certificate in the chain must be valid now and signed by the next (SHA-1 signatures are refused); authorities must be marked as authorities (basic constraints, the certificate-signing key usage) and respect path length limits; authorities that restrict their extended key usage must allow e-mail; name constraints are applied to addresses, domains, directory names and IP addresses (a constraint on another name form fails); unknown critical extensions fail. Otherwise *certificate not trusted*, with the reason (no trusted authority, an authority's certificate expired, the rule the chain breaks). Revocation is **not** checked (no CRLs, no OCSP). To trust your company's own authority, install its root certificate in the system (`trust anchor`, `update-ca-trust` or `update-ca-certificates`, depending on the distribution) and start Envelope again.
6. **It is the sender's certificate**: one of its addresses — its subjectAltName addresses, or the subject's e-mail attribute in certificates without one — is the From address. Otherwise *not the sender's address*.

Clicking the status shows the certificate: its owner and addresses, the authorities up to the root, validity, when the message was signed, serial number, SHA-256 fingerprint and algorithms.

### Receiving

- **Signed** mail (`multipart/signed` with `application/pkcs7-signature`, or Outlook's `x-pkcs7-signature`) shows its content as it is; the `smime.p7s` part is not an attachment. **Opaque signed** mail (`application/pkcs7-mime; smime-type=signed-data`) is not secret: its content is stored as the message body, with a preview and search, and its attachments are read from inside the signature when the message is opened.
- **Encrypted** mail (`enveloped-data`, and `authEnveloped-data` with AES-GCM) is stored as received and decrypted when it is opened, with the private keys of your certificates: RSA key transport (PKCS #1 v1.5 or OAEP) or ECDH key agreement (P-256, P-384); AES-CBC, AES-GCM and, for old mail, 3DES. A signature inside is checked like any other. Mail encrypted to a certificate whose private key isn't here says so, and offers to import it. Decrypted S/MIME mail is kept like decrypted OpenPGP mail: in memory for the session, or with *Remember decrypted messages* in the database.
- Every kind of account delivers the MIME source S/MIME needs: IMAP downloads S/MIME messages whole, whatever their size, and Microsoft 365 (Graph `$value`) and Exchange (EWS `MimeContent`) messages are downloaded as MIME already.

### Sending

- When you have a certificate for the sending address, the lock in the composer offers **OpenPGP or S/MIME** for the message. S/MIME is the default when you reply to S/MIME mail, when the address has a certificate and no OpenPGP key, or when every recipient has a certificate but not everyone an OpenPGP key. An address with a certificate gets no OpenPGP key made just for the Autocrypt header.
- Signed mail is `multipart/signed` (detached: it reads everywhere, signed or not), RSA with SHA-256 or ECDSA with SHA-256 or SHA-384, carrying your certificate, its authorities, the certificate others should encrypt to (`SMIMEEncryptionKeyPreference` and Outlook's attribute) and the ciphers Envelope reads (`SMIMECapabilities`).
- Encrypted mail is `application/pkcs7-mime; smime-type=enveloped-data` with AES-256-CBC, which Outlook, Thunderbird, Apple Mail and OpenSSL all read: to every recipient's certificate (RSA key transport, or ECDH with an ephemeral key and AES key wrap) **and to your own**, so the copy in Sent stays readable. With signing on, the message is signed first and the signed message encrypted. It needs a certificate for every recipient.
- Unlike OpenPGP, S/MIME names every recipient's certificate in the encrypted message, Bcc recipients' too (the composer says so), and it leaves the subject as it is.
- Drafts of encrypted S/MIME messages are stored on the server encrypted to your own certificate alone; without one they stay on this device.

The tests read certificates, PKCS #12 files (OpenSSL 3 and legacy) and messages made with the OpenSSL command line ([`tests/fixtures/generate.py`](../crates/envelope-smime/tests/fixtures/generate.py)), Outlook- and Thunderbird-shaped ones among them; they check every failing case (changed content, damaged signature, expired certificate, unknown root, a certificate for TLS servers, a broken name constraint, another sender) and that OpenSSL verifies and decrypts what Envelope writes.

## Interoperability

The tests decrypt and verify messages made by GnuPG 2.4 shaped like Thunderbird, Enigmail and Mutt mail (`crates/envelope-pgp/tests/fixtures`, regenerated by `generate.py`), and check that GnuPG decrypts and verifies what Envelope writes (when `gpg` is installed). `ENVELOPE_TEST_IMAP=1 cargo test -p envelope-core --test live_pgp` sends an encrypted, signed message with an attachment to yourself through the development server and opens it.

## Not (yet) supported

- S/MIME: revocation checks (CRL, OCSP), certificates with Ed25519 or P-521 keys, mail encrypted with RC2 or single DES, key agreement with the sender's certificate instead of an ephemeral key, triple-wrapped messages (signed, encrypted, then signed again), finding certificates in directories (LDAP), exporting your private key (keep the `.p12` file), and trusting an authority from inside Envelope (install it in the system).
- Editing keys: changing expiry, adding user ids or subkeys, revoking (use GnuPG and import the result).
- Certifications between keys (web of trust); verification is a local mark.
- The Autocrypt Setup Message for moving a key between devices (export and import instead).
- Encrypted or signed messages larger than about 3 MB through Microsoft 365 accounts (Microsoft Graph needs large messages split up, which would undo the encryption).
- Invitations inside encrypted messages, and protected headers for signed-only mail.

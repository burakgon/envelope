#!/usr/bin/env python3
"""Regenerates the S/MIME test fixtures with the OpenSSL command line tool
(3.x, with its legacy provider for the old-style PKCS #12 file).

    python3 crates/envelope-smime/tests/fixtures/generate.py

Keys are new on every run; the tests read everything they compare from the
files written here.

Authorities:
- root.pem              Envelope Test Root CA, the tests' trust anchor
- mail-ca.pem           an intermediate for e-mail certificates
- constrained-ca.pem    an intermediate that may only certify @example.com
- other-root.pem        a root no test trusts

People (certificate `<name>.pem`, PKCS #8 key `<name>.key`):
- alice     RSA 2048, alice@example.com; alice.p12 (PBES2, AES-256,
            password "alice-pass") and alice-legacy.p12 (3DES and RC2-40,
            SHA-1 MAC, as older Windows and OpenSSL wrote them)
- bob       ECDSA P-256, bob@example.org; bob.p12 ("bob-pass")
- carol     ECDSA P-384, carol@example.net
- dave      expired at the end of 2023, address in the subject only
- erin      certified for TLS servers, not for e-mail
- mallory   issued by other-root
- olga      issued by constrained-ca, within its constraint
- oscar     issued by constrained-ca, outside it (oscar@example.net)

Messages:
- signed-alice.eml        multipart/signed by OpenSSL (BER, streamed), RSA
                          PKCS #1 v1.5 with SHA-256, chain included
- signed-bob.eml          ECDSA P-256 with SHA-256
- signed-carol.eml        ECDSA P-384 with SHA-384
- signed-pss.eml          RSA-PSS with SHA-256
- signed-sha1.eml         RSA with SHA-1, as old mail is
- opaque-alice.eml        application/pkcs7-mime; smime-type=signed-data
- encrypted-alice.eml     signed by Bob, then encrypted to Alice and Bob
                          (RSA key transport, AES-256-CBC, streamed BER)
- encrypted-bob.eml       to Bob (ECDH P-256, SHA-256 KDF, AES key wrap),
                          AES-128-CBC; opaque signed by Alice inside
- encrypted-carol.eml     to Carol (ECDH P-384 with OpenSSL's default KDF)
- authenc-alice.eml       authEnveloped-data, AES-256-GCM
- des3-alice.eml          3DES, as old mail is
- oaep-alice.eml          RSA-OAEP with SHA-256 key transport
- outlook-signed.eml      shaped like Outlook: x-pkcs7-signature, the micalg
                          as an OID, multipart/alternative and a PDF inside,
                          DER signature
- outlook-encrypted.eml   Outlook's encrypted form (DER), Bob to Alice, an
                          Outlook-style signed message inside
- tampered.eml            signed-alice.eml with its text changed afterwards
- bad-signature.eml       a signature whose value was damaged
- expired.eml             signed by Dave's expired certificate
- untrusted.eml           signed by Mallory (unknown root)
- mismatch.eml            signed by Alice, sent as eve@example.com
- not-for-email.eml       signed by Erin's TLS certificate
- no-certs.eml            signed by Alice without her certificate
- constrained-ok.eml      signed by Olga
- constrained-bad.eml     signed by Oscar
"""
import base64
import os
import secrets
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
START = "20240101000000Z"
END = "20491231235959Z"
ROOT_END = "20991231235959Z"

EXTENSIONS = """
[root]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash

[ca]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
extendedKeyUsage = emailProtection
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always

[constrained]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
nameConstraints = critical, permitted;email:example.com
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always

[rsa]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = emailProtection
subjectAltName = email:$ENV::EMAIL
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always

[ec]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyAgreement
extendedKeyUsage = emailProtection
subjectAltName = email:$ENV::EMAIL
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always

[subject_only]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = emailProtection
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always

[server]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = email:$ENV::EMAIL
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always
"""


class Work:
    def __init__(self, path):
        self.path = path
        self.ext = os.path.join(path, "ext.cnf")
        with open(self.ext, "w") as f:
            f.write(EXTENSIONS)

    def __call__(self, name):
        return os.path.join(self.path, name)


def openssl(*args, data=None, email="nobody@example.com"):
    env = {**os.environ, "EMAIL": email}
    return subprocess.run(["openssl", *args], input=data, check=True,
                          capture_output=True, env=env).stdout


def crlf(text):
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


def write(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode())


def read(name):
    with open(os.path.join(HERE, name), "rb") as f:
        return f.read()


def base64_lines(data, width=76):
    text = base64.b64encode(data).decode()
    return "\r\n".join(text[i:i + width] for i in range(0, len(text), width)) + "\r\n"


def serial():
    # Positive, as RFC 5280 asks: the first digit is never 8-f.
    return "0x1" + secrets.token_hex(8)


def new_key(path, kind):
    if kind == "rsa":
        openssl("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", path)
    else:
        openssl("genpkey", "-algorithm", "EC", "-pkeyopt", f"ec_paramgen_curve:{kind}",
                "-out", path)


def certify(w, name, subject, kind, profile, issuer=None, email="nobody@example.com",
            not_before=START, not_after=END):
    """Makes a key and a certificate for it, signed by `issuer` (or itself)."""
    new_key(w(f"{name}.key"), kind)
    openssl("req", "-new", "-key", w(f"{name}.key"), "-subj", subject, "-out", w(f"{name}.csr"))
    args = ["x509", "-req", "-in", w(f"{name}.csr"), "-extfile", w.ext, "-extensions", profile,
            "-set_serial", serial(), "-not_before", not_before, "-not_after", not_after,
            "-sha256", "-out", w(f"{name}.pem")]
    if issuer:
        args += ["-CA", w(f"{issuer}.pem"), "-CAkey", w(f"{issuer}.key")]
    else:
        args += ["-key", w(f"{name}.key")]
    openssl(*args, email=email)
    shutil.copy(w(f"{name}.pem"), os.path.join(HERE, f"{name}.pem"))


def headers(sender, to, subject, mid, extra=""):
    return crlf(
        f"From: {sender}\nTo: {to}\nSubject: {subject}\n"
        f"Date: Wed, 28 Oct 2026 09:30:00 +0100\nMessage-ID: <{mid}>\n" + extra
    )


ALICE = "Alice Example <alice@example.com>"
BOB = "Bob Example <bob@example.org>"
CAROL = "Carol Example <carol@example.net>"

TEXT = crlf(
    "Content-Type: text/plain; charset=utf-8\n"
    "Content-Transfer-Encoding: quoted-printable\n\n"
    "Hello Bob,\n\nthe figures are below. G=C3=B6r=C3=BC=C5=9F=C3=BCr=C3=BCz!\n\n"
    "Q1: 42\nQ2: 47\n"
)


def smime_sign(w, signer, entity, *args, chain="mail-ca"):
    """An S/MIME entity signed by OpenSSL's own writer (CRLF line ends)."""
    extra = ["-certfile", w(f"{chain}.pem")] if chain else []
    return openssl("cms", "-sign", "-signer", w(f"{signer}.pem"), "-inkey", w(f"{signer}.key"),
                   *extra, "-crlfeol", *args, data=entity.encode() if isinstance(entity, str)
                   else entity)


def der_signature(w, signer, entity, md="sha256", chain="mail-ca"):
    """A detached signature (DER) over exactly these bytes."""
    extra = ["-certfile", w(f"{chain}.pem")] if chain else []
    return openssl("cms", "-sign", "-binary", "-noindef", "-outform", "DER", "-md", md,
                   "-signer", w(f"{signer}.pem"), "-inkey", w(f"{signer}.key"), *extra,
                   data=entity)


def multipart_signed(entity, signature, boundary, protocol="application/pkcs7-signature",
                     micalg="sha-256", preamble="This is a cryptographically signed message in MIME format.\r\n"):
    return (
        crlf(f'MIME-Version: 1.0\nContent-Type: multipart/signed; protocol="{protocol}";\n'
             f' micalg={micalg}; boundary="{boundary}"\n\n').encode()
        + preamble.encode() + f"\r\n--{boundary}\r\n".encode()
        + entity
        + crlf(f"\n--{boundary}\nContent-Type: {protocol}; name=\"smime.p7s\"\n"
               "Content-Transfer-Encoding: base64\n"
               "Content-Disposition: attachment; filename=\"smime.p7s\"\n"
               "Content-Description: S/MIME Cryptographic Signature\n\n").encode()
        + base64_lines(signature).encode()
        + crlf(f"\n--{boundary}--\n").encode()
    )


def pkcs7_mime(cms, smime_type, x=False):
    kind = "application/x-pkcs7-mime" if x else "application/pkcs7-mime"
    return (
        crlf(f'MIME-Version: 1.0\nContent-Type: {kind}; smime-type={smime_type};\n'
             ' name="smime.p7m"\nContent-Transfer-Encoding: base64\n'
             'Content-Disposition: attachment; filename="smime.p7m"\n\n').encode()
        + base64_lines(cms).encode()
    )


def encrypt(w, entity, recipients, *args, keyopts=()):
    """Enveloped data (DER) for the recipients' certificates; `keyopts` apply
    to the last recipient."""
    files = []
    for r in recipients:
        files += ["-recip", w(f"{r}.pem")]
    for k in keyopts:
        files += ["-keyopt", k]
    return openssl("cms", "-encrypt", "-binary", "-outform", "DER", *args, *files, data=entity)


def outlook_entity(text, pdf_name):
    boundary = "_004_DB9PR01MB82341C0E5D2A4F8E9B1A2C3D4E5F6A7B8C9D0E1F2Aeurprd0_"
    alt = "_000_DB9PR01MB82341C0E5D2A4F8E9B1A2C3D4E5F6A7B8C9D0E1F2Aeurprd0_"
    pdf = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n"
    return crlf(
        f'Content-Type: multipart/mixed;\n\tboundary="{boundary}"\n\n'
        f"--{boundary}\n"
        f'Content-Type: multipart/alternative;\n\tboundary="{alt}"\n\n'
        f"--{alt}\n"
        'Content-Type: text/plain; charset="us-ascii"\n'
        "Content-Transfer-Encoding: quoted-printable\n\n"
        f"{text}\n\n"
        f"--{alt}\n"
        'Content-Type: text/html; charset="us-ascii"\n'
        "Content-Transfer-Encoding: quoted-printable\n\n"
        '<html><head><meta http-equiv=3D"Content-Type" content=3D"text/html; charset=3D=\n'
        'us-ascii"></head><body lang=3D"EN-US"><div class=3D"WordSection1"><p class=3D=\n'
        f'"MsoNormal">{text}<o:p></o:p></p></div></body></html>\n\n'
        f"--{alt}--\n\n"
        f"--{boundary}\n"
        f'Content-Type: application/pdf; name="{pdf_name}"\n'
        f'Content-Description: {pdf_name}\n'
        f'Content-Disposition: attachment; filename="{pdf_name}"; size={len(pdf)};\n'
        '\tcreation-date="Wed, 28 Oct 2026 08:25:00 GMT";\n'
        '\tmodification-date="Wed, 28 Oct 2026 08:25:00 GMT"\n'
        "Content-Transfer-Encoding: base64\n\n"
        + base64_lines(pdf).replace("\r\n", "\n")
        + f"\n--{boundary}--\n"
    ).encode()


def outlook_signed(w, signer, entity):
    boundary = "----=_NextPart_000_0012_01DB2A3C.5E6F7A80"
    return multipart_signed(entity, der_signature(w, signer, entity), boundary,
                            protocol="application/x-pkcs7-signature",
                            micalg="2.16.840.1.101.3.4.2.1",
                            preamble="This is a multipart message in MIME format.\r\n")


def outlook_headers(sender, to, subject, mid):
    return headers(sender, to, subject, mid, extra=(
        f"Thread-Topic: {subject}\n"
        "Thread-Index: AdtJ0x2vQmZk8TbqRqCw3Lw2Q4Yh5g==\n"
        "Accept-Language: en-US\nContent-Language: en-US\n"
        "X-MS-Has-Attach: yes\nX-MS-TNEF-Correlator:\n"
    ))


def main():
    work = tempfile.mkdtemp(prefix="envelope-smime-")
    w = Work(work)
    try:
        certify(w, "root", "/O=Envelope Test/CN=Envelope Test Root CA", "rsa", "root",
                not_after=ROOT_END)
        certify(w, "mail-ca", "/O=Envelope Test/CN=Envelope Test Mail CA", "rsa", "ca",
                issuer="root", not_after=ROOT_END)
        certify(w, "constrained-ca", "/O=Example Inc./CN=Example Inc. Mail CA", "rsa",
                "constrained", issuer="root", not_after=ROOT_END)
        certify(w, "other-root", "/O=Elsewhere/CN=Elsewhere Root CA", "rsa", "root",
                not_after=ROOT_END)

        certify(w, "alice", "/CN=Alice Example", "rsa", "rsa", "mail-ca",
                email="alice@example.com")
        certify(w, "bob", "/CN=Bob Example", "P-256", "ec", "mail-ca", email="bob@example.org")
        certify(w, "carol", "/CN=Carol Example", "P-384", "ec", "mail-ca",
                email="carol@example.net")
        certify(w, "dave", "/CN=Dave Example/emailAddress=dave@example.com", "rsa",
                "subject_only", "mail-ca", not_before="20200101000000Z",
                not_after="20231231235959Z")
        certify(w, "erin", "/CN=mail.example.com", "rsa", "server", "mail-ca",
                email="erin@example.com")
        certify(w, "mallory", "/CN=Mallory", "rsa", "rsa", "other-root",
                email="mallory@example.com")
        certify(w, "olga", "/CN=Olga Example", "rsa", "rsa", "constrained-ca",
                email="olga@example.com")
        certify(w, "oscar", "/CN=Oscar Example", "rsa", "rsa", "constrained-ca",
                email="oscar@example.net")
        for name in ("alice", "bob", "carol"):
            shutil.copy(w(f"{name}.key"), os.path.join(HERE, f"{name}.key"))

        openssl("pkcs12", "-export", "-in", w("alice.pem"), "-inkey", w("alice.key"),
                "-certfile", w("mail-ca.pem"), "-name", "Alice Example",
                "-passout", "pass:alice-pass", "-out", os.path.join(HERE, "alice.p12"))
        openssl("pkcs12", "-export", "-legacy", "-in", w("alice.pem"), "-inkey", w("alice.key"),
                "-certfile", w("mail-ca.pem"), "-name", "Alice Example",
                "-passout", "pass:alice-pass", "-out", os.path.join(HERE, "alice-legacy.p12"))
        openssl("pkcs12", "-export", "-in", w("bob.pem"), "-inkey", w("bob.key"),
                "-certfile", w("mail-ca.pem"), "-name", "Bob Example",
                "-passout", "pass:bob-pass", "-out", os.path.join(HERE, "bob.p12"))

        # Signed, by OpenSSL's S/MIME writer.
        write("signed-alice.eml", headers(ALICE, BOB, "Figures", "signed-alice@example.com").encode()
              + smime_sign(w, "alice", TEXT, "-md", "sha256", "-stream"))
        write("signed-bob.eml", headers(BOB, ALICE, "From Bob", "signed-bob@example.org").encode()
              + smime_sign(w, "bob", TEXT, "-md", "sha256"))
        write("signed-carol.eml", headers(CAROL, ALICE, "From Carol", "signed-carol@example.net")
              .encode() + smime_sign(w, "carol", TEXT, "-md", "sha384"))
        write("signed-pss.eml", headers(ALICE, BOB, "PSS", "signed-pss@example.com").encode()
              + smime_sign(w, "alice", TEXT, "-md", "sha256", "-keyopt",
                           "rsa_padding_mode:pss"))
        write("signed-sha1.eml", headers(ALICE, BOB, "Old mail", "signed-sha1@example.com").encode()
              + smime_sign(w, "alice", TEXT, "-md", "sha1"))
        write("opaque-alice.eml", headers(ALICE, BOB, "Opaque", "opaque-alice@example.com").encode()
              + smime_sign(w, "alice", TEXT, "-md", "sha256", "-nodetach", "-stream"))

        # Encrypted.
        signed_by_bob = smime_sign(w, "bob", TEXT.replace("Hello Bob", "Hello Alice"),
                                   "-md", "sha256", "-stream")
        streamed = openssl("cms", "-encrypt", "-aes-256-cbc", "-stream", "-crlfeol",
                           "-recip", w("alice.pem"), "-recip", w("bob.pem"), data=signed_by_bob)
        write("encrypted-alice.eml",
              headers(BOB, ALICE, "Secret figures", "encrypted-alice@example.org").encode()
              + streamed)
        opaque_by_alice = smime_sign(w, "alice", TEXT, "-md", "sha256", "-nodetach")
        write("encrypted-bob.eml",
              headers(ALICE, BOB, "For Bob", "encrypted-bob@example.com").encode()
              + pkcs7_mime(encrypt(w, opaque_by_alice, ["bob"], "-aes-128-cbc", "-aes256-wrap",
                                   keyopts=["ecdh_kdf_md:sha256"]), "enveloped-data"))
        write("encrypted-carol.eml",
              headers(ALICE, CAROL, "For Carol", "encrypted-carol@example.com").encode()
              + pkcs7_mime(encrypt(w, TEXT.encode(), ["carol"], "-aes-256-cbc"),
                           "enveloped-data", x=True))
        write("authenc-alice.eml",
              headers(BOB, ALICE, "GCM", "authenc-alice@example.org").encode()
              + pkcs7_mime(encrypt(w, TEXT.encode(), ["alice"], "-aes-256-gcm"),
                           "authEnveloped-data"))
        write("des3-alice.eml",
              headers(BOB, ALICE, "Old secret", "des3-alice@example.org").encode()
              + pkcs7_mime(encrypt(w, TEXT.encode(), ["alice"], "-des3"), "enveloped-data"))
        oaep = encrypt(w, TEXT.encode(), ["alice"], "-aes-256-cbc",
                       keyopts=["rsa_padding_mode:oaep", "rsa_oaep_md:sha256"])
        write("oaep-alice.eml",
              headers(BOB, ALICE, "OAEP", "oaep-alice@example.org").encode()
              + pkcs7_mime(oaep, "enveloped-data"))

        # Outlook.
        entity = outlook_entity("Hi Bob, the signed report is attached. Alice", "report.pdf")
        write("outlook-signed.eml",
              outlook_headers(ALICE, BOB, "Signed report", "DB9PR01MB8234A@eurprd01.example.com")
              .encode() + outlook_signed(w, "alice", entity))
        inner = outlook_signed(w, "bob", outlook_entity(
            "Hi Alice, the encrypted report is attached. Bob", "secret.pdf"))
        write("outlook-encrypted.eml",
              outlook_headers(BOB, ALICE, "Encrypted report", "DB9PR01MB8234B@eurprd01.example.org")
              .encode() + pkcs7_mime(encrypt(w, inner, ["alice", "bob"], "-aes-256-cbc"),
                                     "enveloped-data"))

        # Checks that fail.
        signed = read("signed-alice.eml")
        assert b"the figures are below" in signed
        write("tampered.eml", signed.replace(b"the figures are below", b"the figures are ABOVE"))
        entity = TEXT.encode()
        damaged = bytearray(der_signature(w, "alice", entity))
        damaged[-1] ^= 0x01
        write("bad-signature.eml",
              headers(ALICE, BOB, "Damaged", "bad-signature@example.com").encode()
              + multipart_signed(entity, bytes(damaged), "damaged-7f3a"))
        write("expired.eml", headers("Dave Example <dave@example.com>", BOB, "Expired",
                                     "expired@example.com").encode()
              + smime_sign(w, "dave", TEXT, "-md", "sha256"))
        write("untrusted.eml", headers("Mallory <mallory@example.com>", BOB, "Trust me",
                                       "untrusted@example.com").encode()
              + smime_sign(w, "mallory", TEXT, "-md", "sha256", chain=None))
        write("mismatch.eml", headers("Eve <eve@example.com>", BOB, "Not Alice",
                                      "mismatch@example.com").encode()
              + smime_sign(w, "alice", TEXT, "-md", "sha256"))
        write("not-for-email.eml", headers("Erin <erin@example.com>", BOB, "Server",
                                           "not-for-email@example.com").encode()
              + smime_sign(w, "erin", TEXT, "-md", "sha256"))
        write("no-certs.eml", headers(ALICE, BOB, "No certificate", "no-certs@example.com")
              .encode() + smime_sign(w, "alice", TEXT, "-md", "sha256", "-nocerts", chain=None))
        write("constrained-ok.eml", headers("Olga Example <olga@example.com>", BOB, "Constrained",
                                            "constrained-ok@example.com").encode()
              + smime_sign(w, "olga", TEXT, "-md", "sha256", chain="constrained-ca"))
        write("constrained-bad.eml", headers("Oscar Example <oscar@example.net>", BOB,
                                             "Constrained", "constrained-bad@example.net")
              .encode() + smime_sign(w, "oscar", TEXT, "-md", "sha256", chain="constrained-ca"))
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()

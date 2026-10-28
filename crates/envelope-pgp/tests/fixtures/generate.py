#!/usr/bin/env python3
"""Regenerates the OpenPGP interoperability fixtures with GnuPG.

    python3 crates/envelope-pgp/tests/fixtures/generate.py

Creates two keys in a throwaway GNUPGHOME — Gina (Ed25519/Cv25519, no
passphrase) and Rita (RSA 3072, passphrase "rita-pass") — and messages shaped
like the ones Thunderbird, Enigmail, Mutt and plain GnuPG users send:

- tb-encrypted-signed.eml   PGP/MIME, signed and encrypted in one OpenPGP
                            message, protected headers with a legacy display
                            part, an attachment (Thunderbird / Enigmail)
- enigmail-signed.eml       multipart/signed with a SHA-512 detached signature
- nested-signed-encrypted.eml  multipart/signed inside multipart/encrypted
- hidden-recipient.eml      PGP/MIME to an anonymous (hidden) recipient
- inline-encrypted.eml      an armored PGP MESSAGE in a text/plain body
- clearsigned.eml           a cleartext signed text/plain body
"""
import os
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
GINA = "gina@gnupg.example"
RITA = "rita@rsa.example"


def gpg(home, *args, data=None):
    cmd = ["gpg", "--homedir", home, "--batch", "--yes", "--quiet",
           "--pinentry-mode", "loopback", "--trust-model", "always", *args]
    return subprocess.run(cmd, input=data, check=True, capture_output=True).stdout


def crlf(text):
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


def write(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode())


def encrypted_message(headers, armored):
    boundary = "------------gnupgEnc5C1C0FA3D6E0"
    return crlf(
        headers
        + 'MIME-Version: 1.0\n'
        + 'Content-Type: multipart/encrypted;\n protocol="application/pgp-encrypted";\n'
        + f' boundary="{boundary}"\n\n'
        + "This is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\n"
        + f"--{boundary}\n"
        + "Content-Type: application/pgp-encrypted\n"
        + "Content-Description: PGP/MIME version identification\n\n"
        + "Version: 1\n\n"
        + f"--{boundary}\n"
        + 'Content-Type: application/octet-stream; name="encrypted.asc"\n'
        + "Content-Description: OpenPGP encrypted message\n"
        + 'Content-Disposition: inline; filename="encrypted.asc"\n\n'
        + armored.strip() + "\n\n"
        + f"--{boundary}--\n"
    ).encode()


def main():
    home = tempfile.mkdtemp(prefix="envelope-gnupg-")
    try:
        gpg(home, "--gen-key", data=b"""%no-protection
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign,cert
Subkey-Type: ecdh
Subkey-Curve: cv25519
Subkey-Usage: encrypt
Name-Real: Gina GnuPG
Name-Email: gina@gnupg.example
Expire-Date: 0
%commit
""")
        gpg(home, "--gen-key", data=b"""Key-Type: RSA
Key-Length: 3072
Key-Usage: sign,cert
Subkey-Type: RSA
Subkey-Length: 3072
Subkey-Usage: encrypt
Name-Real: Rita RSA
Name-Email: rita@rsa.example
Passphrase: rita-pass
Expire-Date: 0
%commit
""")
        write("gina.pub.asc", gpg(home, "--armor", "--export", GINA))
        write("gina.sec.asc", gpg(home, "--armor", "--export-secret-keys", GINA))
        write("rita.pub.asc", gpg(home, "--armor", "--export", RITA))
        write("rita.sec.asc", gpg(home, "--passphrase", "rita-pass", "--armor",
                                  "--export-secret-keys", RITA))

        envelope = (
            "From: Gina GnuPG <gina@gnupg.example>\n"
            "To: Rita RSA <rita@rsa.example>\n"
            "Date: Wed, 28 Oct 2026 09:30:00 +0100\n"
        )

        # Thunderbird: one OpenPGP message that is signed and encrypted,
        # protected headers, a legacy display part and an attachment.
        inner = crlf(
            'Content-Type: multipart/mixed; boundary="------------tbMixed7A1C";\n'
            ' protected-headers="v1"\n'
            "From: Gina GnuPG <gina@gnupg.example>\n"
            "To: Rita RSA <rita@rsa.example>\n"
            "Message-ID: <tb-1@gnupg.example>\n"
            "Subject: Quarterly numbers ☕\n\n"
            "--------------tbMixed7A1C\n"
            'Content-Type: text/rfc822-headers; protected-headers="v1"\n'
            "Content-Disposition: inline\n\n"
            "Subject: Quarterly numbers\n\n"
            "--------------tbMixed7A1C\n"
            "Content-Type: text/plain; charset=UTF-8; format=flowed\n"
            "Content-Transfer-Encoding: 8bit\n\n"
            "Hi Rita,\n\nthe numbers are attached. Görüşürüz!\n\n"
            "--------------tbMixed7A1C\n"
            'Content-Type: text/plain; charset=UTF-8; name="numbers.txt"\n'
            'Content-Disposition: attachment; filename="numbers.txt"\n'
            "Content-Transfer-Encoding: base64\n\n"
            "UTE6IDQyClEyOiA0Nwo=\n\n"
            "--------------tbMixed7A1C--\n"
        ).encode()
        armored = gpg(home, "--armor", "--local-user", GINA, "--recipient", RITA,
                      "--recipient", GINA, "--sign", "--encrypt", data=inner).decode()
        write("tb-encrypted-signed.eml", encrypted_message(
            envelope + "Subject: ...\nMessage-ID: <tb-1@gnupg.example>\n", armored))

        # Enigmail / Mutt: multipart/signed over a multipart/mixed part.
        signed_part = crlf(
            'Content-Type: multipart/mixed; boundary="enigSig0123"\n\n'
            "--enigSig0123\n"
            "Content-Type: text/plain; charset=utf-8\n"
            "Content-Transfer-Encoding: quoted-printable\n\n"
            "Signed with GnuPG. Merhaba d=C3=BCnya!\n"
            "-- \n"
            "Gina\n\n"
            "--enigSig0123--\n"
        ).encode()
        signature = gpg(home, "--armor", "--local-user", GINA, "--digest-algo", "SHA512",
                        "--detach-sign", data=signed_part).decode()
        boundary = "enigSigned9F3E"
        write("enigmail-signed.eml", crlf(
            envelope
            + "Subject: Signed hello\nMessage-ID: <sig-1@gnupg.example>\nMIME-Version: 1.0\n"
            + 'Content-Type: multipart/signed; micalg=pgp-sha512;\n'
            + f' protocol="application/pgp-signature";\n boundary="{boundary}"\n\n'
            + "This is an OpenPGP/MIME signed message (RFC 4880 and 3156)\n"
            + f"--{boundary}\n"
        ).encode() + signed_part + crlf(
            f"\n--{boundary}\n"
            + 'Content-Type: application/pgp-signature; name="signature.asc"\n'
            + "Content-Description: OpenPGP digital signature\n"
            + 'Content-Disposition: attachment; filename="signature.asc"\n\n'
            + signature.strip() + "\n"
            + f"--{boundary}--\n"
        ).encode())

        # Sign, then encrypt the signed entity (nested PGP/MIME).
        nested_part = crlf(
            "Content-Type: text/plain; charset=us-ascii\n\n"
            "Nested: signed first, then encrypted.\n"
        ).encode()
        nested_sig = gpg(home, "--armor", "--local-user", GINA, "--detach-sign",
                         data=nested_part).decode()
        nested = crlf(
            'Content-Type: multipart/signed; micalg=pgp-sha256;\n'
            ' protocol="application/pgp-signature"; boundary="nestedSig42"\n\n'
            "--nestedSig42\n"
        ).encode() + nested_part + crlf(
            "\n--nestedSig42\n"
            "Content-Type: application/pgp-signature\n\n"
            + nested_sig.strip() + "\n"
            "--nestedSig42--\n"
        ).encode()
        armored = gpg(home, "--armor", "--recipient", RITA, "--encrypt", data=nested).decode()
        write("nested-signed-encrypted.eml", encrypted_message(
            envelope + "Subject: Nested\nMessage-ID: <nested-1@gnupg.example>\n", armored))

        # A hidden (anonymous) recipient, as for Bcc.
        hidden = crlf("Content-Type: text/plain; charset=utf-8\n\nFor your eyes only.\n").encode()
        armored = gpg(home, "--armor", "--hidden-recipient", RITA, "--encrypt",
                      data=hidden).decode()
        write("hidden-recipient.eml", encrypted_message(
            envelope + "Subject: Hidden\nMessage-ID: <hidden-1@gnupg.example>\n", armored))

        # Inline PGP: an armored message in a plain text body.
        armored = gpg(home, "--armor", "--local-user", GINA, "--recipient", RITA,
                      "--sign", "--encrypt",
                      data="Inline secret: şifre 1234\n".encode()).decode()
        write("inline-encrypted.eml", crlf(
            envelope + "Subject: Inline\nMessage-ID: <inline-1@gnupg.example>\nMIME-Version: 1.0\n"
            + "Content-Type: text/plain; charset=us-ascii\nContent-Transfer-Encoding: 7bit\n\n"
            + armored
        ).encode())

        # A cleartext signature, with a line that needs dash-escaping.
        clear = gpg(home, "--local-user", GINA, "--clearsign",
                    data="Clear and signed.\n- a list item\nTschüss\n".encode()).decode()
        write("clearsigned.eml", crlf(
            envelope + "Subject: Clearsigned\nMessage-ID: <clear-1@gnupg.example>\nMIME-Version: 1.0\n"
            + "Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: 8bit\n\n"
            + clear
        ).encode())
    finally:
        subprocess.run(["gpgconf", "--homedir", home, "--kill", "gpg-agent"], check=False)
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()

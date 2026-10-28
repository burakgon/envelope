#!/usr/bin/env python3
"""Development SMTP submission server.

Accepts any credentials and delivers each message into the Maildir of every
recipient that exists in the local Dovecot users file, so mail sent from
Envelope shows up in the test accounts.
"""
import asyncio
import os
import socket
import sys
import time
import uuid

from aiosmtpd.controller import Controller
from aiosmtpd.smtp import AuthResult

ROOT = sys.argv[1]
PORT = int(sys.argv[2])


def known_users():
    with open(os.path.join(ROOT, "users")) as f:
        return {line.split(":", 1)[0].lower() for line in f if ":" in line}


def deliver(user: str, data: bytes) -> None:
    maildir = os.path.join(ROOT, "home", user, "Maildir")
    for sub in ("tmp", "new", "cur"):
        os.makedirs(os.path.join(maildir, sub), exist_ok=True)
    name = f"{time.time_ns()}.{uuid.uuid4().hex}.{socket.gethostname()}"
    tmp = os.path.join(maildir, "tmp", name)
    with open(tmp, "wb") as f:
        f.write(data)
    os.rename(tmp, os.path.join(maildir, "new", name))


class Handler:
    async def handle_DATA(self, server, session, envelope):
        data = envelope.original_content or envelope.content
        if isinstance(data, str):
            data = data.encode()
        users = known_users()
        delivered = [r for r in envelope.rcpt_tos if r.lower() in users]
        for rcpt in delivered:
            deliver(rcpt.lower(), data)
        print(
            f"smtp: {envelope.mail_from} -> {envelope.rcpt_tos} "
            f"({len(data)} bytes, delivered locally to {len(delivered)})",
            flush=True,
        )
        return "250 Message accepted for delivery"


def authenticator(server, session, envelope, mechanism, auth_data):
    return AuthResult(success=True)


def main() -> None:
    controller = Controller(
        Handler(),
        hostname="127.0.0.1",
        port=PORT,
        authenticator=authenticator,
        auth_require_tls=False,
        auth_required=False,
    )
    controller.start()
    print(f"smtp: listening on 127.0.0.1:{PORT}", flush=True)
    try:
        asyncio.new_event_loop().run_forever()
    except KeyboardInterrupt:
        controller.stop()


if __name__ == "__main__":
    main()

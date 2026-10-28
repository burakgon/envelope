#!/usr/bin/env python3
"""Fills a Maildir with a large, realistic mailbox for performance testing.

    python3 scripts/devserver/seed_large.py .devserver/home/sam@envelope.test/Maildir 50000

Messages come in conversations (with In-Reply-To/References), from a few
hundred senders over three years, as plain text or HTML, some with small
attachments. The output is deterministic for a given count.
"""

import os
import random
import sys
import time
from email.utils import format_datetime
from datetime import datetime, timedelta, timezone

FIRST = "Alex Sam Priya Daniel Sofia Maya Olivia Noah Elif Kenji Lukas Hannah Omar Lena Ivan Chloe Mateo Aisha Tom Yuki Carlos Nora Felix Zoe Arjun Mia Leo Sara Ben Ana".split()
LAST = "Morgan Raman Okafor Martinez Chen Brooks Williams Yilmaz Tanaka Weber Becker Haddad Novak Petrov Dubois Garcia Khan Smith Sato Lopez Berg Schmidt Rossi Silva Kim Nguyen Patel Cohen Muller Ozturk".split()
DOMAINS = "lumen.dev northwind.design acme.io globex.com initech.net hooli.xyz umbrella.org stark.co wayne.biz example.org".split()
TOPICS = "roadmap invoice offsite launch budget hiring design review release onboarding contract renewal migration incident postmortem newsletter sprint planning quarterly report partnership webinar feedback security audit pricing proposal".split()
WORDS = ("the a we should next week update please review attached draft notes meeting call schedule thanks quick question "
         "about project team customer timeline numbers plan agree issue fix deploy server data design copy final version "
         "today tomorrow friday morning afternoon let me know if works for you happy to help").split()


def sentence(rng, n=None):
    n = n or rng.randint(6, 16)
    words = [rng.choice(WORDS) for _ in range(n)]
    return " ".join(words).capitalize() + "."


def body_text(rng, name):
    paras = ["\n".join(sentence(rng) for _ in range(rng.randint(1, 4))) for _ in range(rng.randint(1, 4))]
    return f"Hi {name},\n\n" + "\n\n".join(paras) + "\n\nBest,\n"


def main():
    maildir = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 50000
    rng = random.Random(42)
    for sub in ("cur", "new", "tmp"):
        os.makedirs(os.path.join(maildir, sub), exist_ok=True)
    people = [(f"{rng.choice(FIRST)} {rng.choice(LAST)}", None) for _ in range(400)]
    people = [(n, f"{n.split()[0].lower()}.{n.split()[1].lower()}@{rng.choice(DOMAINS)}") for n, _ in people]
    me = ("Sam Rivera", "sam@envelope.test")
    now = datetime.now(timezone.utc)
    start = now - timedelta(days=3 * 365)
    written = 0
    thread_no = 0
    t0 = time.time()
    while written < count:
        thread_no += 1
        topic = rng.choice(TOPICS)
        subject = f"{topic.capitalize()} {rng.choice(['update', 'question', 'for Q' + str(rng.randint(1, 4)), 'notes', 'next steps', 'follow-up', 'draft', '#' + str(rng.randint(100, 999))])}"
        size = min(count - written, rng.choice([1, 1, 1, 1, 2, 2, 3, 4, 6, 9]))
        participants = rng.sample(people, k=min(len(people), rng.randint(1, 3)))
        when = start + timedelta(seconds=rng.randint(0, int((now - start).total_seconds()) - 86400))
        ids = []
        for i in range(size):
            sender = rng.choice(participants + [me]) if i else participants[0]
            to = me if sender != me else participants[0]
            when += timedelta(minutes=rng.randint(5, 60 * 30))
            if when > now:
                when = now - timedelta(minutes=rng.randint(1, 600))
            mid = f"<bench.{thread_no}.{i}@{sender[1].split('@')[1]}>"
            headers = [
                f"From: {sender[0]} <{sender[1]}>",
                f"To: {to[0]} <{to[1]}>",
                f"Subject: {'Re: ' if i else ''}{subject}",
                f"Date: {format_datetime(when)}",
                f"Message-ID: {mid}",
                "MIME-Version: 1.0",
            ]
            if ids:
                headers.append(f"In-Reply-To: {ids[-1]}")
                headers.append(f"References: {' '.join(ids[-8:])}")
            text = body_text(rng, to[0].split()[0]) + sender[0].split()[0] + "\n"
            kind = rng.random()
            if kind < 0.55:
                headers.append("Content-Type: text/plain; charset=utf-8")
                raw = "\r\n".join(headers) + "\r\n\r\n" + text.replace("\n", "\r\n")
            else:
                boundary = f"b{thread_no}x{i}"
                html = "".join(f"<p>{p}</p>" for p in text.split("\n\n"))
                parts = [
                    f"--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}",
                    f"--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body style=\"font-family:sans-serif\">{html}</body></html>",
                ]
                mixed = kind > 0.93
                if mixed:
                    inner = f"a{boundary}"
                    alt = f"Content-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n" + "\r\n".join(p.replace(boundary, inner) for p in parts) + f"\r\n--{inner}--"
                    payload = "".join(rng.choice("ABCDEFGHIJKLMNOPQRSTUVWXYZ") for _ in range(2400))
                    att = (f"--{boundary}\r\nContent-Type: application/pdf; name=\"{topic}-{thread_no}.pdf\"\r\n"
                           f"Content-Disposition: attachment; filename=\"{topic}-{thread_no}.pdf\"\r\n"
                           f"Content-Transfer-Encoding: base64\r\n\r\n" + "\r\n".join(payload[j:j + 76] for j in range(0, len(payload), 76)))
                    headers.append(f"Content-Type: multipart/mixed; boundary=\"{boundary}\"")
                    raw = "\r\n".join(headers) + f"\r\n\r\n--{boundary}\r\n{alt}\r\n{att}\r\n--{boundary}--\r\n"
                else:
                    headers.append(f"Content-Type: multipart/alternative; boundary=\"{boundary}\"")
                    raw = "\r\n".join(headers) + "\r\n\r\n" + "\r\n".join(parts) + f"\r\n--{boundary}--\r\n"
            ids.append(mid)
            data = raw.encode("utf-8")
            seen = "S" if when < now - timedelta(days=2) or rng.random() < 0.6 else ""
            name = f"{int(when.timestamp())}.M{written}P1.bench,S={len(data)}:2,{seen}"
            with open(os.path.join(maildir, "cur", name), "wb") as f:
                f.write(data)
            os.utime(os.path.join(maildir, "cur", name), (when.timestamp(), when.timestamp()))
            written += 1
    print(f"wrote {written} messages in {thread_no} conversations in {time.time() - t0:.1f}s")


if __name__ == "__main__":
    main()

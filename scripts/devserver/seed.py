#!/usr/bin/env python3
"""Seeds a Maildir with realistic, varied mail for developing Envelope.

Usage: seed.py <maildir> <owner-address>

Everything here is fictional: people, companies and products are made up.
"""
import os
import random
import socket
import struct
import sys
import time
import uuid
import zlib
from datetime import datetime, timedelta, timezone
from email.message import EmailMessage
from email.utils import format_datetime, make_msgid

MAILDIR = sys.argv[1]
ME = sys.argv[2] if len(sys.argv) > 2 else "alex@envelope.test"
ME_NAME = "Alex Morgan"
NOW = datetime.now(timezone.utc).replace(second=0, microsecond=0)
random.seed(7)

PEOPLE = {
    "maya": ("Maya Chen", "maya@northwind.design"),
    "daniel": ("Daniel Okafor", "daniel@lumen.dev"),
    "priya": ("Priya Raman", "priya@lumen.dev"),
    "lukas": ("Lukas Weber", "lukas.weber@kraftwerk-studio.de"),
    "elif": ("Elif Yılmaz", "elif@yilmaz.family"),
    "sofia": ("Sofia Martínez", "sofia@lumen.dev"),
    "kenji": ("Kenji Tanaka", "kenji@tanaka.jp"),
    "noah": ("Noah Williams", "noah.w@fastmail.test"),
    "olivia": ("Olivia Brooks", "olivia@lumen.dev"),
    "sam": ("Sam Rivera", "sam@envelope.test"),
}


def addr(key):
    name, email = PEOPLE[key]
    return f"{name} <{email}>"


def png(width=480, height=270, c1=(99, 102, 241), c2=(236, 72, 153)):
    rows = []
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            t = (x / width + y / height) / 2
            row += bytes(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 9)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", raw)
        + chunk(b"IEND", b"")
    )


def pdf(title):
    text = f"BT /F1 24 Tf 72 720 Td ({title}) Tj ET".encode()
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        b"<< /Length " + str(len(text)).encode() + b" >>\nstream\n" + text + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + o + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


def ago(days=0, hours=0, minutes=0):
    return NOW - timedelta(days=days, hours=hours, minutes=minutes)


def html_wrap(body, accent="#4f46e5"):
    return f"""<!doctype html><html><body style="margin:0;background:#f4f4f7;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#1f2330">
<table width="100%" cellpadding="0" cellspacing="0" style="background:#f4f4f7;padding:24px 0"><tr><td align="center">
<table width="600" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:12px;overflow:hidden">
<tr><td style="height:6px;background:{accent}"></td></tr>
<tr><td style="padding:32px 40px;font-size:15px;line-height:1.6">{body}</td></tr>
</table></td></tr></table></body></html>"""


class Seeder:
    def __init__(self, root):
        self.root = root
        self.count = 0

    def folder_path(self, folder):
        if folder in ("INBOX", ""):
            return self.root
        return os.path.join(self.root, "." + folder.replace("/", "."))

    def store(self, msg, folder="INBOX", seen=True, flagged=False, answered=False, draft=False):
        path = self.folder_path(folder)
        for sub in ("cur", "new", "tmp"):
            os.makedirs(os.path.join(path, sub), exist_ok=True)
        if folder not in ("INBOX", "") and not os.path.exists(os.path.join(path, "maildirfolder")):
            open(os.path.join(path, "maildirfolder"), "w").close()
        flags = "".join(
            f for f, on in (("D", draft), ("F", flagged), ("R", answered), ("S", seen)) if on
        )
        self.count += 1
        name = f"{time.time_ns()}.{self.count}.{socket.gethostname()}:2,{flags}"
        data = bytes(msg)
        with open(os.path.join(path, "cur", name), "wb") as f:
            f.write(data)
        # Maildir uses the file mtime as the internal date.
        when = msg["Date"].datetime.timestamp() if hasattr(msg["Date"], "datetime") else time.time()
        os.utime(os.path.join(path, "cur", name), (when, when))

    def mail(self, frm, to, subject, when, text=None, html=None, cc=None, reply_to=None,
             in_reply_to=None, references=None, headers=None, attachments=(), inline=(),
             msgid=None):
        msg = EmailMessage()
        msg["From"] = frm
        msg["To"] = to if isinstance(to, str) else ", ".join(to)
        if cc:
            msg["Cc"] = cc if isinstance(cc, str) else ", ".join(cc)
        if reply_to:
            msg["Reply-To"] = reply_to
        msg["Subject"] = subject
        msg["Date"] = format_datetime(when)
        msg["Message-ID"] = msgid or make_msgid(domain=frm.split("@")[-1].rstrip(">"))
        if in_reply_to:
            msg["In-Reply-To"] = in_reply_to
            msg["References"] = references or in_reply_to
        for k, v in (headers or {}).items():
            msg[k] = v
        msg.set_content(text or "This message is best viewed in an HTML-capable client.")
        if html:
            msg.add_alternative(html, subtype="html")
            for cid, data, mime in inline:
                maintype, subtype = mime.split("/")
                msg.get_payload()[1].add_related(data, maintype=maintype, subtype=subtype, cid=f"<{cid}>")
        for filename, data, mime in attachments:
            maintype, subtype = mime.split("/")
            msg.add_attachment(data, maintype=maintype, subtype=subtype, filename=filename)
        return msg


def thread(seeder, folder_for, messages, subject):
    """messages: list of (sender_key_or_me, recipients, when, text, extra kwargs)."""
    ids = []
    for i, (who, rcpts, when, text, extra) in enumerate(messages):
        frm = f"{ME_NAME} <{ME}>" if who == "me" else addr(who)
        to = [f"{ME_NAME} <{ME}>" if r == "me" else addr(r) for r in rcpts]
        subj = subject if i == 0 else "Re: " + subject
        msg = seeder.mail(
            frm, to, subj, when, text=text,
            in_reply_to=ids[-1] if ids else None,
            references=" ".join(ids) if ids else None,
            **extra,
        )
        ids.append(msg["Message-ID"])
        folder, flags = folder_for(who, i, len(messages))
        seeder.store(msg, folder, **flags)
    return ids


def main():
    s = Seeder(MAILDIR)

    def inbox_or_sent(who, i, n):
        if who == "me":
            return "Sent", {"seen": True}
        return "INBOX", {"seen": i < n - 1, "answered": i < n - 1}

    # 1. Roadmap thread with PDF attachment.
    thread(s, inbox_or_sent, [
        ("priya", ["me", "daniel"], ago(2, 5), "Hi both,\n\nAttached is the draft Q4 roadmap. The big bets are offline search, the new composer and calendar sharing.\n\nCan we lock priorities by Thursday?\n\nPriya", {"attachments": [("Q4-Roadmap-Draft.pdf", pdf("Q4 Roadmap Draft"), "application/pdf")]}),
        ("daniel", ["priya", "me"], ago(2, 2), "Looks solid. I'd move calendar sharing to Q1 - the sync rewrite has to land first.\n\nDaniel", {}),
        ("me", ["priya", "daniel"], ago(1, 20), "Agree with Daniel. Offline search is the one users keep asking for, let's make it the headline.\n\nAlex", {}),
        ("priya", ["me", "daniel"], ago(0, 3), "Great - updated the doc. Final review tomorrow at 10:00, I'll send an invite.\n\nPriya", {}),
    ], "Q4 roadmap - draft for review")

    # 2. Design review with inline image.
    img = png()
    s.store(s.mail(
        addr("maya"), f"{ME_NAME} <{ME}>", "New onboarding flow - first look", ago(0, 1, 12),
        text="Hey Alex! Here's the first pass at the new onboarding. Let me know what you think before Friday.",
        html=html_wrap("""<p>Hey Alex!</p><p>Here's the first pass at the new onboarding. I simplified the account step to a single screen and added a provider picker with auto-detection.</p>
<p><img src="cid:onboarding-preview" width="520" style="border-radius:8px;display:block;max-width:100%" alt="Onboarding preview"></p>
<p>Main open questions:</p><ul><li>Do we show the AI step during onboarding or later?</li><li>Dark mode default?</li></ul>
<p>Let me know what you think before Friday 🙌</p><p>— Maya</p>""", "#ec4899"),
        inline=[("onboarding-preview", img, "image/png")],
    ), seen=False, flagged=True)

    # 3. Calendar invitation.
    start = (NOW + timedelta(days=1)).replace(hour=10, minute=0)
    ics = f"""BEGIN:VCALENDAR
PRODID:-//Lumen//Calendar//EN
VERSION:2.0
METHOD:REQUEST
BEGIN:VEVENT
UID:{uuid.uuid4()}@lumen.dev
DTSTAMP:{NOW.strftime('%Y%m%dT%H%M%SZ')}
DTSTART:{start.strftime('%Y%m%dT%H%M%SZ')}
DTEND:{(start + timedelta(minutes=45)).strftime('%Y%m%dT%H%M%SZ')}
SUMMARY:Roadmap final review
LOCATION:https://meet.lumen.dev/roadmap-review
ORGANIZER;CN=Priya Raman:mailto:priya@lumen.dev
ATTENDEE;CN=Alex Morgan;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{ME}
ATTENDEE;CN=Daniel Okafor;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:daniel@lumen.dev
DESCRIPTION:Final pass on the Q4 roadmap before we share it with the team.
SEQUENCE:0
STATUS:CONFIRMED
END:VEVENT
END:VCALENDAR
"""
    invite = s.mail(addr("priya"), f"{ME_NAME} <{ME}>", "Invitation: Roadmap final review", ago(0, 2, 40),
                    text="Priya Raman invited you to Roadmap final review.")
    invite.add_attachment(ics.encode(), maintype="text", subtype="calendar", filename="invite.ics",
                          params={"method": "REQUEST"})
    s.store(invite, seen=False)

    # 4. Flight confirmation.
    s.store(s.mail(
        "Skyway Airlines <bookings@skyway-air.test>", f"{ME_NAME} <{ME}>",
        "Your trip to Berlin is confirmed — booking SKY7Q2", ago(3, 4),
        text="Booking SKY7Q2\nIstanbul (IST) → Berlin (BER)\nDeparts 14 Nov 09:40, arrives 11:55\nSeat 12A",
        html=html_wrap("""<h2 style="margin:0 0 8px">You're all set, Alex ✈️</h2><p style="color:#6b7080;margin:0 0 24px">Booking reference <b>SKY7Q2</b></p>
<table width="100%" style="border-collapse:collapse;font-size:15px"><tr><td style="padding:12px 0;border-bottom:1px solid #eee"><b>IST → BER</b><br><span style="color:#6b7080">Thu, 14 Nov · 09:40 – 11:55</span></td><td align="right" style="border-bottom:1px solid #eee">Seat <b>12A</b></td></tr>
<tr><td style="padding:12px 0"><b>BER → IST</b><br><span style="color:#6b7080">Sun, 17 Nov · 18:20 – 22:30</span></td><td align="right">Seat <b>14C</b></td></tr></table>
<p style="margin-top:24px"><a href="https://skyway-air.test/manage/SKY7Q2" style="background:#0ea5e9;color:#fff;padding:12px 20px;border-radius:8px;text-decoration:none">Manage booking</a></p>""", "#0ea5e9"),
    ), seen=True)

    # 5. Newsletters with List-Unsubscribe.
    for i, (title, lead) in enumerate([
        ("The Daily Byte: Rust in the kernel, Wayland everywhere", "Today: why distro maintainers are excited about the new scheduler, and a deep dive on fractional scaling."),
        ("The Daily Byte: The quiet comeback of desktop apps", "Native is back. We look at the tools making fast desktop software fun again."),
        ("The Daily Byte: SQLite is all you need?", "Local-first apps are having a moment. Here is what changed."),
        ("The Daily Byte: Open source funding, explained", "Who pays for the software everyone uses? A practical guide."),
    ]):
        s.store(s.mail(
            "The Daily Byte <hello@dailybyte.test>", f"{ME_NAME} <{ME}>", title, ago(i * 2 + 1, 7),
            text=lead,
            html=html_wrap(f"<h1 style='font-size:24px;margin:0 0 12px'>{title.split(': ', 1)[1]}</h1><p>{lead}</p><p><a href='https://dailybyte.test/read'>Read the full issue →</a></p><p style='color:#999;font-size:12px'>You are receiving this because you subscribed. <a href='https://dailybyte.test/unsubscribe'>Unsubscribe</a></p>", "#f59e0b"),
            headers={"List-Unsubscribe": "<https://dailybyte.test/unsubscribe?u=alex>, <mailto:unsubscribe@dailybyte.test>",
                     "List-Unsubscribe-Post": "List-Unsubscribe=One-Click", "List-Id": "Daily Byte <daily.dailybyte.test>"},
        ), seen=i > 0)

    # 6. Code forge notifications.
    for i, (num, title, actor, body) in enumerate([
        (142, "Fix IDLE reconnect after suspend", "daniel", "Merged #142 into main."),
        (139, "Calendar: drag to resize events", "sofia", "Sofia requested your review on this pull request."),
        (137, "Search operators: support larger: and smaller:", "olivia", "Olivia commented: Looks good, one nit on the parser."),
        (131, "Crash when opening message with empty boundary", "noah", "Noah opened this issue."),
    ]):
        s.store(s.mail(
            f"{PEOPLE[actor][0]} <notifications@gitforge.test>", f"lumen/envelope <envelope@noreply.gitforge.test>",
            f"[lumen/envelope] {title} (#{num})", ago(i, 9 + i),
            text=f"{body}\n\n— Reply to this email directly or view it on Gitforge:\nhttps://gitforge.test/lumen/envelope/pull/{num}",
            headers={"List-Id": "lumen/envelope <envelope.lumen.gitforge.test>", "X-Gitforge-Reason": "review_requested"},
        ), seen=i > 1)

    # 7. Receipt.
    s.store(s.mail(
        "Paystream <receipts@paystream.test>", f"{ME_NAME} <{ME}>", "Your receipt from Lumen Cloud #1043-2201", ago(4, 1),
        text="Amount paid: $24.00\nLumen Cloud Pro (monthly)",
        html=html_wrap("<p style='color:#6b7080'>Receipt #1043-2201</p><h1 style='margin:0'>$24.00</h1><p>Lumen Cloud Pro · monthly</p><p style='color:#6b7080;font-size:13px'>Paid with Visa •••• 4242</p>", "#10b981"),
    ))

    # 8. Turkish friend.
    thread(s, inbox_or_sent, [
        ("elif", ["me"], ago(1, 6), "Selam Alex!\n\nBu hafta sonu Kadıköy'de yeni açılan kahveciye gidelim mi? Cumartesi 11 gibi uygun musun?\n\nSevgiler,\nElif", {}),
        ("me", ["elif"], ago(1, 4), "Selam Elif, harika fikir! Cumartesi 11 bana uyar. Görüşürüz ☕", {}),
        ("elif", ["me"], ago(0, 5), "Süper! Adresi atıyorum: Moda Cd. No: 12. Rezervasyon yaptım 🙂", {}),
    ], "Hafta sonu planı?")

    # 9. German client with offer PDF.
    s.store(s.mail(
        addr("lukas"), f"{ME_NAME} <{ME}>", "Angebot für das neue Projekt", ago(5, 3),
        text="Hallo Alex,\n\nanbei unser Angebot für den Relaunch. Wir könnten Anfang Dezember starten.\n\nViele Grüße\nLukas Weber\nKraftwerk Studio",
        attachments=[("Angebot-Relaunch-2026.pdf", pdf("Angebot Relaunch"), "application/pdf")],
    ), flagged=True)

    # 10. Japanese message.
    s.store(s.mail(
        addr("kenji"), f"{ME_NAME} <{ME}>", "東京オフィスの件について", ago(6, 2),
        text="アレックスさん\n\n来月の東京出張の件、ありがとうございます。オフィスは渋谷の新しいビルです。\n会議室を予約しておきます。\n\n田中",
    ))

    # 11. Security alert.
    s.store(s.mail(
        "Lumen Account <security@lumen.dev>", f"{ME_NAME} <{ME}>", "New sign-in from Firefox on Linux", ago(0, 7),
        text="We noticed a new sign-in to your Lumen account from Firefox on Linux (Istanbul, TR). If this was you, you can ignore this email.",
        headers={"Auto-Submitted": "auto-generated"},
    ))

    # 12. Offsite planning thread with several people.
    thread(s, inbox_or_sent, [
        ("olivia", ["me", "daniel", "priya", "sofia"], ago(8, 3), "Team! Offsite is happening 🏔️ Two options:\n\n1. Cappadocia, 3 days, hot air balloons\n2. Black Forest, cabins + hiking\n\nVote by Friday!", {}),
        ("daniel", ["olivia", "me", "priya", "sofia"], ago(8, 1), "Cappadocia, obviously.", {}),
        ("sofia", ["olivia", "me", "daniel", "priya"], ago(7, 20), "+1 Cappadocia! I can look into flights.", {}),
        ("me", ["olivia", "daniel", "priya", "sofia"], ago(7, 18), "Cappadocia from me too. Happy to organise the balloon booking.", {}),
        ("priya", ["olivia", "me", "daniel", "sofia"], ago(7, 2), "Black Forest has better wifi 😅 but I'm outvoted. Cappadocia it is!", {}),
        ("olivia", ["me", "daniel", "priya", "sofia"], ago(6, 4), "Decided: Cappadocia, 21–23 November. Details coming soon.", {}),
    ], "Offsite planning 🏔️")

    # 13. Plain text with quoted reply.
    s.store(s.mail(
        addr("noah"), f"{ME_NAME} <{ME}>", "Re: Book recommendations", ago(9, 6),
        text="Ha, I knew you'd like it. Next up: \"The Pragmatic Programmer\", then we argue about it over dinner.\n\nOn Mon, Alex Morgan wrote:\n> Finished \"A Philosophy of Software Design\" - loved the chapter on deep modules.\n> What should I read next?\n",
    ))

    # 14. Archive, Sent and Drafts.
    for i in range(30):
        who = random.choice(["maya", "daniel", "priya", "sofia", "olivia", "noah"])
        topic = random.choice(["Weekly sync notes", "Quick question about the API", "Invoice for September", "Lunch?",
                               "Feedback on the copy", "Release checklist", "Conference talk proposal", "Bug bash results"])
        s.store(s.mail(addr(who), f"{ME_NAME} <{ME}>", topic, ago(12 + i * 2, random.randint(0, 20)),
                       text=f"Hi Alex,\n\n{topic} — details below. Let me know if anything is unclear.\n\nThanks!"), "Archive")
    for i in range(8):
        who = random.choice(["maya", "daniel", "priya", "lukas"])
        s.store(s.mail(f"{ME_NAME} <{ME}>", addr(who), random.choice(["Following up", "Notes from today", "Slides", "Thanks!"]),
                       ago(10 + i * 3, 3), text="Hi,\n\nSharing as promised.\n\nBest,\nAlex"), "Sent")
    s.store(s.mail(f"{ME_NAME} <{ME}>", addr("maya"), "Onboarding feedback", ago(0, 0, 30),
                   text="Hi Maya,\n\nLove the direction. A few thoughts:\n- "), "Drafts", draft=True)

    # 15. Bulk filler so lists are long enough to scroll.
    senders = ["Tracklane <updates@tracklane.test>", "Cloudshelf <no-reply@cloudshelf.test>",
               "Meetly <events@meetly.test>", "Rust Weekly <editor@rustweekly.test>",
               "Fieldnotes <hello@fieldnotes.test>"]
    subjects = ["Your weekly summary", "3 issues were assigned to you", "Storage almost full",
                "New comment on 'Launch plan'", "Event reminder: Linux Meetup Istanbul", "This week in Rust",
                "Your export is ready", "Tips to get the most out of your workspace"]
    for i in range(70):
        s.store(s.mail(random.choice(senders), f"{ME_NAME} <{ME}>", random.choice(subjects),
                       ago(10 + i, random.randint(0, 23), random.randint(0, 59)),
                       text="Here's what happened since your last visit. Open the app to see everything."),
                seen=True)

    print(f"seeded {s.count} messages into {MAILDIR}")


if __name__ == "__main__":
    main()

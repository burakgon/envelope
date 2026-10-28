#!/usr/bin/env python3
"""Seeds Radicale calendars for alex@envelope.test with realistic events.

Usage: seed_calendar.py <radicale storage folder>
"""
import json
import os
import sys
import uuid
from datetime import date, datetime, timedelta

ROOT = os.path.join(sys.argv[1], "collection-root", "alex@envelope.test")
TODAY = date.today()
MONDAY = TODAY - timedelta(days=TODAY.weekday())


def collection(name, display, color):
    path = os.path.join(ROOT, name)
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, ".Radicale.props"), "w") as f:
        json.dump({
            "tag": "VCALENDAR",
            "D:displayname": display,
            "ICAL:calendar-color": color,
            "C:supported-calendar-component-set": "VEVENT",
        }, f)
    return path


def dt(d, h, m=0):
    return datetime(d.year, d.month, d.day, h, m).strftime("%Y%m%dT%H%M%S")


def event(path, summary, start, end, *, all_day=False, rrule=None, location=None, description=None,
          attendees=(), organizer=None, alarm=None):
    uid = f"{uuid.uuid4()}@envelope.test"
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//Envelope//Dev Seed//EN", "BEGIN:VEVENT",
             f"UID:{uid}", f"DTSTAMP:{datetime.utcnow().strftime('%Y%m%dT%H%M%SZ')}", f"SUMMARY:{summary}"]
    if all_day:
        lines += [f"DTSTART;VALUE=DATE:{start.strftime('%Y%m%d')}", f"DTEND;VALUE=DATE:{end.strftime('%Y%m%d')}"]
    else:
        lines += [f"DTSTART;TZID=Europe/Istanbul:{start}", f"DTEND;TZID=Europe/Istanbul:{end}"]
    if rrule:
        lines.append(f"RRULE:{rrule}")
    if location:
        lines.append(f"LOCATION:{location}")
    if description:
        lines.append("DESCRIPTION:" + description.replace("\n", "\\n").replace(",", "\\,"))
    if organizer:
        lines.append(f"ORGANIZER;CN={organizer[0]}:mailto:{organizer[1]}")
    for name, email, status in attendees:
        lines.append(f"ATTENDEE;CN={name};PARTSTAT={status};ROLE=REQ-PARTICIPANT:mailto:{email}")
    if alarm is not None:
        lines += ["BEGIN:VALARM", "ACTION:DISPLAY", "DESCRIPTION:Reminder", f"TRIGGER:-PT{alarm}M", "END:VALARM"]
    lines += ["END:VEVENT", "END:VCALENDAR", ""]
    with open(os.path.join(path, f"{uid}.ics"), "w") as f:
        f.write("\r\n".join(lines))


work = collection("work", "Work", "#6d5efc")
home = collection("personal", "Personal", "#0ea5e9")

team = [("Priya Raman", "priya@lumen.dev", "ACCEPTED"), ("Daniel Okafor", "daniel@lumen.dev", "ACCEPTED"),
        ("Alex Morgan", "alex@envelope.test", "ACCEPTED")]
event(work, "Daily standup", dt(MONDAY, 9, 30), dt(MONDAY, 9, 45), rrule="FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
      location="https://meet.lumen.dev/standup", attendees=team, organizer=("Priya Raman", "priya@lumen.dev"), alarm=5)
event(work, "Product sync", dt(MONDAY + timedelta(days=3), 10), dt(MONDAY + timedelta(days=3), 10, 45),
      rrule="FREQ=WEEKLY;BYDAY=TH", location="https://meet.lumen.dev/product-sync",
      description="Roadmap, launches and blockers.", attendees=team, organizer=("Daniel Okafor", "daniel@lumen.dev"), alarm=10)
event(work, "1:1 with Daniel", dt(MONDAY + timedelta(days=1), 14), dt(MONDAY + timedelta(days=1), 14, 30),
      rrule="FREQ=WEEKLY;INTERVAL=2;BYDAY=TU", location="Room Bosphorus", alarm=10)
event(work, "Design review: onboarding", dt(TODAY, 15), dt(TODAY, 16), location="Figtree board",
      attendees=[("Maya Chen", "maya@northwind.design", "ACCEPTED"), ("Alex Morgan", "alex@envelope.test", "ACCEPTED")],
      organizer=("Alex Morgan", "alex@envelope.test"), alarm=15)
event(work, "Focus time", dt(MONDAY + timedelta(days=2), 13), dt(MONDAY + timedelta(days=2), 16), rrule="FREQ=WEEKLY;BYDAY=WE")
event(work, "Interview: Senior Rust Engineer", dt(TODAY + timedelta(days=2), 11), dt(TODAY + timedelta(days=2), 12),
      location="https://meet.lumen.dev/interview-7", alarm=10)
event(work, "Team offsite — Cappadocia", TODAY + timedelta(days=24), TODAY + timedelta(days=27), all_day=True,
      location="Göreme, Cappadocia")
event(work, "Q4 planning", dt(MONDAY + timedelta(days=4), 11), dt(MONDAY + timedelta(days=4), 12, 30),
      location="Big room", attendees=team, organizer=("Priya Raman", "priya@lumen.dev"))

event(home, "Coffee with Elif ☕", dt(MONDAY + timedelta(days=5), 11), dt(MONDAY + timedelta(days=5), 12, 30),
      location="Moda Cd. No: 12, Kadıköy")
event(home, "Gym", dt(MONDAY, 18, 30), dt(MONDAY, 19, 30), rrule="FREQ=WEEKLY;BYDAY=MO,WE,FR", location="Pulse Fitness")
event(home, "Flight IST → BER (SKY7Q2)", dt(TODAY + timedelta(days=16), 9, 40), dt(TODAY + timedelta(days=16), 11, 55),
      location="Istanbul Airport (IST)", description="Seat 12A. Booking SKY7Q2.", alarm=180)
event(home, "Mom's birthday 🎂", date(TODAY.year, 11, 21), date(TODAY.year, 11, 22), all_day=True, rrule="FREQ=YEARLY")
event(home, "Dentist", dt(TODAY + timedelta(days=6), 16), dt(TODAY + timedelta(days=6), 16, 45), location="Dr. Aksoy, Nişantaşı", alarm=60)
event(home, "Apartment viewing", dt(MONDAY + timedelta(days=5), 14), dt(MONDAY + timedelta(days=5), 15),
      location="Rosenthaler Str., Berlin")
print(f"seeded calendars in {ROOT}")

"""Scheduling (RFC 6638) compatibility test with the python-caldav library.

Needs a running CalCraft server with `[scheduling] enabled = true` and
`domain = "example.org"`, and two users alice and bob with the same
password. Alice invites bob, bob finds the invitation in his inbox and his
calendar and accepts it, alice sees the answer, and alice asks for bob's
free/busy time.

Usage:
    CALCRAFT_URL=http://127.0.0.1:5232 CALCRAFT_PASSWORD=... \
    python tests/compat/scheduling_client.py
"""

import os
import sys
import uuid
from datetime import datetime, timedelta, timezone

import caldav

URL = os.environ.get("CALCRAFT_URL", "http://127.0.0.1:5232")
PASSWORD = os.environ["CALCRAFT_PASSWORD"]


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        sys.exit(1)
    print(f"ok: {message}")


def client(user):
    return caldav.DAVClient(url=URL, username=user, password=PASSWORD)


def default_calendar(principal):
    return next(c for c in principal.get_calendars() if c.url.path.endswith("/default/"))


def main():
    alice_client = client("alice")
    bob_client = client("bob")
    check(alice_client.check_scheduling_support(), "calendar-auto-schedule in the DAV header")
    alice = alice_client.principal()
    bob = bob_client.principal()
    check("mailto:bob@example.org" in bob.calendar_user_address_set(), "calendar-user-address-set")
    check(bob.schedule_inbox().url is not None, "schedule-inbox-URL")
    check(alice.schedule_outbox().url is not None, "schedule-outbox-URL")

    uid = f"compat-sched-{uuid.uuid4()}"
    start = datetime(2026, 11, 2, 10, 0, tzinfo=timezone.utc)
    ical = (
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//compat//EN\r\nBEGIN:VEVENT\r\n"
        f"UID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261102T100000Z\r\n"
        "DTEND:20261102T110000Z\r\nSUMMARY:Scheduling check\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
    default_calendar(alice).save_with_invites(ical, [bob.get_vcal_address()])
    check(True, "alice saved an event with bob as attendee")

    messages = bob.schedule_inbox().get_items()
    check(any(m.is_invite_request() and uid in m.data for m in messages), "REQUEST in bob's inbox")

    found = default_calendar(bob).search(
        start=start - timedelta(hours=1), end=start + timedelta(hours=2), event=True
    )
    copies = [e for e in found if uid in e.data]
    check(len(copies) == 1, "invitation delivered into bob's calendar")
    copy = copies[0]
    copy.load()
    check("PARTSTAT=NEEDS-ACTION" in copy.data.replace("\r\n ", ""), "bob's status is NEEDS-ACTION")
    copy.change_attendee_status(partstat="ACCEPTED")
    copy.save(increase_seqno=False)
    check(True, "bob accepted")

    organizer = [
        e
        for e in default_calendar(alice).search(
            start=start - timedelta(hours=1), end=start + timedelta(hours=2), event=True
        )
        if uid in e.data
    ]
    check(len(organizer) == 1, "alice's event found")
    organizer[0].load()
    data = organizer[0].data.replace("\r\n ", "")
    check("PARTSTAT=ACCEPTED" in data and "bob@example.org" in data, "bob's answer in alice's event")
    check("SCHEDULE-STATUS=1.2" in data, "SCHEDULE-STATUS shows the delivery")
    alice_messages = alice.schedule_inbox().get_items()
    check(any(m.is_invite_reply() and uid in m.data for m in alice_messages), "REPLY in alice's inbox")

    response = alice.freebusy_request(start - timedelta(hours=2), start + timedelta(hours=4), [bob.get_vcal_address()])
    busy = response.get("mailto:bob@example.org") if response else None
    check(busy is not None and "FREEBUSY" in busy.data, "free/busy request answered")
    check("20261102T100000Z/20261102T110000Z" in busy.data.replace("\r\n ", ""), "bob is busy during the meeting")

    organizer[0].delete()
    cancelled = [e for e in default_calendar(bob).search(
        start=start - timedelta(hours=1), end=start + timedelta(hours=2), event=True
    ) if uid in e.data]
    check(len(cancelled) == 1 and "STATUS:CANCELLED" in cancelled[0].data, "cancellation reached bob")
    print("all scheduling checks passed")


if __name__ == "__main__":
    main()

"""Sharing compatibility test with the python-caldav library.

Needs a running CalCraft server (sharing is on by default) with two users
alice and bob with the same password. Alice shares her default calendar
with bob the way Apple Calendar does (CalendarServer `CS:share` POST), bob
finds the invitation in his notification collection and accepts it with a
`CS:invite-reply`, then python-caldav sees the shared calendar in bob's
calendar home, reads alice's event, cannot write while the share is
read-only and can after alice grants read-write; alice removes the share
and it is gone for bob.

Usage:
    CALCRAFT_URL=http://127.0.0.1:5232 CALCRAFT_PASSWORD=... \
    python tests/compat/sharing_client.py
"""

import os
import re
import sys
import uuid

import caldav

URL = os.environ.get("CALCRAFT_URL", "http://127.0.0.1:5232")
PASSWORD = os.environ["CALCRAFT_PASSWORD"]
NS = 'xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/"'
XML = {"Content-Type": "application/xml; charset=utf-8"}
SHARED = "/dav/calendars/bob/alice-default/"


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        sys.exit(1)
    print(f"ok: {message}")


def client(user):
    return caldav.DAVClient(url=URL, username=user, password=PASSWORD)


def event(uid, summary):
    return (
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//compat//EN\r\nBEGIN:VEVENT\r\n"
        f"UID:{uid}\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261103T100000Z\r\n"
        f"DTEND:20261103T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )


def text(response):
    raw = response.raw
    return raw if isinstance(raw, str) else raw.decode("utf-8")


def share(alice_client, access):
    body = (
        f"<CS:share {NS}><CS:set><D:href>/dav/principals/bob/</D:href>"
        f"<CS:summary>Alice</CS:summary><CS:{access}/></CS:set></CS:share>"
    )
    response = alice_client.request(f"{URL}/dav/calendars/alice/default/", "POST", body, XML)
    check(response.status == 200, f"alice shares her calendar ({access})")


def calendar_paths(principal):
    return [c.url.path for c in principal.get_calendars()]


def main():
    alice_client = client("alice")
    bob_client = client("bob")
    options = alice_client.request(f"{URL}/dav/", "OPTIONS")
    check("calendarserver-sharing" in options.headers.get("DAV", ""), "sharing in the DAV header")
    alice = alice_client.principal()
    bob = bob_client.principal()
    alice_calendar = next(c for c in alice.get_calendars() if c.url.path.endswith("/default/"))
    uid = f"compat-share-{uuid.uuid4()}"
    alice_calendar.save_event(event(uid, "Shared check"))

    share(alice_client, "read")
    check(SHARED not in calendar_paths(bob), "pending share not in bob's home")

    listing = bob_client.request(
        f"{URL}/dav/notifications/bob/",
        "PROPFIND",
        f"<D:propfind {NS}><D:prop><D:resourcetype/><CS:notificationtype/></D:prop></D:propfind>",
        {**XML, "Depth": "1"},
    )
    check(listing.status == 207, "notification collection")
    hrefs = re.findall(r"<d:href>(/dav/notifications/bob/[0-9a-f]{32}\.xml)</d:href>", text(listing))
    check(len(hrefs) == 1, "one invitation for bob")
    invitation = bob_client.request(f"{URL}{hrefs[0]}", "GET")
    match = re.search(r"<cs:uid>([0-9a-f]{32})</cs:uid>", text(invitation))
    check(match is not None and "/dav/calendars/alice/default/" in text(invitation), "invitation body")

    reply = (
        f"<CS:invite-reply {NS}><CS:invite-accepted/>"
        "<CS:hosturl><D:href>/dav/calendars/alice/default/</D:href></CS:hosturl>"
        f"<CS:in-reply-to>{match.group(1)}</CS:in-reply-to></CS:invite-reply>"
    )
    accepted = bob_client.request(f"{URL}/dav/calendars/bob/", "POST", reply, XML)
    check(accepted.status == 200 and SHARED in text(accepted), "bob accepts (CS:shared-as)")

    check(SHARED in calendar_paths(bob), "shared calendar in bob's home")
    shared = next(c for c in bob.get_calendars() if c.url.path == SHARED)
    events = shared.events()
    check(any(uid in e.data for e in events), "bob reads alice's event")

    denied = False
    try:
        shared.save_event(event(f"{uid}-bob", "Bob's write"))
    except Exception:  # noqa: BLE001 - any error from the refused PUT
        denied = True
    check(denied, "bob cannot write to a read-only share")

    share(alice_client, "read-write")
    shared.save_event(event(f"{uid}-bob", "Bob's write"))
    check(
        any(f"{uid}-bob" in e.data for e in alice_calendar.events()),
        "bob's event is in alice's calendar",
    )

    body = f"<CS:share {NS}><CS:remove><D:href>/dav/principals/bob/</D:href></CS:remove></CS:share>"
    removed = alice_client.request(f"{URL}/dav/calendars/alice/default/", "POST", body, XML)
    check(removed.status == 200, "alice removes the share")
    check(SHARED not in calendar_paths(bob), "the share is gone for bob")
    print("sharing compatibility: all checks passed")


if __name__ == "__main__":
    main()

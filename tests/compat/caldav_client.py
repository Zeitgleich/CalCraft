"""Client compatibility test with the python-caldav library.

Runs a typical client session against a running CalCraft server:
discovery, calendar listing, MKCALENDAR, PUT, time-range search, update,
multiget and delete.

Usage:
    CALCRAFT_URL=http://127.0.0.1:5232 CALCRAFT_USER=alice \
    CALCRAFT_PASSWORD=... python tests/compat/caldav_client.py
"""

import os
import sys
import uuid
from datetime import datetime, timedelta, timezone

import caldav

URL = os.environ.get("CALCRAFT_URL", "http://127.0.0.1:5232")
USER = os.environ["CALCRAFT_USER"]
PASSWORD = os.environ["CALCRAFT_PASSWORD"]


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        sys.exit(1)
    print(f"ok: {message}")


def main():
    client = caldav.DAVClient(url=URL, username=USER, password=PASSWORD)
    principal = client.principal()
    check(principal.url.path.rstrip("/").endswith(f"/principals/{USER}"), "principal discovered via .well-known")

    calendars = principal.calendars()
    check(any(c.url.path.endswith("/default/") for c in calendars), "default calendar listed")

    slug = f"compat-{uuid.uuid4().hex[:8]}"
    calendar = principal.make_calendar(name="Compat Test", cal_id=slug)
    check(calendar.url.path.endswith(f"/{slug}/"), "MKCALENDAR created a calendar")
    check(calendar.get_display_name() == "Compat Test", "display name set via MKCALENDAR")

    start = datetime(2026, 10, 21, 10, 0, tzinfo=timezone.utc)
    event = calendar.save_event(
        dtstart=start,
        dtend=start + timedelta(hours=1),
        summary="Compatibility check",
        uid=f"compat-{uuid.uuid4()}",
    )
    check(event.url is not None, "event stored via PUT")

    found = calendar.search(
        start=start - timedelta(days=1),
        end=start + timedelta(days=1),
        event=True,
        expand=False,
    )
    check(len(found) == 1, "time-range calendar-query finds the event")
    check("Compatibility check" in found[0].data, "calendar-data returned")

    missed = calendar.search(
        start=start + timedelta(days=30),
        end=start + timedelta(days=31),
        event=True,
        expand=False,
    )
    check(len(missed) == 0, "time-range query excludes events outside the range")

    event.data = event.data.replace("Compatibility check", "Updated title")
    event.save()
    fetched = calendar.event_by_url(event.url)
    fetched.load()
    check("Updated title" in fetched.data, "update via PUT with If-Match")

    objects = calendar.objects()
    loaded = list(objects)
    check(len(loaded) == 1, "PROPFIND Depth 1 lists the object")

    multiget = calendar.calendar_multiget([event.url])
    check(len(multiget) == 1 and "Updated title" in multiget[0].data, "calendar-multiget")

    event.delete()
    check(len(calendar.events()) == 0, "DELETE removed the event")

    calendar.delete()
    remaining = [c.url.path for c in principal.calendars()]
    check(not any(p.endswith(f"/{slug}/") for p in remaining), "calendar deleted")

    print("all compatibility checks passed")


if __name__ == "__main__":
    main()

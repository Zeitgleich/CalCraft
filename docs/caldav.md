# CalDAV: time ranges, recurrence and time zones

This page describes how CalCraft answers `calendar-query` REPORTs with a
`time-range` filter (RFC 4791 §9.9), and where it is exact or deliberately
conservative. Conservative means that a query may return an object that
does not quite match, but **never misses one that does**.

## How a query is answered

1. **Index.** At PUT (and import), every calendar object gets a time span in
   UTC: the first start and the last end of all its instances. The span is
   stored with the object; the query uses it to preselect candidates.
2. **Exact check.** For recurring objects (RRULE, RDATE, EXDATE or
   `RECURRENCE-ID` overrides) the span is only a hull. Each such candidate is
   expanded around the requested range and returned only if one of its
   instances overlaps it. Single events are decided by the index alone.

Results stay bounded as before: at most 5 000 objects and 32 MiB of
`calendar-data` per REPORT, then a truncated response with a 507 entry.

## Supported

- **RRULE** (RFC 5545 §3.3.10): `FREQ` from `SECONDLY` to `YEARLY`,
  `INTERVAL`, `COUNT`, `UNTIL`, `BYSECOND`, `BYMINUTE`, `BYHOUR`, `BYDAY`
  (with ordinals such as `-1FR`), `BYMONTHDAY` (also negative), `BYYEARDAY`,
  `BYWEEKNO`, `BYMONTH`, `BYSETPOS` and `WKST`. Invalid dates (30 February)
  are skipped. `DTSTART` is always an instance. The RRULE examples of RFC 5545
  are part of the test suite.
- **RDATE** (DATE, DATE-TIME and PERIOD) and **EXDATE**.
- **Overrides** with `RECURRENCE-ID` replace the instance they name, also when
  they move it into or out of the requested range.
- **All-day events** (DATE values), `DTEND`, `DURATION` and `DUE`.
- **Time zones from embedded VTIMEZONE components.** CalCraft has no time
  zone database. It evaluates the `STANDARD`/`DAYLIGHT` observances of the
  VTIMEZONE in the object itself (`DTSTART`, `TZOFFSETFROM`, `TZOFFSETTO`,
  `RRULE`, `RDATE`) up to the year 2200 and converts local times with a
  `TZID` to UTC. Local times in a DST gap are moved forward by the length of
  the gap; ambiguous times in a DST overlap mean their first occurrence
  (RFC 5545 §3.3.5). Almost all clients (Apple Calendar, Thunderbird,
  DAVx⁵, Evolution, Outlook via CalDAV Synchronizer) embed the VTIMEZONE.

## Conservative (widened or open-ended)

| Case | Behaviour |
|---|---|
| Floating times (no `Z`, no `TZID`) and all-day dates | Widened by ±14 h (the largest real UTC offset). CalCraft does not yet use the calendar's `CALDAV:calendar-timezone`. |
| `TZID` without an embedded VTIMEZONE (RFC 7809 style, clients relying on the server's time zone database) | Widened by ±14 h. |
| VTIMEZONE that cannot be evaluated (missing offsets, unsupported rule, more than 5 000 transitions), local times before its first onset or after 2200 | Widened by ±14 h. |
| Open-ended rules (no `COUNT`, no `UNTIL`) | The index stays open towards the future; queries still check the instances exactly (expansion skips ahead to the requested range). |
| Rules too large for the budget (see below) | Index: open-ended, or bounded by `UNTIL`. Query: the object is returned. |
| `RANGE=THISANDFUTURE` overrides | Later instances count both at their original time and shifted by the override's offset, with the override's length. |
| `RSCALE`/`SKIP` (RFC 7529), x-parts in RRULE, combinations RFC 5545 forbids (e.g. `BYWEEKNO` outside `YEARLY`, `BYDAY` ordinals in `WEEKLY`), and irregular rules (repeated parts, out-of-range or malformed BYxxx values such as `BYHOUR=24` or `BYDAY=0MO`, trailing commas, lists over 1 000 entries, more than 32 parts) | Accepted and stored unchanged, but not expanded: treated like a rule too large for the budget. Only what CalCraft always rejected stays an error: no or unknown `FREQ`, invalid `UNTIL`, `COUNT` together with `UNTIL`, zero or non-numeric `INTERVAL`/`COUNT`, parts without `=`. |
| `VTODO` boundaries | Compared inclusively at both ends (RFC 4791's VTODO conditions use `<=`/`>=`). |
| VTODO/VJOURNAL without any date | Match every time range. |

## Budgets

Recurrence expansion is a classic denial-of-service vector (see
[security/angriffshistorie.md](security/angriffshistorie.md), Stalwart,
python-icalendar, rrule.js, Open WebUI). CalCraft bounds it everywhere:

- **One parser** validates RRULEs at PUT and drives expansion, so validation
  and expansion cannot disagree.
- **Per object:** 200 000 work steps (one step per period, candidate or
  date value) and at most 10 000 visited instances per expansion. Empty
  rules such as `FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30` end at year 9999 within
  that budget; sub-daily rules skip non-matching days, hours and minutes as
  a whole.
- **Per REPORT:** 2 000 000 steps, 1 000 checked objects, 64 MiB of object
  data read for checks and 5 seconds in total. The checks run on the
  blocking pool, at most half the CPU cores at a time (a semaphore separate
  from password hashing), and stop at their next step when the request is
  cancelled (client gone, timeout).
- **Per component:** at most 10 000 RDATE plus EXDATE values and 16 distinct
  `TZID`s per object. A VTIMEZONE with more than 5 000 transitions, more than
  16 distinct offsets or transitions less than 6 hours apart is not evaluated
  (times are widened instead), which keeps each time conversion cheap; every
  conversion is charged to the budget.

When a budget runs out, the answer falls back to the conservative one: the
object is indexed as open-ended (or up to `UNTIL`) and returned by queries.
`RANGE=THISANDFUTURE` shifts widen the expansion window by the largest shift,
so moved instances are found however far they moved (within the budget).

## Upgrades

Migration `0007_recurrence` adds the columns `items.recurring` and
`items.index_version`. Objects stored by older versions keep their previous
(conservative) span until the server re-indexes them: right after startup, in
the background, in batches of 32 objects, once per object (idempotent, objects
changed meanwhile are skipped). Re-indexing changes neither the objects, their
ETags nor sync tokens. If it is interrupted, it continues at the next start;
an object that cannot be indexed is marked to match every time range, so it
never blocks the run.

## Collection properties

PROPPATCH (and MKCALENDAR / extended MKCOL when creating) handles the
properties of calendars and address books like this:

| Property | Behaviour |
|---|---|
| `DAV:displayname`, `CALDAV:calendar-description`, `CARDDAV:addressbook-description`, `{http://apple.com/ns/ical/}calendar-color` | Live: validated (no control or bidi characters; colours `#RRGGBB` / `#RRGGBBAA`), 409 otherwise. Descriptions and the colour only on the matching kind (403). |
| `{http://apple.com/ns/ical/}calendar-order` | Calendars only; must be an integer (409 otherwise), stored and returned exactly as sent. The web UI lists calendars by it, then by name. |
| `CALDAV:calendar-timezone`, `CALDAV:schedule-calendar-transp`, `CALDAV:default-alarm-vevent-date`, `CALDAV:default-alarm-vevent-datetime` | Dead: stored and returned, not interpreted. |
| Any other property in the `DAV:`, CalDAV or CardDAV namespace, and `{http://calendarserver.org/ns/}getctag` | Protected: 403 with `DAV:cannot-modify-protected-property` (e.g. `resourcetype`, `getetag`, `sync-token`, `owner`, `principal-URL`, `current-user-privilege-set`, `supported-calendar-component-set`, `quota-used-bytes`). |
| Everything else | Dead property (RFC 4918 §4). |

A PROPPATCH is atomic: if one property fails, nothing changes and the others
are answered with 424 Failed Dependency. Removing a property that is not set
succeeds. MKCALENDAR and MKCOL ignore protected properties and fail with 403
(invalid value) or 507 (too large) as a whole.

Dead properties:

- **Names**: the namespace must be an absolute URI (`scheme:…`) or empty
  (`xmlns=""`, allowed but discouraged); at most 256 bytes for the namespace
  and 128 for the local name. Otherwise 403.
- **Values** may be text or XML fragments. They are not stored as sent: the
  request body is parsed by the hardened XML parser (no DTDs, depth and
  size limits) and the property content is serialised again, with every
  element declaring its own namespace and all text and attribute values
  escaped. Values read back are parsed and serialised the same way again
  before they are sent. Not kept: comments, processing instructions,
  prefixed attributes (including `xml:lang`), attributes of the property
  element itself, whitespace between child elements and the position of
  text between child elements (it is kept in front of them). Characters
  XML 1.0 does not allow (e.g. `&#1;`) are rejected with 409.
- **Limits**: 16 KiB per serialised value (64 KiB for
  `CALDAV:calendar-timezone`, as a VTIMEZONE with its full history can be
  large), 64 properties and 128 KiB (namespaces, names and values) per
  collection. Their bytes count towards the owner's storage quota, also
  while their collection is in the trash (its entry size includes them).
  Limits and quota are checked in the same transaction as the change, before
  anything is written; beyond them the properties being set get 507
  Insufficient Storage. Removing or shrinking properties always works, also
  above the quota.
- **Names** of elements and attributes inside values (and of the
  properties) must be XML 1.0 names; anything else is rejected by the parser
  with 400. A stored property whose name or value does not pass these checks
  again (e.g. a damaged database) is left out of PROPFIND responses and
  logged with its collection id, instead of breaking the response.
- **Scope**: only collections. PROPPATCH on calendar objects and contacts is
  not supported (405); their content is replaced on every PUT anyway.
- They are part of backups (the whole database is copied) and go into the
  trash and back with their collection.

Changing a property – live or dead – bumps the collection's `getctag` and
`sync-token`, so clients such as Apple Calendar read the properties again; no
object changes are recorded, so a `sync-collection` report from the previous
token is empty.

## Not yet supported

- `CALDAV:expand` and `CALDAV:limit-recurrence-set` in `calendar-data`:
  objects are always returned whole, with their rules (clients expand them
  themselves, which all common clients do).
- `CALDAV:limit-freebusy-set` and the `CALDAV:free-busy-query` REPORT on
  calendars. Free/busy lookups between users go through the scheduling
  outbox instead ([scheduling.md](scheduling.md)).
- The calendar's `CALDAV:calendar-timezone` for floating times, and a time
  zone database for `TZID`s without VTIMEZONE.
- `prop-filter`, `param-filter` and `text-match` (rejected with
  `CALDAV:supported-filter`).

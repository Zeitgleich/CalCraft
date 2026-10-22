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
| `RSCALE`/`SKIP` (RFC 7529), x-parts in RRULE, and combinations RFC 5545 forbids (e.g. `BYWEEKNO` outside `YEARLY`, `BYDAY` ordinals in `WEEKLY`) | Accepted, but not expanded: treated like a rule too large for the budget. |
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
- **Per REPORT:** 20 000 000 steps and 10 000 exact checks in total; the
  checks run outside the async worker threads.
- **Per component:** at most 10 000 RDATE plus EXDATE values, 16 distinct
  `TZID`s per object, 1 000 entries per BYxxx list, 32 RRULE parts.

When a budget runs out, the answer falls back to the conservative one: the
object is indexed as open-ended (or up to `UNTIL`) and returned by queries.

## Upgrades

Migration `0007_recurrence` adds the columns `items.recurring` and
`items.index_version`. Objects stored by older versions keep their previous
(conservative) span until the server re-indexes them: right after startup, in
the background, in batches of 32 objects, once per object (idempotent, objects
changed meanwhile are skipped). Re-indexing changes neither the objects, their
ETags nor sync tokens. If it is interrupted, it continues at the next start.

## Not yet supported

- `CALDAV:expand` and `CALDAV:limit-recurrence-set` in `calendar-data`:
  objects are always returned whole, with their rules (clients expand them
  themselves, which all common clients do).
- `CALDAV:limit-freebusy-set` and free-busy queries.
- The calendar's `CALDAV:calendar-timezone` for floating times, and a time
  zone database for `TZID`s without VTIMEZONE.
- `prop-filter`, `param-filter` and `text-match` (rejected with
  `CALDAV:supported-filter`).

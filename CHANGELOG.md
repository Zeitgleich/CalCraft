# Changelog

All notable changes to CalCraft are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions
follow [Semantic Versioning](https://semver.org/). Release process:
[docs/releasing.md](docs/releasing.md).

## [Unreleased]

### Added

- Dead properties (RFC 4918 §4) on calendars and address books: PROPPATCH,
  MKCALENDAR and extended MKCOL store client-defined properties, PROPFIND
  returns them (explicitly, with `allprop` and `propname`), PROPPATCH
  `remove` deletes them. Apple Calendar keeps its calendar order
  (`{http://apple.com/ns/ical/}calendar-order`, an integer) and settings such
  as `CALDAV:calendar-timezone` and `CALDAV:schedule-calendar-transp`.
  Values are stored as canonical XML re-serialised from the parsed request,
  never as the bytes sent. Limits: 64 properties per collection, namespace
  256 / local name 128 bytes, 16 KiB per value and 64 KiB per collection
  (507 Insufficient Storage); namespaces must be absolute URIs or empty
  (403). They survive restarts, are part of backups and are restored with a
  collection from the trash. Migration `0010_dead_properties`.
- The web UI and `GET /api/v1/collections` list calendars by
  `calendar-order`, then by name; `CollectionInfo` has a new field `order`.
  Reordering in the web UI itself is not available yet.

### Changed

- PROPPATCH answers protected properties (everything else in the `DAV:`,
  CalDAV and CardDAV namespaces, e.g. `resourcetype`, `getetag`,
  `sync-token`, `owner`, `current-user-privilege-set`,
  `supported-calendar-component-set`, quota properties, and
  `getctag`) with 403 and `DAV:cannot-modify-protected-property`; the whole
  request still fails atomically (424 for the other properties).

## [0.1.0-alpha.1] – 2026-10-22

First pre-release, for testing only – not for production use (open points:
[docs/produktionsreife.md](docs/produktionsreife.md)). It contains CalDAV,
CardDAV, the REST API, the web UI, OIDC, import/export and backup/restore as
described in the README; the entries below list what changed since this
changelog was started. A new installation applies all database migrations up
to `0007_recurrence`.

### Added

- Trash: DAV and web UI deletions of calendar objects, contacts and whole
  collections move them into the owner's trash for
  `storage.trash_retention_days` (default 30, `0` = final deletes as
  before). Sync clients still see a normal deletion. New **Trash** page and
  REST endpoints `GET /api/v1/trash`, `POST /api/v1/trash/{id}/restore`,
  `DELETE /api/v1/trash/{id}` and `DELETE /api/v1/trash`; restores go back to
  the original collection and name (409 with `name_taken`, `uid_taken`,
  `collection_exists`, `collection_in_trash`, `collection_deleted` or
  `invalid_target` on conflicts; objects of a permanently deleted collection
  can be restored into another one with `?collection=`), keep ETag and
  modification time, and appear as new changes for sync clients. The trash
  counts towards the storage quota and holds at most 101 000 rows per user
  (the oldest entries are dropped early; large ones are left to the
  background purge so a delete never blocks the database for long); expired
  and emptied entries are purged in small batches at startup, hourly and
  shortly after emptying. With `trash_retention_days = 0` all remaining
  entries are purged at startup (logged as a warning). Database migration
  `0006_trash`; backups include the trash.
- DAV `MOVE` and `COPY` (RFC 4918 §9.8/§9.9) for calendar objects and vCards
  between the user's own collections of the same kind, e.g. moving an event
  to another calendar. `Destination` is an absolute path or a URI on the
  request's host (`http` or `https`; other hosts/schemes: `502`), decoded
  like request paths; `..`, query, fragment and user info: `400`.
  `Overwrite: T|F` (default `T`), `If-Match`/`If-None-Match` apply to the
  source, `Depth` must be absent or `0`. Answers: `201`/`204` with the
  (unchanged) `ETag`; `412` for `Overwrite: F` on an existing object or a
  failed condition; `409` if the destination is not an object path in one
  of the user's collections – a missing collection and another user's
  collection get the same answer; `403` for source = destination,
  collections as source or destination, `no-uid-conflict`,
  `supported-calendar-component` and a calendar object into an address book
  (or back); `507` when a copy exceeds the quota (a move is not counted
  twice). Source removal, destination write, tombstone, change-log entries
  and sync tokens of both collections happen in one transaction; a moved
  object never counts as deleted by the user. Moving or copying whole
  collections, other users' collections and other servers are not supported.
- CardDAV contact photos: vCards may be up to `storage.max_vcard_bytes`
  (default 2 MiB, or `server.max_body_bytes` if smaller; an explicit value
  must be at most 10 MiB and `server.max_body_bytes`) instead of 1 MiB;
  `CARDDAV:max-resource-size` reports it. `PHOTO`/`LOGO` must be empty,
  valid base64 of JPEG, PNG, GIF or WebP (vCard 3.0 `ENCODING=b`, vCard 4.0
  `data:image/…` URI; every declared `TYPE` and the magic number of the
  first bytes are checked, with or without a declared type) or an
  `http`/`https` URI (never fetched); `javascript:`, `file:`, SVG and
  invalid base64 are rejected. vCard 4.0 `MEMBER` requires
  `KIND:group`. Tests for Apple and vCard 4.0 groups; the store's
  `test-hooks` feature can lower the quota for quota tests.

- Request limits: at most `server.max_concurrent_requests` (256) requests at
  once (`503`), per client 32 concurrent requests and 120 burst / 20 per
  second (`429`), 20 failed logins per client in 15 minutes; IPv6 per /64,
  bounded client tables. Client addresses from `X-Forwarded-For` only for
  `server.trusted_proxies` (default: loopback, for a reverse proxy on the
  same host; others such as a Docker bridge must be listed).
- Release pipeline (`.github/workflows/release.yml`): binaries for Linux,
  macOS and Windows, `SHA256SUMS`, build provenance attestations and a
  signed multi-arch Docker image on `ghcr.io/zeitgleich/calcraft`; see
  [docs/release-verification.md](docs/release-verification.md).
- Logo ("Blatt und Funke") with wordmark, favicon, Apple touch icon and web
  app manifest; sources and PNG exports in `assets/brand/`, see
  [docs/brand.md](docs/brand.md). The Content Security Policy gains
  `manifest-src 'self'` for the manifest.
- Exact `calendar-query` time ranges for recurring events: RRULE expansion
  (all BYxxx parts incl. BYSETPOS, WKST, ordinal BYDAY, BYWEEKNO), RDATE,
  EXDATE and `RECURRENCE-ID` overrides, with work budgets per object and per
  REPORT; exceeding a budget returns the object conservatively. Time zones
  come from the VTIMEZONE embedded in the object (DST gaps and overlaps per
  RFC 5545), without a time zone database; floating times and `TZID`s
  without VTIMEZONE keep the ±14 h widening. See [docs/caldav.md](docs/caldav.md).
- Fuzz target `recurrence` for the RRULE and VTIMEZONE evaluator.

### Changed

- Database migration `0007_recurrence` (columns `items.recurring`,
  `items.index_version`). After the upgrade, existing calendar objects are
  re-indexed once in the background; until then their time ranges stay
  conservative. Objects, ETags and sync tokens do not change.

- Web UI: refreshed design with the logo in the header and on the login page,
  a token-based colour system (light and dark), clearer buttons, focus rings
  and form fields, collection cards with their colour along the left edge,
  outlined empty states, and tables that scroll inside their frame on phones.

### Fixed

- `calendar-query` no longer misses a VTODO whose DUE equals the end of the
  requested range, nor objects with an RDATE before their DTSTART.

- Web UI: the action column of the user and app-password tables now lines up
  with the other cells; on phone widths the header is compact (user and
  logout next to the name, navigation as a scrollable row below).
- More than five parallel DAV requests of one account with correct
  credentials no longer get 401: app passwords and cached passwords need no
  login attempt slot, and other requests wait (at most 8 per account name,
  5 s) for running attempts instead of counting as a lockout.
- Login attempts cancelled while credentials are compared (client hangs
  up, request timeout) count as failures; cancelled while still waiting
  for a hash permit or the database, they count for nothing, so others
  cannot lock an account by keeping the server busy. Waiting for a database
  connection ends after at most 10 s (half the request timeout if that is
  shorter) with 503 and `Retry-After` instead of the 408 request timeout;
  waits for a login slot or a password-hash permit are still bounded by the
  request timeout (408).
- Cancelled requests keep their Argon2 permit until hashing ends, so no
  more than four password hashes run at once.
- Creating or renaming collections, creating users and OIDC link approvals
  no longer fail with 500 (`SQLITE_BUSY`) under concurrent writes: all write
  transactions start with `BEGIN IMMEDIATE`.
- Under many concurrent writes (seen on Windows CI) a write could wait
  longer than the 5 s busy timeout and fail with 500: SQLite's busy handler
  polls and is not fair. All writes now queue in order for a single writer
  connection; a database that stays locked anyway answers 503 with
  `Retry-After` instead of 500, and storage errors log the SQLite code.
- A full disk answers 507 Insufficient Storage instead of 500, and pooled
  database connections whose transaction SQLite rolled back by itself (disk
  full, I/O error) are closed instead of failing every later write.

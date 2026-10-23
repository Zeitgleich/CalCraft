# Changelog

All notable changes to CalCraft are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions
follow [Semantic Versioning](https://semver.org/). Release process:
[docs/releasing.md](docs/releasing.md).

## [Unreleased]

### Added

- CalDAV scheduling (RFC 6638), stage A: between users of the same server,
  off by default (`[scheduling] enabled = true`, see
  [docs/scheduling.md](docs/scheduling.md)). Implicit scheduling on PUT and
  DELETE: invitations (REQUEST) go into the attendees' default calendars and
  scheduling inboxes, attendees' participation changes (REPLY) come back into
  the organizer's event, removed attendees and deleted events are cancelled;
  recurring events with overrides and per-instance replies (also via
  EXDATE), `SCHEDULE-AGENT`, `SCHEDULE-FORCE-SEND`, server-set
  `SCHEDULE-STATUS`, `Schedule-Tag` and `If-Schedule-Tag-Match`.
  Scheduling inbox and outbox per user (`/dav/scheduling/<user>/inbox/` and
  `/outbox/`), `calendar-user-address-set`, `schedule-default-calendar-URL`,
  `calendar-auto-schedule` in the DAV header, POST (free/busy requests) on
  the outbox. Free/busy answers contain busy periods only (opaque events,
  `schedule-calendar-transp` respected).
- Calendar user addresses: an e-mail address per user, set only by
  administrators (`calcraft user set-email`, `calcraft user add --email`,
  `email` in the admin REST API and web UI), unique across users, plus
  `mailto:<username>@<scheduling.domain>`. Never taken from SSO claims.
- Limits: `max_attendees`, `max_recipients_per_hour` (every delivery, every
  new attendee and every free/busy recipient), `max_delivered_bytes_per_hour`
  and `max_delivered_object_bytes` per recipient, `inbox_max_messages`,
  `max_freebusy_days`; deliveries count towards the recipient's quota and are
  refused with 5.1 instead of failing the organizer's request; disabled users
  are treated like unknown ones. Cancelled invitations the attendee never
  touched are removed instead of kept.
- Users can refuse invitations (web UI *Settings*, `PATCH
  /api/v1/me/scheduling`, `calcraft user set-invitations`); they then look
  like unknown addresses.
- Fuzz target `itip`; the client compatibility CI job also runs a
  python-caldav scheduling session.
- CalDAV managed attachments (RFC 8607), off by default (`[attachments]
  enabled = true` and `base_url`, see
  [docs/attachments.md](docs/attachments.md)): `POST <object>?action=
  attachment-add|attachment-update|attachment-remove` with `managed-id` and
  `rid` (recurrence instances, overrides created as needed),
  `Cal-Managed-ID`, `calendar-managed-attachments` in the DAV header,
  `managed-attachments-server-URL`, `max-attachment-size` and
  `max-attachments-per-resource`. Files are stored in the database (part of
  backups and the trash), owned by the uploader and counted in their quota
  (`max_bytes`, `max_per_object`, `max_bytes_per_user`), and removed when
  no object or trash entry references them. Downloads below
  `/dav/attachments/<managed-id>` for the uploader and for users whose own
  objects reference the file (invitations), always as `Content-Disposition:
  attachment` with `nosniff`, `default-src 'none'; sandbox` and only
  allowlisted media types; sanitised file names. PUTs may keep their own
  attachments (rewritten to the server's form) but not reference somebody
  else's (`CALDAV:valid-managed-id-parameter`); cancelled attendee copies
  lose the organizer's attachments. Fuzz target `attach`.

### Changed

- `POST` is part of the DAV `Allow` header (only the scheduling outbox
  accepts it).
- Migration `0011_scheduling` (columns `users.email`,
  `users.accept_invitations`, `users.inbox_revision`, `items.schedule_tag`,
  table `schedule_inbox`).
- Migration `0012_attachments` (tables `attachments` and `attachment_refs`,
  a trigger that removes unreferenced attachments).
- `server.max_body_bytes` is enforced by an own middleware (same behaviour:
  `413` for a larger `Content-Length`); only attachment uploads get
  `attachments.max_bytes` instead. `MOVE` now moves the stored row (same
  result for clients).

## [0.1.0-alpha.2] – 2026-10-22

Second pre-release, for testing only – not for production use (open points:
[docs/produktionsreife.md](docs/produktionsreife.md)). New: calendar
subscriptions (webcal), `calcraft migrate`, calendar order and dead
properties, a systemd service and a write-aware `/readyz`. Upgrading applies
the database migrations `0008_health`, `0009_subscriptions` and
`0010_dead_properties` (a pre-migration backup is taken by default). New
configuration section `[subscriptions]` (off by default); new Cargo features
`subscriptions` and `migrate` (both on by default).

### Added

- Dead properties (RFC 4918 §4) on calendars and address books: PROPPATCH,
  MKCALENDAR and extended MKCOL store client-defined properties, PROPFIND
  returns them (explicitly, with `allprop` and `propname`), PROPPATCH
  `remove` deletes them. Apple Calendar keeps its calendar order
  (`{http://apple.com/ns/ical/}calendar-order`, an integer) and settings such
  as `CALDAV:calendar-timezone` and `CALDAV:schedule-calendar-transp`.
  Values are stored as canonical XML re-serialised from the parsed request,
  never as the bytes sent. Limits: 64 properties per collection, namespace
  256 / local name 128 bytes, 16 KiB per value (64 KiB for
  `calendar-timezone`) and 128 KiB per collection (507 Insufficient
  Storage); their bytes count towards the storage quota, including the trash.
  Namespaces must be absolute URIs or empty (403). They survive restarts,
  are part of backups and are restored with a collection from the trash.
  Migration `0010_dead_properties`.
- The web UI and `GET /api/v1/collections` list calendars by
  `calendar-order`, then by name; `CollectionInfo` has a new field `order`.
  Reordering in the web UI itself is not available yet.
- Calendar subscriptions (webcal/ICS feeds), off by default
  (`[subscriptions] enabled`): the server fetches a feed and stores its events
  in a read-only calendar that every CalDAV client syncs like any other
  (sync-collection included). Web UI section *Subscribed calendars* on the
  overview page and REST endpoints `GET/POST /api/v1/subscriptions`,
  `PATCH/DELETE /api/v1/subscriptions/{slug}` and
  `POST /api/v1/subscriptions/{slug}/refresh` (errors `invalid_url`,
  `blocked_address`, `fetch_failed`, `not_icalendar`, `too_large`,
  `limit_reached`, `rate_limited`). Refreshes run in the background (15 minutes
  to 7 days, the feed's `REFRESH-INTERVAL`/`X-PUBLISHED-TTL` within these
  bounds, default 6 hours, jitter, two fetches at a time, conditional
  requests); only changed events get a new ETag and sync revision; a broken
  feed keeps its last good content and backs off. SSRF protection: https only
  (http only with `allow_http`), allowed ports, no user info, strict host
  syntax, own DNS resolution with every address checked against private,
  loopback, link-local, metadata, CGNAT, multicast, documentation, NAT64, 6to4,
  Teredo and IPv4-mapped ranges, connection pinned to the checked address
  (DNS rebinding), redirects followed manually and checked again (at most 3,
  never https → http), deadline, size limit (10 MiB), no cookies or
  credentials, fixed User-Agent; optional `allowed_hosts`/`denied_hosts`.
  Limits: 20 subscriptions, 10 subscribe attempts and 10 "refresh now" per 10
  minutes per user, at most 10 s waiting for a fetch slot (`503 busy`), a
  bounded background queue, at most 4 concurrent DNS lookups; names that
  resolve to private addresses, do not exist or time out all answer
  `fetch_failed` (no DNS oracle),
  quota and object limits as for imports. Feed URLs are shown only to their
  owner and never logged (host only). Database migration `0009_subscriptions`.
- DAV: collections can be read-only (subscriptions): writes answer `403` with
  `DAV:need-privileges`, `current-user-privilege-set` lists `read` only;
  `CollectionInfo` in the REST API has a new `read_only` field.
- systemd integration shipped in `contrib/systemd/` (and in the Linux release
  archives): `calcraft.service`, `calcraft.sysusers` and `calcraft.tmpfiles`.
  The unit uses `Type=notify` – CalCraft sends `READY=1` after the database
  is open (migrations applied) and the listener is bound, `STOPPING=1` on
  graceful shutdown, a status line and watchdog keep-alives
  (`WatchdogSec=30s`), implemented without new dependencies; outside
  systemd (no `NOTIFY_SOCKET`) nothing changes. It checks the configuration
  before starting (`ExecStartPre=calcraft check-config --quiet`), restarts
  on failure, stops restarting after 5 failed starts in 5 minutes and keeps
  the existing hardening and resource limits. A CI job installs the files on
  a real systemd and tests readiness, automatic restart after `SIGKILL`,
  graceful restart, persistence, the start limit and the
  `systemd-analyze security` exposure level.
- `calcraft check-config --quiet` validates without printing the effective
  configuration.
- `calcraft migrate`: copies calendars, task lists and address books of one
  account from another CalDAV/CardDAV server (Synology Calendar/Contacts,
  Radicale, Baïkal, Nextcloud, …) into a local user. Discovery via
  `.well-known`, `current-user-principal` and home sets; collections keep name,
  color, description and component set; objects are fetched with multiget in
  batches (GET fallback) and validated like `calcraft import`, keeping UIDs.
  `--dry-run`, `--only`, `--map`, `--on-conflict skip|replace`,
  `--include-shared`; idempotent with `skip`. Password from a prompt,
  `--password-file` (regular file, mode 600) or `CALCRAFT_MIGRATE_PASSWORD`,
  never from the command line, zeroized after use. HTTPS only (`--ca-file` for
  self-signed certificates, `--allow-http` with a warning), redirects only
  within the origin or from http to https on the same host and port (80 to
  443). Fetching stops once the quota or object limit is reached. New Cargo
  feature `migrate` (default on; uses `zeroize`, already a dependency of
  argon2 and rustls).

### Changed

- The XML parser accepts exactly the XML 1.0 name characters in element and
  attribute names (before: Rust's alphanumeric characters, e.g. `a²`, which
  could not be written back as XML). Such requests now fail with 400.
- PROPPATCH, MKCALENDAR and MKCOL bodies may be up to 128 KiB (was 64 KiB).
- PROPPATCH answers protected properties (everything else in the `DAV:`,
  CalDAV and CardDAV namespaces, e.g. `resourcetype`, `getetag`,
  `sync-token`, `owner`, `current-user-privilege-set`,
  `supported-calendar-component-set`, quota properties, and
  `getctag`) with 403 and `DAV:cannot-modify-protected-property`; the whole
  request still fails atomically (424 for the other properties).
- `docs/installation.md` sections 4a and 6 install the shipped systemd files
  instead of copy-pasted ones and explain readiness, the start limit and how
  to check the service after a reboot.

- `/readyz` now checks that the database can be written, not only read: it
  answers `503` with `Retry-After` and a short reason (`database not
  writable`, `storage full`, `database unavailable`) while the database is
  read-only, a write failed for lack of space, or the database cannot be
  read, and `200` again by itself once writes work. A busy writer (write
  burst, lock held by another process) keeps the last known state. The write probe
  rolls back, changes no data and does not grow the database; results are
  cached for 5 seconds. `/healthz` stays liveness only. New migration
  `0008_health` (one-row table for the probe).

- SSO link status: `GET /api/v1/me` (and the login answer) has a new field
  `sso` (`linked`, `issuer`, `linked_at`, `last_login_at`; `null` when SSO is
  not configured; never the IdP subject), shown on the **Account** page.
  `GET /api/v1/admin/users` has `sso_linked`, shown as an **SSO** badge in
  the administration. New admin endpoint
  `DELETE /api/v1/admin/users/{username}/sso` (button **Unlink SSO**, with
  confirmation) removes a user's SSO binding like `calcraft user unbind-sso`
  and ends the user's sessions; admins cannot unlink themselves. The new
  fields are additive; older clients ignore them.

- Deleting a subscription calendar (DAV, REST API or web UI) removes it
  permanently instead of moving it into the trash; trash entries cannot be
  restored into a subscription.

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

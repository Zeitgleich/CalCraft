# Managed attachments (RFC 8607)

CalCraft can store files attached to calendar objects ("managed
attachments"): a client uploads a file with a `POST` to the event, the
server keeps it and adds an `ATTACH` property with a download URL to the
event. Invitations to other users of the server carry the attachment, so
attendees can download it.

Managed attachments are **off by default**: they let users store arbitrary
files on the server and serve them from its origin. Turn them on only if
you want that.

## Enabling

```toml
[attachments]
enabled = true
# Scheme and host clients reach the server at (no path). Required.
base_url = "https://cal.example.org"
```

| Setting | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Uploads and downloads. |
| `base_url` | unset | Scheme and host of the download URLs written into events (`<base_url>/dav/attachments/<managed-id>`). Never taken from the request's `Host` header. Required when enabled. |
| `max_bytes` | 10 MiB | Largest attachment (1 KiB–100 MiB). Only uploads may be this large; `server.max_body_bytes` still applies to every other request. Reported as `CALDAV:max-attachment-size`. |
| `max_per_object` | 20 | Managed attachments per calendar object, all instances together (1–100). Reported as `CALDAV:max-attachments-per-resource`. |
| `max_bytes_per_user` | 256 MiB | Bytes of attachments one user may own (at least `max_bytes`, at most 1 TiB). |

Attachments also count towards the storage quota of the user who uploaded
them (512 MiB for objects, trash, inbox and attachments together).

Clients find the feature through the `DAV` header of `OPTIONS`
(`calendar-managed-attachments`; recurrence instances are supported, so
`calendar-managed-attachments-no-recurrence` is not sent), the empty
`CALDAV:managed-attachments-server-URL` property of the calendar home (the
attachments live on this server) and the two limits above on every
writable calendar.

## Protocol

| Request | Effect |
|---|---|
| `POST <object>?action=attachment-add` | Stores the body as a new attachment. `Content-Type` becomes `FMTTYPE`, the `Content-Disposition` file name (`filename*` preferred) becomes `FILENAME`. The `ATTACH` goes to every instance (master and overrides), or with `rid=` (comma-separated, `M` for the master, RECURRENCE-ID values as written in the object) only to those; overrides are created for instances that have none yet. `201`, `Cal-Managed-ID`. |
| `POST <object>?action=attachment-update&managed-id=<id>` | Replaces the file in every instance. The attachment gets a **new** managed id and URL (RFC 8607 §3.5), so clients notice the change; the old file is removed once nothing references it. `200`, `Cal-Managed-ID`. |
| `POST <object>?action=attachment-remove&managed-id=<id>` | Removes the `ATTACH` from every instance or the `rid=` instances (an override is created if the master has it). `204`. |
| `GET`/`HEAD /dav/attachments/<id>` | Downloads the file (see below). Every other method: `405`. |

The response carries the object's new `ETag`, and `Schedule-Tag` for
scheduling objects; for scheduling objects the server changed (statuses)
there is no ETag, as for a PUT. `If-Match` on the object is honoured.
`Prefer: return=representation` is not supported: clients read the object
again.

The changed object goes through exactly the path of a PUT: validation,
quota, ETag, Schedule-Tag and implicit scheduling. An organizer's change
of an attachment therefore sends an update to the attendees.

Errors (`DAV:error` bodies, RFC 8607 §3.7):

| Condition | Status |
|---|---|
| `CALDAV:valid-action` – no or an unknown `action`, or `action` twice | 403 |
| `CALDAV:valid-managed-id` – `managed-id` missing, or not on the targeted components | 403 |
| `CALDAV:valid-rid` – malformed `rid`, an instance the series does not have, or `rid` with `attachment-update` | 403 |
| `CALDAV:max-attachment-size` – body larger than `max_bytes` (checked against `Content-Length` before reading and while streaming) | 403 |
| `CALDAV:max-attachments-per-resource` | 403 |
| `CALDAV:max-resource-size` – the object would get more than 200 instance components | 403 |
| `DAV:quota-not-exceeded` – storage quota or `max_bytes_per_user` | 507 |
| `DAV:need-privileges` – a subscription, or the user is an attendee (only the organizer may change attachments, RFC 8607 §3.6) | 403 |
| `CALDAV:valid-managed-id-parameter` – a PUT with a managed id it may not reference | 403 |

`managed-id` together with `attachment-add` and an empty body are `400`.

### PUT

Clients may keep, drop and reorder managed `ATTACH` properties when they
write an object. On every PUT the server checks each `MANAGED-ID`:

* the user's own attachments, and attachments the object already
  references (an attendee keeping the organizer's file), are allowed; their
  `ATTACH` properties are rewritten to the server's form (URL, `SIZE`,
  `FILENAME`, `FMTTYPE`), so a client cannot redirect a managed attachment
  to another host or rename it – the response then has no `ETag`
  (RFC 4791 §5.3.4);
* a managed id the stored version of the object already mentions without
  referencing it (e.g. an imported event from another server) stays as it
  is: it is plain text and grants nothing;
* anything else – an unknown id, or another user's id or URL copied into
  an own event – is refused with `CALDAV:valid-managed-id-parameter`.

Dropping an `ATTACH` removes the reference; the file is deleted once
nothing references it any more.

## Access model

Downloads need authentication (the DAV credentials: account password
without 2FA, or an app password). A user may download an attachment if

* they uploaded it, or
* one of their **live** calendar objects references it.

References are rows in the database (`attachment_refs`) that only the
store writes, when an object is stored, after checking that the object may
reference the attachment: the owner's own attachments, attachments the
object referenced before, and – for scheduling deliveries – attachments of
the organizer's object the copy is derived from, checked in the same
transaction. The content of an object never grants anything by itself:
writing someone else's managed id or URL into an own event, an import, a
subscription feed or a COPY of an invitation gives no access.

Everything else is answered with `404`, byte for byte like a managed id
that never existed (`tests/authz_matrix.rs`).

**Scheduling.** Invitations to users of this server reference the
organizer's attachment, so attendees can download it; they cannot add,
update or remove attachments (`DAV:need-privileges`). Access ends when the
attendee no longer has a live copy that references the file: the organizer
removes the attachment or the attendee (cancellation: a kept, cancelled
copy loses its managed `ATTACH` properties), deletes the event (the
untouched copy is removed, an answered one is cancelled), or the attendee
deletes their copy. An attendee's copy in the trash keeps no reference to
the organizer's file, and restoring it does not bring the access back.
Inbox messages are notifications only and grant nothing.

The alternative, unguessable capability URLs without authentication,
would let anyone who ever saw the URL (forwarded invitations, logs,
proxies, a cancelled attendee) download the file forever. The
authenticated model keeps access tied to the current state of the
calendars; the price is that clients must authenticate for downloads,
which DAV clients do anyway (same server, same realm).

**Downloads** are always sent as `Content-Disposition: attachment` (an
ASCII fallback plus the exact name as RFC 8187 `filename*`), with
`X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src
'none'; sandbox`, `Cache-Control: private, no-store` and the global
security headers (`Cross-Origin-Resource-Policy: same-origin`,
`X-Frame-Options: DENY`). `Content-Type` is the stored media type only if
it is on a short allowlist (PDF, office documents, plain images, plain
text, audio/video); HTML, SVG, XML, scripts and anything unknown go out as
`application/octet-stream`. Nothing is ever rendered on the server's
origin. The ETag is derived from the managed id (attachments never
change), so `If-None-Match` gets `304`. Range requests are not supported
(`Accept-Ranges: none`).

**File names** are sanitised before they are stored (RFC 6266 §4.3): only
the last path component, no control, bidirectional-override or invisible
characters, no quotes, `:`, `*`, `?`, `<`, `>`, `|`, no leading dots or
surrounding blanks, at most 255 bytes; `attachment` if nothing is left.
Header values the server sends are visible ASCII only, so a file name can
never inject a header line.

## Storage and lifecycle

Attachments are rows in the SQLite database (`attachments`, migration
`0012_attachments`), not files on disk: backups (`calcraft backup`,
`VACUUM INTO`), restores, pre-migration backups and the trash cover them
without any extra step, and an upload is one transaction.

* An attachment is immutable and owned by the user who uploaded it; its
  managed id (128 random bits) is also its URL.
* It is deleted together with its last reference (a database trigger), however
  that goes away: PUT without the `ATTACH`, `attachment-remove` or
  `-update`, DELETE with the trash disabled, a purged trash entry, a deleted
  collection or user.
* **Trash**: a deleted object keeps its own attachments until the trash
  entry is purged; restoring the object (or its whole calendar) brings them
  back. They count towards the quota meanwhile.
* **MOVE** keeps the references. **COPY** shares the owner's own
  attachments with the copy (one file, counted once in the quota; an
  update through one of them gives that object a new attachment, the other
  keeps the old one); other users' attachments are not carried over.
* An upload that never got referenced (the POST failed or was aborted) is
  removed by the same background task as the trash purge after an hour.

**Concurrency.** The upload is stored first, then the object is changed in
a read–modify–write guarded by its ETag and retried, so parallel uploads
to one object all end up in it (`parallel_uploads_lose_nothing`), and an
upload racing a DELETE leaves nothing behind
(`upload_racing_a_delete_leaves_no_orphans`).

## Limits and abuse

* The body is streamed with a hard limit of `max_bytes` (and refused before
  reading if `Content-Length` is larger); no decompression happens. At most
  4 uploads are held in memory at a time (others wait).
* `max_per_object`, `max_bytes_per_user` and the storage quota bound what
  a user can store; the per-client request limits and the scheduling limits
  (recipients and delivered bytes per hour) apply as usual.
* The server never fetches URLs: external `ATTACH` URLs and inline
  (`VALUE=BINARY`) attachments in imports, migrations and PUTs stay exactly
  as they are.
* Subscriptions (read-only calendars) take no attachments.
* Logs never contain file names, contents or managed ids
  (`tests/log_secrets.rs`).

## Deviations from RFC 8607 and open points

* Attachments are answered with `404` when they are removed or not
  readable, never `410 Gone` (that would reveal that the id existed).
* `Prefer: return=representation` is not supported; redirects (307/308)
  and `102 Processing` are not used.
* No virus scanning (§10 recommends it): the files are served only as
  downloads with the headers above.
* `managed-attachments-server-URL` is always empty: attachments live on
  this server.
* The web UI does not show attachments yet (it has no event views).
* A managed id not known to this server is refused in a PUT unless the
  stored version already mentions it (see *PUT*); RFC 8607 has no rule for
  objects that come from another server.

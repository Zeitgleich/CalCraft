# CalDAV scheduling (RFC 6638)

CalCraft can schedule meetings between users of **the same server**:
invitations, updates, cancellations and replies are delivered by the server
(implicit scheduling, `SCHEDULE-AGENT=SERVER`), and users can look up each
other's free/busy time. Stage B (iMIP: invitations by e-mail to and from
other servers) is not implemented yet; any address outside this server is
answered with `SCHEDULE-STATUS` 5.3.

Scheduling is **off by default**.

## Enabling

```toml
[scheduling]
enabled = true
# Optional: every user gets the address mailto:<username>@example.org.
domain = "example.org"
```

Without `domain`, users can only be invited by their principal URL or by
an e-mail address an administrator set:

```sh
calcraft user set-email alice alice@example.com
calcraft user set-email alice --clear
calcraft user add bob --email bob@example.com
```

or in the web UI (*Administration → E-mail…*) and the REST API
(`PATCH /api/v1/admin/users/{username}` with `{"email": "…"}`, `""`
removes it; `email` in `POST /api/v1/admin/users`).

| Setting | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Implicit scheduling, inboxes, outboxes, free/busy. |
| `domain` | unset | Derived addresses `mailto:<username>@<domain>`. |
| `free_busy` | `true` | Free/busy lookups between users (busy periods only). |
| `max_attendees` | 100 | Distinct attendees per scheduled object (1–1000); more: 403 `CALDAV:max-attendees-per-instance`. |
| `max_recipients_per_hour` | 1000 | Recipients one user may reach per hour (invitations, cancellations, replies, free/busy lookups; 1–100 000). Beyond that: 5.1, the sender's own object is still stored. |
| `inbox_max_messages` | 1000 | Messages kept per inbox (10–100 000); the oldest are dropped. |
| `max_freebusy_days` | 366 | Longest free/busy range (1–1830); longer: 403. |

Clients find everything through the principal: `calendar-user-address-set`,
`calendar-user-type` (`INDIVIDUAL`), `schedule-inbox-URL`
(`/dav/scheduling/<user>/inbox/`) and `schedule-outbox-URL`
(`/dav/scheduling/<user>/outbox/`); the DAV header contains
`calendar-auto-schedule`. Tested with python-caldav (CI job *client
compatibility*); Apple Calendar, Thunderbird and DAVx⁵ are not yet tested.

## Calendar user addresses

A user's addresses are, in this order:

1. the e-mail address an administrator set (optional),
2. `mailto:<username>@<domain>` if `scheduling.domain` is set,
3. the principal URL (`/dav/principals/<username>/`, also as absolute URL).

Addresses are compared case-insensitively (ASCII; internationalised
addresses are not accepted). E-mail addresses are unique (database
constraint, stored in lower case). Within the scheduling domain only a
user's own derived address can be set, so an administrator cannot hand
one user's address to another; should an address still match two users
(e.g. after changing `domain`), it matches nobody and a warning is logged.

**Addresses are never taken from SSO.** An e-mail claim of the identity
provider can be changed by whoever controls the IdP account (or the IdP);
using it would let that person receive another user's invitations or reply
in their name. OIDC accounts are linked by issuer and subject only, and the
scheduling address of an SSO user is set by an administrator like for
everybody else.

## Who may schedule

* **Organizer**: a PUT schedules only if the ORGANIZER is one of the
  authenticated user's own addresses. An object with somebody else as
  ORGANIZER is stored exactly as sent and schedules nothing (unless the user
  is an attendee, see below). Spoofing an organizer therefore reaches
  nobody: no copy, no inbox message (`tests/scheduling.rs`,
  `organizer_spoofing_reaches_nobody`).
* **Attendee**: if somebody else is ORGANIZER and the user is an ATTENDEE,
  the server sends a REPLY when the user's own `PARTSTAT` changes. The
  organizer's copy only changes if it is a scheduling object of the
  organizer with that UID that lists the attendee. Merging changes nothing
  but the attendee's own `PARTSTAT`: not the summary, times, other
  attendees or anything else (`attendee_cannot_reply_for_others_or_add_instances`,
  fuzz target `itip`).
* **Deliveries never overwrite foreign objects**: an invitation updates the
  recipient's object with that UID only if it has the same organizer;
  otherwise (another organizer's event, the recipient's own event) the
  delivery is refused with 5.1.
* Only one scheduling object per UID and user: a second one in another
  calendar gets 403 `CALDAV:unique-scheduling-object-resource`.
* `SCHEDULE-AGENT=CLIENT` and `NONE` are respected; unknown values count
  as `NONE`. `SCHEDULE-FORCE-SEND` (`REQUEST` on an attendee, `REPLY` on the
  organizer) sends even without a change and is never stored.
* `SCHEDULE-STATUS` is set by the server. Values sent by clients are
  replaced (they are kept only for attendees with `SCHEDULE-AGENT=CLIENT`).

## What happens

| Action | Effect |
|---|---|
| Organizer PUT, new or changed for an attendee | REQUEST: the attendee's copy is created in their default calendar (`schedule-default-calendar-URL`: the writable calendar `default`, else the first writable one for the component) or updated; their alarms are kept. Inbox message. `SCHEDULE-STATUS` 1.2. |
| Organizer PUT, attendee removed | CANCEL: the attendee's copy is marked `STATUS:CANCELLED` (kept, so the attendee sees it). |
| Organizer PUT without a relevant change | Nothing is sent. Relevant are all properties except `DTSTAMP`, `LAST-MODIFIED`, `CREATED`, `SEQUENCE`, `X-` properties and the participation status of attendees. |
| Organizer DELETE | CANCEL to every attendee. The object goes to the trash as usual. |
| Attendee PUT with a new `PARTSTAT` | REPLY: the organizer's copy gets the new status; the organizer's Schedule-Tag stays. `SCHEDULE-STATUS` on the attendee's ORGANIZER property. |
| Attendee DELETE | REPLY `DECLINED` (not for cancelled events). |

Each attendee receives only the components that list them: instances of a
series they are not invited to are excluded with EXDATE. A reply for a
single instance (an override with `RECURRENCE-ID`, or an EXDATE the
attendee's client added) adds an override to the organizer's copy – only if
the organizer's series really has that instance (checked with the
recurrence engine within its budget; made-up instances are ignored).

**Status codes**: 1.2 delivered; 3.7 unknown user of this server's domain
or principal URL, also disabled users; 3.8 reply refused (no such event of
the organizer with this attendee); 5.1 not delivered (recipient's quota or
limits, rate limit, conflicting object); 5.3 not deliverable (address
outside this server – iMIP is stage B – or the recipient has no writable
calendar).

**Schedule-Tag** (RFC 6638 §3.2.10): returned on PUT and GET of scheduling
objects; `If-Schedule-Tag-Match` on PUT and DELETE. Merged replies do not
change the organizer's tag, so an organizer's client writing with
`If-Schedule-Tag-Match` succeeds after replies and the replies are kept
(the server keeps the recorded `PARTSTAT` of attendees it schedules, unless
the organizer resets them to `NEEDS-ACTION` together with a relevant
change). Because the server changes scheduled objects (statuses), their PUT
response has no ETag (RFC 4791 §5.3.4); clients read the object again.

**Concurrency**: every change of an existing object is a
read–modify–write guarded by its ETag in one `BEGIN IMMEDIATE` write and
retried on conflicts; two attendees replying at the same time both end up
in the organizer's copy (`concurrent_replies_lose_nothing`).

## Inbox and outbox

The inbox holds iTIP messages (REQUEST, CANCEL, REPLY) as notifications:
the server has already applied them to the calendars. A newer message from
the same sender about the same UID replaces the older one, and beyond
`inbox_max_messages` the oldest are dropped. Supported: PROPFIND (depth 0
and 1), GET/HEAD, DELETE (final: inbox messages never go to the trash) and
REPORT `calendar-multiget` / `calendar-query` (returns all messages). PUT,
PROPPATCH, MKCOL, COPY and MOVE are refused (405; as a COPY/MOVE
destination 403). Messages count towards the recipient's storage quota.

The outbox takes only POST of a free/busy request (`METHOD:REQUEST`,
`VFREEBUSY` with UTC `DTSTART`/`DTEND`, ORGANIZER and up to 100 ATTENDEE):

* only the owner, only with one of their own addresses as ORGANIZER
  (otherwise 403 `CALDAV:organizer-allowed`); another user's outbox is 404
  like any foreign path;
* the answer (`CALDAV:schedule-response`) contains only busy periods
  (`FREEBUSY;FBTYPE=BUSY` / `BUSY-TENTATIVE`), never summaries, UIDs or
  other event data;
* counted: VEVENTs in the recipient's writable calendars, not in
  subscriptions or calendars whose `schedule-calendar-transp` is
  `transparent`, not `TRANSP:TRANSPARENT`, not cancelled and not declined
  by the recipient; tentative events and unanswered invitations are
  `BUSY-TENTATIVE`;
* recurring events are expanded with the recurrence engine (2 000 000
  steps, 2 000 objects, 64 MiB per recipient); objects beyond the budget
  count as busy over their indexed span (never "free" when they might be
  busy). Floating times and all-day events are read as UTC.

## Privacy and enumeration

Only authenticated users can schedule or look up free/busy time. RFC 6638
needs 3.7 for unknown recipients, so an authenticated user can tell whether
an address belongs to a user of this server. This is accepted: the
answers are limited by `max_recipients_per_hour` (shared with invitations),
disabled users look exactly like unknown ones, and nothing beyond
existence and busy periods is revealed. Administrators who do not want
users to see each other's busy times set `free_busy = false` (answer 3.8
for everybody).

Logs contain only user names and counts – never addresses, UIDs or event
data (`tests/log_secrets.rs`).

## Interaction with other features

* **Imports, `calcraft migrate` and subscriptions** never schedule: they
  write through the store, not the DAV PUT path. Deleting an object that
  was never scheduled (no Schedule-Tag, e.g. imported) sends nothing.
* **Trash**: deleting moves the object to the trash and sends the
  cancellation or decline. Restoring does **not** send invitations again
  (and the restored object has no Schedule-Tag); edit it or use
  `SCHEDULE-FORCE-SEND` to invite again. Deleting a whole calendar sends
  nothing.
* **MOVE** keeps an object a scheduling object; **COPY** creates a plain
  copy without Schedule-Tag (deliveries keep updating the original, which
  wins the UID lookup).
* **Subscriptions** are never targets of deliveries and never counted for
  free/busy.
* **Backups** contain the new data (it lives in the same database).

## Deviations from RFC 6638 and open points

* Stage B, iMIP (RFC 6047): no e-mail at all; external addresses get 5.3.
* Cancelling single instances is delivered as an update (REQUEST with the
  new EXDATE/overrides) rather than a CANCEL with RECURRENCE-ID; the
  attendee's copy ends up the same.
* The server does not increment `SEQUENCE` and does not forward one
  attendee's reply to the other attendees.
* `schedule-default-calendar-URL` cannot be changed (PROPPATCH on the
  inbox is refused); `CALDAV:schedule-tag` is a header, not yet a PROPFIND
  property; the inbox has no `sync-collection`.
* POST of VEVENT/VTODO to the outbox (deprecated client-side scheduling) is
  not supported; `CALDAV:free-busy-query` REPORTs on calendars neither.
* Delegation (`DELEGATED-TO`), resources and groups (`CUTYPE` other than
  `INDIVIDUAL`), `SENT-BY` and `SCHEDULE-FORCE-SEND` on cancellations are
  not handled.

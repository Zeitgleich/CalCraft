# Sharing and public calendar links

CalCraft lets users share calendars and address books with other users of
the same server, read-only or read-write, and publish calendars as secret,
read-only links. Sharing is **on by default**, but a share only takes effect
once the sharee accepted it. Public links are **off by default**.

## Configuration

```toml
[sharing]
enabled = true

[publish]
enabled = false
# Scheme and host the links are built with (no path). Required when enabled.
# base_url = "https://cal.example.org"
max_bytes = 8388608     # largest published calendar (64 KiB – 64 MiB)
max_objects = 10000     # most objects of one published calendar (1 – 100 000)
```

| Setting | Default | Meaning |
|---|---|---|
| `sharing.enabled` | `true` | Owners may share; accepted shares appear in the sharees' homes. `false`: no new shares, existing shares are hidden everywhere (DAV, REST API) but stay in the database. |
| `publish.enabled` | `false` | Owners may create public links; `/public/<token>.ics` is served without login. |
| `publish.base_url` | unset | Scheme and host of the links (`<base_url>/public/<token>.ics`, also as `webcal://`). Never taken from the request's `Host` header. Required when enabled. |
| `publish.max_bytes` | 8 MiB | Objects beyond this size of the rendered calendar are left out. |
| `publish.max_objects` | 10 000 | Objects beyond this number are left out. |

## Sharing between users

### Model

* The **owner** shares one of their collections (calendar, address book or
  subscription) with another enabled user, the **sharee**, with `read` or
  `read-write` access. A subscription can only be shared read-only.
* A new share is an **invitation**: nothing appears for the sharee until
  they accept it (web UI, REST API, or the invitation in Apple Calendar).
  This keeps owners from pushing content into somebody else's calendar
  list. The sharee can decline an invitation, and leave an accepted share
  at any time; both remove the share and never touch the owner's data.
* An accepted share is a collection **in the sharee's own home**:
  `/dav/calendars/<sharee>/<slug>/` (or `/dav/addressbooks/…`). The slug is
  `<owner>-<owner's slug>` (with `-2`, `-3`, … if the sharee has a
  collection of that name). The owner's own path
  `/dav/calendars/<owner>/<slug>/` is never reachable for anybody else,
  shared or not, so every client sees shared collections with a plain
  `PROPFIND Depth: 1` of its home (the bind/alias model of sabre/dav and
  Apple's CalendarServer), and every authorization decision starts from
  the sharee's own home.
* The owner keeps sole control of the collection: deleting and renaming
  it, its description, colour, dead properties and supported components,
  its sharing settings and its public links. Objects never move to another
  owner (no `MOVE`/`COPY` between the owner's and the sharee's
  collections).
* Every query ignores shares of disabled users (owner or sharee): a
  disabled user's shares disappear at once and come back if the account is
  enabled again. Deleting a user or the collection deletes its shares;
  restoring a collection from the trash does not restore its shares.
* Limits: 100 direct shares and 20 group shares per collection; 100
  pending invitations per sharee in total and 5 from any one owner (further invitations are refused until
  they accept or decline some). After a sharee declined or left one of an
  owner's shares, that owner cannot invite them again for 24 hours (REST
  `409`, DAV `403 CS:invite-declined-recently`); the owner removing a share
  blocks nothing. A `CS:share` request with several entries is applied in
  one transaction: if one entry fails, nothing changes.
* Collections can also be shared with a **group** of users (see
  [Groups](#groups)); every member then gets their own invitation. The
  store keeps one row per sharee and collection (`shares` table, migrations
  `0013_sharing` and `0014_groups`), whatever grants the access, so every
  access check reads that one row.

### Groups

Administrators create groups and manage their members (`calcraft group
add|remove|add-member|remove-member|list`, `/api/v1/admin/groups`, the
*Groups* section of the admin page). Group names follow the username rules
(1–64 characters `a-z`, `0-9`, `.`, `_`, `-`). At most `groups.max_groups`
groups (default 100) with `groups.max_members` members each (default 200):

```toml
[groups]
max_groups = 100   # 1 – 10 000
max_members = 200  # 1 – 1 000
```

* **Who sees groups.** Users see the groups they are a member of, with
  their enabled fellow members (account page, `GET /api/v1/me/groups`);
  sharing with the group reaches exactly these people, so members may know
  them. Groups a user does not belong to and their members are never shown
  to them; administrators see all groups and members.
* **Who shares with a group.** An owner shares one of their collections
  with a group they are a **member** of (administrators: any group),
  `read` or `read-write`, in the web UI (*Shared with groups* in the share
  panel), with `PUT /api/v1/collections/{kind}/{slug}/group-shares/{group}`
  or with Apple's `CS:share` (below). For everybody else a group does not
  exist: sharing with a group one is not a member of answers exactly like
  an unknown group (`404`, DAV `403 CS:valid-sharee`), so group names cannot
  be probed. Ending a group share needs no membership. At most 20 group
  shares per collection.
* **Every member gets their own invitation**, like a direct share: nothing
  appears until they accept, they decline or leave on their own, and their
  name, colour and position of the collection are their own. The owner is
  never invited to their own collection.
* **Joining** a group invites the new member to every collection shared
  with the group. **Leaving** (an administrator removes the member) ends
  the access at once, in the same transaction, with exactly the semantics
  of a revoked share: writes in flight fail with `404`, and every path of
  the share, `sync-collection` and attachment downloads answer like paths
  that never existed. **Deleting a group** ends all its shares the same
  way; ending a group share does it for all members.
* **Direct share and group share together** (or several groups): the
  member has one share (one invitation, one path) with the **higher**
  access of all grants. Removing one grant keeps the others: removing the
  direct share of a member of a shared group leaves the group's access (the
  share stays accepted), ending the group share leaves the direct one, and
  only when no grant is left does the share end. `DELETE …/shares/{username}`
  removes only a direct share (`404` for a member who has access only
  through groups); the owner's share list shows for each sharee their
  direct access and the groups that grant access. Leaving (or declining)
  ends all grants at once.
* **Spam limits apply per member.** An invitation through a group counts
  against the same limits as a direct one: 5 pending invitations from one
  owner and 100 in total per sharee, and no invitation within 24 hours
  after the member declined or left one of the owner's shares. A member
  who cannot be invited now is **skipped** (the answer says how many);
  saving the group share again later, or a change of the group's members,
  invites them then.
* **Declines are remembered per member.** A member who declines a group
  share's invitation, or leaves the collection, is never invited by that
  group share again – not when the owner saves it again, and not after
  leaving and rejoining the group. A new group share (after ending the old
  one) is subject to the 24-hour block like a new direct share.
* **Disabled users** are excluded like everywhere: a disabled member's
  shares are hidden and come back when the account is enabled again; a
  disabled owner's group shares are hidden for all members. Disabled users
  can be members (and get their invitations, hidden while disabled).
* Groups are not principals of DAV (no `DAV:group-membership`,
  `group-member-set` or group principal resources), and scheduling does not
  expand groups: inviting a group to a meeting is not supported (there are
  no group calendar user addresses). The CLI never prints group names or
  usernames of a change, and group names are never logged (REST paths with
  a group name are logged without it).

### Rights

| | Owner | Read-write sharee | Read sharee | Anybody else |
|---|---|---|---|---|
| See the collection (`PROPFIND`, `GET`, `REPORT`, `sync-collection`, multiget, query) | ✓ | ✓ | ✓ | 404 |
| Create, change, delete objects (`PUT`, `DELETE`) | ✓ | ✓ (into the owner's trash) | 403 | 404 |
| Add/remove managed attachments (`POST ?action=…`) | ✓ | ✓ (owned by the owner) | 403 | 404 |
| Download managed attachments of its objects | ✓ | ✓ | ✓ | 404 |
| `MOVE`/`COPY` within the collection | ✓ | ✓ | 403 | 404 |
| `MOVE`/`COPY` to or from collections of another owner | – | 403 | 403 | 404 |
| Own name, colour and position (`displayname`, `calendar-color`, `calendar-order`) | ✓ (for everybody) | ✓ (only for themselves) | ✓ (only for themselves) | 404 |
| Other properties (description, time zone, dead properties) | ✓ | 403 | 403 | 404 |
| Delete the collection | ✓ (into the trash, ends all shares) | leaves the share | leaves the share | 404 |
| Share, change or end shares, public links | ✓ | 403 | 403 | 404 |
| Change scheduling objects (the owner's meetings and invitations), also by MOVE/COPY over them or MOVE away | ✓ | 403 | 403 | 404 |
| Details of private events (`CLASS:PRIVATE`/`CONFIDENTIAL`) | ✓ | ✓ | busy stub | 404 |
| Attachments of private events | ✓ | ✓ | 404 | 404 |

* `current-user-privilege-set` says the same: owners and read-write
  sharees get `read`, `write`, `write-properties`, `write-content`, `bind`,
  `unbind`; read sharees `read` and `write-properties` (their own name and
  colour) on the collection and `read` on its objects.
* Objects, ETags, sync tokens and `getctag` are the owner's: a client of
  the sharee and one of the owner see the same ETag for the same object,
  and the same sync token (only the hrefs differ). The sharee's own name
  and colour do not change the `getctag`.
* What sharees write counts against the **owner's** quota (objects, trash,
  attachments); what they delete goes into the owner's trash.
* The sharee does not see the owner's dead properties (client settings like
  default alarms, order, colours of other clients), only
  `CALDAV:calendar-timezone`; their own `calendar-order` is stored with
  the share. A PROPPATCH by a sharee of anything but their own name,
  colour and position is refused (`403`, nothing is changed).
* Writes of a sharee are checked again **inside the write transaction**: if
  the owner removes the share (or downgrades it, or a user is disabled)
  while the request is in flight, the write fails with `404` and changes
  nothing. From then on every path of the share answers exactly like a
  path that never existed in the sharee's home, including
  `sync-collection` and attachment downloads.

### Private events and read-only sharees

Read-only sharees see events whose class is not `PUBLIC` (`PRIVATE`,
`CONFIDENTIAL`, any other value; an override without its own `CLASS`
inherits the master's) as **busy stubs**. A stub keeps exactly `UID`,
`DTSTART`, `DTEND`, `DURATION`, `DUE`, `RRULE`, `RDATE`, `EXDATE`,
`RECURRENCE-ID`, `STATUS`, `TRANSP`, `CLASS`, `DTSTAMP`, `SEQUENCE` and
gets `SUMMARY:Busy`; everything else (description, location, organizer and
attendees, attachments, alarms, URL, conference, related-to, `X-`
properties, …) is removed, also the VCALENDAR's own properties except
`VERSION`, `PRODID`, `CALSCALE` (a whitelist). Public components of the
same object stay as they are.

* The stub is what the sharee gets everywhere: `GET`/`HEAD`, `getetag`
  and `getcontentlength` in `PROPFIND`, multiget, `calendar-query` and
  `sync-collection`. Its ETag is a hash of the stub's bytes: it changes
  when the stub changes (new time, status, …) and stays the same when only
  hidden details change, so it reveals nothing of them. (Such a change
  still appears in `sync-collection`, with the unchanged ETag.)
* Queries: time ranges are evaluated on the times the stub keeps, so the
  results are the same as for the owner; `text-match`, `prop-filter` and
  `param-filter` are refused for every collection
  (`CALDAV:supported-filter`), so there is no way to probe hidden text.
* Managed attachments of private objects are not readable by read-only
  sharees (like an id that was never issued). The store keeps a flag per
  object (any CLASS line other than `CLASS:PUBLIC`, computed by a database
  trigger from the unfolded content, conservatively: a `CLASS` with
  parameters counts as private).
* Read-write sharees and the owner see everything; free/busy is unchanged.

### Scheduling

* A sharee's `PUT` and `DELETE` are stored **without implicit scheduling**:
  no invitations, updates or cancellations are ever sent in the owner's
  name (or the sharee's), whatever ORGANIZER and ATTENDEE say. A sharee's
  copy is a plain object without Schedule-Tag.
* Objects the owner's scheduling manages (the owner's meetings and the
  invitations the owner received, i.e. objects with a Schedule-Tag) cannot
  be changed or deleted by sharees (`403 DAV:need-privileges`), also not
  their attachments, not moved away and not replaced by a MOVE or COPY:
  otherwise attendees would never learn about the change. The check is
  part of the write transaction (no Schedule-Tag on the object when it is
  written).
* Free/busy answers for the owner contain the owner's own calendars only;
  calendars shared with somebody never count for the sharee. The default
  calendar for invitations (`schedule-default-calendar-URL`) is always one
  of the user's own calendars. Inbox and outbox are never shared.

### Attachments

Managed attachments of objects in a shared collection can be downloaded by
the sharees as long as the share exists (accepted, both users enabled):
access derives from the reference of a live object in the shared
collection, never from the content. Read-write sharees add and remove
attachments; uploads are owned by (and counted against) the owner. A
sharee cannot pass the attachment on: writing its managed id into an own
event grants nothing (the existing reference rules of
[attachments.md](attachments.md)).

### Apple Calendar (CalendarServer sharing)

CalCraft implements the parts of Apple's CalendarServer sharing
(`caldav-sharing`, the model of `draft-pot-webdav-resource-sharing`) that
macOS and iOS Calendar use to share and to accept:

* `DAV: …, calendarserver-sharing` in `OPTIONS`; `CS:allowed-sharing-modes`
  (`CS:can-be-shared`) on the owner's calendars.
* `POST` of `CS:share` (`CS:set` with `CS:read`/`CS:read-write`,
  `CS:remove`) to an owned calendar or address book. Sharees are named by a
  calendar user address (`mailto:` of the e-mail address an administrator
  set, or `mailto:<user>@<scheduling.domain>` with scheduling on) or by
  their principal URL (`/dav/principals/<user>/`). Unknown, disabled users
  and the owner are refused with `403 CS:valid-sharee`, and the request
  changes nothing. Sharees cannot share (`403`).
* Groups are named by a **group principal URL**,
  `/dav/principals/groups/<group>/` (also as absolute URL), in `CS:set`
  and `CS:remove`. Only groups the owner is a member of (administrators:
  any) are accepted; others are refused like unknown sharees
  (`403 CS:valid-sharee`, identical answers). The URL is only understood in
  share requests; it is not a DAV resource (`PROPFIND` answers `404`), and
  `CS:invite` lists the members' shares, not the group.
* `CS:invite` on the owner's collection lists all sharees (principal URL,
  name, `invite-accepted`/`invite-noresponse`, access); `DAV:resourcetype`
  has `CS:shared-owner` while it is shared.
* `CS:notification-URL` on the principal; the notification collection
  `/dav/notifications/<user>/` (`CS:notification` resource type) has one
  `CS:invite-notification` per pending invitation (`PROPFIND`, `GET`;
  `DELETE` declines).
* `POST` of `CS:invite-reply` (`CS:invite-accepted` or
  `CS:invite-declined`, with `CS:in-reply-to` or `CS:hosturl`) to the
  sharee's calendar or address book home; acceptance answers with
  `CS:shared-as` and the collection's URL in the sharee's home.
* On the sharee's side the collection has `CS:shared` in its resource
  type, the owner as `DAV:owner`, and a `CS:invite` with the owner
  (`CS:organizer`) and the sharee only (sharees do not learn of each
  other).

Not implemented (documented deviations): `CS:shared-url` (the owner's URL
is not reachable for sharees), shares with external e-mail addresses (no
iMIP) or with `mailto:` addresses of groups (groups have none), `CS:invite-reply` notifications back to the owner
(the owner sees the state in `CS:invite`), sync-collection on the
notification collection, and the IETF `DAV:share-resource` POST of
`draft-pot-webdav-resource-sharing`. Sharing has not been tried with a real
Apple client yet; DAVx⁵, Thunderbird and other clients cannot create
shares but see accepted ones like any other collection.

### REST API and web UI

| Route | Who | |
|---|---|---|
| `GET /api/v1/collections/{kind}/{slug}/shares` | owner | shares of a collection |
| `PUT /api/v1/collections/{kind}/{slug}/shares/{username}` | owner | share (`{"access": "read"\|"read-write"}`) or change access |
| `DELETE /api/v1/collections/{kind}/{slug}/shares/{username}` | owner | end a direct share or withdraw the invitation (group shares stay) |
| `GET /api/v1/collections/{kind}/{slug}/group-shares` | owner | group shares of a collection |
| `PUT /api/v1/collections/{kind}/{slug}/group-shares/{group}` | owner (member of the group, or admin) | share with a group (`{"access": …}`), change access, invite members not invited yet |
| `DELETE /api/v1/collections/{kind}/{slug}/group-shares/{group}` | owner | end a group share |
| `GET /api/v1/me/groups` | user | own groups with their enabled members |
| `GET`/`POST /api/v1/admin/groups`, `GET`/`DELETE /api/v1/admin/groups/{group}` | admin | list, create, show (with members), delete groups |
| `PUT`/`DELETE /api/v1/admin/groups/{group}/members/{username}` | admin | add or remove a member |
| `GET /api/v1/shared` | sharee | invitations and accepted shares |
| `POST /api/v1/shared/{id}/accept` | sharee | accept |
| `DELETE /api/v1/shared/{id}` | sharee | decline or leave |
| `GET /api/v1/admin/shares` | admin | who shares what with whom (never content) |

Somebody else's collection or share answers exactly like a missing one
(404). The web UI shows a *Share* panel per collection and a *Shared with
me* list on the overview.

Knowing usernames: sharing with a user tells the owner whether that user
exists and is enabled (404 otherwise), like scheduling does for calendar
user addresses. Only authenticated users can ask.

## Public read-only links

* An owner creates links for a calendar in the web UI or with `POST
  /api/v1/collections/calendar/{slug}/publish` (at most 10 per calendar).
  The response is the only time the URL is shown: the token (256 random
  bits, 64 hex digits) is stored as a BLAKE3 hash only. `DELETE …/publish/{id}`
  revokes a link at once; deleting the calendar deletes its links.
* `GET`/`HEAD <base_url>/public/<token>.ics` without login. Unknown,
  revoked or malformed tokens, links of disabled owners and a disabled
  feature all get the same `404` as any unknown path (byte-identical). The
  lookup goes by the hash (a database index); the stored hash is compared
  again in constant time.
* **Rate limit:** a client that tries 30 unknown tokens within 10 minutes
  gets `429` for every further miss until the window ends (on top of the
  general per-client request limits). Valid links are looked up first and
  always served, so other users behind the same address keep their links.
  The path is read raw (not decoded): malformed escapes are misses like
  any unknown token, never a `400`.
* **Logs:** trace spans record `/public/` only, never the token; tokens are
  never logged (checked by `tests/log_secrets.rs`).
* **Served** as `text/calendar; charset=utf-8` with `Content-Disposition:
  inline; filename="calendar.ics"`, `X-Content-Type-Options: nosniff`,
  `Content-Security-Policy: default-src 'none'; sandbox`, `Cache-Control:
  private, max-age=300` (no shared caches) and an `ETag` (`304` for
  `If-None-Match`); the ETag changes with every change of the calendar.

What is published (rendered from the stored objects, no recurrence
expansion):

* Only `VEVENT`s (no tasks or journal entries) plus the `VTIMEZONE`s they
  need, the calendar's name as `NAME`/`X-WR-CALNAME`.
* Full events are a whitelist: `SUMMARY`, `DESCRIPTION`, `LOCATION`,
  `DTSTART`, `DTEND`, `DURATION`, `RRULE`, `RDATE`, `EXDATE`,
  `RECURRENCE-ID`, `STATUS`, `TRANSP`, `CLASS`, `CATEGORIES`, `DTSTAMP`,
  `SEQUENCE`, `CREATED`, `LAST-MODIFIED`, `GEO`.
* **Never:** alarms (`VALARM`, they would ring for every subscriber),
  attachments (`ATTACH`; managed attachments are not public), `RELATED-TO`,
  `URL`, `CONFERENCE`, `CONTACT`, `COMMENT`, `IMAGE`, `X-` properties, the
  original UIDs (replaced by a hash per link, so links cannot be
  correlated).
* An override that is left out (private and hidden, or free/cancelled in a
  busy-only link) becomes an `EXDATE` of the master, so the master's
  original instance does not show up in its place; a master that is left
  out takes its whole series along. Overrides without `CLASS` inherit the
  master's.
* **Organizer and attendees** (`ORGANIZER`, `ATTENDEE` – e-mail addresses)
  only if the owner ticks *include attendees* (default off).
* **Busy only** links have nothing but the times (`DTSTART`, `DTEND`,
  `DURATION`, recurrence rules and dates) and `SUMMARY:Busy`; free
  (`TRANSP:TRANSPARENT`) and cancelled events are left out.
* **Private events** (`CLASS:PRIVATE`, `CLASS:CONFIDENTIAL`, or any class
  other than `PUBLIC`): as busy blocks (default), left out, or shown like
  the others – chosen per link.

## Privacy defaults

* Sharing needs acceptance; sharees can decline and leave at any time.
* Sharees do not see each other; the owner sees all sharees. Group members
  see their fellow members, nobody else sees a group or its members
  (except administrators).
* Sharees do not see the owner's client settings (dead properties).
* `CLASS:PRIVATE`/`CONFIDENTIAL` events are busy stubs for read-only
  sharees (see above) and busy blocks in public links by default;
  read-write sharees see them in full (they can edit them anyway). Apple's
  `X-CALENDARSERVER-ACCESS` is not interpreted.
* Public links publish no attendee addresses, attachments or alarms unless
  stated above.

## Tests

`crates/calcraft-server/tests/sharing.rs` (read and read-write enforcement
for every DAV method, the Apple share and invite-reply flow, REST API,
revocation indistinguishable from never-existing paths incl.
`sync-collection` and attachments, disabled users, sharing switched off,
owner's quota and trash, scheduling rules, attachments, a share removed
while the sharee writes), `tests/publish.rs` (defaults, configuration,
headers, ETag/304, every link option, uniform 404, revocation, disabled
owners, the miss limit), the authorization matrix (`tests/authz_matrix.rs`:
shared read, shared read-write, pending, revoked and not shared
collections against every method, destination and credential; the new
REST routes), `tests/log_secrets.rs` (share requests, sharee writes, link
tokens, group names), `tests/groups.rs` (group management and the CLI,
each member's invitation, joining and leaving incl. `sync-collection` and
attachments, direct and group shares together, deleting groups,
remembered declines, limits, disabled users, who may share with a group,
`CS:share` with group principals, a member removed while writing; the
matrix also covers group shares: accepted read and read-write, pending,
former member, non-member, ended), store tests (`sharing/tests.rs`,
`groups/tests.rs`, `publish/tests.rs`, the `0014` upgrade), parser tests
and the `dav_xml` fuzz target (sharing bodies), and the python-caldav
compatibility job (`tests/compat/sharing_client.py`).

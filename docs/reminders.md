# E-mail reminders

CalCraft can send an e-mail when an alarm (`VALARM`) of an event or task
fires. Mails go **only to the calendar owner's own address** – never to the
attendees listed in an alarm or an event. Reminders are **off by default**
and need an SMTP server (`[smtp]`).

## Setup

1. Create a password file that only the CalCraft user can read:

   ```sh
   install -m 600 -o calcraft /dev/null /etc/calcraft/smtp-password
   printf '%s\n' 'the-smtp-password' > /etc/calcraft/smtp-password
   ```

   (or use systemd `LoadCredential=` / Docker secrets, which are files too).

2. Configure the server:

   ```toml
   [smtp]
   enabled = true
   host = "smtp.example.org"
   tls = "starttls"          # or "implicit" (port 465)
   username = "calcraft@example.org"
   password_file = "/etc/calcraft/smtp-password"
   from = "calcraft@example.org"

   [reminders]
   enabled = true
   # Link to the web UI in the mails (optional).
   base_url = "https://cal.example.org"
   ```

3. Give users an address (reminders cannot be sent without one):
   `calcraft user set-email alice alice@example.org`, *Administration →
   E-mail…* in the web UI, or `scheduling.domain` for `<user>@<domain>`
   (see [scheduling.md](scheduling.md#calendar-user-addresses) – the same
   addresses, never taken from SSO).

4. Check the settings: `calcraft smtp test --to-self alice` sends one test
   message right away to alice's own address (the address is not printed).
   Users can also send themselves a test mail from *Settings → E-mail
   reminders*.

### Common providers

| Provider | `host` | `tls` | Notes |
|---|---|---|---|
| Your own Postfix/Exim on the same host | `localhost` | `none` | Only for a relay on this machine, without `username`. |
| Mailbox at a hoster (e.g. Strato, IONOS, Hetzner, mailbox.org, Posteo) | the provider's SMTP host | `starttls` (587) or `implicit` (465) | `from` must usually be the mailbox address. |
| Microsoft 365 / Outlook.com | `smtp.office365.com` | `starttls` | SMTP AUTH must be enabled for the mailbox; OAuth is not supported. |
| Google Workspace / Gmail | `smtp.gmail.com` | `starttls` or `implicit` | Needs an app password (2-step verification). |
| Transactional services (Mailgun, Postmark, Amazon SES, Brevo, …) | the service's SMTP host | `starttls` or `implicit` | Use SMTP credentials of the service; verify the sender domain (SPF/DKIM). |

Without SPF/DKIM for the `from` domain, mails may end up as spam.

## Settings

| `[smtp]` | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Outgoing mail. Only used by reminders today (later also iMIP). |
| `host` | – | Mail server; the TLS certificate must be valid for this name. |
| `port` | 587 / 465 / 25 | Default by `tls`. |
| `tls` | `starttls` | `starttls` (mandatory: no mail if the server does not offer it), `implicit`, or `none` – **insecure**, only allowed for `localhost`/`127.0.0.1`/`::1` and without `username`. |
| `username` | – | `AUTH PLAIN` (or `LOGIN`), only ever over TLS. |
| `password_file` | – | Required with `username`. First line of a regular file with mode `600` (checked; links, devices and files readable by others are refused). The password cannot be put into the configuration file or an environment variable. |
| `from` | – | Sender address (`From:` and envelope). |
| `from_name` | `CalCraft` | Display name of the sender. |
| `helo_name` | domain of `from` | Name sent with `EHLO`. |
| `ca_file` | – | Additional trusted CA certificates (PEM), e.g. for an internal relay; the system trust store is always used. |
| `timeout_secs` | 30 | Deadline of every single step (5–300). |
| `max_per_minute` | 60 | Mails per minute of the whole server (1–10 000). |

| `[reminders]` | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Needs `[smtp] enabled = true`. |
| `default_mode` | `email` | For users who did not choose: `email` (only alarms with `ACTION:EMAIL`), `all` (also display and sound alarms, which most apps create) or `off`. |
| `max_per_user_per_hour` | 50 | Mails per user and hour (1–1000), including test mails. |
| `max_per_user_per_day` | 500 | Mails per user and day (hourly limit – 10 000). |
| `max_queue` | 10 000 | Pending mails of all users together (100 – 1 000 000). |
| `max_late_minutes` | 30 | Alarms missed by more (server down, mail server unreachable) are not sent any more (1–1440). |
| `base_url` | – | Scheme and host of the web UI for a link in the mails. |

Each user chooses in *Settings → E-mail reminders* (or `PATCH
/api/v1/me/reminders`): *Server default*, *No alarms*, *E-mail alarms only*
or *All alarms*, and whether the event's **description** is included (off
by default: descriptions often hold meeting links, phone numbers or notes
that do not belong into a mail). The page also shows the address used and
the mails of the last seven days (sent, waiting, failed, not sent because
of a limit) with the newest problem as a short reason – never the mail
server's reply.

Administrators see the queue under *Administration → Reminder e-mails*
(`GET /api/v1/admin/reminders`): counts of the last 24 hours, objects with
upcoming alarms, the last successful delivery and the last mail server
problem as a code.

## What triggers a mail

* `VALARM`s of events and tasks in the user's **own calendars** with
  `ACTION:EMAIL` – and with mode *All alarms* also `DISPLAY` and `AUDIO`.
  Other actions are ignored.
* Triggers relative to the start (`TRIGGER:-PT15M`) or the end
  (`RELATED=END`; `DUE` for tasks) of every instance, or absolute
  (`VALUE=DATE-TIME`, in UTC, once per event); `REPEAT`/`DURATION` (at most
  20 repetitions, at least a minute apart).
* Recurring events with RRULE, RDATE, EXDATE and moved or changed
  instances (overrides bring their own alarms), evaluated with the same
  recurrence engine and work budget as calendar queries. Times with a
  `TZID` use the event's VTIMEZONE (DST-correct); floating times and
  all-day events use the calendar's time zone (`CALDAV:calendar-timezone`,
  set by Apple Calendar and others), or UTC if there is none. Objects that
  cannot be evaluated exactly (budget exhausted, `TZID` without VTIMEZONE)
  get no reminders.
* Skipped: alarms acknowledged at or after their trigger (RFC 9074
  `ACKNOWLEDGED`, Thunderbird's `X-MOZ-LASTACK`), cancelled events,
  completed tasks, several alarms of one instance at the same moment (one
  mail).
* **Not** sent: alarms of subscriptions (the feed author chose them), of
  calendars shared with the user (only the owner gets reminders for their
  calendar, also for objects a sharee wrote), and of disabled users.
  Invitations from other users never carry the organizer's alarms (they are
  removed on delivery), so nobody can plant reminders in someone else's
  calendar.

Changes are picked up within seconds (the index follows the calendar's
revision, so PUT, DELETE, MOVE, COPY, restoring from the trash, imports and
scheduling deliveries all count). **New or changed objects only catch up
on the last five minutes**: importing a calendar with years of past events
sends nothing for them. Alarms missed while the server was down are sent
if they are at most `max_late_minutes` late.

## The mail

Plain text only (quoted-printable UTF-8), no HTML, no images, no tracking:

```
Subject: Reminder: Weekly sync

Weekly sync

When: Mon, 26 Oct 2026, 09:00–10:00 (Europe/Berlin)
Location: Room 1
Calendar: Work

-- 
This reminder was sent by CalCraft for an alarm in your calendar.
Open your calendar: https://cal.example.org/
You can turn reminder e-mails off in your account settings.
```

Times are shown in the event's own time zone. Summaries are cut to 120
characters in the subject; control and invisible characters are removed.
Headers: `Date`, `From`, `To`, `Subject` (RFC 2047 encoded if not ASCII),
a random `Message-ID`, `Auto-Submitted: auto-generated` (no auto-replies)
and `X-Auto-Response-Suppress: All`.

## Delivery

Mails are queued in the database (`mail_outbox`) in the same transaction
that marks the alarm as done, and each reminder has a unique key: a
restart neither loses nor repeats one. (The only window for a duplicate is
a crash between the mail server's `250` and recording it.) The sender
delivers the queue with one connection per run, retries temporary problems
(connection, timeout, TLS, authentication, 4xx) after 1, 5 and 15 minutes,
1 hour and then every 3 hours – until the event starts or
`max_late_minutes` after the alarm, whichever is later, at most 10
attempts – and gives up at once on a
permanent refusal (5xx). A reminder whose event is deleted before it is
sent is dropped. Finished entries lose recipient and message right away and
are deleted after seven days.

Limits: per user per hour and per day (further reminders are recorded as
*not sent* and shown to the user – at most 1000 such records per user and
day, beyond that they are only counted), the queue size of the whole
server, and `smtp.max_per_minute`.

## Security notes

* **Recipients**: only the owner's address set by an administrator (or
  derived from `scheduling.domain`). Addresses in `ATTENDEE` properties of
  alarms or events are never used, so calendar data cannot turn the server
  into a mail relay. The address is validated (ASCII `local@domain`)
  before it is used in an SMTP command or header.
* **Header injection**: subjects and display names are made single-line
  and encoded, addresses validated, the body is quoted-printable (a dot at
  the start of a line is encoded as well), and every message is checked
  (CRLF only, line length, header syntax) before it is queued and again
  before dot-stuffing (`calcraft-core` `mail`, fuzz target `mail`).
* **TLS**: rustls with the system trust store (plus `ca_file`), TLS 1.2+.
  `STARTTLS` cannot be skipped: if the server does not offer it, nothing
  is sent and no credentials leave the host. Data the server sends before
  the TLS handshake is refused (STARTTLS command injection).
* **SMTP replies** are parsed with limits (line length, lines and bytes
  per reply, deadline per step) and never logged or stored; only the reply
  code ends up in the queue as a short reason.
* **Credentials**: the password is read once at startup from a file with
  checked permissions, kept in memory that is wiped when it is dropped, and
  only sent after TLS is established. It is not part of `calcraft
  check-config` output.
* **Logs** contain ids and counts only: no user names, addresses,
  subjects, event data, credentials or server replies
  (`tests/log_secrets.rs`).

## Interfaces

* `GET /api/v1/me/reminders`, `PATCH /api/v1/me/reminders`
  (`{"mode": "off" | "email" | "all" | "default", "include_description":
  bool}`), `POST /api/v1/me/reminders/test` (202, 409 without address)
* `GET /api/v1/admin/reminders` (administrators)
* `calcraft smtp test --to-self <user>`

All answer 404 while reminders are off.

## Limitations

* Language of the mails: English only.
* Floating times and all-day events without a calendar time zone are read
  as UTC.
* No OAuth for SMTP (Microsoft 365 and Google need SMTP AUTH / app
  passwords).
* iMIP (invitations by e-mail, scheduling stage B) is not implemented yet;
  it will use the same SMTP client and queue.

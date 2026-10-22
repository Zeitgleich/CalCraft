<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/logo-wordmark-dark.svg">
    <img src="assets/brand/logo-wordmark.svg" alt="CalCraft logo: a blue calendar page with a white spark" width="320">
  </picture>
</p>

# CalCraft

Self-hosted CalDAV/CardDAV server written in Rust, with an optional web interface.

> **Status:** first pre-release, 0.1.0-alpha.1. Not ready for production use;
> what is still missing is tracked in [docs/produktionsreife.md](docs/produktionsreife.md) (German).

## Goals

- Standards-compliant CalDAV (RFC 4791) and CardDAV (RFC 6352)
- Security first: no `unsafe`, audited dependencies, fuzzed parsers, strict defaults
- Single binary, optional Docker image
- Optional web UI (Leptos) and REST API with OpenAPI – both can be disabled
- Local users or SSO via OpenID Connect (e.g. Keycloak), with per-device app passwords
- Support for iCalendar, jCal, xCal, vCard 3/4, jCard, CSV and webcal subscriptions

See [PLAN.md](PLAN.md) for the roadmap.

## Screenshots

The optional web UI, shown with fictional demo data:

| | |
|---|---|
| ![Calendars and address books overview](docs/screenshots/collections.png)<br>Calendars and address books with their DAV URLs | ![Overview in dark mode](docs/screenshots/collections-dark.png)<br>Dark mode follows the system setting |
| ![Login page](docs/screenshots/login.png)<br>Login with username and password | ![Login page with single sign-on button](docs/screenshots/login-sso.png)<br>Login with single sign-on (OpenID Connect) enabled |
| ![Account settings](docs/screenshots/account.png)<br>Account: password and two-factor authentication | ![App passwords](docs/screenshots/app-passwords.png)<br>One app password per device, revocable individually |
| ![Administration](docs/screenshots/admin.png)<br>Administration: users and update notice | ![Overview on a phone](docs/screenshots/mobile.png)<br>Phone-width layout |

## Building

```sh
cargo build --release
```

Cargo features of `calcraft-server`:

| Feature | Default | Description |
|---|---|---|
| `web-ui` | yes | Embedded web interface |
| `api` | yes | REST API |
| `oidc` | yes | Single sign-on via OpenID Connect (implies `api`; adds an HTTPS client: rustls/ring) |
| `update-check` | yes | Optional update notice for administrators (implies `api`; same HTTPS client; off unless `[updates] check = true`) |
| `migrate` | yes | `calcraft migrate` from another CalDAV/CardDAV server (same HTTPS client) |

CalCraft runs on Linux, macOS and Windows.

### Web UI

The web interface is a Leptos application (`crates/calcraft-ui`) compiled to
WebAssembly. Its files are embedded into the server binary at build time, so
build them first:

```sh
rustup target add wasm32-unknown-unknown                   # listed in rust-toolchain.toml
cargo install wasm-bindgen-cli --version 0.2.114 --locked  # must match crates/calcraft-ui/Cargo.toml
scripts/build-ui.sh                                        # Windows: scripts/build-ui.ps1
cargo build --release
```

Without these assets the server still builds but shows a placeholder page on `/`;
set `CALCRAFT_UI_REQUIRED=1` to make missing assets a build error. No npm or other
JavaScript tooling is needed.

The UI covers login (with two-factor codes), calendars and address books, account
settings (password, TOTP), app passwords and user administration. Disable it at
runtime with `web_ui.enabled = false` or `calcraft serve --no-web-ui`.
Logo, icons and colours: [docs/brand.md](docs/brand.md).

Minimal DAV-only build:

```sh
cargo build --release -p calcraft-server --no-default-features
```

## Installation

A step-by-step guide for production setups — DNS records, build or Docker,
systemd service, TLS with Let's Encrypt and ready-to-use **nginx** and
**Apache** configurations — is in [docs/installation.md](docs/installation.md).

Released binaries (Linux, macOS, Windows) and the Docker image
`ghcr.io/zeitgleich/calcraft` come with SHA-256 checksums, build provenance
attestations and a keyless cosign signature; how to check them is described in
[docs/release-verification.md](docs/release-verification.md). Maintainers: see
[docs/releasing.md](docs/releasing.md) and [CHANGELOG.md](CHANGELOG.md).

## Running

Create a user (the password is prompted for, or read from stdin with `--password-stdin`;
it is never accepted as an argument):

```sh
calcraft user add alice --display-name "Alice"
```

Start the server:

```sh
calcraft serve --config calcraft.toml
```

Point your client at `https://your-host/` – discovery via `/.well-known/caldav` and
`/.well-known/carddav` leads to `/dav/`. Always run CalCraft behind TLS: DAV clients
use HTTP Basic authentication.

Other user commands: `calcraft user list|passwd|disable|enable|set-admin|allow-sso-link|unbind-sso`
(`calcraft user add alice --admin` creates an administrator).

### Request limits and reverse proxies

CalCraft rejects excess requests immediately instead of queueing them:

- at most `server.max_concurrent_requests` (256) requests at the same time, further
  ones get `503` with `Retry-After` (`/healthz` and `/readyz` are exempt);
- per client 32 concurrent requests and a token bucket of 120 requests, refilled with
  20 per second (`429` with `Retry-After`);
- per client 20 failed logins in 15 minutes (DAV, web UI, second factor; known and
  unknown usernames), in addition to the lockout per account (5 in 15 minutes).
  Further logins from that client fail exactly like wrong credentials.

IPv6 clients count per /64 (`client_ipv6_prefix`). The per-client tables are capped
(`max_tracked_clients`, idle entries are evicted), so address hopping cannot exhaust
memory. Blocked entries are never evicted; if an attacker fills the table with
blocked networks (cheap with IPv6), new clients are not counted per client until
those entries expire, and only the per-account lockout applies. The client is the
TCP peer address; `X-Forwarded-For` is only used when the
peer is listed in `server.trusted_proxies`, read from the right and skipping trusted
proxies, so entries a client adds itself are ignored. The default
`["127.0.0.1/32", "::1/128"]` fits a reverse proxy on the same host; a local process
could then choose its client address, set `[]` if that matters. A proxy on another
host or a Docker bridge must be listed, otherwise all users share the proxy's limits;
CalCraft logs a warning once when it ignores `X-Forwarded-For` (see
`docs/installation.md`).

### Object sizes, contact photos and groups

| Limit | Default | Setting |
|---|---|---|
| Request body | 10 MiB | `server.max_body_bytes` |
| Calendar object | 1 MiB | fixed (`CALDAV:max-resource-size`) |
| vCard incl. embedded photos | 2 MiB (less if `server.max_body_bytes` is smaller) | `storage.max_vcard_bytes`, 64 KiB – 10 MiB, at most `server.max_body_bytes` (`CARDDAV:max-resource-size`) |
| Storage per user | 512 MiB | fixed, counts the stored bytes (base64 photos included) |

Larger objects are rejected with `403` and the `max-resource-size` precondition;
a body announced larger than `server.max_body_bytes` gets `413`. A reverse proxy
must allow at least `server.max_body_bytes` (see `docs/installation.md`).

Photos and logos (`PHOTO`, `LOGO`) are accepted embedded – vCard 3.0
`ENCODING=b;TYPE=JPEG:<base64>` (also `ENCODING=BASE64`), vCard 4.0
`data:image/…;base64,…` – if the base64 is syntactically valid, every declared
type (all `TYPE` values, the `data:` media type) is JPEG, PNG, GIF or WebP, and
the magic number of the first (at most 12) decoded bytes is one of them, with or
without a declared type; `ENCODING`/`VALUE` must not contradict each other. Or as
an `http`/`https` URI, which is stored but never fetched. Empty values are accepted, as they occur in exports. Anything else
(`javascript:`, `file:`, SVG, invalid base64) is rejected with
`CARDDAV:valid-address-data`. Apart from that magic number, image bytes are never
decoded or interpreted, and photos are never served as `image/*`.
Groups – vCard 4.0 `KIND:group` with `MEMBER`, Apple's
`X-ADDRESSBOOKSERVER-KIND:group` with `X-ADDRESSBOOKSERVER-MEMBER` – are ordinary
vCards with their own UID. Like every object they are stored and returned
byte for byte as uploaded.

### Backups and upgrades

```sh
calcraft backup /var/backups/calcraft-$(date +%F).db   # online, while the server runs
calcraft restore --yes /var/backups/calcraft-2026-10-21.db   # server stopped
```

Before applying database migrations, CalCraft writes a backup
`calcraft.db.pre-migration-<version>-<timestamp>` next to the database (the last 3
are kept). Migrations are forward-only; to roll back an upgrade, restore that
backup with the previous version. Details in
[`docs/backup-and-upgrade.md`](docs/backup-and-upgrade.md).

Time-range queries, recurrence and time zones (what is exact, what is
conservative, budgets): [`docs/caldav.md`](docs/caldav.md).

### Trash

Deleting a calendar entry, contact, calendar or address book (from a DAV
client or the web UI) moves it into the owner's trash for
`storage.trash_retention_days` (default 30; `0` disables the trash). DAV
clients see the deletion as usual. On the **Trash** page (or
`/api/v1/trash`) users restore entries – into the original collection under
the original name, so clients sync them back – or delete them permanently.
A deleted collection goes to the trash with all its entries and is restored
as a whole; entries whose collection was deleted permanently can be
restored into another collection. The trash counts towards the user's storage
quota until it is emptied or its entries expire; expired entries are purged
at startup and hourly.

Deleted data stays on the server (and in backups) for the retention period,
which matters for deletion requests (GDPR Art. 17): users remove it at once
with **Empty trash** / **Delete permanently**; operators can set
`trash_retention_days = 0` and restart, which purges **all** trash entries
of all users (CalCraft logs a warning with their number). Backups taken
before keep the data until they are rotated.

Optionally, the admin area shows a notice when a new release is out:

```toml
[updates]
check = true          # default: false
interval_hours = 24   # 1-168
```

CalCraft then requests `https://api.github.com/repos/Zeitgleich/CalCraft/releases/latest`
at startup and every `interval_hours` (plain `GET`, no cookies, no credentials, nothing
about your installation). GitHub sees the server's IP address and the User-Agent
`calcraft/<version>`. Drafts and pre-releases are ignored; the result is kept in memory
only and is also available as `GET /api/v1/admin/update`.

### App passwords and two-factor authentication

Each device should get its own app password (`ccap_…`), created
in the web UI (or via `POST /api/v1/app-passwords`) and revocable individually.
Once two-factor authentication (TOTP) is enabled for an account, DAV clients must
use app passwords; the account password then only works together with a code.

The REST API is described in `/api/openapi.json`.

### Single sign-on (OpenID Connect)

The web UI can log in via any OpenID Connect provider (Keycloak, Authentik, Zitadel,
Entra ID, …) using the Authorization Code flow with PKCE. DAV clients cannot do OIDC,
so SSO users create app passwords for their devices.

Keycloak example: create a client `calcraft` in realm `main` (Client authentication
on, Standard flow only), set the valid redirect URI to exactly
`https://cal.example.com/api/v1/auth/oidc/callback` and copy the client secret.
For admin rights from a group, add a *Group Membership* mapper (claim `groups`,
"Add to ID token" on; full group path gives values like `/calcraft-admins`).

```toml
[oidc]
enabled = true
issuer = "https://keycloak.example.com/realms/main"
client_id = "calcraft"
# Secret via file or CALCRAFT_OIDC__CLIENT_SECRET instead of the config file:
client_secret_file = "/run/secrets/calcraft-oidc"
redirect_uri = "https://cal.example.com/api/v1/auth/oidc/callback"
scopes = ["openid", "profile"]
username_claim = "preferred_username"
# Optional: admin rights follow this group (granted and revoked on each login).
admin_claim = "groups"
admin_value = "/calcraft-admins"
# Create unknown users on first login (default: an admin creates them first).
auto_create_users = false
button_label = "Sign in with Keycloak"
```

How accounts are matched:

- The configured claim (default `preferred_username`) must be a valid CalCraft
  username after lowercasing (and optionally removing `username_strip_suffix`, e.g.
  `@example.com` for UPNs); it is never rewritten. E-mail addresses are never used.
- Accounts created via SSO (`auto_create_users`) are bound to the IdP identity
  (issuer + subject) at creation. An **existing** local account is never linked just
  because the name matches: an operator approves exactly one IdP account with
  `calcraft user allow-sso-link <user> --subject <sub> [--hours 1]` (the subject is the
  user's ID in the IdP admin console, e.g. Keycloak's user ID; valid 1 h by default, at
  most 24 h). Only the next SSO login with that issuer + subject binds the account and
  consumes the approval; any other IdP account with the same name fails with
  `account_exists` and leaves the approval in place.
- Once bound, logins must present the same subject, so a different IdP account with
  the same username cannot take over. `calcraft user unbind-sso <user>` resets it.
- The IdP should not let users choose their username freely (open self-registration),
  otherwise someone can claim a name before its owner does. If it does, restrict SSO
  with `required_claim`/`required_value` (e.g. a group only admins assign).
- Disabled accounts cannot log in. Accounts created via SSO have no usable password
  until an administrator sets one (`calcraft user passwd`).
- By default SSO logins do not ask for the local TOTP code: MFA is the IdP's job.
  With `require_local_totp = true`, accounts with local 2FA cannot use SSO
  (`local_2fa_required`).
- `[security] password_login = false` makes SSO the only interactive login: the
  password login API and DAV Basic auth with the account password are refused, app
  passwords keep working; changing the password and local 2FA setup are refused too
  (`password_login_disabled`). CalCraft refuses to start this way without `[oidc]`.
- Pending logins are kept in a bounded table (oldest evicted); the per-client request
  limits (see above) also apply to the SSO endpoints. Failed SSO callbacks do not
  count as failed logins (they involve no guessable secret).

The UI learns whether SSO is available from `GET /api/v1/auth/methods`. Errors are
reported by redirecting to `/?sso_error=<code>`. Discovery, JWKS and token endpoints
must be https on the issuer's host (`extra_endpoint_hosts` allows more, e.g. Google's
`www.googleapis.com`); the system trust store is used, `ca_file` adds a private CA.

To try it against a real Keycloak locally (verified with Keycloak 26.4):

```sh
docker run --rm --name kc -p 127.0.0.1:8080:8080 -e KC_BOOTSTRAP_ADMIN_USERNAME=admin \
  -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin quay.io/keycloak/keycloak:26.4 start-dev
```

In the admin console (`http://127.0.0.1:8080`, admin/admin) create realm `main`, the
client as above with redirect URI `http://localhost:5232/api/v1/auth/oidc/callback`,
and a user. Then run CalCraft with `issuer = "http://127.0.0.1:8080/realms/main"`,
that redirect URI, `[security] secure_cookies = false` and
`allow_insecure_issuer_for_testing = true` (plain HTTP – never in production), open
`http://localhost:5232/api/v1/auth/oidc/login` and log in. Use the same host name for
the issuer everywhere: Keycloak derives `iss` from the URL it is called with.
Stop the container with `docker stop kc`.

### Import and export

Calendars can be exported and imported as iCalendar (`ics`), jCal (`jcal`), xCal
(`xcal`) and CSV (`csv`); address books as vCard 3.0/4.0 (`vcf`), jCard (`jcard`) and
CSV. On the command line:

```sh
calcraft export --user alice --collection calendar/default --format ics > alice.ics
calcraft import --user alice --collection calendar/default --format ics alice.ics
calcraft import --user alice --collection addressbook/default --format csv \
    --on-conflict replace contacts.csv     # `-` reads standard input
```

Via the REST API (session cookie; `import` needs the `X-CalCraft-Request` header):
`GET /api/v1/collections/{kind}/{slug}/export?format=…` and
`POST /api/v1/collections/{kind}/{slug}/import?format=…&on_conflict=skip|replace`
with the file as body. The import answers with counts of created, replaced, skipped
and rejected objects and the reasons for the first 20 rejections.

Every imported object is validated exactly like a CalDAV/CardDAV upload, and the
storage quota and object limits apply. Files may have at most 20 MiB and 10 000
objects; via HTTP the global `server.max_body_bytes` (10 MiB by default) applies
as well, so import larger files with the CLI. The converted objects may take at most
four times the file size (64 MiB at most), which bounds the copies of time zones.
Events are grouped by UID together with their recurrence overrides, and
the referenced time zones are copied into each object. Objects whose UID already
exists in the collection are skipped unless `on_conflict=replace` is given.

CSV columns are `summary,start,end,all_day,location,description,uid` for events and
`name,emails,phones,org,uid` for contacts (several values separated by `;`). CSV
exports hold events only (the master of a recurring event, without its rule), and
cells starting with `=`, `+`, `-`, `@`, TAB or CR are prefixed with `'` so that
spreadsheets do not evaluate them; the import removes this prefix again.

### Migrating from another server

`calcraft migrate` copies all calendars, task lists and address books of one
account on another CalDAV/CardDAV server into a CalCraft user. It finds them via
`.well-known`, `current-user-principal` and the home sets, keeps name, color,
description and component set (VEVENT/VTODO/VJOURNAL) of each collection, and
imports every object through the same validation as `calcraft import`, keeping
its UID. Valid objects are stored as received, after validation and after line
endings are normalised to CRLF (multiget responses carry the data inside XML,
whose parsing turns CRLF into LF); a resource with several UIDs is split, one
without UID gets a UID derived from its source name.

```sh
calcraft migrate --from https://nas.example.com:5001/caldav/ \
    --carddav-from https://nas.example.com:5001/carddav/ \
    --source-user alice --user alice --dry-run
```

| Source | `--from` (placeholders) | Notes |
|---|---|---|
| Synology Calendar / Contacts | `https://<nas>:5001/caldav/` plus `--carddav-from https://<nas>:5001/carddav/` | Self-signed certificate: export it (or your own CA) as PEM and pass `--ca-file nas.pem`. |
| Radicale | `https://<host>:5232/` | |
| Baïkal | `https://<host>/dav.php/` | |
| Nextcloud | `https://<host>/remote.php/dav/` | Use an app password; shared calendars only with `--include-shared`. |

- **Password:** prompted for, or the first line of `--password-file <file>`
  (a regular file, not a link, mode `600`), or `CALCRAFT_MIGRATE_PASSWORD`; never
  on the command line, never in the URL, never printed or logged, and wiped from
  memory when no longer needed. The environment variable is not removed from the
  process environment (that needs `unsafe` code, which CalCraft forbids), so
  prefer the prompt or the file. It is sent as HTTP Basic, only to the origin of
  `--from`.
- **Options:** `--dry-run` (discover, fetch and validate, write nothing),
  `--only calendars|addressbooks`, `--map source-slug=target-slug` (repeatable;
  otherwise the slug comes from the last path segment of the source, made safe),
  `--on-conflict skip|replace` (UIDs already in the target collection; default
  `skip`), `--include-shared`, `--batch-size` (multiget, default 50),
  `--max-response-mib` (default 32), `--timeout` (seconds per request, default 60),
  `--delay-ms` (pause before each request).
- **Idempotent:** existing target collections are reused (their properties are
  not changed), and with `skip` a second run copies only what is new, so an
  interrupted run can simply be repeated.
- Objects come via `calendar-multiget`/`addressbook-multiget` in batches, or one
  by one with `GET` if the server has no multiget. Scheduling inboxes/outboxes,
  notifications and webcal subscriptions are skipped; `calendar-timezone` is not
  stored (objects carry their own `VTIMEZONE`). The report lists per collection
  how many objects were created, replaced, skipped and rejected, with reasons
  but without contents; the exit code is non-zero if a collection could not be
  copied completely.
- **Network rules:** HTTPS only, with the system trust store plus `--ca-file`;
  there is no switch to disable certificate checks. `--allow-http` permits plain
  HTTP with a warning (password and data unencrypted). Redirects are followed only
  within the origin, or from `http` to `https` on the same host and port (or
  from port 80 to 443); a redirect to another port, such as Synology's
  5000 → 5001, is refused – pass the `https://…:5001/` URL instead. Hrefs
  pointing elsewhere are ignored, and URLs in messages lose query and fragment.
  Once the quota or object limit is reached, the rest of a collection is not
  fetched (reported as skipped). Private and loopback
  addresses are allowed: unlike server-side fetches such as webcal subscriptions,
  where users choose URLs and SSRF rules keep the server out of its own network,
  this command is run by the administrator against a server they name – usually
  a NAS on the local network.
- Limits: responses up to `--max-response-mib`, objects up to the object size limit
  (1 MiB, vCards `storage.max_vcard_bytes`), at most 100 000 objects per
  collection; quota and limits of the target user apply.

See [`calcraft.example.toml`](calcraft.example.toml). Every setting can be overridden via
environment variables with the prefix `CALCRAFT_` and `__` as separator,
e.g. `CALCRAFT_SERVER__BIND=0.0.0.0:5232`.

## Tests

Numbers as of 2026-10-22 (version 0.1.0-alpha.1); they are not updated
automatically.

`cargo test --workspace --all-features` runs **426 tests** (1 of them
ignored by default, see below): 278 unit tests in the crates' `src/` and
148 integration tests in `tests/` directories. There are no doc tests.

| Crate | Unit | Integration |
|---|---:|---:|
| calcraft-core | 5 | – |
| calcraft-dav | 28 | – |
| calcraft-ical | 78 | 12 |
| calcraft-server | 86 | 136 |
| calcraft-store | 61 | – |
| calcraft-ui (host tests) | 18 | – |
| calcraft-web | 2 | – |

Integration suites of `calcraft-server` (`crates/calcraft-server/tests/`):

- **Authorization matrix** (`authz_matrix.rs`): every routed DAV method and
  unknown methods × every kind of path (own, another user's, a nonexistent
  user's, traversal and percent-encoding variants) × every kind of
  credential; COPY/MOVE with 13 `Destination` variants; every REST route
  from `api::ROUTES`. It checks exact status codes, byte-identical answers
  for foreign and nonexistent resources, that marker strings from the other
  user's data never leak, and that the other user's data is unchanged after
  every write. One run sends about 42 600 requests and makes over 163 000
  counted assertions. A DAV method or REST route without a matrix entry
  makes it fail.
- **Protocols:** `dav.rs`, `caldav.rs`, `carddav_sync.rs`,
  `carddav_photos.rs` (photos, groups, size limits), `recurrence.rs` (RRULE
  expansion, time zones, budgets), `move_copy.rs`, `trash.rs`, `http.rs`.
- **API and authentication:** `api.rs`, `oidc.rs` (against a mock IdP).
- **Limits and robustness:** `limits.rs` (global and per-client request
  limits, login throttling), `stress.rs` (32 parallel writers: no lost
  updates, no 500; a heavier variant with 64 writers is `#[ignore]`d and
  runs nightly on Linux),
  `failures.rs` (read-only database, full disk, aborted uploads).
- **Logs:** `log_secrets.rs` checks that passwords, tokens, TOTP and
  recovery codes, cookies and calendar/contact contents never reach the
  logs.
- **Data:** `backup.rs`, `transfer.rs` (import/export); `calcraft-ical` has
  its own `transfer.rs` for the format conversions.

Further checks:

- **Fuzzing** (`fuzz/`, cargo-fuzz): 4 targets (`ical_object`, `dav_xml`,
  `recurrence`, `import`). CI runs each of them for 60 s on every change
  and for 20 min nightly.
- **CI** on Linux, macOS and Windows: `cargo fmt`, Clippy, all tests with
  all features and DAV-only, wasm build of the web UI, a client
  compatibility run with python-caldav, an upgrade test from every older
  migration state and a backup/restore smoke test with the real binary.
- **cargo-deny** (advisories, licenses, sources) on every change and an
  advisory check daily; **CodeQL** (`security-extended`) for Rust and the
  workflows on pushes and pull requests to `main` and weekly.
- **Coverage:** 92.2 % of lines (16 037 of 17 401), measured with
  `cargo llvm-cov --workspace --all-features`; per crate and known gaps in
  [docs/coverage.md](docs/coverage.md). CI measures it on every pull request.

## Security

What CalCraft does, in short (details: [docs/produktionsreife.md](docs/produktionsreife.md),
German):

- **Authentication:** passwords hashed with Argon2id (with a dummy hash for
  unknown users against timing); per-device app passwords, stored only as
  hashes; TOTP two-factor authentication with hashed recovery codes;
  lockout per account (5 failures in 15 minutes) plus a throttle per client
  address (20 in 15 minutes, also for unknown names). A request cancelled
  after the password comparison has started still counts as a failure.
  OpenID Connect with PKCE; SSO accounts are bound to issuer + subject,
  and an existing local account is only linked after an explicit operator
  approval for one subject.
- **Authorization:** every DAV path is resolved against the authenticated
  owner in one place; another user's resources answer exactly like
  nonexistent ones. The authorization matrix test (see *Tests*) enforces
  this for every method and route.
- **Input handling:** XML without DTDs or entities, with depth, element
  and size limits; iCalendar and vCard are validated on upload, with
  limits for size, components, properties and parameters; request body,
  REPORT and import budgets; recurrence expansion with CPU budgets per
  object and per REPORT (see [docs/caldav.md](docs/caldav.md)); contact
  photos are checked against JPEG/PNG/GIF/WebP magic numbers, never decoded
  and never served as images.
- **Web UI and API:** strict Content Security Policy (no inline scripts or
  styles, same origin only), `HttpOnly`, `SameSite=Strict` and `Secure`
  session cookies, a required CSRF header on state-changing API requests,
  plus `X-Frame-Options`, `Referrer-Policy` and optional HSTS
  (`security.hsts`).
- **Operations:** global and per-client request limits, `X-Forwarded-For`
  only from `server.trusted_proxies`, a verified backup before every
  database migration, and logs without secrets or contents (tested).
- **Supply chain:** few dependencies, each justified; no `unsafe` code
  (`forbid`); cargo-deny on every change, advisories also daily;
  Dependabot; all GitHub Actions pinned to commit hashes; CodeQL. Release archives carry SHA-256
  checksums and Sigstore build provenance, the Docker image is signed with
  cosign (keyless) – see [docs/release-verification.md](docs/release-verification.md).
- **Process:** changes go through pull requests with the full CI; known
  attacks on other calendar and contact servers are collected in
  [docs/security/angriffshistorie.md](docs/security/angriffshistorie.md)
  (German) together with the countermeasures and regression tests derived
  from them.

Please report vulnerabilities privately, see [SECURITY.md](SECURITY.md).

## License

CalCraft is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).

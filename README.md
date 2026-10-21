# CalCraft

Self-hosted CalDAV/CardDAV server written in Rust, with an optional web interface.

> **Status:** early development (milestone M3: CalDAV, CardDAV, sync). Not ready for production use.

## Goals

- Standards-compliant CalDAV (RFC 4791) and CardDAV (RFC 6352)
- Security first: no `unsafe`, audited dependencies, fuzzed parsers, strict defaults
- Single binary, optional Docker image
- Optional web UI (Leptos) and REST API with OpenAPI – both can be disabled
- Local users or SSO via OpenID Connect (e.g. Keycloak), with per-device app passwords
- Support for iCalendar, jCal, xCal, vCard 3/4, jCard, CSV and webcal subscriptions

See [PLAN.md](PLAN.md) for the roadmap.

## Building

```sh
cargo build --release
```

Cargo features of `calcraft-server`:

| Feature | Default | Description |
|---|---|---|
| `web-ui` | yes | Embedded web interface |
| `api` | yes | REST API |

CalCraft runs on Linux, macOS and Windows.

Minimal DAV-only build:

```sh
cargo build --release -p calcraft-server --no-default-features
```

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

Other user commands: `calcraft user list|passwd|disable|enable|set-admin`
(`calcraft user add alice --admin` creates an administrator).

### App passwords and two-factor authentication

Each device should get its own app password (`ccap_…`), created via the REST API
(`POST /api/v1/app-passwords`, soon in the web UI) and revocable individually.
Once two-factor authentication (TOTP) is enabled for an account, DAV clients must
use app passwords; the account password then only works together with a code.

The REST API is described in `/api/openapi.json`.

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
as well. Events are grouped by UID together with their recurrence overrides, and
the referenced time zones are copied into each object. Objects whose UID already
exists in the collection are skipped unless `on_conflict=replace` is given.

CSV columns are `summary,start,end,all_day,location,description,uid` for events and
`name,emails,phones,org,uid` for contacts (several values separated by `;`). CSV
exports hold events only (the master of a recurring event, without its rule), and
cells starting with `=`, `+`, `-`, `@`, TAB or CR are prefixed with `'` so that
spreadsheets do not evaluate them; the import removes this prefix again.

See [`calcraft.example.toml`](calcraft.example.toml). Every setting can be overridden via
environment variables with the prefix `CALCRAFT_` and `__` as separator,
e.g. `CALCRAFT_SERVER__BIND=0.0.0.0:5232`.

## Security

Please report vulnerabilities privately, see [SECURITY.md](SECURITY.md).

## License

CalCraft is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).

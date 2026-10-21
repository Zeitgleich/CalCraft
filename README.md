# CalCraft

Self-hosted CalDAV/CardDAV server written in Rust, with an optional web interface.

> **Status:** early development (milestone M4: REST API and first web UI). Not ready for production use.

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

Minimal DAV-only build:

```sh
cargo build --release -p calcraft-server --no-default-features
```

## Installation

A step-by-step guide for production setups — DNS records, build or Docker,
systemd service, TLS with Let's Encrypt and ready-to-use **nginx** and
**Apache** configurations — is in [docs/installation.md](docs/installation.md).

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

### App passwords and two-factor authentication

Each device should get its own app password (`ccap_…`), created
in the web UI (or via `POST /api/v1/app-passwords`) and revocable individually.
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

See [`calcraft.example.toml`](calcraft.example.toml). Every setting can be overridden via
environment variables with the prefix `CALCRAFT_` and `__` as separator,
e.g. `CALCRAFT_SERVER__BIND=0.0.0.0:5232`.

## Security

Please report vulnerabilities privately, see [SECURITY.md](SECURITY.md).

## License

CalCraft is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).

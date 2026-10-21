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
| `oidc` | yes | Single sign-on via OpenID Connect (implies `api`; adds an HTTPS client: rustls/ring) |

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
  because the name matches: an operator approves it once with
  `calcraft user allow-sso-link <user> [--hours 24]`; the next SSO login binds it and
  consumes the approval. Without approval the login fails with `account_exists`.
- Once bound, logins must present the same subject, so a different IdP account with
  the same username cannot take over. `calcraft user unbind-sso <user>` resets it.
- `required_claim`/`required_value` restrict SSO to e.g. one group.
- Disabled accounts cannot log in. Accounts created via SSO have no usable password
  until an administrator sets one (`calcraft user passwd`).
- By default SSO logins do not ask for the local TOTP code: MFA is the IdP's job.
  With `require_local_totp = true`, accounts with local 2FA cannot use SSO
  (`local_2fa_required`).
- `[security] password_login = false` makes SSO the only interactive login: the
  password login API and DAV Basic auth with the account password are refused, app
  passwords keep working. CalCraft refuses to start this way without `[oidc]`.
- Starting logins is rate-limited globally (10/s, burst 50; `rate_limited`).

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

See [`calcraft.example.toml`](calcraft.example.toml). Every setting can be overridden via
environment variables with the prefix `CALCRAFT_` and `__` as separator,
e.g. `CALCRAFT_SERVER__BIND=0.0.0.0:5232`.

## Security

Please report vulnerabilities privately, see [SECURITY.md](SECURITY.md).

## License

CalCraft is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).

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
| `oidc` | yes | Single sign-on via OpenID Connect (implies `api`; adds an HTTPS client: rustls/ring) |

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

Other user commands: `calcraft user list|passwd|disable|enable|set-admin|unbind-sso`
(`calcraft user add alice --admin` creates an administrator).

### App passwords and two-factor authentication

Each device should get its own app password (`ccap_…`), created via the REST API
(`POST /api/v1/app-passwords`, soon in the web UI) and revocable individually.
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
- The first SSO login binds the IdP identity (issuer + subject) to the account; later
  logins must present the same subject, so a different IdP account with the same
  username cannot take over. `calcraft user unbind-sso <user>` resets the binding.
- Only use providers where users cannot choose their username freely (no open
  self-registration), or restrict SSO with `required_claim`/`required_value`: on the
  first login an existing local account with that name is bound to the IdP account.
- Disabled accounts cannot log in. Accounts created via SSO have no usable password
  until an administrator sets one (`calcraft user passwd`).
- SSO logins do not ask for the local TOTP code; enforce MFA at the IdP.

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

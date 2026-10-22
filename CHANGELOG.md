# Changelog

All notable changes to CalCraft are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions
follow [Semantic Versioning](https://semver.org/). Release process:
[docs/releasing.md](docs/releasing.md).

## [Unreleased]

### Added

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

### Changed

- Web UI: refreshed design with the logo in the header and on the login page,
  a token-based colour system (light and dark), clearer buttons, focus rings
  and form fields, collection cards with their colour along the left edge,
  outlined empty states, and tables that scroll inside their frame on phones.

### Fixed

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
  cannot lock an account by keeping the server busy. Database overload
  during login answers 503 with `Retry-After`; pool waits end after at most
  10 s (half the request timeout if that is shorter).
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

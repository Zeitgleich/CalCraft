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
  `server.trusted_proxies` (empty by default). Behind a local reverse proxy,
  set `trusted_proxies = ["127.0.0.1/32", "::1/128"]`.
- Release pipeline (`.github/workflows/release.yml`): binaries for Linux,
  macOS and Windows, `SHA256SUMS`, build provenance attestations and a
  signed multi-arch Docker image on `ghcr.io/zeitgleich/calcraft`; see
  [docs/release-verification.md](docs/release-verification.md).

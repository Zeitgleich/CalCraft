# Changelog

All notable changes to CalCraft are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions
follow [Semantic Versioning](https://semver.org/). Release process:
[docs/releasing.md](docs/releasing.md).

## [Unreleased]

### Added

- Release pipeline (`.github/workflows/release.yml`): binaries for Linux,
  macOS and Windows, `SHA256SUMS`, build provenance attestations and a
  signed multi-arch Docker image on `ghcr.io/zeitgleich/calcraft`; see
  [docs/release-verification.md](docs/release-verification.md).

### Fixed

- More than five parallel DAV requests of one account with correct
  credentials (app password or cached password) no longer get 401: they wait
  for running login attempts instead of counting as a lockout.
- Creating or renaming collections, creating users and OIDC link approvals
  no longer fail with 500 (`SQLITE_BUSY`) under concurrent writes: all write
  transactions start with `BEGIN IMMEDIATE`.
- A full disk answers 507 Insufficient Storage instead of 500, and pooled
  database connections whose transaction SQLite rolled back by itself (disk
  full, I/O error) are closed instead of failing every later write.

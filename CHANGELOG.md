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

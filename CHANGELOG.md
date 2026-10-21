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

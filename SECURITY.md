# Security Policy

Security is the top priority of CalCraft. Thank you for helping keep it safe.

## Reporting a vulnerability

**Do not open a public issue.** Report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/Zeitgleich/CalCraft/security/advisories/new).

Please include:

- affected version or commit
- description and impact
- steps to reproduce or a proof of concept

We aim to acknowledge reports within 72 hours and to provide a fix or mitigation
plan within 14 days. We credit reporters unless they prefer otherwise.

## Supported versions

Until 1.0, only the latest release receives security fixes.

## Security practices

- `#![forbid(unsafe_code)]` in all crates
- Dependency checks with `cargo-deny` (RustSec advisories, licenses, sources) on every change, advisories also daily
- Fuzzing of the iCalendar, recurrence, XML and import parsers and of the scheduling (iTIP) logic (five targets, on every change and nightly)
- Strict HTTP security headers and conservative defaults
- More detail: the *Security* section of the [README](README.md#security)

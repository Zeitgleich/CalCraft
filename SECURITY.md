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
- Fuzzing of the iCalendar, recurrence (including alarm evaluation), XML (including the sharing request bodies) and import parsers, of the scheduling (iTIP) logic, of the attachment handling (file names, `Content-Disposition`, ATTACH changes) and of the mail code (SMTP replies, message generation, dot-stuffing) (seven targets, on every change and nightly)
- Sharing between users needs the sharee's acceptance and is re-checked inside every write; public calendar links are off by default, use hashed 256-bit tokens and answer every wrong token with the same 404 (see [docs/sharing.md](docs/sharing.md))
- E-mail reminders are off by default, go only to the calendar owner's own address (never to addresses from calendar data), use STARTTLS or implicit TLS (plaintext only to a relay on localhost) and read the SMTP password only from an owner-only file; logs carry no addresses, subjects or server replies (see [docs/reminders.md](docs/reminders.md))
- Strict HTTP security headers and conservative defaults
- More detail: the *Security* section of the [README](README.md#security)

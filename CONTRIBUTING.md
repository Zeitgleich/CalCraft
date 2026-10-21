# Contributing to CalCraft

Thanks for your interest in contributing!

## Developer Certificate of Origin

All commits must be signed off (`git commit -s`), certifying the
[Developer Certificate of Origin](https://developercertificate.org/).

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

## Guidelines

- No `unsafe` code.
- New dependencies need a short justification in the pull request.
- Parsers and protocol handlers need tests for malformed and hostile input.
- Security issues: see [SECURITY.md](SECURITY.md), never in public issues.

By contributing, you agree that your contributions are licensed under the
AGPL-3.0-or-later.

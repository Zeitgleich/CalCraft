# Release erstellen (Checkliste für Maintainer)

Ein Release entsteht ausschließlich durch ein gepushtes Tag `vX.Y.Z`
(Vorabversion: `vX.Y.Z-alpha.N`, `vX.Y.Z-rc.N` o. Ä.). Der Workflow
[`release.yml`](../.github/workflows/release.yml) baut dann Binaries,
Archive, `SHA256SUMS`, das Docker-Image, signiert/attestiert alles und legt
das GitHub-Release an. Lokal wird nichts gebaut oder signiert.

## Vorher

1. `main` ist grün (CI), offene Sicherheitsmeldungen sind bearbeitet,
   `cargo deny check` ohne Befund.
2. Optional ein Probelauf: *Actions → Release → Run workflow* auf `main`.
   Er baut alle Binaries und beide Docker-Images, veröffentlicht aber nichts
   (keine Attestierung, kein Push, kein Release).

## Release

1. Version in `Cargo.toml` setzen: `[workspace.package] version` **und**
   die `version` der internen Crates unter `[workspace.dependencies]`.
   Danach `cargo update -w` oder `cargo check --workspace` (aktualisiert
   `Cargo.lock`; der Workflow prüft mit `--locked`).
2. `CHANGELOG.md`: Abschnitt *Unreleased* in `## [X.Y.Z] – JJJJ-MM-TT`
   umbenennen und darüber ein leeres `## [Unreleased]` anlegen;
   Migrationen und Konfigurationsänderungen ausdrücklich nennen.
3. Commit über einen Pull Request nach `main` (`git commit -s`), CI abwarten,
   mergen.
4. Auf dem gemergten Commit taggen (signiertes Tag, falls eingerichtet):

   ```bash
   git switch main && git pull --ff-only
   git tag -s v1.2.3 -m "CalCraft 1.2.3"
   git push origin v1.2.3
   ```

   Der Workflow bricht ab, wenn das Tag nicht zur Version in `Cargo.toml`
   passt. Ein Tag mit `-` (z. B. `v1.3.0-rc.1`, `v0.1.0-alpha.1`) wird als
   Vorabversion (*Pre-release*) veröffentlicht und bekommt nur das
   Image-Tag `1.3.0-rc.1`, nicht `1.3`/`latest`. Der Update-Hinweis im
   Admin-Bereich ignoriert Vorabversionen.

## Danach

1. Workflow-Lauf prüfen: alle Jobs grün, Release mit 5 Archiven und
   `SHA256SUMS`.
2. Die Schritte aus [release-verification.md](release-verification.md) einmal
   selbst ausführen (Prüfsummen, `gh attestation verify` für ein Archiv und
   das Image, `cosign verify`).
3. Paket `calcraft` auf ghcr.io beim ersten Release auf *public* stellen
   (*Package settings → Change visibility*); neue Pakete sind privat.

## Wenn etwas schiefgeht

- Fehlgeschlagener Lauf vor dem Release-Job: Ursache beheben, Tag löschen
  (`git push origin :refs/tags/v1.2.3`, lokal `git tag -d v1.2.3`) und neu
  setzen. Lief der Job `docker manifest` schon durch, existiert das
  Image-Tag bereits und die Pipeline verweigert das Überschreiben: dann die
  Paketversion in GHCR löschen oder gleich eine neue Patch-Version
  herausgeben.
- Veröffentlichte Releases nicht überschreiben: stattdessen eine neue
  Patch-Version herausgeben.

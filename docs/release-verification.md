# Releases prüfen

Jedes Release wird ausschließlich vom Workflow
[`.github/workflows/release.yml`](../.github/workflows/release.yml) auf
GitHub-Runnern gebaut, ausgelöst durch ein Tag `vX.Y.Z`. Es gibt keine
privaten Signaturschlüssel: Signaturen und Attestierungen sind *keyless*
(Sigstore). Das Zertifikat bindet sie an genau diesen Workflow, dieses
Repository und das Tag; das Ereignis wird im öffentlichen Transparenzlog
(Rekor) protokolliert.

Ein Release enthält:

| Datei | Inhalt |
|---|---|
| `calcraft-<version>-<target>.tar.gz` | Linux und macOS: `calcraft`, `LICENSE`, `README.md`, `calcraft.example.toml` |
| `calcraft-<version>-x86_64-pc-windows-msvc.zip` | Windows: `calcraft.exe` und dieselben Dateien |
| `SHA256SUMS` | SHA-256 aller Archive |
| `ghcr.io/zeitgleich/calcraft:<version>` | Docker-Image für `linux/amd64` und `linux/arm64` |

Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (beide
glibc ≥ 2.35, z. B. Debian 12, Ubuntu 22.04), `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-pc-windows-msvc`. Die Web-Oberfläche ist in
allen Binaries enthalten.

Die macOS- und Windows-Binaries sind **nicht** mit einem Apple- bzw.
Microsoft-Zertifikat signiert. Die Herkunft belegen die Attestierungen
unten; macOS fordert beim ersten Start ggf. eine Freigabe
(`xattr -d com.apple.quarantine calcraft`, nachdem die Prüfung erfolgreich
war).

Benötigte Werkzeuge: `sha256sum` (macOS: `shasum -a 256`), die
[GitHub CLI](https://cli.github.com/) ≥ 2.49 und für das Image
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/) ≥ 3.

In den Beispielen steht `1.2.3` für die Version; ersetzen.

## 1. Prüfsummen

Archiv und `SHA256SUMS` in dasselbe Verzeichnis laden, dann:

```bash
sha256sum --ignore-missing -c SHA256SUMS
```

```text
calcraft-1.2.3-x86_64-unknown-linux-gnu.tar.gz: OK
```

macOS: `shasum -a 256 --ignore-missing -c SHA256SUMS`. Windows (PowerShell):
`Get-FileHash calcraft-1.2.3-x86_64-pc-windows-msvc.zip` und den Wert mit
der Zeile in `SHA256SUMS` vergleichen.

Prüfsummen allein schützen nur vor Übertragungsfehlern: wer das Archiv
austauschen kann, kann auch `SHA256SUMS` austauschen. Deshalb zusätzlich
Schritt 2.

## 2. Build-Provenance der Archive

```bash
gh attestation verify calcraft-1.2.3-x86_64-unknown-linux-gnu.tar.gz \
  --repo Zeitgleich/CalCraft
```

Das prüft, dass genau diese Datei (per SHA-256) von einem GitHub-Actions-Lauf
im Repository `Zeitgleich/CalCraft` gebaut wurde, und zeigt Workflow, Commit
und Tag. Strenger, auf den Release-Workflow und das Tag beschränkt:

```bash
gh attestation verify calcraft-1.2.3-x86_64-unknown-linux-gnu.tar.gz \
  --repo Zeitgleich/CalCraft \
  --signer-workflow Zeitgleich/CalCraft/.github/workflows/release.yml \
  --source-ref refs/tags/v1.2.3
```

`SHA256SUMS` ist ebenfalls attestiert (`gh attestation verify SHA256SUMS
--repo Zeitgleich/CalCraft`); ist die Datei geprüft, genügt für die Archive
Schritt 1.

`gh attestation verify` braucht eine Anmeldung bei GitHub (`gh auth login`)
oder kann offline mit einem heruntergeladenen Bundle arbeiten
(`gh attestation download`, dann `--bundle`).

## 3. Docker-Image

Das Image ist per Digest mit cosign (keyless) signiert. Den Digest nennen die
Release-Notes (`Image: … = ghcr.io/zeitgleich/calcraft@sha256:…`). Geprüft
wird immer per Digest, denn ein Tag könnte nachträglich auf ein anderes Image
zeigen:

```bash
cosign verify ghcr.io/zeitgleich/calcraft@sha256:<digest> \
  --certificate-identity-regexp '^https://github\.com/Zeitgleich/CalCraft/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Für genau ein Release statt des regulären Ausdrucks:
`--certificate-identity https://github.com/Zeitgleich/CalCraft/.github/workflows/release.yml@refs/tags/v1.2.3`.

Die Build-Provenance des Images:

```bash
gh attestation verify oci://ghcr.io/zeitgleich/calcraft@sha256:<digest> \
  --repo Zeitgleich/CalCraft
```

Die Pipeline überschreibt keine vorhandenen Versions-Tags und bricht ab, wenn
es das Release schon gibt. Trotzdem das Image im Betrieb **per Digest**
einbinden, damit ein verschobenes Tag nichts ändert:

```bash
docker pull ghcr.io/zeitgleich/calcraft@sha256:<digest>
```

Tags: `<version>` (z. B. `1.2.3`), `<major>.<minor>` (`1.2`) und `latest`.
Vorabversionen (`1.3.0-rc.1`) erhalten nur das Versions-Tag.

## Wenn eine Prüfung fehlschlägt

Die Datei nicht verwenden und den Fall vertraulich melden, siehe
[SECURITY.md](../SECURITY.md).

# Testabdeckung

Die Zeilenabdeckung der Workspace-Tests wird mit
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) gemessen. Die
CI (`.github/workflows/coverage.yml`) misst sie bei jedem Pull Request und
jedem Push auf `main`, schreibt die Tabelle pro Crate in die Job-Zusammenfassung
und legt `lcov.info` sowie den HTML-Bericht 14 Tage als Artefakt `coverage` ab.
Es gibt noch keine Schwellwerte; die Messung ist rein informativ. Es werden
keine Daten an externe Dienste (Codecov, Coveralls o. Ä.) gesendet.

## Lokal ausführen

Einmalig:

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.1 --locked
```

Messung (wie in der CI):

```sh
cargo llvm-cov --workspace --all-features --no-report
cargo llvm-cov report --summary-only          # Tabelle pro Datei
cargo llvm-cov report --html --open           # HTML-Bericht
cargo llvm-cov report --lcov --output-path lcov.info
```

Die instrumentierten Builds liegen unter `target/llvm-cov-target/`;
`cargo llvm-cov clean --workspace` bzw. `cargo clean` entfernt sie wieder.

Hinweise zur Interpretation:

- `fuzz/` ist kein Workspace-Mitglied und wird nicht gemessen.
- `calcraft-ui` wird nur mit seinen Host-Tests gemessen; Code hinter
  `cfg(target_arch = "wasm32")` erscheint nicht.
- Unit-Tests in `#[cfg(test)]`-Modulen innerhalb von `src/` zählen in der
  Statistik mit und heben die Prozentwerte etwas an.
- Die Smoke-Tests der CI (Kompatibilität, Backup/Restore) starten das
  nicht instrumentierte Binary. CLI-Pfade (`users.rs`, `transfer_cmd.rs`,
  `main.rs`) erscheinen deshalb als ungetestet, obwohl ein Teil davon dort
  durchlaufen wird.

## Stand 22.10.2026

Gemessen auf `main` (b771c22) mit Rust 1.99.1 und cargo-llvm-cov 0.9.1,
`cargo llvm-cov --workspace --all-features`, macOS. Alle Tests grün.

| Crate | Zeilen abgedeckt | Abdeckung |
| --- | ---: | ---: |
| calcraft-core | 90 / 93 | 96,8 % |
| calcraft-dav | 725 / 747 | 97,1 % |
| calcraft-ical | 2961 / 3154 | 93,9 % |
| calcraft-server | 4886 / 5602 | 87,2 % |
| calcraft-store | 2353 / 2503 | 94,0 % |
| calcraft-ui | 254 / 289 | 87,9 % |
| calcraft-web | 42 / 61 | 68,9 % |
| **gesamt** | **11311 / 12449** | **90,9 %** |

Auffällige Dateien: `calcraft-server/src/users.rs` 0 %,
`transfer_cmd.rs` 0 %, `main.rs` 14 %, `backup_cmd.rs` 74 %, `https.rs` 81 %,
`calcraft-store/src/backup.rs` 85 %, `oidc/mod.rs` 89 %.

## Kritische Pfade ohne Tests

Nach Risiko geordnet (Authentifizierung, Autorisierung, DAV-Schreibpfade,
Store-Transaktionen, Backup). Zeilenangaben beziehen sich auf den oben
genannten Stand.

1. **Deaktivieren eines Benutzers über die Admin-API widerruft die Sitzungen**
   (`api/admin.rs:100-103`, `delete_sessions_of`): nicht getestet. Ein
   Regressionsfehler ließe deaktivierte Benutzer mit bestehender Sitzung
   weiterarbeiten. Ebenso ungetestet: die Fehlerabbildung der
   Passwortrichtlinie beim Anlegen (`api/admin.rs:56-69`).
2. **Zwei-Faktor-Verwaltung:** `totp_disable` komplett
   (`api/account.rs:279-288`), falscher Code bei `totp_enable` inkl.
   `record_failure` (`244-249`), erneutes Setup bei aktiver 2FA (`212-220`).
3. **Login-Sperre (`auth.rs`):** Überlauf der Fehlertabelle
   (`MAX_TRACKED`, `243-248`), Zurücksetzen nach Ablauf des Sperrfensters
   (`256-257`, `450-451`), `clear_failures` bei laufenden Versuchen
   (`466-468`), Datenbankfehler beim App-Passwort (`294-304`) und beim
   Laden der Anmeldedaten (`346-348`).
4. **OIDC-Signaturprüfung (`oidc/jwt.rs`):** RS384/RS512/PS256/PS384/PS512
   und ES384 werden nie verifiziert (`114-119`, `130-135`); JWK-Filter
   `use != "sig"` und `key_ops` ohne `verify` (`201-206`) sowie unbekannte
   Kurven (`213`) sind ungetestet.
5. **OIDC-Provider (`oidc/mod.rs`):** Discovery-Cache und Wartezeit nach
   Fehlschlag (`345-359`), Rückfall auf gecachte Schlüssel bei
   JWKS-Fehler (`431-441`), `client_secret_post` (`582-590`) und
   `client_secret_file` (`254-269`).
6. **Tombstone-Bereinigung im Store** (`calcraft-store/src/items.rs:543-564`):
   das Löschen alter Tombstones oberhalb `MAX_TOMBSTONES` samt Setzen von
   `sync_floor` läuft in keinem Test; davon hängt die Ungültigkeit alter
   Sync-Tokens ab.
7. **Restore-Vorbedingungen und Fehlerpfade** (`backup.rs`): fehlende
   Sicherung, Sicherung = Datenbank, `-wal` neben der Sicherung
   (`552-567`); Abbruch, wenn die Kopie der alten Datenbank fehlschlägt
   (Platte voll, `612-620`), sowie Fehler beim Entfernen der Seitendateien
   bzw. beim Umbenennen (`631-636`).
8. **Backup-Prüfung und Sperren** (`backup.rs`): fehlgeschlagener
   `integrity_check` und „keine CalCraft-Datenbank“ (`517-527`), Ziel
   existiert ohne `--force` (`474-477`), exklusiv gesperrte Datenbank
   (`293-297`, `429-432`), Abbruch der Öffnungsschleife bei parallelen
   Migrationen (`340-343`).
9. **DAV-Schreibpfade** (`dav/objects.rs`, `dav/report.rs`): `DELETE` einer
   Collection mit ETag in `If-Match` → 412 (`objects.rs:280`), ungültige
   Bedingungs-Header (leere/zu viele ETags, `If-Match` und `If-None-Match`
   zugleich: `65`, `79`, `262`), Store-Fehler beim `DELETE` (`284`);
   REPORT-Limit 507 und nicht unterstützter Report (`report.rs:55-57`, `61`).
10. **Fehlerabbildung der REST-API** (`api/mod.rs`): `StoreError::Limit` →
    507 und 413 bei zu großen Bodys (`208-223`), interne Store-Fehler → 500
    (`236-241`); dazu die CLI-Pfade `users.rs` (0 %) und `transfer_cmd.rs`
    (0 %), die nur über das nicht instrumentierte Binary laufen.

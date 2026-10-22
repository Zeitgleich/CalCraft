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

## Stand 22.10.2026 (0.1.0-alpha.1)

Gemessen auf `main` (2f432f0) plus Versionsanhebung auf 0.1.0-alpha.1 mit
Rust 1.99.1 und cargo-llvm-cov 0.9.1, `cargo llvm-cov --workspace
--all-features`, macOS. Alle Tests grün (425 bestanden, 1 ignoriert).

| Crate | Zeilen abgedeckt | Abdeckung |
| --- | ---: | ---: |
| calcraft-core | 90 / 93 | 96,8 % |
| calcraft-dav | 725 / 747 | 97,1 % |
| calcraft-ical | 4507 / 4746 | 95,0 % |
| calcraft-server | 6443 / 7280 | 88,5 % |
| calcraft-store | 3814 / 4009 | 95,1 % |
| calcraft-ui | 406 / 455 | 89,2 % |
| calcraft-web | 52 / 71 | 73,2 % |
| **gesamt** | **16037 / 17401** | **92,2 %** |

Vorherige Messung (22.10.2026, b771c22, vor Papierkorb, MOVE/COPY,
Kontaktfotos und RRULE-Expansion): 11311 / 12449 Zeilen, 90,9 %.

Auffällige Dateien: `calcraft-server/src/users.rs` 0 %,
`transfer_cmd.rs` 0 %, `main.rs` 13 %, `trash.rs` 61 %, `backup_cmd.rs` 74 %,
`https.rs` 81 %, `calcraft-store/src/backup.rs` 85 %,
`calcraft-ical/src/xcal.rs` 84 %, `jcal.rs` 87 %, `oidc/mod.rs` 89 %.

## Kritische Pfade ohne Tests

Nach Risiko geordnet (Authentifizierung, Autorisierung, DAV-Schreibpfade,
Store-Transaktionen, Backup). Zeilenangaben beziehen sich auf den oben
genannten Stand.

1. **Benutzerverwaltung über die Admin-API** (`api/admin.rs`): Setzen bzw.
   Entziehen des Admin-Rechts (`103`) und die Fehlerabbildung der
   Passwortrichtlinie beim Anlegen (`56-69`) sind ungetestet. (Das
   Widerrufen der Sitzungen beim Deaktivieren läuft inzwischen in den Tests.)
2. **Zwei-Faktor-Verwaltung:** `totp_disable` komplett
   (`api/account.rs:342-358`), erneutes Setup bei aktiver 2FA (`244-252`)
   sowie die seltenen Ausgänge von `totp_enable` (bereits aktiv, kein
   Setup, schon benutzter Zeitschritt, Store-Fehler: `312-327`).
3. **Login-Sperre (`auth.rs`):** Überlauf der Fehlertabelle
   (`MAX_TRACKED`, `750-755`), Zurücksetzen nach Ablauf des Sperrfensters
   (`761-762`), Datenbankfehler beim Laden der Anmeldedaten (`523-525`,
   `562-564`) und beim App-Passwort (`529-531`), Abbildung von
   Store-Fehlern (`storage_outcome`, `142-150`).
4. **OIDC-Signaturprüfung (`oidc/jwt.rs`):** RS384/RS512/PS256/PS384/PS512
   und ES384 werden nie verifiziert (`114-119`, `130-135`); JWK-Filter
   `use != "sig"` und `key_ops` ohne `verify` (`201-206`) sowie unbekannte
   Kurven (`213`) sind ungetestet.
5. **OIDC-Provider (`oidc/mod.rs`):** Wartezeit nach fehlgeschlagener
   Discovery (`374-377`), Aufwärmen beim Start (`warm_up`, `345-359`), Rückfall auf gecachte Schlüssel bei
   JWKS-Fehler (`431-441`), `client_secret_post` (`582-590`) und
   `client_secret_file` (`254-269`).
6. **Tombstone-Bereinigung im Store** (`calcraft-store/src/items.rs:895-916`):
   das Löschen alter Tombstones oberhalb `MAX_TOMBSTONES` samt Setzen von
   `sync_floor` läuft in keinem Test; davon hängt die Ungültigkeit alter
   Sync-Tokens ab.
7. **Restore-Vorbedingungen und Fehlerpfade** (`backup.rs`): fehlende
   Sicherung, Sicherung = Datenbank, `-wal` neben der Sicherung
   (`602-619`); Abbruch, wenn die Kopie der alten Datenbank fehlschlägt
   (Platte voll, `661-665`), sowie Fehler beim Entfernen der Seitendateien
   bzw. beim Umbenennen (`679-687`).
8. **Backup-Prüfung und Sperren** (`backup.rs`): fehlgeschlagener
   `integrity_check` und „keine CalCraft-Datenbank“ (`568-578`), Ziel
   existiert ohne `--force` (`525-528`), exklusiv gesperrte Datenbank
   (`328-332`, `480-483`), Fehler beim Öffnen der Schreibverbindung
   (`351-354`).
9. **DAV-Schreibpfade** (`dav/objects.rs`, `dav/report.rs`): `DELETE` einer
   Collection mit ETag in `If-Match` → 412 (`objects.rs:316`), ungültige
   Bedingungs-Header (leere/zu viele ETags, `If-Match` und `If-None-Match`
   zugleich: `70`, `84`), Store-Fehler beim `DELETE` (`310`, `325`);
   REPORT-Limit 507 und nicht unterstützter Report (`report.rs:76-81`).
10. **Fehlerabbildung der REST-API** (`api/mod.rs`): `StoreError::Limit` →
    507 und 413 bei zu großen Bodys (`226-235`, `266-269`), interne
    Store-Fehler und `Busy` → 500/503 (`237-246`, `274-280`); dazu die
    CLI-Pfade `users.rs` (0 %) und `transfer_cmd.rs` (0 %), die nur über das
    nicht instrumentierte Binary laufen.
11. **Hintergrund-Bereinigung des Papierkorbs** (`calcraft-server/src/trash.rs:58-105`):
    der stündliche Lauf (`spawn_purge`, `purge_and_log`) und die Warnung bei
    `trash_retention_days = 0` mit vorhandenen Einträgen laufen in keinem
    Test; die Löschlogik selbst ist im Store getestet
    (`calcraft-store/src/trash.rs`, 94 %).

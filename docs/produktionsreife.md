# Produktionsreife – Anforderungen, Ist-Stand und Fahrplan

Stand: 22.10.2026, `main` mit M5a (OIDC), M6a (Import/Export), Phase-2-Limits und -Robustheit, Release-Pipeline, Update-Hinweis, CodeQL/Coverage und neuer Web-UI.
Dieses Dokument ist die Grundlage für die Freigabe einer stabilen Version.
Ein Punkt gilt erst als **erledigt**, wenn es einen automatisierten Test
(oder einen dokumentierten, wiederholbaren manuellen Nachweis) dafür gibt.

Legende: ✅ erledigt · 🟡 teilweise · ❌ offen · 👤 braucht reale Geräte/Personen

## P0 – Sicherheit und Mandantentrennung (vor Livebetrieb)

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Rechteprüfung für jede DAV-Methode und jeden Ressourcenpfad | ✅ | Zentrale Auflösung in `dav/paths.rs`: fremde Pfade → 404. Systematische Matrix in `crates/calcraft-server/tests/authz_matrix.rs`: jede Methode aus `ALLOWED_METHODS` (alle REPORT-Varianten, MKCOL, MKCALENDAR, PROPPATCH …) plus nicht implementierte Methoden × jeder Ressourcentyp (Root, Principal, Homes, Collections, Objekte, ungemappte/fehlende Pfade, Traversal- und Percent-Encoding-Varianten) × eigener/fremder/nicht existierender Benutzer × alle Credential-Arten; exakte Statuscodes, fremd = nicht existent (byte-identisch). |
| Tests gegen unberechtigten Zugriff auf fremde Kalender/Kontakte | ✅ | Isolationstests für DAV, multiget-hrefs, Sync-Tokens, REST-API, dazu `tests/authz_matrix.rs`: Marker-Strings in fremden Daten dürfen in keiner Antwort auftauchen, fremde Daten (ETag/Inhalt/CTag) bleiben nach jedem schreibenden Versuch unverändert, jede REST-Route aus `api::ROUTES` ohne Session, mit fremder Session und ohne CSRF-Header. Der Test schlägt fehl, wenn `ALLOWED_METHODS` oder `ROUTES` einen Eintrag ohne Matrix-Abdeckung enthält. |
| Schutz gegen Brute-Force | ✅ | Sperre pro Konto (5/15 min) plus pro Client-IP (`server.max_login_failures_per_client`, 20/15 min, auch für unbekannte Namen und gesperrte Konten; Antwort wie falsches Passwort). Abgebrochene Anfragen zählen als Fehlversuch, sobald der Vergleich läuft; Warten paralleler Versuche begrenzt (8 pro Name, 5 s); 2FA-Aktivierung mit derselben Sperre. Client-IP per `server.trusted_proxies` (Standard Loopback; Warnung bei `X-Forwarded-For` von nicht vertrautem Peer). Tests: `auth/tests.rs`, `tests/limits.rs`, `tests/api.rs`. **Akzeptiertes Restrisiko (dokumentiert):** Kontosperre durch Dritte auslösbar (Kompromiss); ein Angreifer mit sehr vielen IPv6-Netzen kann die Zählung pro Client aussetzen (Sperre pro Konto bleibt). |
| Schutz gegen Missbrauch und übergroße XML-/HTTP-Anfragen | 🟡 | Body-Limit, Timeout, XML-Limits (Tiefe, Elemente, keine DTD), iCal/vCard-Limits, REPORT-Budgets, Quota 512 MiB, Import-Budgets, Fuzzing. Global `server.max_concurrent_requests` (256 → 503 + `Retry-After`, Health ausgenommen), pro Client 32 parallel und Token-Bucket 120/20 s⁻¹ (429), IPv6 je /64, Tabellen hart begrenzt (`tests/limits.rs`). **Lücke:** keine Begrenzung offener Keep-alive-Verbindungen (nur `LimitNOFILE`), kein Header-Größen-Test. |
| Sichere Passwortspeicherung | ✅ | Argon2id, Dummy-Hash gegen Timing, App-Passwörter nur als Hash, Wiederherstellungscodes gehasht. |
| Sitzungsverwaltung | ✅ | 256-bit-Token, nur Hash gespeichert, HttpOnly/SameSite=Strict/Secure, Ablauf 12 h, Ende bei Logout/Passwortwechsel/Sperre, CSRF-Header. |
| TLS-Konfiguration | 🟡 | Dokumentiert und getestet (nginx/Apache, `docs/installation.md`). **Lücke:** kein automatischer Check der Proxy-Konfiguration (z. B. Warnung, wenn ohne HTTPS genutzt). |
| Prüfung auf bekannte Schwachstellen (direkt + transitiv) | 🟡 | `cargo deny` in jeder CI und täglich, Dependabot, CodeQL (`security-extended`) für Rust und Workflows bei jedem PR und wöchentlich (`.github/workflows/codeql.yml`, Testcode ausgenommen). **Lücke:** kein SBOM, kein `cargo-geiger`-Report. |

## P0 – Datenintegrität und Wiederherstellung (vor Livebetrieb)

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Atomare Schreibvorgänge, korrekte Transaktionen | ✅ | `BEGIN IMMEDIATE` für alle Schreibtransaktionen, alle Schreibzugriffe über eine eigene Verbindung in fester Reihenfolge (kein unfaires Warten im SQLite-Busy-Handler), PROPPATCH atomar, Änderungsprotokoll in derselben Transaktion. |
| Keine verlorenen Änderungen bei parallelen Zugriffen | ✅ | ETag/`If-Match` → 412, UID-Prüfung atomar. Stresstest mit 32 parallelen Clients (`tests/stress.rs`, auf Linux, macOS und Windows): kein 500, keine verlorenen Updates, Sync-Protokoll konsistent; Überlast → 503 + `Retry-After`. |
| Backups mit dokumentiertem **und getestetem** Restore | ✅ | `calcraft backup` (online per `VACUUM INTO`, verifiziert, atomar, 0600) und `calcraft restore` (nur bei gestopptem Server, erkannt über Dateisperre; verweigert neuere Backups; alte DB bleibt erhalten). Tests: `calcraft-store` `backup/tests.rs`, `calcraft-server/tests/backup.rs`, CI-Job `backup` (`scripts/backup-smoke-test.sh`). Doku: `docs/backup-and-upgrade.md`. |
| Sichere Datenbankmigrationen inkl. Rollback-Strategie | ✅ | Vor Migrationen verifiziertes Backup `*.pre-migration-<v>-<ts>` (konfigurierbar, letzte N), bei Fehler keine Migration; Migration nur mit exklusiver Sperre; neuere/abgebrochene Schemata und fremde SQLite-Dateien werden abgelehnt. Rollback = Pre-Migration-Backup mit alter Version einspielen. Test „jeder ältere Migrationsstand → aktuell“ (`upgrade_from_every_previous_migration`, auch im CI-Job `backup`). |
| Korrekte Behandlung von ETags, `If-Match`, Sync-Tokens | ✅ | RFC-konform inkl. Listen und Weak-ETags, Sync-Tokens an Collection gebunden (Salt), abgeschnittene Antworten mit Fortsetzung; Tests. |

## P1 – Kalenderkompatibilität

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Serientermine mit Ausnahmen und verschobenen Einzelterminen | 🟡 | Speichern/Abrufen inkl. `RECURRENCE-ID`-Overrides. **Lücke:** keine exakte RRULE-Expansion, Zeitraumabfragen konservativ (liefern ggf. zu viel); kein `expand`. |
| Zeitzonen, Sommerzeit, ganztägige Termine | 🟡 | Ganztägig und lokale Zeiten werden konservativ indexiert (±14 h). **Lücke:** keine Zeitzonen-Datenbank, keine Tests über DST-Wechsel. |
| Konflikte bei Änderungen auf mehreren Geräten | 🟡 | 412 bei veralteter ETag. 👤 Verhalten realer Clients bei Konflikten nicht geprüft. |
| Löschen | ✅ | DELETE mit Vorbedingungen, Tombstones für Sync. |
| Verschieben | ❌ | `MOVE`/`COPY` nicht implementiert (405). Manche Clients verschieben Termine zwischen Kalendern per MOVE. |
| Freigeben | ❌ | Freigaben/ACL (M5) noch nicht umgesetzt. |
| Wiederherstellen (Papierkorb) | ❌ | Kein Soft-Delete/Papierkorb. |
| CardDAV: Fotos, Gruppen, vCard 3.0/4.0 | 🟡 | vCard 3.0/4.0 validiert, Gruppen werden unverändert gespeichert. **Lücke:** Fotos > 1 MiB werden abgelehnt (Limit dokumentieren/konfigurierbar), keine Tests mit Fotos und Apple-Gruppen (`X-ADDRESSBOOKSERVER-KIND`). |

## P1 – Betrieb und Überwachung

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Strukturierte Logs ohne Passwörter/sensible Inhalte | 🟡 | JSON-Logs, Trace-Spans ohne Query-String. `tests/log_secrets.rs` prüft Login, Kontosperre, TOTP, Wiederherstellungscodes, App-Passwörter, Cookies, Passwortwechsel, Admin-Anlage, Kalender-/Kontaktinhalte und Query-Strings; `tests/failures.rs` prüft die Fehlerlogs. **Lücke:** OIDC-Ablauf nicht im Log-Test. |
| Metriken | ❌ | Kein `/metrics` (Prometheus). |
| Healthchecks | ✅ | `/healthz`, `/readyz` (prüft Datenbank). |
| Alarmierung | ❌ | Keine Doku/Beispielregeln (abhängig von Metriken). |
| Graceful Shutdown | ✅ | SIGTERM/Ctrl+C, laufende Anfragen werden beendet. |
| Verhalten bei Datenbankausfällen | 🟡 | Nicht beschreibbar → Start verweigert bzw. 500 ohne Details, Lesen geht weiter; Platte voll → 507 ohne Teilzustand; Erholung ohne Neustart; abgebrochene Uploads ohne Spuren (`tests/failures.rs`). **Lücke:** `/readyz` bleibt 200, solange gelesen werden kann. |
| Begrenzung von Speicher, CPU, Anfragen, Verbindungen | 🟡 | Globale und Client-Limits (s. P0), Argon2 auf 4 gleichzeitige Berechnungen begrenzt (auch bei Abbruch), systemd-Grenzen (`LimitNOFILE`, `MemoryMax`, `TasksMax`) in `docs/installation.md`. **Lücke:** Keep-alive-Verbindungen nur über `LimitNOFILE` begrenzt. |
| Dokumentierte Installation, Updates, Wiederherstellung | ✅ | `docs/installation.md`, `docs/backup-and-upgrade.md`, `docs/release-verification.md`; Proxy-Konfigurationen und Restore getestet. |

## Fahrplan zur Freigabe

Die Reihenfolge folgt den Prioritäten; Release-Pipeline und Update-Hinweis
laufen parallel, weil sie für den Pilotbetrieb gebraucht werden.

### Phase 1 – Audit und Bestandsaufnahme (≈ Woche 1)

- [x] Abgleich aller Funktionen gegen diese Liste (dieses Dokument)
- [x] Testabdeckung messen (`cargo llvm-cov`, 90,9 % Zeilen, Stand 22.10.2026; Workflow `coverage.yml`), kritische Lücken in `docs/coverage.md`
- [x] Statische Analyse: CodeQL für Rust und Workflows (`codeql.yml`) – `cargo-geiger`-Report steht noch aus
- [ ] Ergebnis: priorisierte Fehlerliste als GitHub-Issues

### Phase 2 – Sicherheit und Datenintegrität (≈ Woche 2–3)

- [x] **Autorisierungs-Matrix-Test:** jede DAV-Methode × Ressourcentyp × fremder/eigener Benutzer, plus jeder REST-Endpunkt; der Test schlägt fehl, wenn eine neue Route nicht in der Matrix steht (`crates/calcraft-server/tests/authz_matrix.rs`)
- [x] Begrenzung gleichzeitiger Anfragen (global und pro Client-IP, mit Trusted-Proxy-Konfiguration für `X-Forwarded-For`)
- [x] Stresstest paralleler Schreiber (keine verlorenen Änderungen, kein 500 bei `SQLITE_BUSY`)
- [x] `calcraft backup` / `calcraft restore` mit Integritätsprüfung; Restore-Test in der CI
- [x] Automatisches Backup vor jeder Migration, dokumentierte Rollback-Strategie (Restore des Pre-Migration-Backups + alte Version), CI-Test „DB von Release N → N+1“
- [x] Fehlertests: Datenbank nicht beschreibbar, Platte voll, abgebrochene Uploads
- [ ] Fuzzing ausbauen (REST-JSON, Pfade/hrefs)
- [x] Log-Test: keine Passwörter, Tokens, Codes oder Kalenderinhalte in Logs

### Phase 3 – Kompatibilität und Ende-zu-Ende-Tests (≈ Woche 3–5)

- [ ] `MOVE`/`COPY` für Objekte zwischen eigenen Collections
- [ ] RRULE-Expansion mit Budget (exakte Zeitraumabfragen, `expand`), Zeitzonen-Datenbank, DST-Tests
- [ ] Papierkorb (Soft-Delete mit Wiederherstellung)
- [ ] CardDAV: Fotos (Limit konfigurierbar), Apple-Gruppen, Tests
- [ ] Freigaben/ACL (M5b)
- [ ] 👤 Reale Clients: Apple Kalender/Kontakte (macOS, iOS), Thunderbird, DAVx⁵, GNOME – Testprotokoll in `docs/compat/`
- [ ] Serientermine, Offlinebetrieb und Konflikte mit realen Clients
- [ ] Langzeit-/Lasttest: mehrere Benutzer, große Kalender (≥ 50 000 Objekte)

### Phase 4 – Pilotbetrieb und Release (≈ Woche 5–6)

- [x] Release-Pipeline (`.github/workflows/release.yml`, Tag `vX.Y.Z`): Binaries für Linux (x86_64/aarch64), macOS (aarch64/x86_64) und Windows (x86_64) mit eingebetteter Web-UI, `SHA256SUMS`, Build-Provenance (Sigstore, keyless) für jedes Archiv und `SHA256SUMS`, Multi-Arch-Docker-Image auf ghcr.io, per Digest mit cosign signiert und attestiert; Prüfanleitung `docs/release-verification.md`, Checkliste `docs/releasing.md`. „Signiert“ heißt hier Sigstore-Attestierung, **keine** Apple-/Microsoft-Codesignatur (offen, siehe Client-Phase C4). Probelauf per `workflow_dispatch` erfolgreich (alle 5 Binaries, Docker amd64/arm64). **Nachweis offen:** ein echter Tag-Lauf (Attestierung, Push, cosign, Release-Anlage).
- [x] Optionaler Update-Hinweis im Admin-Bereich (standardmäßig aus): `[updates] check`, fester GitHub-Endpunkt, `GET /api/v1/admin/update` (`crates/calcraft-server/src/updates.rs`, PLAN.md §12)
- [ ] `/metrics` (Prometheus) und Beispiel-Alarmregeln
- [ ] Installation auf frischem System nach `docs/installation.md` reproduzieren
- [ ] Update inkl. Migration von Vorversion testen
- [ ] 👤 Pilot mit wenigen Testbenutzern im Alltag (mind. 2 Wochen)
- [ ] Erst danach: stabile Version 1.0

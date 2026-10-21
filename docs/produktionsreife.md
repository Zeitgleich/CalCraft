# Produktionsreife – Anforderungen, Ist-Stand und Fahrplan

Stand: 21.10.2026, `main` nach M4b (M5a OIDC und M6a Import/Export in Review).
Dieses Dokument ist die Grundlage für die Freigabe einer stabilen Version.
Ein Punkt gilt erst als **erledigt**, wenn es einen automatisierten Test
(oder einen dokumentierten, wiederholbaren manuellen Nachweis) dafür gibt.

Legende: ✅ erledigt · 🟡 teilweise · ❌ offen · 👤 braucht reale Geräte/Personen

## P0 – Sicherheit und Mandantentrennung (vor Livebetrieb)

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Rechteprüfung für jede DAV-Methode und jeden Ressourcenpfad | ✅ | Zentrale Auflösung in `dav/paths.rs`: fremde Pfade → 404. Systematische Matrix in `crates/calcraft-server/tests/authz_matrix.rs`: jede Methode aus `ALLOWED_METHODS` (alle REPORT-Varianten, MKCOL, MKCALENDAR, PROPPATCH …) plus nicht implementierte Methoden × jeder Ressourcentyp (Root, Principal, Homes, Collections, Objekte, ungemappte/fehlende Pfade, Traversal- und Percent-Encoding-Varianten) × eigener/fremder/nicht existierender Benutzer × alle Credential-Arten; exakte Statuscodes, fremd = nicht existent (byte-identisch). |
| Tests gegen unberechtigten Zugriff auf fremde Kalender/Kontakte | ✅ | Isolationstests für DAV, multiget-hrefs, Sync-Tokens, REST-API, dazu `tests/authz_matrix.rs`: Marker-Strings in fremden Daten dürfen in keiner Antwort auftauchen, fremde Daten (ETag/Inhalt/CTag) bleiben nach jedem schreibenden Versuch unverändert, jede REST-Route aus `api::ROUTES` ohne Session, mit fremder Session und ohne CSRF-Header. Der Test schlägt fehl, wenn `ALLOWED_METHODS` oder `ROUTES` einen Eintrag ohne Matrix-Abdeckung enthält. |
| Schutz gegen Brute-Force | 🟡 | Sperre pro Konto (5 Fehlversuche/15 min) inkl. paralleler Anfragen und 2. Faktor; gleiche Antwort für gesperrt/falsch. **Lücke:** keine Begrenzung pro Client-IP (braucht Trusted-Proxy-Konfiguration), Kontosperre ist durch Dritte auslösbar (dokumentierter Kompromiss). |
| Schutz gegen Missbrauch und übergroße XML-/HTTP-Anfragen | 🟡 | Body-Limit, Timeout, XML-Limits (Tiefe, Elemente, keine DTD), iCal/vCard-Limits, REPORT-Budgets, Quota 512 MiB, Import-Budgets, Fuzzing. **Lücke:** keine Begrenzung gleichzeitiger Verbindungen/Anfragen insgesamt und pro Client, kein Header-Größen-Test. |
| Sichere Passwortspeicherung | ✅ | Argon2id, Dummy-Hash gegen Timing, App-Passwörter nur als Hash, Wiederherstellungscodes gehasht. |
| Sitzungsverwaltung | ✅ | 256-bit-Token, nur Hash gespeichert, HttpOnly/SameSite=Strict/Secure, Ablauf 12 h, Ende bei Logout/Passwortwechsel/Sperre, CSRF-Header. |
| TLS-Konfiguration | 🟡 | Dokumentiert und getestet (nginx/Apache, `docs/installation.md`). **Lücke:** kein automatischer Check der Proxy-Konfiguration (z. B. Warnung, wenn ohne HTTPS genutzt). |
| Prüfung auf bekannte Schwachstellen (direkt + transitiv) | 🟡 | `cargo deny` in jeder CI und täglich, Dependabot. **Lücke:** keine statische Codeanalyse (CodeQL/semgrep), kein SBOM. |

## P0 – Datenintegrität und Wiederherstellung (vor Livebetrieb)

| Anforderung | Stand | Nachweis / Lücke |
|---|---|---|
| Atomare Schreibvorgänge, korrekte Transaktionen | ✅ | `BEGIN IMMEDIATE` für alle Objekt-Schreibvorgänge, PROPPATCH atomar, Änderungsprotokoll in derselben Transaktion. |
| Keine verlorenen Änderungen bei parallelen Zugriffen | 🟡 | ETag/`If-Match` → 412, UID-Prüfung atomar. **Lücke:** kein Stresstest mit vielen parallelen Schreibern; Verhalten bei `SQLITE_BUSY` unter Last nicht gemessen. |
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
| Strukturierte Logs ohne Passwörter/sensible Inhalte | 🟡 | JSON-Logs, keine Passwörter/Tokens, Trace-Spans ohne Query-String (M5a). **Lücke:** kein Test, der Logs auf Geheimnisse und Kalenderinhalte prüft. |
| Metriken | ❌ | Kein `/metrics` (Prometheus). |
| Healthchecks | ✅ | `/healthz`, `/readyz` (prüft Datenbank). |
| Alarmierung | ❌ | Keine Doku/Beispielregeln (abhängig von Metriken). |
| Graceful Shutdown | ✅ | SIGTERM/Ctrl+C, laufende Anfragen werden beendet. |
| Verhalten bei Datenbankausfällen | 🟡 | `/readyz` → 503, Fehler → 500 ohne Details. **Lücke:** kein Test (z. B. Datei nicht beschreibbar, Platte voll). |
| Begrenzung von Speicher, CPU, Anfragen, Verbindungen | 🟡 | Viele Einzellimits (s. P0). **Lücke:** kein globales Limit gleichzeitiger Anfragen/Verbindungen, keine systemd-Ressourcengrenzen in der Doku. |
| Dokumentierte Installation, Updates, Wiederherstellung | ✅ | `docs/installation.md` (PR #8), Proxy-Konfigurationen getestet. Restore-Test fehlt (s. P0). |

## Fahrplan zur Freigabe

Die Reihenfolge folgt den Prioritäten; Release-Pipeline und Update-Hinweis
laufen parallel, weil sie für den Pilotbetrieb gebraucht werden.

### Phase 1 – Audit und Bestandsaufnahme (≈ Woche 1)

- [x] Abgleich aller Funktionen gegen diese Liste (dieses Dokument)
- [ ] Testabdeckung messen (`cargo llvm-cov`), kritische Pfade ohne Tests auflisten
- [ ] Statische Analyse ergänzen (CodeQL für Rust, `cargo-geiger`-Report für unsafe in Abhängigkeiten)
- [ ] Ergebnis: priorisierte Fehlerliste als GitHub-Issues

### Phase 2 – Sicherheit und Datenintegrität (≈ Woche 2–3)

- [x] **Autorisierungs-Matrix-Test:** jede DAV-Methode × Ressourcentyp × fremder/eigener Benutzer, plus jeder REST-Endpunkt; der Test schlägt fehl, wenn eine neue Route nicht in der Matrix steht (`crates/calcraft-server/tests/authz_matrix.rs`)
- [ ] Begrenzung gleichzeitiger Anfragen (global und pro Client-IP, mit Trusted-Proxy-Konfiguration für `X-Forwarded-For`)
- [ ] Stresstest paralleler Schreiber (keine verlorenen Änderungen, kein 500 bei `SQLITE_BUSY`)
- [x] `calcraft backup` / `calcraft restore` mit Integritätsprüfung; Restore-Test in der CI
- [x] Automatisches Backup vor jeder Migration, dokumentierte Rollback-Strategie (Restore des Pre-Migration-Backups + alte Version), CI-Test „DB von Release N → N+1“
- [ ] Fehlertests: Datenbank nicht beschreibbar, Platte voll, abgebrochene Uploads
- [ ] Fuzzing ausbauen (REST-JSON, Pfade/hrefs)
- [ ] Log-Test: keine Passwörter, Tokens, Codes oder Kalenderinhalte in Logs

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

- [ ] Release-Pipeline: signierte Binaries (Linux/macOS/Windows), Prüfsummen, Build-Provenance, signiertes Docker-Image
- [ ] Optionaler Update-Hinweis im Admin-Bereich (standardmäßig aus)
- [ ] `/metrics` (Prometheus) und Beispiel-Alarmregeln
- [ ] Installation auf frischem System nach `docs/installation.md` reproduzieren
- [ ] Update inkl. Migration von Vorversion testen
- [ ] 👤 Pilot mit wenigen Testbenutzern im Alltag (mind. 2 Wochen)
- [ ] Erst danach: stabile Version 1.0

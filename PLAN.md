# CalCraft – Projektplan

Selbst gehosteter CalDAV-/CardDAV-Server in Rust mit optionaler Web-Oberfläche.
Alternative zu Radicale und Baïkal. Lizenz: **AGPL-3.0-or-later**.

## 1. Ziele

- Ein einziges, statisch gelinktes Binary (optional Docker-Image).
- Standardkonformes CalDAV/CardDAV, getestet gegen die gängigen Clients.
- **Sicherheit hat oberste Priorität** – sie ist Teil der Architektur, nicht nachträglich.
- Web-UI (Leptos) und REST-API mit OpenAPI/Swagger, beide abschaltbar.
- Nutzerverwaltung lokal oder per SSO (OIDC, z. B. Keycloak).
- Unterstützung aller gängigen Kalender- und Kontaktformate.
- Läuft auf **Linux, macOS und Windows** (x86_64 und aarch64).
- Zusätzlich ein **eigener Kalender-Client** (Desktop, später mobil).

**Nicht im Scope (v1):** Mail/Groupware, Exchange/EWS/ActiveSync, Clustering.

**Abgrenzung:** RustiCal (Rust, AGPL) und Stalwart (Mail + DAV) existieren bereits.
CalCraft hebt sich ab durch vollwertige Kalender-Web-UI, SSO mit App-Passwörtern,
REST/OpenAPI, breite Format-Unterstützung und konsequenten Security-Fokus.

## 2. Standards

| RFC | Inhalt | Meilenstein |
|---|---|---|
| 4918 | WebDAV | M1 |
| 5397 / 6764 | current-user-principal, `.well-known` | M1 |
| 4791 | CalDAV | M2 |
| 5545 | iCalendar | M2 |
| 6578 | sync-collection | M3 |
| 6352 / 6350 / 2426 | CardDAV, vCard 4.0 / 3.0 | M3 |
| 3744 (Teilmenge) | ACL / Freigaben | M5 |
| 7265 / 6321 / 7095 | jCal, xCal, jCard | M6 |
| 6638 | Scheduling (iTIP/iMIP) | nach v1 |

## 3. Architektur

Cargo-Workspace:

```
crates/
├── calcraft-core    # Domain-Typen, Fehler, Validierung (wasm-kompatibel)
├── calcraft-ical    # iCal/vCard-Parsing, Limits, RRULE-Expansion (wasm-kompatibel)
├── calcraft-dav     # WebDAV/CalDAV/CardDAV-Protokoll, XML
├── calcraft-store   # Storage-Trait, SQLite (Default) + PostgreSQL
├── calcraft-api     # REST-API + OpenAPI (utoipa); einzige API, auch für die UI
├── calcraft-web     # Leptos-App (SSR + Hydration), Feature `web-ui`
├── calcraft-ui      # Wiederverwendbare Leptos-Komponenten (Kalenderansichten), für Web-UI und Client
├── calcraft-client  # CalDAV/CardDAV-Client-Bibliothek (Sync, Offline-Cache)
└── calcraft-server  # Binary: Config, CLI, Routing, Middleware
apps/
└── calcraft-desktop # Tauri-2-App (Linux, macOS, Windows; später iOS/Android)
```

`calcraft-ui`, `calcraft-client` und `apps/` entstehen erst mit dem Client-Track.

Grundsätze:

- **Eine API:** Die Web-UI nutzt ausschließlich die REST-API, keine Leptos Server Functions.
- **Shared Code:** `calcraft-core` und `calcraft-ical` kompilieren auch für `wasm32`.
- **Speicher:** Rohdaten (`.ics`/`.vcf`) unverändert als Blob + extrahierter Index
  (UID, DTSTART/DTEND, RRULE-Flag) für Time-Range-Queries. ETag = Inhalts-Hash.
  Changelog-Tabelle mit monotoner Sync-ID für `sync-token`/`getctag`.

### Feature-Flags

| Ebene | Schalter |
|---|---|
| Compile-Zeit (Cargo) | `web-ui`, `api`, `swagger-ui`, `postgres` |
| Laufzeit (Config/ENV/CLI) | `web_ui.enabled`, `api.enabled`, `api.swagger_ui` |

Ohne Feature ist der Code nicht im Binary (kleinere Angriffsfläche).

## 4. Sicherheit

**Code & Lieferkette**
- `#![forbid(unsafe_code)]` in allen Crates.
- `cargo-deny` (Lizenzen, Advisories, Quellen) und `cargo-audit` in der CI.
- Renovate/Dependabot, SBOM (CycloneDX), signierte Releases (cosign).
- Docker: distroless, non-root, read-only rootfs.
- `SECURITY.md`, Threat Model, externes Review vor v1.0.

**Protokoll & Parser**
- XML ohne DTD/Entities (XXE, Billion Laughs); Tiefen-/Größenlimits.
- iCal-Limits: max. Item-Größe, Properties, Verschachtelung, RRULE-Instanzen und -Zeitraum.
- Dauer-Fuzzing (`cargo-fuzz`) aller Parser.
- Body-Limits, Timeouts, Rate-Limiting.

**Auth & Web**
- Argon2id, Lockout/Backoff, timing-safe Vergleiche.
- Lokal: TOTP + WebAuthn/Passkeys.
- OIDC (Authorization Code + PKCE), Rollen aus Claims, Backchannel-Logout.
- **App-Passwörter** pro Gerät (widerrufbar, mit Scopes) – nötig, da DAV-Clients kein OIDC können.
- Cookies `HttpOnly; Secure; SameSite=Strict`, CSRF-Token.
- Strikte CSP (`script-src 'self' 'wasm-unsafe-eval'`, Nonces), HSTS, `X-Frame-Options: DENY`.
- CORS standardmäßig aus; Trusted-Proxy-Liste für `X-Forwarded-*`.
- SSRF-Schutz für Webcal-Abos (private Netze blocken, Redirect-Limit).
- Audit-Log für sicherheitsrelevante Aktionen.
- Mandanten-Isolation in jeder Query, mit eigenen Tests.

**Lehren aus früheren Angriffen** (Details: [docs/security/angriffshistorie.md](docs/security/angriffshistorie.md)):
- Zentrale Autorisierungsschicht: jede Abfrage ist auf den anfragenden Benutzer beschränkt, auch PROPPATCH,
  REPORT/multiget pro Objekt, sync-token, Papierkorb/Hard-Delete und MOVE/COPY-Ziel; ACL-Properties sind read-only.
  Testmatrix Methode × Ressource × Rolle.
- Recurrence-Budget (Instanzen, Zeitfenster, COUNT/INTERVAL/SECONDLY, VALARM REPEAT); derselbe Parser für
  Validierung und Expansion; keine Panics auf Eingaben.
- Web-UI rendert Kalenderdaten nur als Text, URI-Schema-Allowlist, Fotos nie ungeprüft als `image/*`.
- DAV-Endpunkte akzeptieren keine Cookie-Sessions; absolute URLs nur aus `public_base_url`.
- Tokens ≥ 128 Bit, nur gehasht gespeichert; bei aktivierter 2FA Basic Auth nur mit App-Passwörtern.
- OIDC: state, nonce, PKCE, exakte Redirect-URI, kein Account-Linking über E-Mail-Gleichheit.
- iTIP/iMIP: ORGANIZER muss verifizierte Absenderadresse sein, Rate-Limits für Einladungen.
- Webcal-Fetcher: DNS-Pinning, alle privaten/gemappten Bereiche blocken, jeden Redirect neu prüfen.
- CSV-Export mit Formel-Escaping, verschlüsselte Backups.
- Die Regressionstests aus Abschnitt 5 des Berichts werden im jeweiligen Meilenstein übernommen.

**AGPL §13:** Die Web-UI zeigt einen Link auf den Quellcode der laufenden Version.

## 5. Formate

iCalendar (RFC 5545), jCal, xCal, vCard 3.0/4.0, jCard, CSV-Import/-Export,
vCalendar 1.0 (Import, best effort), Webcal-Abos (eingehend und veröffentlichen).

## 6. Web-UI (Leptos)

| Stufe | Inhalt |
|---|---|
| UI-1 | Login, Kalender-/Adressbuchliste, Einstellungen, App-Passwörter, Admin |
| UI-2 | Agenda- und Monatsansicht, Termin-Dialog |
| UI-3 | Woche/Tag mit Zeitraster, Layout überlappender Termine |
| UI-4 | Drag & Drop, Resize, Touch, Tastatur, ARIA |
| UI-5 | Kontakte, Freigaben, i18n (de/en), Dark Mode |

Build mit `cargo-leptos`, `wasm-opt -Oz`; Ziel < 500 KB gzip. Kein npm.

## 7. Plattformen

- Server: Linux, macOS, Windows (x86_64 + aarch64); CI testet alle drei Systeme.
- Kein plattformspezifischer Code außer Signal-Handling und Dienst-Integration.
- Dienst-Integration: systemd-Unit, launchd-Plist, Windows-Dienst.
- Release-Artefakte pro Plattform via `cargo-dist`, signiert.

## 8. Kalender-Client

Eigener Client auf Basis von **Tauri 2** + Leptos:

- Teilt die Kalender-Komponenten (`calcraft-ui`) mit der Web-UI.
- Spricht standardkonformes CalDAV/CardDAV über `calcraft-client` – funktioniert
  also auch mit anderen Servern (Radicale, Baïkal, Nextcloud, iCloud, …).
- Offline-fähig: lokaler Cache (SQLite), Sync über `sync-collection`, Konfliktauflösung über ETags.
- Zugangsdaten im System-Schlüsselbund (Keychain, Credential Manager, Secret Service).
- Benachrichtigungen für Erinnerungen über das Betriebssystem.
- Login per App-Passwort oder OIDC (Browser-Flow mit PKCE, Loopback-Redirect).

| | Inhalt |
|---|---|
| **C1** | `calcraft-client`: Discovery, PROPFIND, REPORT, Sync, Tests gegen CalCraft und Radicale |
| **C2** | `calcraft-ui` aus der Web-UI herauslösen; Tauri-Grundgerüst, Konten-Verwaltung |
| **C3** | Offline-Cache, Hintergrund-Sync, Erinnerungen |
| **C4** | Signierte Installer (MSI, DMG notarisiert, AppImage/Flatpak), Auto-Update mit Signaturprüfung |
| **C5** | Mobil (iOS/Android) über Tauri Mobile |

Der Client-Track startet nach M4 (wenn Web-UI-Komponenten existieren).

## 9. Meilensteine

| | Inhalt |
|---|---|
| **M0** ✅ | Workspace, Lizenz, Doku, CI auf Linux/macOS/Windows (fmt, clippy, test, deny), Dockerfile, Server-Grundgerüst mit Config, Feature-Flags, Security-Headern, Health-Endpunkten, `.well-known` |
| **M1** ✅ | WebDAV-Kern: OPTIONS, PROPFIND (Depth 0/1), Principal Discovery, Basic Auth, Storage (SQLite) |
| **M2** ✅ | CalDAV: MKCALENDAR, PUT/GET/DELETE mit Preconditions, calendar-query, calendar-multiget; Parser-Limits + Fuzzing |
| **M3** | sync-collection, CardDAV |
| **M4** | REST-API + OpenAPI, lokale Auth mit 2FA, App-Passwörter, Web-UI Stufe 1–2 |
| **M5** | OIDC/Keycloak, Freigaben, Gruppen, Erinnerungen (SMTP), Web-UI Stufe 3–4 |
| **M6** | Import/Export aller Formate, Webcal mit SSRF-Schutz, Backup/Restore, Migration aus Radicale/Baïkal |
| **M7** | Externes Security-Review, Doku, Paketierung, Release v1.0 |

## 10. Weitere Anforderungen

- Erinnerungen (VALARM per E-Mail), optional Webhooks/ntfy.
- Backup/Restore per CLI, Datenexport und Kontolöschung (DSGVO).
- Papierkorb (Soft-Delete) mit Wiederherstellung.
- Gruppen-, Team- und Ressourcenkalender; Free/Busy; Quotas.
- Observability: `/healthz`, `/readyz`, Prometheus `/metrics`, JSON-Logs.
- Paketierung: Docker, Helm, `.deb`/`.rpm`, Homebrew, NixOS-Modul.

## 11. Tests

- Litmus (WebDAV), Apple CalDAVTester.
- Client-Matrix: Thunderbird, Apple Kalender (macOS/iOS), DAVx⁵, Evolution, GNOME Kalender.
- Aufgezeichnete Client-Requests als Golden-File-Regressionstests.
- Fuzzing in der CI.

## 12. Bekannte Einschränkungen (Stand M2)

- `calendar-query` filtert nach Komponententyp und Zeitraum. Der Zeitraum wird konservativ bestimmt:
  Wiederholungen ohne `UNTIL` gelten als offen, lokale Zeiten werden um ±14 h erweitert. Es können also
  zusätzliche Objekte zurückkommen, aber nie fehlen. Exakte RRULE-Expansion (mit Budget) folgt später.
- `prop-filter`, `param-filter` und `text-match` werden mit `CALDAV:supported-filter` (403) abgelehnt
  statt stillschweigend ignoriert.
- `calendar-data` liefert immer das ganze Objekt (keine Teilabfrage, kein `expand`).
- Unbekannte Properties bei MKCALENDAR (z. B. `calendar-order`) werden nicht gespeichert.

## 13. Offene Entscheidungen

- CardDAV bereits in v1? (aktuell geplant: ja, M3)
- Lizenz der wiederverwendbaren Bibliotheken (`calcraft-ical`, ggf. `leptos-calendar`):
  AGPL wie der Rest oder `MIT OR Apache-2.0`?
- Contributions: DCO (`Signed-off-by`) statt CLA – sofern keine Dual-Lizenz geplant ist.

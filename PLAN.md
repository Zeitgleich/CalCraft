# CalCraft – Projektplan

Selbst gehosteter CalDAV-/CardDAV-Server in Rust mit optionaler Web-Oberfläche.
Alternative zu Radicale und Baïkal. Lizenz: **AGPL-3.0-or-later**.

**Freigabe:** Anforderungen, Ist-Stand und Fahrplan bis zur stabilen Version stehen in
[docs/produktionsreife.md](docs/produktionsreife.md) und haben Vorrang vor neuen Funktionen.

**Stand 23.10.2026:** Vorabversion **0.1.0-alpha.3** (CalDAV-Scheduling zwischen lokalen Nutzern
(RFC 6638, Stufe A), Managed Attachments (RFC 8607), M0–M4b, M5a, M6 mit Webcal-Abos und
`calcraft migrate`, Phase 2 und aus Phase 3 Papierkorb, `MOVE`/`COPY`, CardDAV-Fotos/-Gruppen,
RRULE-Expansion mit Zeitzonen, Kalender-Reihenfolge und Dead Properties; dazu systemd-Dienst,
`/readyz` mit Schreibzustand und SSO-Status in der Web-UI).
Nicht für den Produktivbetrieb.

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
| 6638 / 5546 | Scheduling (iTIP) zwischen Benutzern eines Servers | ✅ Stufe A (`docs/scheduling.md`) |
| 6047 | iMIP (Einladungen per E-Mail) | Stufe B, offen |

## 3. Architektur

Cargo-Workspace:

```
crates/
├── calcraft-core    # Domain-Typen, Fehler, Validierung (wasm-kompatibel)
├── calcraft-ical    # iCal/vCard-Parsing, Limits, RRULE-Expansion (wasm-kompatibel)
├── calcraft-dav     # WebDAV/CalDAV/CardDAV-Protokoll, XML
├── calcraft-store   # Storage-Trait, SQLite (Default) + PostgreSQL
├── calcraft-web     # Bettet die UI-Assets ein und liefert sie aus, Feature `web-ui`
├── calcraft-ui      # Leptos-App (CSR, wasm32); Komponenten später auch für den Client
├── calcraft-client  # CalDAV/CardDAV-Client-Bibliothek (Sync, Offline-Cache)
└── calcraft-server  # Binary: Config, CLI, Routing, Middleware, DAV und REST-API (Modul `api`)
apps/
└── calcraft-desktop # Tauri-2-App (Linux, macOS, Windows; später iOS/Android)
```

`calcraft-client` und `apps/` entstehen erst mit dem Client-Track.

Grundsätze:

- **Eine API:** Die Web-UI nutzt ausschließlich die REST-API, keine Leptos Server Functions.
  Die JSON-Typen liegen in `calcraft-core::api` und werden von Server und UI geteilt.
- **OpenAPI:** handgeschriebene Spezifikation (`/api/openapi.json`), ein Test prüft, dass jede Route
  dokumentiert ist (kein utoipa, um Abhängigkeiten zu sparen).
- **Shared Code:** `calcraft-core` und `calcraft-ical` kompilieren auch für `wasm32`.
- **Speicher:** Rohdaten (`.ics`/`.vcf`) unverändert als Blob + extrahierter Index
  (UID, DTSTART/DTEND, RRULE-Flag) für Time-Range-Queries. ETag = Inhalts-Hash.
  Changelog-Tabelle mit monotoner Sync-ID für `sync-token`/`getctag`.

### Feature-Flags

| Ebene | Schalter |
|---|---|
| Compile-Zeit (Cargo) | `web-ui`, `api`, `oidc`, `update-check`, `migrate`, `swagger-ui`, `postgres` |
| Laufzeit (Config/ENV/CLI) | `web_ui.enabled`, `api.enabled`, `api.swagger_ui`, `updates.check` |

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

**Stand M6a** – Import und Export über REST-API (`GET …/collections/{kind}/{slug}/export`,
`POST …/import`) und CLI (`calcraft export|import`):

| Format | Kalender | Adressbuch | Import | Export |
|---|---|---|---|---|
| iCalendar `.ics` (RFC 5545) | ✅ | | ✅ | ✅ |
| jCal (RFC 7265) | ✅ | | ✅ | ✅ |
| xCal (RFC 6321) | ✅ | | ✅ | ✅ |
| vCard `.vcf` 3.0/4.0 | | ✅ | ✅ | ✅ |
| jCard (RFC 7095) | | ✅ | ✅ | ✅ |
| CSV | ✅ (einfache Termine) | ✅ | ✅ | ✅ |

- Alle Formate werden in `calcraft-ical` (ohne I/O, wasm-fähig) in iCalendar-/vCard-Text übersetzt; jedes
  Objekt läuft danach durch dieselbe Prüfung wie ein DAV-PUT (`CalendarObject::parse` / `VCard::parse`,
  Komponenten-Set) und durch `put_item` (Quota 512 MiB pro Benutzer, 100 000 Objekte pro Collection).
- Termine werden nach UID gruppiert (Ausnahmen mit `RECURRENCE-ID` bleiben beim Master), referenzierte
  `VTIMEZONE`s werden in jedes Objekt kopiert, `METHOD` wird entfernt. Objekte ohne UID erhalten eine UUID.
- Limits: Datei ≤ 20 MiB (per HTTP zusätzlich `server.max_body_bytes`, Standard 10 MiB – größere Dateien
  über die CLI), ≤ 10 000 Objekte (schon beim Gruppieren geprüft), Objekt ≤ 1 MiB, erzeugte Objekte
  zusammen ≤ 4 × Dateigröße (mind. 4 MiB, höchstens 64 MiB; begrenzt kopierte `VTIMEZONE`s), Parser-Budgets (200 000 Komponenten, 500 000 Properties, JSON-Tiefe/-Wertezahl vorab
  geprüft, xCal ≤ 1 000 000 Elemente), höchstens 2 Importe gleichzeitig (Slot vor dem Lesen des Bodys). Ablehnungen werden
  gezählt, die ersten 20 mit Grund gemeldet (Steuerzeichen ersetzt). RECUR-, Zahlen-, GEO- und Datumswerte
  aus jCal/xCal werden gegen ihre Syntax geprüft.
- UID-Konflikte nur innerhalb der Ziel-Collection: `on_conflict=skip` (Standard) oder `replace`.
- xCal nutzt den einzigen XML-Parser (`calcraft_ical::xml`, von `calcraft-dav` mitbenutzt): keine DTDs,
  keine Entities.
- Export wird gestreamt (Objekte einzeln geladen, 64-KiB-Blöcke über einen begrenzten Kanal),
  `Content-Disposition: attachment`, `Cache-Control: no-store`.
- Serializer escapt TEXT (`\\`, `;`, `,`, Zeilenumbrüche), faltet bei 75 Oktetten ohne UTF-8-Zeichen zu
  teilen und lehnt Steuerzeichen in Rohwerten und ungültige Namen ab. CSV-Export neutralisiert Formeln
  (`=`, `+`, `-`, `@`, TAB, CR → `'`-Präfix).
- Fuzz-Target `import` deckt alle Formate ab.

Einschränkungen: CSV-Export enthält nur Termine (Master ohne Wiederholungsregel, lokale Zeiten ohne
Zeitzone); jCard-Export behält `VERSION:3.0` bei 3.0-Karten; xCal-Export lässt Properties weg, deren Name
kein gültiger XML-Name ist.

**Stand M6b** – Webcal-Abos (eingehend, Cargo-Feature `subscriptions`, `[subscriptions] enabled`
standardmäßig aus): Der Server holt den Feed und legt die Termine als normale Objekte eines
read-only Kalenders ab (`collections.read_only`, Tabelle `subscriptions`, Migration
`0009_subscriptions`); jeder CalDAV-Client sieht ihn inkl. sync-collection. Schreibzugriffe per DAV
(PUT/DELETE/COPY/MOVE hinein, PROPPATCH) → 403 `DAV:need-privileges`, Privilegien nur `read`;
Store, REST-Import und Papierkorb verweigern Schreibzugriffe ebenfalls; Löschen eines Abos ist
endgültig (kein Papierkorb). Fetcher (`subscriptions/ssrf.rs`, `fetch.rs`): nur https (webcal →
https, http nur mit `allow_http`), erlaubte Ports, keine Userinfo, strikte Hostnamen (keine
numerischen/lokalen Namen, IDN nur als `xn--`), eigene DNS-Auflösung mit Prüfung **aller** Adressen
(privat, Loopback, Link-Local/Metadaten, CGNAT, Multicast, Doku/Benchmark, außerhalb 2000::/3,
NAT64, 6to4, Teredo, IPv4-mapped nach IPv4), Verbindung zur geprüften Adresse mit SNI/Host des
Namens (Pinning), Redirects manuell (max. 3, jeder Hop neu geprüft, nie https → http), eine
Deadline, Größenlimit, bedingte Requests, fester User-Agent, keine Cookies/Credentials; Logs nur
mit Host. Refresh: gleicher Parser und gleiche Validierung wie der Import, stabile Namen pro UID,
nur echte Änderungen bekommen eine neue Revision; kaputter Feed behält den letzten guten Stand
(Fehler für den Besitzer sichtbar, Back-off). Hintergrund-Task: Intervall 15 min bis 7 Tage
(Feed-`REFRESH-INTERVAL`/`X-PUBLISHED-TTL` innerhalb der Grenzen, Standard 6 h), Jitter, max. 2
parallele Abrufe, 20 Abos und 10 Abo-Versuche pro 10 min je Benutzer, „Jetzt aktualisieren“
höchstens einmal pro Minute. REST `/api/v1/subscriptions` (ROUTES, OpenAPI, Autorisierungsmatrix),
Web-UI-Abschnitt auf der Übersicht.

Offen: Abos mit eigenen Zugangsdaten (Basic), Veröffentlichen eigener Kalender als webcal-Feed,
eine Admin-Ausnahmeliste für interne Feed-Server.

## 6. Web-UI (Leptos)

| Stufe | Inhalt |
|---|---|
| UI-1 | Login, Kalender-/Adressbuchliste, Einstellungen, App-Passwörter, Admin |
| UI-2 | Agenda- und Monatsansicht, Termin-Dialog |
| UI-3 | Woche/Tag mit Zeitraster, Layout überlappender Termine |
| UI-4 | Drag & Drop, Resize, Touch, Tastatur, ARIA |
| UI-5 | Kontakte, Freigaben, i18n (de/en), Dark Mode |

Ziel < 500 KB gzip (Stand UI-1: ca. 430 KB wasm, 165 KB gzip). Kein npm, kein CSS-Framework.

**Architektur (ab M4b):**

- Client-seitig gerenderte Leptos-App (CSR) in `crates/calcraft-ui`, kompiliert nach `wasm32-unknown-unknown`.
  Sie spricht nur die REST-API (`credentials: same-origin`, CSRF-Header bei jeder Änderung) und nutzt die
  JSON-Typen aus `calcraft-core::api`. Kein SSR, keine Server Functions.
- Die reine Logik (Validierung, Fehlertexte, Formatierung, alle Texte in `text.rs` für spätere i18n) liegt in der
  Bibliothek des Crates und wird nativ getestet; Leptos wird nur für `wasm32` gebaut.
- Navigation über das URL-Fragment (`#/settings`), damit der Server nur `/` und `/assets/…` kennt.
- Läuft unter der unveränderten CSP: keine Inline-Skripte/-Styles, keine `style`-Attribute (Farben als SVG-`fill`),
  Nutzerdaten nur als Text, nie `inner_html`.

**Build:** `scripts/build-ui.sh` (Windows: `scripts/build-ui.ps1`) baut mit dem Profil `wasm-release`
(`opt-level = "z"`) und ruft die `wasm-bindgen`-CLI auf, deren Version exakt der gepinnten Crate-Version entsprechen
muss. Ergebnis: `crates/calcraft-ui/dist/` (nicht im Git). Das `build.rs` von `calcraft-web` bettet die Dateien mit
Inhalts-Hash im Namen per `include_bytes!` ein und erzeugt ein Bootstrap-Modul; fehlen sie, wird eine Platzhalterseite
ausgeliefert (`CALCRAFT_UI_REQUIRED=1` macht das zum Build-Fehler, z. B. im Docker-Image).
Auslieferung: `/` mit `Cache-Control: no-cache`, `/assets/<name>-<hash>.<ext>` ein Jahr `immutable`.
Noch offen: `wasm-opt` und vorkomprimierte Assets (gzip/brotli).

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
| **M3** ✅ | sync-collection, CardDAV · aus Phase 3 erledigt: Kontaktfotos (`storage.max_vcard_bytes`) und Gruppen (Apple, vCard 4.0), `MOVE`/`COPY` zwischen eigenen Collections, Papierkorb, exakte RRULE-Expansion mit Budgets und eingebetteten VTIMEZONEs (`docs/caldav.md`; `expand` und Zeitzonen-Datenbank offen) |
| **M4** | **M4a** ✅ REST-API + OpenAPI, Sessions, 2FA (TOTP + Wiederherstellungscodes), App-Passwörter, Admin · **M4b** ✅ Web-UI Stufe 1 · **M4c** 🟡 SSO-Login in der Web-UI ✅, Logo/Favicon/Design-System ✅, Termin- und Kontaktansichten offen |
| **M5** | **M5a** ✅ OIDC/SSO (Keycloak & Co.) · Freigaben, Gruppen, Erinnerungen (SMTP), Web-UI Stufe 3–4 |
| **M6** | **M6a** ✅ Import/Export aller Formate (REST + CLI) · Backup/Restore + Pre-Migration-Backups ✅ · **M6b** ✅ Webcal-Abos (eingehend) mit SSRF-Schutz · Migration aus Radicale/Baïkal/Synology/Nextcloud ✅ (`calcraft migrate`: Discovery, Multiget mit GET-Fallback, Validierung wie Import, `--dry-run`, idempotent; getestet gegen einen Fake-Server, noch nicht gegen echte Instanzen) |
| **S** | **Scheduling (RFC 6638):** **Stufe A** ✅ zwischen Benutzern desselben Servers (`[scheduling]`, standardmäßig aus): implizites Scheduling bei PUT/DELETE (REQUEST, CANCEL, REPLY inkl. Serien, Overrides, EXDATE), Inbox/Outbox, Schedule-Tag/`If-Schedule-Tag-Match`, Free/Busy über die Outbox, Adressen nur vom Admin (`calcraft user set-email`, REST, Web-UI) oder `<user>@<domain>`, nie aus OIDC; Limits (Teilnehmer, Empfänger pro Stunde, Inbox-Größe, Quota), Tests `tests/scheduling.rs`, Matrix, Fuzz-Ziel `itip`, python-caldav in der CI · **Stufe B** offen: iMIP (SMTP ausgehend, eingehende Antworten mit DMARC-Prüfung) |
| **A** | **Managed Attachments (RFC 8607)** ✅ (`[attachments]`, standardmäßig aus): Upload/Update/Remove per `POST …?action=attachment-*` inkl. `rid`, Speicherung in SQLite (Backup, Papierkorb, Quota des Hochladenden), Download unter `/dav/attachments/<managed-id>` nur für Hochladende und Benutzer, deren eigene Objekte es referenzieren (Einladungen), immer als Download mit `nosniff`/Sandbox-CSP, fremde Managed-IDs per PUT abgelehnt, Tests `tests/attachments.rs`, Matrix, Fuzz-Ziel `attach` (`docs/attachments.md`) · offen: Anzeige in der Web-UI, Virenscan |
| **M7** | Release-Pipeline 🟡 (Sigstore-Attestierung, Provenance, GHCR; Probelauf grün, echter Tag-Lauf offen), Update-Hinweis ✅ · externes Security-Review, Release v1.0 |

## 10. Weitere Anforderungen

- Erinnerungen (VALARM per E-Mail), optional Webhooks/ntfy.
- Backup/Restore per CLI, Datenexport und Kontolöschung (DSGVO).
- ✅ Papierkorb (Soft-Delete) mit Wiederherstellung (`storage.trash_retention_days`, `/api/v1/trash`, Web-UI-Seite *Trash*).
- Gruppen-, Team- und Ressourcenkalender; Quotas. Free/Busy zwischen Benutzern ✅ (Scheduling Stufe A).
- Observability: `/healthz`, `/readyz`, Prometheus `/metrics`, JSON-Logs.
- Paketierung: Docker, Helm, `.deb`/`.rpm`, Homebrew, NixOS-Modul.

## 11. Tests

- Litmus (WebDAV), Apple CalDAVTester.
- Client-Matrix: Thunderbird, Apple Kalender (macOS/iOS), DAVx⁵, Evolution, GNOME Kalender.
- Aufgezeichnete Client-Requests als Golden-File-Regressionstests.
- Fuzzing in der CI.

## 12. OIDC / SSO (M5a)

Umsetzung in `crates/calcraft-server/src/oidc/` (Cargo-Feature `oidc`, Default an) und
`api/oidc.rs`. Konfiguration im Abschnitt `[oidc]` (siehe README und `calcraft.example.toml`),
standardmäßig aus; wird beim Start validiert.

- **Ablauf:** `GET /api/v1/auth/oidc/login` → IdP (Authorization Code + PKCE S256, `state`,
  `nonce`) → `GET /api/v1/auth/oidc/callback` → gleiche Session wie Passwort-Login → `/`.
  Fehler: Redirect auf `/?sso_error=<code>` (feste Codes, keine Reflexion, kein `return_to`).
  `GET /api/v1/auth/methods` sagt der UI, ob SSO verfügbar ist.
- **state/nonce/PKCE:** je 256 Bit Zufall, serverseitig im Speicher (max. 10 000, 10 min),
  einmalig (auch bei Fehlern verbraucht), gebunden an ein zufälliges Browser-Cookie
  (`calcraft_oidc`, `HttpOnly`, `SameSite=Lax`, Pfad `/api/v1/auth/oidc`), Vergleich in
  konstanter Zeit. Ausstehende Logins gehen bei einem Neustart verloren (kein Clustering).
- **Discovery/JWKS:** nur `<issuer>/.well-known/openid-configuration`; dessen `issuer` muss
  exakt passen; Endpunkte nur https auf dem Issuer-Host/-Port (oder `extra_endpoint_hosts`).
  JWKS-Cache 1 h, bei unbekannter `kid` Neuladen (höchstens 1×/min). Ausgehend: Timeout 10 s,
  Antwort ≤ 256 KiB, keine Redirects, System-Truststore (+ `ca_file`), rustls mit ring.
- **ID-Token:** RS256/384/512, PS256/384/512, ES256/384 (konfigurierbare Allowlist, Default
  RS256+ES256); `none`/`HS*` nie; `crit` abgelehnt; `iss`, `aud` (+`azp` bei mehreren),
  `exp`/`iat`/`nbf` mit ≤ 60 s Toleranz, `nonce`, `sub`. RFC-9207-`iss` im Callback geprüft.
- **Konten:** Benutzername aus Claim (Default `preferred_username`), nur gültig nach
  `Username`-Regeln, nie umgeschrieben; kein E-Mail-Linking. Per SSO angelegte Konten
  werden bei der Anlage an `iss`+`sub` gebunden (Tabelle `oidc_identities`, Migration
  0005). Bestehende lokale Konten nie allein über den Namen: nur nach einmaliger,
  ablaufender Freigabe für genau eine IdP-Identität `calcraft user allow-sso-link <user>
  --subject <sub>` (Default 1 h, max. 24 h; Verbrauch und Bindung in einer Transaktion;
  andere Subjects → `account_exists`, Freigabe bleibt). Der IdP darf keine frei
  wählbaren Benutzernamen erlauben, sonst `required_claim` nutzen. Danach
  muss `sub` passen; `calcraft user unbind-sso` setzt zurück. Auto-Anlage nur mit `auto_create_users`
  (Passwort-Hash unbrauchbar). Deaktivierte Konten abgelehnt. Admin-Recht aus Claim nur,
  wenn `admin_claim` gesetzt ist (dann in beide Richtungen synchronisiert).
- **Optionen (Default aus):** `oidc.require_local_totp` – Konten mit lokaler 2FA können
  nicht per SSO; sonst verlässt sich SSO auf die MFA des IdP. `security.password_login =
  false` – nur SSO: Passwort-Login-API und DAV-Basic mit Kontopasswort abgelehnt,
  App-Passwörter gehen weiter; Passwortwechsel und lokale 2FA-Einrichtung ebenfalls
  gesperrt (`password_login_disabled`); Start ohne `[oidc]` wird verweigert.
- Ausstehende Logins: begrenzte Tabelle mit Verdrängung des ältesten Eintrags, kein
  globales Rate-Limit (ein anonymer Client könnte sonst alle SSO-Logins blockieren).
  Rate-Limiting pro Client kommt mit der Trusted-Proxy-/Client-IP-Konfiguration.
  TraceLayer-Spans enthalten nur den Pfad, nie die Query.
- **Später:** Backchannel-Logout und RP-initiated Logout, Admin-API zum Lösen von
  SSO-Bindungen und -Freigaben, Rate-Limit pro Client.

### Update-Hinweis (Phase 4)

`crates/calcraft-server/src/updates.rs` (Cargo-Feature `update-check`, Default an),
Konfiguration `[updates] check = false` (Default aus), `interval_hours = 24` (1–168).
Der ausgehende HTTPS-Client liegt jetzt in `src/https.rs` (internes Feature
`https-client`, von `oidc` und `update-check` genutzt).

- Nur wenn eingeschaltet: Hintergrund-Task holt beim Start und danach alle
  `interval_hours` (+ bis zu 10 % Zufall) `GET
  https://api.github.com/repos/Zeitgleich/CalCraft/releases/latest`. Feste URL, nicht
  konfigurierbar; nur HTTPS, Timeout 10 s, Antwort ≤ 256 KiB, keine Redirects, keine
  Cookies, keine Credentials, User-Agent `calcraft/<version>`, sonst nichts (GitHub sieht
  IP und User-Agent).
- Gelesen werden nur `tag_name`, `html_url`, `prerelease`, `draft`. Drafts/Pre-Releases
  und Tags ≠ `vX.Y.Z` werden ignoriert; Versionsvergleich handgeschrieben (kein
  semver-Crate). `html_url` nur, wenn unter `https://github.com/Zeitgleich/CalCraft/releases/`
  und ohne Sonderzeichen, sonst verworfen.
- Ergebnis nur im Speicher; Fehler als Warnung ohne Antwortinhalt, ohne Einfluss auf den
  Betrieb. `GET /api/v1/admin/update` (nur Admins) liefert `{enabled, current, latest,
  newer, url, checked_at}`; die Web-UI zeigt im Admin-Bereich „New version X available“
  mit Link (`target=_blank`, `rel="noopener noreferrer"`), bei `enabled: false` einen
  Hinweis zum Einschalten.

## 13. Bekannte Einschränkungen (Stand 0.1.0-alpha.1)

- `calendar-query` filtert nach Komponententyp und Zeitraum. Serientermine werden exakt expandiert
  (RRULE, RDATE, EXDATE, Overrides), Zeiten mit `TZID` über die eingebettete VTIMEZONE umgerechnet.
  Konservativ (±14 h bzw. offen) bleiben Floating-Zeiten und ganztägige Termine, `TZID`s ohne VTIMEZONE,
  nicht auswertbare Regeln und erschöpfte Budgets: dann können zusätzliche Objekte zurückkommen, aber
  nie fehlen. Details: `docs/caldav.md`.
- `prop-filter`, `param-filter` und `text-match` werden mit `CALDAV:supported-filter` (403) abgelehnt
  statt stillschweigend ignoriert.
- `calendar-data` liefert immer das ganze Objekt (keine Teilabfrage, kein `expand`/`limit-recurrence-set`).
- Dead Properties (z. B. `calendar-order`) nur auf Collections, nicht auf einzelnen Objekten; höchstens
  64 pro Collection, 16 KiB pro Wert (`calendar-timezone`: 64 KiB), 128 KiB insgesamt; sie zählen
  zur Quota. Werte werden als kanonisch neu serialisiertes XML gespeichert: Kommentare, präfixierte Attribute, Attribute des Property-Elements selbst und die
  Position von Text zwischen Kindelementen gehen verloren. Details: `docs/caldav.md`.
- `addressbook-query` unterstützt nur einen leeren Filter (alle Kontakte); Filter werden mit
  `CARDDAV:supported-filter` abgelehnt.
- `sync-collection` nur mit `sync-level` 1. Große Ergebnisse werden abgeschnitten (507-Eintrag) und
  mit dem gelieferten Token fortgesetzt. Das Änderungsprotokoll hält nur die letzte Änderung pro Objekt;
  höchstens 10 000 Tombstones pro Collection, ältere Tokens erzwingen eine Neusynchronisation.
- `calendar-query` / `addressbook-query` liefern höchstens 5 000 Objekte bzw. 32 MiB Daten, danach
  eine abgeschnittene Antwort mit 507-Eintrag.

## 14. Offene Entscheidungen

- CardDAV bereits in v1? (aktuell geplant: ja, M3)
- Lizenz der wiederverwendbaren Bibliotheken (`calcraft-ical`, ggf. `leptos-calendar`):
  AGPL wie der Rest oder `MIT OR Apache-2.0`?
- Contributions: DCO (`Signed-off-by`) statt CLA – sofern keine Dual-Lizenz geplant ist.

# Angriffshistorie: CalDAV-, CardDAV- und WebDAV-Server

## 1. Einleitung

**Zweck.** Dieses Dokument sammelt bekannte Schwachstellen und reale Angriffe auf Kalender- und Kontaktserver sowie auf iCalendar- und vCard-Verarbeitung in Clients. Daraus leiten wir konkrete Maßnahmen und Regressionstests für CalCraft ab. Fehler, die andere Projekte schon gemacht haben, sollen in CalCraft gar nicht erst entstehen. Wo sie doch entstehen, sollen Tests sie sofort finden.

**Stand:** Oktober 2026.

**Methode.** Die Angaben stammen aus Websuchen in NVD und CVE-Mirrors (CIRCL, OSV, OpenCVE, Debian Security Tracker), GitHub/GitLab Security Advisories, Vendor-Advisories, Distributions-Trackern (Debian, Red Hat, SUSE, Ubuntu), CERT/CC, CISA KEV und Security-Blogs (Tarlogic, Varonis, Check Point, X41, Core Security, Ethiack, HackDefense).

Zur Verlässlichkeit:
- CVE-IDs stehen nur dort, wo sie in mindestens einer Datenbank auftauchten.
- Viele Einträge aus 2025/2026 kennen wir nur aus Sekundärquellen (Aggregatoren). Sie sind mit „(Sek.)“ markiert und vor externer Zitierung gegen NVD oder das Vendor-Advisory zu prüfen.
- Wo keine ID bekannt ist, steht „–“.
- Die Liste ist nicht vollständig. Für Bedework, Apple Calendar Server, ical4j und RustiCal ergab die Recherche kaum oder keine öffentlichen CVEs. Das heißt nicht, dass dort keine Fehler existieren.

## 2. Tabelle der Vorfälle

| # | Produkt | Jahr | ID | Klasse | Kurzbeschreibung / Ursache | Quelle |
|---|---|---|---|---|---|---|
| 1 | Radicale | 2015 | CVE-2015-8747 | Path Traversal | Die Multifilesystem-Storage übernahm Komponentennamen ungeprüft als Dateipfad. Folge: beliebiges Lesen und Schreiben von Dateien. | https://advisories.gitlab.com/pkg/pypi/radicale/CVE-2015-8747/ |
| 2 | Radicale | 2015 | CVE-2015-8748 | Auth-/ACL-Bypass (Regex-Injection) | Der Benutzername wurde in einen Regex der Rechte-Regeln eingesetzt. Mit „.*“ als Benutzername ließen sich owner_only und owner_write umgehen. | https://www.cvedetails.com/vulnerability-list/vendor_id-15782/year-2016/Radicale.html |
| 3 | Radicale | 2016 | CVE-2016-1505 | Path Traversal (Windows) | Laufwerkspfade wie `/c:/file/ignore` wurden nicht abgefangen. Folge: beliebige Dateien lesbar und schreibbar. | https://www.endorlabs.com/vulnerability/cve-2016-1505 |
| 4 | Radicale | 2017 | CVE-2017-8342 | Timing-Orakel / Brute Force | Der htpasswd-Vergleich lief nicht in konstanter Zeit, und es gab keine Bremse gegen Brute Force. | https://advisories.gitlab.com/pkg/pypi/radicale/CVE-2017-8342 |
| 5 | sabre/dav (auch ownCloud) | 2014 | CVE-2014-2055 | XXE | Der XML-Parser löste externe Entities auf. Folge: Lesen lokaler Dateien und DoS ohne Authentifizierung. | https://advisories.gitlab.com/pkg/composer/sabre/dav/CVE-2014-2055/ |
| 6 | sabre/dav HTMLBrowser-Plugin | 2013 | CVE-2013-1939 | Path Traversal (Windows) | Backslash als Pfadtrenner wurde nicht normalisiert. Folge: Lesen beliebiger Dateien. | https://nvd.nist.gov/vuln/detail/CVE-2013-1939 |
| 7 | Baïkal (Admin-UI) | 2025/26 | – | Stored XSS → Admin-Übernahme | Ein Kalendername, den ein angemeldeter Benutzer setzt, wurde im Admin-Interface ungefiltert ausgegeben. Release-Note: Fix mit sabre/dav 4.7.1. | https://community-scripts.org/scripts/baikal (Sek.) |
| 8 | DAViCal | 2019 | CVE-2019-18345 | Reflected XSS | Der Parameter `action` wurde ohne Encoding zurückgegeben. | https://security-tracker.debian.org/tracker/CVE-2019-18345 |
| 9 | DAViCal | 2019 | CVE-2019-18346 | CSRF | Die Admin-Oberfläche hatte keinen CSRF-Schutz. Eine fremde Webseite konnte z. B. einen neuen Admin anlegen. | https://security-tracker.debian.org/tracker/CVE-2019-18346 |
| 10 | DAViCal | 2019 | CVE-2019-18347 | Stored XSS | Username, Display Name und E-Mail (von unprivilegierten Benutzern setzbar) wurden ungefiltert angezeigt. | https://nvd.nist.gov/vuln/detail/CVE-2019-18347 |
| 11 | RSCDS (Vorgänger von DAViCal) | 2007 | CVE-2007-5953 | Informationsabfluss | Nicht näher spezifiziertes Informationsleck vor 0.9.0. | https://www.incibe.es/en/incibe-cert/early-warning/vulnerabilities/cve-2007-5953 |
| 12 | SOGo | 2014 | CVE-2014-9905 | Stored XSS | Termintitel und Kontaktfelder wurden im Web-Kalender ungefiltert gerendert. | https://www.cvedetails.com/cve/CVE-2014-9905/ |
| 13 | SOGo | 2015 | CVE-2015-5395 | CSRF | Kein CSRF-Schutz vor 3.1.0. | https://cvefeed.io/vuln/detail/CVE-2015-5395 |
| 14 | SOGo | 2016 | CVE-2016-6189 | Info-Disclosure (Blacklist) | Die ICS- und XML-Ausgabe vertraulicher Termine filterte Felder per unvollständiger Blacklist. | https://exploit-intel.com/vuln/CVE-2016-6189 |
| 15 | SOGo | 2016 | CVE-2016-6190 | Info-Disclosure / ACL | UID und DTSTAMP waren für Benutzer ohne Leserecht sichtbar. | https://www.incibe.es/en/incibe-cert/early-warning/vulnerabilities/cve-2016-6190 |
| 16 | SOGo | 2026 | CVE-2026-8496 | Stored XSS über ICS-Einladung, aktiv ausgenutzt | Ein SVG mit `onrepeat`-Handler in DESCRIPTION einer Einladung führte beim Anzeigen im Webmail JavaScript aus. | https://kb.cert.org/vuls/id/487613 |
| 17 | SOGo | 2026 | CVE-2026-93453 | Host-/Origin-Header-Injection im Passwort-Reset | Der Reset-Link wurde aus dem ungeprüften Origin-Header gebaut. Das Reset-Token landete so auf einer Angreifer-Domain. | https://security-tracker.debian.org/tracker/CVE-2026-93453 (Sek.) |
| 18 | Nextcloud Server | 2020 | CVE-2020-8118 | SSRF über Kalender-Abo | Eine authentifizierte Abo-URL erlaubte das Abtasten interner und externer Dienste. | https://cve.circl.lu/vuln/cve-2020-8118 |
| 19 | Nextcloud Server | 2020 | CVE-2020-8138 | SSRF-Filter-Bypass | IPv4-in-IPv6-Adressen (z. B. `::ffff:127.0.0.1`) wurden beim Abo-Filter nicht erkannt. | https://www.opencve.io/cve/CVE-2020-8138 |
| 20 | Nextcloud Server | 2017 | CVE-2017-0894 / -0895 (Zuordnung prüfen) | Info-Disclosure | Gültige Share-Tokens öffentlicher Kalender konnten offengelegt werden. Andere Benutzer sahen Kalender- und Adressbuchnamen. | https://vulnerability.circl.lu/vuln/cve-2017-0894 |
| 21 | Nextcloud Server | 2023 | CVE-2023-39959 | Enumeration (unauth.) | Per DAV-Request ließ sich ohne Anmeldung prüfen, ob Kalender oder Adressbücher eines Opfers existieren. | https://recordedfuture.com/vulnerability-database/CVE-2023-39959 |
| 22 | Nextcloud Server | 2026 | CVE-2026-45281 | IDOR / ACL-Bypass (CWE-639) | Ein einziger PROPPATCH auf `group-member-set` eines fremden Principals gab dem Angreifer Schreibzugriff auf alle Kalender des Opfers. | https://ethiack.com/info-hub/research/nextcloud-cve-2026-45281 |
| 23 | Nextcloud Calendar | 2026 | CVE-2026-45286 / GHSA-r697-74m9-gvf2 | Benutzer-Enumeration | Der Endpoint für Teilnehmer-Vorschläge ignorierte die Sharing-Beschränkungen. | https://osv.dev/vulnerability/CVE-2026-45286 |
| 24 | Nextcloud Calendar | 2025 | CVE-2025-66511 | Vorhersagbare Tokens (CWE-330) | Teilnehmer-Tokens für Terminvorschläge wurden per Hash statt CSPRNG erzeugt. Fremde konnten sie berechnen und im Namen anderer abstimmen. | https://www.sentinelone.com/vulnerability-database/cve-2025-66511/ (Sek.) |
| 25 | Nextcloud Calendar | 2024 | CVE-2024-37316 | Open Redirect | Manipulierte Attachment-Daten in einem Termin führten Teilnehmer auf fremde Ziele. | https://access.redhat.com/security/cve/CVE-2024-37316 |
| 26 | Nextcloud Calendar | 2025 | CVE-2025-66550 | Ungewollter Download | Ein präpariertes Attachment löste einen Download ohne Bestätigung aus. | https://cveawg.mitre.org/api/cve/CVE-2025-66550 |
| 27 | Nextcloud Calendar | 2018 | CVE-2018-3763 | Stored XSS | Autocomplete-Ergebnisse (Gruppennamen) wurden nicht escaped. | https://www.opencve.io/cve/CVE-2018-3763 |
| 28 | Nextcloud Calendar | 2023 | CVE-2023-33183 / GHSA-2792-2734-hr7j | Improper Authorization | Details im Suchergebnis nicht verfügbar, siehe Advisory. | https://vulnerability.cert.dk/vuln/gsd-2023-33183 |
| 29 | Nextcloud Server | ~2024 | GHSA-r936-8gwm-w452 | DoS (Ressourcen) | Extrem lange E-Mail-Adressen in Termineinladungen machten den Server unresponsiv. | https://vulners.com/nextcloud/GHSA-R936-8GWM-W452 |
| 30 | Nextcloud/ownCloud | 2016 | CVE-2016-9465 | Stored XSS (Bild-Export) | Das in einer vCard eingebettete PHOTO wurde ohne Prüfung des Content-Types ausgeliefert. | https://vulnerability.circl.lu/vuln/CVE-2016-9465 |
| 31 | ownCloud Contacts | 2015 | CVE-2015-3011 | Stored XSS | Ein präparierter Kontakt führte Skript im Web-UI aus. | https://packetstormsecurity.com/files/cve/CVE-2015-3011 |
| 32 | ownCloud Calendar/Contacts | 2012 | CVE-2012-4397 | Stored XSS | Betroffen waren der Display-Name des Kalenders und die vCard-Verarbeitung. | https://www.opencve.io/cve/CVE-2012-4397 |
| 33 | Cyrus IMAP (httpd/CalDAV) | 2019 | CVE-2019-11356 | Buffer Overflow → RCE | Ein überlanger iCalendar-Property-Name in einem PUT führte zu RCE. | https://security-tracker.debian.org/tracker/CVE-2019-11356 |
| 34 | Cyrus IMAP | 2026 | CVE-2026-61909 | ACL-Bypass (multiget) | `calendar-multiget` und `addressbook-multiget` prüften die ACL nicht pro href. | https://app.opencve.io/cve/CVE-2026-61909 (Sek.) |
| 35 | Cyrus IMAP | 2026 | CVE-2026-61915 | Double-Free (DoS) | Ein präpariertes PATCH (VPATCH BYPARAM) brachte den CalDAV-Worker zum Absturz. | https://bugzilla.redhat.com/show_bug.cgi?id=2526315 |
| 36 | Stalwart (CalDAV) | 2025 | CVE-2025-59045 / GHSA-xv4r-q6gr-6pfg | RRULE-Expansions-DoS (CWE-770) | REPORT mit Expansion speicherte alle Instanzen ohne Obergrenze. Eine Anfrage konnte rund 2 GB RAM belegen. | https://nvd.nist.gov/vuln/detail/CVE-2025-59045 |
| 37 | RustiCal | 2026 | GHSA-9cp5-7crm-9cf7 | IDOR / fehlender Scope | Ein DELETE, das den Papierkorb umgeht, war nicht auf den Principal beschränkt. Folge: endgültiges Löschen fremder Objekte. | https://github.com/lennart-k/rustical/security/advisories |
| 38 | Zimbra (Classic Web Client) | 2024 | CVE-2024-27443 | XSS über CalendarInvite, CISA KEV | Ein präparierter Kalender-Header in einer E-Mail führte das XSS-Payload aus. | https://nvd.nist.gov/vuln/detail/CVE-2024-27443 |
| 39 | Zimbra | 2025 | CVE-2025-27915 | Stored XSS über ICS, Zero-Day | HTML in ICS-Dateien wurde nicht ausreichend bereinigt (`ontoggle`). Angreifer legten damit Weiterleitungsfilter an. | https://www.scworld.com/news/attackers-exploit-xxs-flaw-in-zimbra-collaboration-suite |
| 40 | Zimbra | 2019 | CVE-2019-9670 | XXE (Autodiscover), mit SSRF verkettet, KEV | Der XML-Parser des Mailbox-Dienstes löste externe Entities auf. In Verbindung mit ProxyServlet-SSRF führte das zu RCE. | https://nvd.nist.gov/vuln/detail/CVE-2019-9670 |
| 41 | Horde Kronolith | 2005 | CVE-2005-4189 | Stored XSS | Betroffen waren Kalender- und Terminfelder. | https://opencve.greenant.net/cve/CVE-2005-4189 |
| 42 | Horde Kronolith | 2008 | CVE-2008-1974 | XSS | Eingaben im Add-Event-Dialog wurden nicht escaped. | https://lwn.net/Articles/280015/ |
| 43 | Horde Kronolith | 2012 | CVE-2012-5566 / CVE-2012-5567 | XSS | Betroffen waren der Ort (location) in Monatsansichten sowie die Task- und Suchansichten. | https://nvd.nist.gov/vuln/detail/CVE-2012-5567 |
| 44 | EGroupware | 2004 | CVE-2004-1467 | XSS | Betroffen waren Datums- und Suchfelder im Kalender. | https://nvd.nist.gov/vuln/detail/CVE-2004-1467 |
| 45 | EGroupware | 2023 | CVE-2023-38327 | User-Enumeration (unauth.) | `calendar/freebusy.php` beantwortete Anfragen für existierende und nicht existierende Benutzer unterschiedlich. | https://osv.dev/vulnerability/CVE-2023-38327 |
| 46 | Open-Xchange App Suite | 2013 | CVE-2013-5690 | Stored XSS | Betroffen waren text/xml-Inhalte und der Statuskommentar eines Termins. | https://packetstormsecurity.com/files/cve/CVE-2013-5690 |
| 47 | Open-Xchange App Suite | 2019 / 2022 | – / CVE-2022-37312 (Bereich) | SSRF / DNS-Rebinding | Der Anti-SSRF-Schutz prüfte nur den ersten A- oder AAAA-Record. | https://packetstormsecurity.com/files/154826/oxappsuite-ssrfxssdisclose.txt |
| 48 | Bedework bw-webdav | 2018 | CVE-2018-20000 | XXE | Ein Invite-Reply-Dokument konnte lokale Dateien lesen. | https://web.nvd.nist.gov/vuln/detail/CVE-2018-20000 |
| 49 | Bedework bw-calendar-engine | 2018 | CVE-2018-1000836 | XXE | XXE im Calendar-Engine. Patch-Status unklar. | https://vulert.com/vuln-db/maven/org-bedework-caleng-bw-calendar-engine |
| 50 | Apple iOS CalDAV-Client | 2011 | CVE-2011-3253 | TLS-Zertifikatsprüfung | Die Zertifikate des CalDAV-Servers wurden nicht ausreichend geprüft. | https://ocve.arawa.fr/cve/CVE-2011-3253 |
| 51 | Apple iCal (Client) | 2008 | CVE-2008-1035, CVE-2008-2006 u. a. | Speicherfehler im ICS-Parser | Ein präparierter `.ics` führte zu Absturz oder Codeausführung. Beispiel: `COUNT=2147483646` erzeugte einen Integer-Overflow und dann eine Null-Pointer-Dereferenzierung. | https://www.coresecurity.com/content/ical |
| 52 | libical | 2016 | CVE-2016-5824, -5825, -5826, -5827 | Use-after-free / Heap Over-Read | Betroffen waren `icalparser_parse_string`, `parser_get_next_char` und `icaltime_from_string`. | https://bugzilla.redhat.com/show_bug.cgi?id=1375104 |
| 53 | Thunderbird (libical-Fork) | 2019 | X41-2019-002 (CVE-Zuordnung unklar) | Heap Overflow (möglicherweise RCE) | Der Upstream-Fix in libical war nie in den Fork eingeflossen. | https://www.exploit-db.com/exploits/47003/ |
| 54 | Thunderbird | 2026 | CVE-2026-84637 | Codeausführung über ICS-Attachment | Einladungen mit `file:`-URI-Attachments führten unter Windows Programme aus, teils unter irreführendem Dateinamen. | https://research.averlon.ai/vulnerability-intelligence/cve/CVE-2026-84637 (Sek.) |
| 55 | python-vobject | ~2025 | – (Issue #134) | DoS durch Rekursion | Tief verschachtelte BEGIN/END-Blöcke lösten einen `RecursionError` aus (~995 Ebenen). | https://github.com/py-vobject/vobject/issues/134 |
| 56 | python-icalendar | 2026 | CVE-2026-77399 | Unbegrenzte Expansion (VALARM REPEAT) | Ein vom Angreifer gesetztes REPEAT wurde ohne Limit expandiert. | https://osv.dev/vulnerability/CVE-2026-77399 (Sek.) |
| 57 | rrule.js | ~2019 | – (Snyk) | Endlosschleife | `INTERVAL=0` führte zu einer Endlosschleife. | https://security.snyk.io/vuln/SNYK-JS-RRULE-72421 |
| 58 | Open WebUI | 2026 | CVE-2026-109297 | RRULE-DoS durch Validator/Expander-Mismatch | Der Validator parste strenger als die Expansionsbibliothek. Varianten mit FREQ=SECONDLY kamen dadurch durch. | https://www.vulncheck.com/advisories/open-webui-0.11.0-before-0.11.4-denial-of-service-via-automation-recurrence-rules (Sek.) |
| 59 | MagicMirror | 2026 | CVE-2026-63643 | SSRF über Kalender-URL | Die Kalender-URL war voll angreiferkontrolliert, inklusive Auth-Headern und abschaltbarer TLS-Prüfung. Geparste Events kamen zurück, also vollständige Exfiltration. | https://advisories.gitlab.com/npm/magicmirror/CVE-2026-63643/ (Sek.) |
| 60 | plone.app.event | 2026 | CVE-2026-55247 | SSRF + DoS beim iCal-Import | Der Import holte entfernte URLs und verarbeitete unbegrenzte Datenmengen. | https://vulert.com/vuln-db/CVE-2026-55247 (Sek.) |
| 61 | Wallos | 2026 | CVE-2026-77353 | iCalendar-Injection (CRLF) | CRLF in Namen und Notizen fügte im exportierten `.ics` beliebige Events ein. | https://api.osv.dev/v1/vulns/CVE-2026-77353 |
| 62 | Microsoft Outlook | 2023 | CVE-2023-35636 (Varonis; ID gegen MSRC prüfen) | NTLM-Hash-Leak über `.ics` | Eine präparierte Kalenderfreigabe brachte Outlook dazu, sich per SMB/NTLM bei einem fremden Host zu authentifizieren. | https://community.isc2.org/t5/Threats/Accepting-a-calendar-invite-in-Outlook-could-leak-your-password/m-p/66708 |
| 63 | Microsoft Outlook / Exchange | 2023–25 | – | iTIP/iMIP-Organizer-Spoofing | ORGANIZER, ATTENDEE und LOCATION im VCALENDAR wurden ohne Abgleich mit dem SMTP-Absender angezeigt. Die automatische Kalenderverarbeitung trug Termine ohne Zutun ein. | https://www.tarlogic.com/blog/abusing-calendar-processing/ |
| 64 | Google Calendar | 2024–25 | – | Invite-Phishing / Spam | Einladungen kamen über Google-Server (SPF, DKIM und DMARC bestanden) mit Links zu Forms oder Drawings. Ein Storno löste eine zweite Mail aus. | https://www.bleepingcomputer.com/news/security/ongoing-phishing-attack-abuses-google-calendar-to-bypass-spam-filters/ |
| 65 | AgenDAV (Doku-Warnung) | – | – | SSRF über ICS-Abos | Die Dokumentation warnt ausdrücklich vor internen URLs. Abos sind deshalb standardmäßig aus, und nur http(s) ist erlaubt. | https://agendav.readthedocs.io/en/latest/features/subscriptions.html |
| 66 | RFC 4791 §11 | 2007 | – | Ressourcen-DoS (Spezifikation) | Die Spezifikation verlangt Schutz gegen Ressourcenerschöpfung. Beispiel: eine sekündliche RRULE über 100 Jahre ergibt rund 3 Mrd. Instanzen. | https://icalendar.org/CalDAV-Access-RFC-4791/11-security-considerations.html |

Insgesamt **66 Einträge**: 60 konkrete Schwachstellen oder Vorfälle sowie 6 Kampagnen, Dokumentationswarnungen und Spezifikationshinweise.

## 3. Muster und wiederkehrende Angriffsklassen

1. **XSS über Kalender- und Kontaktfelder** (ca. 20 Einträge: SOGo, Zimbra, DAViCal, Baïkal, ownCloud/Nextcloud, Horde, EGroupware, OX). Das ist die häufigste Klasse überhaupt.
   - Typische Felder sind SUMMARY, DESCRIPTION, LOCATION, Kalender-Displayname, Benutzer- und Anzeigename sowie vCard-PHOTO.
   - Die Payloads kommen meist von außen über eine **Einladung per E-Mail**. Deshalb wurde die Klasse 2024–2026 zweimal aktiv ausgenutzt (Zimbra, SOGo).
   - Varianten sind HTML-Beschreibungen (`X-ALT-DESC`), SVG mit Event-Handlern, `<details ontoggle>` und Bilder mit falschem Content-Type.
2. **Fehlende Autorisierung zwischen Benutzern (IDOR/ACL)** (Nextcloud 2026, Cyrus multiget 2026, RustiCal 2026, SOGo 2016, Radicale 2015).
   - Die Prüfung sitzt am „Haupteingang“ (GET/PUT). Sie fehlt aber bei Nebenpfaden: PROPPATCH auf ACL-relevante Properties (`group-member-set`), REPORT/multiget mit fremden hrefs, Papierkorb- und Hard-Delete-Pfade, Teilnehmer-Vorschlags-Endpoints.
3. **XXE und unsicheres XML** (sabre/dav, Bedework ×2, Zimbra). WebDAV ist XML. Jeder Parser mit aktivierten Entities ist ein Datei-Leser.
4. **Ressourcenerschöpfung** (Stalwart RRULE, python-icalendar REPEAT, rrule.js INTERVAL=0, Open WebUI, Nextcloud lange E-Mail-Adressen, vobject-Rekursion, plone Import).
   - Gemeinsamer Nenner: Expansion oder Rekursion ohne Budget.
   - Ein besonders tückisches Muster: Validator und Expander parsen unterschiedlich.
5. **SSRF über Abo- und Import-URLs** (Nextcloud ×2, MagicMirror, plone, OX, AgenDAV).
   - Bypässe liefen über IPv4-in-IPv6, DNS-Rebinding (nur der erste Record wurde geprüft), Redirects, abschaltbares TLS und frei setzbare Header.
6. **Path Traversal im Storage** (Radicale ×2, sabre/dav). Ressourcennamen wurden als Dateipfade verwendet, Windows-Laufwerke und Backslashes übersehen.
7. **Speicherfehler in iCal-Parsern** (libical, Cyrus, Apple iCal, Thunderbird-Fork). In Rust ist das weitgehend vermeidbar. Panics, Integer-Overflows und Rekursion bleiben aber auch dort relevant.
8. **Enumeration und Informationsabfluss** (EGroupware free-busy, Nextcloud Existenzprüfung und Teilnehmer-Suche, SOGo vertrauliche Felder, Nextcloud Share-Tokens). Free-Busy-Endpoints und „vertrauliche“ Termine per Blacklist filtern hat wiederholt versagt.
9. **CSRF** (DAViCal, SOGo). Admin-UIs ohne Token.
10. **iTIP/iMIP-Spoofing, Invite-Spam und Phishing** (Outlook, Google, Thunderbird, Nextcloud-Tokens). ORGANIZER und ATTENDEE sind frei wählbarer Text. Automatisches Eintragen von Einladungen macht den Kalender zum Spam-Kanal.
11. **Unsichere Tokens und Auth-Nebenpfade** (Nextcloud Hash-Tokens, SOGo Reset-Link aus Origin-Header, Radicale Timing, iOS ohne Zertifikatsprüfung).
12. **Injection in Exportformate** (Wallos CRLF in ICS, CSV-Formel-Injection CWE-1236). Text aus dem Web-UI landet ungeprüft in Zeilenformaten.
13. **Gefährliche URIs in Feldern** (Thunderbird `file:`-ATTACH, Outlook UNC/SMB, Nextcloud Open Redirect und Auto-Download über ATTACH und URL).

## 4. Konkrete Lehren für CalCraft

Legende: ✅ = bereits vorhanden, 🆕 = neu und bisher nicht im Plan.

### Bereits vorhandene Maßnahmen (Abgleich)

| Vorhandene Maßnahme | Deckt ab |
|---|---|
| ✅ XML-Parser lehnt DOCTYPE und Entities ab | XXE (#5, #40, #48, #49) |
| ✅ Verschachtelungs- und Property-Count-Limits im XML | XML-DoS, Billion Laughs, tiefe Bäume |
| ✅ Benutzernamen und Slugs nur `[a-z0-9._-]` | Path Traversal (#1, #3, #6), Regex- und Pfad-Injection (#2) |
| ✅ Pfade fremder Benutzer liefern 404 | Teil von IDOR, Existenz-Enumeration (#21) |
| ✅ Argon2id mit Dummy-Hash für unbekannte Benutzer | Timing und User-Enumeration beim Login (#4) |
| ✅ Login-Lockout pro Benutzer | Brute Force (#4) |
| ✅ Strikte CSP und Security-Header | Begrenzt die Folgen von XSS (#7–#10, #16, #38, #39) |
| ✅ Body-Size-Limits | Grobe DoS-Klasse (#29, #60) |
| ✅ `forbid(unsafe_code)` | Speicherfehler-Klasse (#33, #35, #51–#53) |

### Maßnahmen pro Angriffsklasse mit Meilenstein

| Klasse | Maßnahme | Meilenstein | Status |
|---|---|---|---|
| XXE / XML | Parser-Konfiguration als einzige zentrale Funktion. Kein zweiter XML-Parser im Projekt, auch nicht für xCal-Import (M6) und Invite-Replies. Lint/Test: `cargo deny` bzw. Grep-Test gegen weitere XML-Crates. | M1 (M6 erneut prüfen) | ✅ (M6a: xCal nutzt `calcraft_ical::xml`, weitere XML-Crates per `deny.toml` gesperrt) |
| Path Traversal | Storage nie über Dateipfade aus Ressourcennamen (SQLite). Objekt-Namen (`*.ics`/`*.vcf`) zusätzlich validieren: kein `/`, `\`, `..`, NUL, `%2f` nach Decoding, Längenlimit. Doppeltes Percent-Decoding testen. | M1 | 🆕 für Objektnamen |
| IDOR / ACL | **Zentrale Autorisierungsschicht**: jede Storage-Abfrage nimmt einen `Principal`-Kontext und filtert per SQL auf `owner_id`/Freigabe (*scoped queries*, keine nachgelagerte Prüfung). Gilt für jede Methode einschließlich PROPPATCH, REPORT (multiget **pro href**), sync-collection, DELETE, Papierkorb, MOVE/COPY (Ziel prüfen!). | M1 Grundgerüst, M3 multiget/sync, M5 Freigaben | 🆕 |
| IDOR / ACL | ACL- und Gruppen-Properties (`group-member-set`, `acl`, `calendar-proxy-*`, `owner`) sind serverseitig schreibgeschützt bzw. nur über die Freigabe-API änderbar. Test: PROPPATCH auf fremden und eigenen Principal. | M1 / M5 | 🆕 |
| IDOR / ACL | Autorisierungs-Matrix-Test: alle HTTP-Methoden × alle Ressourcentypen × {Eigentümer, Lesefreigabe, Schreibfreigabe, Fremder, anonym}. Erwartung als Tabelle im Repo. | M1 starten, M5 vervollständigen, M7 Abnahme | 🆕 |
| Enumeration | 404 für fremde Pfade ✅ auch bei multiget-hrefs, PROPFIND Depth:1 auf `/principals/`, Teilnehmer-Autocomplete und Free-Busy. Antwortzeit und -größe dürfen nicht verraten, ob es den Benutzer gibt. | M1, M2 (Free-Busy), M4 (Autocomplete) | ✅ teilw. / 🆕 |
| Info-Disclosure (CLASS) | Für `CLASS:PRIVATE`/`CONFIDENTIAL` bei Lesefreigaben eine **Whitelist** der ausgegebenen Properties (nur DTSTART, DTEND, DURATION, RRULE, TRANSP, STATUS), keine Blacklist. Gleiche Logik für ICS-, jCal-, xCal-Export und Web-UI. | M5 | 🆕 |
| Free-Busy | Free-Busy nur für authentifizierte Benutzer mit Recht `read-free-busy`. Ausgabe nur Zeitblöcke (keine UID, SUMMARY, DTSTAMP). Rate-Limit. | M2 / M5 | 🆕 |
| RRULE-/Expansions-DoS | **Expansions-Budget**: max. Instanzen pro Anfrage (z. B. 10 000), max. Zeitfenster für `expand`/`limit-recurrence-set`, `COUNT` ≤ Limit, `INTERVAL` ≥ 1, FREQ=SECONDLY/MINUTELY nur mit kurzem Fenster oder ablehnen, BYxxx-Listen begrenzen, Iterationsbudget auch bei leeren Treffern (z. B. `BYMONTHDAY=31;BYMONTH=2`). VALARM `REPEAT` begrenzen. **Ein** RRULE-Parser für Validierung und Expansion, kein Validator-Expander-Mismatch. | M2 | 🆕 |
| Parser-DoS | iCal- und vCard-Parser: Limits für Zeilenlänge, Anzahl Properties, Parameter pro Property, Komponenten pro Objekt, Verschachtelungstiefe (iterativ statt rekursiv, sonst Tiefe ≤ 8), Anzahl ATTENDEE (z. B. 500), Länge einzelner Werte (E-Mail ≤ 254). Kein `panic!`/`unwrap` auf Eingaben: `clippy::unwrap_used`/`indexing_slicing` als deny im Parser-Crate. Checked arithmetic bei Datum und Zeit. | M2 (vCard M3) | 🆕 |
| Parser-Robustheit | `cargo-fuzz`-Targets für iCal, vCard, jCal, xCal, CSV, XML-Requests und RRULE-Expansion. Korpus aus libical-, ical4j- und vobject-Testdaten. Fuzzing in CI (Kurzlauf) und nightly. | M2 (Start), M6 (Formate), M7 (Langlauf) | 🆕 |
| Stored XSS | Web-UI (Leptos): Kalenderdaten **nur als Text** rendern, kein `inner_html`. HTML-Beschreibungen (`X-ALT-DESC;FMTTYPE=text/html`) nicht rendern oder nur über eine strikte Allowlist ohne SVG, `style` und Event-Attribute. Displaynamen, Kalendernamen und Farben (`calendar-color` per Regex `^#[0-9a-fA-F]{6,8}$`) validieren. CSP ✅ bleibt zweite Linie. Admin-Seiten genauso behandeln (Baïkal-Fall). | M4 | ✅ CSP / 🆕 Rendering-Regeln |
| XSS über Downloads | Bei GET auf Objekte immer `Content-Type: text/calendar`/`text/vcard` und `X-Content-Type-Options: nosniff`. vCard-PHOTO nie als `image/*` aus Benutzerdaten ausliefern. Falls Fotos angezeigt werden: Magic-Byte-Prüfung, nur PNG/JPEG/WebP, Neu-Encoding oder `Content-Disposition: attachment` sowie eigene Sandbox-CSP. | M3 / M4 | 🆕 |
| Gefährliche URIs | `URL`, `ATTACH`, `IMAGE`, `CONFERENCE`, vCard `URL`/`PHOTO`-URI: im Web-UI nur `https:`/`http:`/`mailto:` verlinken, nie `file:`, `javascript:`, `data:`, `\\host\share` oder `smb:`. Links mit `rel="noopener noreferrer"` und Hinweis auf externes Ziel. Kein automatisches Nachladen entfernter Bilder. | M4 | 🆕 |
| CSRF | REST-API und Web-UI: SameSite=Strict-Session-Cookie plus CSRF-Token oder Origin-/`Sec-Fetch-Site`-Prüfung für alle zustandsändernden Requests. DAV-Endpoints akzeptieren **keine** Cookie-Session, nur Basic/Bearer/App-Passwort. Damit kann eine fremde Webseite keine DAV-Requests im Namen des Benutzers absetzen. CORS standardmäßig aus. | M4 | 🆕 |
| Auth-Nebenpfade | Absolute Links (Passwort-Reset, Einladungen, OIDC-Redirect) nur aus der konfigurierten `public_base_url`, **nie** aus `Host`, `Origin` oder `X-Forwarded-*`. Tokens (Reset, Share, Teilnehmer, App-Passwort, webcal-Freigabe) aus CSPRNG mit ≥ 128 Bit, nur gehasht in der DB, Vergleich in konstanter Zeit, mit Ablauf. | M4 / M5 | 🆕 |
| Auth / App-Passwörter | App-Passwörter nur mit Scope (DAV vs. API, read-only möglich), einzeln widerrufbar, letzte Nutzung protokolliert. 2FA darf über Basic Auth nicht umgehbar sein, DAV also nur mit App-Passwort, wenn 2FA aktiv ist. Lockout ✅ zusätzlich IP-basiert gegen Password-Spraying über viele Benutzer. | M4 | ✅ teilw. / 🆕 |
| OIDC | `state`, `nonce` und PKCE prüfen. Issuer und Audience prüfen. Redirect-URI exakt matchen (kein Open Redirect über `return_to`; nur relative Pfade erlauben). Account-Linking nur über `sub`+`iss`, nie über E-Mail-Gleichheit. | M5 | 🆕 |
| SSRF (Webcal) | Abo-Fetcher als eigenes Modul: nur `http`/`https`/`webcal` (→ https). DNS **einmal** auflösen, **alle** A/AAAA prüfen und genau diese IP verbinden (Pinning gegen Rebinding). Blockliste: Loopback, RFC1918, Link-Local (169.254/16, fe80::/10), ULA fc00::/7, CGNAT 100.64/10, 0.0.0.0/8, Multicast, `::ffff:0:0/96` (IPv4-mapped), NAT64 64:ff9b::/96, 6to4, Metadaten-IPs. Redirects manuell, jeden Hop neu prüfen, max. 3. Kein Benutzer-Header, keine abschaltbare TLS-Prüfung. Timeout, Größenlimit (z. B. 10 MB), Content-Type prüfen. Fehlermeldungen an den Benutzer generisch (kein Port-Scan-Orakel). Admin-Schalter „Abos erlaubt“ und optionale Host-Allowlist. | M6 | ✅ (M6b) |
| Import-Injection | Import von iCal/vCard/jCal/xCal/CSV durch denselben validierenden Parser und dieselben Limits wie PUT. UID-Kollisionen dürfen keine fremden Objekte überschreiben (UID-Eindeutigkeit nur pro Kalender, Import nie kalenderübergreifend). | M6 | ✅ (M6a) |
| Export-Injection | Serializer escapt `\`, `;`, `,`, CR/LF in TEXT-Werten und faltet Zeilen gemäß RFC 5545. Keine CR/LF in Parametern (Quoting). **CSV-Export**: Zellen mit Präfix `=`, `+`, `-`, `@`, TAB, CR mit `'` neutralisieren (CWE-1236). | M2 (ICS), M6 (CSV) | ✅ (M6a) |
| iTIP/iMIP-Spoofing | Eingehende iMIP-Antworten (falls umgesetzt) nur akzeptieren, wenn die DMARC-geprüfte Absenderadresse zum ATTENDEE passt. Sonst als „unbestätigt“ markieren. Ausgehende Einladungen und Erinnerungen: ORGANIZER muss eine verifizierte Adresse des sendenden Benutzers sein. CalCraft darf kein offenes Relay für frei wählbare ATTENDEE-Listen sein. | S (Stufe A/B) | ✅ Stufe A (lokal): nur eigene Adressen als ORGANIZER, Adressen nur vom Admin, nie aus OIDC; Antworten ändern nur den eigenen PARTSTAT; keine Zustellung über Objekte eines anderen Organisators (UID-Übernahme). iMIP 🆕 (Stufe B) |
| Invite-Spam | Rate-Limit für E-Mails pro Benutzer und Tag. Max. Teilnehmer pro Einladung. Einladungen von außen nicht automatisch in den Kalender übernehmen (Opt-in, „nur bekannte Absender“). Storno-Mails nicht erneut mit Freitext des Angreifers versenden. Header-Injection in Betreff und Name (CRLF) ausschließen. | S (Stufe A/B) | ✅ Stufe A: `max_attendees`, `max_recipients_per_hour`, Inbox-Größe, Quota des Empfängers; Einladungen nur von Benutzern desselben Servers. E-Mail 🆕 (Stufe B) |
| Scheduling-Missbrauch | Server-seitiges Scheduling (RFC 6638): Schedule-Outbox nur für den eigenen Principal. Free-Busy-Anfragen nur für erlaubte Ziele. Inbox-Objekte fremder Benutzer unzugänglich. | S | ✅ Outbox-POST nur eigener Principal mit eigener ORGANIZER-Adresse, Inbox/Outbox fremder Benutzer = nicht existent (Autorisierungsmatrix), Free/Busy nur Zeiträume, abschaltbar (`free_busy = false`), Zeitraum begrenzt, Budgets für Serien (`docs/scheduling.md`) |
| Speicher-/Ressourcen | Neben Body-Limits ✅: Request-Timeouts, maximale Anzahl hrefs pro multiget (z. B. 1 000), Ergebnislimit und Pagination für REPORT und sync-collection (`507`/truncation), PROPFIND `Depth: infinity` ablehnen (403 `propfind-finite-depth`). Concurrency-Limit pro Benutzer. SQLite-Statement-Timeouts. | M1 (Depth), M3 (multiget/sync) | ✅ teilw. / 🆕 |
| SQL-Injection | Ausschließlich parametrisierte Queries (sqlx/rusqlite). Kein `format!` in SQL. Test/Lint per Grep in CI. `LIKE`-Suchen escapen `%`/`_`. | M1 | 🆕 (formalisieren) |
| Backup | Backups enthalten Passwort-Hashes und Tokens: verschlüsselt oder mit restriktiven Dateirechten (0600). Restore validiert Inhalte wie ein Import. Kein Download über die Web-UI ohne Re-Auth. | M6 | 🆕 |
| Supply-Chain | `cargo audit`/`cargo deny` in CI. Fremde iCal-Crates nur nach Review, da Upstream-Fixes nicht automatisch in Forks einfließen (Thunderbird-libical-Lehre). | M1 (CI), M7 | 🆕 |
| Prozess | Security-Policy, `SECURITY.md` mit Meldeweg, GitHub Private Vulnerability Reporting, Threat-Model-Dokument. Externes Review des Autorisierungs- und Parser-Codes. | M7 | 🆕 |

## 5. Testfälle, die wir als Regressionstests übernehmen sollten

Die Beschreibungen sind Payload-Ideen für **eigene** Tests gegen CalCraft. Sie sind keine funktionsfähigen Exploits gegen Dritte.

### WebDAV-Kern und XML (M1)
1. PROPFIND-Body mit `<!DOCTYPE>` und interner Entity (Billion-Laughs-Muster) → 400, kein Speicheranstieg.
2. Body mit externer Entity `SYSTEM "file:///etc/passwd"` und mit Parameter-Entity → 400, kein Dateizugriff, kein ausgehender Netzwerk-Request.
3. XML mit 10 000 Verschachtelungsebenen bzw. 100 000 `<prop>`-Elementen → 400 bzw. 413.
4. Objektnamen `../x.ics`, `..%2fx.ics`, `%252e%252e%252f`, `a\b.ics`, `c:/x.ics`, Name mit NUL-Byte, 10 kB langer Name → 400 oder 404, nie ein Zugriff außerhalb der Collection.
5. Benutzername `.*`, `admin%00`, Großbuchstaben und Unicode-Homoglyphen (`аdmin` mit kyrillischem a) → abgelehnt.
6. `PROPFIND Depth: infinity` → 403 mit `DAV:propfind-finite-depth`.
7. MOVE/COPY mit `Destination` in die Collection eines anderen Benutzers bzw. auf einen fremden Host → 403/404/502, Quelle unverändert.
8. Login-Timing: Antwortzeit für existierenden und nicht existierenden Benutzer statistisch gleich (Dummy-Hash ✅). Lockout greift auch bei verteilten Fehlversuchen.

### Autorisierung und IDOR (M1, M3, M5)
9. Benutzer B: GET, PUT, DELETE, PROPFIND, PROPPATCH, REPORT und MKCOL auf `/calendars/a/...` → jeweils 404, identisch zur Antwort für einen nicht existierenden Pfad.
10. **Nextcloud-Muster:** B setzt per PROPPATCH `group-member-set` oder `calendar-proxy-write` auf A's Principal bzw. fügt sich selbst ein → 403/404. Danach hat B weiterhin keinen Zugriff auf A's Kalender.
11. **Cyrus-Muster:** `calendar-multiget` in B's eigener Collection, mit einer Mischung aus eigenen hrefs und hrefs aus A's Kalender → fremde hrefs liefern 404 im Multistatus, keine Daten.
12. **RustiCal-Muster:** DELETE mit Hard-Delete- oder Papierkorb-Bypass-Option auf ein fremdes Objekt sowie Wiederherstellen und Endgültig-Löschen aus fremdem Papierkorb → kein Effekt.
13. sync-collection mit manipuliertem oder fremdem `sync-token` (Token aus A's Collection in B's Request) → Fehler `valid-sync-token`, keine Änderungsliste von A.
14. Lesefreigabe: PUT und DELETE durch die Leserolle → 403. Freigabe widerrufen → sofort 404 (kein Cache).
15. REST-API: dieselbe Matrix über numerische und UUID-IDs (IDs von A mit Token von B).

### iCalendar-Parser und Recurrence (M2)
16. **Stalwart-Muster:** `RRULE:FREQ=SECONDLY` ohne COUNT/UNTIL plus REPORT mit `<expand>` über 100 Jahre → Ablehnung oder Abbruch nach Budget, Speicher bleibt unter Grenze X.
17. `COUNT=2147483646`, `COUNT=-1`, `INTERVAL=0`, `INTERVAL=-1`, `UNTIL` im Jahr 99999, `BYSETPOS=0` → sauberer Fehler, kein Panic, keine Endlosschleife.
18. „Leere“ Regel `FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30` → terminiert innerhalb des Iterationsbudgets.
19. BYxxx-Listen mit 10 000 Einträgen und 10 000 RDATE/EXDATE → Limit greift.
20. VALARM mit `REPEAT:1000000000` und `DURATION:PT1S` → Limit greift (python-icalendar-Muster).
21. 300 Overrides (RECURRENCE-ID) mit je 1 MB DESCRIPTION → Body- oder Objektlimit greift.
22. Property-Name mit 100 000 Zeichen (Cyrus-Muster), Zeile ohne Doppelpunkt, nur CR ohne LF, ungültiges UTF-8, abgeschnittene Folding-Sequenz → 400 ohne Panic.
23. 1 000 verschachtelte `BEGIN:VEVENT` bzw. `BEGIN:VALARM` (vobject-Muster) → 400.
24. 10 000 ATTENDEE-Zeilen bzw. ATTENDEE mit 100 kB langer `mailto:`-Adresse (Nextcloud-Muster) → 400.
25. TZID-Referenz auf eine nicht vorhandene VTIMEZONE, VTIMEZONE mit 100 000 Übergängen, DTSTART mit Jahr 0 oder 10000 → sauberer Fehler.
26. Roundtrip-Test: Parser → Serializer → Parser ist idempotent. TEXT mit `\n`, `;`, `,` und CRLF bleibt Text und wird nicht zu neuen Properties (Wallos-Muster).
27. Fuzz-Korpus-Regression: jeder Absturzfund aus `cargo-fuzz` wird als fester Testfall eingecheckt.

### CardDAV und vCard (M3)
28. vCard mit `PHOTO;ENCODING=b;TYPE=PNG:` und Inhalt `<svg onload=...>` bzw. HTML → Auslieferung nie als `image/svg+xml` oder `text/html`, `nosniff` gesetzt (Nextcloud-CVE-2016-9465-Muster).
29. `PHOTO;VALUE=uri:http://169.254.169.254/...` bzw. `file:///` → Server lädt nichts nach.
30. vCard 2.1/3.0/4.0 gemischt, QUOTED-PRINTABLE mit überlangen Soft-Breaks, 10 000 TEL-Einträge → Limits greifen.

### Web-UI, REST-API, Auth (M4)
31. SUMMARY, LOCATION, DESCRIPTION, Kalender-Displayname, Benutzer-Displayname, Kontaktname und ORG jeweils mit `<img src=x onerror=…>`, `<svg><animate onrepeat=…>`, `<details open ontoggle=…>`, `"><script>`, `javascript:`-URL → wird als Text angezeigt, kein DOM-Element entsteht, CSP-Report bleibt leer. Gilt ausdrücklich auch für Admin-Seiten (Baïkal-Muster).
32. `X-ALT-DESC;FMTTYPE=text/html` mit Script, `<style>` und `<base href>` → nicht gerendert bzw. vollständig entschärft.
33. `calendar-color` mit `red;background:url(...)` oder `#fff" onmouseover="` → 400.
34. `URL`/`ATTACH` mit `file:///C:/Windows/System32/calc.exe`, `\\attacker\share\x`, `javascript:alert(1)`, `data:text/html,…` → im UI nicht klickbar (Thunderbird- und Outlook-Muster).
35. ATTACH mit externer URL auf Open-Redirect-Ziel → UI zeigt Ziel-Host vor dem Öffnen (Nextcloud-CVE-2024-37316-Muster).
36. CSRF: zustandsändernder REST-Request mit fremdem `Origin` bzw. ohne CSRF-Token und mit Session-Cookie → 403. DAV-Request mit nur Session-Cookie → 401.
37. Passwort-Reset mit manipuliertem `Host`, `Origin` und `X-Forwarded-Host` → Link in der Mail verwendet trotzdem `public_base_url` (SOGo-Muster).
38. Basic Auth mit Hauptpasswort bei aktivierter 2FA → 401. App-Passwort mit Scope „read-only“ → PUT 403.
39. Token-Qualität: 10 000 erzeugte Share-, Reset- und Teilnehmer-Tokens haben keine Kollisionen und keine erkennbare Struktur (Nextcloud-CVE-2025-66511-Muster). Tokens stehen nur gehasht in der DB.
40. Teilnehmer-Autocomplete und Benutzersuche liefern nur erlaubte Benutzer. Antwort ohne Treffer ist identisch für existierende und nicht existierende Namen (Nextcloud-CVE-2026-45286-Muster).

### OIDC, Freigaben, E-Mail (M5)
41. OIDC-Callback mit fehlendem oder falschem `state`, wiederverwendetem `code`, falschem `iss`/`aud`, abgelaufenem Token, `nonce`-Replay → abgelehnt.
42. `return_to=https://evil.example` bzw. `//evil.example` bzw. `/\evil.example` → nur relative, interne Pfade erlaubt.
43. OIDC-Benutzer mit gleicher E-Mail wie ein lokaler Admin → wird **nicht** automatisch verknüpft.
44. Vertraulicher Termin (`CLASS:CONFIDENTIAL`) über Lesefreigabe, Free-Busy, ICS-Export, jCal, xCal und REST → nur Zeitdaten, keine UID, SUMMARY, DTSTAMP oder ATTENDEE (SOGo-CVE-2016-6189/6190-Muster).
45. Free-Busy anonym bzw. für nicht existierenden Benutzer → gleiche Antwort (EGroupware-Muster).
46. Einladung mit ORGANIZER ungleich verifizierter Adresse des Absenders → wird nicht versendet. ✅ lokal (`tests/scheduling.rs`, `organizer_spoofing_reaches_nobody`)
47. Einladung an 5 000 Teilnehmer bzw. 100 Einladungen pro Minute → Rate-Limit. ✅ lokal (`limits`)
48. SUMMARY mit CRLF und `Bcc:`-Header → keine Header-Injection in der ausgehenden Mail.
49. (Falls iMIP-Eingang) REPLY von Absender X für ATTENDEE Y → Status wird nicht übernommen bzw. als unbestätigt markiert.

### Import, Export, Webcal, Backup (M6)
50. Abo-URLs `http://127.0.0.1`, `http://[::1]`, `http://[::ffff:127.0.0.1]`, `http://0x7f000001`, `http://2130706433`, `http://0177.0.0.1`, `http://169.254.169.254`, `http://[fd00::1]`, `http://100.64.0.1`, `http://[64:ff9b::7f00:1]`, `file:///etc/passwd`, `gopher://`, `ftp://` → abgelehnt (Nextcloud-CVE-2020-8138-Muster).
51. Öffentliche URL, die per 302 auf `http://127.0.0.1:8080/admin` umleitet → abgelehnt.
52. DNS-Rebinding-Simulation: lokaler Test-Resolver liefert erst eine öffentliche, dann eine interne IP bzw. mehrere Records (öffentlich + intern) → abgelehnt (OX-Muster).
53. Abo-Antwort von 1 GB, Slow-Loris-Antwort und `Content-Type: text/html` → Größen- und Zeitlimit greifen. Fehlermeldung an den Benutzer verrät weder Port-Status noch Inhalte.
54. Abo-Feed mit XSS-Payloads und RRULE-Bomben → dieselben Filter und Limits wie bei PUT.
55. Import einer ICS mit UID, die bereits in einem fremden Kalender existiert → kein Überschreiben fremder Daten.
56. xCal-Import mit DOCTYPE oder Entity → abgelehnt (gleicher XML-Parser).
57. CSV-Import und -Export: Feld `=HYPERLINK("http://…")`, `+cmd|…`, `@SUM(…)`, Feld mit führendem TAB → im Export mit `'` neutralisiert. CSV-Import mit 10 Mio. Zeilen bzw. Zeile mit 100 MB → Limit greift.
58. Export: TEXT mit CRLF + `BEGIN:VEVENT` → erzeugt kein zusätzliches Event (Wallos-Muster).
59. Backup-Datei: Rechte 0600, Restore einer manipulierten Sicherung (Pfad-Einträge mit `../`, übergroße Objekte) → abgelehnt.

### Managed Attachments (RFC 8607)
61. Benutzer B schreibt die Managed-ID bzw. die Download-URL eines Anhangs von A in ein eigenes Termin-ATTACH (PUT, Import, Abo-Feed, COPY einer Einladung) → kein Zugriff, PUT mit `CALDAV:valid-managed-id-parameter` abgelehnt; Download fremder Anhänge byte-identisch zu nie vergebenen IDs (404). ✅ (`tests/attachments.rs`, `foreign_managed_ids_cannot_be_injected`, Matrix)
62. Upload als `text/html`, `image/svg+xml` oder `application/xhtml+xml` mit Script → Auslieferung nur als `application/octet-stream` mit `Content-Disposition: attachment`, `nosniff` und `Content-Security-Policy: default-src 'none'; sandbox` (Nextcloud-Auto-Download-/XSS-Muster). ✅ (`hostile_file_names_and_media_types`)
63. Dateiname mit `../`, Backslashes, CRLF + `Set-Cookie:`, RTL-Override (`invoice\u202Efdp.exe`), unsichtbaren Zeichen → bereinigt, kein Header-Injection, Header nur sichtbares ASCII (RFC 6266 §4.3). ✅ (Unit-Tests, Fuzz-Ziel `attach`)
64. Teilnehmer ändert oder entfernt den Anhang des Organisators per POST → 403 `need-privileges`; nach Absage bzw. Entfernen aus der Teilnehmerliste kein Download mehr. ✅ (`attendees_read_but_cannot_change_and_lose_access_on_cancel`)
65. Upload größer als `max_bytes` (mit und ohne `Content-Length`), mehr Anhänge als `max_per_object`, volle Quota (auch durch den Papierkorb) → 403/507 ohne Rückstände. ✅ (`size_limits_and_quota`)
66. Parallele Uploads bzw. Upload parallel zu DELETE → nichts geht verloren, keine verwaisten Dateien. ✅ (`parallel_uploads_lose_nothing`, `upload_racing_a_delete_leaves_no_orphans`)

### Release (M7)
60. Gesamter Autorisierungs-Matrix-Test grün. Fuzzing-Langlauf ohne Funde. `cargo audit` sauber. Header-Check (CSP, HSTS, nosniff, frame-ancestors, Referrer-Policy) auf allen UI- und API-Antworten.

---

*Hinweis: Einträge mit „(Sek.)“ und alle CVEs aus 2025/2026 stammen teils aus Aggregatoren. Vor einer Weitergabe nach außen gegen NVD oder das Vendor-Advisory verifizieren.*

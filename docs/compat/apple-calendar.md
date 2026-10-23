# Testprotokoll: Apple Kalender, Erinnerungen und Kontakte

Manueller Test mit echten Apple-Geräten (Punkt „Reale Clients“ in
[produktionsreife.md](../produktionsreife.md)). Pro Fall Ergebnis eintragen:
✅ funktioniert · ⚠️ funktioniert mit Einschränkung (Notiz) · ❌ Fehler (Notiz,
möglichst mit Uhrzeit für die Server-Logs) · – nicht getestet.

| Feld | Wert |
|---|---|
| CalCraft-Version | |
| Geräte und Systemversionen | z. B. macOS 26.1 (Kalender 15), iOS 26.1 |
| Einrichtung | Konto „CalDAV“, manuell / automatisch (`.well-known`), App-Passwort ja/nein |
| Server-Konfiguration | `[scheduling]`, `[attachments]`, `[subscriptions]`, `[sharing]`, `[publish]` an/aus |
| Getestet von, am | |

## Grundfunktionen

| # | Fall | macOS | iOS | Notiz |
|---|---|---|---|---|
| 1 | Konto einrichten (Discovery über `.well-known`, Principal, Home-Sets) | | | |
| 2 | Kalender anlegen, umbenennen, Farbe ändern, löschen | | | |
| 3 | Reihenfolge der Kalender ändern, bleibt nach Neustart erhalten | | | |
| 4 | Termin anlegen, ändern, verschieben, löschen | | | |
| 5 | Ganztägiger und mehrtägiger Termin | | | |
| 6 | Termin mit Erinnerung (Hinweis), Erinnerung erscheint | | | |
| 7 | Änderung auf Gerät A erscheint auf Gerät B (Sync) | | | |
| 8 | Gelöschter Termin im Papierkorb der Web-UI, Wiederherstellen erscheint wieder | | | |
| 9 | Erinnerungen-App: Aufgabe anlegen, erledigen, löschen (VTODO) | | | |
| 10 | Kontakte: Kontakt mit Foto anlegen, ändern, Gruppe anlegen | | | |

## Serien und Zeitzonen

| # | Fall | macOS | iOS | Notiz |
|---|---|---|---|---|
| 11 | Wöchentliche Serie, einzelne Instanz verschieben | | | |
| 12 | Einzelne Instanz löschen (EXDATE), „dieses und alle zukünftigen“ ändern | | | |
| 13 | Termin in anderer Zeitzone, Anzeige nach Sommerzeitwechsel | | | |
| 14 | Monatliche Serie „letzter Freitag“ | | | |

## Konflikte und Offlinebetrieb

| # | Fall | macOS | iOS | Notiz |
|---|---|---|---|---|
| 15 | Offline ändern, danach online: Änderung kommt an | | | |
| 16 | Gleichzeitig auf zwei Geräten ändern: Konflikt sichtbar, keine Daten verloren | | | |

## Neue Funktionen (nur wenn eingeschaltet)

| # | Fall | macOS | iOS | Notiz |
|---|---|---|---|---|
| 17 | Abo (webcal) über die Web-UI anlegen, erscheint schreibgeschützt | | | |
| 18 | Einladung an zweiten Nutzer, Annahme auf dessen Gerät, Status beim Organisator | | | |
| 19 | Absage durch den Organisator kommt beim Teilnehmer an | | | |
| 20 | Verfügbarkeit (Frei/Belegt) des anderen Nutzers beim Planen | | | |
| 21 | Anhang an Termin hinzufügen, beim Teilnehmer herunterladen | | | |
| 22 | Kalender teilen („Teilen“), Einladung annehmen, lesend/schreibend | | | |
| 23 | Freigabe entfernen: Kalender verschwindet beim Empfänger | | | |
| 24 | Öffentlichen Link (`webcal://`) abonnieren | | | |

## Gefundene Probleme

| Datum | Fall | Beschreibung | Issue/PR |
|---|---|---|---|
| | | | |

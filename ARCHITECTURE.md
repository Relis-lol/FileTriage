# Architektur

## Stack-Entscheidung

**Tauri 2 mit Rust-Backend und statischem Vanilla-JS-Frontend.**

Der harte Teil dieses Produkts ist nicht die Oberfläche, sondern das Backend:
rekursiver Scan über 100.000 Dateien, mehrstufiges Hashing, SQLite-Index,
Undo-Journal, robustes Verhalten bei gesperrten Dateien und langen Pfaden.
Rust liefert dafür `jwalk` und `rayon` für parallelen Scan ohne GC-Pausen,
`blake3` als deutlich schnellere Alternative zu SHA-256 und eine
`Result`-basierte Fehlerbehandlung, die jeden IO-Fehler zu einer bewussten
Entscheidung zwingt — bei einem Werkzeug, das Dateien verschiebt, ist das ein
Sicherheitsargument.

Die Alternative C#/WPF wäre bei nativen Shell-Vorschauen und RAM-Verbrauch im
Vorteil (etwa 50 MB weniger). Dagegen stand: Die Oberfläche ist im Kern ein
Hotkey-Leser mit Kartenansicht, was HTML/CSS präziser abbildet als XAML, und
das Frontend bleibt hier ohne Node.js und ohne Build-Pipeline auskommend.

Gegen Electron sprach der Speicherverbrauch: WebView2 ist auf Windows 11 und
aktuellem Windows 10 bereits installiert, ein eigener Chromium muss nicht
mitgeliefert werden.

## Schichten

```
ui/                     Statisches Frontend (HTML/CSS/JS, kein Bundler)
  └── invoke() ──────►  commands.rs        dünne Befehlsschicht, Validierung
                          ├── state.rs     Konfiguration, Scan, Überwachung
                          ├── scan.rs      Walker, inkrementelle Indexpflege
                          ├── query.rs     Listen, Suche, Kennzahlen
                          ├── fileops.rs   Verschieben, Papierkorb, Kollisionen
                          ├── journal.rs   Operationsjournal + Zustandsautomat
                          ├── recovery.rs  Diagnose unfertiger Operationen
                          ├── undo.rs      Einzel-, Stapel- und Sitzungs-Undo
                          ├── fsmeta.rs    Reparse Points, Cloud, Dateikennung
                          ├── dup.rs       dreistufige Duplikatsuche
                          ├── rules.rs     Regel-Engine + Vorschläge
                          ├── watcher.rs   Ordnerüberwachung mit Rescan-Signal
                          └── preview.rs   Vorschauen im Arbeitsspeicher
                                └── db.rs  SQLite-Index (nur Metadaten)
```

## Die Operationszustandsmaschine

Der Kern der Datensicherheit. Jede verändernde Operation schreibt ihre Absicht
ins Journal, **bevor** sie das Dateisystem anfasst:

```
                    ┌──────────────────────────── failed
                    │
plan ──► planned ──►┼── (gleiches Volume) ──────► completed
                    │
                    └── copying ──► copied ──► verified ──► source_removal_pending ──► completed
                                        │           │                  │
                                        └───────────┴──────────────────┴──► rollback_pending ──► rolled_back
```

`recovery.rs` liest beim Start alle Einträge ohne Endzustand und bildet aus
Journal **und** tatsächlichem Dateisystem eine Diagnose. Automatisch aufgelöst
werden ausschließlich die zwei Fälle, in denen keine Datei angefasst wird:
„nachweislich nichts passiert" und „war doch fertig, nur der Vermerk fehlte".
Alles andere legt der Nutzer fest.

Warum kein verteiltes Transaktionssystem: Es gibt genau einen Prozess, eine
Datenbank und ein Dateisystem. Gebraucht wird lokale Absturzsicherheit, nicht
Konsens.

## Datei-Identität

`fsmeta.rs` holt über `GetFileInformationByHandle` die NTFS-Dateikennung
(Volume-Seriennummer + Dateiindex + Anzahl der Verweise). Damit lassen sich
drei Fragen beantworten, die die Rust-Standardbibliothek auf stabilem Rust
nicht beantwortet:

- Ist das zwischen Prüfung und Ausführung noch dieselbe Datei? (TOCTOU)
- Zeigen zwei Pfade auf dieselben Daten? (Hardlink)
- Wurde die Datei nur umbenannt oder ausgetauscht?

Geöffnet wird nur mit `FILE_READ_ATTRIBUTES` und `FILE_FLAG_OPEN_REPARSE_POINT`:
Das sperrt nichts, folgt keiner Verknüpfung und löst insbesondere keinen
Cloud-Download aus.

Wichtig: Die Kennung beweist Identität, **nicht** unveränderten Inhalt. Wird
eine Datei an Ort und Stelle überschrieben, bleibt die Kennung gleich. Deshalb
prüft Undo immer beides — Kennung und Fingerabdruck.

Fachmodule kennen weder Tauri noch die Oberfläche. Sie bekommen eine
`Connection` und liefern `Result<T, AppError>`. Damit sind sie ohne laufendes
Fenster testbar — alle Tests laufen als reine Rust-Tests.

## Nebenläufigkeit

Die Oberfläche darf nie blockieren, deshalb:

- **Scan** läuft in einem eigenen Thread, meldet Fortschritt über das Ereignis
  `scan:progress` und ist über ein `AtomicBool` abbrechbar.
- **Hashing** parallelisiert über `rayon`, aber erst in Stufe 2 und 3 der
  Duplikatsuche.
- **Vorschauen** werden auf Anfrage einzeln erzeugt und verworfen, mit harter
  Größengrenze.
- **SQLite** läuft im WAL-Modus, Verbindungen werden pro Vorgang geöffnet. Das
  vermeidet eine globale Sperre zwischen Oberfläche und Hintergrundscan.

## Datenmodell

| Tabelle | Zweck |
|---|---|
| `roots` | ausgewählte Ordner und ihre Scan-Optionen |
| `files` | Metadaten je Datei, Hash-Status, Triage-Zustand |
| `actions` | Journal aller Dateiaktionen, Grundlage für Undo |
| `rules` | Regeln mit Bedingung (JSON), Verknüpfung und Zielordner |
| `move_stats` | Verhaltensstatistik als Grundlage für Typvorschläge |
| `dismissed_patterns` | weggeklickte Namensmuster — sie kommen nicht wieder |
| `settings` | Konfiguration als JSON unter dem Schlüssel `config` |

Der Index ist **niemals die Wahrheit** — die Platte ist es. Vor jeder Aktion
wird gegen das Dateisystem geprüft. Der Index darf jederzeit gelöscht werden.

Schema-Versionierung über `PRAGMA user_version`, Migrationen in `db::migrate`.

## Regeln: was sie dürfen

Eine Regel ist eine **Suchanfrage mit einem Zielvorschlag**, keine
Automatisierung. Der Weg zur Ausführung ist genau einer und lässt sich nicht
abkürzen:

```
Regel  →  Treffer ermitteln  →  Dry Run  →  Bestätigung  →  fileops::move_file
```

Es gibt keinen Zeitgeber, keinen Hintergrundlauf und keinen Pfad von der
Ordnerüberwachung zu einer Dateioperation. `rules::apply` ist die einzige
Funktion, die etwas bewegt, und sie bekommt die Anzahl mit, die der Nutzer in
der Vorschau gesehen hat: Weicht der tatsächliche Umfang stark davon ab,
passiert nichts. Verschoben wird über `fileops::move_file` — die Regel-Engine
enthält keinen eigenen Dateicode und umgeht keine Prüfung.

### Bedingungen

| Feld | Vergleiche |
|---|---|
| `name` | `contains`, `starts_with`, `ends_with`, `eq` |
| `path` | `contains`, `starts_with`, `ends_with`, `eq` |
| `ext` | `in` |
| `category` | `in` |
| `size` | `gt`, `lt`, `eq` |
| `age_days` | `gt`, `lt` |

`match_mode` verknüpft sie mit `all` (Voreinstellung) oder `any`.

Für Namen und Pfade gilt durchgehend:

- **Groß- und Kleinschreibung ist egal**, Unicode-korrekt — `rechnung` findet
  `RECHNUNG.pdf`, `übersicht` findet `ÜBERSICHT.pdf`.
- **Namen werden nie verändert.** Regeln lesen, sie benennen nicht um.
- Ein **führender Punkt** gehört beim Namen zum Muster: `.backup` sucht
  wirklich nach `.backup`. Nur bei `ext` fällt er weg, weil dort `.pdf` und
  `pdf` dasselbe meinen.
- `ends_with` auf `name` vergleicht den Namen **ohne Endung**: `_backup`
  findet `projekt_backup.zip`. Zusätzlich passt der vollständige Name, damit
  ein eingetipptes `.zip` nicht wirkungslos bleibt.
- Mehrere Werte in einer Bedingung wirken als „eines davon".
- Eine Bedingung ohne Text wird abgelehnt — sie würde auf alles passen.

Reguläre Ausdrücke gibt es nicht. Sie sind für die Zielgruppe schwer richtig
zu schreiben und im Fehlerfall teuer.

### Vorschläge

Zwei Quellen, beide passiv, beide ausschließlich aus dem eigenen Verhalten:

- **`move_stats`** — Dateityp und Endung: „du hast PDFs viermal hierhin
  geschoben".
- **`patterns`** — wiederkehrende Wörter im Dateinamen, gelesen aus dem
  Aktionsjournal. Zurückgenommene Aktionen zählen nicht.

Ein Vorschlag wird nur beim Öffnen der Regelseite berechnet, meldet sich nie
von selbst und fasst keine Datei an. Er nennt seine Belege, damit man ihm nicht
glauben muss.

## Inkrementeller Scan

Jeder Durchlauf bekommt eine streng monoton steigende Markierung. Gefundene
Dateien werden per `INSERT … ON CONFLICT` aktualisiert und mit der Markierung
versehen; danach fallen alle Zeilen des Ordners mit kleinerer Markierung heraus.
Wird der Scan abgebrochen, unterbleibt dieser Schritt — ein Abbruch darf den
Index nicht leerräumen.

Der Triage-Zustand überlebt einen erneuten Scan. Hashes werden verworfen, sobald
sich Größe oder Änderungszeit unterscheiden.

## Windows-Besonderheiten

- **Lange Pfade**: `paths::extended` setzt bei Bedarf das Präfix `\\?\`, für
  UNC-Pfade `\\?\UNC\`. Gespeichert und angezeigt wird immer die Form ohne
  Präfix.
- **Groß-/Kleinschreibung**: Eindeutigkeit läuft über `path_key`
  (kleingeschrieben, Trennzeichen vereinheitlicht), Anzeige über die
  Originalschreibweise.
- **Versteckte und System-Dateien** werden über die Dateiattribute erkannt,
  nicht über den Namen.
- **Symlinks und Junctions** werden standardmäßig nicht verfolgt: sonst drohen
  Endlosschleifen und Aktionen außerhalb des gewählten Ordners.

## Erweiterbarkeit

Für spätere Funktionen ohne Umbau vorgesehen:

- OCR, PDF-Volltext und Embeddings hängen sich an `files` über zusätzliche
  Tabellen mit Fremdschlüssel, nicht über neue Spalten.
- Die Regel-Engine bewertet eine Liste von Bedingungen; neue Bedingungsarten
  sind additiv.
- Die Vorschau ist ein Trait-artiger Dispatch nach Kategorie; neue Erzeuger
  (PDF, Video) kommen als weiterer Zweig dazu.

//! Prüfungen der Oberfläche.
//!
//! Automatisierte Tests können nicht beurteilen, ob eine Oberfläche gut
//! aussieht. Sie können aber sehr wohl prüfen, ob die Zusagen eingehalten
//! werden, die wir uns gegeben haben — und genau das tun sie hier, indem sie
//! die Quelldateien der Oberfläche lesen:
//!
//!   * keine Emoji als Bedienelemente
//!   * jede Schaltfläche ohne Text hat einen zugänglichen Namen
//!   * jede Ansicht hat einen erklärenden leeren Zustand
//!   * jede Tastaturaktion hat eine anklickbare Entsprechung
//!   * Farben kommen aus Tokens, nicht als verstreute Hexcodes
//!   * Hell und Dunkel sind beide vollständig definiert
//!
//! Das ersetzt keinen Blick auf einen Screenshot. Es verhindert aber, dass
//! einer dieser Punkte still wieder verloren geht.

#![cfg(test)]

use std::path::{Path, PathBuf};

fn ui_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR zeigt auf src-tauri.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ui")
}

fn lies(rel: &str) -> String {
    let p = ui_dir().join(rel);
    let roh = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{} nicht lesbar: {e}", p.display()));
    // Zeilenenden vereinheitlichen. Einige Dateien liegen mit CRLF vor, und
    // Pruefungen, die einen Block zwischen zwei Zeilenumbruechen suchen,
    // wuerden sonst stillschweigend die ganze Datei betrachten - und damit
    // nichts mehr pruefen, ohne dass es auffaellt.
    roh.replace("\r\n", "\n")
}

/// Entfernt Kommentare. In einem Kommentar darf ein Pfeil stehen - er wird
/// niemandem angezeigt.
fn ohne_kommentare(quelle: &str) -> String {
    let mut out = String::with_capacity(quelle.len());
    let mut im_block = false;
    for zeile in quelle.lines() {
        let t = zeile.trim_start();
        if im_block {
            if let Some(p) = t.find("*/") {
                im_block = false;
                out.push_str(&t[p + 2..]);
            }
            out.push('\n');
            continue;
        }
        if t.starts_with("//") {
            out.push('\n');
            continue;
        }
        if t.starts_with("/*") {
            if !t.contains("*/") {
                im_block = true;
            }
            out.push('\n');
            continue;
        }
        // Kommentar am Zeilenende. Ein "//" innerhalb einer Zeichenkette
        // (etwa "tauri://drag-drop") darf dabei nicht abgeschnitten werden.
        match zeile.find("//") {
            Some(p) if !zeile[..p].ends_with(':') => out.push_str(&zeile[..p]),
            _ => out.push_str(zeile),
        }
        out.push('\n');
    }
    out
}

/// Der deutsche Sprachkatalog als roher Text.
///
/// Die Sprachkataloge sind die Quelle sichtbarer Texte. Der separate
/// JavaScript-Scanner bleibt eine Heuristik; null Fundstellen sind kein
/// Vollstaendigkeitsbeweis.
fn katalog() -> String {
    lies("locales/de.json")
}

/// Der englische Katalog. Er ist die Rueckfallebene und muss deshalb
/// vollstaendig sein.
fn katalog_en() -> String {
    lies("locales/en.json")
}

fn alle_js() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for rel in [
        "app.js",
        "lib/api.js",
        "lib/dom.js",
        "lib/format.js",
        "lib/icons.js",
        "views/home.js",
        "views/triage.js",
        "views/files.js",
        "views/duplicates.js",
        "views/rules.js",
        "views/settings.js",
        "views/recovery.js",
    ] {
        out.push((rel.to_string(), lies(rel)));
    }
    out
}

// ---------------------------------------------------------------------------
// Erster Start
// ---------------------------------------------------------------------------

/// Der Erststart darf kein Assistent mehr sein.
#[test]
fn es_gibt_keinen_einrichtungsassistenten_mehr() {
    assert!(
        !ui_dir().join("views/onboarding.js").exists(),
        "der Assistent ist wieder da - der erste Start soll direkt zur Sache kommen"
    );
    let app = lies("app.js");
    assert!(!app.contains("onboarding"), "app.js verweist noch auf den Assistenten");

    // Keine Schrittzählung mehr in der Oberfläche.
    for (name, inhalt) in alle_js() {
        assert!(
            !inhalt.contains("schritt === 0"),
            "{name} enthält noch eine Schrittfolge"
        );
    }
}

/// Der erste Bildschirm nennt Zweck, Hauptaktion und die bekannten Ordner.
#[test]
fn erster_bildschirm_erklaert_sich_selbst() {
    let home = lies("views/home.js");
    let texte = katalog();
    assert!(texte.contains("Ordner wählen"), "die Hauptaktion fehlt");
    assert!(
        home.contains("knownFolders"),
        "die erkannten Windows-Ordner werden nicht angeboten"
    );
    assert!(
        texte.contains("in dieses Fenster ziehen"),
        "auf Drag and Drop wird nicht hingewiesen"
    );
    // Der Zweck in einem Satz.
    assert!(
        texte.contains("Räum einen unordentlichen Ordner auf"),
        "der Zweck steht nicht auf dem ersten Bildschirm"
    );
}

/// Ein hineingezogener Ordner darf nur ausgewählt werden, niemals eine
/// Dateioperation auslösen.
#[test]
fn drag_and_drop_waehlt_nur_aus() {
    let app = lies("app.js");
    assert!(app.contains("tauri://drag-drop"), "es wird kein Ordner angenommen");
    assert!(
        app.contains("ordnerUebernehmen"),
        "der Drop geht nicht durch die geprüfte Übernahme"
    );

    // Der Drop-Zweig darf keine verändernde Operation aufrufen.
    let start = app.find("tauri://drag-drop").unwrap();
    let ende = (start + 900).min(app.len());
    let zweig = &app[start..ende];
    for verboten in ["moveTo", "deleteFile", "deleteMany", "renameFile", "applyRule"] {
        assert!(
            !zweig.contains(verboten),
            "der Drop-Zweig ruft {verboten} auf - ein Drop darf nur auswählen"
        );
    }
}

// ---------------------------------------------------------------------------
// Symbole
// ---------------------------------------------------------------------------

/// Emoji sind auf jedem System anders und lassen sich nicht einfärben.
#[test]
fn keine_emoji_als_bedienelemente() {
    for (name, roh) in alle_js() {
        let inhalt = ohne_kommentare(&roh);
        // Einzige Ausnahme: In `taste()` benennen die Pfeile die Pfeiltasten
        // der Tastatur. Dort ist "→" die richtige Beschriftung und kein Ersatz
        // fuer ein Symbol.
        let pfeile_erlaubt = name == "lib/format.js";
        for zeichen in inhalt.chars() {
            let c = zeichen as u32;
            let ist_pfeil = (0x2190..=0x21FF).contains(&c);
            if ist_pfeil && pfeile_erlaubt {
                continue;
            }
            let ist_emoji = (0x1F300..=0x1FAFF).contains(&c)
                || (0x2600..=0x27BF).contains(&c)
                || (0x2B00..=0x2BFF).contains(&c)
                || ist_pfeil
                || (0x2300..=0x23FF).contains(&c)
                || (0x25A0..=0x25FF).contains(&c);
            assert!(
                !ist_emoji,
                "{name} enthält das Symbolzeichen U+{c:04X} ({zeichen}) - \
                 Bedienelemente kommen aus lib/icons.js"
            );
        }
    }
}

/// Jedes in der Oberfläche verwendete Symbol muss es auch geben.
#[test]
fn alle_verwendeten_symbole_existieren() {
    let icons = lies("lib/icons.js");
    // Namen aus der FORMEN-Tabelle einsammeln.
    let tabelle_start = icons.find("const FORMEN = {").expect("FORMEN fehlt");
    let tabelle_ende = icons.find("/** Dateikategorie").expect("Ende der Tabelle fehlt");
    let tabelle = &icons[tabelle_start..tabelle_ende];

    let mut vorhanden: Vec<String> = Vec::new();
    for zeile in tabelle.lines() {
        let t = zeile.trim();
        // Zeilen der Form "name: [" oder "name: ['...'],"
        if let Some(pos) = t.find(':') {
            let name = &t[..pos];
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric())
                && t[pos..].trim_start_matches(':').trim_start().starts_with('[')
            {
                vorhanden.push(name.to_string());
            }
        }
    }
    assert!(vorhanden.len() > 25, "erwartet ein vollständiges Set, gefunden {}", vorhanden.len());

    // Alle Aufrufe icon('name') und btn({ icon: 'name' }) prüfen.
    let mut fehlend = Vec::new();
    for (datei, inhalt) in alle_js() {
        if datei == "lib/icons.js" {
            continue;
        }
        for muster in ["icon('", "icon: '", "iconBtn('"] {
            let mut rest = inhalt.as_str();
            while let Some(p) = rest.find(muster) {
                rest = &rest[p + muster.len()..];
                if let Some(e) = rest.find('\'') {
                    let name = &rest[..e];
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric())
                        && !vorhanden.iter().any(|v| v == name)
                    {
                        fehlend.push(format!("{datei}: {name}"));
                    }
                }
            }
        }
    }
    assert!(fehlend.is_empty(), "unbekannte Symbole verwendet: {fehlend:?}");
}

// ---------------------------------------------------------------------------
// Bedienbarkeit
// ---------------------------------------------------------------------------

/// Eine Schaltfläche ohne sichtbaren Text braucht einen Namen für alle, die
/// sie nicht sehen.
#[test]
fn schaltflaechen_ohne_text_haben_einen_namen() {
    let dom = lies("lib/dom.js");
    assert!(
        dom.contains("export function iconBtn"),
        "der Helfer für Symbol-Schaltflächen fehlt"
    );
    // iconBtn erzwingt das Label über die Signatur.
    assert!(
        dom.contains("ariaLabel: label"),
        "iconBtn setzt keinen zugänglichen Namen"
    );
    // Und btn() setzt einen Namen, wenn kein Text da ist.
    assert!(
        dom.contains("'aria-label': o.ariaLabel"),
        "btn() setzt keinen zugänglichen Namen"
    );

    // Jeder iconBtn-Aufruf muss ein zweites Argument haben.
    for (datei, inhalt) in alle_js() {
        for (nr, zeile) in inhalt.lines().enumerate() {
            if let Some(p) = zeile.find("iconBtn(") {
                let rest = zeile[p + "iconBtn(".len()..].trim_end();
                // Steht auf der Zeile nichts mehr hinter der Klammer, sind die
                // Argumente umbrochen - dann sagt diese Zeile nichts aus.
                let umbrochen = rest.is_empty() || rest.ends_with('(') || rest.ends_with(',');
                assert!(
                    rest.matches(',').count() >= 1 || umbrochen,
                    "{datei}:{} ruft iconBtn ohne Beschriftung auf: {}",
                    nr + 1,
                    zeile.trim()
                );
            }
        }
    }
}

/// Jede Tastaturaktion der Triage muss auch anklickbar sein.
#[test]
fn jede_tastaturaktion_hat_eine_schaltflaeche() {
    let triage = lies("views/triage.js");

    // Aktionen, die im Tastaturzweig behandelt werden.
    for (taste, funktion) in [
        ("h.delete", "loeschen(datei)"),
        ("h.skip", "markieren(datei, 'skipped'"),
        ("h.keep", "markieren(datei, 'kept'"),
        ("h.open", "api.openPath(datei.path)"),
        ("h.explorer", "api.revealPath(datei.path)"),
        ("h.rename", "umbenennen(datei)"),
    ] {
        assert!(
            triage.contains(taste),
            "die Taste für {funktion} wird nicht behandelt"
        );
        // Dieselbe Funktion muss mindestens zweimal vorkommen: einmal am
        // Knopf, einmal in der Tastaturbehandlung.
        assert!(
            triage.matches(funktion).count() >= 2,
            "{funktion} gibt es nur an einer Stelle - Maus und Tastatur müssen \
             dieselbe Aktion erreichen"
        );
    }

    // Und die Zifferntasten haben ihre Knöpfe.
    assert!(triage.contains("verschieben(datei, nummer)"), "Zifferntasten fehlen");
    assert!(triage.contains("verschieben(datei, z.key)"), "die Zielknöpfe fehlen");
}

/// Tastenkürzel stehen an den Aktionen, damit sie niemand suchen muss.
#[test]
fn tastenkuerzel_sind_sichtbar() {
    let triage = lies("views/triage.js");
    assert!(triage.contains("hotkey: taste(h.delete"), "am Papierkorb fehlt die Taste");
    assert!(triage.contains("hotkey: taste(h.skip"), "am Überspringen fehlt die Taste");
    assert!(triage.contains("hotkey: taste(h.open"), "am Öffnen fehlt die Taste");
    assert!(
        triage.contains("el('kbd', { text: z.key"),
        "an den Zielordnern fehlt die Zifferntaste"
    );

    let dom = lies("lib/dom.js");
    assert!(dom.contains("aria-keyshortcuts"), "Kürzel werden nicht angesagt");
}

/// Rückgängig muss in der Triage sichtbar sein, nicht nur als Tastendruck.
#[test]
fn rueckgaengig_ist_sichtbar() {
    let triage = lies("views/triage.js");
    assert!(
        triage.contains("common.undo") && katalog().contains("Rückgängig"),
        "kein sichtbarer Rückgängig-Knopf"
    );
    let app = lies("app.js");
    assert!(app.contains("undoLetzte"), "Rückgängig ist nicht global erreichbar");
}

/// Das Zurücknehmen einer ganzen Sitzung ist erreichbar und zeigt vorher an,
/// was passieren würde.
#[test]
fn sitzungs_undo_ist_erreichbar_und_zeigt_vorher_an() {
    let home = lies("views/home.js");
    assert!(home.contains("planSessionUndo"), "kein Dry Run vor dem Sitzungs-Undo");
    assert!(home.contains("undoSession"), "Sitzungs-Undo fehlt");
    // Der Dry Run muss vor der Ausführung stehen.
    let plan = home.find("planSessionUndo").unwrap();
    let aus = home.find("api.undoSession").unwrap();
    assert!(plan < aus, "das Zurücknehmen läuft vor der Vorschau");
}

// ---------------------------------------------------------------------------
// Zustände
// ---------------------------------------------------------------------------

/// Jede Ansicht braucht einen leeren Zustand, der erklärt statt zu schweigen.
#[test]
fn jede_ansicht_hat_einen_erklaerenden_leeren_zustand() {
    // Der Startbildschirm braucht keinen eigenen leeren Zustand: Ohne Ordner
    // *ist* er der leere Zustand - eine Frage und Antworten zum Anklicken.
    let home = lies("views/home.js");
    assert!(home.contains("div.hero"), "der Startbildschirm hat keinen Einstieg");
    assert!(
        home.contains("home.choose_folder"),
        "der Startbildschirm bietet ohne Ordner keine Handlung an"
    );

    for datei in [
        "views/triage.js",
        "views/files.js",
        "views/duplicates.js",
        "views/rules.js",
        "views/recovery.js",
    ] {
        let inhalt = lies(datei);
        assert!(
            inhalt.contains("div.empty"),
            "{datei} hat keinen leeren Zustand"
        );
        assert!(
            inhalt.contains("empty-mark"),
            "{datei} zeigt im leeren Zustand kein Symbol"
        );
        assert!(
            inhalt.contains("el('h2'"),
            "{datei} hat im leeren Zustand keine Überschrift"
        );
    }
}

/// Fehler erreichen den Nutzer in normaler Sprache, nicht als Rust-Ausgabe.
#[test]
fn fehler_werden_uebersetzt_angezeigt() {
    let api = lies("lib/api.js");
    assert!(api.contains("this.title"), "der verständliche Titel geht verloren");
    assert!(api.contains("this.hint"), "der Handlungshinweis geht verloren");

    let dom = lies("lib/dom.js");
    assert!(dom.contains("export function fehlerDialog"), "kein Fehlerdialog");
    assert!(
        dom.contains("common.details") && katalog().contains("Details"),
        "die technische Fassung ist nicht wegklappbar"
    );

    // Keine Ansicht darf die technische Meldung als Hauptmeldung zeigen.
    for datei in ["views/triage.js", "views/files.js", "views/recovery.js", "views/settings.js"] {
        let inhalt = lies(datei);
        assert!(
            !inhalt.contains("toast(e.message"),
            "{datei} zeigt die technische Fehlermeldung direkt an"
        );
    }
}

// ---------------------------------------------------------------------------
// Regeln
// ---------------------------------------------------------------------------

/// Die Frage, die jeder als erstes stellt, muss auf der Seite beantwortet sein
/// und nicht im Handbuch: Sortiert das jetzt automatisch meine Dateien?
#[test]
fn die_regelseite_sagt_zuerst_dass_sie_nichts_automatisch_tut() {
    let regeln = lies("views/rules.js");
    let texte = katalog();
    assert!(
        texte.contains("Regeln sortieren nichts automatisch."),
        "die Kernbotschaft fehlt"
    );
    assert!(
        texte.contains("bestätigst die Stapelaktion selbst"),
        "es steht nicht da, dass der Nutzer selbst bestätigt"
    );
    assert!(
        texte.contains("Nichts davon läuft im Hintergrund."),
        "der Hintergrundlauf wird nicht ausgeschlossen"
    );
    // Auch in der Rückfallsprache - sonst stünde die Zusage nur auf Deutsch.
    assert!(
        katalog_en().contains("never sort anything automatically"),
        "die Kernbotschaft fehlt auf Englisch"
    );

    // Und die Erklärung steht direkt in der Ansicht, nicht in einem Dialog.
    let vor_dem_ersten_dialog = regeln.split("await dialog(").next().unwrap();
    assert!(
        vor_dem_ersten_dialog.contains("rules.headline"),
        "die Erklärung darf kein Popup sein"
    );
}

/// Die Oberfläche darf nirgends behaupten, eine Regel laufe von selbst.
#[test]
fn keine_oberflaeche_verspricht_automatik() {
    for (name, roh) in alle_js() {
        let inhalt = ohne_kommentare(&roh);
        for verboten in [
            "läuft automatisch",
            "automatisch sortiert",
            "Auto Apply",
            "automatisch verschoben",
            "im Hintergrund sortiert",
        ] {
            assert!(
                !inhalt.contains(verboten),
                "{name} verspricht Automatik: {verboten}"
            );
        }
    }
}

/// Der Weg zur Ausführung ist genau einer: Vorschau, dann Bestätigung. Die
/// Oberfläche darf `applyRule` nirgends ohne vorherigen Dry Run aufrufen.
#[test]
fn eine_regel_wird_nur_nach_der_vorschau_ausgefuehrt() {
    for (name, roh) in alle_js() {
        if name == "lib/api.js" {
            continue;
        }
        let inhalt = ohne_kommentare(&roh);
        for (nr, _) in inhalt.match_indices("applyRule(") {
            let davor = &inhalt[..nr];
            assert!(
                davor.contains("dryRun("),
                "{name} führt eine Regel aus, ohne vorher die Vorschau geholt zu haben"
            );
            // Die bestätigte Anzahl muss mitgehen - sonst kann das Backend
            // nicht prüfen, ob der Nutzer denselben Umfang gesehen hat.
            let danach = &inhalt[nr..(nr + 200).min(inhalt.len())];
            assert!(
                danach.contains("lauf.count"),
                "{name} übergibt die bestätigte Anzahl nicht"
            );
        }
    }
}

/// Die Vorschau muss alle vier Angaben zeigen, die vor einem Stapel zählen.
#[test]
fn die_vorschau_zeigt_umfang_ziel_und_konflikte() {
    let regeln = lies("views/rules.js");
    let vorschau = regeln.split("async function vorschau(").nth(1).expect("vorschau fehlt");
    for (feld, was) in [
        ("lauf.count", "die Anzahl"),
        ("lauf.total_size", "die Gesamtgröße"),
        ("lauf.target", "den Zielordner"),
        ("lauf.conflicts", "die Namenskonflikte"),
        ("lauf.safe", "die konfliktfreien Treffer"),
        ("lauf.truncated", "den Hinweis auf abgeschnittene Ergebnisse"),
    ] {
        assert!(vorschau.contains(feld), "die Vorschau zeigt nicht {was}");
    }
}

/// Ein Vorschlag bleibt passiv: kein Dialog, keine Meldung, kein Fokuswechsel
/// beim Erzeugen. Er wird angezeigt und wartet.
#[test]
fn vorschlaege_bleiben_passiv() {
    let regeln = lies("views/rules.js");
    let inhalt = ohne_kommentare(&regeln);

    // Sie liegen eingeklappt.
    assert!(
        inhalt.contains("details.details") && inhalt.contains("rules.suggestions"),
        "Vorschläge liegen nicht eingeklappt hinter einer Zahl"
    );

    // Zwischen dem Abholen und dem Zeichnen darf nichts aufpoppen.
    let bis_zur_ansicht = inhalt.split("const wurzel = el(").next().unwrap();
    for verboten in ["toast(", "dialog(", "alert("] {
        assert!(
            !bis_zur_ansicht.contains(verboten),
            "das Holen der Vorschläge löst {verboten} aus"
        );
    }

    // Und jeder Vorschlag lässt sich wegklicken.
    assert!(inhalt.contains("dismissPattern("), "ein Namensmuster lässt sich nicht verwerfen");
    assert!(inhalt.contains("dismissSuggestion("), "ein Typvorschlag lässt sich nicht verwerfen");
}

/// Ein Vorschlag muss belegen können, worauf er beruht.
#[test]
fn ein_vorschlag_nennt_seinen_grund() {
    let regeln = lies("views/rules.js");
    assert!(
        regeln.contains("rules.pattern_reason") && regeln.contains("rules.type_reason"),
        "der strukturierte Vorschlagsgrund wird nicht lokalisiert angezeigt"
    );
    assert!(regeln.contains("v.examples"), "die Belege werden nicht angezeigt");
    assert!(
        regeln.contains("rules.examples") && katalog().contains("Zum Beispiel"),
        "die Belege sind nicht als solche benannt"
    );
}

/// Der Editor soll eine Regel als Satz zeigen, nicht als Datenbankmaske.
#[test]
fn der_regeleditor_liest_sich_als_satz() {
    let regeln = lies("views/rules.js");
    assert!(regeln.contains("rules.if"), "dem Editor fehlt das Wenn");
    assert!(regeln.contains("rules.then"), "dem Editor fehlt das Dann");
    // Die Verknüpfung steht zwischen den Bedingungen und ist benannt.
    let texte = katalog();
    assert!(
        texte.contains("UND — alle müssen zutreffen") && texte.contains("ODER — eine reicht"),
        "die Verknüpfung ist nicht in Worten erklärt"
    );
    // Alle Namensvergleiche stehen zur Auswahl.
    for op in ["contains", "starts_with", "ends_with", "eq"] {
        assert!(
            regeln.contains(&format!("'{op}'")),
            "der Vergleich {op} fehlt in der Auswahl"
        );
    }
}

/// Regex ist für normale Nutzer eine Falle. In der Oberfläche gibt es das
/// nicht — weder als Feld noch als Vergleich.
#[test]
fn regex_wird_normalen_nutzern_nicht_angeboten() {
    let regeln = ohne_kommentare(&lies("views/rules.js"));
    for verboten in ["regex", "Regex", "regulärer Ausdruck", "RegExp("] {
        assert!(
            !regeln.contains(verboten),
            "die Regelseite bietet {verboten} an"
        );
    }
}

// ---------------------------------------------------------------------------
// Ordnergrößen
// ---------------------------------------------------------------------------

/// Größen werden nachgetragen, nicht abgewartet. Eine Startseite, die auf eine
/// Zahl wartet, ist kaputt — auch wenn die Zahl stimmt.
#[test]
fn ordnergroessen_blockieren_die_startseite_nicht() {
    let home = lies("views/home.js");
    assert!(home.contains("data-groesse"), "es gibt keinen Platz für die Größe");
    // Erst zeichnen, dann nachtragen: der Aufruf steht unmittelbar vor dem
    // Rueckgabewert, nicht im Aufbau der Ansicht.
    let ohne_leerraum: String = home.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        ohne_leerraum.contains("groessenNachtragen(wurzel);returnwurzel;"),
        "die Größen werden vor dem Zeichnen abgewartet"
    );
    // Kein await auf die Größen im Aufbau der Ansicht.
    assert!(
        !home.contains("await groessenNachtragen"),
        "auf die Größen wird gewartet, statt sie nachzutragen"
    );
    assert!(
        home.contains("home.file_count") && home.contains("bytes(z.total_size)"),
        "Anzahl und Größe stehen nicht zusammen"
    );
    // Unbekannt heißt unbekannt, nicht null.
    assert!(
        home.contains("if (!z?.known) continue;"),
        "ein unbekannter Ordner bekäme eine erfundene Zahl"
    );
}

// ---------------------------------------------------------------------------
// Duplikate
// ---------------------------------------------------------------------------

/// "Dieselbe Datei" ist eine Zusage. Sie darf nur fallen, wenn der Inhalt
/// vollständig verglichen wurde — nicht nach Größe und nicht nach Teil-Hash.
#[test]
fn identisch_wird_erst_nach_vollstaendigem_vergleich_behauptet() {
    let dupe = lies("views/duplicates.js");
    let texte = katalog();
    assert!(
        texte.contains("Verglichen wird bis aufs letzte Byte"),
        "die Seite sagt nicht, wie verglichen wird"
    );
    // Die drei Stufen sind benannt, damit während der Suche klar ist, dass
    // noch nichts feststeht.
    for stufe in ["Stufe 1 von 3", "Stufe 2 von 3", "Stufe 3 von 3"] {
        assert!(texte.contains(stufe), "{stufe} wird nicht angezeigt");
    }
    for schluessel in ["dupe.stage1", "dupe.stage2", "dupe.stage3"] {
        assert!(dupe.contains(schluessel), "{schluessel} wird nicht verwendet");
    }
    // Gruppen kommen ausschließlich aus dem Backend, das nur vollständig
    // geprüfte Gruppen liefert.
    assert!(
        dupe.contains("api.duplicateGroups("),
        "die Gruppen kommen nicht aus der geprüften Quelle"
    );
}

/// Während eines Scans muss erkennbar sein, dass etwas läuft — ohne erfundene
/// Prozentwerte.
#[test]
fn ladezustand_zeigt_echte_zahlen() {
    let app = lies("app.js");
    // Echte Zahlen aus dem Fortschritt, kein erfundener Prozentwert - und
    // seit rc3 auch die Groesse, weil der Scan sie ohnehin mitzaehlt.
    assert!(app.contains("count: p.files"), "der Scan meldet keine Dateizahl");
    assert!(app.contains("bytes(p.bytes"), "der Scan meldet keine Groesse");
    assert!(app.contains("common.cancel"), "ein laufender Scan lässt sich nicht abbrechen");

    let css = lies("styles.css");
    assert!(
        css.contains(".progress.indeterminate"),
        "es fehlt eine Anzeige für unbekannte Dauer"
    );
}

// ---------------------------------------------------------------------------
// Gestaltung
// ---------------------------------------------------------------------------

/// Farben und Abstände kommen aus Tokens.
#[test]
fn gestaltung_kommt_aus_tokens() {
    let css = lies("styles.css");
    for token in [
        "--s-1", "--s-4", "--r-md", "--t-base", "--bg", "--surface", "--border", "--text",
        "--accent", "--ok", "--warn", "--danger", "--focus",
    ] {
        assert!(css.contains(token), "das Token {token} fehlt");
    }

    // Hexcodes gehören in den Token-Block, nicht in einzelne Regeln.
    let token_ende = css.find("/* ====").map(|_| css.find("Grundlagen").unwrap()).unwrap_or(0);
    let rest = &css[token_ende..];
    let hex = rest.matches('#').count();
    assert!(
        hex <= 2,
        "außerhalb der Tokens stehen noch {hex} Hexcodes - Farben gehören in :root"
    );
}

/// Hell und Dunkel müssen beide vollständig sein.
#[test]
fn hell_und_dunkel_sind_beide_definiert() {
    let css = lies("styles.css");
    let hell_start = css.find(":root {").expect("heller Satz fehlt");
    let dunkel_start = css.find(":root[data-theme='dark']").expect("dunkler Satz fehlt");

    let hell = &css[hell_start..dunkel_start];
    let dunkel = &css[dunkel_start..];

    // Jede Farbe des hellen Satzes braucht eine dunkle Entsprechung.
    for zeile in hell.lines() {
        let t = zeile.trim();
        if !t.starts_with("--") {
            continue;
        }
        let name = t.split(':').next().unwrap().trim();
        // Nur Farben und Schatten, keine Maße.
        if name.starts_with("--s-") || name.starts_with("--r-") || name.starts_with("--t-") {
            continue;
        }
        if name.contains("font") || name.contains("sidebar") || name.contains("topbar") {
            continue;
        }
        assert!(
            dunkel.contains(&format!("{name}:")),
            "{name} ist im dunklen Satz nicht definiert"
        );
    }
}

/// Sichtbarer Fokus ist Pflicht, und er darf nicht abgeschaltet sein.
#[test]
fn fokus_bleibt_sichtbar() {
    let css = lies("styles.css");
    assert!(css.contains(":focus-visible"), "kein sichtbarer Fokus definiert");
    assert!(
        !css.contains("outline: none") && !css.contains("outline:none"),
        "irgendwo wird der Fokusrahmen abgeschaltet"
    );
}

/// Auf kleinen Laptopbildschirmen muss alles bedienbar bleiben.
#[test]
fn kleine_bildschirme_sind_beruecksichtigt() {
    let css = lies("styles.css");
    assert!(css.contains("@media (max-width: 1000px)"), "keine schmale Fassung");
    assert!(css.contains("@media (max-height: 780px)"), "keine flache Fassung");
    // Der Körper darf nie waagerecht scrollen.
    assert!(css.contains("overflow: hidden"), "der Körper kann waagerecht scrollen");
}

/// Bewegung lässt sich abschalten.
#[test]
fn reduzierte_bewegung_wird_beachtet() {
    let css = lies("styles.css");
    assert!(
        css.contains("prefers-reduced-motion"),
        "die Systemeinstellung für weniger Bewegung wird ignoriert"
    );
}

// ---------------------------------------------------------------------------
// Ruhe
// ---------------------------------------------------------------------------

/// Keine ungefragten Fenster. Ein Dialog ist nur erlaubt, wenn wirklich etwas
/// zu entscheiden ist.
#[test]
fn keine_ungefragten_dialoge() {
    for (datei, inhalt) in alle_js() {
        // Werbe- und Hinweistexte, die es nicht geben darf.
        for verboten in [
            "Wusstest du",
            "Tipp:",
            "Bewerte",
            "Wir haben bemerkt",
            "Möchtest du eine Regel",
        ] {
            assert!(
                !inhalt.contains(verboten),
                "{datei} enthält einen ungefragten Hinweis: {verboten}"
            );
        }
    }

    // Vorschläge dürfen nie von selbst einen Dialog öffnen.
    let triage = lies("views/triage.js");
    let start = triage.find("async function ladeVorschlag").expect("Vorschlagsfunktion fehlt");
    let ende = triage[start..].find("\n}\n").map(|e| start + e).unwrap_or(triage.len());
    let block = &triage[start..ende];
    assert!(!block.contains("dialog("), "ein Vorschlag öffnet ein Fenster");
    assert!(!block.contains("toast("), "ein Vorschlag meldet sich von selbst");

    // Und die Regelansicht zeigt sie eingeklappt.
    let rules = lies("views/rules.js");
    assert!(
        rules.contains("details.details") && rules.contains("rules.suggestions"),
        "Vorschläge sind nicht eingeklappt"
    );
}

/// Der Papierkorb ist der Normalfall und heißt auch so.
#[test]
fn loeschen_heisst_papierkorb() {
    let triage = lies("views/triage.js");
    assert!(
        triage.contains("triage.trash") && katalog().contains("Papierkorb"),
        "der Knopf heißt nicht Papierkorb"
    );
    assert!(
        katalog().contains("Endgültig löschen?"),
        "endgültiges Löschen wird nicht gesondert bestätigt"
    );

    // Endgültiges Löschen gehört in die erweiterten Einstellungen.
    let settings = lies("views/settings.js");
    let allgemein = settings.find("fn allgemein").or(settings.find("function allgemein")).unwrap();
    let erweitert = settings.find("function erweitert").unwrap();
    let allgemein_block = &settings[allgemein..erweitert];
    assert!(
        !allgemein_block.contains("delete_mode"),
        "das Löschverhalten steht in den allgemeinen Einstellungen"
    );
    assert!(
        settings[erweitert..].contains("delete_mode"),
        "das Löschverhalten fehlt in den erweiterten Einstellungen"
    );
}

// ---------------------------------------------------------------------------
// Windows-Identität
// ---------------------------------------------------------------------------

/// Der Installer muss ein Endnutzerprogramm ergeben, kein Entwicklerartefakt.
#[test]
fn windows_identitaet_ist_gesetzt() {
    let conf = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
    )
    .expect("tauri.conf.json fehlt");

    assert!(conf.contains("\"productName\": \"FileTriage\""), "falscher Produktname");
    assert!(conf.contains("\"publisher\""), "kein Herausgeber");
    assert!(conf.contains("icons/icon.ico"), "kein Programmsymbol");
    assert!(conf.contains("\"installerIcon\""), "der Installer hat kein Symbol");
    assert!(conf.contains("\"startMenuFolder\": \"FileTriage\""), "kein Startmenü-Eintrag");
    assert!(
        conf.contains("\"installMode\": \"currentUser\""),
        "die Installation verlangt Administratorrechte"
    );
    // Keine Entwicklerbegriffe im sichtbaren Namen.
    for verboten in ["tauri-app", "com.tauri", "myapp", "test"] {
        assert!(
            !conf.contains(verboten),
            "die Konfiguration enthält den Entwicklerbegriff {verboten}"
        );
    }

    // Das Fenster startet in einer Größe, die auf einen kleinen Laptop passt.
    assert!(conf.contains("\"width\": 1180"), "unerwartete Startbreite");
    assert!(conf.contains("\"height\": 760"), "unerwartete Starthöhe");
    assert!(conf.contains("\"minWidth\""), "keine Mindestbreite");
}

/// Alle Symbolgrößen, die Windows braucht, müssen im ICO liegen.
#[test]
fn app_symbol_deckt_alle_groessen_ab() {
    let ico = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons").join("icon.ico");
    let daten = std::fs::read(&ico).expect("icon.ico fehlt");
    assert!(daten.len() > 5_000, "das Symbol ist verdächtig klein");

    // ICO-Kopf: Reserviert(0), Typ(1), Anzahl.
    assert_eq!(&daten[0..4], &[0, 0, 1, 0], "keine gültige ICO-Datei");
    let anzahl = u16::from_le_bytes([daten[4], daten[5]]) as usize;
    assert!(anzahl >= 6, "nur {anzahl} Größen im Symbol, erwartet mindestens 6");

    // Die von der Taskleiste und dem Explorer verlangten Größen.
    let mut groessen = Vec::new();
    for i in 0..anzahl {
        let off = 6 + i * 16;
        let b = daten[off];
        groessen.push(if b == 0 { 256u32 } else { b as u32 });
    }
    for verlangt in [16u32, 32, 48, 256] {
        assert!(
            groessen.contains(&verlangt),
            "im Symbol fehlt die Größe {verlangt}, vorhanden: {groessen:?}"
        );
    }
}

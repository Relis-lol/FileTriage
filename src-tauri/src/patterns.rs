//! Wiederkehrende Dateinamensmuster im eigenen Verhalten.
//!
//! Die bestehenden Vorschlaege in [`crate::rules`] arbeiten mit Dateityp und
//! Kategorie: "du hast PDFs schon dreimal hierhin geschoben". Das trifft aber
//! nicht den haeufigsten realen Fall. Wer Rechnungen einsortiert, verschiebt
//! nicht *alle* PDFs nach `Dokumente\Rechnungen`, sondern die, in deren Namen
//! "rechnung" steht.
//!
//! Dieses Modul liest dafuer ausschliesslich das Aktionsjournal - also das,
//! was der Nutzer nachweislich selbst getan hat - und sucht darin Woerter, die
//! wiederholt in denselben Ordner gewandert sind.
//!
//! Drei Dinge tut es ausdruecklich **nicht**:
//!
//! * Es legt keine Regel an. Es schlaegt eine vor.
//! * Es verschiebt nichts. Kein Aufruf aus diesem Modul fasst eine Datei an.
//! * Es meldet sich nicht. Die Oberflaeche holt die Vorschlaege ab, wenn der
//!   Nutzer die Regelseite oeffnet - es gibt kein Fenster und keinen Hinweis.
//!
//! Der schwierige Teil ist nicht das Finden, sondern das Weglassen. Ein
//! Vorschlag `Dateiname enthaelt "2026"` waere technisch korrekt und trotzdem
//! Unsinn. Deshalb steht unten eine Liste belangloser Woerter, und es gibt
//! Mindestschwellen, die eher zu streng als zu locker sind.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::rules::{Condition, Rule};

/// Woerter, die in Dateinamen nichts ueber ihren Inhalt aussagen.
///
/// Sie kommen haeufig vor und wuerden deshalb zuverlaessig jede Schwelle
/// reissen - und dabei Regeln erzeugen, die auf voellig unbeteiligte Dateien
/// passen. Jahreszahlen werden zusaetzlich ueber die Ziffernpruefung
/// aussortiert; hier stehen nur die Woerter.
const BELANGLOS: &[&str] = &[
    "final", "final2", "fertig", "neu", "new", "alt", "old", "kopie", "copy", "dokument",
    "document", "datei", "file", "img", "image", "bild", "foto", "photo", "pic", "scan", "screen",
    "download", "downloads", "unbenannt", "untitled", "temp", "tmp", "test", "version", "entwurf",
    "draft", "backup2", "teil", "part", "seite", "page", "und", "der", "die", "das", "the", "and",
    "for", "von", "mit", "aus", "januar", "februar", "maerz", "april", "juni", "juli", "august",
    "september", "oktober", "november", "dezember",
];

/// Kuerzere Woerter sind zu unspezifisch: "abc" steckt in zu vielem.
const MIN_LAENGE: usize = 4;

/// So viele verschiedene Dateien muessen ein Wort mindestens geteilt haben.
///
/// Bewusst zusaetzlich zur Zaehlschwelle: Dieselbe Datei mehrfach hin und her
/// zu schieben ist kein Muster.
const MIN_VERSCHIEDENE: usize = 3;

/// Ein erkanntes Muster. Nichts daran ist ausgefuehrt oder gespeichert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternSuggestion {
    /// Das wiederkehrende Wort, kleingeschrieben.
    pub token: String,
    /// Zielordner, in den diese Dateien gewandert sind.
    pub target: String,
    /// Wie oft das beobachtet wurde.
    pub count: i64,
    /// Bis zu drei echte Dateinamen als Beleg - damit der Vorschlag nicht
    /// behauptet, sondern zeigt.
    pub examples: Vec<String>,
    /// Gesetzt, wenn alle beobachteten Dateien dieselbe Endung hatten. Dann
    /// bekommt die vorgeschlagene Regel diese Endung als zweite Bedingung -
    /// das ist genauer und trifft nicht versehentlich ein Bild, in dessen
    /// Namen dasselbe Wort steht.
    pub ext: Option<String>,
}

impl PatternSuggestion {
    /// Die Regel, die aus dem Vorschlag wuerde - erst auf Zustimmung.
    pub fn to_rule(&self) -> Rule {
        let mut bedingungen = vec![Condition {
            field: "name".into(),
            op: "contains".into(),
            value: serde_json::json!([self.token]),
        }];
        if let Some(ext) = self.ext.as_ref().filter(|e| !e.is_empty()) {
            bedingungen.push(Condition {
                field: "ext".into(),
                op: "in".into(),
                value: serde_json::json!([ext]),
            });
        }
        let mut regel = Rule {
            id: None,
            name: String::new(),
            enabled: true,
            priority: 100,
            conditions: bedingungen,
            match_mode: "all".into(),
            target: self.target.clone(),
            source: "suggested".into(),
            created_at: 0,
            hits: 0,
        };
        regel.name = crate::rules::default_name(&regel);
        regel
    }
}

/// Zerlegt einen Dateinamen in Woerter.
///
/// Getrennt wird an allem, was kein Buchstabe und keine Ziffer ist. Das
/// funktioniert fuer `Rechnung_Amazon.pdf` genauso wie fuer
/// `Rechnung-Uebersicht 2026.pdf` und laesst koreanische oder japanische
/// Namen unangetastet - `is_alphanumeric` ist Unicode-korrekt.
pub fn tokens(name: &str) -> Vec<String> {
    let stamm = match name.rfind('.') {
        Some(0) | None => name,
        Some(p) => &name[..p],
    };
    stamm
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Taugt dieses Wort als Grundlage einer Regel?
fn brauchbar(token: &str) -> bool {
    if token.chars().count() < MIN_LAENGE {
        return false;
    }
    // Reine Zahlen - Jahre, Nummern, Zeitstempel - sagen nichts ueber den
    // Inhalt. "2026" steckt in allem, was dieses Jahr entstanden ist.
    if token.chars().all(|c| c.is_numeric()) {
        return false;
    }
    if BELANGLOS.contains(&token) {
        return false;
    }
    true
}

struct Beobachtung {
    count: i64,
    namen: Vec<String>,
    endungen: std::collections::HashSet<String>,
}

/// Sucht wiederkehrende Namensmuster im Aktionsjournal.
///
/// `threshold` ist die Anzahl gleicher Beobachtungen, ab der ein Muster
/// ueberhaupt gezeigt wird - dieselbe Schwelle wie bei den Typvorschlaegen,
/// mindestens aber 3.
///
/// Betrachtet werden nur abgeschlossene, nicht zurueckgenommene Verschiebungen.
/// Was der Nutzer rueckgaengig gemacht hat, war offensichtlich nicht das, was
/// er wollte - daraus darf kein Vorschlag entstehen.
pub fn suggestions(conn: &Connection, threshold: i64) -> Result<Vec<PatternSuggestion>> {
    let schwelle = threshold.max(3);

    let mut stmt = conn.prepare(
        "SELECT original_name, ext, dst FROM actions
         WHERE kind = 'move' AND state = 'completed' AND undone = 0 AND dst IS NOT NULL
         ORDER BY ts DESC LIMIT 2000",
    )?;
    let zeilen = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut nach_muster: std::collections::HashMap<(String, String), Beobachtung> =
        std::collections::HashMap::new();

    for (name, ext, dst) in zeilen {
        // Der Zielordner ist der Elternordner der verschobenen Datei.
        let Some(ziel) = std::path::Path::new(&dst).parent() else { continue };
        let ziel = ziel.display().to_string();
        let ext_klein = ext.to_lowercase();

        // Ein Wort zaehlt je Datei einmal, auch wenn es doppelt vorkommt.
        let mut gesehen = std::collections::HashSet::new();
        for token in tokens(&name) {
            if !brauchbar(&token) || token == ext_klein || !gesehen.insert(token.clone()) {
                continue;
            }
            let eintrag = nach_muster
                .entry((token, crate::paths::path_key(&ziel)))
                .or_insert_with(|| Beobachtung {
                    count: 0,
                    namen: Vec::new(),
                    endungen: std::collections::HashSet::new(),
                });
            eintrag.count += 1;
            if !eintrag.namen.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                eintrag.namen.push(name.clone());
            }
            eintrag.endungen.insert(ext_klein.clone());
        }
    }

    // Zielpfade in lesbarer Form nachschlagen: der Schluessel ist
    // kleingeschrieben, angezeigt werden soll der echte Pfad.
    let mut anzeige: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT dst FROM actions WHERE kind = 'move' AND dst IS NOT NULL LIMIT 2000",
        )?;
        let dsts = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for d in dsts {
            if let Some(p) = std::path::Path::new(&d).parent() {
                let echt = p.display().to_string();
                anzeige.entry(crate::paths::path_key(&echt)).or_insert(echt);
            }
        }
    }

    let bestehende = crate::rules::list(conn)?;
    let abgelehnt = abgelehnte(conn)?;

    let mut out: Vec<PatternSuggestion> = nach_muster
        .into_iter()
        .filter(|((_, _), b)| b.count >= schwelle && b.namen.len() >= MIN_VERSCHIEDENE)
        .filter_map(|((token, ziel_key), b)| {
            let target = anzeige.get(&ziel_key).cloned().unwrap_or(ziel_key.clone());
            if abgelehnt.contains(&(token.clone(), ziel_key.clone())) {
                return None;
            }
            // Deckt eine bestehende Regel dasselbe schon ab, waere der
            // Vorschlag nur Laerm.
            let schon_geregelt = bestehende.iter().any(|r| {
                crate::paths::path_key(&r.target) == ziel_key
                    && r.conditions.iter().any(|c| {
                        c.field == "name"
                            && c.value
                                .as_array()
                                .map(|a| {
                                    a.iter().any(|v| {
                                        v.as_str().map(str::to_lowercase).as_deref()
                                            == Some(token.as_str())
                                    })
                                })
                                .unwrap_or(false)
                    })
            });
            if schon_geregelt {
                return None;
            }

            let mut beispiele = b.namen.clone();
            beispiele.truncate(3);
            // Nur wenn wirklich *alle* beobachteten Dateien dieselbe Endung
            // hatten, wird daraus eine zusaetzliche Bedingung. Sonst waere die
            // Regel enger als das beobachtete Verhalten.
            let einheitliche_endung = if b.endungen.len() == 1 {
                b.endungen.iter().next().filter(|e| !e.is_empty()).cloned()
            } else {
                None
            };
            Some(PatternSuggestion {
                token,
                target,
                count: b.count,
                examples: beispiele,
                ext: einheitliche_endung,
            })
        })
        .collect();

    // Das haeufigste Muster zuerst; bei Gleichstand das laengere Wort, weil es
    // spezifischer ist.
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| b.token.chars().count().cmp(&a.token.chars().count()))
            .then_with(|| a.token.cmp(&b.token))
    });
    out.truncate(5);
    Ok(out)
}

/// Merkt sich, dass ein Muster abgelehnt wurde. Es kommt nicht wieder.
pub fn dismiss(conn: &Connection, token: &str, target: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO dismissed_patterns (token, target_key, ts) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            token.to_lowercase(),
            crate::paths::path_key(target),
            crate::db::now_ts()
        ],
    )?;
    Ok(())
}

fn abgelehnte(conn: &Connection) -> Result<std::collections::HashSet<(String, String)>> {
    let mut stmt = conn.prepare("SELECT token, target_key FROM dismissed_patterns")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::TempDir;

    /// Traegt eine abgeschlossene Verschiebung ins Journal ein - so, wie es
    /// `fileops` nach einer echten Aktion tut.
    fn verschoben(conn: &Connection, name: &str, ziel: &str) {
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        conn.execute(
            "INSERT INTO actions (batch_id, ts, kind, src, dst, original_name, size, ext,
                                  undone, undoable, state, session_id)
             VALUES (NULL, ?1, 'move', ?2, ?3, ?4, 1, ?5, 0, 1, 'completed', 's')",
            rusqlite::params![
                crate::db::now_ts(),
                format!("C:\\Downloads\\{name}"),
                format!("{ziel}\\{name}"),
                name,
                ext
            ],
        )
        .unwrap();
    }

    fn frisch(name: &str) -> (TempDir, Db) {
        let dir = TempDir::new(name);
        let db = Db::open(dir.path().join("i.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn woerter_werden_unicode_korrekt_zerlegt() {
        assert_eq!(tokens("Rechnung_Amazon.pdf"), vec!["rechnung", "amazon"]);
        assert_eq!(tokens("Rechnung_\u{dc}bersicht.pdf"), vec!["rechnung", "\u{fc}bersicht"]);
        assert_eq!(tokens("영수증_2026.pdf"), vec!["영수증", "2026"]);
        // Die Endung gehoert nicht zu den Woertern.
        assert!(!tokens("bericht.pdf").contains(&"pdf".to_string()));
    }

    #[test]
    fn wiederkehrendes_namensmuster_wird_erkannt() {
        let (_d, db) = frisch("muster");
        let conn = db.conn().unwrap();
        verschoben(&conn, "Rechnung_Amazon.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "Rechnung_Strom.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "meine_rechnung_august.pdf", "C:\\Docs\\Rechnungen");

        let v = suggestions(&conn, 3).unwrap();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].token, "rechnung");
        assert_eq!(v[0].count, 3);
        assert_eq!(v[0].examples.len(), 3, "der Vorschlag soll seine Belege zeigen");
        assert_eq!(v[0].target, "C:\\Docs\\Rechnungen");
    }

    #[test]
    fn belanglose_woerter_erzeugen_keinen_vorschlag() {
        let (_d, db) = frisch("belanglos");
        let conn = db.conn().unwrap();
        // Gemeinsam haben diese Namen nur "2026" und "final".
        verschoben(&conn, "urlaub_2026_final.jpg", "C:\\Bilder");
        verschoben(&conn, "hausbau_2026_final.png", "C:\\Bilder");
        verschoben(&conn, "portrait_2026_final.jpg", "C:\\Bilder");
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    #[test]
    fn zwei_dateien_reichen_nicht() {
        let (_d, db) = frisch("schwelle");
        let conn = db.conn().unwrap();
        verschoben(&conn, "Rechnung_A.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "Rechnung_B.pdf", "C:\\Docs\\Rechnungen");
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    #[test]
    fn dieselbe_datei_mehrfach_ist_kein_muster() {
        let (_d, db) = frisch("wiederholung");
        let conn = db.conn().unwrap();
        for _ in 0..6 {
            verschoben(&conn, "Rechnung_Amazon.pdf", "C:\\Docs\\Rechnungen");
        }
        assert!(
            suggestions(&conn, 3).unwrap().is_empty(),
            "sechsmal dieselbe Datei sind kein wiederkehrendes Muster"
        );
    }

    #[test]
    fn verschiedene_ziele_werden_nicht_vermischt() {
        let (_d, db) = frisch("ziele");
        let conn = db.conn().unwrap();
        verschoben(&conn, "Rechnung_A.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "Rechnung_B.pdf", "C:\\Archiv");
        verschoben(&conn, "Rechnung_C.pdf", "C:\\Desktop");
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    #[test]
    fn zurueckgenommene_aktionen_zaehlen_nicht() {
        let (_d, db) = frisch("undo");
        let conn = db.conn().unwrap();
        verschoben(&conn, "Rechnung_A.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "Rechnung_B.pdf", "C:\\Docs\\Rechnungen");
        verschoben(&conn, "Rechnung_C.pdf", "C:\\Docs\\Rechnungen");
        conn.execute("UPDATE actions SET undone = 1 WHERE original_name LIKE 'Rechnung_C%'", [])
            .unwrap();
        assert!(
            suggestions(&conn, 3).unwrap().is_empty(),
            "was zurueckgenommen wurde, war nicht gewollt"
        );
    }

    #[test]
    fn abgelehntes_muster_kommt_nicht_wieder() {
        let (_d, db) = frisch("abgelehnt");
        let conn = db.conn().unwrap();
        for n in ["Rechnung_A.pdf", "Rechnung_B.pdf", "Rechnung_C.pdf"] {
            verschoben(&conn, n, "C:\\Docs\\Rechnungen");
        }
        let v = suggestions(&conn, 3).unwrap();
        assert_eq!(v.len(), 1);
        dismiss(&conn, &v[0].token, &v[0].target).unwrap();
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    #[test]
    fn aus_einem_muster_wird_eine_verstaendliche_regel() {
        let (_d, db) = frisch("zurregel");
        let conn = db.conn().unwrap();
        for n in ["Rechnung_A.pdf", "Rechnung_B.pdf", "Rechnung_C.pdf"] {
            verschoben(&conn, n, "C:\\Docs\\Rechnungen");
        }
        let v = suggestions(&conn, 3).unwrap();
        let regel = v[0].to_rule();
        assert_eq!(regel.name, "rechnung nach Rechnungen");
        assert_eq!(regel.source, "suggested");
        // Alle drei Belege waren PDFs - die Endung kommt als zweite Bedingung
        // dazu, damit die Regel nicht auf ein Bild gleichen Namens passt.
        assert_eq!(regel.conditions.len(), 2);
        assert_eq!(regel.conditions[0].field, "name");
        assert_eq!(regel.conditions[0].op, "contains");
        assert_eq!(regel.conditions[1].field, "ext");
        assert_eq!(regel.match_mode, "all");
        regel.validate().unwrap();

        // Und danach schlaegt dasselbe Muster nicht noch einmal an.
        crate::rules::save(&conn, &regel).unwrap();
        assert!(suggestions(&conn, 3).unwrap().is_empty());
    }

    /// Ein Vorschlag ist eine Aussage, keine Handlung. Nichts in diesem Modul
    /// darf eine Datei anfassen - auch nicht ueber Umwege.
    #[test]
    fn ein_vorschlag_fuehrt_nichts_aus() {
        let dir = TempDir::new("passiv");
        let quelle = dir.file("Rechnung_A.pdf", b"x");
        dir.file("Rechnung_B.pdf", b"x");
        dir.file("Rechnung_C.pdf", b"x");
        let ziel = dir.dir("Rechnungen");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        for n in ["Rechnung_A.pdf", "Rechnung_B.pdf", "Rechnung_C.pdf"] {
            verschoben(&conn, n, &ziel.display().to_string());
        }

        let v = suggestions(&conn, 3).unwrap();
        assert_eq!(v.len(), 1);
        // Auch das Umwandeln in eine Regel und das Speichern bewegt nichts.
        crate::rules::save(&conn, &v[0].to_rule()).unwrap();

        assert!(quelle.exists(), "die Quelldatei muss unberuehrt liegen bleiben");
        assert_eq!(
            std::fs::read_dir(&ziel).unwrap().count(),
            0,
            "im Zielordner darf nichts angekommen sein"
        );
    }
}

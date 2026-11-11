//! Leistungsmessungen.
//!
//! Laufen nicht in der normalen Suite mit, weil sie Minuten brauchen und
//! zehntausende Dateien anlegen:
//!
//! ```text
//! cargo test --lib perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Die Zahlen landen in PERFORMANCE.md. Keine Messung rechtfertigt es, eine
//! Sicherheitsgarantie zu lockern - sie zeigen nur, was die Garantien kosten.

#![cfg(test)]

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::config::ScanSettings;
use crate::db::Db;
use crate::scan::{ScanOptions, scan};
use crate::testutil::TempDir;

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

/// Legt `n` Dateien in einer Ordnerstruktur an (100 je Ordner).
fn baue_baum(dir: &TempDir, n: usize) -> std::path::PathBuf {
    let wurzel = dir.dir("feld");
    let start = Instant::now();
    for i in 0..n {
        let unter = wurzel.join(format!("ordner-{:04}", i / 100));
        if i % 100 == 0 {
            std::fs::create_dir_all(&unter).unwrap();
        }
        // Wechselnde Endungen und Groessen, damit Filter und Gruppierung
        // realistisch belastet werden.
        let ext = ["pdf", "jpg", "txt", "zip", "exe", "mp4"][i % 6];
        let groesse = 64 + (i % 400);
        std::fs::write(unter.join(format!("datei-{i}.{ext}")), vec![(i % 251) as u8; groesse])
            .unwrap();
    }
    println!("  Testdaten: {n} Dateien in {} ms angelegt", ms(start));
    wurzel
}

fn scanne(db: &Db, wurzel: &std::path::Path) -> u128 {
    let start = Instant::now();
    let p = scan(
        db,
        &ScanOptions { root: wurzel.to_path_buf(), settings: ScanSettings::default() },
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let dauer = ms(start);
    println!("  Scan: {} Dateien in {dauer} ms ({} Fehler)", p.files, p.errors);
    dauer
}

#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_scan_10k() {
    let dir = TempDir::new("perf-10k");
    let wurzel = baue_baum(&dir, 10_000);
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();

    println!("== 10.000 Dateien ==");
    let erst = scanne(&db, &wurzel);
    let zweit = scanne(&db, &wurzel);
    println!("  Erstscan {erst} ms, inkrementeller Zweitscan {zweit} ms");

    let conn = db.conn().unwrap();
    for (name, q) in abfragen() {
        let start = Instant::now();
        let seite = crate::query::list_files(&conn, &q).unwrap();
        println!("  Abfrage {name}: {} Treffer in {} ms", seite.total, ms(start));
    }
}

#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_scan_100k() {
    let dir = TempDir::new("perf-100k");
    let wurzel = baue_baum(&dir, 100_000);
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();

    println!("== 100.000 Dateien ==");
    let erst = scanne(&db, &wurzel);
    let zweit = scanne(&db, &wurzel);
    println!("  Erstscan {erst} ms, inkrementeller Zweitscan {zweit} ms");

    let conn = db.conn().unwrap();
    for (name, q) in abfragen() {
        let start = Instant::now();
        let seite = crate::query::list_files(&conn, &q).unwrap();
        println!("  Abfrage {name}: {} Treffer in {} ms", seite.total, ms(start));
    }

    let start = Instant::now();
    let verteilung = crate::query::category_breakdown(&conn, &Default::default()).unwrap();
    println!("  Kategorieuebersicht ({} Gruppen): {} ms", verteilung.len(), ms(start));
}

fn abfragen() -> Vec<(&'static str, crate::query::FileQuery)> {
    use crate::query::FileQuery;
    vec![
        ("alle (Seite 1)", FileQuery { limit: 200, ..Default::default() }),
        (
            "Textsuche",
            FileQuery { text: Some("datei-9".into()), limit: 200, ..Default::default() },
        ),
        (
            "Kategorie + Groesse",
            FileQuery {
                categories: vec!["pdf".into()],
                min_size: Some(100),
                limit: 200,
                ..Default::default()
            },
        ),
        (
            "nach Groesse sortiert",
            FileQuery { sort: "size".into(), desc: true, limit: 200, ..Default::default() },
        ),
    ]
}

#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_duplikate() {
    let dir = TempDir::new("perf-dup");
    let wurzel = dir.dir("feld");
    // 6.000 Dateien, davon viele mit gleicher Groesse und ein Drittel echte
    // Duplikate - das ist der ungemuetliche Fall fuer die Stufen 2 und 3.
    for i in 0..6_000 {
        let inhalt = if i % 3 == 0 {
            vec![7u8; 200_000]
        } else {
            let mut v = vec![7u8; 200_000];
            v[i % 200_000] = (i % 251) as u8;
            v
        };
        std::fs::write(wurzel.join(format!("d{i}.bin")), inhalt).unwrap();
    }
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    scanne(&db, &wurzel);

    println!("== Duplikatsuche ueber 6.000 Dateien a 200 KB ==");
    let start = Instant::now();
    let p = crate::dup::find(
        &db,
        Some(&wurzel.display().to_string()),
        1,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    println!("  Gesamt: {} ms, {} Gruppen, {} Fehler", ms(start), p.groups, p.errors);

    let conn = db.conn().unwrap();
    let start = Instant::now();
    let g = crate::dup::groups(&conn, Some(&wurzel.display().to_string()), 1).unwrap();
    println!("  Gruppierung abrufen: {} Gruppen in {} ms", g.len(), ms(start));
}

#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_hashing() {
    let dir = TempDir::new("perf-hash");
    let gross = dir.sized_file("gross.bin", 200 * 1024 * 1024, 0xAB);

    let start = Instant::now();
    let _ = crate::dup::quick_hash(&gross, 200 * 1024 * 1024).unwrap();
    println!("== Hashing 200 MB ==");
    println!("  Teil-Hash (Anfang+Ende): {} ms", ms(start));

    let start = Instant::now();
    let _ = crate::dup::full_hash(&gross).unwrap();
    let dauer = ms(start);
    let mbs = 200_000.0 / dauer.max(1) as f64;
    println!("  Vollstaendiger Hash: {dauer} ms ({mbs:.0} MB/s)");
}

/// Was kostet die Sicherheit? Jede Dateioperation schreibt mehrere
/// Journalzustaende, und `synchronous=FULL` heisst ein fsync je Zustand.
#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_dateioperationen() {
    use crate::fileops::{CollisionPolicy, move_file};

    let dir = TempDir::new("perf-ops");
    let quelle = dir.dir("quelle");
    let ziel = dir.dir("ziel");
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    let conn = db.conn().unwrap();

    let n = 1_000;
    let mut pfade = Vec::with_capacity(n);
    for i in 0..n {
        let p = quelle.join(format!("f{i}.txt"));
        std::fs::write(&p, vec![b'x'; 4096]).unwrap();
        pfade.push(p);
    }

    println!("== {n} Verschiebevorgaenge ==");
    let start = Instant::now();
    for p in &pfade {
        move_file(&conn, p, &ziel, CollisionPolicy::Rename, Some("perf"));
    }
    let dauer = ms(start);
    println!(
        "  Innerhalb eines Volumes: {dauer} ms gesamt, {:.2} ms je Datei",
        dauer as f64 / n as f64
    );

    // Undo desselben Stapels.
    let start = Instant::now();
    let ergebnisse = crate::undo::undo_batch(&conn, "perf").unwrap();
    println!(
        "  Undo von {} Aktionen: {} ms ({} erfolgreich)",
        ergebnisse.len(),
        ms(start),
        ergebnisse.iter().filter(|r| r.ok).count()
    );

    // Derselbe Stapel ueber den Kopierpfad, also mit Verifikation.
    let quelle2 = dir.dir("quelle2");
    let ziel2 = dir.dir("ziel2");
    let mut pfade2 = Vec::with_capacity(200);
    for i in 0..200 {
        let p = quelle2.join(format!("g{i}.txt"));
        std::fs::write(&p, vec![b'y'; 65536]).unwrap();
        pfade2.push(p);
    }
    let _reset = crate::fileops::crashpoint::Reset;
    crate::fileops::crashpoint::force_copy_path(true);
    let start = Instant::now();
    for p in &pfade2 {
        move_file(&conn, p, &ziel2, CollisionPolicy::Rename, Some("perf2"));
    }
    let dauer = ms(start);
    println!(
        "  Ueber Volumegrenzen mit Hash-Verifikation (64 KB je Datei): {dauer} ms gesamt, \
         {:.2} ms je Datei",
        dauer as f64 / 200.0
    );
}

#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_regelabgleich() {
    use crate::rules::{Condition, Rule};

    let dir = TempDir::new("perf-rules");
    let wurzel = baue_baum(&dir, 20_000);
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    scanne(&db, &wurzel);
    let conn = db.conn().unwrap();

    let id = crate::rules::save(
        &conn,
        &Rule {
            id: None,
            name: "Messung".into(),
            enabled: true,
            priority: 100,
            match_mode: "all".into(),
            conditions: vec![
                Condition { field: "ext".into(), op: "in".into(), value: serde_json::json!(["pdf"]) },
                Condition {
                    field: "name".into(),
                    op: "contains".into(),
                    value: serde_json::json!(["datei"]),
                },
                Condition { field: "size".into(), op: "gt".into(), value: serde_json::json!(50) },
            ],
            target: dir.dir("regelziel").display().to_string(),
            source: "user".into(),
            created_at: 0,
            hits: 0,
        },
    )
    .unwrap();

    println!("== Regelabgleich ueber 20.000 Dateien ==");
    let start = Instant::now();
    let lauf = crate::rules::dry_run(&conn, id, None).unwrap();
    println!(
        "  Dry Run: {} Treffer in {} ms (abgeschnitten: {}, Konflikte: {})",
        lauf.count,
        ms(start),
        lauf.truncated,
        lauf.conflicts
    );

    // Die Regelliste zeigt bei jeder Regel eine Trefferzahl. Sie muss so
    // billig sein, dass das Oeffnen der Seite nicht darauf wartet.
    let regel = crate::rules::by_id(&conn, id).unwrap();
    let start = Instant::now();
    let (anzahl, _) = crate::rules::match_count(&conn, &regel, None).unwrap();
    println!("  Nur zaehlen: {anzahl} Treffer in {} ms", ms(start));

    // Reine Namensvergleiche, ohne Datenbank - der heisse Pfad beim Abgleich.
    let datei = crate::model::FileEntry {
        id: 1,
        path: "C:\\Downloads\\Rechnung_Amazon_2026.pdf".into(),
        parent: "C:\\Downloads".into(),
        name: "Rechnung_Amazon_2026.pdf".into(),
        ext: "pdf".into(),
        size: 1000,
        created: 0,
        modified: crate::db::now_ts(),
        category: "pdf".into(),
        state: "pending".into(),
    };
    let now = crate::db::now_ts();
    let start = Instant::now();
    let mut treffer = 0u64;
    for _ in 0..1_000_000 {
        if regel.matches(&datei, now) {
            treffer += 1;
        }
    }
    println!("  1.000.000 Namensvergleiche in {} ms ({treffer} Treffer)", ms(start));
}

/// Wie teuer sind die passiven Namensmuster-Vorschlaege?
///
/// Sie werden beim Oeffnen der Regelseite berechnet. Wenn das spuerbar
/// dauert, gehoert es in den Hintergrund - also nachmessen.
#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_muster_vorschlaege() {
    let dir = TempDir::new("perf-muster");
    let db = Db::open(dir.path().join("i.db")).unwrap();
    let conn = db.conn().unwrap();

    // Ein volles Journal: 2.000 Verschiebungen, wie es nach laengerem
    // Gebrauch aussieht.
    let woerter = ["rechnung", "screenshot", "urlaub", "vertrag", "beleg"];
    let ziele = ["C:\\Docs\\Rechnungen", "C:\\Bilder", "C:\\Archiv"];
    for i in 0..2_000 {
        let name = format!("{}_{i}.pdf", woerter[i % woerter.len()]);
        let ziel = ziele[i % ziele.len()];
        conn.execute(
            "INSERT INTO actions (batch_id, ts, kind, src, dst, original_name, size, ext,
                                  undone, undoable, state, session_id)
             VALUES (NULL, ?1, 'move', ?2, ?3, ?4, 1, 'pdf', 0, 1, 'completed', 's')",
            rusqlite::params![
                crate::db::now_ts(),
                format!("C:\\Downloads\\{name}"),
                format!("{ziel}\\{name}"),
                name
            ],
        )
        .unwrap();
    }

    println!("== Namensmuster aus 2.000 Journaleintraegen ==");
    let start = Instant::now();
    let v = crate::patterns::suggestions(&conn, 3).unwrap();
    println!("  {} Vorschlaege in {} ms", v.len(), ms(start));
}

/// Ordnergroessen fuer die Startseite - reine Indexarbeit.
#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_ordnergroessen() {
    let dir = TempDir::new("perf-groessen");
    let wurzel = baue_baum(&dir, 50_000);
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    scanne(&db, &wurzel);
    let conn = db.conn().unwrap();

    println!("== Ordnergroesse aus dem Index (50.000 Dateien) ==");
    let start = Instant::now();
    let seite = crate::query::list_files(
        &conn,
        &crate::query::FileQuery {
            root: Some(wurzel.display().to_string()),
            limit: 1,
            ..Default::default()
        },
    )
    .unwrap();
    println!(
        "  {} Dateien, {} Bytes in {} ms",
        seite.total,
        seite.total_size,
        ms(start)
    );
}

/// Wie schnell zieht die Ueberwachung bei einem Ereignissturm nach?
#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_watcher_sturm() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = TempDir::new("perf-watch");
    let feld = dir.dir("feld");
    std::fs::write(feld.join("start.txt"), b"x").unwrap();
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    scanne(&db, &feld);

    let geaendert = Arc::new(AtomicUsize::new(0));
    let rescans = Arc::new(AtomicUsize::new(0));
    let g = geaendert.clone();
    let r = rescans.clone();
    let _handle = crate::watcher::start_mit_rescan(
        db.clone(),
        feld.clone(),
        true,
        move |n| {
            g.fetch_add(n, Ordering::Relaxed);
        },
        move |_| {
            r.fetch_add(1, Ordering::Relaxed);
        },
    )
    .unwrap();

    println!("== Ereignissturm: 8.000 Dateien anlegen ==");
    let start = Instant::now();
    for i in 0..8_000 {
        std::fs::write(feld.join(format!("sturm-{i}.txt")), b"x").unwrap();
    }
    println!("  Anlegen: {} ms", ms(start));

    // Der Ueberwachung Zeit geben, den Rueckstand abzuarbeiten.
    let start = Instant::now();
    std::thread::sleep(std::time::Duration::from_secs(12));
    let conn = db.conn().unwrap();
    let im_index: i64 = conn.query_row("SELECT count(*) FROM files", [], |x| x.get(0)).unwrap();
    println!(
        "  Nach {} ms: {} Eintraege im Index, {} nachgezogen, {} Rescan-Anforderungen",
        ms(start),
        im_index,
        geaendert.load(Ordering::Relaxed),
        rescans.load(Ordering::Relaxed)
    );
}

/// Speicherverhalten bei wiederholten Durchlaeufen - sucht nach Wachstum,
/// das nicht wieder freigegeben wird.
#[test]
#[ignore = "Leistungsmessung - dauert Minuten"]
fn messung_wiederholte_scans() {
    let dir = TempDir::new("perf-wiederholt");
    let wurzel = baue_baum(&dir, 5_000);
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();

    println!("== 20 aufeinanderfolgende Scans ==");
    let mut dauern = Vec::new();
    for i in 0..20 {
        let start = Instant::now();
        scan(
            &db,
            &ScanOptions { root: wurzel.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        dauern.push(ms(start));
        if i % 5 == 0 {
            let groesse = std::fs::metadata(db.path()).map(|m| m.len()).unwrap_or(0);
            println!("  Durchlauf {i}: {} ms, Indexdatei {} KB", dauern[i], groesse / 1024);
        }
    }
    let erste = dauern[0];
    let letzte = *dauern.last().unwrap();
    println!("  Erster Durchlauf {erste} ms, letzter {letzte} ms");
    assert!(
        letzte < erste.max(1) * 4,
        "die Dauer waechst ueber die Durchlaeufe stark an: {erste} -> {letzte}"
    );
}

//! Fuzzing der Eingabepfade.
//!
//! Alles, was aus einer Datei, einer Datenbank oder einem Import kommen kann,
//! wird hier mit boesartigen Werten beschossen. Die Messlatte ist niedrig
//! angesetzt und trotzdem hart:
//!
//! - **Kein Absturz.** Keine Panik, kein Ueberlauf, keine Endlosschleife.
//! - **Kein Ausbruch.** Keine Operation ausserhalb erlaubter Pfade.
//! - **Kein stiller Schaden.** Ungueltige Eingaben werden abgelehnt, nicht
//!   "so gut es geht" ausgefuehrt.

#![cfg(test)]

use crate::config::{Config, DeleteMode};
use crate::db::{self, Db};
use crate::paths;
use crate::rules::{Condition, Rule};
use crate::testutil::TempDir;

/// Wiederholbarer Zufall ohne Abhaengigkeit.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[(self.next() % v.len() as u64) as usize]
    }
}

/// Bausteine, aus denen boesartige Zeichenketten entstehen.
const GIFT: [&str; 28] = [
    "..",
    "../..",
    "..\\..\\..\\Windows\\System32",
    "C:\\Windows\\System32",
    "\\\\?\\C:\\Windows",
    "\\\\server\\share",
    "%SYSTEMROOT%",
    "$(rm -rf /)",
    "; calc.exe",
    "& calc.exe",
    "| calc.exe",
    "`calc.exe`",
    "\"; DROP TABLE files; --",
    "' OR 1=1 --",
    "<script>alert(1)</script>",
    "\0",
    "\r\n",
    "\u{202e}txt.exe",
    "con",
    "NUL",
    "COM1",
    "....",
    "   ",
    "\t",
    "ünïcödé",
    "🚀🚀🚀",
    "日本語",
    "",
];

fn giftige_zeichenkette(rng: &mut Rng) -> String {
    let teile = (rng.next() % 4) + 1;
    let mut s = String::new();
    for _ in 0..teile {
        s.push_str(rng.pick(&GIFT));
        if rng.next() % 3 == 0 {
            s.push('\\');
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Pfade und Namen
// ---------------------------------------------------------------------------

/// Kein giftiger Pfad darf als Zielordner durchgehen, ohne absolut und frei
/// von `..` zu sein.
#[test]
fn zielordner_pruefung_haelt_jeder_eingabe_stand() {
    let mut rng = Rng::new(0xC0FFEE);
    for _ in 0..20_000 {
        let s = giftige_zeichenkette(&mut rng);
        let p = std::path::Path::new(&s);
        match paths::validate_directory_path(p) {
            Ok(()) => {
                // Was durchgeht, muss beide Bedingungen erfuellen.
                assert!(p.is_absolute(), "nicht absolut durchgelassen: {s:?}");
                assert!(
                    !paths::display_path(p).split(['\\', '/']).any(|t| t == ".."),
                    ".. durchgelassen: {s:?}"
                );
            }
            Err(_) => {}
        }
    }
}

#[test]
fn dateinamenspruefung_haelt_jeder_eingabe_stand() {
    let mut rng = Rng::new(42);
    for _ in 0..20_000 {
        let s = giftige_zeichenkette(&mut rng);
        if let Ok(name) = paths::validate_file_name(&s) {
            assert!(!name.is_empty());
            assert!(!name.contains(['\\', '/', ':', '<', '>', '"', '|', '?', '*']));
            assert!(!name.ends_with('.'));
            assert!(!name.chars().any(|c| (c as u32) < 0x20));
            assert_ne!(name, ".");
            assert_ne!(name, "..");
        }
    }
}

/// `path_key` und `display_path` duerfen bei keiner Eingabe in Panik geraten
/// und muessen in sich stimmig bleiben.
#[test]
fn pfadnormalisierung_ist_stabil() {
    let mut rng = Rng::new(7);
    for _ in 0..20_000 {
        let s = giftige_zeichenkette(&mut rng);
        let key = paths::path_key(&s);
        // Zweimal anwenden aendert nichts mehr.
        assert_eq!(paths::path_key(&key), key, "nicht idempotent: {s:?}");
        assert!(!key.contains('/'), "Trennzeichen nicht vereinheitlicht: {s:?}");
        let _ = paths::display_path(std::path::Path::new(&s));
        let _ = paths::extended(std::path::Path::new(&s));
    }
}

// ---------------------------------------------------------------------------
// Konfiguration
// ---------------------------------------------------------------------------

/// Beliebiger Muell im Konfigurationsfeld darf den Start nicht verhindern -
/// und niemals gefaehrliche Einstellungen aktivieren.
#[test]
fn beschaedigte_konfiguration_faellt_immer_sicher_zurueck() {
    let dir = TempDir::new("fuzz-config");
    let db = Db::open(dir.path().join("i.db")).unwrap();
    let conn = db.conn().unwrap();

    let proben = [
        "",
        "null",
        "[]",
        "{}",
        "{\"delete_mode\":\"permanent\"}",
        "{\"delete_mode\":\"permanent\",\"permanent_delete_confirmed_at\":null}",
        "{\"delete_mode\":42}",
        "{\"targets\":[{\"key\":\"1\",\"label\":\"x\",\"path\":\"..\\\\..\\\\Windows\"}]}",
        "{\"targets\":\"kein array\"}",
        "{\"suggestion_threshold\":-5}",
        "{\"preview_max_bytes\":18446744073709551615}",
        "{\"large_file_mb\":0}",
        "{\"hotkeys\":{\"delete\":null}}",
        "{\"telemetry_opt_in\":true}",
        "{kaputt",
        "\u{0}\u{1}\u{2}",
        "{\"theme\":\"<script>alert(1)</script>\"}",
    ];

    for probe in proben {
        db::set_setting(&conn, "config", probe).unwrap();
        let cfg = Config::load(&conn).expect("Laden darf nie fehlschlagen");
        assert_eq!(
            cfg.effective_delete_mode(),
            DeleteMode::Recycle,
            "beschaedigte Konfiguration darf endgueltiges Loeschen nicht aktivieren: {probe}"
        );
        assert!(cfg.suggestion_threshold >= 1 || cfg.validate().is_err());
    }
}

/// Zielordner aus einer fremden Konfiguration duerfen nie ungeprueft
/// verwendet werden.
#[test]
fn zielordner_aus_der_konfiguration_werden_geprueft() {
    let mut rng = Rng::new(99);
    for _ in 0..2_000 {
        let mut cfg = Config::default();
        cfg.targets.push(crate::config::TargetFolder {
            key: "1".into(),
            label: "Fremd".into(),
            path: giftige_zeichenkette(&mut rng),
        });
        if cfg.validate().is_ok() {
            let p = std::path::Path::new(&cfg.targets[0].path);
            assert!(p.is_absolute());
            assert!(paths::validate_directory_path(p).is_ok());
        }
    }
}

// ---------------------------------------------------------------------------
// Regeln
// ---------------------------------------------------------------------------

/// Regeln koennen theoretisch aus einer fremden Quelle stammen. Keine Regel
/// darf mit einem Ziel ausserhalb der erlaubten Form gespeichert werden.
#[test]
fn regeln_mit_giftigem_ziel_werden_abgelehnt() {
    let mut rng = Rng::new(1234);
    for _ in 0..5_000 {
        let regel = Rule {
            id: None,
            name: giftige_zeichenkette(&mut rng),
            enabled: true,
            priority: 100,
            match_mode: "all".into(),
            conditions: vec![Condition {
                field: giftige_zeichenkette(&mut rng),
                op: giftige_zeichenkette(&mut rng),
                value: serde_json::json!([giftige_zeichenkette(&mut rng)]),
            }],
            target: giftige_zeichenkette(&mut rng),
            source: "user".into(),
            created_at: 0,
            hits: 0,
        };
        if regel.validate().is_ok() {
            assert!(std::path::Path::new(&regel.target).is_absolute());
            assert!(paths::validate_directory_path(std::path::Path::new(&regel.target)).is_ok());
            assert!(!regel.name.trim().is_empty());
        }
    }
}

/// Der Regelabgleich darf bei keiner Kombination aus Feld, Operator und Wert
/// in Panik geraten - auch nicht bei Zahlen an Textfeldern und umgekehrt.
#[test]
fn regelabgleich_geraet_nie_in_panik() {
    let mut rng = Rng::new(555);
    let felder = ["ext", "name", "path", "category", "size", "age_days", "quatsch", ""];
    let operatoren = ["in", "eq", "contains", "starts_with", "ends_with", "gt", "lt", "??", ""];

    let datei = crate::model::FileEntry {
        id: 1,
        path: "C:\\Downloads\\Ünïcödé 🚀.pdf".into(),
        parent: "C:\\Downloads".into(),
        name: "Ünïcödé 🚀.pdf".into(),
        ext: "pdf".into(),
        size: i64::MAX,
        created: i64::MIN,
        modified: i64::MAX,
        category: "pdf".into(),
        state: "pending".into(),
    };

    for _ in 0..20_000 {
        let wert = match rng.next() % 6 {
            0 => serde_json::json!(giftige_zeichenkette(&mut rng)),
            1 => serde_json::json!([giftige_zeichenkette(&mut rng)]),
            2 => serde_json::json!(i64::MAX),
            3 => serde_json::json!(i64::MIN),
            4 => serde_json::Value::Null,
            _ => serde_json::json!({ "verschachtelt": true }),
        };
        let regel = Rule {
            id: None,
            name: "fuzz".into(),
            enabled: true,
            priority: 0,
            match_mode: "all".into(),
            conditions: vec![Condition {
                field: rng.pick(&felder).to_string(),
                op: rng.pick(&operatoren).to_string(),
                value: wert,
            }],
            target: "C:\\Ziel".into(),
            source: "user".into(),
            created_at: 0,
            hits: 0,
        };
        // Darf beliebig antworten - nur nicht abstuerzen oder haengen.
        let _ = regel.matches(&datei, i64::MAX);
        let _ = regel.matches(&datei, 0);
        let _ = regel.matches(&datei, i64::MIN);
    }
}

// ---------------------------------------------------------------------------
// Abfragen
// ---------------------------------------------------------------------------

/// Suchtext und Sortierfelder kommen direkt aus der Oberflaeche. Keine
/// Eingabe darf die Abfrage veraendern oder einen Fehler ausloesen.
#[test]
fn suchabfragen_bleiben_bei_jeder_eingabe_gueltig() {
    use crate::query::{FileQuery, list_files};

    let dir = TempDir::new("fuzz-query");
    let feld = dir.dir("feld");
    std::fs::write(feld.join("a.pdf"), b"x").unwrap();
    std::fs::write(feld.join("b.txt"), b"y").unwrap();
    // Der Index liegt ausserhalb des gescannten Ordners.
    let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
    crate::scan::scan(
        &db,
        &crate::scan::ScanOptions { root: feld.clone(), settings: Default::default() },
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let conn = db.conn().unwrap();

    let mut rng = Rng::new(31337);
    for _ in 0..3_000 {
        let q = FileQuery {
            root: if rng.next() % 2 == 0 { Some(giftige_zeichenkette(&mut rng)) } else { None },
            text: Some(giftige_zeichenkette(&mut rng)),
            categories: vec![giftige_zeichenkette(&mut rng)],
            exts: vec![giftige_zeichenkette(&mut rng)],
            states: vec![giftige_zeichenkette(&mut rng)],
            min_size: Some(i64::MIN),
            max_size: Some(i64::MAX),
            modified_before: Some(i64::MAX),
            modified_after: Some(i64::MIN),
            sort: giftige_zeichenkette(&mut rng),
            desc: rng.next() % 2 == 0,
            limit: (rng.next() % 100_000) as i64 - 50_000,
            offset: (rng.next() % 100_000) as i64 - 50_000,
        };
        let ergebnis = list_files(&conn, &q);
        assert!(ergebnis.is_ok(), "Abfrage fehlgeschlagen: {:?}", ergebnis.err());
    }

    // Die Tabelle steht danach noch.
    let n: i64 = conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2, "die Daten muessen alle Abfragen ueberleben");
}

// ---------------------------------------------------------------------------
// Klassifizierung
// ---------------------------------------------------------------------------

#[test]
fn dateityperkennung_geraet_nie_in_panik() {
    let mut rng = Rng::new(2024);
    for _ in 0..50_000 {
        let name = giftige_zeichenkette(&mut rng);
        let _ = crate::classify::categorize(&name);
        let ext = crate::classify::extension_of(&name);
        assert!(!ext.contains('.'), "Endung enthaelt einen Punkt: {name:?} -> {ext:?}");
        assert_eq!(ext, ext.to_lowercase());
    }
}

/// Die Namensfindung fuer Kollisionen muss auch bei absurden Namen einen
/// gueltigen, freien Namen liefern - oder sauber aufgeben.
#[test]
fn freie_namensfindung_bleibt_stabil() {
    let dir = TempDir::new("fuzz-unique");
    let mut rng = Rng::new(808);
    for _ in 0..300 {
        let name = giftige_zeichenkette(&mut rng);
        let sauber = match paths::validate_file_name(&name) {
            Ok(n) => n,
            Err(_) => continue,
        };
        match crate::fileops::unique_target(dir.path(), &sauber) {
            Ok(p) => {
                assert!(!p.exists(), "vorgeschlagener Name ist belegt: {p:?}");
                assert_eq!(p.parent(), Some(dir.path()), "Vorschlag verlaesst den Ordner");
            }
            Err(_) => {}
        }
    }
}

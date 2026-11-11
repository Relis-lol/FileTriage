//! Ordnerscan und inkrementelle Indexpflege.
//!
//! Der Scan laeuft immer in einem Hintergrund-Thread und meldet Fortschritt
//! ueber einen Callback. Er ist inkrementell: bestehende Zeilen werden
//! aktualisiert, verschwundene Dateien am Ende entfernt. Der Triage-Zustand
//! einer Datei bleibt dabei erhalten.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use jwalk::WalkDirGeneric;
use rusqlite::Connection;

use crate::classify;
use crate::config::ScanSettings;
use crate::db::{self, Db};
use crate::error::{AppError, Result};
use crate::model::ScanProgress;
use crate::paths;

/// Wie viele Dateien pro Transaktion geschrieben werden.
const BATCH: usize = 1_000;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    pub settings: ScanSettings,
}

struct PendingFile {
    path: String,
    parent: String,
    name: String,
    ext: String,
    size: i64,
    created: i64,
    modified: i64,
    category: &'static str,
}

/// Legt den Wurzelordner an oder aktualisiert ihn und liefert seine ID.
pub fn upsert_root(conn: &Connection, root: &Path, settings: &ScanSettings) -> Result<i64> {
    let display = paths::display_path(root);
    let key = paths::path_key(&display);
    conn.execute(
        "INSERT INTO roots(path, path_key, recursive, include_hidden, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path_key) DO UPDATE SET
            path = excluded.path,
            recursive = excluded.recursive,
            include_hidden = excluded.include_hidden",
        rusqlite::params![
            display,
            key,
            settings.recursive as i32,
            settings.include_hidden as i32,
            db::now_ts()
        ],
    )?;
    let id: i64 = conn.query_row("SELECT id FROM roots WHERE path_key = ?1", [&key], |r| r.get(0))?;
    Ok(id)
}

pub fn root_id_for(conn: &Connection, root: &str) -> Result<Option<i64>> {
    let key = paths::path_key(root);
    let mut stmt = conn.prepare("SELECT id FROM roots WHERE path_key = ?1")?;
    let mut rows = stmt.query([&key])?;
    match rows.next()? {
        Some(r) => Ok(Some(r.get(0)?)),
        None => Ok(None),
    }
}

/// Zeitstempel in Millisekunden - nur als Scan-Markierung, nicht fuer Anzeige.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(1)
}

fn unix_secs(t: std::io::Result<SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn is_hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Fuehrt einen vollstaendigen Scan durch. Bricht bei gesetztem `cancel` ab und
/// laesst den Index dabei in einem gueltigen Zustand zurueck.
pub fn scan(
    db: &Db,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&ScanProgress),
) -> Result<ScanProgress> {
    let started = Instant::now();
    let root = opts.root.clone();
    if !root.exists() {
        return Err(AppError::NotFound(format!("Ordner {}", root.display())));
    }
    if !root.is_dir() {
        return Err(AppError::Invalid(format!("{} ist kein Ordner", root.display())));
    }

    let mut conn = db.conn()?;
    let root_id = upsert_root(&conn, &root, &opts.settings)?;
    // Markierung dieses Durchlaufs: alles mit kleinerem Wert ist danach veraltet.
    // Millisekunden reichen allein nicht - zwei schnelle Scans muessen sich
    // sicher unterscheiden, sonst bleiben geloeschte Dateien im Index stehen.
    let scan_marker = {
        let vorher: i64 = conn.query_row(
            "SELECT coalesce(max(seen_at), 0) FROM files WHERE root_id = ?1",
            [root_id],
            |r| r.get(0),
        )?;
        now_ms().max(vorher + 1)
    };

    let mut progress = ScanProgress {
        root: paths::display_path(&root),
        ..Default::default()
    };

    let skip_dirs: Vec<String> = opts.settings.skip_dirs.iter().map(|s| s.to_lowercase()).collect();
    let include_hidden = opts.settings.include_hidden;
    let max_depth = if opts.settings.recursive { usize::MAX } else { 1 };

    let walker = WalkDirGeneric::<((), ())>::new(paths::extended(&root))
        .skip_hidden(false)
        .follow_links(opts.settings.follow_symlinks)
        .max_depth(max_depth)
        .parallelism(jwalk::Parallelism::RayonDefaultPool {
            busy_timeout: std::time::Duration::from_secs(5),
        })
        .process_read_dir(move |_depth, _path, _state, children| {
            children.retain(|entry| match entry {
                Ok(e) => {
                    if !e.file_type().is_dir() {
                        return true;
                    }
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    !skip_dirs.contains(&name)
                }
                // Fehlerhafte Eintraege behalten, damit sie gezaehlt werden.
                Err(_) => true,
            });
        });

    let mut batch: Vec<PendingFile> = Vec::with_capacity(BATCH);
    let mut last_emit = Instant::now();
    let mut cancelled = false;

    for entry in walker {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                progress.errors += 1;
                tracing::debug!("Scan-Eintrag übersprungen: {e}");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path();
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) => {
                // Datei wurde waehrend des Scans geloescht oder ist gesperrt.
                progress.errors += 1;
                tracing::debug!("Metadaten nicht lesbar ({}): {e}", full.display());
                continue;
            }
        };
        if !include_hidden && is_hidden(&meta) {
            continue;
        }
        let size = meta.len();
        if size < opts.settings.min_size_bytes {
            continue;
        }

        let display = paths::display_path(&full);
        let name = entry.file_name().to_string_lossy().to_string();
        let parent = full
            .parent()
            .map(paths::display_path)
            .unwrap_or_else(|| display.clone());

        batch.push(PendingFile {
            ext: classify::extension_of(&name),
            category: classify::categorize(&name).as_str(),
            name,
            parent,
            path: display.clone(),
            size: size as i64,
            created: unix_secs(meta.created()),
            modified: unix_secs(meta.modified()),
        });

        progress.files += 1;
        progress.bytes += size;
        progress.current = display;

        if batch.len() >= BATCH {
            flush(&mut conn, root_id, scan_marker, &mut batch)?;
        }
        if last_emit.elapsed().as_millis() >= 150 {
            progress.elapsed_ms = started.elapsed().as_millis() as u64;
            on_progress(&progress);
            last_emit = Instant::now();
        }
    }

    flush(&mut conn, root_id, scan_marker, &mut batch)?;

    if !cancelled {
        // Erst nach einem vollstaendigen Durchlauf duerfen Zeilen verschwinden,
        // sonst wuerde ein Abbruch den Index leerraeumen.
        progress.removed = conn.execute(
            "DELETE FROM files WHERE root_id = ?1 AND seen_at < ?2",
            rusqlite::params![root_id, scan_marker],
        )? as u64;
        conn.execute(
            "UPDATE roots SET last_scan = ?1 WHERE id = ?2",
            rusqlite::params![db::now_ts(), root_id],
        )?;
    }

    progress.done = true;
    progress.cancelled = cancelled;
    progress.current = String::new();
    progress.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&progress);
    Ok(progress)
}

/// Pflegt einen einzelnen Pfad ein - fuer die Ueberwachung im laufenden Betrieb.
/// Liefert `true`, wenn sich am Index etwas geaendert hat.
pub fn index_path(conn: &mut Connection, root_id: i64, path: &Path) -> Result<bool> {
    let key = paths::path_key(&paths::display_path(path));
    let meta = match std::fs::symlink_metadata(paths::extended(path)) {
        Ok(m) => m,
        Err(_) => {
            // Weg oder unlesbar: aus dem Index nehmen.
            let n = conn.execute("DELETE FROM files WHERE path_key = ?1", [&key])?;
            return Ok(n > 0);
        }
    };
    if !meta.is_file() {
        return Ok(false);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut batch = vec![PendingFile {
        ext: classify::extension_of(&name),
        category: classify::categorize(&name).as_str(),
        parent: path.parent().map(paths::display_path).unwrap_or_default(),
        path: paths::display_path(path),
        size: meta.len() as i64,
        created: unix_secs(meta.created()),
        modified: unix_secs(meta.modified()),
        name,
    }];
    flush(conn, root_id, now_ms(), &mut batch)?;
    Ok(true)
}

fn flush(conn: &mut Connection, root_id: i64, marker: i64, batch: &mut Vec<PendingFile>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files
               (root_id, path, path_key, parent, name, name_lower, ext, size,
                created, modified, category, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path_key) DO UPDATE SET
                root_id  = excluded.root_id,
                path     = excluded.path,
                parent   = excluded.parent,
                name     = excluded.name,
                name_lower = excluded.name_lower,
                ext      = excluded.ext,
                size     = excluded.size,
                created  = excluded.created,
                modified = excluded.modified,
                category = excluded.category,
                seen_at  = excluded.seen_at,
                -- Hashes nur behalten, solange die Datei unveraendert ist.
                quick_hash = CASE WHEN files.size = excluded.size AND files.modified = excluded.modified
                                  THEN files.quick_hash ELSE NULL END,
                full_hash  = CASE WHEN files.size = excluded.size AND files.modified = excluded.modified
                                  THEN files.full_hash ELSE NULL END,
                -- Eine Datei, die wieder da ist, ist erneut zu entscheiden.
                state    = CASE WHEN files.state IN ('moved','deleted') THEN 'pending' ELSE files.state END",
        )?;
        for f in batch.iter() {
            stmt.execute(rusqlite::params![
                root_id,
                f.path,
                paths::path_key(&f.path),
                f.parent,
                f.name,
                f.name.to_lowercase(),
                f.ext,
                f.size,
                f.created,
                f.modified,
                f.category,
                marker,
            ])?;
        }
    }
    tx.commit()?;
    batch.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// Ein eingelesener Unterordner macht seinen Elternordner nicht bekannt.
    ///
    /// Der Index kennt Dateien *unterhalb* eines Ordners, ohne den Ordner je
    /// gesehen zu haben. Wer daraus eine Ordnergroesse ableitet, zeigt am
    /// Desktop die Zahlen eines einzigen Unterordners an - "35 Dateien" fuer
    /// einen Ordner mit 500. Eine falsche Zahl ist schlechter als keine.
    #[test]
    fn ein_eingelesener_unterordner_macht_den_elternordner_nicht_bekannt() {
        let dir = TempDir::new("wurzelkennung");
        let unten = dir.dir("eltern\\kind");
        std::fs::write(unten.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("eltern").join("b.txt"), b"y").unwrap();

        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        scan(
            &db,
            &ScanOptions { root: unten.clone(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let conn = db.conn().unwrap();
        let eltern = dir.path().join("eltern").display().to_string();
        assert!(
            root_id_for(&conn, &unten.display().to_string()).unwrap().is_some(),
            "der eingelesene Ordner muss bekannt sein"
        );
        assert!(
            root_id_for(&conn, &eltern).unwrap().is_none(),
            "der Elternordner wurde nie eingelesen und darf nicht als bekannt gelten"
        );

        // Die Abfrage nach Pfad wuerde die Datei des Kindordners mitzaehlen -
        // genau deshalb entscheidet die Wurzelkennung, nicht die Trefferzahl.
        let ueber_pfad = crate::query::list_files(
            &conn,
            &crate::query::FileQuery {
                root: Some(eltern),
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(ueber_pfad.total, 1, "b.txt wurde nie eingelesen, a.txt schon");
    }

    fn run(dir: &TempDir, db: &Db, settings: ScanSettings) -> ScanProgress {
        let cancel = AtomicBool::new(false);
        let opts = ScanOptions { root: dir.path().to_path_buf(), settings };
        scan(db, &opts, &cancel, &mut |_| {}).unwrap()
    }

    fn open_db(dir: &TempDir) -> Db {
        Db::open(dir.path().join(".index.db")).unwrap()
    }

    #[test]
    fn findet_dateien_rekursiv_und_klassifiziert() {
        let dir = TempDir::new("scan");
        dir.file("a.pdf", b"pdf");
        dir.file("unter/b.png", b"png");
        let db = open_db(&dir);

        let p = run(&dir, &db, ScanSettings::default());
        // Die Indexdatei selbst liegt mit im Ordner und wird mitgezaehlt.
        assert!(p.files >= 2, "gefunden: {}", p.files);

        let conn = db.conn().unwrap();
        let kat: String = conn
            .query_row("SELECT category FROM files WHERE name = 'a.pdf'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kat, "pdf");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM files WHERE name = 'b.png'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ohne_rekursion_nur_erste_ebene() {
        let dir = TempDir::new("scan-flat");
        dir.file("oben.txt", b"x");
        dir.file("unter/tief.txt", b"x");
        let db = open_db(&dir);

        run(&dir, &db, ScanSettings { recursive: false, ..Default::default() });
        let conn = db.conn().unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM files WHERE name = 'tief.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn zweiter_scan_entfernt_geloeschte_dateien() {
        let dir = TempDir::new("scan-inc");
        let weg = dir.file("weg.txt", b"x");
        dir.file("bleibt.txt", b"x");
        let db = open_db(&dir);
        run(&dir, &db, ScanSettings::default());

        std::fs::remove_file(&weg).unwrap();
        let p = run(&dir, &db, ScanSettings::default());
        assert!(p.removed >= 1);

        let conn = db.conn().unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM files WHERE name = 'weg.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn triage_zustand_ueberlebt_erneuten_scan() {
        let dir = TempDir::new("scan-state");
        dir.file("merk.txt", b"x");
        let db = open_db(&dir);
        run(&dir, &db, ScanSettings::default());

        let conn = db.conn().unwrap();
        conn.execute("UPDATE files SET state = 'kept' WHERE name = 'merk.txt'", []).unwrap();
        drop(conn);

        run(&dir, &db, ScanSettings::default());
        let conn = db.conn().unwrap();
        let s: String = conn
            .query_row("SELECT state FROM files WHERE name = 'merk.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(s, "kept");
    }

    #[test]
    fn unicode_und_lange_pfade() {
        let dir = TempDir::new("scan-uni");
        dir.file("Ünïcödé — Grüße 🚀.txt", b"x");
        let tief = format!("{}/{}/{}", "a".repeat(80), "b".repeat(80), "c".repeat(80));
        dir.file(&format!("{tief}/sehr-tief.txt"), b"x");
        let db = open_db(&dir);

        let p = run(&dir, &db, ScanSettings::default());
        assert_eq!(p.errors, 0, "lange oder Unicode-Pfade duerfen keine Fehler erzeugen");
        let conn = db.conn().unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM files WHERE name = 'sehr-tief.txt'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fehlender_ordner_liefert_fehler_statt_panik() {
        let dir = TempDir::new("scan-missing");
        let db = open_db(&dir);
        let cancel = AtomicBool::new(false);
        let opts = ScanOptions {
            root: dir.path().join("gibt-es-nicht"),
            settings: ScanSettings::default(),
        };
        let err = scan(&db, &opts, &cancel, &mut |_| {}).unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[test]
    fn abbruch_laesst_index_gueltig() {
        let dir = TempDir::new("scan-cancel");
        for i in 0..20 {
            dir.file(&format!("f{i}.txt"), b"x");
        }
        let db = open_db(&dir);
        let cancel = AtomicBool::new(true);
        let opts = ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() };
        let p = scan(&db, &opts, &cancel, &mut |_| {}).unwrap();
        assert!(p.cancelled);
        assert_eq!(p.removed, 0);
    }
}

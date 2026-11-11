//! Ueberwachung des aktiven Ordners.
//!
//! Ziel ist nicht Vollstaendigkeit, sondern dass der Index nicht veraltet,
//! waehrend der Nutzer arbeitet. Ereignisse werden gebuendelt und einzeln
//! nachgezogen - ein kompletter Neuscan findet nie automatisch statt.
//!
//! Kann die Ueberwachung nicht starten (Netzlaufwerk, fehlende Rechte), ist das
//! kein Fehler: dann bleibt es beim Scan auf Zuruf.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::db::Db;
use crate::error::Result;
use crate::paths;
use crate::scan;

/// So lange wird nach dem letzten Ereignis gewartet, bevor der Index nachzieht.
const SAMMELZEIT: Duration = Duration::from_millis(1200);
/// Obergrenze fuer das Sammeln. Ohne sie wuerde ein dauerhaft belebter Ordner
/// (Download laeuft, Programm schreibt staendig) das Nachziehen ewig aufschieben.
const MAX_SAMMELZEIT: Duration = Duration::from_secs(4);

/// Solange dieses Handle lebt, laeuft die Ueberwachung.
pub struct WatchHandle {
    pub root: String,
    stop: Arc<AtomicBool>,
    _watcher: notify::RecommendedWatcher,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Startet die Ueberwachung. `on_change` bekommt die Zahl der angepassten
/// Eintraege, sobald ein Schwung abgearbeitet ist.
/// So viele Pfade werden hoechstens gesammelt, bevor die Ueberwachung
/// aufgibt und einen vollstaendigen Scan verlangt. Bei einem Ereignissturm
/// (Entpacken eines grossen Archivs) ist ein Scan guenstiger als zehntausende
/// Einzelabfragen - und vor allem ehrlicher als eine Warteschlange, die
/// unbegrenzt waechst.
const MAX_OFFENE_PFADE: usize = 5_000;

pub fn start(
    db: Db,
    root: PathBuf,
    recursive: bool,
    on_change: impl Fn(usize) + Send + 'static,
) -> Result<WatchHandle> {
    start_mit_rescan(db, root, recursive, on_change, |_| {})
}

/// Wie [`start`], meldet zusaetzlich, wenn der Index nicht mehr nachgefuehrt
/// werden kann und ein vollstaendiger Scan noetig ist.
///
/// Das passiert bei Ereignisverlust: Windows meldet einen Pufferueberlauf,
/// oder es kommen mehr Aenderungen, als sich einzeln nachziehen lassen. Beides
/// darf nicht stillschweigend hingenommen werden - sonst haelt FileTriage
/// einen unvollstaendigen Index fuer die Wahrheit.
pub fn start_mit_rescan(
    db: Db,
    root: PathBuf,
    recursive: bool,
    on_change: impl Fn(usize) + Send + 'static,
    on_rescan_noetig: impl Fn(&str) + Send + 'static,
) -> Result<WatchHandle> {
    let root_id = {
        let conn = db.conn()?;
        scan::root_id_for(&conn, &paths::display_path(&root))?
    };
    let Some(root_id) = root_id else {
        return Err(crate::error::AppError::NotFound(
            "Der Ordner ist noch nicht indexiert".into(),
        ));
    };

    let (sender, receiver) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(sender)
        .map_err(|e| crate::error::AppError::Io(format!("Überwachung: {e}")))?;
    watcher
        .watch(
            &paths::extended(&root),
            if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive },
        )
        .map_err(|e| crate::error::AppError::Io(format!("Überwachung von {}: {e}", root.display())))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    std::thread::spawn(move || {
        let mut offen: HashSet<PathBuf> = HashSet::new();
        let mut sammelt_seit: Option<std::time::Instant> = None;
        loop {
            if stop_thread.load(Ordering::Relaxed) {
                break;
            }
            match receiver.recv_timeout(SAMMELZEIT) {
                Ok(Ok(event)) => {
                    if matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
                        if sammelt_seit.is_none() {
                            sammelt_seit = Some(std::time::Instant::now());
                        }
                        offen.extend(event.paths);
                    }
                    if offen.len() > MAX_OFFENE_PFADE {
                        tracing::warn!(
                            "Ereignissturm: {} offene Pfade - vollständiger Scan noetig",
                            offen.len()
                        );
                        offen.clear();
                        sammelt_seit = None;
                        on_rescan_noetig("event_storm");
                        continue;
                    }
                    // Weiter sammeln, solange Ereignisse kommen - aber nicht laenger
                    // als MAX_SAMMELZEIT, sonst zieht der Index nie nach.
                    let ueberfaellig =
                        sammelt_seit.is_some_and(|t| t.elapsed() >= MAX_SAMMELZEIT);
                    if !ueberfaellig {
                        continue;
                    }
                }
                Ok(Err(e)) => {
                    // Ein Fehler der Ueberwachung bedeutet fast immer verlorene
                    // Ereignisse (Pufferueberlauf von ReadDirectoryChangesW).
                    // Danach ist der Index nicht mehr verlaesslich nachgefuehrt.
                    tracing::warn!("Überwachungsfehler, Index gilt als veraltet: {e}");
                    offen.clear();
                    sammelt_seit = None;
                    on_rescan_noetig("watch_error");
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            if offen.is_empty() {
                sammelt_seit = None;
                continue;
            }
            sammelt_seit = None;
            let stapel: Vec<PathBuf> = offen.drain().collect();
            let mut geaendert = 0usize;
            match db.conn() {
                Ok(mut conn) => {
                    for pfad in stapel {
                        match scan::index_path(&mut conn, root_id, &pfad) {
                            Ok(true) => geaendert += 1,
                            Ok(false) => {}
                            Err(e) => tracing::debug!("Index nicht angepasst ({}): {e}", pfad.display()),
                        }
                    }
                }
                Err(e) => tracing::warn!("Überwachung ohne Datenbank: {e}"),
            }
            if geaendert > 0 {
                on_change(geaendert);
            }
        }
        tracing::debug!("Überwachung beendet");
    });

    Ok(WatchHandle {
        root: paths::display_path(&root),
        stop,
        _watcher: watcher,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScanSettings;
    use crate::scan::ScanOptions;
    use crate::testutil::TempDir;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn neue_datei_landet_ohne_neuscan_im_index() {
        let dir = TempDir::new("watch");
        // Der Index liegt bewusst ausserhalb des ueberwachten Ordners - im
        // echten Betrieb ebenso (LOCALAPPDATA), sonst loesen die eigenen
        // Datenbankschreibvorgaenge laufend Ereignisse aus.
        let dbdir = TempDir::new("watch-db");
        dir.file("start.txt", b"x");
        let db = Db::open(dbdir.path().join("idx.db")).unwrap();
        scan::scan(
            &db,
            &ScanOptions { root: dir.path().to_path_buf(), settings: ScanSettings::default() },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();

        let zaehler = Arc::new(AtomicUsize::new(0));
        let z = zaehler.clone();
        let _handle = start(db.clone(), dir.path().to_path_buf(), true, move |n| {
            z.fetch_add(n, Ordering::Relaxed);
        })
        .unwrap();

        dir.file("später.pdf", b"neu");

        // Sammelzeit plus Reserve abwarten.
        let mut gefunden = false;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(200));
            let conn = db.conn().unwrap();
            let n: i64 = conn
                .query_row("SELECT count(*) FROM files WHERE name = 'später.pdf'", [], |r| r.get(0))
                .unwrap();
            if n == 1 {
                gefunden = true;
                break;
            }
        }
        assert!(gefunden, "die neue Datei muss ohne erneuten Scan im Index auftauchen");
        assert!(zaehler.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn ueberwachung_eines_fehlenden_ordners_scheitert_sauber() {
        let dir = TempDir::new("watch-missing");
        let db = Db::open(dir.path().join(".idx.db")).unwrap();
        let Err(err) = start(db, dir.path().join("gibtsnicht"), true, |_| {}) else {
            panic!("ein fehlender Ordner darf nicht überwacht werden können");
        };
        assert!(err.is_skippable() || err.kind() == "not_found");
    }
}

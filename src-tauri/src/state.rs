//! Gemeinsamer Zustand der Anwendung.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::config::Config;
use crate::db::Db;
use crate::error::Result;
use crate::model::ScanProgress;

/// Steuerung des laufenden Scans.
#[derive(Default)]
pub struct ScanControl {
    pub running: bool,
    pub cancel: Option<Arc<AtomicBool>>,
    pub last: Option<ScanProgress>,
    pub error: Option<String>,
}

/// Steuerung der laufenden Duplikatsuche.
#[derive(Default)]
pub struct DupControl {
    pub running: bool,
    pub cancel: Option<Arc<AtomicBool>>,
    pub last: Option<crate::dup::DupProgress>,
    pub error: Option<String>,
}

pub struct AppState {
    pub db: Db,
    pub config: RwLock<Config>,
    pub scan: Mutex<ScanControl>,
    pub dup: Mutex<DupControl>,
    /// Ueberwachung des aktiven Ordners; `None`, wenn sie nicht laeuft.
    pub watch: Mutex<Option<crate::watcher::WatchHandle>>,
    /// Einmalige Meldung vom Start, etwa ein neu angelegter Index.
    pub startup_notice: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(db: Db) -> Result<AppState> {
        let config = {
            let conn = db.conn()?;
            Config::load(&conn)?
        };
        Ok(AppState {
            db,
            config: RwLock::new(config),
            scan: Mutex::new(ScanControl::default()),
            dup: Mutex::new(DupControl::default()),
            watch: Mutex::new(None),
            startup_notice: Mutex::new(None),
        })
    }

    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    pub fn set_config(&self, cfg: Config) -> Result<()> {
        let conn = self.db.conn()?;
        cfg.save(&conn)?;
        *self.config.write() = cfg;
        Ok(())
    }

    /// Bricht einen laufenden Scan ab (idempotent).
    pub fn cancel_scan(&self) {
        let guard = self.scan.lock();
        if let Some(flag) = guard.cancel.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Bricht eine laufende Duplikatsuche ab (idempotent).
    pub fn cancel_dup(&self) {
        let guard = self.dup.lock();
        if let Some(flag) = guard.cancel.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

//! SQLite-Index.
//!
//! Der Index ist reine Beschleunigung, niemals die Wahrheit: Er speichert nur
//! Metadaten, nie Dateiinhalte. Er darf jederzeit geloescht und durch einen
//! erneuten Scan wiederhergestellt werden.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::{AppError, Result};
use crate::paths;

/// Schema-Version. Bei Erhoehung eine Migration in [`migrate`] ergaenzen.
const SCHEMA_VERSION: i32 = 6;

/// Version 6: vollstaendige Temp-Beweise und starke Windows-Identitaeten.
/// Alle Felder bleiben NULL fuer alte Aktionen; Recovery behandelt das
/// konservativ als fehlenden Beweis.
const SCHEMA_V6: &str = r#"
ALTER TABLE actions ADD COLUMN temp_hash TEXT;
ALTER TABLE actions ADD COLUMN source_strong_id TEXT;
ALTER TABLE actions ADD COLUMN temp_strong_id TEXT;
ALTER TABLE actions ADD COLUMN dst_strong_id TEXT;
"#;

/// Version 5: kryptografische Inhaltsbeweise fuer Crash-Recovery. File-IDs
/// beweisen nur Objektidentitaet und reichen vor einem Delete nicht aus.
const SCHEMA_V5: &str = r#"
ALTER TABLE actions ADD COLUMN src_hash TEXT;
ALTER TABLE actions ADD COLUMN verified_hash TEXT;
"#;

/// Version 4: stabile Identitaet verifizierter Zwischen- und Zieldateien.
const SCHEMA_V4: &str = r#"
ALTER TABLE actions ADD COLUMN temp_volume INTEGER;
ALTER TABLE actions ADD COLUMN temp_index  INTEGER;
ALTER TABLE actions ADD COLUMN dst_volume  INTEGER;
ALTER TABLE actions ADD COLUMN dst_index   INTEGER;
"#;

/// Version 2: Das Aktionsjournal wird zum Zustandsautomaten.
///
/// Bis Version 1 wurde erst gehandelt und danach protokolliert - ein Absturz
/// dazwischen liess eine verschobene Datei ohne Undo-Eintrag zurueck. Ab hier
/// steht die Absicht vor der Tat, und jeder Schritt hat einen Zustand.
/// Version 3: abgelehnte Namensmuster.
///
/// Ein passiver Vorschlag, den der Nutzer weggeklickt hat, darf nicht beim
/// naechsten Oeffnen wieder dastehen. Die Typvorschlaege merken sich das in
/// `move_stats.dismissed`; die Namensmuster stammen aus dem Journal und
/// brauchen deshalb eine eigene Ablage.
const SCHEMA_V3: &str = r#"
-- Verknuepfung der Bedingungen einer Regel: "all" (Voreinstellung) oder "any".
ALTER TABLE rules ADD COLUMN match_mode TEXT NOT NULL DEFAULT 'all';

CREATE TABLE IF NOT EXISTS dismissed_patterns (
    token      TEXT    NOT NULL,
    target_key TEXT    NOT NULL,
    ts         INTEGER NOT NULL,
    PRIMARY KEY (token, target_key)
);
"#;

const SCHEMA_V2: &str = r#"
ALTER TABLE actions ADD COLUMN state        TEXT    NOT NULL DEFAULT 'completed';
ALTER TABLE actions ADD COLUMN session_id   TEXT    NOT NULL DEFAULT '';
ALTER TABLE actions ADD COLUMN temp         TEXT;
ALTER TABLE actions ADD COLUMN finished_at  INTEGER;
ALTER TABLE actions ADD COLUMN src_mtime    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE actions ADD COLUMN src_mtime_ns INTEGER NOT NULL DEFAULT 0;
ALTER TABLE actions ADD COLUMN dst_size     INTEGER;
ALTER TABLE actions ADD COLUMN dst_mtime    INTEGER;
ALTER TABLE actions ADD COLUMN dst_mtime_ns INTEGER;
ALTER TABLE actions ADD COLUMN file_volume  INTEGER;
ALTER TABLE actions ADD COLUMN file_index   INTEGER;
ALTER TABLE actions ADD COLUMN cross_volume INTEGER NOT NULL DEFAULT 0;
ALTER TABLE actions ADD COLUMN verify       TEXT;
ALTER TABLE actions ADD COLUMN trash_from   INTEGER;
ALTER TABLE actions ADD COLUMN trash_to     INTEGER;
ALTER TABLE actions ADD COLUMN error        TEXT;

CREATE INDEX IF NOT EXISTS idx_actions_state   ON actions(state);
CREATE INDEX IF NOT EXISTS idx_actions_session ON actions(session_id);
"#;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS roots (
    id             INTEGER PRIMARY KEY,
    path           TEXT    NOT NULL,
    path_key       TEXT    NOT NULL UNIQUE,
    recursive      INTEGER NOT NULL DEFAULT 1,
    include_hidden INTEGER NOT NULL DEFAULT 0,
    added_at       INTEGER NOT NULL,
    last_scan      INTEGER
);

CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY,
    root_id    INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    path_key   TEXT    NOT NULL UNIQUE,
    parent     TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    name_lower TEXT    NOT NULL,
    ext        TEXT    NOT NULL,
    size       INTEGER NOT NULL,
    created    INTEGER NOT NULL DEFAULT 0,
    modified   INTEGER NOT NULL DEFAULT 0,
    category   TEXT    NOT NULL,
    quick_hash TEXT,
    full_hash  TEXT,
    hash_size  INTEGER,
    hash_mtime INTEGER,
    state      TEXT    NOT NULL DEFAULT 'pending',
    decided_at INTEGER,
    seen_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_files_root      ON files(root_id);
CREATE INDEX IF NOT EXISTS idx_files_size      ON files(size DESC);
CREATE INDEX IF NOT EXISTS idx_files_modified  ON files(modified);
CREATE INDEX IF NOT EXISTS idx_files_ext       ON files(ext);
CREATE INDEX IF NOT EXISTS idx_files_category  ON files(category);
CREATE INDEX IF NOT EXISTS idx_files_state     ON files(state);
CREATE INDEX IF NOT EXISTS idx_files_name      ON files(name_lower);
CREATE INDEX IF NOT EXISTS idx_files_fullhash  ON files(full_hash);

-- Journal aller Dateiaktionen. Basis fuer Undo und fuer Regelvorschlaege.
CREATE TABLE IF NOT EXISTS actions (
    id            INTEGER PRIMARY KEY,
    batch_id      TEXT,
    ts            INTEGER NOT NULL,
    kind          TEXT    NOT NULL,
    src           TEXT    NOT NULL,
    dst           TEXT,
    original_name TEXT    NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    ext           TEXT    NOT NULL DEFAULT '',
    undone        INTEGER NOT NULL DEFAULT 0,
    undoable      INTEGER NOT NULL DEFAULT 1,
    note          TEXT
);

CREATE INDEX IF NOT EXISTS idx_actions_ts    ON actions(ts DESC);
CREATE INDEX IF NOT EXISTS idx_actions_batch ON actions(batch_id);

CREATE TABLE IF NOT EXISTS rules (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    priority   INTEGER NOT NULL DEFAULT 100,
    match_json TEXT    NOT NULL,
    target     TEXT    NOT NULL,
    source     TEXT    NOT NULL DEFAULT 'user',
    created_at INTEGER NOT NULL,
    hits       INTEGER NOT NULL DEFAULT 0
);

-- Verhaltensstatistik: wie oft ging welcher Dateityp in welchen Zielordner.
CREATE TABLE IF NOT EXISTS move_stats (
    ext        TEXT    NOT NULL,
    category   TEXT    NOT NULL,
    target     TEXT    NOT NULL,
    count      INTEGER NOT NULL DEFAULT 0,
    last_ts    INTEGER NOT NULL,
    dismissed  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ext, category, target)
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Handle auf die Indexdatenbank. Verbindungen werden pro Vorgang geoeffnet;
/// mit WAL ist das guenstig und vermeidet eine globale Sperre zwischen
/// Oberflaeche und Hintergrundscan.
#[derive(Clone)]
pub struct Db {
    file: PathBuf,
    // Haelt die gemeinsam benannte In-Memory-Datenbank des sicheren
    // Lesefallbacks am Leben. Produktive Datenbanken brauchen keinen Anker.
    memory_anchor: Option<std::sync::Arc<parking_lot::Mutex<Connection>>>,
    /// Beim Oeffnen existierte noch kein Bestandteil des SQLite-Bundles.
    /// Nur dann darf der Lizenzdienst einen neuen Kulanzanker anlegen.
    new_install: bool,
    /// Exactly the incident snapshot created for this startup, if it contained
    /// unresolved journal state. Unrelated bundles are never owned here.
    recovery_bundle: Option<PathBuf>,
}

impl Db {
    pub fn open_default() -> Result<Db> {
        paths::ensure_dirs()?;
        Db::open(paths::db_path())
    }

    /// Öffnet den Index und weicht einem unbrauchbaren Index aus.
    ///
    /// Der Index ist **nie die Wahrheit** - die Platte ist es. Wenn er sich
    /// nicht öffnen lässt (beschädigt, von einem anderen Programm gesperrt,
    /// aus einer neueren Version), darf das nicht bedeuten, dass FileTriage
    /// gar nicht erst startet. Der alte Index wird beiseitegelegt und ein
    /// neuer angelegt; ein Scan füllt ihn in Sekunden wieder.
    ///
    /// Beiseitelegen statt löschen: Falls jemand ihn untersuchen will, ist er
    /// noch da. Dateien des Nutzers sind davon ohnehin nie betroffen.
    pub fn open_default_resilient(recovery_required: bool) -> (Db, Option<String>) {
        paths::ensure_dirs().ok();
        Db::open_resilient_with_mode(paths::db_path(), recovery_required)
    }

    #[cfg(test)]
    fn open_resilient(pfad: PathBuf) -> (Db, Option<String>) {
        Db::open_resilient_with_mode(pfad, true)
    }

    fn open_resilient_with_mode(pfad: PathBuf, recovery_required: bool) -> (Db, Option<String>) {
        let vorhanden = BundlePresence::at(&pfad);
        if !vorhanden.any() {
            return match Db::open(&pfad) {
                Ok(db) => (db, None),
                Err(e) => resilient_fallback(&pfad, e, None, RecoveryState::Unknown(
                    "Datenbank konnte nicht erstmals angelegt werden".into(),
                )),
            };
        }

        // Der durable Run-Marker beweist, dass der vorige Prozess nach dem
        // Ende aller IPC-Arbeit sauber beendet wurde. Nur dann ist kein
        // moeglicherweise offener Journalzustand zu konservieren.
        if !recovery_required {
            return match Db::open(&pfad) {
                Ok(db) => (db, None),
                Err(e) => resilient_fallback(
                    &pfad,
                    e,
                    None,
                    RecoveryState::Clean,
                ),
            };
        }

        // Das Original darf vor der forensischen Kopie nicht einmal durch
        // SQLite geoeffnet werden: Bereits Connection::open und WAL-Pragmas
        // koennen Header oder Sidecars veraendern bzw. checkpointen.
        let Some(sicherung) = backup_database_bundle(&pfad) else {
            return resilient_fallback(
                &pfad,
                AppError::JournalUnavailable("SQLite-Bundle konnte nicht gesichert werden".into()),
                None,
                RecoveryState::Unknown("SQLite-Bundle konnte nicht gesichert werden".into()),
            );
        };
        let analyse = analyze_recovery_state(&sicherung.main);
        if matches!(analyse, RecoveryState::Unknown(_)) {
            return resilient_fallback(
                &pfad,
                AppError::JournalUnavailable("Recovery-Zustand ist nicht eindeutig".into()),
                Some(sicherung.dir),
                analyse,
            );
        }

        match Db::open(&pfad) {
            Ok(mut db) => {
                // Ein Bundle mit offenem Journal ist forensischer Zustand und
                // bleibt bis zu einem spaeteren nachweislich sauberen Ende
                // erhalten. Nur ein bestaetigt sauberer False Alarm darf hier
                // sofort verschwinden.
                if matches!(analyse, RecoveryState::Clean) {
                    let _ = std::fs::remove_dir_all(&sicherung.dir);
                } else {
                    db.recovery_bundle=Some(sicherung.dir.clone());
                }
                (db, None)
            }
            Err(e) => resilient_fallback(&pfad, e, Some(sicherung.dir), analyse),
        }
    }

    pub fn open(file: impl Into<PathBuf>) -> Result<Db> {
        let file = file.into();
        let new_install = !BundlePresence::at(&file).any();
        let db = Db { file, memory_anchor: None, new_install, recovery_bundle:None };
        if let Some(parent) = db.file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::from_io(&e, &format!("Index {}", parent.display())))?;
        }
        let conn = db.conn()?;
        migrate(&conn)?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.file
    }

    pub fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.file)?;
        configure(&conn)?;
        Ok(conn)
    }

    pub(crate) fn open_memory_for_reading() -> Result<Db> {
        let uri = format!(
            "file:filetriage-diagnose-{}-{}?mode=memory&cache=shared",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let anchor = Connection::open(&uri)?;
        configure(&anchor)?;
        migrate(&anchor)?;
        Ok(Db {
            file: PathBuf::from(uri),
            memory_anchor: Some(std::sync::Arc::new(parking_lot::Mutex::new(anchor))),
            new_install: false,
            recovery_bundle: None,
        })
    }

    pub fn has_persistent_journal(&self) -> bool {
        self.memory_anchor.is_none()
    }

    pub fn is_new_install(&self) -> bool {
        self.new_install
    }

    pub(crate) fn resolve_own_recovery_bundle(&self) {
        let Some(dir)=self.recovery_bundle.as_ref() else{return};
        if !validate_bundle(dir){return}
        let manifest_path=dir.join("manifest.json");
        let Ok(mut manifest)=std::fs::read(&manifest_path).ok().and_then(|v|serde_json::from_slice::<BundleManifest>(&v).ok()).ok_or(()) else{return};
        if manifest.resolution!="unresolved"{return}
        manifest.resolution="resolved".into();
        let Ok(raw)=serde_json::to_vec_pretty(&manifest) else{return};
        let temp=dir.join("manifest.resolved.tmp");
        let persisted=std::fs::File::create(&temp).and_then(|mut f|{use std::io::Write;f.write_all(&raw)?;f.sync_all()}).and_then(|_|std::fs::rename(&temp,&manifest_path));
        if persisted.is_ok()&&validate_bundle(dir){let _=std::fs::remove_dir_all(dir);}
    }
}

pub fn bundle_exists(path: &Path) -> bool {
    BundlePresence::at(path).any()
}

fn resilient_fallback(
    _path: &Path,
    error: AppError,
    sicherung: Option<PathBuf>,
    analyse: RecoveryState,
) -> (Db, Option<String>) {
    tracing::warn!("Index nicht benutzbar: {error}");
    let db = Db::open_memory_for_reading().unwrap_or_else(|memory_error| {
        panic!("Weder persistenter noch sicherer Diagnoseindex verfügbar: {error}; {memory_error}")
    });
    let analyse_text = match analyse {
        RecoveryState::Open(count) => {
            format!("Die gesicherte Datenbank enthält {count} offene Dateioperation(en).")
        }
        RecoveryState::Clean => {
            "In der lesbaren Datenbank wurden keine offenen Dateioperationen gefunden.".to_string()
        }
        RecoveryState::Unknown(reason) => format!(
            "Der Recovery-Zustand konnte nicht eindeutig gelesen werden: {reason}."
        ),
    };
    let sicherung_text = sicherung
        .map(|p| format!(" Eine unveränderte Sicherung liegt unter {}.", p.display()))
        .unwrap_or_else(|| {
            " Die Datenbank konnte nicht zusätzlich gesichert werden; das Original blieb unverändert."
                .to_string()
        });
    let hinweis = format!(
        "Der persistente Index ist nicht verfügbar. {analyse_text}{sicherung_text} \
         Scannen, Anzeigen und Diagnose bleiben möglich. Dateioperationen sind gesperrt, \
         bis der Journalzustand eindeutig geklärt ist."
    );
    tracing::error!("{hinweis}");
    (db, Some(hinweis))
}

fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // `FULL` statt `NORMAL`: Das Journal muss auch einen Stromausfall
        // ueberstehen, nicht nur einen Prozessabbruch. Mit NORMAL koennte im
        // WAL-Modus die zuletzt festgeschriebene Transaktion verloren gehen -
        // ausgerechnet die, die "Datei ist jetzt woanders" festhaelt. Der Preis
        // ist ein fsync je Zustandswechsel; gemessen in PERFORMANCE.md.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum RecoveryState {
    Clean,
    Open(i64),
    Unknown(String),
}

/// Untersucht eine alte Datenbank ausschliesslich lesend. Jede Unsicherheit
/// bleibt Unsicherheit; sie darf niemals als "kein Recovery noetig" gelten.
fn analyze_recovery_state(path: &Path) -> RecoveryState {
    if !path.is_file() {
        return RecoveryState::Unknown("Datenbankdatei fehlt".into());
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(path, flags) {
        Ok(c) => c,
        Err(e) => return RecoveryState::Unknown(e.to_string()),
    };
    let has_actions: i64 = match conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='actions'",
        [],
        |r| r.get(0),
    ) {
        Ok(n) => n,
        Err(e) => return RecoveryState::Unknown(e.to_string()),
    };
    if has_actions == 0 {
        return RecoveryState::Clean;
    }
    let has_state: i64 = match conn.query_row(
        "SELECT count(*) FROM pragma_table_info('actions') WHERE name='state'",
        [],
        |r| r.get(0),
    ) {
        Ok(n) => n,
        Err(e) => return RecoveryState::Unknown(e.to_string()),
    };
    if has_state == 0 {
        return RecoveryState::Clean;
    }
    match conn.query_row(
        "SELECT count(*) FROM actions WHERE state NOT IN ('completed','failed','rolled_back')",
        [],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(0) => RecoveryState::Clean,
        Ok(n) => RecoveryState::Open(n),
        Err(e) => RecoveryState::Unknown(e.to_string()),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct BundlePresence {
    main: bool,
    wal: bool,
    shm: bool,
}

impl BundlePresence {
    fn at(path: &Path) -> Self {
        Self {
            main: path.is_file(),
            wal: sidecar(path, "-wal").is_file(),
            shm: sidecar(path, "-shm").is_file(),
        }
    }

    fn any(self) -> bool {
        self.main || self.wal || self.shm
    }
}

struct BundleSnapshot {
    dir: PathBuf,
    main: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    version: u32,
    complete: bool,
    created_at: i64,
    incident_id: String,
    resolution: String,
    reason: String,
    db_generation: Option<String>,
    components: Vec<BundleComponent>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleComponent {
    name: String,
    present: bool,
    size: Option<u64>,
    blake3: Option<String>,
}

/// Kopiert Hauptdatei und WAL-Begleiter als unveraendertes Beweispaket. Das
/// Original wird weder umbenannt noch geloescht.
fn backup_database_bundle(path: &Path) -> Option<BundleSnapshot> {
    backup_database_bundle_inner(path, None)
}

fn backup_database_bundle_inner(path: &Path, stop_after_copies: Option<usize>) -> Option<BundleSnapshot> {
    let presence = BundlePresence::at(path);
    if !presence.any() {
        return None;
    }
    let stamp = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    let preferred_base = path.parent()?.to_path_buf();
    let fallback_base = std::env::temp_dir().join("FileTriage-Recovery");
    let partial_name = format!(".recovery-partial-{stamp}");
    let final_name = format!("recovery-{stamp}");
    let (partial, final_dir) = if std::fs::create_dir_all(preferred_base.join(&partial_name)).is_ok() {
        (preferred_base.join(&partial_name), preferred_base.join(&final_name))
    } else if std::fs::create_dir_all(fallback_base.join(&partial_name)).is_ok() {
        (fallback_base.join(&partial_name), fallback_base.join(&final_name))
    } else {
        return None;
    };

    let sources = [
        (path.to_path_buf(), "index.db"),
        (sidecar(path, "-wal"), "index.db-wal"),
        (sidecar(path, "-shm"), "index.db-shm"),
    ];
    let mut components = Vec::with_capacity(sources.len());
    let mut copied = 0usize;
    for (source, fixed_name) in sources {
        let name = fixed_name.to_string();
        if !source.is_file() {
            components.push(BundleComponent { name, present: false, size: None, blake3: None });
            continue;
        }
        let target = partial.join(&name);
        if std::fs::copy(&source, &target).is_err() {
            return None;
        }
        let sync = std::fs::OpenOptions::new().write(true).open(&target).and_then(|f| f.sync_all());
        if sync.is_err() {
            return None;
        }
        copied += 1;
        if stop_after_copies == Some(copied) {
            return None;
        }
        components.push(BundleComponent {
            name,
            present: true,
            size: std::fs::metadata(&target).ok().map(|m| m.len()),
            blake3: hash_file(&target),
        });
    }
    let manifest = BundleManifest {
        version: 2,
        complete: true,
        created_at: now_ts(),
        incident_id: stamp,
        resolution: "unresolved".into(),
        reason: "unclean_start".into(),
        // The incident id is the recovery generation. Reading application
        // metadata from the copied SQLite bundle here could checkpoint or
        // create sidecars and would invalidate the forensic hashes.
        db_generation: None,
        components,
    };
    let metadata = serde_json::to_vec_pretty(&manifest).ok()?;
    let manifest_path = partial.join("manifest.json");
    let mut manifest_file = std::fs::File::create(&manifest_path).ok()?;
    use std::io::Write as _;
    manifest_file.write_all(&metadata).ok()?;
    manifest_file.sync_all().ok()?;
    drop(manifest_file);
    if !validate_bundle(&partial) {
        return None;
    }
    if crate::lifecycle::publish_new(&partial, &final_dir).is_err() || !validate_bundle(&final_dir) {
        return None;
    }
    Some(BundleSnapshot {
        main: final_dir.join("index.db"),
        dir: final_dir,
    })
}

fn hash_file(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn validate_bundle(dir: &Path) -> bool {
    let manifest: BundleManifest = match std::fs::read(dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<BundleManifest>(&raw).ok())
    {
        Some(value) if value.complete && value.version == 2 && matches!(value.resolution.as_str(),"unresolved"|"resolved"|"abandoned_by_user") && !value.incident_id.is_empty() && !value.reason.is_empty() => value,
        _ => return false,
    };
    const EXPECTED: [&str; 3] = ["index.db", "index.db-wal", "index.db-shm"];
    if manifest.components.len() != EXPECTED.len() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    for component in &manifest.components {
        let component_path = Path::new(&component.name);
        if !EXPECTED.contains(&component.name.as_str())
            || component_path.file_name().and_then(|v| v.to_str()) != Some(component.name.as_str())
            || component_path.components().count() != 1
            || !seen.insert(component.name.as_str())
        {
            return false;
        }
        let path = dir.join(&component.name);
        if !component.present {
            if std::fs::symlink_metadata(&path).is_ok()
                || component.size.is_some()
                || component.blake3.is_some()
            {
                return false;
            }
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else { return false };
        if !meta.file_type().is_file()
            || crate::fsmeta::is_reparse_point(&meta)
            || Some(meta.len()) != component.size
            || hash_file(&path) != component.blake3
        {
            return false;
        }
    }
    if !EXPECTED.iter().all(|name| seen.contains(name)) {
        return false;
    }
    let allowed = ["manifest.json", "index.db", "index.db-wal", "index.db-shm"];
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { return false };
        if !allowed.contains(&name.as_str()) { return false; }
    }
    true
}


pub fn migrate(conn: &Connection) -> Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    // Die gesamte Migration laeuft in einer Transaktion: entweder das Schema
    // ist vollstaendig aktuell, oder es bleibt unveraendert. Ein Abbruch
    // mittendrin darf keine halbe Schemaversion hinterlassen.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let ergebnis = (|| -> Result<()> {
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_V2)?;
        }
        if version < 3 {
            conn.execute_batch(SCHEMA_V3)?;
        }
        if version < 4 {
            conn.execute_batch(SCHEMA_V4)?;
        }
        if version < 5 {
            conn.execute_batch(SCHEMA_V5)?;
        }
        if version < 6 {
            conn.execute_batch(SCHEMA_V6)?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    })();
    match ergebnis {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Einfacher Schluessel-/Wert-Speicher fuer Konfiguration.
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use crate::testutil::TempDir;

    fn temp_db() -> (TempDir, Db) {
        let dir = TempDir::new("filetriage-db");
        let db = Db::open(dir.path().join("index.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn schema_wird_angelegt_und_ist_idempotent() {
        let (_dir, db) = temp_db();
        let conn = db.conn().unwrap();
        migrate(&conn).unwrap();
        let v: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 6, "erwartet mindestens 6 Tabellen, waren {n}");
    }

    /// Ein beschaedigter Index darf den Start nicht verhindern. Er ist
    /// jederzeit aus dem Dateisystem wiederherstellbar - im Gegensatz zu den
    /// Dateien des Nutzers.
    #[test]
    fn beschaedigter_index_wird_gesichert_und_mutationen_bleiben_gesperrt() {
        let dir = TempDir::new("db-kaputt");
        // Etwas, das keine SQLite-Datenbank ist.
        let pfad = dir.path().join("index.db");
        std::fs::write(&pfad, b"das ist keine Datenbank").unwrap();

        let (db, hinweis) = Db::open_resilient(pfad.clone());
        assert!(hinweis.is_some(), "der Nutzer muss davon erfahren");
        assert!(
            hinweis.as_deref().unwrap().contains("Dateioperationen sind gesperrt"),
            "der Hinweis muss die Sperre erklaeren"
        );
        assert!(!db.has_persistent_journal());
        assert_eq!(std::fs::read(&pfad).unwrap(), b"das ist keine Datenbank");

        // Der Diagnoseindex bleibt fuer nicht mutierende Daten verwendbar.
        let conn = db.conn().unwrap();
        set_setting(&conn, "probe", "geht").unwrap();
        assert_eq!(get_setting(&conn, "probe").unwrap().as_deref(), Some("geht"));

        let sicherungen: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("recovery-"))
            .collect();
        assert_eq!(sicherungen.len(), 1, "die alte DB wurde nicht gesichert");
    }

    fn hash(path: &Path) -> blake3::Hash {
        blake3::hash(&std::fs::read(path).unwrap())
    }

    fn active_wal(path: &Path) -> (Db, Connection) {
        let db = Db::open(path).unwrap();
        let conn = db.conn().unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute(
            "INSERT INTO actions (ts, kind, state, src, original_name) \
             VALUES (1, 'move', 'planned', 'C:\\Quelle.txt', 'Quelle.txt')",
            [],
        )
        .unwrap();
        assert!(sidecar(path, "-wal").is_file());
        (db, conn)
    }

    fn copy_bundle(source: &Path, target: &Path) {
        for (from, to) in [
            (source.to_path_buf(), target.to_path_buf()),
            (sidecar(source, "-wal"), sidecar(target, "-wal")),
            (sidecar(source, "-shm"), sidecar(target, "-shm")),
        ] {
            if from.is_file() {
                std::fs::copy(from, to).unwrap();
            }
        }
    }

    #[test]
    fn nicht_oeffenbare_datenbank_startet_nur_im_gesperrten_diagnosemodus() {
        let dir = TempDir::new("db-nicht-oeffenbar");
        let als_datei_unbrauchbar = dir.dir("index.db");
        let (db, hinweis) = Db::open_resilient(als_datei_unbrauchbar);
        assert!(!db.has_persistent_journal());
        assert!(hinweis.unwrap().contains("Dateioperationen sind gesperrt"));
    }

    #[test]
    fn beschaedigte_db_mit_moeglichem_offenem_journal_bleibt_unveraendert() {
        let dir = TempDir::new("db-offenes-journal-kaputt");
        let pfad = dir.path().join("index.db");
        let db = Db::open(&pfad).unwrap();
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO actions (ts, kind, state, src, original_name) \
             VALUES (1, 'move', 'planned', 'C:\\Quelle.txt', 'Quelle.txt')",
            [],
        )
        .unwrap();
        drop(conn);
        assert_eq!(analyze_recovery_state(&pfad), RecoveryState::Open(1));

        let mut bytes = std::fs::read(&pfad).unwrap();
        bytes[..16].copy_from_slice(b"kaputte-db-heade");
        std::fs::write(&pfad, &bytes).unwrap();
        let vorher = std::fs::read(&pfad).unwrap();

        let (fallback, notice) = Db::open_resilient(pfad.clone());
        assert!(!fallback.has_persistent_journal());
        assert_eq!(std::fs::read(&pfad).unwrap(), vorher, "Original-DB wurde veraendert");
        assert!(notice.unwrap().contains("nicht eindeutig"));
    }

    #[test]
    fn sidecars_ohne_hauptdatei_werden_gesichert_und_nicht_ersetzt() {
        let dir = TempDir::new("db-verwaiste-sidecars");
        let pfad = dir.path().join("index.db");
        std::fs::write(sidecar(&pfad, "-wal"), b"wal-beweis").unwrap();
        std::fs::write(sidecar(&pfad, "-shm"), b"shm-beweis").unwrap();

        let (db, notice) = Db::open_resilient(pfad.clone());
        assert!(!db.has_persistent_journal());
        assert!(!pfad.exists(), "eine fehlende Hauptdatei darf nicht neu erzeugt werden");
        assert_eq!(std::fs::read(sidecar(&pfad, "-wal")).unwrap(), b"wal-beweis");
        assert!(notice.unwrap().contains("nicht eindeutig"));

        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("recovery-"))
            .unwrap()
            .path();
        let metadata: BundleManifest =
            serde_json::from_slice(&std::fs::read(recovery.join("manifest.json")).unwrap()).unwrap();
        assert!(metadata.complete);
        assert!(!metadata.components.iter().find(|c| c.name == "index.db").unwrap().present);
        assert!(metadata.components.iter().find(|c| c.name == "index.db-wal").unwrap().present);
        assert!(metadata.components.iter().find(|c| c.name == "index.db-shm").unwrap().present);
    }

    #[test]
    fn echte_wal_analyse_beruehrt_das_originalbundle_nicht() {
        let dir = TempDir::new("db-echte-wal");
        let pfad = dir.path().join("index.db");
        let db = Db::open(&pfad).unwrap();
        let conn = db.conn().unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute(
            "INSERT INTO actions (ts, kind, state, src, original_name) \
             VALUES (1, 'move', 'planned', 'C:\\Quelle.txt', 'Quelle.txt')",
            [],
        )
        .unwrap();
        let wal = sidecar(&pfad, "-wal");
        let shm = sidecar(&pfad, "-shm");
        assert!(wal.is_file() && shm.is_file(), "Test braucht ein echtes aktives WAL-Bundle");
        let vorher = [hash(&pfad), hash(&wal), hash(&shm)];

        let snapshot = backup_database_bundle(&pfad).unwrap();
        assert_eq!(analyze_recovery_state(&snapshot.main), RecoveryState::Open(1));

        assert_eq!([hash(&pfad), hash(&wal), hash(&shm)], vorher);
        drop(conn);
    }

    #[test]
    fn beschaedigte_main_mit_echtem_wal_bleibt_vor_der_entscheidung_unveraendert() {
        let dir = TempDir::new("db-wal-main-kaputt");
        let source = dir.path().join("source.db");
        let (_db, conn) = active_wal(&source);
        let target = dir.path().join("broken.db");
        copy_bundle(&source, &target);
        let mut main = std::fs::read(&target).unwrap();
        main[..16].copy_from_slice(b"kaputte-db-heade");
        std::fs::write(&target, main).unwrap();
        let wal = sidecar(&target, "-wal");
        let shm = sidecar(&target, "-shm");
        let vorher = [hash(&target), hash(&wal), hash(&shm)];

        let (fallback, _) = Db::open_resilient(target.clone());
        assert!(!fallback.has_persistent_journal());
        assert_eq!([hash(&target), hash(&wal), hash(&shm)], vorher);
        drop(conn);
    }

    #[test]
    fn fehlende_main_mit_echtem_wal_allein_wird_nicht_neu_angelegt() {
        let dir = TempDir::new("db-wal-ohne-main");
        let source = dir.path().join("source.db");
        let (_db, conn) = active_wal(&source);
        let target = dir.path().join("missing.db");
        std::fs::copy(sidecar(&source, "-wal"), sidecar(&target, "-wal")).unwrap();
        let wal = sidecar(&target, "-wal");
        let vorher = hash(&wal);

        let (fallback, _) = Db::open_resilient(target.clone());
        assert!(!fallback.has_persistent_journal());
        assert!(!target.exists());
        assert_eq!(hash(&wal), vorher);
        drop(conn);
    }

    #[test]
    fn sauberer_start_kopiert_keinen_datenbankbestandteil() {
        let dir = TempDir::new("db-clean-start");
        let path = dir.path().join("index.db");
        let db = Db::open(&path).unwrap();
        drop(db);

        let (opened, notice) = Db::open_resilient_with_mode(path, false);
        assert!(opened.has_persistent_journal());
        assert!(notice.is_none());
        assert!(!std::fs::read_dir(dir.path()).unwrap().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.starts_with("recovery-") || name.starts_with(".recovery-partial-")
        }));
    }

    #[test]
    fn unklarer_start_sichert_vor_dem_schreibenden_open() {
        let dir = TempDir::new("db-unclean-start");
        let path = dir.path().join("index.db");
        std::fs::write(&path, b"keine sqlite datenbank").unwrap();
        let before = hash(&path);

        let (opened, _) = Db::open_resilient_with_mode(path.clone(), true);
        assert!(!opened.has_persistent_journal());
        assert_eq!(hash(&path), before);
        let recovery = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with("recovery-"))
            .unwrap()
            .path();
        assert!(validate_bundle(&recovery));
    }

    #[test]
    fn unterbrochene_bundle_erstellung_bleibt_eindeutig_partiell() {
        let dir = TempDir::new("db-partial-bundle");
        let path = dir.path().join("index.db");
        let db = Db::open(&path).unwrap();
        drop(db);

        assert!(backup_database_bundle_inner(&path, Some(1)).is_none());
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert!(entries.iter().any(|entry| entry.file_name().to_string_lossy().starts_with(".recovery-partial-")));
        assert!(!entries.iter().any(|entry| entry.file_name().to_string_lossy().starts_with("recovery-")));
        let partial = entries
            .iter()
            .find(|entry| entry.file_name().to_string_lossy().starts_with(".recovery-partial-"))
            .unwrap()
            .path();
        assert!(!validate_bundle(&partial));
    }

    #[test]
    fn vollstaendiges_bundle_hat_gepruefte_hashes_und_erkennt_manipulation() {
        let dir = TempDir::new("db-complete-bundle");
        let path = dir.path().join("index.db");
        let db = Db::open(&path).unwrap();
        drop(db);
        let snapshot = backup_database_bundle(&path).unwrap();
        assert!(validate_bundle(&snapshot.dir));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&snapshot.main)
            .unwrap()
            .write_all(b"manipuliert")
            .unwrap();
        assert!(!validate_bundle(&snapshot.dir));
    }

    #[test]
    fn bundle_manifest_muss_exakt_und_vollstaendig_sein(){
        let d=TempDir::new("bundle-strict");let p=d.path().join("index.db");drop(Db::open(&p).unwrap());
        for mutation in 0..5 {let s=backup_database_bundle(&p).unwrap();let mp=s.dir.join("manifest.json");let mut m:BundleManifest=serde_json::from_slice(&std::fs::read(&mp).unwrap()).unwrap();match mutation{0=>m.components.clear(),1=>{m.components.pop();},2=>m.components.push(m.components[0].clone()),3=>m.components[0].name="..\\escape.db".into(),_=>{std::fs::write(s.dir.join("unexpected.bin"),b"x").unwrap();}}if mutation<4{std::fs::write(&mp,serde_json::to_vec(&m).unwrap()).unwrap();}assert!(!validate_bundle(&s.dir),"mutation {mutation}");}
    }

    #[test]
    fn snapshot_mit_offenem_journal_bleibt_nach_erfolgreichem_open(){
        let d=TempDir::new("bundle-retained");let p=d.path().join("index.db");let db=Db::open(&p).unwrap();let c=db.conn().unwrap();c.execute("INSERT INTO actions(ts,kind,state,src,original_name,size)VALUES(1,'move','planned','a','a',1)",[]).unwrap();drop(c);drop(db);
        let(opened,notice)=Db::open_resilient_with_mode(p.clone(),true);assert!(opened.has_persistent_journal());assert!(notice.is_none());assert!(std::fs::read_dir(d.path()).unwrap().flatten().any(|e|e.file_name().to_string_lossy().starts_with("recovery-")));
    }

    #[test]
    fn sauberer_unabhaengiger_index_loescht_keinen_fremden_incident_snapshot(){
        let d=TempDir::new("bundle-incident-scope");let old=d.path().join("old.db");drop(Db::open(&old).unwrap());
        let snapshot=backup_database_bundle(&old).unwrap();let clean=Db::open(d.path().join("new.db")).unwrap();
        clean.resolve_own_recovery_bundle();assert!(snapshot.dir.exists());assert!(validate_bundle(&snapshot.dir));
    }

    #[test]
    fn nur_der_explizit_zugeordnete_incident_wird_aufgeloest(){
        let d=TempDir::new("bundle-two-incidents");let a=d.dir("one").join("index.db");let b=d.dir("two").join("index.db");drop(Db::open(&a).unwrap());drop(Db::open(&b).unwrap());
        let one=backup_database_bundle(&a).unwrap();let two=backup_database_bundle(&b).unwrap();
        let owner=Db{file:a,memory_anchor:None,new_install:false,recovery_bundle:Some(one.dir.clone())};owner.resolve_own_recovery_bundle();
        assert!(!one.dir.exists());assert!(two.dir.exists());assert!(validate_bundle(&two.dir));
    }

    #[test]
    #[ignore]
    fn sauberer_start_mit_grosser_db_und_wal_kopiert_nichts() {
        let dir = TempDir::new("db-large-clean-start");
        let path = dir.path().join("index.db");
        let (db, conn) = active_wal(&path);
        conn.execute("CREATE TABLE payload(data BLOB)", []).unwrap();
        conn.execute("INSERT INTO payload(data) VALUES (?1)", [vec![7u8; 8 * 1024 * 1024]])
            .unwrap();
        let started = std::time::Instant::now();
        let (opened, notice) = Db::open_resilient_with_mode(path, false);
        eprintln!("clean large DB/WAL startup: {:?}", started.elapsed());
        assert!(opened.has_persistent_journal());
        assert!(notice.is_none());
        assert!(!std::fs::read_dir(dir.path()).unwrap().flatten().any(|entry| {
            entry.file_name().to_string_lossy().contains("recovery-")
        }));
        drop(conn);
        drop(db);
    }

    #[test]
    fn einstellungen_ueberschreiben_sich() {
        let (_dir, db) = temp_db();
        let conn = db.conn().unwrap();
        assert_eq!(get_setting(&conn, "theme").unwrap(), None);
        set_setting(&conn, "theme", "dark").unwrap();
        set_setting(&conn, "theme", "light").unwrap();
        assert_eq!(get_setting(&conn, "theme").unwrap().as_deref(), Some("light"));
    }
}

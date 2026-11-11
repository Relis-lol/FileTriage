//! Befehle, die die Oberflaeche aufrufen darf.
//!
//! Die Schicht ist bewusst duenn: Validierung und Fachlogik liegen in den
//! Fachmodulen, hier wird nur uebersetzt und der Zustand verwaltet.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::config::Config;
use crate::db;
use crate::dup;
use crate::error::{AppError, Result};
use crate::fileops::{self, CollisionPolicy, OpResult, OpStatus};
use crate::model::ScanProgress;
use crate::preview;
use crate::query::{self, CategoryCount, FilePage, FileQuery};
use crate::rules;
use crate::scan::{self, ScanOptions};
use crate::state::AppState;
use crate::undo;

/// Ereignisname fuer Scan-Fortschritt.
pub const EVENT_SCAN_PROGRESS: &str = "scan:progress";

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    pub db_path: String,
    pub log_dir: String,
    /// Einmalige Meldung vom Start - etwa, dass ein neuer Index angelegt
    /// werden musste. Wird genau einmal abgeholt.
    pub notice: Option<String>,
}

/// Reifegrad dieses Standes.
///
/// Die Paketversion bleibt `0.1.0`, weil der NSIS-Installer nur numerische
/// Versionen kennt. Sichtbar soll trotzdem sein, dass dies ein Release
/// Candidate ist und keine fertige Fassung - wer das Programm benutzt, soll
/// nicht raten muessen.
pub const RELEASE_LABEL: &str = "0.1.0-rc4-store";

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: RELEASE_LABEL.to_string(),
        data_dir: crate::paths::data_dir().display().to_string(),
        db_path: crate::paths::db_path().display().to_string(),
        log_dir: crate::paths::log_dir().display().to_string(),
        notice: state.startup_notice.lock().take(),
    }
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Config {
    state.config()
}

#[tauri::command]
pub fn save_config(state: State<'_, AppState>, config: Config) -> Result<Config> {
    state.set_config(config)?;
    Ok(state.config())
}

/// Oeffnet den Windows-Ordnerdialog. Liefert `None`, wenn abgebrochen wurde.
#[tauri::command]
pub async fn pick_folder(title: Option<String>) -> Option<String> {
    let dialog = rfd::AsyncFileDialog::new()
        .set_title(title.unwrap_or_else(|| "Ordner auswählen".into()));
    dialog.pick_folder().await.map(|h| h.path().display().to_string())
}

// ---------------------------------------------------------------------------
// Ordnerangebote fuer den Start
// ---------------------------------------------------------------------------

/// Die bekannten Windows-Ordner, die es auf diesem Rechner wirklich gibt.
///
/// Der Erststart bietet sie an, damit niemand erst einen Dateidialog bedienen
/// muss, um zu verstehen, was das Programm tut.
#[tauri::command]
pub fn known_folders() -> Vec<crate::knownfolders::FolderOffer> {
    crate::knownfolders::verfuegbare()
}

#[derive(Serialize)]
pub struct RecentRoot {
    pub path: String,
    /// Nur der letzte Namensbestandteil - das liest sich besser als der Pfad.
    pub name: String,
    /// Wahr, wenn der Ordner noch existiert.
    pub available: bool,
    /// Offene Dateien aus dem Index, sofern er den Ordner kennt.
    pub pending: i64,
}

/// Zuletzt aufgeraeumte Ordner.
///
/// Wird nur gelesen und angezeigt - hier wird nichts gescannt und nichts
/// angefasst. Das passiert erst, wenn der Nutzer einen davon waehlt.
#[tauri::command]
pub fn recent_roots(state: State<'_, AppState>) -> Result<Vec<RecentRoot>> {
    let cfg = state.config();
    let conn = state.db.conn()?;
    let mut out = Vec::new();
    for p in &cfg.recent_roots {
        let pfad = Path::new(p);
        let offen = query::list_files(
            &conn,
            &FileQuery {
                root: Some(p.clone()),
                states: vec!["pending".into()],
                limit: 1,
                ..Default::default()
            },
        )
        .map(|s| s.total)
        .unwrap_or(0);
        out.push(RecentRoot {
            name: pfad
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone()),
            available: pfad.is_dir(),
            pending: offen,
            path: p.clone(),
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn forget_recent(state: State<'_, AppState>, path: String) -> Result<()> {
    let mut cfg = state.config();
    cfg.remove_recent(&path);
    state.set_config(cfg)
}

#[derive(Serialize)]
pub struct PreparedRoot {
    pub path: String,
    pub name: String,
    /// Wahr, wenn dabei zum ersten Mal Standardziele gesetzt wurden.
    pub defaults_applied: bool,
    pub targets: Vec<crate::config::TargetFolder>,
}

/// Nimmt einen gewaehlten oder hineingezogenen Ordner an.
///
/// Prueft, dass es wirklich ein Ordner ist, merkt ihn als zuletzt verwendet und
/// sorgt dafuer, dass Zielordner bereitstehen. **Es wird dabei keine einzige
/// Datei angefasst** - auch nicht bei Drag and Drop.
#[tauri::command]
pub fn prepare_root(state: State<'_, AppState>, path: String) -> Result<PreparedRoot> {
    let pfad = PathBuf::from(path.trim());
    crate::paths::validate_directory_path(&pfad)?;
    if !crate::paths::extended(&pfad).is_dir() {
        return Err(AppError::Invalid(format!(
            "{} ist kein Ordner. Ziehe einen Ordner herein, keine einzelne Datei.",
            pfad.display()
        )));
    }
    // Eine Verknuepfung als Arbeitsordner waere unklar - siehe fsmeta.
    if let Ok(zustand) = crate::fsmeta::inspect(&pfad) {
        if zustand.class == crate::fsmeta::FileClass::Reparse {
            return Err(AppError::Invalid(
                "Dieser Ordner ist eine Verknüpfung. Wähle bitte den Ordner selbst.".into(),
            ));
        }
    }

    let anzeige = crate::paths::display_path(&pfad);
    let mut cfg = state.config();
    let neu_gesetzt = cfg.apply_default_targets(Some(&anzeige));
    cfg.push_recent(&anzeige);
    cfg.last_root = Some(anzeige.clone());
    // Ab hier gilt der Erststart als erledigt: Der Nutzer hat einen Ordner
    // gewaehlt, mehr braucht es nicht.
    cfg.onboarding_done = true;
    state.set_config(cfg)?;

    let cfg = state.config();
    Ok(PreparedRoot {
        name: pfad
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| anzeige.clone()),
        path: anzeige,
        defaults_applied: neu_gesetzt,
        targets: cfg.targets.clone(),
    })
}

#[derive(Serialize)]
pub struct RootInfo {
    pub id: i64,
    pub path: String,
    pub recursive: bool,
    pub last_scan: Option<i64>,
    pub files: i64,
    pub total_size: i64,
}

#[tauri::command]
pub fn list_roots(state: State<'_, AppState>) -> Result<Vec<RootInfo>> {
    let conn = state.db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT r.id, r.path, r.recursive, r.last_scan,
                (SELECT count(*) FROM files f WHERE f.root_id = r.id),
                (SELECT coalesce(sum(size),0) FROM files f WHERE f.root_id = r.id)
         FROM roots r ORDER BY r.added_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RootInfo {
                id: r.get(0)?,
                path: r.get(1)?,
                recursive: r.get::<_, i32>(2)? != 0,
                last_scan: r.get(3)?,
                files: r.get(4)?,
                total_size: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[tauri::command]
pub fn remove_root(state: State<'_, AppState>, id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    // Entfernt nur den Index, niemals Dateien auf der Platte.
    conn.execute("DELETE FROM roots WHERE id = ?1", [id])?;
    Ok(())
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<'_, AppState>, root: String) -> Result<()> {
    let root_path = PathBuf::from(root.trim());
    if root_path.as_os_str().is_empty() {
        return Err(AppError::Invalid("Kein Ordner angegeben".into()));
    }
    if !root_path.is_absolute() {
        return Err(AppError::Invalid("Es wird ein absoluter Pfad benötigt".into()));
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut guard = state.scan.lock();
        if guard.running {
            return Err(AppError::Invalid("Es laeuft bereits ein Scan".into()));
        }
        guard.running = true;
        guard.error = None;
        guard.cancel = Some(cancel.clone());
        guard.last = Some(ScanProgress {
            root: root_path.display().to_string(),
            ..Default::default()
        });
    }

    // Zuletzt gewaehlten Ordner merken, damit der naechste Start dort beginnt.
    let mut cfg = state.config();
    let settings = cfg.scan.clone();
    cfg.last_root = Some(root_path.display().to_string());
    let _ = state.set_config(cfg);

    let db = state.db.clone();
    std::thread::spawn(move || {
        let opts = ScanOptions { root: root_path, settings };
        let result = scan::scan(&db, &opts, &cancel, &mut |p| {
            let _ = app.emit(EVENT_SCAN_PROGRESS, p);
            if let Some(state) = app.try_state::<AppState>() {
                state.scan.lock().last = Some(p.clone());
            }
        });

        if let Some(state) = app.try_state::<AppState>() {
            let mut guard = state.scan.lock();
            guard.running = false;
            guard.cancel = None;
            match result {
                Ok(p) => {
                    let fertig = !p.cancelled;
                    guard.last = Some(p);
                    drop(guard);
                    if fertig {
                        starte_ueberwachung(&app, &state, &opts.root);
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!("Scan fehlgeschlagen: {e}");
                    guard.error = Some(e.to_string());
                    if let Some(last) = guard.last.as_mut() {
                        last.done = true;
                    }
                    let _ = app.emit(
                        EVENT_SCAN_PROGRESS,
                        ScanProgress { done: true, ..Default::default() },
                    );
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>) {
    state.cancel_scan();
}

#[derive(Serialize)]
pub struct ScanStatus {
    pub running: bool,
    pub progress: Option<ScanProgress>,
    pub error: Option<String>,
}

#[tauri::command]
pub fn scan_status(state: State<'_, AppState>) -> ScanStatus {
    let guard = state.scan.lock();
    ScanStatus {
        running: guard.running,
        progress: guard.last.clone(),
        error: guard.error.clone(),
    }
}

#[tauri::command]
pub fn list_files(state: State<'_, AppState>, query: FileQuery) -> Result<FilePage> {
    let conn = state.db.conn()?;
    query::list_files(&conn, &query)
}

#[tauri::command]
pub fn category_breakdown(
    state: State<'_, AppState>,
    query: FileQuery,
) -> Result<Vec<CategoryCount>> {
    let conn = state.db.conn()?;
    query::category_breakdown(&conn, &query)
}

pub const EVENT_INDEX_CHANGED: &str = "index:changed";
pub const EVENT_RESCAN_NEEDED: &str = "index:rescan";

/// Startet die Ordnerueberwachung fuer den gerade gescannten Ordner.
///
/// Meldet die Ueberwachung, dass sie Aenderungen verloren hat, wird ein
/// vollstaendiger Scan nachgeschoben. Ein stiller, unvollstaendiger Index
/// waere gefaehrlicher als ein kurzer Mehraufwand: Auf ihm beruhen Anzeigen,
/// aus denen der Nutzer Loeschentscheidungen ableitet.
fn starte_ueberwachung(app: &AppHandle, state: &AppState, root: &Path) {
    let app_change = app.clone();
    let app_rescan = app.clone();
    let root_fuer_rescan = root.to_path_buf();
    let recursive = state.config().scan.recursive;

    let handle = crate::watcher::start_mit_rescan(
        state.db.clone(),
        root.to_path_buf(),
        recursive,
        move |n| {
            let _ = app_change.emit(EVENT_INDEX_CHANGED, n);
        },
        move |grund| {
            tracing::info!("Erneuter Scan angefordert: {grund}");
            let _ = app_rescan.emit(EVENT_RESCAN_NEEDED, grund);
            // Der Scan laeuft ueber denselben Weg wie ein Scan von Hand -
            // inklusive Sperre gegen parallele Laeufe.
            if let Some(state) = app_rescan.try_state::<AppState>() {
                let laeuft = state.scan.lock().running;
                if !laeuft {
                    let _ = start_scan(
                        app_rescan.clone(),
                        state,
                        root_fuer_rescan.display().to_string(),
                    );
                }
            }
        },
    );

    match handle {
        Ok(h) => *state.watch.lock() = Some(h),
        Err(e) => {
            // Ohne Ueberwachung bleibt alles nutzbar, der Index altert nur.
            tracing::info!("Ordnerüberwachung nicht möglich: {e}");
            *state.watch.lock() = None;
        }
    }
}

#[derive(Serialize)]
pub struct WatchStatus {
    pub active: bool,
    pub root: Option<String>,
}

#[tauri::command]
pub fn watch_status(state: State<'_, AppState>) -> WatchStatus {
    let guard = state.watch.lock();
    WatchStatus {
        active: guard.is_some(),
        root: guard.as_ref().map(|h| h.root.clone()),
    }
}

#[tauri::command]
pub fn stop_watch(state: State<'_, AppState>) {
    *state.watch.lock() = None;
}

/// Kennzahlen fuer die Startseite.
#[derive(Serialize, Default)]
pub struct Overview {
    pub files: i64,
    pub total_size: i64,
    pub pending: i64,
    pub large: i64,
    pub old: i64,
    /// Wahr, solange fuer diesen Ordner noch nie nach Duplikaten gesucht wurde.
    pub duplicates_unchecked: bool,
}

#[tauri::command]
pub fn overview(state: State<'_, AppState>, root: Option<String>) -> Result<Overview> {
    let conn = state.db.conn()?;
    let cfg = state.config();
    let base = FileQuery { root: root.clone(), ..Default::default() };

    let all = query::list_files(&conn, &FileQuery { limit: 1, ..base.clone() })?;
    let pending = query::list_files(
        &conn,
        &FileQuery { states: vec!["pending".into()], limit: 1, ..base.clone() },
    )?;
    let large = query::list_files(
        &conn,
        &FileQuery {
            min_size: Some(cfg.large_file_mb as i64 * 1024 * 1024),
            limit: 1,
            ..base.clone()
        },
    )?;
    let cutoff = db::now_ts() - (cfg.old_file_days as i64 * 86_400);
    let old = query::list_files(
        &conn,
        &FileQuery { modified_before: Some(cutoff), limit: 1, ..base },
    )?;

    // Nur den betrachteten Ordner zaehlen, sonst gilt ein anderer Ordner
    // faelschlich als bereits geprueft.
    let root_id = match root.as_deref() {
        Some(r) => scan::root_id_for(&conn, r)?,
        None => None,
    };
    let gehasht: i64 = match root_id {
        Some(id) => conn.query_row(
            "SELECT count(*) FROM files WHERE quick_hash IS NOT NULL AND root_id = ?1",
            [id],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT count(*) FROM files WHERE quick_hash IS NOT NULL",
            [],
            |r| r.get(0),
        )?,
    };

    Ok(Overview {
        files: all.total,
        total_size: all.total_size,
        pending: pending.total,
        large: large.total,
        old: old.total,
        duplicates_unchecked: gehasht == 0,
    })
}

// ---------------------------------------------------------------------------
// Dateiaktionen
// ---------------------------------------------------------------------------

/// Verschiebt eine Datei in den Zielordner der angegebenen Zifferntaste.
#[tauri::command]
pub fn move_to_target(
    state: State<'_, AppState>,
    path: String,
    target_key: String,
    policy: Option<CollisionPolicy>,
) -> Result<OpResult> {
    let cfg = state.config();
    let target = cfg
        .target_for_key(&target_key)
        .ok_or_else(|| AppError::Invalid(format!("Kein Zielordner auf Taste {target_key}")))?;
    let conn = state.db.conn()?;
    Ok(fileops::move_file(
        &conn,
        Path::new(&path),
        Path::new(&target.path),
        policy.unwrap_or_default(),
        None,
    ))
}

/// Verschiebt in einen frei angegebenen Ordner (z. B. aus der Regelvorschau).
#[tauri::command]
pub fn move_to_folder(
    state: State<'_, AppState>,
    path: String,
    folder: String,
    policy: Option<CollisionPolicy>,
) -> Result<OpResult> {
    let conn = state.db.conn()?;
    Ok(fileops::move_file(
        &conn,
        Path::new(&path),
        Path::new(&folder),
        policy.unwrap_or_default(),
        None,
    ))
}

/// Stapelverschiebung mit gemeinsamer Stapel-ID, damit sie als Ganzes
/// zurueckgenommen werden kann.
#[tauri::command]
pub fn move_many(
    state: State<'_, AppState>,
    paths: Vec<String>,
    folder: String,
    policy: Option<CollisionPolicy>,
) -> Result<BatchResult> {
    let conn = state.db.conn()?;
    let batch_id = new_batch_id();
    let policy = policy.unwrap_or_default();
    let results: Vec<OpResult> = paths
        .iter()
        .map(|p| {
            fileops::move_file(
                &conn,
                Path::new(p),
                Path::new(&folder),
                policy,
                Some(&batch_id),
            )
        })
        .collect();
    Ok(BatchResult::new(batch_id, results))
}

#[tauri::command]
pub fn delete_file(
    state: State<'_, AppState>,
    path: String,
    confirm_permanent: Option<bool>,
) -> Result<OpResult> {
    let mode = pruefe_loeschmodus(&state, confirm_permanent)?;
    let conn = state.db.conn()?;
    Ok(fileops::delete_file(&conn, Path::new(&path), mode, None))
}

/// Ermittelt den geltenden Loeschmodus.
///
/// Endgueltiges Loeschen verlangt zweierlei: eine bestaetigte Einstellung
/// **und** eine ausdrueckliche Bestaetigung fuer genau diesen Aufruf. Damit
/// kann weder eine beschaedigte Konfiguration noch ein falsch belegter Hotkey
/// unbemerkt am Papierkorb vorbei loeschen.
fn pruefe_loeschmodus(
    state: &State<'_, AppState>,
    confirm_permanent: Option<bool>,
) -> Result<crate::config::DeleteMode> {
    use crate::config::DeleteMode;
    let mode = state.config().effective_delete_mode();
    if mode == DeleteMode::Permanent && confirm_permanent != Some(true) {
        return Err(AppError::Invalid(
            "Endgültiges Löschen muss für jede Aktion einzeln bestätigt werden".into(),
        ));
    }
    Ok(mode)
}

#[tauri::command]
pub fn delete_many(
    state: State<'_, AppState>,
    paths: Vec<String>,
    confirm_permanent: Option<bool>,
) -> Result<BatchResult> {
    let mode = pruefe_loeschmodus(&state, confirm_permanent)?;
    let conn = state.db.conn()?;
    let batch_id = new_batch_id();
    let results: Vec<OpResult> = paths
        .iter()
        .map(|p| fileops::delete_file(&conn, Path::new(p), mode, Some(&batch_id)))
        .collect();
    Ok(BatchResult::new(batch_id, results))
}

#[tauri::command]
pub fn rename_file(state: State<'_, AppState>, path: String, new_name: String) -> Result<OpResult> {
    let conn = state.db.conn()?;
    Ok(fileops::rename_file(&conn, Path::new(&path), &new_name))
}

/// Markiert eine Datei, ohne sie anzufassen: "kept", "skipped" oder "pending".
#[tauri::command]
pub fn set_file_state(state: State<'_, AppState>, path: String, new_state: String) -> Result<()> {
    let conn = state.db.conn()?;
    fileops::set_state(&conn, &path, &new_state)
}

#[derive(Serialize)]
pub struct BatchResult {
    pub batch_id: String,
    pub done: usize,
    pub skipped: usize,
    pub collisions: usize,
    pub failed: usize,
    pub results: Vec<OpResult>,
}

impl BatchResult {
    fn new(batch_id: String, results: Vec<OpResult>) -> BatchResult {
        let zaehle = |s: OpStatus| results.iter().filter(|r| r.status == s).count();
        BatchResult {
            done: zaehle(OpStatus::Done),
            skipped: zaehle(OpStatus::Skipped),
            collisions: zaehle(OpStatus::Collision),
            failed: zaehle(OpStatus::Failed),
            batch_id,
            results,
        }
    }
}

fn new_batch_id() -> String {
    format!(
        "b{}-{}",
        db::now_ts(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    )
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn undo_last(state: State<'_, AppState>) -> Result<Option<undo::UndoResult>> {
    let conn = state.db.conn()?;
    undo::undo_last(&conn)
}

#[tauri::command]
pub fn undo_action(state: State<'_, AppState>, id: i64) -> Result<undo::UndoResult> {
    let conn = state.db.conn()?;
    Ok(undo::undo(&conn, id))
}

#[tauri::command]
pub fn undo_batch(state: State<'_, AppState>, batch_id: String) -> Result<Vec<undo::UndoResult>> {
    let conn = state.db.conn()?;
    undo::undo_batch(&conn, &batch_id)
}

#[tauri::command]
pub fn action_history(state: State<'_, AppState>) -> Result<Vec<undo::ActionRecord>> {
    let conn = state.db.conn()?;
    undo::history(&conn, undo::HISTORY_LIMIT)
}

// ---------------------------------------------------------------------------
// Sitzungen
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_sessions(
    state: State<'_, AppState>,
) -> Result<Vec<crate::journal::SessionSummary>> {
    let conn = state.db.conn()?;
    undo::sessions(&conn, 20)
}

/// Zeigt ohne jede Veraenderung, was ein Sitzungs-Undo bewirken wuerde.
#[tauri::command]
pub fn plan_session_undo(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<undo::SessionUndoPlan> {
    let conn = state.db.conn()?;
    undo::plan_session_undo(&conn, &session_id)
}

/// Rollt eine Sitzung zurueck - nur die konfliktfreien Aktionen.
#[tauri::command]
pub fn undo_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<undo::UndoResult>> {
    let conn = state.db.conn()?;
    undo::undo_session(&conn, &session_id)
}

#[tauri::command]
pub fn current_session(_state: State<'_, AppState>) -> String {
    crate::session_id()
}

// ---------------------------------------------------------------------------
// Wiederherstellung nach Absturz
// ---------------------------------------------------------------------------

/// Bericht ueber unfertige Operationen. Wird beim Start abgefragt.
#[tauri::command]
pub fn recovery_report(state: State<'_, AppState>) -> Result<crate::recovery::RecoveryReport> {
    let conn = state.db.conn()?;
    // Ohne automatische Aufloesung: Der Bericht zeigt, was ist.
    crate::recovery::inspect(&conn, false)
}

#[tauri::command]
pub fn resolve_recovery(
    state: State<'_, AppState>,
    action_id: i64,
    choice: String,
) -> Result<String> {
    let conn = state.db.conn()?;
    crate::recovery::resolve(&conn, action_id, &choice)
}

// ---------------------------------------------------------------------------
// Duplikate
// ---------------------------------------------------------------------------

pub const EVENT_DUP_PROGRESS: &str = "dup:progress";

#[tauri::command]
pub fn find_duplicates(
    app: AppHandle,
    state: State<'_, AppState>,
    root: Option<String>,
    min_size: Option<i64>,
) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut guard = state.dup.lock();
        if guard.running {
            return Err(AppError::Invalid("Die Duplikatsuche laeuft bereits".into()));
        }
        guard.running = true;
        guard.error = None;
        guard.cancel = Some(cancel.clone());
        guard.last = None;
    }

    let db = state.db.clone();
    let min_size = min_size.unwrap_or(1).max(1);
    std::thread::spawn(move || {
        let result = dup::find(&db, root.as_deref(), min_size, &cancel, &mut |p| {
            let _ = app.emit(EVENT_DUP_PROGRESS, p);
        });
        if let Some(state) = app.try_state::<AppState>() {
            let mut guard = state.dup.lock();
            guard.running = false;
            guard.cancel = None;
            match result {
                Ok(p) => guard.last = Some(p),
                Err(e) => {
                    tracing::warn!("Duplikatsuche fehlgeschlagen: {e}");
                    guard.error = Some(e.to_string());
                    let _ = app.emit(
                        EVENT_DUP_PROGRESS,
                        dup::DupProgress { done: true, ..Default::default() },
                    );
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_duplicates(state: State<'_, AppState>) {
    state.cancel_dup();
}

#[derive(Serialize)]
pub struct DupStatus {
    pub running: bool,
    pub progress: Option<dup::DupProgress>,
    pub error: Option<String>,
}

#[tauri::command]
pub fn duplicate_status(state: State<'_, AppState>) -> DupStatus {
    let guard = state.dup.lock();
    DupStatus {
        running: guard.running,
        progress: guard.last.clone(),
        error: guard.error.clone(),
    }
}

#[tauri::command]
pub fn duplicate_groups(
    state: State<'_, AppState>,
    root: Option<String>,
    min_size: Option<i64>,
) -> Result<Vec<dup::DupGroup>> {
    let conn = state.db.conn()?;
    dup::groups(&conn, root.as_deref(), min_size.unwrap_or(1).max(1))
}

// ---------------------------------------------------------------------------
// Regeln
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_rules(state: State<'_, AppState>) -> Result<Vec<rules::Rule>> {
    let conn = state.db.conn()?;
    rules::list(&conn)
}

#[tauri::command]
pub fn save_rule(state: State<'_, AppState>, rule: rules::Rule) -> Result<i64> {
    let conn = state.db.conn()?;
    rules::save(&conn, &rule)
}

#[tauri::command]
pub fn delete_rule(state: State<'_, AppState>, id: i64) -> Result<()> {
    let conn = state.db.conn()?;
    rules::delete(&conn, id)
}

#[tauri::command]
pub fn rule_suggestions(state: State<'_, AppState>) -> Result<Vec<rules::Suggestion>> {
    let cfg = state.config();
    if !cfg.suggestions_enabled {
        return Ok(Vec::new());
    }
    let conn = state.db.conn()?;
    rules::suggestions(&conn, cfg.suggestion_threshold as i64)
}

#[tauri::command]
pub fn accept_suggestion(
    state: State<'_, AppState>,
    suggestion: rules::Suggestion,
) -> Result<i64> {
    let conn = state.db.conn()?;
    rules::save(&conn, &rules::rule_from_suggestion(&suggestion))
}

#[tauri::command]
pub fn dismiss_suggestion(
    state: State<'_, AppState>,
    ext: String,
    category: String,
    target: String,
) -> Result<()> {
    let conn = state.db.conn()?;
    rules::dismiss(&conn, &ext, &category, &target)
}

/// Passive Vorschlaege aus wiederkehrenden Dateinamensmustern.
///
/// Wird nur aufgerufen, wenn der Nutzer die Regelseite oeffnet. Es gibt keinen
/// Hintergrundlauf, keine Meldung und keine Benachrichtigung.
#[tauri::command]
pub fn pattern_suggestions(
    state: State<'_, AppState>,
) -> Result<Vec<crate::patterns::PatternSuggestion>> {
    let cfg = state.config();
    if !cfg.suggestions_enabled {
        return Ok(Vec::new());
    }
    let conn = state.db.conn()?;
    crate::patterns::suggestions(&conn, cfg.suggestion_threshold as i64)
}

/// Macht aus einem Namensmuster eine Regel. **Verschiebt nichts.**
#[tauri::command]
pub fn accept_pattern(
    state: State<'_, AppState>,
    suggestion: crate::patterns::PatternSuggestion,
) -> Result<i64> {
    let conn = state.db.conn()?;
    rules::save(&conn, &suggestion.to_rule())
}

/// Wie viele Dateien der Vorschlag im aktuellen Ordner gerade traefe.
///
/// Baut die Regel nur im Speicher und zaehlt - es wird nichts gespeichert und
/// nichts verschoben. Damit kann die Oberflaeche neben einem Vorschlag eine
/// echte Zahl zeigen, statt den Nutzer raten zu lassen.
#[tauri::command]
pub fn pattern_match_count(
    state: State<'_, AppState>,
    suggestion: crate::patterns::PatternSuggestion,
    root: Option<String>,
) -> Result<RuleMatchCount> {
    let conn = state.db.conn()?;
    let (count, truncated) = rules::match_count(&conn, &suggestion.to_rule(), root.as_deref())?;
    Ok(RuleMatchCount { rule_id: 0, count, truncated })
}

#[tauri::command]
pub fn dismiss_pattern(state: State<'_, AppState>, token: String, target: String) -> Result<()> {
    let conn = state.db.conn()?;
    crate::patterns::dismiss(&conn, &token, &target)
}

#[derive(Serialize)]
pub struct RuleMatchCount {
    pub rule_id: i64,
    pub count: usize,
    /// Wahr, wenn an der Obergrenze abgeschnitten wurde - dann ist `count`
    /// eine Untergrenze und die Oberflaeche muss "mindestens" sagen.
    pub truncated: bool,
}

/// Aktuelle Trefferzahl je Regel - fuer die Regelliste.
///
/// Zaehlt nur; es wird nichts gehasht, nichts von der Platte gelesen und
/// nichts veraendert. Die Oberflaeche ruft das nach dem ersten Zeichnen ab,
/// damit die Seite nicht darauf wartet.
#[tauri::command]
pub fn rule_match_counts(
    state: State<'_, AppState>,
    root: Option<String>,
) -> Result<Vec<RuleMatchCount>> {
    let conn = state.db.conn()?;
    let mut out = Vec::new();
    for regel in rules::list(&conn)? {
        let Some(id) = regel.id else { continue };
        let (count, truncated) = rules::match_count(&conn, &regel, root.as_deref())?;
        out.push(RuleMatchCount { rule_id: id, count, truncated });
    }
    Ok(out)
}

#[derive(Serialize)]
pub struct FolderSize {
    pub path: String,
    pub files: i64,
    /// Summe der Dateigroessen aus dem Index - die **logische** Groesse, nicht
    /// der auf der Platte belegte Platz. Bei Hardlinks und komprimierten
    /// Ordnern liegen die beiden auseinander; FileTriage behauptet deshalb
    /// nirgends, dass so viel Platz frei wuerde.
    pub total_size: i64,
    /// Wahr, wenn dieser Ordner selbst schon einmal eingelesen wurde. Sonst
    /// sind beide Zahlen 0 und bedeuten "unbekannt", nicht "leer".
    pub known: bool,
}

/// Dateizahl und Gesamtgroesse mehrerer Ordner - ausschliesslich aus dem Index.
///
/// Es wird **nicht** gescannt und nichts von der Platte gelesen.
///
/// Gezaehlt wird nur fuer Ordner, die **selbst** schon einmal eingelesen
/// wurden. Das ist der wichtige Teil: Der Index kennt auch Dateien *unterhalb*
/// eines Ordners, ohne den Ordner je gesehen zu haben. Wer einmal
/// `Desktop\Bilder` aufgeraeumt hat, bekaeme sonst am Desktop dessen Zahlen
/// angezeigt - "35 Dateien" fuer einen Ordner mit 500. Eine falsche Zahl ist
/// schlechter als keine.
#[tauri::command]
pub fn folder_sizes(state: State<'_, AppState>, paths: Vec<String>) -> Result<Vec<FolderSize>> {
    let conn = state.db.conn()?;
    let mut out = Vec::new();
    for p in paths.into_iter().take(50) {
        let eingelesen = scan::root_id_for(&conn, &p)?.is_some();
        if !eingelesen {
            out.push(FolderSize { path: p, files: 0, total_size: 0, known: false });
            continue;
        }
        let seite = query::list_files(
            &conn,
            &FileQuery { root: Some(p.clone()), limit: 1, ..Default::default() },
        )?;
        out.push(FolderSize {
            path: p,
            files: seite.total,
            total_size: seite.total_size,
            known: true,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn dry_run(
    state: State<'_, AppState>,
    rule_id: i64,
    root: Option<String>,
) -> Result<rules::DryRun> {
    let conn = state.db.conn()?;
    rules::dry_run(&conn, rule_id, root.as_deref())
}

/// Fuehrt eine Regel aus. Wird von der Oberflaeche erst nach dem Dry Run und
/// einer ausdruecklichen Bestaetigung aufgerufen.
#[tauri::command]
pub fn apply_rule(
    state: State<'_, AppState>,
    rule_id: i64,
    root: Option<String>,
    policy: Option<CollisionPolicy>,
    confirmed_count: Option<usize>,
) -> Result<BatchResult> {
    let conn = state.db.conn()?;
    let batch_id = new_batch_id();
    let ergebnisse = rules::apply(
        &conn,
        rule_id,
        root.as_deref(),
        policy.unwrap_or(CollisionPolicy::Rename),
        &batch_id,
        confirmed_count,
    )?;
    Ok(BatchResult::new(batch_id, ergebnisse))
}

/// Zielvorschlag fuer die aktuelle Datei in der Triage.
#[derive(Serialize)]
pub struct TargetSuggestion {
    /// Zifferntaste, falls der Ordner als Ziel eingerichtet ist.
    pub key: Option<String>,
    pub path: String,
    pub reason_code: String,
    pub rule_name: Option<String>,
    pub count: Option<i64>,
}

#[tauri::command]
pub fn suggest_target(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<TargetSuggestion>> {
    let cfg = state.config();
    if !cfg.suggestions_enabled {
        return Ok(None);
    }
    let conn = state.db.conn()?;
    let key = crate::paths::path_key(&path);
    let Some(datei) = query::file_by_path(&conn, &key)? else {
        return Ok(None);
    };
    let Some((ordner, reason_code, rule_name, count)) =
        rules::suggest_folder(&conn, &datei, cfg.suggestion_threshold as i64)?
    else {
        return Ok(None);
    };
    let taste = cfg
        .targets
        .iter()
        .find(|t| crate::paths::path_key(&t.path) == crate::paths::path_key(&ordner))
        .map(|t| t.key.clone());
    Ok(Some(TargetSuggestion { key: taste, path: ordner, reason_code, rule_name, count }))
}

/// Erzeugt eine Vorschau. Laeuft bewusst auf einem Blocking-Thread, damit
/// grosse Bilder die Oberflaeche nicht anhalten.
#[tauri::command]
pub async fn preview(state: State<'_, AppState>, path: String) -> Result<preview::Preview> {
    let cfg = state.config();
    if !cfg.previews_enabled {
        return Ok(preview::Preview {
            kind: preview::PreviewKind::None,
            path,
            data_url: None,
            text: None,
            truncated: false,
            width: None,
            height: None,
            meta: Vec::new(),
            note_code: Some("disabled".into()),
        });
    }
    let grenze = cfg.preview_max_bytes;
    tauri::async_runtime::spawn_blocking(move || preview::generate(Path::new(&path), grenze))
        .await
        .map_err(|e| AppError::Io(format!("Vorschau abgebrochen: {e}")))
}

/// Oeffnet eine Datei mit der Standardanwendung.
// ---------------------------------------------------------------------------
// Lizenz
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct LicenseInfo {
    /// Der Zustand, wie ihn die Oberflaeche anzeigt.
    pub state: crate::license::LicenseState,
    /// Duerfen gerade Dateien veraendert werden?
    ///
    /// Die Oberflaeche nutzt das nur, um Knoepfe auszugrauen und den Grund zu
    /// erklaeren. Die eigentliche Sperre sitzt im Kern; diese Angabe ist
    /// Hoeflichkeit, kein Schutz.
    pub may_modify: bool,
    /// Vertriebskanal dieses Baus.
    pub channel: &'static str,
    /// Laeuft die Anwendung mit Paketidentitaet? Ohne sie gibt es keine
    /// Store-Lizenz.
    pub packaged: bool,
    /// Zielpreis fuer die Anzeige, bis der Store selbst einen nennt.
    pub price_hint: &'static str,
}

/// Der aktuelle Lizenzzustand.
#[tauri::command]
pub fn license_state(state: State<'_, AppState>) -> Result<LicenseInfo> {
    let dienst = crate::license::service();
    let zustand = dienst.state();
    // Eindeutige Auskuenfte werden gemerkt, damit eine spaetere Stoerung den
    // Nutzer nicht aussperrt.
    if let Ok(conn) = state.db.conn() {
        let _ = dienst.merken(&conn, &zustand);
    }
    Ok(LicenseInfo {
        may_modify: dienst.may_modify(),
        state: zustand,
        channel: crate::license::CHANNEL.as_str(),
        packaged: crate::license::MicrosoftStoreLicenseProvider::hat_paketidentitaet(),
        price_hint: crate::license::PREIS_HINWEIS,
    })
}

/// Oeffnet die Produktseite im Microsoft Store.
///
/// Der `ms-windows-store:`-Link braucht die Store-ID, die es noch nicht gibt.
/// Solange sie fehlt, wird **kein** erfundener Link geoeffnet - die Oberflaeche
/// bekommt einen Fehler und erklaert, wie man das Programm im Store findet.
#[tauri::command]
pub fn open_purchase() -> Result<()> {
    if crate::license::CHANNEL != crate::license::Channel::Store {
        return Err(AppError::Invalid(
            "Dieser Distributionskanal hat keinen Store-Kaufweg".into(),
        ));
    }
    let Some(id) = crate::license::STORE_PRODUCT_ID else {
        return Err(AppError::Invalid(
            "Die Store-Produktkennung steht noch nicht fest".into(),
        ));
    };
    let ziel = format!("ms-windows-store://pdp/?productid={id}");
    std::process::Command::new("explorer.exe")
        .arg(&ziel)
        .spawn()
        .map_err(|e| AppError::from_io(&e, "Microsoft Store"))?;
    Ok(())
}

#[tauri::command]
pub fn open_path(path: String) -> Result<()> {
    let p = PathBuf::from(&path);
    ensure_exists(&p)?;
    // explorer.exe uebergibt das Argument unveraendert an die Shell-Zuordnung,
    // ohne Kommandozeileninterpretation wie bei `cmd /c start`.
    std::process::Command::new("explorer.exe")
        .arg(&p)
        .spawn()
        .map_err(|e| AppError::from_io(&e, "Öffnen"))?;
    Ok(())
}

/// Zeigt die Datei im Explorer an und markiert sie.
#[tauri::command]
pub fn reveal_path(path: String) -> Result<()> {
    let p = PathBuf::from(&path);
    ensure_exists(&p)?;
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", p.display()))
        .spawn()
        .map_err(|e| AppError::from_io(&e, "Explorer"))?;
    Ok(())
}

fn ensure_exists(p: &Path) -> Result<()> {
    if !crate::paths::extended(p).exists() {
        return Err(AppError::NotFound(p.display().to_string()));
    }
    Ok(())
}

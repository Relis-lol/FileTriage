//! FileTriage - "Inbox Zero fuer Dateien".
//!
//! Aufbau:
//! - [`scan`]   findet Dateien und pflegt den Index
//! - [`db`]     haelt den SQLite-Index (nur Metadaten)
//! - [`query`]  beantwortet Listen- und Suchanfragen
//! - [`commands`] ist die duenne Bruecke zur Oberflaeche
//!
//! Grundsatz des Projekts: keine Datei wird ohne ausdrueckliche Bestaetigung
//! des Nutzers veraendert, und jede Aktion ist protokolliert.

pub mod classify;
pub mod commands;
pub mod config;
pub mod db;
pub mod dup;
pub mod error;
pub mod fileops;
pub mod fsmeta;
pub mod journal;
mod instance;
mod lifecycle;
pub mod knownfolders;
pub mod license;
pub mod logging;
pub mod recovery;
mod recycle;
pub mod model;
pub mod paths;
pub mod patterns;
pub mod preview;
pub mod query;
pub mod rules;
pub mod scan;
pub mod state;
pub mod undo;
pub mod windowstate;
pub mod watcher;

#[cfg(test)]
mod attacks;
#[cfg(test)]
mod chaos;
#[cfg(test)]
mod crashtests;
#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod perf;
#[cfg(test)]
mod uicheck;

#[cfg(test)]
mod release_hardening;

#[cfg(test)]
mod i18ncheck;
#[cfg(test)]
mod testutil;

use tauri::Manager;

/// Kennung des laufenden Programmstarts.
///
/// Jede Aktion traegt sie, damit sich "alles von heute Nachmittag" als Ganzes
/// zuruecknehmen laesst.
pub fn session_id() -> String {
    use std::sync::OnceLock;
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(journal::new_session_id).clone()
}

/// Startet die Anwendung.
pub fn run() {
    paths::ensure_dirs().ok();
    let db_path = paths::db_path();
    // Der Mutex steht vor jedem Zugriff auf Run-Marker und private Datenbank.
    // Stirbt der Prozess, gibt Windows ihn automatisch frei.
    let _instance_guard = match instance::InstanceGuard::acquire(&db_path) {
        Ok(guard) => guard,
        Err(error) => { instance::show_error(&error); return; }
    };
    let marker_result = lifecycle::RunMarker::begin(paths::run_marker_path(), db_path.clone());
    let (run_marker, recovery_required, marker_failure) = match marker_result {
        Ok((marker, recovery_required)) => (Some(marker), recovery_required, None),
        Err(error) => (None, true, Some(error.to_string())),
    };
    let _log_guard = logging::init();
    tracing::info!("FileTriage {} startet", env!("CARGO_PKG_VERSION"));
    if let Some(error) = marker_failure.as_deref() {
        tracing::error!(
            "Laufzustand konnte nicht dauerhaft markiert werden; persistente DB bleibt geschlossen: {error}"
        );
    }
    let marker_db = std::sync::Arc::new(parking_lot::Mutex::new(None::<db::Db>));
    let marker_db_for_setup = marker_db.clone();

    let run_result = tauri::Builder::default()
        .setup(move |app| {
            // Ein unbrauchbarer Index darf den Start nicht verhindern.
            let (db, index_hinweis) = if marker_failure.is_some() {
                (
                    db::Db::open_memory_for_reading()?,
                    Some(
                        "Der Laufzustand kann nicht dauerhaft gespeichert werden. Scannen, Anzeigen und Diagnose bleiben möglich; Dateioperationen sind gesperrt."
                            .to_string(),
                    ),
                )
            } else {
                db::Db::open_default_resilient(recovery_required)
            };
            if db.has_persistent_journal() {
                *marker_db_for_setup.lock() = Some(db.clone());
            }
            // Der Lizenzcache und der Anker der Kulanzfrist muessen vor dem
            // ersten IPC-Aufruf und vor jeder moeglichen Dateioperation aus
            // genau dieser persistenten Datenbank geladen sein.
            let license_service = license::LicenseService::fuer_kanal();
            // Auch ein Ladefehler ist ein expliziter, fail-closed Zustand und
            // darf Scans/Diagnose nicht am App-Start hindern.
            license_service.mark_load_failed();
            if db.has_persistent_journal() {
                match db.conn() {
                    Ok(conn) => {
                        if let Err(e) = license_service.laden(&conn, db.is_new_install()) {
                            tracing::error!("Lizenzzustand konnte nicht geladen werden: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Lizenzdatenbank konnte nicht geöffnet werden: {e}");
                    }
                }
            }
            license::install(license_service)?;
            // Journal beschneiden: angeboten werden ohnehin nur die letzten 100
            // Aktionen, alles davor ist nur noch Ballast.
            if let Ok(conn) = db.conn() {
                if let Err(e) = undo::prune(&conn, undo::HISTORY_LIMIT) {
                    tracing::warn!("Journal konnte nicht beschnitten werden: {e}");
                }
            }
            let state = state::AppState::new(db)?;
            if index_hinweis.is_some() {
                *state.startup_notice.lock() = Some("journal_unavailable".into());
            }
            app.manage(state);

            // Fenster erst nach dem Wiederherstellen zeigen - sonst springt es
            // beim Start sichtbar an seinen Platz.
            if let Some(window) = app.get_webview_window("main") {
                if let Some(app_state) = app.try_state::<state::AppState>() {
                    if let Ok(conn) = app_state.db.conn() {
                        if let Some(gemerkt) = windowstate::load(&conn) {
                            windowstate::restore(&window, gemerkt);
                        }
                    }
                }
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Beim Schliessen merken, wie das Fenster stand.
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                windowstate::persist(&window.app_handle().clone());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_config,
            commands::save_config,
            commands::pick_folder,
            commands::known_folders,
            commands::recent_roots,
            commands::forget_recent,
            commands::prepare_root,
            commands::list_roots,
            commands::remove_root,
            commands::start_scan,
            commands::cancel_scan,
            commands::scan_status,
            commands::watch_status,
            commands::stop_watch,
            commands::list_files,
            commands::category_breakdown,
            commands::overview,
            commands::move_to_target,
            commands::move_to_folder,
            commands::move_many,
            commands::delete_file,
            commands::delete_many,
            commands::rename_file,
            commands::set_file_state,
            commands::undo_last,
            commands::undo_action,
            commands::undo_batch,
            commands::action_history,
            commands::list_sessions,
            commands::plan_session_undo,
            commands::undo_session,
            commands::current_session,
            commands::recovery_report,
            commands::resolve_recovery,
            commands::find_duplicates,
            commands::cancel_duplicates,
            commands::duplicate_status,
            commands::duplicate_groups,
            commands::list_rules,
            commands::save_rule,
            commands::delete_rule,
            commands::rule_suggestions,
            commands::pattern_suggestions,
            commands::accept_pattern,
            commands::dismiss_pattern,
            commands::pattern_match_count,
            commands::rule_match_counts,
            commands::folder_sizes,
            commands::accept_suggestion,
            commands::dismiss_suggestion,
            commands::dry_run,
            commands::apply_rule,
            commands::suggest_target,
            commands::preview,
            commands::license_state,
            commands::open_purchase,
            commands::open_path,
            commands::reveal_path,
        ])
        .run(tauri::generate_context!());
    if run_result.is_ok() {
        if let (Some(run_marker), Some(db)) =
            (run_marker.as_ref(), marker_db.lock().as_ref().cloned())
        {
            let shutdown = journal::begin_shutdown();
            match shutdown.and_then(|permit| run_marker.mark_clean_if_journal_clear(&db, &permit)) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    "Programm endet mit offenem Journal; Laufzustand bleibt fuer Recovery aktiv"
                ),
                Err(e) => {
                    tracing::error!("Sauberer Programmabschluss konnte nicht markiert werden: {e}");
                }
            }
        }
    }
    run_result.expect("FileTriage konnte nicht gestartet werden");
}



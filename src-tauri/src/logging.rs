//! Lokales Logging mit taeglicher Rotation.
//!
//! Es werden nur Pfade und Fehlermeldungen protokolliert, niemals Dateiinhalte.
//! Die Logs verlassen den Rechner nicht.

use tracing_subscriber::EnvFilter;

use crate::paths;

/// Anzahl aufbewahrter Tagesdateien.
const KEEP_FILES: usize = 7;

/// Muss beim Start gehalten werden, sonst gehen gepufferte Zeilen verloren.
pub struct LogGuard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);

pub fn init() -> Option<LogGuard> {
    if paths::ensure_dirs().is_err() {
        return None;
    }
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("filetriage")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(paths::log_dir())
        .ok()?;

    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("FILETRIAGE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("filetriage=info,warn"));

    let result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init();

    if result.is_err() {
        // Schon initialisiert (z. B. in Tests) - kein Grund zum Abbrechen.
        return None;
    }
    Some(LogGuard(guard))
}

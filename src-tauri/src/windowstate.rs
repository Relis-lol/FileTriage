//! Fensterposition und -groesse merken.
//!
//! Ein normales Windows-Programm oeffnet sich dort, wo man es zuletzt gelassen
//! hat. Zwei Dinge muessen dabei stimmen, sonst ist es schlimmer als gar nichts:
//!
//! 1. Das Fenster darf nicht groesser sein als der Bildschirm.
//! 2. Es darf nicht ausserhalb aller Bildschirme landen. Genau das passiert,
//!    wenn jemand mit zwei Monitoren arbeitet, das Fenster auf den zweiten
//!    schiebt und ihn danach absteckt - das Fenster waere unerreichbar.

use serde::{Deserialize, Serialize};
use tauri::{LogicalPosition, LogicalSize, Manager, WebviewWindow};

use crate::db;
use crate::error::Result;

const KEY: &str = "window_state";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowState {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
    pub maximized: bool,
}

/// Liest den gemerkten Zustand.
pub fn load(conn: &rusqlite::Connection) -> Option<WindowState> {
    let roh = db::get_setting(conn, KEY).ok()??;
    serde_json::from_str(&roh).ok()
}

pub fn save(conn: &rusqlite::Connection, s: &WindowState) -> Result<()> {
    db::set_setting(conn, KEY, &serde_json::to_string(s)?)
}

/// Uebernimmt den gemerkten Zustand ins Fenster - so weit er sinnvoll ist.
pub fn restore(window: &WebviewWindow, s: WindowState) {
    let Ok(monitore) = window.available_monitors() else { return };
    if monitore.is_empty() {
        return;
    }

    // Der gesamte sichtbare Bereich ueber alle Bildschirme.
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    let mut kleinste_breite = f64::MAX;
    let mut kleinste_hoehe = f64::MAX;

    for m in &monitore {
        let skala = m.scale_factor();
        let pos = m.position().to_logical::<f64>(skala);
        let groesse = m.size().to_logical::<f64>(skala);
        min_x = min_x.min(pos.x);
        min_y = min_y.min(pos.y);
        max_x = max_x.max(pos.x + groesse.width);
        max_y = max_y.max(pos.y + groesse.height);
        kleinste_breite = kleinste_breite.min(groesse.width);
        kleinste_hoehe = kleinste_hoehe.min(groesse.height);
    }

    // Nie groesser als der kleinste vorhandene Bildschirm.
    let breite = s.width.clamp(860.0, kleinste_breite.max(860.0));
    let hoehe = s.height.clamp(580.0, kleinste_hoehe.max(580.0));
    let _ = window.set_size(LogicalSize::new(breite, hoehe));

    // Mindestens ein gutes Stueck Titelleiste muss erreichbar bleiben.
    let sichtbar = 120.0;
    let passt = s.x + breite > min_x + sichtbar
        && s.x < max_x - sichtbar
        && s.y + 40.0 > min_y
        && s.y < max_y - sichtbar;

    if passt {
        let _ = window.set_position(LogicalPosition::new(s.x, s.y));
    } else {
        // Der gemerkte Platz gibt es nicht mehr - zurueck in die Mitte.
        tracing::info!("Gemerkte Fensterposition liegt ausserhalb aller Bildschirme");
        let _ = window.center();
    }

    if s.maximized {
        let _ = window.maximize();
    }
}

/// Liest den aktuellen Zustand aus dem Fenster.
pub fn current(window: &WebviewWindow) -> Option<WindowState> {
    let skala = window.scale_factor().ok()?;
    let maximized = window.is_maximized().unwrap_or(false);

    // Im maximierten Zustand die Groesse *vor* dem Maximieren behalten waere
    // aufwendig; Windows merkt sich das selbst, wenn wir maximiert starten.
    let groesse = window.inner_size().ok()?.to_logical::<f64>(skala);
    let pos = window.outer_position().ok()?.to_logical::<f64>(skala);

    Some(WindowState {
        width: groesse.width,
        height: groesse.height,
        x: pos.x,
        y: pos.y,
        maximized,
    })
}

/// Sichert den Fensterzustand. Fehler sind hier belanglos.
pub fn persist(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };
    let Some(zustand) = current(&window) else { return };
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        if let Ok(conn) = state.db.conn() {
            let _ = save(&conn, &zustand);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::TempDir;

    #[test]
    fn zustand_ueberlebt_speichern_und_laden() {
        let dir = TempDir::new("winstate");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        assert!(load(&conn).is_none(), "frisch gibt es nichts zu laden");

        let s = WindowState { width: 1400.0, height: 900.0, x: 120.0, y: 60.0, maximized: false };
        save(&conn, &s).unwrap();

        let geladen = load(&conn).unwrap();
        assert_eq!(geladen.width, 1400.0);
        assert_eq!(geladen.x, 120.0);
        assert!(!geladen.maximized);
    }

    #[test]
    fn beschaedigter_zustand_wird_ignoriert() {
        let dir = TempDir::new("winstate-kaputt");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        db::set_setting(&conn, KEY, "{das ist kein json").unwrap();
        assert!(load(&conn).is_none(), "Muell darf den Start nicht stoeren");
    }
}

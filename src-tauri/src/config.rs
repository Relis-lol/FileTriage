//! Lokale Konfiguration.
//!
//! Liegt als JSON in der `settings`-Tabelle des Index. Fehlende Felder werden
//! durch Standardwerte ersetzt, damit eine aeltere oder beschaedigte
//! Konfiguration die Anwendung nicht blockiert.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::{AppError, Result};

const CONFIG_KEY: &str = "config";

/// Ein Zielordner, erreichbar ueber die Zifferntaste `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetFolder {
    /// "1" bis "9".
    pub key: String,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanSettings {
    pub recursive: bool,
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    /// Dateien unterhalb dieser Groesse werden nicht indexiert (0 = alle).
    pub min_size_bytes: u64,
    /// Ordnernamen, die nie betreten werden.
    pub skip_dirs: Vec<String>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        ScanSettings {
            recursive: true,
            include_hidden: false,
            // Symlinks/Junctions nicht verfolgen: sonst drohen Endlosschleifen
            // und Aktionen ausserhalb des gewaehlten Ordners.
            follow_symlinks: false,
            min_size_bytes: 0,
            skip_dirs: vec![
                "$RECYCLE.BIN".into(),
                "System Volume Information".into(),
                "node_modules".into(),
                ".git".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// Standard: in den Windows-Papierkorb.
    Recycle,
    /// Expertenoption, muss ausdruecklich eingeschaltet werden.
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// "system" | "dark" | "light"
    pub theme: String,
    pub targets: Vec<TargetFolder>,
    /// Aktion -> Taste, z. B. "delete" -> "d".
    pub hotkeys: BTreeMap<String, String>,
    pub delete_mode: DeleteMode,
    /// Zeitpunkt, zu dem der Nutzer das endgueltige Loeschen ausdruecklich
    /// bestaetigt hat.
    ///
    /// Zwei Felder statt einem, mit Absicht: Ein einzelnes umgekipptes Feld -
    /// durch eine beschaedigte Datei, eine fremde Konfiguration oder einen
    /// Fehler beim Zusammenfuehren - wuerde sonst genuegen, damit Loeschen
    /// unwiderruflich wird. Fehlt die Bestaetigung, gilt der Papierkorb.
    pub permanent_delete_confirmed_at: Option<i64>,
    pub scan: ScanSettings,
    pub previews_enabled: bool,
    /// Groessengrenze fuer Vorschauen; darueber wird nichts gelesen.
    pub preview_max_bytes: u64,
    pub large_file_mb: u64,
    pub old_file_days: u32,
    pub suggestions_enabled: bool,
    /// Ab wie vielen gleichartigen Verschiebungen ein Regelvorschlag kommt.
    pub suggestion_threshold: u32,
    pub onboarding_done: bool,
    pub last_root: Option<String>,
    /// Zuletzt aufgeraeumte Ordner, neueste zuerst. Rein lokal, jederzeit
    /// loeschbar, und **kein** Anlass, beim Start irgendetwas zu scannen.
    pub recent_roots: Vec<String>,
    /// Wurden die Standardziele aus den Windows-Ordnern schon einmal gesetzt?
    ///
    /// Getrennt von `targets`, damit ein Nutzer, der alle Ziele bewusst
    /// entfernt, sie nicht beim naechsten Start wiederbekommt.
    pub defaults_applied: bool,
    /// Standardmaessig aus. Es gibt in Version 0.1 ohnehin keinen Sender.
    pub telemetry_opt_in: bool,
    /// Sprachkennung, etwa "de" oder "pt-BR". `None` heisst: die Sprache des
    /// Systems verwenden. Bewusst nicht "en" als Vorgabe - wer nichts
    /// einstellt, soll die Sprache bekommen, die auf seinem Windows steht.
    #[serde(default)]
    pub language: Option<String>,
}

/// So viele zuletzt verwendete Ordner werden gemerkt.
pub const RECENT_LIMIT: usize = 6;

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "system".into(),
            targets: Vec::new(),
            hotkeys: default_hotkeys(),
            delete_mode: DeleteMode::Recycle,
            permanent_delete_confirmed_at: None,
            scan: ScanSettings::default(),
            previews_enabled: true,
            preview_max_bytes: 64 * 1024 * 1024,
            large_file_mb: 100,
            old_file_days: 365,
            suggestions_enabled: true,
            suggestion_threshold: 3,
            onboarding_done: false,
            last_root: None,
            recent_roots: Vec::new(),
            defaults_applied: false,
            telemetry_opt_in: false,
            language: None,
        }
    }
}

pub fn default_hotkeys() -> BTreeMap<String, String> {
    let pairs = [
        ("target1", "1"),
        ("target2", "2"),
        ("target3", "3"),
        ("target4", "4"),
        ("target5", "5"),
        ("target6", "6"),
        ("target7", "7"),
        ("target8", "8"),
        ("target9", "9"),
        ("delete", "d"),
        ("skip", "s"),
        ("keep", "k"),
        ("open", "o"),
        ("explorer", "e"),
        ("rename", "r"),
        ("undo", "Backspace"),
        ("preview", " "),
        ("confirm", "Enter"),
        ("next", "ArrowRight"),
        ("prev", "ArrowLeft"),
    ];
    pairs.iter().map(|(a, k)| ((*a).to_string(), (*k).to_string())).collect()
}

impl Config {
    pub fn load(conn: &Connection) -> Result<Config> {
        match db::get_setting(conn, CONFIG_KEY)? {
            Some(json) => match serde_json::from_str::<Config>(&json) {
                Ok(cfg) => Ok(cfg),
                Err(e) => {
                    // Kaputte Konfiguration darf den Start nicht verhindern.
                    tracing::warn!("Konfiguration unlesbar, nutze Standardwerte: {e}");
                    Ok(Config::default())
                }
            },
            None => Ok(Config::default()),
        }
    }

    pub fn save(&self, conn: &Connection) -> Result<()> {
        self.validate()?;
        db::set_setting(conn, CONFIG_KEY, &serde_json::to_string(self)?)
    }

    /// Prueft die Werte, bevor sie gespeichert werden. Ein ungueltiger
    /// Zielordner ist gefaehrlich, deshalb hier hart abweisen.
    pub fn validate(&self) -> Result<()> {
        for t in &self.targets {
            if t.path.trim().is_empty() {
                return Err(AppError::Invalid(format!(
                    "Zielordner \"{}\" hat keinen Pfad",
                    t.label
                )));
            }
            // Dieselbe Pruefung wie fuer jeden anderen Zielordner. Zwei
            // unterschiedlich strenge Pruefungen fuer denselben Zweck sind
            // genau die Luecke, durch die ein Pfad mit ".." schluepft.
            crate::paths::validate_directory_path(Path::new(&t.path)).map_err(|e| {
                AppError::Invalid(format!("Zielordner \"{}\": {e}", t.label))
            })?;
        }
        let mut keys: Vec<&str> = self.targets.iter().map(|t| t.key.as_str()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        if keys.len() != before {
            return Err(AppError::Invalid("Eine Zifferntaste ist doppelt belegt".into()));
        }
        if self.suggestion_threshold == 0 {
            return Err(AppError::Invalid("Schwelle für Vorschlaege muss > 0 sein".into()));
        }
        // Endgueltiges Loeschen laesst sich nicht durch blosses Setzen des
        // Modus einschalten - die Bestaetigung gehoert dazu.
        if self.delete_mode == DeleteMode::Permanent
            && self.permanent_delete_confirmed_at.is_none_or(|ts| ts <= 0)
        {
            return Err(AppError::Invalid(
                "Endgültiges Löschen muss ausdrücklich bestätigt werden".into(),
            ));
        }
        Ok(())
    }

    pub fn target_for_key(&self, key: &str) -> Option<&TargetFolder> {
        self.targets.iter().find(|t| t.key == key)
    }

    /// Setzt einmalig brauchbare Zielordner aus den Windows-Ordnern.
    ///
    /// Der Sinn: Nach der Installation soll sofort etwas Nuetzliches passieren
    /// koennen. Wer nichts einrichtet, hat trotzdem Dokumente, Bilder, Videos,
    /// Musik und Desktop auf den Zifferntasten. Wer alles wieder entfernt,
    /// bekommt es nicht zurueckgedraengt - deshalb das eigene Merkfeld.
    ///
    /// Liefert `true`, wenn etwas gesetzt wurde.
    pub fn apply_default_targets(&mut self, quelle: Option<&str>) -> bool {
        if self.defaults_applied || !self.targets.is_empty() {
            self.defaults_applied = true;
            return false;
        }
        let ziele = crate::knownfolders::standard_ziele(quelle);
        if ziele.is_empty() {
            // Keine bekannten Ordner gefunden - dann bleibt es leer, und die
            // Triage funktioniert trotzdem (ueberspringen, Papierkorb, oeffnen).
            self.defaults_applied = true;
            return false;
        }
        self.targets = ziele
            .into_iter()
            .take(9)
            .enumerate()
            .map(|(i, o)| TargetFolder {
                key: (i + 1).to_string(),
                label: o.label,
                path: o.path,
            })
            .collect();
        self.defaults_applied = true;
        true
    }

    /// Merkt sich einen Ordner als zuletzt verwendet.
    pub fn push_recent(&mut self, root: &str) {
        let key = crate::paths::path_key(root);
        self.recent_roots.retain(|r| crate::paths::path_key(r) != key);
        self.recent_roots.insert(0, root.to_string());
        self.recent_roots.truncate(RECENT_LIMIT);
    }

    pub fn remove_recent(&mut self, root: &str) {
        let key = crate::paths::path_key(root);
        self.recent_roots.retain(|r| crate::paths::path_key(r) != key);
    }

    /// Das tatsaechlich geltende Loeschverhalten.
    ///
    /// **Nur diese Methode darf entscheiden, ob endgueltig geloescht wird.**
    /// Ein direkt gelesenes `delete_mode` waere die Stelle, an der eine
    /// beschaedigte Konfiguration zum Datenverlust wird.
    pub fn effective_delete_mode(&self) -> DeleteMode {
        match (self.delete_mode, self.permanent_delete_confirmed_at) {
            (DeleteMode::Permanent, Some(ts)) if ts > 0 => DeleteMode::Permanent,
            (DeleteMode::Permanent, _) => {
                tracing::warn!(
                    "Endgueltiges Loeschen ist eingestellt, aber nicht bestaetigt - \
                     es wird der Papierkorb verwendet."
                );
                DeleteMode::Recycle
            }
            _ => DeleteMode::Recycle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::TempDir;

    fn conn() -> (TempDir, Connection) {
        let dir = TempDir::new("cfg");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let c = db.conn().unwrap();
        (dir, c)
    }

    #[test]
    fn standardwerte_sind_sicher() {
        let c = Config::default();
        assert_eq!(c.delete_mode, DeleteMode::Recycle);
        assert!(!c.telemetry_opt_in);
        assert!(!c.scan.follow_symlinks);
        assert!(!c.onboarding_done);
    }

    #[test]
    fn speichern_und_laden_erhaelt_werte() {
        let (_d, c) = conn();
        let mut cfg = Config::default();
        cfg.theme = "dark".into();
        cfg.targets.push(TargetFolder {
            key: "1".into(),
            label: "Rechnungen".into(),
            path: "C:\\Users\\Test\\Documents".into(),
        });
        cfg.save(&c).unwrap();

        let geladen = Config::load(&c).unwrap();
        assert_eq!(geladen.theme, "dark");
        assert_eq!(geladen.target_for_key("1").unwrap().label, "Rechnungen");
    }

    #[test]
    fn kaputte_konfiguration_faellt_auf_standard_zurueck() {
        let (_d, c) = conn();
        db::set_setting(&c, CONFIG_KEY, "{kein json").unwrap();
        assert_eq!(Config::load(&c).unwrap().theme, "system");
    }

    /// Eine beschaedigte oder fremde Konfiguration darf den Papierkorb nicht
    /// umgehen koennen, indem sie einfach den Modus umstellt.
    #[test]
    fn endgueltiges_loeschen_gilt_nur_mit_bestaetigung() {
        let mut cfg = Config::default();
        cfg.delete_mode = DeleteMode::Permanent;
        cfg.permanent_delete_confirmed_at = None;

        assert_eq!(
            cfg.effective_delete_mode(),
            DeleteMode::Recycle,
            "ohne Bestätigung bleibt es beim Papierkorb"
        );
        assert!(cfg.validate().is_err(), "so darf die Einstellung gar nicht gespeichert werden");

        cfg.permanent_delete_confirmed_at = Some(0);
        assert_eq!(cfg.effective_delete_mode(), DeleteMode::Recycle);

        cfg.permanent_delete_confirmed_at = Some(db::now_ts());
        assert_eq!(cfg.effective_delete_mode(), DeleteMode::Permanent);
        assert!(cfg.validate().is_ok());
    }

    /// Der Angriff: Jemand schiebt eine praeparierte Konfiguration in die
    /// Datenbank, die endgueltiges Loeschen einschaltet.
    #[test]
    fn untergeschobene_konfiguration_schaltet_endgueltiges_loeschen_nicht_ein() {
        let (_d, c) = conn();
        db::set_setting(
            &c,
            CONFIG_KEY,
            r#"{"delete_mode":"permanent","theme":"dark"}"#,
        )
        .unwrap();

        let geladen = Config::load(&c).unwrap();
        assert_eq!(geladen.delete_mode, DeleteMode::Permanent);
        assert_eq!(
            geladen.effective_delete_mode(),
            DeleteMode::Recycle,
            "die fehlende Bestätigung schuetzt vor der untergeschobenen Einstellung"
        );
    }

    /// Der wichtigste Test dieser Datei fuer den Erststart: Eine frische
    /// Konfiguration muss ohne jedes Zutun benutzbare Ziele haben.
    #[test]
    fn erster_start_hat_sofort_brauchbare_ziele() {
        let mut cfg = Config::default();
        assert!(cfg.targets.is_empty(), "frisch ist frisch");

        let gesetzt = cfg.apply_default_targets(None);
        assert!(gesetzt, "auf Windows muss mindestens ein bekannter Ordner da sein");
        assert!(!cfg.targets.is_empty());
        assert!(cfg.targets.len() <= 9);
        assert!(cfg.validate().is_ok(), "die Standardziele müssen gueltig sein");

        // Tasten luecklos ab 1.
        let tasten: Vec<&str> = cfg.targets.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(tasten[0], "1");
        for (i, t) in tasten.iter().enumerate() {
            assert_eq!(*t, (i + 1).to_string());
        }
        for t in &cfg.targets {
            assert!(std::path::Path::new(&t.path).is_dir(), "{} existiert nicht", t.path);
            assert!(!t.label.is_empty());
        }
    }

    #[test]
    fn standardziele_werden_nur_einmal_gesetzt() {
        let mut cfg = Config::default();
        cfg.apply_default_targets(None);
        let vorher = cfg.targets.len();

        // Der Nutzer raeumt bewusst alles weg.
        cfg.targets.clear();
        let nochmal = cfg.apply_default_targets(None);
        assert!(!nochmal, "einmal entfernte Ziele draengen sich nicht wieder auf");
        assert!(cfg.targets.is_empty());
        assert!(vorher > 0);
    }

    #[test]
    fn eigene_ziele_werden_nicht_ueberschrieben() {
        let mut cfg = Config::default();
        cfg.targets.push(TargetFolder {
            key: "1".into(),
            label: "Meine Rechnungen".into(),
            path: "C:\\Eigenes".into(),
        });
        assert!(!cfg.apply_default_targets(None));
        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets[0].label, "Meine Rechnungen");
    }

    #[test]
    fn zuletzt_verwendete_ordner_bleiben_uebersichtlich() {
        let mut cfg = Config::default();
        for i in 0..10 {
            cfg.push_recent(&format!("C:\\Ordner{i}"));
        }
        assert_eq!(cfg.recent_roots.len(), RECENT_LIMIT);
        assert_eq!(cfg.recent_roots[0], "C:\\Ordner9", "neuester zuerst");

        // Erneutes Verwenden schiebt nach vorn statt zu doppeln.
        cfg.push_recent("C:\\Ordner5");
        assert_eq!(cfg.recent_roots[0], "C:\\Ordner5");
        assert_eq!(
            cfg.recent_roots.iter().filter(|r| r.contains("Ordner5")).count(),
            1
        );

        // Gross-/Kleinschreibung ist derselbe Ordner.
        cfg.push_recent("c:\\ordner5");
        assert_eq!(
            cfg.recent_roots.iter().filter(|r| r.to_lowercase().contains("ordner5")).count(),
            1
        );

        cfg.remove_recent("C:\\ORDNER5");
        assert!(!cfg.recent_roots.iter().any(|r| r.to_lowercase().contains("ordner5")));
    }

    #[test]
    fn relative_zielpfade_werden_abgelehnt() {
        let mut cfg = Config::default();
        cfg.targets.push(TargetFolder {
            key: "1".into(),
            label: "X".into(),
            path: "..\\woanders".into(),
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn doppelte_tastenbelegung_wird_abgelehnt() {
        let mut cfg = Config::default();
        for label in ["A", "B"] {
            cfg.targets.push(TargetFolder {
                key: "1".into(),
                label: label.into(),
                path: "C:\\Ziel".into(),
            });
        }
        assert!(cfg.validate().is_err());
    }
}

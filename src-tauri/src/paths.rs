//! Ablageorte der Anwendung und Pfad-Hilfsfunktionen fuer Windows.
//!
//! Lange Pfade: Windows-APIs brechen ohne das Praefix `\\?\` bei 260 Zeichen ab.
//! [`extended`] setzt es fuer absolute Laufwerkspfade, sobald es eng wird.

use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

/// Ueberschreibbar fuer Tests, damit niemals in echte Nutzerdaten geschrieben wird.
pub const DATA_DIR_ENV: &str = "FILETRIAGE_DATA_DIR";

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(DATA_DIR_ENV) {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    crate::knownfolders::local_app_data()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FileTriage")
}

pub fn db_path() -> PathBuf {
    data_dir().join("index.db")
}

pub fn run_marker_path() -> PathBuf {
    data_dir().join("run-state")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn ensure_dirs() -> Result<()> {
    let data = data_dir();
    std::fs::create_dir_all(&data)
        .map_err(|e| AppError::from_io(&e, &format!("Datenordner {}", data.display())))?;
    let logs = log_dir();
    std::fs::create_dir_all(&logs)
        .map_err(|e| AppError::from_io(&e, &format!("Logordner {}", logs.display())))?;
    Ok(())
}

/// Fuegt bei Bedarf das Verbatim-Praefix an, damit lange Pfade funktionieren.
pub fn extended(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if s.starts_with("\\\\?\\") || s.len() < 240 || !path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("\\\\") {
        // UNC: \\server\share -> \\?\UNC\server\share
        return PathBuf::from(format!("\\\\?\\UNC\\{rest}"));
    }
    PathBuf::from(format!("\\\\?\\{s}"))
}

/// Entfernt das Verbatim-Praefix wieder - fuer Anzeige und Speicherung.
pub fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        return format!("\\\\{rest}");
    }
    s.strip_prefix("\\\\?\\").map(str::to_string).unwrap_or(s)
}

/// Schluessel fuer Eindeutigkeit: Windows-Pfade sind praktisch
/// gross-/kleinschreibungsunabhaengig, Trennzeichen werden vereinheitlicht.
pub fn path_key(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Unter Windows reservierte Geraetenamen. Eine Datei so zu benennen ist
/// entweder unmoeglich oder fuehrt zu Zugriffen auf ein Geraet statt auf eine
/// Datei - beides nichts, was FileTriage erzeugen sollte.
const RESERVIERTE_NAMEN: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Prueft einen Ordnerpfad, bevor er als Ziel verwendet wird.
///
/// Abgelehnt werden relative Pfade und alles mit `..`. Beides koennte aus einer
/// Konfiguration oder einer importierten Regel stammen und aus dem gemeinten
/// Bereich herausfuehren - der klassische Weg, aus "sortiere nach Dokumente"
/// ein "schreibe nach Windows\System32" zu machen.
pub fn validate_directory_path(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() {
        return Err(AppError::Invalid("Kein Zielordner angegeben".into()));
    }
    if !dir.is_absolute() {
        return Err(AppError::Invalid(format!(
            "Der Zielordner braucht einen absoluten Pfad: {}",
            dir.display()
        )));
    }
    for teil in dir.components() {
        if matches!(teil, std::path::Component::ParentDir) {
            return Err(AppError::Invalid(format!(
                "Pfade mit \"..\" werden nicht akzeptiert: {}",
                dir.display()
            )));
        }
    }
    // Auch die Textform pruefen: `Path::components` normalisiert "a/../b"
    // nicht weg, aber ein Pfad aus einer fremden Quelle kann Formen enthalten,
    // die erst die Windows-API aufloest.
    let text = display_path(dir);
    if text.split(['\\', '/']).any(|t| t == "..") {
        return Err(AppError::Invalid(format!(
            "Pfade mit \"..\" werden nicht akzeptiert: {text}"
        )));
    }
    Ok(())
}

/// Prueft einen neuen Dateinamen und gibt ihn bereinigt zurueck.
pub fn validate_file_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("Der neue Name ist leer".into()));
    }
    if name == "." || name == ".." {
        return Err(AppError::Invalid("Dieser Name ist nicht zulässig".into()));
    }
    if name.contains(['\\', '/', ':']) {
        return Err(AppError::Invalid(
            "Der Name darf keine Pfadtrenner enthalten".into(),
        ));
    }
    if name.contains(['<', '>', '"', '|', '?', '*']) {
        return Err(AppError::Invalid(
            "Der Name enthält unter Windows unzulässige Zeichen".into(),
        ));
    }
    if name.chars().any(|c| (c as u32) < 0x20) {
        return Err(AppError::Invalid(
            "Der Name enthält Steuerzeichen".into(),
        ));
    }
    // Windows entfernt einen abschliessenden Punkt stillschweigend - die Datei
    // hiesse danach anders, als der Nutzer eingegeben hat. Umgebende
    // Leerzeichen werden dagegen bewusst abgeschnitten (siehe `trim` oben):
    // Das betrifft die Eingabe, nicht den gemeinten Namen.
    if name.ends_with('.') {
        return Err(AppError::Invalid("Der Name darf nicht mit einem Punkt enden".into()));
    }
    let stamm = name.split('.').next().unwrap_or(name).to_lowercase();
    if RESERVIERTE_NAMEN.contains(&stamm.as_str()) {
        return Err(AppError::Invalid(format!(
            "\"{stamm}\" ist unter Windows ein reservierter Gerätename"
        )));
    }
    if name.len() > 255 {
        return Err(AppError::Invalid("Der Name ist zu lang".into()));
    }
    Ok(name.to_string())
}

/// Vergleichsschluessel, der auch 8.3-Kurznamen und Junctions aufloest.
///
/// Windows liefert denselben Ordner je nach Quelle als `C:\Users\BENUTZ~1\...`
/// oder ausgeschrieben. Fuer Vergleiche (etwa mit Eintraegen des Papierkorbs)
/// muss deshalb kanonisiert werden. Existiert der Pfad nicht mehr, bleibt der
/// einfache Schluessel als Rueckfall.
pub fn canonical_key(path: &Path) -> String {
    match std::fs::canonicalize(extended(path)) {
        Ok(p) => path_key(&display_path(&p)),
        Err(_) => path_key(&display_path(path)),
    }
}

/// Prueft, ob `child` innerhalb von `parent` liegt (Schutz gegen Verschieben
/// eines Ordners in sich selbst).
pub fn is_inside(child: &Path, parent: &Path) -> bool {
    let c = path_key(&display_path(child));
    let p = path_key(&display_path(parent));
    c == p || c.starts_with(&format!("{p}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_key_normalisiert_trenner_und_schreibweise() {
        assert_eq!(path_key("C:/Users/Test\\"), "c:\\users\\test");
        assert_eq!(path_key("C:\\Users\\TEST"), path_key("c:\\users\\test"));
    }

    #[test]
    fn extended_nur_bei_langen_absoluten_pfaden() {
        let kurz = PathBuf::from("C:\\Temp\\a.txt");
        assert_eq!(extended(&kurz), kurz);

        let lang = PathBuf::from(format!("C:\\{}\\datei.txt", "x".repeat(250)));
        assert!(extended(&lang).to_string_lossy().starts_with("\\\\?\\C:"));
    }

    #[test]
    fn display_path_entfernt_praefix() {
        assert_eq!(display_path(Path::new("\\\\?\\C:\\a")), "C:\\a");
        assert_eq!(display_path(Path::new("\\\\?\\UNC\\srv\\f")), "\\\\srv\\f");
    }

    #[test]
    fn is_inside_erkennt_unterordner_aber_keine_namenspraefixe() {
        assert!(is_inside(Path::new("C:\\a\\b"), Path::new("C:\\a")));
        assert!(!is_inside(Path::new("C:\\ab"), Path::new("C:\\a")));
    }

    #[test]
    fn zielordner_mit_punktpunkt_wird_abgelehnt() {
        assert!(validate_directory_path(Path::new("C:\\a\\..\\b")).is_err());
        assert!(validate_directory_path(Path::new("C:\\a\\b\\..")).is_err());
        assert!(validate_directory_path(Path::new("..\\b")).is_err());
        assert!(validate_directory_path(Path::new("relativ\\pfad")).is_err());
        assert!(validate_directory_path(Path::new("")).is_err());
        assert!(validate_directory_path(Path::new("C:\\a\\b")).is_ok());
        assert!(validate_directory_path(Path::new("\\\\server\\share\\ordner")).is_ok());
    }

    #[test]
    fn dateiname_wird_streng_geprueft() {
        assert!(validate_file_name("Bericht.pdf").is_ok());
        assert!(validate_file_name("Ünïcödé — 🚀.txt").is_ok());
        // Umgebende Leerzeichen gehoeren zur Eingabe, nicht zum Namen.
        assert_eq!(validate_file_name("  Rand.txt  ").unwrap(), "Rand.txt");

        for boese in [
            "",
            "   ",
            ".",
            "..",
            "..\\woanders.txt",
            "unter/ordner.txt",
            "C:datei.txt",
            "frage?.txt",
            "stern*.txt",
            "pipe|.txt",
            "endet.",
            "endet .",
            "con",
            "CON.txt",
            "nul.log",
            "lpt1.dat",
        ] {
            assert!(
                validate_file_name(boese).is_err(),
                "\"{boese}\" haette abgelehnt werden müssen"
            );
        }
        assert!(validate_file_name(&"a".repeat(300)).is_err());
        assert!(validate_file_name("datei\u{0007}.txt").is_err());
    }
}

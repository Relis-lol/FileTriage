//! Hilfen fuer Tests.
//!
//! Wichtig: Tests arbeiten ausschliesslich in einem eigenen Ordner unterhalb von
//! `%TEMP%` und fassen niemals echte Nutzerdaten an.

#![cfg(test)]

use std::path::{Path, PathBuf};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> TempDir {
        let unique = format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join("filetriage-tests").join(unique);
        std::fs::create_dir_all(&dir).expect("Testordner anlegen");
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Legt eine Datei mit Inhalt an und liefert ihren Pfad.
    pub fn file(&self, rel: &str, content: &[u8]) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("Unterordner anlegen");
        }
        std::fs::write(&p, content).expect("Testdatei schreiben");
        p
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).expect("Testordner anlegen");
        p
    }

    /// Legt eine Datei bestimmter Groesse mit wiederholbarem Inhalt an.
    pub fn sized_file(&self, rel: &str, bytes: usize, fuellwert: u8) -> PathBuf {
        self.file(rel, &vec![fuellwert; bytes])
    }
}

/// Verzeichnis-Junction (`mklink /J`). Braucht keine erhoehten Rechte.
///
/// Liefert `false`, wenn das System sie nicht anlegen konnte - der Test soll
/// dann uebersprungen und nicht als Fehlschlag gewertet werden.
pub fn make_junction(link: &Path, ziel: &Path) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J", &link.display().to_string(), &ziel.display().to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Datei-Symlink (`mklink`). Braucht Entwicklermodus oder Administratorrechte.
pub fn make_file_symlink(link: &Path, ziel: &Path) -> bool {
    std::os::windows::fs::symlink_file(ziel, link).is_ok()
}

/// Verzeichnis-Symlink (`mklink /D`).
pub fn make_dir_symlink(link: &Path, ziel: &Path) -> bool {
    std::os::windows::fs::symlink_dir(ziel, link).is_ok()
}

/// NTFS-Hardlink.
pub fn make_hardlink(link: &Path, ziel: &Path) -> bool {
    std::fs::hard_link(ziel, link).is_ok()
}

/// Schreibt einen Alternate Data Stream (`datei.txt:stream`).
pub fn write_ads(datei: &Path, stream: &str, inhalt: &[u8]) -> bool {
    let pfad = format!("{}:{stream}", datei.display());
    std::fs::write(pfad, inhalt).is_ok()
}

pub fn read_ads(datei: &Path, stream: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("{}:{stream}", datei.display())).ok()
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Aufraeumen darf fehlschlagen (gesperrte Datei) - das ist kein Testfehler.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

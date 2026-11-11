//! Datentypen, die zwischen Backend, Datenbank und Oberflaeche geteilt werden.

use serde::{Deserialize, Serialize};

/// Grobe Dateikategorie - rein deterministisch aus Endung/Name abgeleitet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Installer,
    Archive,
    Image,
    Video,
    Audio,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Code,
    Torrent,
    Temp,
    Font,
    Ebook,
    DiskImage,
    Shortcut,
    Unknown,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Installer => "installer",
            Category::Archive => "archive",
            Category::Image => "image",
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Pdf => "pdf",
            Category::Document => "document",
            Category::Spreadsheet => "spreadsheet",
            Category::Presentation => "presentation",
            Category::Code => "code",
            Category::Torrent => "torrent",
            Category::Temp => "temp",
            Category::Font => "font",
            Category::Ebook => "ebook",
            Category::DiskImage => "disk_image",
            Category::Shortcut => "shortcut",
            Category::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Category {
        match s {
            "installer" => Category::Installer,
            "archive" => Category::Archive,
            "image" => Category::Image,
            "video" => Category::Video,
            "audio" => Category::Audio,
            "pdf" => Category::Pdf,
            "document" => Category::Document,
            "spreadsheet" => Category::Spreadsheet,
            "presentation" => Category::Presentation,
            "code" => Category::Code,
            "torrent" => Category::Torrent,
            "temp" => Category::Temp,
            "font" => Category::Font,
            "ebook" => Category::Ebook,
            "disk_image" => Category::DiskImage,
            "shortcut" => Category::Shortcut,
            _ => Category::Unknown,
        }
    }
}

/// Bearbeitungsstand einer Datei in der Triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageState {
    /// Noch nicht angesehen.
    Pending,
    /// Bewusst uebersprungen - taucht in der Standardansicht nicht mehr auf.
    Skipped,
    /// Verschoben.
    Moved,
    /// In den Papierkorb gelegt (oder endgueltig geloescht).
    Deleted,
    /// Als "gehoert hierher" markiert.
    Kept,
}

impl TriageState {
    pub fn as_str(self) -> &'static str {
        match self {
            TriageState::Pending => "pending",
            TriageState::Skipped => "skipped",
            TriageState::Moved => "moved",
            TriageState::Deleted => "deleted",
            TriageState::Kept => "kept",
        }
    }
}

/// Ein indexierter Dateieintrag, so wie ihn die Oberflaeche sieht.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: i64,
    pub path: String,
    pub parent: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    /// Unix-Sekunden; 0 wenn das Dateisystem den Zeitstempel nicht liefert.
    pub created: i64,
    pub modified: i64,
    pub category: String,
    pub state: String,
}

/// Zaehlwerte fuer die Startseite.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RootStats {
    pub files: i64,
    pub total_size: i64,
    pub pending: i64,
    pub large_files: i64,
    pub old_files: i64,
    pub last_scan: Option<i64>,
}

/// Fortschrittsmeldung waehrend eines Scans.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanProgress {
    pub root: String,
    pub files: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current: String,
    pub done: bool,
    pub cancelled: bool,
    pub removed: u64,
    pub elapsed_ms: u64,
}

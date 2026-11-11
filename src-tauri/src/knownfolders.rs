//! Windows Known Folders.
//!
//! Ein Endnutzerprogramm darf nicht raten, wo "Dokumente" liegt, und schon gar
//! nicht einen Pfad aus dem Benutzernamen zusammenbauen. Der Ordner kann
//! umgeleitet, umbenannt, auf einem anderen Laufwerk oder in OneDrive sein.
//! Windows beantwortet die Frage selbst - ueber `SHGetKnownFolderPath`.
//!
//! Ergebnis: FileTriage kann beim ersten Start sinnvolle Ziele anbieten, ohne
//! dass der Nutzer irgendetwas einrichtet.

use std::path::PathBuf;

use serde::Serialize;

/// GUID-Layout, wie es die Windows-API erwartet.
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

// Die GUIDs der Ordner, die wir anbieten. Quelle: KnownFolders.h.
const DOWNLOADS: Guid = Guid {
    data1: 0x374D_E290,
    data2: 0x123F,
    data3: 0x4565,
    data4: [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
};
const DOCUMENTS: Guid = Guid {
    data1: 0xFDD3_9AD0,
    data2: 0x238F,
    data3: 0x46AF,
    data4: [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
};
const PICTURES: Guid = Guid {
    data1: 0x33E2_8130,
    data2: 0x4E1E,
    data3: 0x4676,
    data4: [0x83, 0x5A, 0x98, 0x39, 0x5C, 0x3B, 0xC3, 0xBB],
};
const VIDEOS: Guid = Guid {
    data1: 0x1898_9B1D,
    data2: 0x99B5,
    data3: 0x455B,
    data4: [0x84, 0x1C, 0xAB, 0x7C, 0x74, 0xE4, 0xDD, 0xFC],
};
const MUSIC: Guid = Guid {
    data1: 0x4BD8_D571,
    data2: 0x6D19,
    data3: 0x48D3,
    data4: [0xBE, 0x97, 0x42, 0x22, 0x20, 0x08, 0x0E, 0x43],
};
const DESKTOP: Guid = Guid {
    data1: 0xB4BF_CC3A,
    data2: 0xDB2C,
    data3: 0x424C,
    data4: [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41],
};

/// `%LOCALAPPDATA%`. Kein Ziel fuer die Triage - hier liegen Index und
/// Protokolle.
const LOCAL_APP_DATA: Guid = Guid {
    data1: 0xF1B3_2785,
    data2: 0x6FBA,
    data3: 0x4FCF,
    data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
};

/// Der Ordner, in dem die Anwendung ihre eigenen Daten ablegt.
///
/// Frueher kam dieser Pfad aus der Kiste `dirs` - fuer genau diesen einen
/// Aufruf, den Windows selbst beantwortet und fuer den das FFI hier ohnehin
/// schon stand.
///
/// Der Anlass war die Lizenzpruefung: `dirs` zieht `option-ext` nach, und das
/// steht unter MPL-2.0. Die direkte Abhaengigkeit ist damit weg - **die
/// Lizenzpflicht aber nicht**, denn Tauri selbst zieht `dirs` ebenfalls.
/// Gewonnen ist eine Abhaengigkeit weniger, nicht ein Lizenzproblem weniger.
pub fn local_app_data() -> Option<PathBuf> {
    pfad_zu(LOCAL_APP_DATA)
}

/// Welche bekannten Ordner FileTriage kennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownFolder {
    Downloads,
    Documents,
    Pictures,
    Videos,
    Music,
    Desktop,
}

impl KnownFolder {
    fn guid(self) -> Guid {
        match self {
            KnownFolder::Downloads => DOWNLOADS,
            KnownFolder::Documents => DOCUMENTS,
            KnownFolder::Pictures => PICTURES,
            KnownFolder::Videos => VIDEOS,
            KnownFolder::Music => MUSIC,
            KnownFolder::Desktop => DESKTOP,
        }
    }

    /// Bezeichner fuer die Oberflaeche - dort wird daraus Beschriftung und Symbol.
    pub fn key(self) -> &'static str {
        match self {
            KnownFolder::Downloads => "downloads",
            KnownFolder::Documents => "documents",
            KnownFolder::Pictures => "pictures",
            KnownFolder::Videos => "videos",
            KnownFolder::Music => "music",
            KnownFolder::Desktop => "desktop",
        }
    }

    /// Deutsche Beschriftung. Windows liefert lokalisierte Anzeigenamen nur
    /// ueber weitere Shell-Aufrufe; fuer sechs feste Ordner waere das mehr
    /// Aufwand als Nutzen.
    pub fn label(self) -> &'static str {
        match self {
            KnownFolder::Downloads => "Downloads",
            KnownFolder::Documents => "Dokumente",
            KnownFolder::Pictures => "Bilder",
            KnownFolder::Videos => "Videos",
            KnownFolder::Music => "Musik",
            KnownFolder::Desktop => "Desktop",
        }
    }
}

/// Alle Ordner in der Reihenfolge, in der sie angeboten werden.
pub const ALLE: [KnownFolder; 6] = [
    KnownFolder::Downloads,
    KnownFolder::Desktop,
    KnownFolder::Documents,
    KnownFolder::Pictures,
    KnownFolder::Videos,
    KnownFolder::Music,
];

/// Fragt Windows nach dem Pfad. `None`, wenn der Ordner nicht existiert oder
/// nicht erreichbar ist - dann wird er schlicht nicht angeboten.
#[cfg(windows)]
pub fn path_of(folder: KnownFolder) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const Guid,
            dwFlags: u32,
            hToken: isize,
            ppszPath: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pv: *mut core::ffi::c_void);
    }

    pfad_zu(folder.guid())
}

/// Der eigentliche Aufruf. Getrennt von [`path_of`], damit auch Ordner
/// abgefragt werden koennen, die nicht in [`KnownFolder`] stehen.
#[cfg(windows)]
fn pfad_zu(guid: Guid) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const Guid,
            dwFlags: u32,
            hToken: isize,
            ppszPath: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pv: *mut core::ffi::c_void);
    }

    let mut roh: *mut u16 = std::ptr::null_mut();
    // SAFETY: `guid` lebt ueber den Aufruf hinaus, `roh` ist ein gueltiger
    // Ausgabezeiger. Der von Windows belegte Speicher wird auf jedem Pfad
    // genau einmal mit CoTaskMemFree freigegeben.
    let hr = unsafe { SHGetKnownFolderPath(&guid, 0, 0, &mut roh) };
    if hr < 0 || roh.is_null() {
        if !roh.is_null() {
            unsafe { CoTaskMemFree(roh as *mut core::ffi::c_void) };
        }
        return None;
    }

    // SAFETY: Windows liefert eine nullterminierte UTF-16-Zeichenkette.
    let laenge = unsafe {
        let mut n = 0usize;
        while *roh.add(n) != 0 {
            n += 1;
        }
        n
    };
    // SAFETY: `laenge` Zeichen sind gueltig belegt.
    let scheibe = unsafe { std::slice::from_raw_parts(roh, laenge) };
    let pfad = PathBuf::from(std::ffi::OsString::from_wide(scheibe));
    // SAFETY: derselbe Zeiger, genau einmal freigegeben.
    unsafe { CoTaskMemFree(roh as *mut core::ffi::c_void) };

    // Ein Pfad, den es nicht gibt, ist kein Angebot wert.
    if pfad.as_os_str().is_empty() || !pfad.is_dir() {
        return None;
    }
    Some(pfad)
}

#[cfg(not(windows))]
pub fn path_of(_folder: KnownFolder) -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn local_app_data() -> Option<PathBuf> {
    None
}

/// Ein Ordner, wie ihn die Oberflaeche anbietet.
#[derive(Debug, Clone, Serialize)]
pub struct FolderOffer {
    pub key: String,
    pub label: String,
    pub path: String,
}

/// Alle vorhandenen bekannten Ordner. Nicht vorhandene fallen weg.
pub fn verfuegbare() -> Vec<FolderOffer> {
    ALLE.iter()
        .filter_map(|f| {
            path_of(*f).map(|p| FolderOffer {
                key: f.key().to_string(),
                label: f.label().to_string(),
                path: crate::paths::display_path(&p),
            })
        })
        .collect()
}

/// Sinnvolle Zielordner fuer die Triage.
///
/// Downloads faellt grundsaetzlich heraus: Es ist der klassische Quellordner,
/// kein Ablageort. Ebenso jeder Ordner, der mit der gerade aufzuraeumenden
/// Quelle identisch ist - eine Datei von Bilder nach Bilder zu verschieben
/// waere kein Angebot, sondern eine Stolperfalle.
pub fn standard_ziele(quelle: Option<&str>) -> Vec<FolderOffer> {
    let quell_key = quelle.map(crate::paths::path_key);
    const ZIELE: [KnownFolder; 5] = [
        KnownFolder::Documents,
        KnownFolder::Pictures,
        KnownFolder::Videos,
        KnownFolder::Music,
        KnownFolder::Desktop,
    ];
    ZIELE
        .iter()
        .filter_map(|f| path_of(*f).map(|p| (f, p)))
        .filter(|(_, p)| {
            let key = crate::paths::path_key(&crate::paths::display_path(p));
            quell_key.as_deref() != Some(key.as_str())
        })
        .map(|(f, p)| FolderOffer {
            key: f.key().to_string(),
            label: f.label().to_string(),
            path: crate::paths::display_path(&p),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bekannte_ordner_werden_gefunden() {
        let dokumente = path_of(KnownFolder::Documents);
        assert!(dokumente.is_some(), "Dokumente müssen auffindbar sein");
        let p = dokumente.unwrap();
        assert!(p.is_dir());
        assert!(p.is_absolute());
        assert!(!crate::paths::display_path(&p).is_empty());
    }

    #[test]
    fn jeder_gelieferte_pfad_existiert_wirklich() {
        for f in ALLE {
            if let Some(p) = path_of(f) {
                assert!(p.is_dir(), "{:?} wurde geliefert, ist aber kein Ordner", f);
            }
        }
    }

    #[test]
    fn verfuegbare_ordner_haben_beschriftung_und_pfad() {
        let liste = verfuegbare();
        assert!(!liste.is_empty(), "mindestens ein bekannter Ordner muss da sein");
        for o in &liste {
            assert!(!o.key.is_empty());
            assert!(!o.label.is_empty());
            assert!(std::path::Path::new(&o.path).is_dir());
        }
    }

    #[test]
    fn die_quelle_wird_nicht_als_ziel_angeboten() {
        let Some(dokumente) = path_of(KnownFolder::Documents) else { return };
        let als_text = crate::paths::display_path(&dokumente);

        let ziele = standard_ziele(Some(&als_text));
        assert!(
            !ziele
                .iter()
                .any(|z| crate::paths::path_key(&z.path) == crate::paths::path_key(&als_text)),
            "der aufzuraeumende Ordner darf nicht sein eigenes Ziel sein"
        );

        // Ohne Quelle ist er wieder dabei.
        let alle = standard_ziele(None);
        assert!(alle.iter().any(|z| z.key == "documents"));
    }

    #[test]
    fn downloads_ist_nie_ein_standardziel() {
        for ziel in standard_ziele(None) {
            assert_ne!(ziel.key, "downloads");
        }
    }

    #[test]
    fn ziele_passen_auf_zifferntasten() {
        assert!(standard_ziele(None).len() <= 9);
    }

    #[test]
    fn ziele_sind_absolute_gueltige_pfade() {
        for ziel in standard_ziele(None) {
            let p = std::path::Path::new(&ziel.path);
            assert!(p.is_absolute(), "{} ist nicht absolut", ziel.path);
            assert!(crate::paths::validate_directory_path(p).is_ok());
        }
    }
}

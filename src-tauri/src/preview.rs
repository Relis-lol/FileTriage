//! Vorschauen fuer die Triage.
//!
//! Alles passiert im Arbeitsspeicher und wird danach verworfen - es werden
//! keine Miniaturbilder auf die Platte geschrieben. Jede Vorschau hat eine
//! harte Groessengrenze; scheitert die Erzeugung, ist das kein Fehler, sondern
//! einfach "keine Vorschau".

use std::io::Read;
use std::path::Path;

use base64::Engine;
use serde::Serialize;

use crate::error::{AppError, Result};
use crate::model::Category;
use crate::{classify, paths};

/// Kantenlaenge des erzeugten Miniaturbilds.
const THUMB_MAX: u32 = 720;
/// So viel Text wird hoechstens gelesen.
const TEXT_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Image,
    Text,
    /// Keine Vorschau moeglich - die Oberflaeche zeigt dann ein Typ-Symbol.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub kind: PreviewKind,
    pub path: String,
    /// Bilddaten als `data:`-URL (nur bei `Image`).
    pub data_url: Option<String>,
    pub text: Option<String>,
    /// Wurde der Text abgeschnitten?
    pub truncated: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Zusatzangaben, etwa EXIF - als Paare fuer die Anzeige.
    pub meta: Vec<(String, String)>,
    /// Grund, warum keine Vorschau erzeugt wurde.
    pub note_code: Option<String>,
}

impl Preview {
    pub(crate) fn empty(path: &Path, note_code: &str) -> Preview {
        Preview {
            kind: PreviewKind::None,
            path: paths::display_path(path),
            data_url: None,
            text: None,
            truncated: false,
            width: None,
            height: None,
            meta: Vec::new(),
            note_code: Some(note_code.into()),
        }
    }
}

/// Erzeugt eine Vorschau. Gibt bei Problemen eine leere Vorschau mit Hinweis
/// zurueck statt eines Fehlers - eine kaputte Datei darf die Triage nicht stoppen.
pub fn generate(path: &Path, max_bytes: u64) -> Preview {
    let epath = paths::extended(path);
    let meta = match std::fs::metadata(&epath) {
        Ok(m) => m,
        Err(_) => return Preview::empty(path, "unreadable"),
    };
    if meta.len() > max_bytes {
        return Preview::empty(path, "too_large");
    }

    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match classify::categorize(&name) {
        Category::Image => match image_preview(path) {
            Ok(p) => p,
            Err(_) => Preview::empty(path, "image_unavailable"),
        },
        Category::Document | Category::Code | Category::Spreadsheet
            if is_texty(&classify::extension_of(&name)) =>
        {
            match text_preview(path) {
                Ok(p) => p,
                Err(_) => Preview::empty(path, "text_unreadable"),
            }
        }
        _ => Preview::empty(path, "unsupported_type"),
    }
}

fn is_texty(ext: &str) -> bool {
    !matches!(
        ext,
        "doc" | "docx" | "odt" | "pages" | "wpd" | "rtf" | "xls" | "xlsx" | "xlsm" | "ods"
    )
}

fn image_preview(path: &Path) -> Result<Preview> {
    let epath = paths::extended(path);
    let reader = image::ImageReader::open(&epath)
        .map_err(|e| AppError::from_io(&e, "Bild öffnen"))?
        .with_guessed_format()
        .map_err(|e| AppError::from_io(&e, "Bildformat"))?;

    let (w, h) = reader.into_dimensions().map_err(|e| AppError::Io(e.to_string()))?;

    let img = image::ImageReader::open(&epath)
        .map_err(|e| AppError::from_io(&e, "Bild öffnen"))?
        .with_guessed_format()
        .map_err(|e| AppError::from_io(&e, "Bildformat"))?
        .decode()
        .map_err(|e| AppError::Io(e.to_string()))?;

    let thumb = img.thumbnail(THUMB_MAX, THUMB_MAX);
    let mut puffer = std::io::Cursor::new(Vec::new());
    thumb
        .write_to(&mut puffer, image::ImageFormat::Png)
        .map_err(|e| AppError::Io(e.to_string()))?;
    let daten = base64::engine::general_purpose::STANDARD.encode(puffer.into_inner());

    Ok(Preview {
        kind: PreviewKind::Image,
        path: paths::display_path(path),
        data_url: Some(format!("data:image/png;base64,{daten}")),
        text: None,
        truncated: false,
        width: Some(w),
        height: Some(h),
        meta: exif_meta(path),
        note_code: None,
    })
}

/// Liest die wichtigsten EXIF-Felder. Fehler sind hier normal und still.
fn exif_meta(path: &Path) -> Vec<(String, String)> {
    let Ok(datei) = std::fs::File::open(paths::extended(path)) else {
        return Vec::new();
    };
    let mut leser = std::io::BufReader::new(datei);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut leser) else {
        return Vec::new();
    };

    let felder = [
        (exif::Tag::DateTimeOriginal, "captured"),
        (exif::Tag::Make, "camera"),
        (exif::Tag::Model, "model"),
        (exif::Tag::LensModel, "lens"),
        (exif::Tag::FNumber, "aperture"),
        (exif::Tag::ExposureTime, "exposure"),
        (exif::Tag::PhotographicSensitivity, "ISO"),
        (exif::Tag::Orientation, "orientation"),
    ];
    felder
        .iter()
        .filter_map(|(tag, label)| {
            exif.get_field(*tag, exif::In::PRIMARY).map(|f| {
                (
                    (*label).to_string(),
                    f.display_value().with_unit(&exif).to_string(),
                )
            })
        })
        .collect()
}

fn text_preview(path: &Path) -> Result<Preview> {
    let mut datei = std::fs::File::open(paths::extended(path))
        .map_err(|e| AppError::from_io(&e, "Textdatei öffnen"))?;
    let mut puffer = vec![0u8; TEXT_MAX_BYTES];
    let gelesen = datei
        .read(&mut puffer)
        .map_err(|e| AppError::from_io(&e, "Textdatei lesen"))?;
    puffer.truncate(gelesen);

    // Nullbytes deuten auf eine Binaerdatei mit irrefuehrender Endung hin.
    if puffer.iter().take(1024).any(|b| *b == 0) {
        return Ok(Preview::empty(path, "binary_text"));
    }

    let text = String::from_utf8_lossy(&puffer).to_string();
    let laenge = std::fs::metadata(paths::extended(path)).map(|m| m.len()).unwrap_or(0);

    Ok(Preview {
        kind: PreviewKind::Text,
        path: paths::display_path(path),
        data_url: None,
        text: Some(text),
        truncated: laenge as usize > gelesen,
        width: None,
        height: None,
        meta: Vec::new(),
        note_code: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const GRENZE: u64 = 64 * 1024 * 1024;

    #[test]
    fn textvorschau_liest_anfang_und_meldet_kuerzung() {
        let dir = TempDir::new("prev");
        let inhalt = "Zeile\n".repeat(10_000);
        let p = dir.file("gross.txt", inhalt.as_bytes());
        let v = generate(&p, GRENZE);
        assert!(matches!(v.kind, PreviewKind::Text));
        assert!(v.truncated);
        assert!(v.text.unwrap().starts_with("Zeile"));
    }

    #[test]
    fn bildvorschau_erzeugt_data_url() {
        let dir = TempDir::new("prev-img");
        let bild = image::RgbImage::from_fn(40, 20, |x, _| image::Rgb([x as u8, 10, 200]));
        let p = dir.path().join("test.png");
        bild.save(&p).unwrap();

        let v = generate(&p, GRENZE);
        assert!(matches!(v.kind, PreviewKind::Image), "{:?}", v.note_code);
        assert!(v.data_url.unwrap().starts_with("data:image/png;base64,"));
        assert_eq!((v.width, v.height), (Some(40), Some(20)));
    }

    #[test]
    fn kaputtes_bild_liefert_hinweis_statt_fehler() {
        let dir = TempDir::new("prev-broken");
        let p = dir.file("kaputt.png", b"das ist kein PNG");
        let v = generate(&p, GRENZE);
        assert!(matches!(v.kind, PreviewKind::None));
        assert!(v.note_code.is_some());
    }

    #[test]
    fn zu_grosse_datei_wird_nicht_gelesen() {
        let dir = TempDir::new("prev-big");
        let p = dir.file("gross.txt", &vec![b'x'; 4096]);
        let v = generate(&p, 1024);
        assert!(matches!(v.kind, PreviewKind::None));
        assert_eq!(v.note_code.as_deref(), Some("too_large"));
    }

    #[test]
    fn binaerdatei_mit_txt_endung_wird_erkannt() {
        let dir = TempDir::new("prev-bin");
        let p = dir.file("falsch.txt", &[0u8, 1, 2, 0, 3, 4]);
        let v = generate(&p, GRENZE);
        assert!(matches!(v.kind, PreviewKind::None));
    }

    #[test]
    fn fehlende_datei_stuerzt_nicht_ab() {
        let dir = TempDir::new("prev-missing");
        let v = generate(&dir.path().join("weg.png"), GRENZE);
        assert!(matches!(v.kind, PreviewKind::None));
    }
}

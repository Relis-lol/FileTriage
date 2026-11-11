//! Deterministische Dateityperkennung.
//!
//! Bewusst nur Endung und Namensmuster - keine Inhaltsanalyse. Das ist schnell,
//! nachvollziehbar und kann keine Datei "falsch verstehen", ohne dass der Nutzer
//! das an der Endung selbst sehen koennte.

use crate::model::Category;

/// Endung in Kleinschreibung, ohne Punkt. Leer, wenn keine vorhanden.
pub fn extension_of(name: &str) -> String {
    // Fuehrende Punkte gehoeren zum Namen (.gitignore hat keine Endung).
    let trimmed = name.trim_start_matches('.');
    match trimmed.rfind('.') {
        Some(idx) if idx + 1 < trimmed.len() => trimmed[idx + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Erkennt typische Zwischenstaende von Downloads und Editoren.
fn is_temp_name(name_lower: &str, ext: &str) -> bool {
    const TEMP_EXT: [&str; 8] = ["tmp", "temp", "part", "crdownload", "partial", "download", "bak", "!ut"];
    TEMP_EXT.contains(&ext)
        || name_lower.starts_with("~$")
        || name_lower.ends_with(".tmp")
        || name_lower == "thumbs.db"
        || name_lower == "desktop.ini"
}

pub fn categorize(name: &str) -> Category {
    let lower = name.to_lowercase();
    let ext = extension_of(&lower);

    if is_temp_name(&lower, &ext) {
        return Category::Temp;
    }

    match ext.as_str() {
        "exe" | "msi" | "msix" | "appx" | "msp" => Category::Installer,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "cab" | "tgz" | "lzh"
        | "arj" => Category::Archive,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "heif"
        | "avif" | "svg" | "ico" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "psd"
        | "xcf" => Category::Image,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" | "ts"
        | "m2ts" => Category::Video,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "opus" | "wma" | "m4a" | "aiff" | "mid"
        | "midi" => Category::Audio,
        "pdf" => Category::Pdf,
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "tex" | "pages" | "wpd" => {
            Category::Document
        }
        "xls" | "xlsx" | "xlsm" | "ods" | "csv" | "tsv" => Category::Spreadsheet,
        "ppt" | "pptx" | "odp" | "key" => Category::Presentation,
        // ".ts" bleibt bewusst beim Video: MPEG-Transportstroeme landen weit
        // haeufiger in Downloads-Ordnern als einzelne TypeScript-Dateien.
        "rs" | "py" | "js" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cs" | "java"
        | "go" | "rb" | "php" | "swift" | "kt" | "lua" | "sh" | "ps1" | "bat" | "cmd" | "sql"
        | "html" | "css" | "scss" | "json" | "yaml" | "yml" | "toml" | "xml" | "ini" | "cfg"
        | "conf" | "vue" | "svelte" | "dart" | "scala" | "clj" | "ex" | "erl" | "hs" | "ml"
        | "r" | "jl" | "asm" | "vb" | "pl" => Category::Code,
        "torrent" => Category::Torrent,
        "ttf" | "otf" | "woff" | "woff2" | "fon" | "fnt" => Category::Font,
        "epub" | "mobi" | "azw" | "azw3" | "fb2" | "djvu" | "cbz" | "cbr" => Category::Ebook,
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "dmg" | "bin" | "nrg" | "mdf" => {
            Category::DiskImage
        }
        "lnk" | "url" | "webloc" => Category::Shortcut,
        _ => Category::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endung_wird_klein_und_ohne_punkt_geliefert() {
        assert_eq!(extension_of("Foto.JPG"), "jpg");
        assert_eq!(extension_of("archiv.tar.gz"), "gz");
        assert_eq!(extension_of("README"), "");
        assert_eq!(extension_of(".gitignore"), "");
        assert_eq!(extension_of("punkt."), "");
    }

    #[test]
    fn kategorien_nach_endung() {
        assert_eq!(categorize("setup.exe"), Category::Installer);
        assert_eq!(categorize("Urlaub.HEIC"), Category::Image);
        assert_eq!(categorize("rechnung.pdf"), Category::Pdf);
        assert_eq!(categorize("daten.csv"), Category::Spreadsheet);
        assert_eq!(categorize("main.rs"), Category::Code);
        assert_eq!(categorize("linux.iso"), Category::DiskImage);
        assert_eq!(categorize("irgendwas.qwertz"), Category::Unknown);
    }

    #[test]
    fn temporaere_dateien_haben_vorrang() {
        assert_eq!(categorize("film.mkv.part"), Category::Temp);
        assert_eq!(categorize("~$Bericht.docx"), Category::Temp);
        assert_eq!(categorize("Thumbs.db"), Category::Temp);
        assert_eq!(categorize("installer.exe.crdownload"), Category::Temp);
    }

    #[test]
    fn unicode_namen_stuerzen_nicht_ab() {
        assert_eq!(categorize("Ünïcödé — Datei.PNG"), Category::Image);
        assert_eq!(categorize("日本語のファイル.txt"), Category::Document);
        assert_eq!(categorize("emoji-🚀.zip"), Category::Archive);
    }
}

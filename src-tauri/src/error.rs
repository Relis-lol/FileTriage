//! Zentraler Fehlertyp.
//!
//! Grundsatz: Kein Dateisystemfehler darf die Anwendung beenden. Jeder Fehler
//! wird in eine Variante uebersetzt, die das Frontend unterscheiden kann, damit
//! die Oberflaeche z. B. "gesperrt" anders behandelt als "geloescht".

use serde::ser::{Serialize, SerializeStruct, Serializer};

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Ein-/Ausgabefehler: {0}")]
    Io(String),
    #[error("Zugriff verweigert: {0}")]
    Denied(String),
    #[error("Datei wird gerade verwendet: {0}")]
    Busy(String),
    #[error("Nicht gefunden: {0}")]
    NotFound(String),
    #[error("Datenbankfehler: {0}")]
    Db(String),
    /// Eine Dateioperation wurde abgewiesen, weil ihre Absicht nicht in ein
    /// dauerhaftes Journal geschrieben werden kann.
    #[error("Dauerhaftes Aktionsjournal nicht verfügbar: {0}")]
    JournalUnavailable(String),
    #[error("Ungültige Eingabe: {0}")]
    Invalid(String),
    #[error("Papierkorb: {0}")]
    Trash(String),
    #[error("Abgebrochen")]
    Cancelled,
    /// Die Datei ist nicht mehr die, die geprueft wurde. Immer ein Grund, die
    /// Operation abzubrechen, nie ein Grund, trotzdem weiterzumachen.
    #[error("{0}")]
    Changed(String),
    #[error("Kein Platz: {0}")]
    NoSpace(String),
    /// Die Lizenz erlaubt gerade keine Dateiaenderung. Kein Fehler im
    /// eigentlichen Sinn - der Vorgang wurde bewusst nicht begonnen, und
    /// zwar bevor irgendetwas angefasst wurde.
    #[error("Diese Aktion braucht eine gueltige Lizenz")]
    LicenseRequired,
    /// Nur in Tests: simuliert einen Prozessabbruch an einem Crashpoint.
    /// Der Aufrufer raeumt bewusst **nicht** auf.
    #[cfg(test)]
    #[error("Simulierter Absturz bei {0}")]
    Simulated(&'static str),
}

impl AppError {
    /// Kurzer, stabiler Bezeichner fuer das Frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::Io(_) => "io",
            AppError::Denied(_) => "denied",
            AppError::Busy(_) => "busy",
            AppError::NotFound(_) => "not_found",
            AppError::Db(_) => "db",
            AppError::JournalUnavailable(_) => "journal_unavailable",
            AppError::Invalid(_) => "invalid",
            AppError::Trash(_) => "trash",
            AppError::Cancelled => "cancelled",
            AppError::Changed(_) => "changed",
            AppError::NoSpace(_) => "no_space",
            AppError::LicenseRequired => "license_required",
            #[cfg(test)]
            AppError::Simulated(_) => "simulated",
        }
    }

    /// Uebersetzt einen IO-Fehler mit Pfadkontext in die passende Variante.
    pub fn from_io(err: &std::io::Error, context: &str) -> Self {
        use std::io::ErrorKind;
        // Windows: 5 = Zugriff verweigert, 32/33 = Sperre durch anderen Prozess.
        let raw = err.raw_os_error().unwrap_or(0);
        match (err.kind(), raw) {
            (ErrorKind::NotFound, _) => AppError::NotFound(context.to_string()),
            (ErrorKind::PermissionDenied, _) | (_, 5) => AppError::Denied(context.to_string()),
            (_, 32) | (_, 33) => AppError::Busy(context.to_string()),
            _ => AppError::Io(format!("{context}: {err}")),
        }
    }

    /// Kurze Ueberschrift in normaler Sprache.
    ///
    /// Was der Nutzer sieht, ist nicht `Os { code: 32, kind: Uncategorized }`,
    /// sondern "Die Datei ist gerade in Benutzung". Die technische Fassung
    /// bleibt als Detail erhalten und steht im Log.
    pub fn title(&self) -> &'static str {
        match self {
            AppError::Io(_) => "Die Datei ließ sich nicht bearbeiten",
            AppError::Denied(_) => "Kein Zugriff auf diese Datei",
            AppError::Busy(_) => "Die Datei ist gerade in Benutzung",
            AppError::NotFound(_) => "Diese Datei gibt es nicht mehr",
            AppError::Db(_) => "Der Index ließ sich nicht lesen",
            AppError::JournalUnavailable(_) => "Dateioperationen sind vorsorglich gesperrt",
            AppError::Invalid(_) => "Das geht so nicht",
            AppError::Trash(_) => "Der Papierkorb hat abgelehnt",
            AppError::Cancelled => "Abgebrochen",
            AppError::Changed(_) => "Die Datei hat sich verändert",
            AppError::NoSpace(_) => "Kein Platz mehr auf dem Ziellaufwerk",
            AppError::LicenseRequired => "Die Testphase ist abgelaufen",
            #[cfg(test)]
            AppError::Simulated(_) => "Simulierter Abbruch",
        }
    }

    /// Was der Nutzer jetzt tun kann. Leer, wenn es nichts Sinnvolles gibt.
    pub fn hint(&self) -> &'static str {
        match self {
            AppError::Busy(_) => {
                "Sie ist wahrscheinlich in einem anderen Programm geoeffnet. \
                 Schliesse es und versuche es noch einmal."
            }
            AppError::Denied(_) => {
                "Windows laesst den Zugriff nicht zu. Das passiert bei \
                 Systemordnern und bei Dateien anderer Benutzerkonten."
            }
            AppError::NotFound(_) => {
                "Sie wurde inzwischen verschoben oder geloescht. \
                 Ein neuer Scan bringt die Liste auf Stand."
            }
            AppError::Changed(_) => {
                "Jemand hat sie bearbeitet, seit sie hier angezeigt wurde. \
                 Zur Sicherheit ist nichts passiert."
            }
            AppError::NoSpace(_) => "Schaffe Platz oder wähle ein anderes Ziel.",
            AppError::LicenseRequired => {
                "Deine Dateien und Einstellungen bleiben unveraendert. \
                 Ansehen geht weiter; zum Sortieren wird die Vollversion \
                 gebraucht."
            }
            AppError::Trash(_) => {
                "Auf Netzlaufwerken und manchen USB-Sticks gibt es keinen \
                 Papierkorb."
            }
            AppError::Db(_) => {
                "Der Index laesst sich in den erweiterten Einstellungen neu \
                 aufbauen. Deine Dateien sind davon nicht betroffen."
            }
            AppError::JournalUnavailable(_) => {
                "Das dauerhafte Aktionsjournal ist nicht verfügbar. Ansehen und Scannen gehen \
                 weiter; Dateien werden erst wieder verändert, wenn der Journalzustand eindeutig ist."
            }
            _ => "",
        }
    }

    /// Wahr, wenn ein Scan diesen Eintrag ueberspringen darf, statt abzubrechen.
    pub fn is_skippable(&self) -> bool {
        matches!(
            self,
            AppError::Denied(_) | AppError::Busy(_) | AppError::NotFound(_) | AppError::Io(_)
        )
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Db(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Invalid(format!("JSON: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::from_io(&e, "Dateisystem")
    }
}

impl From<trash::Error> for AppError {
    fn from(e: trash::Error) -> Self {
        AppError::Trash(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kein Fehler darf dem Nutzer als Rust-Debug-Ausgabe begegnen.
    #[test]
    fn jeder_fehler_hat_eine_verstaendliche_ueberschrift() {
        let faelle = [
            AppError::Io("Os { code: 32 }".into()),
            AppError::Denied("C:\\Windows".into()),
            AppError::Busy("budget.xlsx".into()),
            AppError::NotFound("weg.txt".into()),
            AppError::Db("no such column".into()),
            AppError::Invalid("Pfad mit ..".into()),
            AppError::Trash("0x80070057".into()),
            AppError::Cancelled,
            AppError::Changed("bericht.docx".into()),
            AppError::NoSpace("D:".into()),
        ];
        for f in faelle {
            let titel = f.title();
            assert!(!titel.is_empty(), "{:?} hat keine Überschrift", f.kind());
            assert!(
                !titel.contains("Os {") && !titel.contains("code:") && !titel.contains('{'),
                "die Überschrift ist technisch: {titel}"
            );
            assert!(titel.len() < 60, "die Überschrift ist zu lang: {titel}");
        }
    }

    #[test]
    fn haeufige_fehler_sagen_was_zu_tun_ist() {
        // Genau die Faelle, die ein Nutzer beim Aufraeumen wirklich trifft.
        for f in [
            AppError::Busy("x".into()),
            AppError::Denied("x".into()),
            AppError::NotFound("x".into()),
            AppError::Changed("x".into()),
            AppError::NoSpace("x".into()),
        ] {
            assert!(!f.hint().is_empty(), "{} braucht einen Hinweis", f.kind());
        }
    }

    #[test]
    fn technische_fassung_bleibt_erhalten() {
        let f = AppError::Busy("budget.xlsx".into());
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"kind\":\"busy\""));
        assert!(!json.contains("\"title\""));
        assert!(!json.contains("\"hint\""));
        // Der Dateiname gehoert in die Details, damit man ihn nachschlagen kann.
        assert!(json.contains("budget.xlsx"));
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        // Nur der stabile Code ist UI-Vertrag. Die lokalisierte Oberflaeche
        // erzeugt Titel und Hinweis; `message` bleibt technisches Detail.
        let mut st = s.serialize_struct("AppError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

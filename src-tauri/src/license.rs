//! Lizenzierung.
//!
//! FileTriage soll ueber den Microsoft Store verkauft werden: einmalig, ohne
//! Abo, ohne Konto, mit einer vollstaendigen Testversion. Dieses Modul haelt
//! alles zusammen, was damit zu tun hat - und zwar an *einer* Stelle. Verstreute
//! `if licensed`-Abfragen waeren nicht nur unwartbar, sie waeren auch die
//! Sorte Schutz, die man an fuenf Stellen vergisst und an der sechsten
//! umgeht.
//!
//! # Aufbau
//!
//! ```text
//! LicenseService
//!     |
//!     +-- DevelopmentLicenseProvider     (nur in Entwicklungsbauten)
//!     |
//!     +-- MicrosoftStoreLicenseProvider  (Store-Kanal)
//!     |
//!     +-- DirectLicenseProvider          (Bauten ausserhalb des Stores)
//! ```
//!
//! Der Produktcode fragt ausschliesslich [`state`] und [`ensure_may_modify`].
//! Woher die Antwort kommt, entscheidet der Anbieter, und der wird genau
//! einmal beim Start gesetzt.
//!
//! # Wo geprueft wird
//!
//! Nicht in der Oberflaeche. Eine Schaltflaeche auszugrauen ist Hoeflichkeit,
//! kein Schutz - eine Zeile JavaScript, und sie ist wieder da.
//!
//! Geprueft wird in [`crate::journal::plan`]. Das ist die Stelle, an der die
//! Architektur ohnehin schon zusammenlaeuft: "erst schreiben, dann handeln"
//! heisst, dass **jede** veraendernde Operation ihre Absicht ins Journal
//! schreibt, bevor sie eine Datei anfasst. Wer daran vorbei will, muss die
//! Zustandsmaschine umgehen - und damit die Sicherheitsarchitektur, nicht die
//! Lizenz.
//!
//! Ausdruecklich **nicht** gesperrt sind Rueckgaengig und Wiederherstellung.
//! Sie legen keine neue Absicht an, sondern nehmen eine bestehende zurueck.
//! Wem mitten in der Arbeit die Testphase auslaeuft, der darf nicht mit einem
//! halb sortierten Ordner sitzen bleiben; und eine Wiederherstellung nach
//! einem Absturz zu verweigern, waere ein Verstoss gegen die oberste Regel
//! dieses Programms - es muss jederzeit beweisbar sein, wo die Daten liegen.
//!
//! # Was hier bewusst *nicht* passiert
//!
//! Keine Hardware-Kennung, keine Anti-Debug-Tricks, keine Onlinepflicht, keine
//! zweite Testuhr neben der des Stores. Fuer ein Werkzeug zu diesem Preis
//! waere das ein schlechtes Geschaeft: Es trifft zahlende Nutzer haerter als
//! alle anderen.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

// ---------------------------------------------------------------------------
// Vertriebskanal
// ---------------------------------------------------------------------------

/// Woher dieses Programm stammt. Wird beim Bauen festgelegt, nicht zur
/// Laufzeit - es gibt keinen Schalter, mit dem sich der Kanal wechseln liesse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Oeffentliche Source-Available-Ausgabe. Voll nutzbar fuer die nach der
    /// PolyForm Noncommercial License erlaubten Zwecke, ohne Aktivierung.
    Community,
    /// Aus dem Microsoft Store. Der Store ist die Lizenzautoritaet.
    Store,
    /// Ausserhalb des Stores gebaut - der Installer, den der Eigentuemer
    /// selbst weitergibt. Dieser Kanal ist nicht der kommerzielle Weg; er
    /// entsteht nur aus dem privaten Quelltext.
    Direct,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Community => "community",
            Channel::Store => "store",
            Channel::Direct => "direct",
        }
    }
}

/// Der Kanal dieses Bauvorgangs.
///
/// Gesetzt ueber die Umgebungsvariable `FILETRIAGE_CHANNEL` **zur Bauzeit**
/// (siehe `build.rs`). Ohne Angabe gilt `community`, damit ein versehentlich
/// gebautes Paket die oeffentliche Community-Ausgabe ist.
pub const CHANNEL: Channel = match option_env!("FILETRIAGE_CHANNEL") {
    Some(s) if matches!(s.as_bytes(), b"store") => Channel::Store,
    Some(s) if matches!(s.as_bytes(), b"direct") => Channel::Direct,
    _ => Channel::Community,
};

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

/// Der Lizenzzustand.
///
/// Die drei unsicheren Zustaende sind bewusst getrennt. `StoreUnavailable`
/// heisst "wir konnten nicht fragen", `Unknown` heisst "wir haben gefragt und
/// die Antwort nicht verstanden". Keiner von beiden bedeutet "gekauft", und
/// keiner bedeutet "abgelaufen" - beides waere eine Behauptung, fuer die es
/// keine Grundlage gibt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LicenseState {
    /// Oeffentliche Community-Ausgabe: keine Aktivierung und kein Ablauf.
    Community,
    /// Entwicklungsbau. Gibt es in ausgelieferten Programmen nicht.
    Development,
    /// Testphase laeuft.
    TrialActive {
        /// Verbleibende volle Tage, sofern der Store ein Enddatum nennt.
        days_left: Option<i64>,
    },
    /// Testphase abgelaufen, nicht gekauft.
    TrialExpired,
    /// Gekauft.
    Purchased,
    /// Der Store liess sich nicht fragen - kein Paket, kein Dienst, offline.
    StoreUnavailable {
        /// Was genau schiefging. Fuer das Protokoll, nicht fuer den Nutzer.
        reason: String,
    },
    /// Gefragt, aber die Antwort ergab kein klares Bild.
    Unknown,
}

impl LicenseState {
    pub fn as_str(&self) -> &'static str {
        match self {
            LicenseState::Community => "community",
            LicenseState::Development => "development",
            LicenseState::TrialActive { .. } => "trial_active",
            LicenseState::TrialExpired => "trial_expired",
            LicenseState::Purchased => "purchased",
            LicenseState::StoreUnavailable { .. } => "store_unavailable",
            LicenseState::Unknown => "unknown",
        }
    }

    /// Eindeutige Zustaende, die sich zwischenspeichern lassen.
    ///
    /// Nur diese beiden sind Aussagen des Stores ueber die Lizenz selbst.
    /// Alles andere ist entweder voruebergehend oder eine Nichtantwort und
    /// taugt nicht als Grundlage fuer spaetere Entscheidungen.
    fn ist_eindeutig(&self) -> bool {
        matches!(self, LicenseState::Purchased | LicenseState::TrialExpired)
    }
}

// ---------------------------------------------------------------------------
// Anbieter
// ---------------------------------------------------------------------------

/// Woher der Lizenzzustand kommt.
pub trait LicenseProvider: Send + Sync {
    fn state(&self) -> LicenseState;
    /// Kurzname fuer Protokoll und Diagnose.
    fn name(&self) -> &'static str;
}

/// Fuer Bauten ausserhalb des Stores.
///
/// Diese entstehen nur aus dem privaten Quelltext und werden nicht verkauft.
/// Sie sind voll nutzbar - eine Testphase waere hier sinnlos, weil es keinen
/// Kaufweg gibt, an dessen Ende sie stehen koennte.
pub struct DirectLicenseProvider;

impl LicenseProvider for DirectLicenseProvider {
    fn state(&self) -> LicenseState {
        LicenseState::Purchased
    }
    fn name(&self) -> &'static str {
        "direct"
    }
}

/// Anbieter der oeffentlichen Source-Available-Ausgabe. Die rechtliche
/// Beschraenkung auf nichtkommerzielle Nutzung ist eine Lizenzbedingung und
/// wird nicht als DRM im Programm durchgesetzt.
pub struct CommunityLicenseProvider;

impl LicenseProvider for CommunityLicenseProvider {
    fn state(&self) -> LicenseState {
        LicenseState::Community
    }
    fn name(&self) -> &'static str {
        "community"
    }
}

/// Umschaltbarer Anbieter fuer Entwicklung und Tests.
///
/// **Existiert nur in Entwicklungsbauten.** Ein Release ohne das Merkmal
/// `dev-license` uebersetzt diesen Block nicht mit; ein Test in
/// `release_hardening.rs` haelt das fest.
#[cfg(any(debug_assertions, feature = "dev-license"))]
pub mod dev {
    use super::*;
    use parking_lot::RwLock;

    pub struct DevelopmentLicenseProvider {
        zustand: RwLock<LicenseState>,
    }

    impl DevelopmentLicenseProvider {
        /// Liest den Startzustand aus `FILETRIAGE_LICENSE`.
        ///
        /// Erlaubt: `development`, `trial_active`, `trial_active:<Tage>`,
        /// `trial_expired`, `purchased`, `store_unavailable`, `unknown`.
        pub fn from_env() -> DevelopmentLicenseProvider {
            let roh = std::env::var("FILETRIAGE_LICENSE").unwrap_or_default();
            DevelopmentLicenseProvider { zustand: RwLock::new(parse(&roh)) }
        }

        pub fn set(&self, zustand: LicenseState) {
            *self.zustand.write() = zustand;
        }
    }

    pub fn parse(roh: &str) -> LicenseState {
        let (name, rest) = match roh.split_once(':') {
            Some((a, b)) => (a, Some(b)),
            None => (roh, None),
        };
        match name.trim() {
            "trial_active" => LicenseState::TrialActive {
                days_left: rest.and_then(|r| r.trim().parse().ok()).or(Some(7)),
            },
            "trial_expired" => LicenseState::TrialExpired,
            "purchased" => LicenseState::Purchased,
            "store_unavailable" => {
                LicenseState::StoreUnavailable { reason: "erzwungen (Entwicklung)".into() }
            }
            "unknown" => LicenseState::Unknown,
            _ => LicenseState::Development,
        }
    }

    impl LicenseProvider for DevelopmentLicenseProvider {
        fn state(&self) -> LicenseState {
            self.zustand.read().clone()
        }
        fn name(&self) -> &'static str {
            "development"
        }
    }
}

// ---------------------------------------------------------------------------
// Microsoft Store
// ---------------------------------------------------------------------------

/// Fragt den Microsoft Store nach der Lizenz dieser Anwendung.
///
/// # Stand
///
/// Der erste Schritt - laeuft dieses Programm ueberhaupt mit Paketidentitaet? -
/// ist hier vollstaendig umgesetzt und laeuft auf jedem Windows. Ohne
/// Paketidentitaet gibt es keine Store-Lizenz, und zwar unabhaengig davon, ob
/// der Rechner online ist.
///
/// Der zweite Schritt - die eigentliche Abfrage - steht in [`Self::abfragen`]
/// und ist noch nicht ausgefuehrt. Dafuer braucht es
/// `Windows.Services.Store.StoreContext::GetAppLicenseAsync`, und die liefert
/// erst dann etwas Sinnvolles, wenn das Paket eine echte Store-Identitaet
/// besitzt und im Partner Center ein Produkt mit Testphase hinterlegt ist.
/// Beides gibt es noch nicht. Code zu schreiben, der sich nicht einmal
/// ausfuehren laesst, und ihn dann als geprueft auszugeben, waere schlechter
/// als eine ehrliche Luecke.
///
/// Was dort einzusetzen ist, steht in `docs/STORE_LICENSING.md`.
pub struct MicrosoftStoreLicenseProvider;

impl MicrosoftStoreLicenseProvider {
    /// Laeuft dieser Prozess mit Paketidentitaet?
    ///
    /// `GetCurrentPackageFullName` liefert `APPMODEL_ERROR_NO_PACKAGE`
    /// (15700), wenn nicht. Das ist die offizielle Art, die Frage zu stellen,
    /// und sie braucht weder WinRT noch Netz.
    pub fn hat_paketidentitaet() -> bool {
        paket_voller_name().is_some()
    }

    fn abfragen(&self) -> LicenseState {
        if !Self::hat_paketidentitaet() {
            return LicenseState::StoreUnavailable {
                reason: "ohne Paketidentitaet gibt es keine Store-Lizenz".into(),
            };
        }
        // HIER kommt `StoreContext::GetAppLicenseAsync` hin.
        // Siehe docs/STORE_LICENSING.md.
        LicenseState::StoreUnavailable {
            reason: "Store-Abfrage noch nicht angebunden".into(),
        }
    }
}

impl LicenseProvider for MicrosoftStoreLicenseProvider {
    fn state(&self) -> LicenseState {
        self.abfragen()
    }
    fn name(&self) -> &'static str {
        "microsoft_store"
    }
}

/// Voller Paketname dieses Prozesses, falls er mit Paketidentitaet laeuft.
pub fn paket_voller_name() -> Option<String> {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentPackageFullName(len: *mut u32, name: *mut u16) -> u32;
        }
        // 0 = ERROR_SUCCESS, 122 = ERROR_INSUFFICIENT_BUFFER,
        // 15700 = APPMODEL_ERROR_NO_PACKAGE.
        let mut len: u32 = 0;
        let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
        if rc != 122 || len == 0 {
            return None;
        }
        let mut puffer = vec![0u16; len as usize];
        let rc = unsafe { GetCurrentPackageFullName(&mut len, puffer.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let _ = std::ptr::null::<c_void>();
        let ende = puffer.iter().position(|&c| c == 0).unwrap_or(puffer.len());
        Some(String::from_utf16_lossy(&puffer[..ende]))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Der Dienst
// ---------------------------------------------------------------------------

/// Was der zuletzt eindeutig bekannte Zustand war.
///
/// Kein DRM-Speicher, sondern eine Hoeflichkeit: Wenn der Store gerade nicht
/// antwortet, soll ein zahlender Nutzer weiterarbeiten koennen. Der Eintrag
/// ist bewusst nicht die Autoritaet - er kann eine Sperre nur aufheben, wenn
/// vorher einmal ein Kauf bestaetigt war, und er kann eine abgelaufene
/// Testphase nicht wieder gueltig machen.
///
/// Ihn zu loeschen bringt niemandem etwas: Ohne Eintrag gilt die
/// Kulanzfrist, und die ist kuerzer als die Testphase.
pub const SETTING_LAST_KNOWN: &str = "license_last_known";
pub const SETTING_FIRST_SEEN: &str = "license_first_seen";
pub const SETTING_LAST_SEEN: &str = "license_last_seen";
pub const SETTING_STATE_VERSION: &str = "license_state_version";
const SETTING_ROLLBACK_USED: &str = "license_clock_rollback_used";
const STATE_VERSION: &str = "2";
const CLOCK_ROLLBACK_TOLERANCE: i64 = 5 * 60;

/// So lange darf weitergearbeitet werden, wenn der Store nie erreichbar war.
///
/// Drei Tage sind lang genug fuer eine Stoerung bei Microsoft und zu kurz, um
/// daraus eine dauerhafte Gratisnutzung zu machen.
pub const KULANZ_SEKUNDEN: i64 = 3 * 24 * 3600;

/// Produktkennung im Microsoft Store.
///
/// Kommt aus dem Partner Center und existiert noch nicht. `None` heisst: Es
/// wird kein Kauf-Link geoeffnet. Einen auszudenken waere schlimmer als
/// keiner - er fuehrte ins Leere und saehe trotzdem echt aus.
pub const STORE_PRODUCT_ID: Option<&str> = option_env!("FILETRIAGE_STORE_PRODUCT_ID");

/// Zielpreis fuer die Anzeige, solange der Store keinen nennt.
///
/// Sobald die Store-Anbindung steht, ist der Store die Preisautoritaet: Er
/// kennt die Waehrung des Nutzers, Aktionen und regionale Preise. Dieser Wert
/// steht deshalb an genau einer Stelle und nicht verstreut im Programm.
pub const PREIS_HINWEIS: &str = match option_env!("FILETRIAGE_PRICE_HINT") {
    Some(p) => p,
    None => "12,99 \u{20ac}",
};

pub struct LicenseService {
    provider: Box<dyn LicenseProvider>,
    clock: std::sync::Arc<dyn Clock>,
    init: parking_lot::RwLock<InitState>,
    /// Zuletzt eindeutig bekannter Zustand, aus dem Index geladen.
    zuletzt_bekannt: parking_lot::RwLock<Option<LicenseState>>,
    /// Wann dieses Programm zum ersten Mal gestartet wurde - Anker der
    /// Kulanzfrist.
    erstkontakt: parking_lot::RwLock<Option<i64>>,
    zuletzt_gesehen: parking_lot::RwLock<Option<i64>>,
    rollback_toleriert: parking_lot::RwLock<bool>,
    clock_update: parking_lot::Mutex<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitState {
    Uninitialized,
    Loaded,
    LoadFailed,
}

trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        crate::db::now_ts()
    }
}

impl LicenseService {
    pub fn new(provider: Box<dyn LicenseProvider>) -> LicenseService {
        Self::with_clock(provider, std::sync::Arc::new(SystemClock))
    }

    fn with_clock(
        provider: Box<dyn LicenseProvider>,
        clock: std::sync::Arc<dyn Clock>,
    ) -> LicenseService {
        LicenseService {
            provider,
            clock,
            init: parking_lot::RwLock::new(InitState::Uninitialized),
            zuletzt_bekannt: parking_lot::RwLock::new(None),
            erstkontakt: parking_lot::RwLock::new(None),
            zuletzt_gesehen: parking_lot::RwLock::new(None),
            rollback_toleriert: parking_lot::RwLock::new(false),
            clock_update: parking_lot::Mutex::new(()),
        }
    }

    /// Der Anbieter, der fuer diesen Kanal zustaendig ist.
    pub fn fuer_kanal() -> LicenseService {
        #[cfg(any(debug_assertions, feature = "dev-license"))]
        {
            // In Entwicklungsbauten hat die Umgebungsvariable Vorrang, damit
            // sich jeder Zustand reproduzierbar durchspielen laesst.
            if std::env::var_os("FILETRIAGE_LICENSE").is_some() {
                return LicenseService::new(Box::new(dev::DevelopmentLicenseProvider::from_env()));
            }
        }
        match CHANNEL {
            Channel::Community => LicenseService::new(Box::new(CommunityLicenseProvider)),
            Channel::Store => LicenseService::new(Box::new(MicrosoftStoreLicenseProvider)),
            Channel::Direct => LicenseService::new(Box::new(DirectLicenseProvider)),
        }
    }

    /// Laedt den zwischengespeicherten Zustand aus dem Index.
    pub fn laden(&self, conn: &rusqlite::Connection, fresh_install: bool) -> Result<()> {
        let jetzt = self.clock.now();
        // Fail closed, falls auch nur eine der folgenden Lese-/Schreiboperationen
        // scheitert. Ein fehlender oder kaputter Zeitanker darf die Kulanzfrist
        // nicht bei jedem Start neu beginnen lassen.
        *self.init.write() = InitState::LoadFailed;
        *self.zuletzt_bekannt.write() = None;
        *self.erstkontakt.write() = None;
        *self.zuletzt_gesehen.write() = None;
        *self.rollback_toleriert.write() = false;

        // Store-Entitlement und lokale Offline-Kulanz sind getrennte
        // Sicherheitsdomänen. Eine eindeutige Store-Entscheidung darf niemals
        // an korrupten oder unplausiblen Grace-Zeitankern scheitern.
        let provider_state=self.provider.state();
        let cached=crate::db::get_setting(conn,SETTING_LAST_KNOWN)?
            .and_then(|raw|serde_json::from_str::<LicenseState>(&raw).ok())
            .filter(LicenseState::ist_eindeutig);
        *self.zuletzt_bekannt.write()=cached.clone();
        let entitlement=match &provider_state{
            LicenseState::Purchased=>Some(LicenseState::Purchased),
            LicenseState::TrialExpired=>Some(LicenseState::TrialExpired),
            LicenseState::Community|LicenseState::TrialActive{..}|LicenseState::Development=>Some(provider_state.clone()),
            LicenseState::StoreUnavailable{..}|LicenseState::Unknown=>cached.clone(),
        };
        if let Some(state) = entitlement {
            if matches!(state,LicenseState::Purchased|LicenseState::TrialExpired){
                crate::db::set_setting(conn,SETTING_LAST_KNOWN,&serde_json::to_string(&state)?)?;
                *self.zuletzt_bekannt.write()=Some(state);
            }
            *self.init.write()=InitState::Loaded;
            return Ok(())
        }

        let version = crate::db::get_setting(conn, SETTING_STATE_VERSION)?;
        let first_raw = crate::db::get_setting(conn, SETTING_FIRST_SEEN)?;
        let last_raw = crate::db::get_setting(conn, SETTING_LAST_SEEN)?;
        let rollback_raw = crate::db::get_setting(conn, SETTING_ROLLBACK_USED)?;
        let parse = |value: Option<String>, name: &str| -> Result<Option<i64>> {
            value
                .map(|raw| {
                    raw.parse::<i64>().map_err(|_| {
                        AppError::Invalid(format!("Ungültiger Lizenzzeitstempel: {name}"))
                    })
                })
                .transpose()
        };
        let mut first = parse(first_raw, SETTING_FIRST_SEEN)?;
        let mut last = parse(last_raw, SETTING_LAST_SEEN)?;

        let mut rollback_toleriert = match (version.as_deref(), rollback_raw.as_deref()) {
            (Some(STATE_VERSION), Some("0")) => false,
            (Some(STATE_VERSION), Some("1")) => true,
            (Some(STATE_VERSION), _) => {
                return Err(AppError::Invalid(
                    "Initialisierter Lizenzzustand hat keinen gültigen Clock-Status".into(),
                ));
            }
            _ => false,
        };

        match version.as_deref() {
            Some(STATE_VERSION) => {
                if first.is_none() || last.is_none() {
                    return Err(AppError::Invalid(
                        "Initialisierter Lizenzzustand hat keinen vollständigen Zeitanker".into(),
                    ));
                }
            }
            Some("1") => {
                if first.is_none() || last.is_none() {
                    return Err(AppError::Invalid(
                        "Initialisierter Lizenzzustand hat keinen vollständigen Zeitanker".into(),
                    ));
                }
                rollback_toleriert = false;
            }
            None if first.is_some() => {
                // Migration von der vorherigen Version, die bereits einen
                // dauerhaften first_seen-Anker hatte.
                last = Some(last.unwrap_or(jetzt.max(first.unwrap())));
            }
            None if fresh_install && first.is_none() && last.is_none() => {
                first = Some(jetzt);
                last = Some(jetzt);
            }
            _ => {
                return Err(AppError::Invalid(
                    "Lizenzzustand fehlt oder stammt aus einer unbekannten Version".into(),
                ));
            }
        }

        let first = first.unwrap();
        let last = last.unwrap();
        if first < 0
            || last < first
            || first > jetzt.saturating_add(CLOCK_ROLLBACK_TOLERANCE)
            || last > jetzt.saturating_add(CLOCK_ROLLBACK_TOLERANCE)
        {
            return Err(AppError::Invalid(
                "Lizenzzeit wurde zurückgestellt oder ist beschädigt".into(),
            ));
        }

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let persist = (|| -> Result<()> {
            crate::db::set_setting(conn, SETTING_STATE_VERSION, STATE_VERSION)?;
            crate::db::set_setting(conn, SETTING_FIRST_SEEN, &first.to_string())?;
            let effective_last = last.max(jetzt);
            crate::db::set_setting(conn, SETTING_LAST_SEEN, &effective_last.to_string())?;
            crate::db::set_setting(
                conn,
                SETTING_ROLLBACK_USED,
                if rollback_toleriert { "1" } else { "0" },
            )?;
            Ok(())
        })();
        if let Err(e) = persist {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
        conn.execute_batch("COMMIT")?;

        if let Some(roh) = crate::db::get_setting(conn, SETTING_LAST_KNOWN)? {
            let geladen = serde_json::from_str::<LicenseState>(&roh)
                .ok()
                .filter(LicenseState::ist_eindeutig);
            *self.zuletzt_bekannt.write() = geladen;
        }
        *self.erstkontakt.write() = Some(first);
        *self.zuletzt_gesehen.write() = Some(last.max(jetzt));
        *self.rollback_toleriert.write() = rollback_toleriert;
        *self.init.write() = InitState::Loaded;
        Ok(())
    }

    /// Der aktuelle Zustand, bereits um die Kulanzregel bereinigt.
    pub fn state(&self) -> LicenseState {
        let roh = self.provider.state();
        if roh.ist_eindeutig() {
            return roh;
        }
        match &roh {
            // Unsicher: Der zuletzt eindeutige Zustand entscheidet.
            LicenseState::StoreUnavailable { .. } | LicenseState::Unknown => {
                match self.zuletzt_bekannt.read().clone() {
                    Some(LicenseState::Purchased) => LicenseState::Purchased,
                    Some(LicenseState::TrialExpired) => LicenseState::TrialExpired,
                    _ => roh,
                }
            }
            _ => roh,
        }
    }

    /// Merkt sich einen eindeutigen Zustand.
    pub fn merken(&self, conn: &rusqlite::Connection, zustand: &LicenseState) -> Result<()> {
        if !zustand.ist_eindeutig() {
            return Ok(());
        }
        if *self.init.read() != InitState::Loaded {
            return Err(AppError::LicenseRequired);
        }
        crate::db::set_setting(conn, SETTING_LAST_KNOWN, &serde_json::to_string(zustand)?)?;
        *self.zuletzt_bekannt.write() = Some(zustand.clone());
        Ok(())
    }

    /// Darf jetzt eine Datei veraendert werden?
    pub fn may_modify(&self) -> bool {
        let state=self.state();
        if *self.init.read() == InitState::Uninitialized { return false; }
        if matches!(state,LicenseState::Community|LicenseState::Purchased|LicenseState::Development|LicenseState::TrialActive{..}){
            return true
        }
        if matches!(state,LicenseState::TrialExpired){return false}
        if *self.init.read() != InitState::Loaded { return false; }
        match state {
            LicenseState::Community
            | LicenseState::Development
            | LicenseState::Purchased
            | LicenseState::TrialActive { .. } => true,
            LicenseState::TrialExpired => false,
            // Nie gefragt werden konnte, nie etwas Eindeutiges bekannt: Der
            // Nutzer soll nicht fuer eine Stoerung bestraft werden - aber auch
            // nicht dauerhaft davon leben.
            LicenseState::StoreUnavailable { .. } | LicenseState::Unknown => {
                let Some(anker) = *self.erstkontakt.read() else { return false };
                let Some(last) = *self.zuletzt_gesehen.read() else { return false };
                let jetzt = self.clock.now();
                if jetzt.saturating_add(CLOCK_ROLLBACK_TOLERANCE) < last {
                    return false;
                }
                if jetzt < last && *self.rollback_toleriert.read() {
                    return false;
                }
                jetzt.max(last).saturating_sub(anker) < KULANZ_SEKUNDEN
            }
        }
    }

    pub fn mark_load_failed(&self) {
        *self.init.write() = InitState::LoadFailed;
    }

    fn authorize_mutation(&self, conn: &rusqlite::Connection) -> Result<()> {
        let _clock_update = self.clock_update.lock();
        let state=self.state();
        if *self.init.read() == InitState::Uninitialized { return Err(AppError::LicenseRequired); }
        if matches!(state,LicenseState::Purchased){
            crate::db::set_setting(conn,SETTING_LAST_KNOWN,&serde_json::to_string(&LicenseState::Purchased)?)?;
            *self.zuletzt_bekannt.write()=Some(LicenseState::Purchased);
            *self.init.write()=InitState::Loaded;
            return Ok(())
        }
        if matches!(state,LicenseState::Community|LicenseState::Development|LicenseState::TrialActive{..}){return Ok(())}
        if matches!(state,LicenseState::TrialExpired){return Err(AppError::LicenseRequired)}
        if *self.init.read() != InitState::Loaded { return Err(AppError::LicenseRequired); }
        // Eine eindeutige Store-Entscheidung braucht und veraendert keinen
        // lokalen Grace-Anker. Insbesondere darf ein frueher erkannter
        // Uhr-Rollback einen spaeteren Kauf nie sperren.
        if !self.may_modify() {
            return Err(AppError::LicenseRequired);
        }
        let jetzt = self.clock.now();
        let last = (*self.zuletzt_gesehen.read()).ok_or(AppError::LicenseRequired)?;
        if jetzt.saturating_add(CLOCK_ROLLBACK_TOLERANCE) < last {
            return Err(AppError::LicenseRequired);
        }
        let effective = jetzt.max(last);
        // Die kleine Toleranz ist fuer diesen Kulanzanker genau einmal
        // verfuegbar. Wenn die Uhr den High-Water-Wert wieder erreicht, darf
        // weitergearbeitet werden; die verbrauchte Toleranz wird dabei aber
        // nicht erneuert. Sonst koennte eine Folge aus -4 Minuten und
        // anschliessendem Aufholen die 72 Stunden beliebig einfrieren.
        let rollback_toleriert = *self.rollback_toleriert.read() || jetzt < last;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let persist = (|| -> Result<()> {
            crate::db::set_setting(conn, SETTING_LAST_SEEN, &effective.to_string())?;
            crate::db::set_setting(
                conn,
                SETTING_ROLLBACK_USED,
                if rollback_toleriert { "1" } else { "0" },
            )?;
            Ok(())
        })();
        if let Err(e) = persist {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
        conn.execute_batch("COMMIT")?;
        *self.zuletzt_gesehen.write() = Some(effective);
        *self.rollback_toleriert.write() = rollback_toleriert;
        Ok(())
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }
}

// ---------------------------------------------------------------------------
// Die eine Sperre
// ---------------------------------------------------------------------------

static DIENST: OnceLock<LicenseService> = OnceLock::new();

/// Setzt den Dienst. Genau einmal beim Start.
pub fn install(service: LicenseService) -> Result<()> {
    install_into(&DIENST, service)
}

fn install_into(cell: &OnceLock<LicenseService>, service: LicenseService) -> Result<()> {
    if *service.init.read() == InitState::Uninitialized {
        return Err(AppError::Invalid(
            "Lizenzdienst wurde vor dem Laden installiert".into(),
        ));
    }
    cell
        .set(service)
        .map_err(|_| AppError::Invalid("Lizenzdienst wurde bereits installiert".into()))
}

/// Der Dienst. Ohne vorheriges [`install`] gilt der Kanal-Standard - so
/// verhalten sich Tests wie ein normaler Bau, ohne dass jeder Test den Dienst
/// aufsetzen muss.
pub fn service() -> &'static LicenseService {
    static FAIL_CLOSED: OnceLock<LicenseService> = OnceLock::new();
    DIENST
        .get()
        .unwrap_or_else(|| FAIL_CLOSED.get_or_init(LicenseService::fuer_kanal))
}

pub fn state() -> LicenseState {
    service().state()
}

/// Erlaubt Tests, einen Zustand vorzugeben.
///
/// Nur unter `cfg(test)` vorhanden - in einem ausgelieferten Programm gibt es
/// diesen Weg nicht, und `release_hardening.rs` prueft das. Der Wert gilt je
/// Thread, damit parallel laufende Tests sich nicht gegenseitig stoeren.
#[cfg(test)]
thread_local! {
    static TESTZUSTAND: std::cell::RefCell<Option<LicenseState>> =
        const { std::cell::RefCell::new(None) };
}

/// Setzt den Zustand fuer diesen Thread. `None` stellt das normale Verhalten
/// wieder her.
#[cfg(test)]
pub fn test_override(zustand: Option<LicenseState>) {
    TESTZUSTAND.with(|z| *z.borrow_mut() = zustand);
}

/// Fuehrt `f` mit einem vorgegebenen Zustand aus und raeumt danach auf - auch
/// wenn `f` in Panik geraet.
#[cfg(test)]
pub fn mit_zustand<T>(zustand: LicenseState, f: impl FnOnce() -> T) -> T {
    struct Aufraeumer;
    impl Drop for Aufraeumer {
        fn drop(&mut self) {
            test_override(None);
        }
    }
    test_override(Some(zustand));
    let _a = Aufraeumer;
    f()
}

/// Die Sperre, die [`crate::journal::plan`] aufruft.
///
/// Sie steht bewusst dort und nirgends sonst: an der Stelle, an der jede
/// veraendernde Operation ohnehin vorbeikommt.
pub fn ensure_may_modify(conn: &rusqlite::Connection) -> Result<()> {
    #[cfg(test)]
    {
        let _ = conn;
        let vorgabe = TESTZUSTAND.with(|z| z.borrow().clone());
        if let Some(z) = vorgabe {
            return if matches!(
                z,
                LicenseState::Community | LicenseState::Development | LicenseState::Purchased | LicenseState::TrialActive { .. }
            ) {
                Ok(())
            } else {
                Err(AppError::LicenseRequired)
            };
        }
        // Bestehende Kern-/Safety-Tests laufen ohne globalen App-Startup.
        // Nur dieser Testpfad entspricht dem Entwicklungszustand; Tests der
        // Lizenzsperre setzen oben immer eine explizite Vorgabe.
        Ok(())
    }
    #[cfg(not(test))]
    {
        service().authorize_mutation(conn)
    }
}

/// Anbieter mit festem Zustand. Ausserhalb von Tests unbenutzt.
#[cfg(test)]
struct Fest(LicenseState);

#[cfg(test)]
impl LicenseProvider for Fest {
    fn state(&self) -> LicenseState {
        self.0.clone()
    }
    fn name(&self) -> &'static str {
        "test"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestClock(std::sync::atomic::AtomicI64);

    impl TestClock {
        fn new(now: i64) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self(std::sync::atomic::AtomicI64::new(now)))
        }

        fn set(&self, now: i64) {
            self.0.store(now, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> i64 {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    struct MutableProvider(std::sync::Arc<parking_lot::RwLock<LicenseState>>);
    impl LicenseProvider for MutableProvider {
        fn state(&self)->LicenseState{self.0.read().clone()}
        fn name(&self)->&'static str{"mutable-test"}
    }

    fn dienst(z: LicenseState) -> LicenseService {
        let d = LicenseService::new(Box::new(Fest(z)));
        let now = crate::db::now_ts();
        *d.init.write() = InitState::Loaded;
        *d.erstkontakt.write() = Some(now);
        *d.zuletzt_gesehen.write() = Some(now);
        d
    }

    #[test]
    fn community_gekauft_und_testphase_duerfen_veraendern() {
        assert!(dienst(LicenseState::Community).may_modify());
        assert!(dienst(LicenseState::Purchased).may_modify());
        assert!(dienst(LicenseState::TrialActive { days_left: Some(6) }).may_modify());
        assert!(dienst(LicenseState::Development).may_modify());
    }

    #[test]
    fn abgelaufene_testphase_darf_nicht_veraendern() {
        assert!(!dienst(LicenseState::TrialExpired).may_modify());
    }

    /// Der wichtigste Fall: Microsoft ist kurz nicht erreichbar. Wer gekauft
    /// hat, arbeitet weiter.
    #[test]
    fn wer_gekauft_hat_bleibt_bei_stoerung_freigeschaltet() {
        let d = dienst(LicenseState::StoreUnavailable { reason: "offline".into() });
        *d.zuletzt_bekannt.write() = Some(LicenseState::Purchased);
        assert_eq!(d.state(), LicenseState::Purchased);
        assert!(d.may_modify());
    }

    /// Und die Gegenprobe: Offlinegehen macht aus einer abgelaufenen
    /// Testphase keine dauerhafte Vollversion.
    #[test]
    fn offline_verlaengert_keine_abgelaufene_testphase() {
        let d = dienst(LicenseState::StoreUnavailable { reason: "offline".into() });
        *d.zuletzt_bekannt.write() = Some(LicenseState::TrialExpired);
        assert_eq!(d.state(), LicenseState::TrialExpired);
        assert!(!d.may_modify());
    }

    /// Ohne jede eindeutige Auskunft gilt eine Frist - kurz genug, dass sie
    /// keine Gratisnutzung ist, lang genug fuer eine Stoerung.
    #[test]
    fn ohne_auskunft_gilt_eine_befristete_kulanz() {
        let d = dienst(LicenseState::StoreUnavailable { reason: "kein Paket".into() });
        *d.erstkontakt.write() = Some(crate::db::now_ts());
        assert!(d.may_modify(), "am ersten Tag darf gearbeitet werden");

        *d.erstkontakt.write() = Some(crate::db::now_ts() - KULANZ_SEKUNDEN - 1);
        assert!(!d.may_modify(), "nach Ablauf der Frist nicht mehr");
    }

    /// `StoreUnavailable` ist weder "gekauft" noch "abgelaufen". Das muss auch
    /// dann gelten, wenn nichts zwischengespeichert ist.
    #[test]
    fn unsicherer_zustand_wird_nicht_zu_einer_aussage() {
        let d = dienst(LicenseState::StoreUnavailable { reason: "x".into() });
        assert!(matches!(d.state(), LicenseState::StoreUnavailable { .. }));
        let d = dienst(LicenseState::Unknown);
        assert_eq!(d.state(), LicenseState::Unknown);
    }

    /// Nur eindeutige Zustaende werden gemerkt. Sonst koennte eine Stoerung
    /// den Zwischenspeicher entwerten.
    #[test]
    fn nur_eindeutige_zustaende_werden_gemerkt() {
        let dir = crate::testutil::TempDir::new("lizenz-cache");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let d = dienst(LicenseState::Purchased);

        d.merken(&conn, &LicenseState::StoreUnavailable { reason: "x".into() }).unwrap();
        assert!(crate::db::get_setting(&conn, SETTING_LAST_KNOWN).unwrap().is_none());

        d.merken(&conn, &LicenseState::Purchased).unwrap();
        assert!(crate::db::get_setting(&conn, SETTING_LAST_KNOWN).unwrap().is_some());
    }

    /// Der Anker der Kulanzfrist wird beim ersten Start gesetzt und danach
    /// nicht mehr verschoben - ein Neustart verlaengert nichts.
    #[test]
    fn der_kulanzanker_wird_nur_einmal_gesetzt() {
        let dir = crate::testutil::TempDir::new("lizenz-anker");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();

        let d1 = dienst(LicenseState::Unknown);
        d1.laden(&conn, true).unwrap();
        let erst = (*d1.erstkontakt.read()).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        let d2 = dienst(LicenseState::Unknown);
        d2.laden(&conn, false).unwrap();
        assert_eq!((*d2.erstkontakt.read()).unwrap(), erst);
    }

    #[test]
    fn purchased_ignoriert_grosse_uhr_rueckstellung_und_verbrauchte_toleranz(){
        const T0:i64=2_000_000_000;let dir=crate::testutil::TempDir::new("purchase-clock");let db=crate::db::Db::open(dir.path().join("i.db")).unwrap();let conn=db.conn().unwrap();let clock=TestClock::new(T0);
        let service=LicenseService::with_clock(Box::new(Fest(LicenseState::Purchased)),clock.clone());service.laden(&conn,true).unwrap();*service.zuletzt_gesehen.write()=Some(T0+48*3600);*service.rollback_toleriert.write()=true;clock.set(T0-24*3600);
        assert!(service.authorize_mutation(&conn).is_ok());
    }

    #[test]
    fn store_stoerung_nach_persistiertem_kauf_bleibt_auch_beim_authorisieren_frei(){
        let dir=crate::testutil::TempDir::new("purchase-offline-authorize");let db=crate::db::Db::open(dir.path().join("i.db")).unwrap();let conn=db.conn().unwrap();let first=dienst(LicenseState::Purchased);first.laden(&conn,true).unwrap();first.merken(&conn,&LicenseState::Purchased).unwrap();
        let restarted=dienst(LicenseState::StoreUnavailable{reason:"offline".into()});restarted.laden(&conn,false).unwrap();assert!(restarted.authorize_mutation(&conn).is_ok());
    }

    #[test]
    fn kaputte_grace_wird_durch_spaeteren_store_kauf_sofort_ueberstimmt(){
        let shared=std::sync::Arc::new(parking_lot::RwLock::new(LicenseState::StoreUnavailable{reason:"offline".into()}));
        let dir=crate::testutil::TempDir::new("purchase-after-load-failure");let db=crate::db::Db::open(dir.path().join("i.db")).unwrap();let conn=db.conn().unwrap();
        crate::db::set_setting(&conn,SETTING_STATE_VERSION,STATE_VERSION).unwrap();crate::db::set_setting(&conn,SETTING_FIRST_SEEN,"broken").unwrap();crate::db::set_setting(&conn,SETTING_LAST_SEEN,"broken").unwrap();crate::db::set_setting(&conn,SETTING_ROLLBACK_USED,"0").unwrap();
        let service=LicenseService::new(Box::new(MutableProvider(shared.clone())));assert!(service.laden(&conn,false).is_err());assert!(!service.may_modify());
        *shared.write()=LicenseState::Purchased;assert!(service.authorize_mutation(&conn).is_ok());
        let restarted=dienst(LicenseState::StoreUnavailable{reason:"offline".into()});assert!(restarted.laden(&conn,false).is_ok());assert!(restarted.may_modify());
    }

    #[test]
    fn trial_expired_wird_durch_uhr_oder_store_stoerung_nicht_frei(){
        let shared=std::sync::Arc::new(parking_lot::RwLock::new(LicenseState::TrialExpired));let clock=TestClock::new(2_000_000_000);let service=LicenseService::with_clock(Box::new(MutableProvider(shared.clone())),clock.clone());let dir=crate::testutil::TempDir::new("expired-transition");let db=crate::db::Db::open(dir.path().join("i.db")).unwrap();let conn=db.conn().unwrap();service.laden(&conn,true).unwrap();service.merken(&conn,&LicenseState::TrialExpired).unwrap();clock.set(2_100_000_000);assert!(service.authorize_mutation(&conn).is_err());*shared.write()=LicenseState::StoreUnavailable{reason:"offline".into()};assert_eq!(service.state(),LicenseState::TrialExpired);assert!(service.authorize_mutation(&conn).is_err());
    }

    #[test]
    fn gekaufter_status_ueberlebt_einen_neustart_bei_store_ausfall() {
        let dir = crate::testutil::TempDir::new("lizenz-neustart-kauf");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let erster = dienst(LicenseState::Purchased);
        erster.laden(&conn, true).unwrap();
        erster.merken(&conn, &LicenseState::Purchased).unwrap();
        drop(conn);

        let conn = db.conn().unwrap();
        let nach_neustart = dienst(LicenseState::StoreUnavailable { reason: "offline".into() });
        nach_neustart.laden(&conn, false).unwrap();
        assert_eq!(nach_neustart.state(), LicenseState::Purchased);
        assert!(nach_neustart.may_modify());
    }

    #[test]
    fn unsichere_gespeicherte_zustaende_erteilen_keine_dauerfreigabe() {
        let dir = crate::testutil::TempDir::new("lizenz-unsicher-cache");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        for gespeichert in [
            LicenseState::TrialActive { days_left: Some(5) },
            LicenseState::StoreUnavailable { reason: "alt".into() },
        ] {
            crate::db::set_setting(
                &conn,
                SETTING_LAST_KNOWN,
                &serde_json::to_string(&gespeichert).unwrap(),
            )
            .unwrap();
            crate::db::set_setting(
                &conn,
                SETTING_FIRST_SEEN,
                &(crate::db::now_ts() - KULANZ_SEKUNDEN - 1).to_string(),
            )
            .unwrap();
            let neu = dienst(LicenseState::StoreUnavailable { reason: "jetzt".into() });
            neu.laden(&conn, false).unwrap();
            assert!(matches!(neu.state(), LicenseState::StoreUnavailable { .. }));
            assert!(!neu.may_modify());
        }
    }

    #[test]
    fn kulanz_laeuft_ab_und_neustart_setzt_sie_nicht_zurueck() {
        let dir = crate::testutil::TempDir::new("lizenz-kulanz-neustart");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        crate::db::set_setting(
            &conn,
            SETTING_FIRST_SEEN,
            &(crate::db::now_ts() - KULANZ_SEKUNDEN).to_string(),
        )
        .unwrap();
        drop(conn);

        for _ in 0..2 {
            let conn = db.conn().unwrap();
            let neu = dienst(LicenseState::Unknown);
            neu.laden(&conn, false).unwrap();
            assert!(!neu.may_modify());
        }
    }

    #[test]
    fn fehlender_anker_startet_einmalig_kaputter_anker_sperrt_konservativ() {
        let dir = crate::testutil::TempDir::new("lizenz-anker-konservativ");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();

        let frisch = dienst(LicenseState::Unknown);
        frisch.laden(&conn, true).unwrap();
        assert!(frisch.may_modify());
        assert!(crate::db::get_setting(&conn, SETTING_FIRST_SEEN).unwrap().is_some());

        crate::db::set_setting(&conn, SETTING_FIRST_SEEN, "kein-zeitstempel").unwrap();
        crate::db::set_setting(&conn, SETTING_LAST_KNOWN, "{kaputt").unwrap();
        let kaputt = dienst(LicenseState::Unknown);
        assert!(kaputt.laden(&conn, false).is_err());
        assert!(!kaputt.may_modify());
        assert_eq!(*kaputt.zuletzt_bekannt.read(), None);
    }

    #[test]
    fn ungeladener_dienst_und_bestehende_db_ohne_anker_sperren() {
        let ungeladen = LicenseService::new(Box::new(Fest(LicenseState::Purchased)));
        assert!(!ungeladen.may_modify(), "auch Purchased braucht einen geladenen Startup-Zustand");

        let dir = crate::testutil::TempDir::new("lizenz-fehlender-marker");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let bestehend = LicenseService::new(Box::new(Fest(LicenseState::Unknown)));
        assert!(bestehend.laden(&conn, false).is_err());
        assert!(!bestehend.may_modify());
        assert!(crate::db::get_setting(&conn, SETTING_FIRST_SEEN).unwrap().is_none());
    }

    #[test]
    fn startup_laesst_purchased_und_aktive_trial_erst_nach_laden_zu() {
        for state in [LicenseState::Purchased, LicenseState::TrialActive { days_left: Some(5) }] {
            let dir = crate::testutil::TempDir::new("lizenz-startup-loaded");
            let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
            let conn = db.conn().unwrap();
            let service = LicenseService::new(Box::new(Fest(state)));
            assert!(!service.may_modify());
            service.laden(&conn, true).unwrap();
            assert!(service.may_modify());
        }
    }

    #[test]
    fn temporaerer_db_fehler_und_spaetere_leere_db_erzeugen_keine_grace() {
        let service = LicenseService::new(Box::new(Fest(LicenseState::Unknown)));
        service.mark_load_failed();
        assert!(!service.may_modify());

        let dir = crate::testutil::TempDir::new("lizenz-db-spaeter");
        let db = crate::db::Db::open(dir.path().join("bestehend.db")).unwrap();
        let conn = db.conn().unwrap();
        assert!(service.laden(&conn, false).is_err());
        assert!(!service.may_modify());
        assert!(crate::db::get_setting(&conn, SETTING_FIRST_SEEN).unwrap().is_none());
    }

    #[test]
    fn kulanz_72h_und_neustart_laufen_mit_kontrollierter_uhr_ab() {
        const T0: i64 = 2_000_000_000;
        let dir = crate::testutil::TempDir::new("lizenz-uhr-72h");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let clock = TestClock::new(T0);
        let first = LicenseService::with_clock(
            Box::new(Fest(LicenseState::StoreUnavailable { reason: "offline".into() })),
            clock.clone(),
        );
        first.laden(&conn, true).unwrap();
        assert!(first.may_modify());
        clock.set(T0 + 24 * 3600);
        assert!(first.may_modify());
        clock.set(T0 + 71 * 3600);
        assert!(first.may_modify());

        let restarted = LicenseService::with_clock(
            Box::new(Fest(LicenseState::Unknown)),
            clock.clone(),
        );
        restarted.laden(&conn, false).unwrap();
        clock.set(T0 + 73 * 3600);
        assert!(!restarted.may_modify());
        assert_eq!(
            crate::db::get_setting(&conn, SETTING_FIRST_SEEN).unwrap().as_deref(),
            Some(T0.to_string().as_str())
        );
    }

    #[test]
    fn deutliche_systemzeit_rueckstellung_sperrt_auch_nach_neustart() {
        const T0: i64 = 2_000_000_000;
        let dir = crate::testutil::TempDir::new("lizenz-uhr-rueckwaerts");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let clock = TestClock::new(T0);
        let first = LicenseService::with_clock(Box::new(Fest(LicenseState::Unknown)), clock.clone());
        first.laden(&conn, true).unwrap();
        clock.set(T0 + 48 * 3600);
        first.authorize_mutation(&conn).unwrap();

        clock.set(T0 + 12 * 3600);
        assert!(!first.may_modify());
        let restarted = LicenseService::with_clock(Box::new(Fest(LicenseState::Unknown)), clock);
        assert!(restarted.laden(&conn, false).is_err());
        assert!(!restarted.may_modify());
    }

    #[test]
    fn wiederholte_kleine_rueckstellungen_koennen_kulanz_nicht_einfrieren() {
        const T0: i64 = 2_000_000_000;
        let dir = crate::testutil::TempDir::new("lizenz-kleine-rueckstellungen");
        let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        let clock = TestClock::new(T0);
        let service = LicenseService::with_clock(
            Box::new(Fest(LicenseState::StoreUnavailable { reason: "offline".into() })),
            clock.clone(),
        );
        service.laden(&conn, true).unwrap();
        clock.set(T0 + 71 * 3600);
        service.authorize_mutation(&conn).unwrap();

        clock.set(T0 + 71 * 3600 - 4 * 60);
        service.authorize_mutation(&conn).unwrap();
        assert!(service.authorize_mutation(&conn).is_err(), "die Toleranz darf nicht erneuerbar sein");

        // Nach dem Aufholen ist die Uhr wieder plausibel, aber die einmalige
        // Toleranz bleibt verbraucht. Auch nach weiterem Vorwaertslauf darf
        // ein zweiter kleiner Ruecksprung nicht akzeptiert werden.
        clock.set(T0 + 71 * 3600);
        service.authorize_mutation(&conn).unwrap();
        clock.set(T0 + 71 * 3600 + 4 * 60);
        service.authorize_mutation(&conn).unwrap();
        clock.set(T0 + 71 * 3600);
        assert!(service.authorize_mutation(&conn).is_err());

        let restarted = LicenseService::with_clock(Box::new(Fest(LicenseState::Unknown)), clock);
        restarted.laden(&conn, false).unwrap();
        assert!(!restarted.may_modify(), "der Verdacht muss einen Neustart ueberleben");
    }

    #[test]
    fn uhr_rueckstellung_hat_klare_fuenf_minuten_grenze() {
        const T0: i64 = 2_000_000_000;
        for (delta, first_allowed) in [(60, true), (299, true), (301, false), (3600, false)] {
            let dir = crate::testutil::TempDir::new(&format!("lizenz-clock-boundary-{delta}"));
            let db = crate::db::Db::open(dir.path().join("i.db")).unwrap();
            let conn = db.conn().unwrap();
            let clock = TestClock::new(T0);
            let service = LicenseService::with_clock(
                Box::new(Fest(LicenseState::StoreUnavailable { reason: "offline".into() })),
                clock.clone(),
            );
            service.laden(&conn, true).unwrap();
            let highwater = T0 + 71 * 3600;
            clock.set(highwater);
            service.authorize_mutation(&conn).unwrap();
            clock.set(highwater - delta);
            assert_eq!(service.authorize_mutation(&conn).is_ok(), first_allowed, "delta={delta}");
            if first_allowed {
                assert!(service.authorize_mutation(&conn).is_err(), "Toleranz ist nur einmal nutzbar");
            }
        }
    }

    #[test]
    fn installation_verlangt_initialisierten_dienst_und_ist_einmalig() {
        let cell = OnceLock::new();
        assert!(install_into(
            &cell,
            LicenseService::new(Box::new(Fest(LicenseState::Purchased)))
        )
        .is_err());

        let service = LicenseService::new(Box::new(Fest(LicenseState::Unknown)));
        service.mark_load_failed();
        install_into(&cell, service).unwrap();
        let second = LicenseService::new(Box::new(Fest(LicenseState::Unknown)));
        second.mark_load_failed();
        assert!(install_into(&cell, second).is_err());
    }

    #[test]
    fn zustaende_serialisieren_stabil() {
        for z in [
            LicenseState::Community,
            LicenseState::Development,
            LicenseState::TrialActive { days_left: Some(3) },
            LicenseState::TrialExpired,
            LicenseState::Purchased,
            LicenseState::StoreUnavailable { reason: "x".into() },
            LicenseState::Unknown,
        ] {
            let s = serde_json::to_string(&z).unwrap();
            let zurueck: LicenseState = serde_json::from_str(&s).unwrap();
            assert_eq!(z, zurueck, "{s}");
            assert!(s.contains(z.as_str()), "{s} nennt {} nicht", z.as_str());
        }
    }

    #[test]
    fn ohne_paketidentitaet_gibt_es_keine_store_lizenz() {
        // Der Testprozess laeuft nicht als Paket.
        assert!(!MicrosoftStoreLicenseProvider::hat_paketidentitaet());
        assert!(paket_voller_name().is_none());
        let z = MicrosoftStoreLicenseProvider.state();
        assert!(matches!(z, LicenseState::StoreUnavailable { .. }), "{z:?}");
    }

    // -----------------------------------------------------------------
    // Die Sperre, durch die echten Dateioperationen hindurch
    // -----------------------------------------------------------------

    fn testordner(name: &str) -> (crate::testutil::TempDir, crate::db::Db) {
        let dir = crate::testutil::TempDir::new(name);
        let db = crate::db::Db::open(dir.path().join("idx").join("i.db")).unwrap();
        (dir, db)
    }

    /// Nach Ablauf der Testphase darf keine Datei mehr bewegt werden - und
    /// zwar nicht, weil eine Schaltflaeche ausgegraut ist, sondern weil der
    /// Kern es ablehnt.
    #[test]
    fn abgelaufene_testphase_sperrt_verschieben_umbenennen_und_papierkorb() {
        let (dir, db) = testordner("sperre-ops");
        let quelle = dir.file("wichtig.txt", b"Inhalt");
        let ziel = dir.dir("ziel");
        let conn = db.conn().unwrap();

        mit_zustand(LicenseState::TrialExpired, || {
            let e = crate::fileops::move_file(
                &conn,
                &quelle,
                &ziel,
                crate::fileops::CollisionPolicy::Rename,
                None,
            );
            assert_eq!(e.status, crate::fileops::OpStatus::Failed);
            assert_eq!(e.error_kind.as_deref(), Some("license_required"), "{e:?}");

            let e = crate::fileops::rename_file(&conn, &quelle, "anders.txt");
            assert_eq!(e.error_kind.as_deref(), Some("license_required"), "{e:?}");

            let e = crate::fileops::delete_file(
                &conn,
                &quelle,
                crate::config::DeleteMode::Recycle,
                None,
            );
            assert_eq!(e.error_kind.as_deref(), Some("license_required"), "{e:?}");
        });

        // Und das Entscheidende: Es wurde nichts angefasst.
        assert!(quelle.exists(), "die Quelldatei muss unberuehrt liegen bleiben");
        assert_eq!(std::fs::read(&quelle).unwrap(), b"Inhalt");
        assert_eq!(std::fs::read_dir(&ziel).unwrap().count(), 0);
    }

    /// Die Sperre greift, *bevor* etwas geschieht. Das Journal darf keinen
    /// angefangenen Vorgang zuruecklassen - sonst meldete die
    /// Wiederherstellung beim naechsten Start eine Operation, die es nie gab.
    #[test]
    fn eine_gesperrte_aktion_hinterlaesst_keinen_journaleintrag() {
        let (dir, db) = testordner("sperre-journal");
        let quelle = dir.file("a.txt", b"x");
        let ziel = dir.dir("ziel");
        let conn = db.conn().unwrap();

        mit_zustand(LicenseState::TrialExpired, || {
            let _ = crate::fileops::move_file(
                &conn,
                &quelle,
                &ziel,
                crate::fileops::CollisionPolicy::Rename,
                None,
            );
        });

        let offen = crate::journal::unfinished(&conn).unwrap();
        assert!(offen.is_empty(), "kein halber Vorgang: {offen:?}");
        let alle: i64 =
            conn.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert_eq!(alle, 0, "die Sperre darf nichts protokollieren");
    }

    /// Rueckgaengig bleibt erlaubt. Wem mitten in der Arbeit die Testphase
    /// auslaeuft, der darf nicht mit einem halb sortierten Ordner sitzen
    /// bleiben.
    #[test]
    fn rueckgaengig_funktioniert_auch_nach_ablauf_der_testphase() {
        let (dir, db) = testordner("sperre-undo");
        let quelle = dir.file("brief.txt", b"Inhalt");
        let ziel = dir.dir("ziel");
        let conn = db.conn().unwrap();

        // Waehrend der Testphase verschoben.
        let erg = mit_zustand(LicenseState::TrialActive { days_left: Some(1) }, || {
            crate::fileops::move_file(
                &conn,
                &quelle,
                &ziel,
                crate::fileops::CollisionPolicy::Rename,
                None,
            )
        });
        assert_eq!(erg.status, crate::fileops::OpStatus::Done);
        assert!(ziel.join("brief.txt").exists());

        // Testphase laeuft ab - die Ruecknahme muss trotzdem gehen.
        mit_zustand(LicenseState::TrialExpired, || {
            let z = crate::undo::undo_last(&conn).unwrap().expect("es gibt etwas zurueckzunehmen");
            assert!(z.ok, "{z:?}");
        });
        assert!(quelle.exists(), "die Datei muss zurueck sein");
        assert!(!ziel.join("brief.txt").exists());
    }

    /// Waehrend der Testphase ist alles erlaubt - eine Testversion, die nur
    /// die Haelfte kann, ist keine.
    #[test]
    fn waehrend_der_testphase_ist_nichts_gesperrt() {
        let (dir, db) = testordner("sperre-trial");
        let quelle = dir.file("bild.png", b"x");
        let ziel = dir.dir("ziel");
        let conn = db.conn().unwrap();

        mit_zustand(LicenseState::TrialActive { days_left: Some(7) }, || {
            let e = crate::fileops::move_file(
                &conn,
                &quelle,
                &ziel,
                crate::fileops::CollisionPolicy::Rename,
                None,
            );
            assert_eq!(e.status, crate::fileops::OpStatus::Done, "{e:?}");
        });
        assert!(ziel.join("bild.png").exists());
    }

    #[test]
    fn community_operation_bleibt_journal_und_recovery_gesichert() {
        let (dir, db) = testordner("community-journal");
        let quelle = dir.file("community.txt", b"journalpflichtig");
        let ziel = dir.dir("ziel");
        let conn = db.conn().unwrap();

        let ergebnis = mit_zustand(LicenseState::Community, || {
            crate::fileops::move_file(
                &conn,
                &quelle,
                &ziel,
                crate::fileops::CollisionPolicy::Rename,
                None,
            )
        });

        assert_eq!(ergebnis.status, crate::fileops::OpStatus::Done, "{ergebnis:?}");
        assert!(!quelle.exists());
        assert!(ziel.join("community.txt").exists());
        let journal_count: i64 = conn
            .query_row("SELECT count(*) FROM actions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_count, 1, "Community darf das Journal nicht umgehen");
        let undo = crate::undo::undo_last(&conn).unwrap().expect("Journalaktion fehlt");
        assert!(undo.ok, "{undo:?}");
        assert!(quelle.exists());
    }

    /// Auch der Stapelweg ueber eine Regel geht durch dieselbe Sperre. Er
    /// braucht keine eigene Pruefung - und darf auch keine haben, sonst gibt
    /// es zwei Wahrheiten.
    #[test]
    fn auch_eine_regel_kommt_nicht_an_der_sperre_vorbei() {
        use crate::rules::{Condition, Rule};
        let dir = crate::testutil::TempDir::new("sperre-regel");
        let quelle = dir.dir("quelle");
        for i in 0..3 {
            std::fs::write(quelle.join(format!("rechnung{i}.pdf")), b"x").unwrap();
        }
        let ziel = dir.dir("ziel");
        let db = crate::db::Db::open(dir.path().join("idx").join("i.db")).unwrap();
        crate::scan::scan(
            &db,
            &crate::scan::ScanOptions {
                root: quelle.clone(),
                settings: crate::config::ScanSettings::default(),
            },
            &std::sync::atomic::AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let conn = db.conn().unwrap();
        let id = crate::rules::save(
            &conn,
            &Rule {
                id: None,
                name: "Test".into(),
                enabled: true,
                priority: 100,
                conditions: vec![Condition {
                    field: "ext".into(),
                    op: "in".into(),
                    value: serde_json::json!(["pdf"]),
                }],
                match_mode: "all".into(),
                target: ziel.display().to_string(),
                source: "user".into(),
                created_at: 0,
                hits: 0,
            },
        )
        .unwrap();

        mit_zustand(LicenseState::TrialExpired, || {
            let erg = crate::rules::apply(
                &conn,
                id,
                None,
                crate::fileops::CollisionPolicy::Rename,
                "b1",
                Some(3),
            )
            .unwrap();
            assert!(
                erg.iter().all(|r| r.error_kind.as_deref() == Some("license_required")),
                "{erg:?}"
            );
        });
        assert_eq!(std::fs::read_dir(&ziel).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&quelle).unwrap().count(), 3);
    }

    /// Lesen bleibt vollstaendig erlaubt. Ein abgelaufener Test soll das
    /// Programm nicht unbrauchbar machen, sondern nur das Veraendern sperren.
    #[test]
    fn lesen_bleibt_nach_ablauf_erlaubt() {
        let dir = crate::testutil::TempDir::new("sperre-lesen");
        dir.file("a.txt", b"x");
        dir.file("b.txt", b"y");
        let db = crate::db::Db::open(dir.path().join("idx").join("i.db")).unwrap();

        mit_zustand(LicenseState::TrialExpired, || {
            // Scannen, Abfragen und Duplikatsuche sind Lesevorgaenge.
            crate::scan::scan(
                &db,
                &crate::scan::ScanOptions {
                    root: dir.path().to_path_buf(),
                    settings: crate::config::ScanSettings::default(),
                },
                &std::sync::atomic::AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
            let conn = db.conn().unwrap();
            let seite = crate::query::list_files(&conn, &Default::default()).unwrap();
            assert!(seite.total >= 2, "der Index muss lesbar bleiben");
            crate::dup::find(
                &db,
                None,
                1,
                &std::sync::atomic::AtomicBool::new(false),
                &mut |_| {},
            )
            .unwrap();
        });
    }

    #[cfg(any(debug_assertions, feature = "dev-license"))]
    #[test]
    fn die_entwicklungsschalter_ergeben_die_erwarteten_zustaende() {
        use dev::parse;
        assert_eq!(parse("purchased"), LicenseState::Purchased);
        assert_eq!(parse("trial_expired"), LicenseState::TrialExpired);
        assert_eq!(parse("trial_active:3"), LicenseState::TrialActive { days_left: Some(3) });
        assert_eq!(parse("trial_active"), LicenseState::TrialActive { days_left: Some(7) });
        assert_eq!(parse("unknown"), LicenseState::Unknown);
        assert!(matches!(parse("store_unavailable"), LicenseState::StoreUnavailable { .. }));
        assert_eq!(parse("quatsch"), LicenseState::Development);
    }
}

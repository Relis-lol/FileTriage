//! Dateioperationen.
//!
//! Der gefaehrlichste Teil des Programms, deshalb der strengste. Die Regeln,
//! die hier nirgends umgangen werden:
//!
//! - **Erst schreiben, dann handeln.** Vor jeder Veraenderung steht ein
//!   festgeschriebener Journaleintrag mit der Absicht.
//! - **Nur echte Dateien.** Verknuepfungen und Cloud-Platzhalter werden nicht
//!   angefasst, weil unklar ist, was tatsaechlich betroffen waere.
//! - **Nie blind ueberschreiben.** Eine bestehende Datei am Ziel fuehrt zur
//!   Rueckfrage, nie zum Verlust.
//! - **Ueber Volumegrenzen wird geprueft, bevor die Quelle faellt.** Erst wenn
//!   die Kopie nachweislich vollstaendig ist, verschwindet das Original.
//! - **Time-of-check ist nicht Time-of-use.** Unmittelbar vor dem
//!   unwiderruflichen Schritt wird erneut geprueft, ob es noch dieselbe Datei ist.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::classify;
use crate::config::DeleteMode;
use crate::db;
use crate::error::{AppError, Result};
use crate::fsmeta::{self, FileState, Fingerprint};
use crate::journal::{self, Intent, OpState, VerifyLevel};
use crate::paths;

/// Bis zu dieser Groesse wird eine Kopie ueber Volumegrenzen vollstaendig
/// gehasht. Darueber wird stichprobenartig geprueft und das im Journal
/// vermerkt - behauptet wird nie mehr, als geprueft wurde.
pub const FULL_VERIFY_LIMIT: u64 = 512 * 1024 * 1024;

/// Sicherheitsabstand beim Platzcheck, damit nicht die letzte Reserve des
/// Volumes aufgebraucht wird.
const PLATZ_RESERVE: u64 = 8 * 1024 * 1024;

/// Wie mit einer bestehenden Datei am Ziel umgegangen wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// Nicht ausfuehren, sondern zurueckmelden und den Nutzer fragen.
    #[default]
    Ask,
    /// Datei am Ziel behalten, Quelle unangetastet lassen.
    Skip,
    /// Automatisch einen freien Namen waehlen: "Datei (2).pdf".
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    Done,
    Skipped,
    /// Am Ziel liegt bereits eine Datei; es wurde nichts veraendert.
    Collision,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpResult {
    pub status: OpStatus,
    pub src: String,
    pub dst: Option<String>,
    /// ID im Journal, sofern eine Absicht festgeschrieben wurde.
    pub action_id: Option<i64>,
    pub error_kind: Option<String>,
    pub message: Option<String>,
    /// Bei Kollision: Angaben zur bereits vorhandenen Datei fuer den Vergleich.
    pub existing: Option<ExistingFile>,
    /// Wie gruendlich eine Kopie geprueft wurde.
    pub verified: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExistingFile {
    pub path: String,
    pub size: i64,
    pub modified: i64,
    pub suggested_name: String,
}

impl OpResult {
    fn failed(src: &Path, err: AppError) -> OpResult {
        OpResult {
            status: OpStatus::Failed,
            src: paths::display_path(src),
            dst: None,
            action_id: None,
            error_kind: Some(err.kind().to_string()),
            message: Some(err.to_string()),
            existing: None,
            verified: None,
        }
    }

    fn ok(src: &Path, dst: &Path, id: i64, verify: VerifyLevel) -> OpResult {
        OpResult {
            status: OpStatus::Done,
            src: paths::display_path(src),
            dst: Some(paths::display_path(dst)),
            action_id: Some(id),
            error_kind: None,
            message: None,
            existing: None,
            verified: Some(verify.as_str().to_string()),
        }
    }

    fn skipped(src: &Path, grund: &str) -> OpResult {
        OpResult {
            status: OpStatus::Skipped,
            src: paths::display_path(src),
            dst: None,
            action_id: None,
            error_kind: None,
            message: Some(grund.to_string()),
            existing: None,
            verified: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Fehlerinjektion fuer Crash-Tests
// ---------------------------------------------------------------------------

/// Punkte, an denen ein Test den Prozess "sterben" lassen kann.
///
/// Ein echter Prozessabbruch laesst Dateisystem und Journal genau so stehen,
/// wie sie in diesem Moment sind. Genau das bildet die Injektion nach: Sie
/// bricht ohne jede Aufraeumarbeit ab.
#[cfg(test)]
pub mod crashpoint {
    use std::cell::{Cell, RefCell};

    thread_local! {
        static ARMED: RefCell<Option<&'static str>> = const { RefCell::new(None) };
        /// Erzwingt den Kopierpfad, auch wenn Quelle und Ziel auf demselben
        /// Volume liegen. In der Testumgebung steht kein zweites Volume zur
        /// Verfuegung; die Logik ist dieselbe.
        static FORCE_COPY: Cell<bool> = const { Cell::new(false) };
        static FORCE_RENAME_PATH: Cell<bool> = const { Cell::new(false) };
        static HOOK: RefCell<Option<Box<dyn FnMut(&str, &std::path::Path, &std::path::Path)>>> = RefCell::new(None);
    }

    /// Bewaffnet den benannten Punkt fuer den aktuellen Thread.
    pub fn arm(name: &'static str) {
        ARMED.with(|a| *a.borrow_mut() = Some(name));
    }

    pub fn disarm() {
        ARMED.with(|a| *a.borrow_mut() = None);
    }

    pub fn triggered(name: &str) -> bool {
        ARMED.with(|a| {
            let mut b = a.borrow_mut();
            if *b == Some(name) {
                *b = None;
                true
            } else {
                false
            }
        })
    }

    pub fn force_copy_path(an: bool) {
        FORCE_COPY.with(|f| f.set(an));
    }

    pub fn copy_path_forced() -> bool {
        FORCE_COPY.with(|f| f.get())
    }

    pub fn force_rename_path(an:bool){FORCE_RENAME_PATH.with(|f|f.set(an))}
    pub fn rename_path_forced()->bool{FORCE_RENAME_PATH.with(|f|f.get())}

    pub fn set_hook(hook: impl FnMut(&str,&std::path::Path,&std::path::Path)+'static){HOOK.with(|h|*h.borrow_mut()=Some(Box::new(hook)))}
    pub fn run_hook(name:&str,src:&std::path::Path,dst:&std::path::Path){HOOK.with(|h|if let Some(callback)=h.borrow_mut().as_mut(){callback(name,src,dst)})}

    /// Setzt alles zurueck - am Ende jedes Tests aufzurufen.
    pub struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            disarm();
            force_copy_path(false);
            force_rename_path(false);
            HOOK.with(|h|*h.borrow_mut()=None);
        }
    }
}

#[cfg(test)]
fn run_test_hook(name:&str,src:&Path,dst:&Path){crashpoint::run_hook(name,src,dst)}
#[cfg(not(test))]
fn run_test_hook(_name:&str,_src:&Path,_dst:&Path){}

#[cfg(test)]
pub(crate) fn run_recovery_test_hook(name:&str,src:&Path,dst:&Path){run_test_hook(name,src,dst)}
#[cfg(not(test))]
pub(crate) fn run_recovery_test_hook(_name:&str,_src:&Path,_dst:&Path){}

#[cfg(test)]
fn kopierpfad_erzwungen() -> bool {
    crashpoint::copy_path_forced()
}

#[cfg(not(test))]
fn kopierpfad_erzwungen() -> bool {
    false
}

#[cfg(test)]
fn renamepfad_erzwungen()->bool{crashpoint::rename_path_forced()}
#[cfg(not(test))]
fn renamepfad_erzwungen()->bool{false}

/// Bricht ab, als waere der Prozess an dieser Stelle gestorben.
macro_rules! crash_here {
    ($name:literal) => {
        #[cfg(test)]
        if crate::fileops::crashpoint::triggered($name) {
            return Err(AppError::Simulated($name));
        }
    };
}

#[cfg(test)]
fn ist_simulierter_absturz(e: &AppError) -> bool {
    matches!(e, AppError::Simulated(_))
}

#[cfg(not(test))]
fn ist_simulierter_absturz(_e: &AppError) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Namensfindung
// ---------------------------------------------------------------------------

/// Sucht einen freien Dateinamen im Zielordner: "Bericht (2).pdf".
pub fn unique_target(dir: &Path, file_name: &str) -> Result<PathBuf> {
    let candidate = dir.join(file_name);
    if !paths::extended(&candidate).exists() {
        return Ok(candidate);
    }
    let (stem, ext) = split_name(file_name);
    for n in 2..10_000u32 {
        let name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let candidate = dir.join(&name);
        if !paths::extended(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(AppError::Invalid(format!(
        "Kein freier Name für \"{file_name}\" in {}",
        dir.display()
    )))
}

/// Trennt "Bericht.tar.gz" in ("Bericht.tar", "gz").
fn split_name(file_name: &str) -> (String, String) {
    let trimmed = file_name.trim_start_matches('.');
    let dots = file_name.len() - trimmed.len();
    match trimmed.rfind('.') {
        Some(idx) if idx + 1 < trimmed.len() => (
            file_name[..dots + idx].to_string(),
            trimmed[idx + 1..].to_string(),
        ),
        _ => (file_name.to_string(), String::new()),
    }
}

// ---------------------------------------------------------------------------
// Pruefungen vor jeder Operation
// ---------------------------------------------------------------------------

/// Prueft Quelle und Zielordner, bevor irgendetwas angefasst wird.
///
/// Liefert den Zustand der Quelle, damit der Aufrufer ihn spaeter erneut
/// vergleichen kann.
fn validate(src: &Path, dst_dir: &Path, expected: Option<Fingerprint>) -> Result<FileState> {
    paths::validate_directory_path(dst_dir)?;
    let state = fsmeta::require_regular_file(src)?;

    if let Some(erwartet) = expected {
        if !state.fingerprint.matches(&erwartet) {
            return Err(AppError::Changed(format!(
                "{} hat sich seit der Anzeige verändert. Zur Sicherheit passiert nichts.",
                src.display()
            )));
        }
    }
    // Ziel darf nicht unterhalb der Quelle liegen.
    if paths::is_inside(dst_dir, src) {
        return Err(AppError::Invalid("Der Zielordner liegt innerhalb der Quelle".into()));
    }
    Ok(state)
}

fn ensure_dir(dst_dir: &Path) -> Result<()> {
    let edir = paths::extended(dst_dir);
    if edir.is_dir() {
        return Ok(());
    }
    if edir.exists() {
        return Err(AppError::Invalid(format!(
            "{} ist ein Ordnerziel, aber eine Datei",
            dst_dir.display()
        )));
    }
    std::fs::create_dir_all(&edir)
        .map_err(|e| AppError::from_io(&e, &format!("Zielordner {} anlegen", dst_dir.display())))
}

/// Reicht der Platz auf dem Zielvolume?
fn ensure_space(dst_dir: &Path, benoetigt: u64) -> Result<()> {
    let Some(frei) = fsmeta::free_space(dst_dir) else {
        // Kann der Platz nicht ermittelt werden, wird nicht geraten - der
        // Kopiervorgang selbst meldet ein volles Volume sauber zurueck.
        return Ok(());
    };
    if frei < benoetigt.saturating_add(PLATZ_RESERVE) {
        return Err(AppError::NoSpace(format!(
            "Auf {} sind nur {} frei, gebraucht werden {}",
            dst_dir.display(),
            menschlich(frei),
            menschlich(benoetigt)
        )));
    }
    Ok(())
}

fn menschlich(bytes: u64) -> String {
    const E: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut wert = bytes as f64;
    let mut i = 0;
    while wert >= 1024.0 && i < E.len() - 1 {
        wert /= 1024.0;
        i += 1;
    }
    format!("{wert:.1} {}", E[i])
}

// ---------------------------------------------------------------------------
// Verschieben
// ---------------------------------------------------------------------------

pub fn move_file(
    conn: &Connection,
    src: &Path,
    dst_dir: &Path,
    policy: CollisionPolicy,
    batch_id: Option<&str>,
) -> OpResult {
    move_file_checked(conn, src, dst_dir, policy, batch_id, None)
}

/// Verschiebt eine Datei und prueft dabei, dass es noch die erwartete ist.
///
/// `expected` stammt aus dem Moment, in dem der Nutzer die Datei gesehen hat.
/// Weicht die Datei davon ab, passiert nichts.
pub fn move_file_checked(
    conn: &Connection,
    src: &Path,
    dst_dir: &Path,
    policy: CollisionPolicy,
    batch_id: Option<&str>,
    expected: Option<Fingerprint>,
) -> OpResult {
    match move_inner(conn, src, dst_dir, policy, batch_id, expected) {
        Ok(r) => r,
        Err(e) => OpResult::failed(src, e),
    }
}

fn move_inner(
    conn: &Connection,
    src: &Path,
    dst_dir: &Path,
    policy: CollisionPolicy,
    batch_id: Option<&str>,
    expected: Option<Fingerprint>,
) -> Result<OpResult> {
    let _mutation = journal::begin_mutation()?;
    let quelle = validate(src, dst_dir, expected)?;

    let file_name = src
        .file_name()
        .ok_or_else(|| AppError::Invalid(format!("{} hat keinen Dateinamen", src.display())))?
        .to_string_lossy()
        .to_string();

    // Schon am Ziel? Dann ist nichts zu tun.
    if let Some(parent) = src.parent() {
        if paths::path_key(&paths::display_path(parent)) == paths::path_key(&paths::display_path(dst_dir))
        {
            return Ok(OpResult::skipped(src, "Datei liegt bereits in diesem Ordner"));
        }
    }

    let ziel = match kollision_aufloesen(src, dst_dir, &file_name, policy, &quelle)? {
        Aufloesung::Weiter(p) => p,
        Aufloesung::Antwort(r) => return Ok(r),
    };

    let ueber_volume = (!fsmeta::same_volume(src, &ziel) && !renamepfad_erzwungen()) || kopierpfad_erzwungen();
    if ueber_volume {
        ensure_space(dst_dir, quelle.fingerprint.size.max(0) as u64)?;
    }
    let src_hash = crate::dup::full_hash(src)?;
    toctou_pruefen(src, &quelle)?;

    // Ab hier ist die Absicht festgeschrieben.
    let session = crate::session_id();
    let id = journal::plan(
        conn,
        &Intent {
            kind: "move",
            src: &paths::display_path(src),
            dst: Some(&paths::display_path(&ziel)),
            original_name: &file_name,
            fingerprint: quelle.fingerprint,
            file_id: quelle.id,
            strong_file_id: fsmeta::strong_file_id(src),
            src_hash: Some(&src_hash),
            cross_volume: ueber_volume,
            undoable: true,
            batch_id,
            session_id: &session,
        },
    )?;
    crash_here!("nach_plan");

    // Auch ein neuer Zielordner ist eine Dateisystemmutation. Erst wenn die
    // Absicht dauerhaft im Journal steht, darf er angelegt werden.
    if let Err(e) = ensure_dir(dst_dir) {
        journal::fail(conn, id, OpState::Failed, &e.to_string())?;
        return Err(e);
    }
    crash_here!("nach_zielordner");

    let ergebnis = if ueber_volume {
        ueber_volume_verschieben(conn, id, src, &ziel, &quelle, &src_hash, None)
    } else {
        gleiches_volume_verschieben(conn, id, src, &ziel, &quelle, &src_hash)
    };

    match ergebnis {
        Ok(verify) => {
            let ziel_fp = fsmeta::inspect(&ziel).ok().map(|s| s.fingerprint);
            journal::complete(conn, id, ziel_fp, verify)?;
            index_after_move(conn, src, &ziel)?;
            record_move_stat(conn, &file_name, dst_dir)?;
            Ok(OpResult::ok(src, &ziel, id, verify))
        }
        Err(e) => {
            // Bei simuliertem Absturz bleibt der Journalzustand genau so
            // stehen, wie ein echter Prozesstod ihn hinterlassen haette.
            if !ist_simulierter_absturz(&e) {
                let state=journal::by_id(conn,id)?.state();
                if !matches!(state,OpState::RollbackPending|OpState::RolledBack){
                    journal::fail(conn, id, OpState::Failed, &e.to_string())?;
                }
            }
            Err(e)
        }
    }
}

enum Aufloesung {
    Weiter(PathBuf),
    Antwort(OpResult),
}

fn kollision_aufloesen(
    src: &Path,
    dst_dir: &Path,
    file_name: &str,
    policy: CollisionPolicy,
    quelle: &FileState,
) -> Result<Aufloesung> {
    let ziel = dst_dir.join(file_name);
    if !paths::extended(&ziel).exists() {
        return Ok(Aufloesung::Weiter(ziel));
    }

    // Sonderfall Hardlink: Quelle und Ziel sind dieselben Daten. Ein
    // "Verschieben" waere ein Loeschen - das darf nie stillschweigend passieren.
    if let (Some(a), Some(b)) = (quelle.id, fsmeta::file_id(&ziel)) {
        if a.volume == b.volume && a.index == b.index {
            return Ok(Aufloesung::Antwort(OpResult::skipped(
                src,
                "Quelle und Ziel sind dieselbe Datei (Hardlink) - es passiert nichts",
            )));
        }
    }

    match policy {
        CollisionPolicy::Skip => Ok(Aufloesung::Antwort(OpResult {
            dst: Some(paths::display_path(&ziel)),
            ..OpResult::skipped(src, "Am Ziel existiert bereits eine Datei")
        })),
        CollisionPolicy::Rename => Ok(Aufloesung::Weiter(unique_target(dst_dir, file_name)?)),
        CollisionPolicy::Ask => {
            let meta = std::fs::metadata(paths::extended(&ziel)).ok();
            let vorschlag = unique_target(dst_dir, file_name)
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| file_name.to_string());
            Ok(Aufloesung::Antwort(OpResult {
                status: OpStatus::Collision,
                src: paths::display_path(src),
                dst: Some(paths::display_path(&ziel)),
                action_id: None,
                error_kind: None,
                message: Some("Am Ziel existiert bereits eine Datei".into()),
                verified: None,
                existing: Some(ExistingFile {
                    path: paths::display_path(&ziel),
                    size: meta.as_ref().map(|m| m.len() as i64).unwrap_or(0),
                    modified: meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                    suggested_name: vorschlag,
                }),
            }))
        }
    }
}

/// Innerhalb eines Volumes: ein atomares Umbenennen, keine Kopie.
fn gleiches_volume_verschieben(
    conn: &Connection,
    id: i64,
    src: &Path,
    ziel: &Path,
    quelle: &FileState,
    src_hash: &str,
) -> Result<VerifyLevel> {
    let mut locked = fsmeta::LockedFile::open(src)?;
    if quelle.id.is_none_or(|id| locked.id().volume != id.volume || locked.id().index != id.index)
        || !locked.fingerprint()?.matches(&quelle.fingerprint)
        || locked.full_hash()? != src_hash
    { return Err(AppError::Changed(format!("{} hat sich vor dem Verschieben verändert",src.display()))); }
    journal::set_source_strong_identity(conn,id,locked.strong_id())?;
    crash_here!("vor_rename");
    match locked.rename_to(ziel) {
        Ok(()) => {
            journal::set_target_evidence(conn,id,Some(locked.id()),locked.strong_id())?;
            Ok(VerifyLevel::NotNeeded)
        },
        Err(AppError::Io(message)) if message.contains("Windows-Fehler 17") || message.contains("os error 17") => {
            // Die Volume-Erkennung lag daneben (Mount Point, SUBST-Laufwerk).
            journal::set_cross_volume(conn, id, true)?;
            // Ein fehlgeschlagener Rename veraendert das File Object nicht.
            // Derselbe bereits verifizierte Handle geht in den Kopierpfad und
            // bleibt bis zur abschliessenden Quellloeschung gebunden.
            ueber_volume_verschieben(conn, id, src, ziel, quelle, src_hash, Some(locked))
        }
        Err(e) => Err(e),
    }
}

/// Ueber Volumegrenzen: kopieren, pruefen, einhaengen, erst dann die Quelle
/// entfernen. Jeder Schritt ist im Journal sichtbar.
fn ueber_volume_verschieben(
    conn: &Connection,
    id: i64,
    src: &Path,
    ziel: &Path,
    quelle: &FileState,
    src_hash: &str,
    source_locked: Option<fsmeta::LockedFile>,
) -> Result<VerifyLevel> {
    let dst_dir = ziel
        .parent()
        .ok_or_else(|| AppError::Invalid("Ziel ohne Ordner".into()))?;
    let temp = dst_dir.join(journal::temp_name(
        id,
        &ziel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
    ));

    // Alternative Datenstroeme ueberleben ein `copy` nicht. Eine Datei mit
    // solchen Stroemen ueber eine Volumegrenze zu verschieben, hiesse Daten zu
    // vernichten, ohne dass jemand es merkt. Also gar nicht erst anfangen.
    match fsmeta::alternate_streams(src) {
        Some(stroeme) if !stroeme.is_empty() => {
            return Err(AppError::Invalid(format!(
                "{} hat zusaetzliche Datenstroeme ({}). Beim Verschieben auf ein anderes \
                 Laufwerk gingen sie verloren. FileTriage verschiebt die Datei deshalb nicht - \
                 innerhalb desselben Laufwerks ist das gefahrlos moeglich.",
                src.display(),
                stroeme.join(", ")
            )));
        }
        Some(_) => {}
        None => {
            // Nicht feststellbar: Im Zweifel wird nichts behauptet, aber auch
            // nicht blockiert - das waere bei Netzpfaden zu streng.
            tracing::warn!(
                "Datenstroeme von {} nicht pruefbar - ein Verschieben ueber Laufwerksgrenzen \
                 koennte zusaetzliche Stroeme verlieren",
                src.display()
            );
        }
    }

    let mut source_locked=match source_locked {
        Some(locked)=>locked,
        None=>fsmeta::LockedFile::open(src)?,
    };
    if quelle.id.is_none_or(|x|source_locked.id().volume!=x.volume||source_locked.id().index!=x.index)
        || !source_locked.fingerprint()?.matches(&quelle.fingerprint)
        || source_locked.full_hash()?!=src_hash {
        return Err(AppError::Changed(format!("Quelle {} wurde vor dem Kopieren verändert",src.display())));
    }
    journal::set_source_strong_identity(conn,id,source_locked.strong_id())?;

    journal::set_temp(conn,id,Some(&paths::display_path(&temp)))?;
    journal::set_state(conn,id,OpState::Copying)?;
    crash_here!("nach_copying_markiert");

    let (mut target_locked,kopiert)=match source_locked.copy_to_new(&temp){
        Ok(value)=>value,
        Err(error)=>{journal::set_state(conn,id,OpState::RollbackPending)?;return Err(error)}
    };
    let temp_hash=target_locked.full_hash()?;
    journal::set_temp_evidence(conn,id,Some(target_locked.id()),target_locked.strong_id(),&temp_hash)?;
    journal::set_state(conn,id,OpState::Copied)?;
    crash_here!("nach_kopie");

    let erwartet=quelle.fingerprint.size.max(0) as u64;
    if kopiert!=erwartet
        || target_locked.fingerprint()?.size!=quelle.fingerprint.size
        || source_locked.full_hash()?!=src_hash
        || temp_hash!=src_hash {
        let error=AppError::Io("Die Kopie stimmt nicht vollständig mit der gesperrten Quelle überein".into());
        match target_locked.delete(){
            Ok(())=>journal::set_temp(conn,id,None)?,
            Err(cleanup)=>{journal::set_state(conn,id,OpState::RollbackPending)?;return Err(AppError::Io(format!("{error}; Temp-Datei konnte nicht sicher entfernt werden: {cleanup}")))}
        }
        return Err(error)
    }
    let verified_hash=temp_hash;
    journal::set_verified_hash(conn,id,&verified_hash)?;
    journal::set_state(conn,id,OpState::Verified)?;
    crash_here!("nach_verifikation");

    if paths::extended(ziel).exists(){
        match target_locked.delete(){
            Ok(())=>journal::set_temp(conn,id,None)?,
            Err(cleanup)=>{journal::set_state(conn,id,OpState::RollbackPending)?;return Err(AppError::Io(format!("Am Ziel entstand eine Datei; Zwischendatei konnte nicht sicher entfernt werden: {cleanup}")))}
        }
        return Err(AppError::Invalid(format!("Am Ziel ist während des Kopierens eine Datei entstanden: {}",ziel.display())))
    }

    target_locked.rename_to(ziel)?;
    journal::set_target_evidence(conn,id,Some(target_locked.id()),target_locked.strong_id())?;
    journal::set_temp(conn,id,None)?;
    journal::set_state(conn,id,OpState::SourceRemovalPending)?;
    crash_here!("vor_quelle_loeschen");
    run_test_hook("after_target_rename_before_source_delete",src,ziel);

    let source_ok=quelle.id.is_some_and(|x|source_locked.id().volume==x.volume&&source_locked.id().index==x.index)
        && source_locked.fingerprint()?.matches(&quelle.fingerprint)
        && source_locked.full_hash()?==src_hash;
    if !source_ok{
        let changed=AppError::Changed(format!("Quelle {} wurde ersetzt oder verändert",src.display()));
        journal::set_state(conn,id,OpState::RollbackPending)?;
        let rollback=if target_locked.full_hash()?==verified_hash{target_locked.delete()}else{Err(AppError::Changed("Zielkopie wurde nach Verifikation verändert".into()))};
        match rollback{Ok(())=>journal::fail(conn,id,OpState::RolledBack,&changed.to_string())?,Err(cleanup)=>journal::fail(conn,id,OpState::RollbackPending,&format!("{changed}; {cleanup}"))?}
        return Err(changed)
    }
    if let Err(fehler)=source_locked.delete(){
        journal::set_state(conn,id,OpState::RollbackPending)?;
        let rollback=if target_locked.full_hash()?==verified_hash{target_locked.delete()}else{Err(AppError::Changed("Zielkopie wurde nach Verifikation verändert".into()))};
        match rollback{Ok(())=>journal::fail(conn,id,OpState::RolledBack,&fehler.to_string())?,Err(e2)=>journal::fail(conn,id,OpState::RollbackPending,&format!("{fehler}; Kopie konnte nicht zurückgenommen werden: {e2}"))?}
        return Err(fehler)
    }
    drop(target_locked);
    Ok(VerifyLevel::Full)
}

fn ist_platzmangel(e: &std::io::Error) -> bool {
    // 39 = ERROR_DISK_FULL, 112 = ERROR_DISK_FULL (Win32)
    matches!(e.raw_os_error(), Some(39) | Some(112))
        || e.kind() == std::io::ErrorKind::StorageFull
}

/// Beweist, dass die Kopie den Inhalt der Quelle traegt.
fn kopie_pruefen(src: &Path, temp: &Path, quelle: &FileState, kopiert: u64, source_hash: &str) -> Result<(VerifyLevel, String)> {
    let erwartet = quelle.fingerprint.size.max(0) as u64;
    if kopiert != erwartet {
        return Err(AppError::Io(format!(
            "Kopie unvollständig: {kopiert} von {erwartet} Bytes"
        )));
    }
    let kopie = fsmeta::inspect(temp)?;
    if kopie.fingerprint.size != quelle.fingerprint.size {
        return Err(AppError::Io(format!(
            "Kopie hat {} statt {} Bytes",
            kopie.fingerprint.size, quelle.fingerprint.size
        )));
    }

    // Hat sich die Quelle waehrend des Kopierens veraendert, ist der kopierte
    // Inhalt ein Mischmasch aus zwei Staenden.
    toctou_pruefen(src, quelle)?;

    // Ein Recovery-Delete braucht einen dauerhaften kryptografischen Beweis
    // ueber den gesamten Inhalt. Ein Quick-Hash ueber Anfang und Ende reicht
    // dafuer auch bei grossen Dateien nicht.
    let a = crate::dup::full_hash(src)?;
    let b = crate::dup::full_hash(temp)?;
    if a != b || a != source_hash {
        return Err(AppError::Io(
            "Die Kopie stimmt inhaltlich nicht mit der Quelle überein".into(),
        ));
    }
    Ok((VerifyLevel::Full, b))
}

/// Prueft unmittelbar vor dem unwiderruflichen Schritt, ob es noch dieselbe
/// Datei ist wie bei der Pruefung.
fn toctou_pruefen(src: &Path, erwartet: &FileState) -> Result<()> {
    let jetzt = fsmeta::inspect(src)?;
    if !jetzt.same_file_as(erwartet) {
        return Err(AppError::Changed(format!(
            "{} wurde zwischenzeitlich verändert oder ausgetauscht",
            src.display()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Loeschen
// ---------------------------------------------------------------------------

pub fn delete_file(
    conn: &Connection,
    src: &Path,
    mode: DeleteMode,
    batch_id: Option<&str>,
) -> OpResult {
    delete_file_checked(conn, src, mode, batch_id, None)
}

pub fn delete_file_checked(
    conn: &Connection,
    src: &Path,
    mode: DeleteMode,
    batch_id: Option<&str>,
    expected: Option<Fingerprint>,
) -> OpResult {
    match delete_inner(conn, src, mode, batch_id, expected) {
        Ok(r) => r,
        Err(e) => OpResult::failed(src, e),
    }
}

fn delete_inner(
    conn: &Connection,
    src: &Path,
    mode: DeleteMode,
    batch_id: Option<&str>,
    expected: Option<Fingerprint>,
) -> Result<OpResult> {
    let _mutation = journal::begin_mutation()?;
    let quelle = fsmeta::require_regular_file(src)?;
    if let Some(erwartet) = expected {
        if !quelle.fingerprint.matches(&erwartet) {
            return Err(AppError::Changed(format!(
                "{} hat sich seit der Anzeige verändert. Zur Sicherheit passiert nichts.",
                src.display()
            )));
        }
    }
    let mut locked=fsmeta::LockedFile::open(src)?;
    if quelle.id.is_none_or(|id|locked.id().volume!=id.volume||locked.id().index!=id.index)
        || !locked.fingerprint()?.matches(&quelle.fingerprint){
        return Err(AppError::Changed(format!("{} wurde vor dem Löschen verändert oder ausgetauscht",src.display())))
    }
    let src_hash=locked.full_hash()?;

    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let (kind, undoable) = match mode {
        DeleteMode::Recycle => ("delete_recycle", true),
        DeleteMode::Permanent => ("delete_permanent", false),
    };

    let session = crate::session_id();
    let id = journal::plan(
        conn,
        &Intent {
            kind,
            src: &paths::display_path(src),
            dst: None,
            original_name: &file_name,
            fingerprint: quelle.fingerprint,
            file_id: quelle.id,
            strong_file_id: locked.strong_id(),
            src_hash: Some(&src_hash),
            cross_volume: false,
            undoable,
            batch_id,
            session_id: &session,
        },
    )?;
    crash_here!("nach_plan_loeschen");

    match mode {
        DeleteMode::Recycle => {
            // Das Zeitfenster grenzt spaeter ein, welcher Papierkorbeintrag zu
            // dieser Aktion gehoert. Ohne das bliebe die Zuordnung Raterei.
            let von = db::now_ts() - 2;
            let parent=src.parent().ok_or_else(||AppError::Invalid("Datei ohne Elternordner".into()))?;
            let stage=parent.join(journal::trash_stage_name(id,&file_name)?);
            journal::set_temp(conn,id,Some(&paths::display_path(&stage)))?;
            journal::set_temp_evidence(conn,id,Some(locked.id()),locked.strong_id(),&src_hash)?;
            journal::set_state(conn,id,OpState::SourceRemovalPending)?;
            locked.rename_to(&stage)?;
            run_test_hook("after_recycle_stage_before_shell",src,&stage);
            crash_here!("vor_papierkorb");
            drop(locked);
            if let Err(e) = trash::delete(&stage) {
                // Der eindeutige Staging-Pfad bleibt journalisiert und damit
                // recoverbar. Ein generisches Failed wuerde ihn verstecken.
                return Err(AppError::Trash(e.to_string()));
            }
            journal::set_trash_window(conn, id, von, db::now_ts() + 2)?;
        }
        DeleteMode::Permanent => {
            journal::set_state(conn, id, OpState::SourceRemovalPending)?;
            crash_here!("vor_endgueltig_loeschen");
            run_test_hook("before_permanent_disposition",src,src);
            if !locked.fingerprint()?.matches(&quelle.fingerprint)||locked.full_hash()?!=src_hash{return Err(AppError::Changed("Zu löschende Datei wurde nach der Entscheidung verändert".into()))}
            if let Err(fehler) = locked.delete() {
                journal::fail(conn, id, OpState::Failed, &fehler.to_string())?;
                return Err(fehler);
            }
        }
    }

    journal::complete(conn, id, None, VerifyLevel::NotNeeded)?;
    conn.execute(
        "UPDATE files SET state = 'deleted', decided_at = ?1 WHERE path_key = ?2",
        rusqlite::params![db::now_ts(), paths::path_key(&paths::display_path(src))],
    )?;

    Ok(OpResult {
        status: OpStatus::Done,
        src: paths::display_path(src),
        dst: None,
        action_id: Some(id),
        error_kind: None,
        message: None,
        existing: None,
        verified: None,
    })
}

// ---------------------------------------------------------------------------
// Umbenennen
// ---------------------------------------------------------------------------

pub fn rename_file(conn: &Connection, src: &Path, new_name: &str) -> OpResult {
    match rename_inner(conn, src, new_name) {
        Ok(r) => r,
        Err(e) => OpResult::failed(src, e),
    }
}

fn rename_inner(conn: &Connection, src: &Path, new_name: &str) -> Result<OpResult> {
    let _mutation = journal::begin_mutation()?;
    let name = paths::validate_file_name(new_name)?;
    let quelle = fsmeta::require_regular_file(src)?;

    let dir = src
        .parent()
        .ok_or_else(|| AppError::Invalid("Quelle hat keinen Ordner".into()))?;
    let alt = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ziel = dir.join(&name);

    // Gleicher Name in anderer Schreibweise ist erlaubt (Umbenennen von
    // "datei.txt" zu "Datei.txt"), eine *andere* bestehende Datei nicht.
    if paths::extended(&ziel).exists() && paths::path_key(&name) != paths::path_key(&alt) {
        return Err(AppError::Invalid(format!("\"{name}\" existiert bereits")));
    }

    let src_hash = crate::dup::full_hash(src)?;
    toctou_pruefen(src, &quelle)?;
    let session = crate::session_id();
    let id = journal::plan(
        conn,
        &Intent {
            kind: "rename",
            src: &paths::display_path(src),
            dst: Some(&paths::display_path(&ziel)),
            original_name: &alt,
            fingerprint: quelle.fingerprint,
            file_id: quelle.id,
            strong_file_id: fsmeta::strong_file_id(src),
            src_hash: Some(&src_hash),
            cross_volume: false,
            undoable: true,
            batch_id: None,
            session_id: &session,
        },
    )?;
    crash_here!("nach_plan_umbenennen");

    if let Err(e) = toctou_pruefen(src, &quelle) {
        journal::fail(conn, id, OpState::Failed, &e.to_string())?;
        return Err(e);
    }
    let mut locked=fsmeta::LockedFile::open(src)?;
    if quelle.id.is_none_or(|x|locked.id().volume!=x.volume||locked.id().index!=x.index){return Err(AppError::Changed("Umzubenennende Datei wurde ausgetauscht".into()))}
    if locked.full_hash()?!=src_hash{return Err(AppError::Changed("Umzubenennende Datei wurde inhaltlich verändert".into()))}
    if let Err(fehler) = locked.rename_to(&ziel) {
        journal::fail(conn, id, OpState::Failed, &fehler.to_string())?;
        return Err(fehler);
    }
    journal::set_target_evidence(conn,id,Some(locked.id()),locked.strong_id())?;

    let ziel_fp = fsmeta::inspect(&ziel).ok().map(|s| s.fingerprint);
    journal::complete(conn, id, ziel_fp, VerifyLevel::NotNeeded)?;
    index_after_move(conn, src, &ziel)?;
    Ok(OpResult::ok(src, &ziel, id, VerifyLevel::NotNeeded))
}

// ---------------------------------------------------------------------------
// Zustand ohne Dateiveraenderung
// ---------------------------------------------------------------------------

/// Markiert eine Datei, ohne sie anzufassen ("behalten" / "uebersprungen").
pub fn set_state(conn: &Connection, path: &str, state: &str) -> Result<()> {
    if !["pending", "skipped", "kept"].contains(&state) {
        return Err(AppError::Invalid(format!("Unbekannter Zustand: {state}")));
    }
    conn.execute(
        "UPDATE files SET state = ?1, decided_at = ?2 WHERE path_key = ?3",
        rusqlite::params![state, db::now_ts(), paths::path_key(path)],
    )?;
    Ok(())
}

/// Zieht den Indexeintrag mit der Datei mit.
pub(crate) fn index_after_move(conn: &Connection, src: &Path, dst: &Path) -> Result<()> {
    let src_key = paths::path_key(&paths::display_path(src));
    let dst_display = paths::display_path(dst);
    let dst_key = paths::path_key(&dst_display);
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = dst.parent().map(paths::display_path).unwrap_or_default();

    // Ein evtl. vorhandener Eintrag am Ziel stammt aus einem alten Scan.
    conn.execute("DELETE FROM files WHERE path_key = ?1", [&dst_key])?;
    conn.execute(
        "UPDATE files SET path = ?1, path_key = ?2, parent = ?3, name = ?4, name_lower = ?5,
                          ext = ?6, category = ?7, state = 'moved', decided_at = ?8,
                          quick_hash = NULL, full_hash = NULL
         WHERE path_key = ?9",
        rusqlite::params![
            dst_display,
            dst_key,
            parent,
            name,
            name.to_lowercase(),
            classify::extension_of(&name),
            classify::categorize(&name).as_str(),
            db::now_ts(),
            src_key,
        ],
    )?;
    Ok(())
}

/// Zaehlt mit, welcher Dateityp wohin verschoben wurde - Grundlage fuer
/// Regelvorschlaege. Es werden nur Endung, Kategorie und Zielordner gespeichert.
pub(crate) fn record_move_stat(conn: &Connection, file_name: &str, dst_dir: &Path) -> Result<()> {
    let ext = classify::extension_of(file_name);
    let category = classify::categorize(file_name).as_str();
    conn.execute(
        "INSERT INTO move_stats (ext, category, target, count, last_ts)
         VALUES (?1, ?2, ?3, 1, ?4)
         ON CONFLICT(ext, category, target) DO UPDATE SET
            count = count + 1, last_ts = excluded.last_ts",
        rusqlite::params![ext, category, paths::display_path(dst_dir), db::now_ts()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::testutil::TempDir;

    fn setup() -> (TempDir, Db, Connection) {
        let dir = TempDir::new("ops");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        (dir, db, conn)
    }

    #[cfg(windows)]
    #[test]
    fn cross_volume_haelt_quelle_und_ziel_bis_zur_quellloeschung_gebunden() {
        use std::{cell::Cell,rc::Rc};
        let (dir,_db,conn)=setup(); let src=dir.file("locked/source.bin",b"original"); let target_dir=dir.dir("target");
        let attempts=Rc::new(Cell::new(0)); let seen=attempts.clone();
        crashpoint::set_hook(move |point,source,target| if point=="after_target_rename_before_source_delete"{
            assert!(std::fs::OpenOptions::new().write(true).open(source).is_err());
            assert!(std::fs::remove_file(source).is_err());
            assert!(std::fs::OpenOptions::new().write(true).open(target).is_err());
            assert!(std::fs::remove_file(target).is_err()); seen.set(4);
        });
        let _reset=crashpoint::Reset; crashpoint::force_copy_path(true);
        let r=move_file(&conn,&src,&target_dir,CollisionPolicy::Ask,None);
        assert_eq!(r.status,OpStatus::Done,"{:?}",r.message); assert_eq!(attempts.get(),4);
        assert_eq!(std::fs::read(target_dir.join("source.bin")).unwrap(),b"original");
    }

    #[cfg(windows)]
    #[test]
    fn error_not_same_device_nutzt_den_bereits_verifizierten_source_handle() {
        let (dir,_db,conn)=setup();
        let src=dir.file("fallback/source.bin",b"vollstaendiger inhalt");
        let target_dir=dir.dir("fallback-target");
        crate::fsmeta::force_next_locked_rename_cross_volume();
        let result=move_file(&conn,&src,&target_dir,CollisionPolicy::Ask,None);
        assert_eq!(result.status,OpStatus::Done,"{:?}",result.message);
        assert!(!src.exists());
        assert_eq!(std::fs::read(target_dir.join("source.bin")).unwrap(),b"vollstaendiger inhalt");
        let action=journal::by_id(&conn,result.action_id.unwrap()).unwrap();
        assert!(action.cross_volume);
        assert_eq!(action.verify.as_deref(),Some("full"));
    }

    #[cfg(windows)]
    #[test]
    fn fehlgeschlagener_cross_volume_fallback_behaelt_die_quelle() {
        let (dir,_db,conn)=setup();
        let src=dir.file("fallback-fail/source.bin",b"unersetzlich");
        let target_dir=dir.dir("fallback-fail-target");
        crate::fsmeta::force_next_locked_rename_cross_volume();
        crate::fsmeta::force_next_locked_copy_failure();
        let result=move_file(&conn,&src,&target_dir,CollisionPolicy::Ask,None);
        assert_eq!(result.status,OpStatus::Failed);
        assert_eq!(std::fs::read(&src).unwrap(),b"unersetzlich");
        assert!(!target_dir.join("source.bin").exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "benoetigt FILETRIAGE_TEST_VOLUME_A und FILETRIAGE_TEST_VOLUME_B auf verschiedenen echten Volumes"]
    fn echter_error_not_same_device_fallback_auf_zwei_volumes() {
        let (Some(root_a),Some(root_b))=(std::env::var_os("FILETRIAGE_TEST_VOLUME_A"),std::env::var_os("FILETRIAGE_TEST_VOLUME_B")) else { eprintln!("SKIP: Volume-Variablen fehlen"); return };
        let unique=format!("filetriage-two-volume-{}-{}",std::process::id(),crate::db::now_ts());
        let a=std::path::PathBuf::from(root_a).join(&unique);let b=std::path::PathBuf::from(root_b).join(&unique);
        std::fs::create_dir_all(&a).unwrap();std::fs::create_dir_all(&b).unwrap();
        struct Cleanup(std::path::PathBuf,std::path::PathBuf);impl Drop for Cleanup{fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);let _=std::fs::remove_dir_all(&self.1);}}
        let _cleanup=Cleanup(a.clone(),b.clone());
        let src=a.join("source.bin");let content=b"echter ERROR_NOT_SAME_DEVICE produktpfad";std::fs::write(&src,content).unwrap();
        if fsmeta::same_volume(&src,&b.join("source.bin")){eprintln!("SKIP: konfigurierte Pfade liegen auf demselben Volume");return}
        let db=Db::open(a.join("journal.db")).unwrap();let conn=db.conn().unwrap();
        let expected=crate::dup::full_hash(&src).unwrap();let _reset=crashpoint::Reset;crashpoint::force_rename_path(true);
        let result=move_file(&conn,&src,&b,CollisionPolicy::Ask,None);
        assert_eq!(result.status,OpStatus::Done,"{:?}",result.message);assert!(!src.exists());
        let target=b.join("source.bin");assert_eq!(std::fs::read(&target).unwrap(),content);assert_eq!(crate::dup::full_hash(&target).unwrap(),expected);
        let action=journal::by_id(&conn,result.action_id.unwrap()).unwrap();assert!(action.cross_volume);assert_eq!(action.state(),OpState::Completed);assert_eq!(action.verify.as_deref(),Some("full"));
    }

    #[cfg(windows)]
    #[test]
    fn permanent_delete_verwendet_bis_zur_disposition_denselben_handle() {
        use std::{cell::Cell,rc::Rc};
        let (dir,_db,conn)=setup(); let src=dir.file("delete.bin",b"original");
        let blocked=Rc::new(Cell::new(false));let seen=blocked.clone();
        crashpoint::set_hook(move |point,source,_|if point=="before_permanent_disposition"{
            seen.set(std::fs::OpenOptions::new().write(true).open(source).is_err()&&std::fs::remove_file(source).is_err());
        }); let _reset=crashpoint::Reset;
        let r=delete_file(&conn,&src,DeleteMode::Permanent,None);assert_eq!(r.status,OpStatus::Done,"{:?}",r.message);assert!(blocked.get());assert!(!src.exists());
    }

    #[cfg(windows)]
    #[test]
    fn recycle_staging_schuetzt_neue_datei_am_originalpfad() {
        use std::{cell::Cell,rc::Rc};
        let (dir,_db,conn)=setup();let src=dir.file("recycle-race.bin",b"selected");
        let checked=Rc::new(Cell::new(false));let seen=checked.clone();
        crashpoint::set_hook(move |point,source,stage|if point=="after_recycle_stage_before_shell"{
            assert!(!source.exists());std::fs::write(source,b"unrelated").unwrap();
            assert!(std::fs::OpenOptions::new().write(true).open(stage).is_err());assert!(std::fs::remove_file(stage).is_err());seen.set(true);
        });let _reset=crashpoint::Reset;
        let r=delete_file(&conn,&src,DeleteMode::Recycle,None);assert_eq!(r.status,OpStatus::Done,"{:?}",r.message);assert!(checked.get());
        assert_eq!(std::fs::read(&src).unwrap(),b"unrelated");
        std::fs::remove_file(&src).unwrap();let undo=crate::undo::undo(&conn,r.action_id.unwrap());assert!(undo.ok,"{:?}",undo.message);std::fs::remove_file(&src).unwrap();
    }

    #[test]
    fn verschieben_legt_ziel_an_und_protokolliert() {
        let (dir, _db, conn) = setup();
        let src = dir.file("a.pdf", b"inhalt");
        let ziel = dir.path().join("Rechnungen");

        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
        assert!(!src.exists());
        assert!(ziel.join("a.pdf").exists());

        let eintrag = journal::by_id(&conn, r.action_id.unwrap()).unwrap();
        assert_eq!(eintrag.state(), OpState::Completed);
        assert_eq!(eintrag.kind, "move");
        assert!(eintrag.dst.unwrap().ends_with("a.pdf"));
        assert!(eintrag.dst_size.is_some(), "Zielzustand muss erfasst sein");
    }

    #[test]
    fn kollision_ueberschreibt_niemals() {
        let (dir, _db, conn) = setup();
        let src = dir.file("quelle/b.txt", b"neu");
        let ziel = dir.dir("ziel");
        std::fs::write(ziel.join("b.txt"), b"alt").unwrap();

        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Collision);
        assert_eq!(std::fs::read(ziel.join("b.txt")).unwrap(), b"alt");
        assert!(src.exists(), "Quelle muss unangetastet bleiben");
        assert_eq!(r.existing.unwrap().suggested_name, "b (2).txt");
        // Eine Kollision darf keinen Journaleintrag hinterlassen.
        assert!(journal::unfinished(&conn).unwrap().is_empty());
    }

    #[test]
    fn kollision_mit_umbenennen_findet_freien_namen() {
        let (dir, _db, conn) = setup();
        let src = dir.file("quelle/b.txt", b"neu");
        let ziel = dir.dir("ziel");
        std::fs::write(ziel.join("b.txt"), b"alt").unwrap();

        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Rename, None);
        assert_eq!(r.status, OpStatus::Done);
        assert!(ziel.join("b (2).txt").exists());
        assert_eq!(std::fs::read(ziel.join("b.txt")).unwrap(), b"alt");
    }

    #[test]
    fn kollision_mit_ueberspringen_laesst_beides_stehen() {
        let (dir, _db, conn) = setup();
        let src = dir.file("quelle/b.txt", b"neu");
        let ziel = dir.dir("ziel");
        std::fs::write(ziel.join("b.txt"), b"alt").unwrap();

        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Skip, None);
        assert_eq!(r.status, OpStatus::Skipped);
        assert!(src.exists());
    }

    #[test]
    fn fehlende_quelle_meldet_not_found_ohne_panik() {
        let (dir, _db, conn) = setup();
        let r = move_file(
            &conn,
            &dir.path().join("gibtsnicht.txt"),
            &dir.dir("ziel"),
            CollisionPolicy::Ask,
            None,
        );
        assert_eq!(r.status, OpStatus::Failed);
        assert_eq!(r.error_kind.as_deref(), Some("not_found"));
    }

    #[test]
    fn gesperrte_datei_bricht_nicht_ab() {
        let (dir, _db, conn) = setup();
        let src = dir.file("gesperrt.bin", b"x");
        let _lock = std::fs::OpenOptions::new().read(true).write(true).open(&src).unwrap();
        let r = move_file(&conn, &src, &dir.dir("ziel"), CollisionPolicy::Ask, None);
        assert!(matches!(r.status, OpStatus::Done | OpStatus::Failed));
        if r.status == OpStatus::Failed {
            assert!(src.exists(), "bei Fehlschlag muss die Quelle bleiben");
        }
    }

    #[test]
    fn unicode_und_lange_namen_funktionieren() {
        let (dir, _db, conn) = setup();
        let src = dir.file("Ünïcödé — Grüße 🚀.txt", b"x");
        let ziel = dir.dir(&"z".repeat(120));
        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
        assert!(ziel.join("Ünïcödé — Grüße 🚀.txt").exists());
    }

    #[test]
    fn umbenennen_lehnt_pfadtrenner_ab() {
        let (dir, _db, conn) = setup();
        let src = dir.file("x.txt", b"x");
        let r = rename_file(&conn, &src, "..\\woanders.txt");
        assert_eq!(r.status, OpStatus::Failed);
        assert!(src.exists());
    }

    #[test]
    fn umbenennen_erlaubt_reine_schreibweisenaenderung() {
        let (dir, _db, conn) = setup();
        let src = dir.file("datei.txt", b"x");
        let r = rename_file(&conn, &src, "Datei.txt");
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
    }

    #[test]
    fn freier_name_beruecksichtigt_doppelendungen() {
        let dir = TempDir::new("uniq");
        std::fs::write(dir.path().join("a.tar.gz"), b"x").unwrap();
        let p = unique_target(dir.path(), "a.tar.gz").unwrap();
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "a.tar (2).gz");
    }

    #[test]
    fn verschiebestatistik_zaehlt_mit() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("Rechnungen");
        for i in 0..3 {
            let src = dir.file(&format!("r{i}.pdf"), b"x");
            assert_eq!(
                move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None).status,
                OpStatus::Done
            );
        }
        let n: i64 = conn
            .query_row("SELECT count FROM move_stats WHERE ext = 'pdf'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn datei_bereits_am_ziel_wird_uebersprungen() {
        let (dir, _db, conn) = setup();
        let src = dir.file("ordner/c.txt", b"x");
        let ziel = dir.path().join("ordner");
        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Skipped);
        assert!(src.exists());
    }

    #[test]
    fn erwarteter_zustand_schuetzt_vor_verwechslung() {
        let (dir, _db, conn) = setup();
        let src = dir.file("a.txt", b"eins");
        let erwartet = fsmeta::inspect(&src).unwrap().fingerprint;

        // Unveraendert: geht durch.
        let r = move_file_checked(
            &conn,
            &src,
            &dir.dir("ziel"),
            CollisionPolicy::Ask,
            None,
            Some(erwartet),
        );
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
    }

    #[test]
    fn loeschen_schreibt_papierkorbfenster() {
        let (dir, _db, conn) = setup();
        let src = dir.file("weg.txt", b"x");
        let r = delete_file(&conn, &src, DeleteMode::Recycle, None);
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);

        let id = r.action_id.unwrap();
        let (von, bis) = journal::trash_window(&conn, id).unwrap().expect("Zeitfenster fehlt");
        assert!(von <= bis);
        assert_eq!(journal::by_id(&conn, id).unwrap().state(), OpState::Completed);
    }
}

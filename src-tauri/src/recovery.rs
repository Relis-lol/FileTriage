//! Wiederherstellung nach einem Absturz.
//!
//! Beim Start wird jeder Journaleintrag untersucht, der keinen Endzustand hat.
//! Aus Journal **und** tatsaechlichem Dateisystemzustand wird eine Diagnose
//! gebildet.
//!
//! Zwei Regeln bestimmen alles hier:
//!
//! 1. **Nichts wird stillschweigend geloescht, um aufzuraeumen.** Die einzige
//!    Ausnahme sind unsere eigenen, nachweislich unvollstaendigen
//!    Zwischendateien - und auch die nur, wenn die Quelle nachweislich noch da
//!    ist, und auch dann nur auf ausdrueckliche Anweisung.
//! 2. **Unklar heisst unklar.** Wo sich nicht beweisen laesst, welche Version
//!    gueltig ist, entscheidet der Nutzer, nicht das Programm.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::fsmeta;
use crate::journal::{self, OpRecord, OpState};
use crate::paths;

/// Was aus Sicht der Wiederherstellung passiert ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Diagnosis {
    /// Am Dateisystem ist nachweislich nichts passiert.
    NothingHappened,
    /// Die Operation ist tatsaechlich durchgelaufen, nur der letzte
    /// Journaleintrag fehlte.
    CompletedAfterAll,
    /// Eine unvollstaendige Zwischendatei liegt herum; die Quelle ist intakt.
    LeftoverTempFile,
    /// Ziel ist vollstaendig und geprueft, die Quelle liegt noch da.
    /// Beide Versionen existieren - der Nutzer entscheidet.
    BothCopiesPresent,
    /// Weder Quelle noch Ziel auffindbar. Der schlimmste Fall; wird immer
    /// gemeldet, nie stillschweigend abgehakt.
    DataMissing,
    /// Der Zustand passt zu keiner Erwartung.
    Inconclusive,
}

impl Diagnosis {
    pub fn as_str(self) -> &'static str {
        match self {
            Diagnosis::NothingHappened => "nothing_happened",
            Diagnosis::CompletedAfterAll => "completed_after_all",
            Diagnosis::LeftoverTempFile => "leftover_temp_file",
            Diagnosis::BothCopiesPresent => "both_copies_present",
            Diagnosis::DataMissing => "data_missing",
            Diagnosis::Inconclusive => "inconclusive",
        }
    }

    /// Kann FileTriage das ohne Rueckfrage in Ordnung bringen?
    ///
    /// Nur dort wahr, wo die Aufloesung keine Nutzerdaten anfasst.
    pub fn auto_resolvable(self) -> bool {
        matches!(self, Diagnosis::NothingHappened | Diagnosis::CompletedAfterAll)
    }

    /// Wie ernst ist der Befund?
    pub fn severity(self) -> &'static str {
        match self {
            Diagnosis::NothingHappened | Diagnosis::CompletedAfterAll => "info",
            Diagnosis::LeftoverTempFile => "hinweis",
            Diagnosis::BothCopiesPresent | Diagnosis::Inconclusive => "achtung",
            Diagnosis::DataMissing => "kritisch",
        }
    }
}

/// Ein untersuchter Journaleintrag.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryItem {
    pub action_id: i64,
    pub kind: String,
    pub state: String,
    pub src: String,
    pub dst: Option<String>,
    pub temp: Option<String>,
    pub original_name: String,
    pub size: i64,
    pub ts: i64,
    pub diagnosis: String,
    pub severity: String,
    /// Erklaerung in ganzen Saetzen - der Nutzer soll verstehen, was er waehlt.
    #[serde(skip_serializing)]
    pub explanation: String,
    /// Handlungsmoeglichkeiten, die FileTriage anbietet.
    pub choices: Vec<RecoveryChoice>,
    pub src_exists: bool,
    pub dst_exists: bool,
    pub temp_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryChoice {
    pub id: String,
    #[serde(skip_serializing)]
    pub label: String,
    /// Wahr, wenn diese Wahl Nutzerdaten veraendert.
    pub touches_data: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RecoveryReport {
    pub items: Vec<RecoveryItem>,
    pub auto_resolved: usize,
    /// Anzahl der Faelle, die der Nutzer ansehen muss.
    pub needs_attention: usize,
    pub critical: usize,
}

fn existiert(p: &str) -> bool {
    !p.is_empty() && paths::extended(Path::new(p)).exists()
}

fn identity_matches(path: &str, expected: Option<fsmeta::FileId>) -> bool {
    expected.is_some() && fsmeta::file_id(Path::new(path)) == expected
}

fn strong_identity_matches(path:&str,expected:Option<fsmeta::StrongFileId>)->bool{
    expected.is_some()&&fsmeta::strong_file_id(Path::new(path))==expected
}

fn target_belongs_to_action(e: &OpRecord) -> bool {
    let Some(dst) = e.dst.as_deref() else { return false };
    let expected = if e.cross_volume || e.state() != OpState::Planned {
        e.dst_file_id()
    } else {
        // Ein Rename auf demselben Volume behaelt die Windows File-ID.
        e.file_id()
    };
    if !identity_matches(dst, expected) || !strong_identity_matches(dst,e.dst_strong_id()) { return false; }
    let expected_hash = if e.cross_volume { e.verified_hash.as_deref() } else { e.src_hash.as_deref() };
    expected_hash.is_some_and(|hash| crate::dup::full_hash(Path::new(dst)).ok().as_deref() == Some(hash))
}

fn temp_belongs_to_action(e: &OpRecord) -> bool {
    e.temp
        .as_deref()
        .is_some_and(|temp| identity_matches(temp, e.temp_file_id())&&strong_identity_matches(temp,e.temp_strong_id()))
}

fn temp_has_persisted_content(e:&OpRecord)->bool{
    e.temp.as_deref().zip(e.temp_hash.as_deref()).is_some_and(|(path,hash)|crate::dup::full_hash(Path::new(path)).ok().as_deref()==Some(hash))
}


fn locked_identity_matches(file:&fsmeta::LockedFile,legacy:Option<fsmeta::FileId>,strong:Option<fsmeta::StrongFileId>)->bool{
    legacy.is_some_and(|id|file.id().volume==id.volume&&file.id().index==id.index)
        && strong.is_some_and(|id|file.strong_id()==Some(id))
}

/// Untersucht alle unfertigen Operationen.
///
/// `auto_resolve` erlaubt nur die beiden Faelle, in denen die Aufloesung
/// ausschliesslich das Journal korrigiert und keine Datei anfasst.
pub fn inspect(conn: &Connection, auto_resolve: bool) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();

    for eintrag in journal::unfinished(conn)? {
        let item = diagnose(&eintrag);

        if auto_resolve && Diagnosis::from_str(&item.diagnosis).auto_resolvable() {
            match Diagnosis::from_str(&item.diagnosis) {
                Diagnosis::CompletedAfterAll => {
                    // Das Ziel liegt nachweislich da und traegt die erwartete
                    // Groesse. Nur das Journal wird nachgezogen.
                    let fp = eintrag
                        .dst
                        .as_deref()
                        .and_then(|d| fsmeta::inspect(Path::new(d)).ok())
                        .map(|s| s.fingerprint);
                    journal::complete(conn, eintrag.id, fp, journal::VerifyLevel::NotNeeded)?;
                    if let (Some(dst), true) = (eintrag.dst.as_deref(), true) {
                        let _ = crate::fileops::index_after_move(
                            conn,
                            Path::new(&eintrag.src),
                            Path::new(dst),
                        );
                    }
                }
                Diagnosis::NothingHappened => {
                    journal::fail(
                        conn,
                        eintrag.id,
                        OpState::Failed,
                        "Beim Programmstart festgestellt: Die Operation war nur geplant, \
                         am Dateisystem ist nichts passiert.",
                    )?;
                }
                _ => unreachable!("auto_resolvable deckt nur diese beiden Faelle ab"),
            }
            report.auto_resolved += 1;
            continue;
        }

        if item.severity == "kritisch" {
            report.critical += 1;
        }
        report.needs_attention += 1;
        report.items.push(item);
    }
    Ok(report)
}

impl Diagnosis {
    fn from_str(s: &str) -> Diagnosis {
        match s {
            "nothing_happened" => Diagnosis::NothingHappened,
            "completed_after_all" => Diagnosis::CompletedAfterAll,
            "leftover_temp_file" => Diagnosis::LeftoverTempFile,
            "both_copies_present" => Diagnosis::BothCopiesPresent,
            "data_missing" => Diagnosis::DataMissing,
            _ => Diagnosis::Inconclusive,
        }
    }
}

/// Bildet aus Journalzustand und Dateisystem eine Diagnose.
pub fn diagnose(e: &OpRecord) -> RecoveryItem {
    let src_da = existiert(&e.src);
    let dst_da = e.dst.as_deref().map(existiert).unwrap_or(false);
    let temp_da = e.temp.as_deref().map(existiert).unwrap_or(false);

    let (diagnose, erklaerung, wahlen) = match e.state() {
        // ------------------------------------------------------------------
        OpState::Planned => match (src_da, dst_da) {
            (true, false) => (
                Diagnosis::NothingHappened,
                format!(
                    "\"{}\" liegt unveraendert an seinem Platz. Die Aktion war nur vorgemerkt, \
                     als das Programm beendet wurde.",
                    e.original_name
                ),
                vec![],
            ),
            (false, true) if target_belongs_to_action(e) => (
                Diagnosis::CompletedAfterAll,
                format!(
                    "\"{}\" liegt bereits am Ziel und nicht mehr am Ausgangsort. Die Aktion \
                     ist durchgelaufen, nur der Abschlussvermerk fehlte.",
                    e.original_name
                ),
                vec![],
            ),
            (false, true) => (
                Diagnosis::Inconclusive,
                format!(
                    "Am Ziel von \"{}\" liegt eine Datei, deren Zugehoerigkeit zur Aktion \
                     nicht bewiesen werden kann.",
                    e.original_name
                ),
                vec![wahl("acknowledge", "Von Hand klären", false)],
            ),
            (true, true) => (
                Diagnosis::BothCopiesPresent,
                format!(
                    "\"{}\" liegt sowohl am Ausgangsort als auch am Ziel. Welche der beiden \
                     die gemeinte ist, kann FileTriage nicht entscheiden.",
                    e.original_name
                ),
                vec![
                    wahl("keep_both", "Beide behalten und Vermerk schließen", false),
                    wahl("show_both", "Beide im Explorer zeigen", false),
                ],
            ),
            (false, false) => (
                Diagnosis::DataMissing,
                format!(
                    "\"{}\" ist weder am Ausgangsort noch am Ziel auffindbar. Das kann \
                     bedeuten, dass die Datei anderweitig verschoben wurde.",
                    e.original_name
                ),
                vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
            ),
        },

        // ------------------------------------------------------------------
        OpState::Copying | OpState::Copied => {
            if src_da && temp_da && temp_belongs_to_action(e) {
                (
                    Diagnosis::LeftoverTempFile,
                    format!(
                        "Von \"{}\" wurde eine Kopie begonnen, aber nie fertiggestellt. \
                         Das Original ist unversehrt; die unvollstaendige Kopie liegt noch da.",
                        e.original_name
                    ),
                    vec![
                        wahl("delete_temp", "Unvollständige Kopie entfernen", true),
                        wahl("keep_temp", "Kopie liegen lassen", false),
                    ],
                )
            } else if src_da && !temp_da {
                (
                    Diagnosis::NothingHappened,
                    format!(
                        "Von \"{}\" wurde eine Kopie begonnen, es ist aber nichts davon \
                         uebrig. Das Original ist unversehrt.",
                        e.original_name
                    ),
                    vec![],
                )
            } else {
                (
                    Diagnosis::Inconclusive,
                    format!(
                        "Waehrend des Kopierens von \"{}\" wurde das Programm beendet, und \
                         das Original ist nicht mehr auffindbar.",
                        e.original_name
                    ),
                    vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
                )
            }
        }

        // ------------------------------------------------------------------
        OpState::Verified => {
            if src_da && temp_da && temp_belongs_to_action(e) {
                (
                    Diagnosis::LeftoverTempFile,
                    format!(
                        "Die Kopie von \"{}\" wurde geprueft, aber nicht mehr an ihren Platz \
                         gebracht. Das Original ist unversehrt.",
                        e.original_name
                    ),
                    vec![
                        wahl("finish_move", "Verschieben zu Ende führen", true),
                        wahl("delete_temp", "Kopie entfernen, Original behalten", true),
                    ],
                )
            } else {
                (
                    Diagnosis::Inconclusive,
                    format!("Der Zwischenstand von \"{}\" lässt sich nicht deuten.", e.original_name),
                    vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
                )
            }
        }

        // ------------------------------------------------------------------
        OpState::SourceRemovalPending => match (src_da, dst_da, e.kind.as_str()) {
            (_, _, "delete_recycle") if temp_da && temp_belongs_to_action(e) && temp_has_persisted_content(e) => (
                Diagnosis::LeftoverTempFile,
                format!("\"{}\" wurde sicher zwischengelagert, aber noch nicht in den Papierkorb verschoben.",e.original_name),
                vec![wahl("finish_recycle","Papierkorb-Vorgang abschließen",true),wahl("restore_staged","Datei zurücklegen",true)],
            ),
            (_, _, "delete_recycle") if temp_da => (
                Diagnosis::Inconclusive,
                format!("Der zwischengelagerte Papierkorbzustand von \"{}\" ist nicht eindeutig.",e.original_name),
                vec![wahl("acknowledge","Von Hand klären",false)],
            ),
            // Papierkorb/Loeschen: kein Ziel, nur die Quelle zaehlt.
            (false, _, "delete_recycle") | (false, _, "delete_permanent") => (
                Diagnosis::CompletedAfterAll,
                format!("\"{}\" wurde gelöscht, nur der Vermerk fehlte.", e.original_name),
                vec![],
            ),
            (true, _, "delete_recycle") | (true, _, "delete_permanent") => (
                Diagnosis::NothingHappened,
                format!("\"{}\" liegt noch da; gelöscht wurde nichts.", e.original_name),
                vec![],
            ),
            (true, true, _) if target_belongs_to_action(e) => (
                Diagnosis::BothCopiesPresent,
                format!(
                    "Die Kopie von \"{}\" ist vollstaendig und geprueft, das Original liegt \
                     aber noch am Ausgangsort. Das Verschieben wurde nicht zu Ende gefuehrt.",
                    e.original_name
                ),
                vec![
                    wahl("finish_move", "Original entfernen, Verschieben abschließen", true),
                    wahl("undo_move", "Kopie entfernen, alles beim Alten lassen", true),
                    wahl("keep_both", "Beide behalten", false),
                ],
            ),
            (false, true, _) if target_belongs_to_action(e) => (
                Diagnosis::CompletedAfterAll,
                format!("\"{}\" ist vollständig am Ziel angekommen.", e.original_name),
                vec![],
            ),
            (true, false, _) => (
                Diagnosis::NothingHappened,
                format!("\"{}\" liegt unverändert am Ausgangsort.", e.original_name),
                vec![],
            ),
            (false, false, _) => (
                Diagnosis::DataMissing,
                format!(
                    "\"{}\" ist weder am Ausgangsort noch am Ziel auffindbar.",
                    e.original_name
                ),
                vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
            ),
            (_, true, _) => (
                Diagnosis::Inconclusive,
                format!(
                    "Am Ziel von \"{}\" liegt eine Datei ohne passenden Identitaetsnachweis.",
                    e.original_name
                ),
                vec![wahl("acknowledge", "Von Hand klären", false)],
            ),
        },

        // ------------------------------------------------------------------
        OpState::RollbackPending => (
            if src_da && !dst_da { Diagnosis::NothingHappened } else { Diagnosis::Inconclusive },
            format!(
                "Die Rücknahme von \"{}\" wurde unterbrochen. Quelle {}, Ziel {}.",
                e.original_name,
                if src_da { "vorhanden" } else { "fehlt" },
                if dst_da { "vorhanden" } else { "fehlt" }
            ),
            vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
        ),

        // ------------------------------------------------------------------
        zustand => (
            Diagnosis::Inconclusive,
            format!("Unerwarteter Zustand \"{}\" für \"{}\".", zustand.as_str(), e.original_name),
            vec![wahl("acknowledge", "Zur Kenntnis genommen", false)],
        ),
    };

    RecoveryItem {
        action_id: e.id,
        kind: e.kind.clone(),
        state: e.state.clone(),
        src: e.src.clone(),
        dst: e.dst.clone(),
        temp: e.temp.clone(),
        original_name: e.original_name.clone(),
        size: e.size,
        ts: e.ts,
        diagnosis: diagnose.as_str().to_string(),
        severity: diagnose.severity().to_string(),
        explanation: erklaerung,
        choices: wahlen,
        src_exists: src_da,
        dst_exists: dst_da,
        temp_exists: temp_da,
    }
}

fn wahl(id: &str, label: &str, touches_data: bool) -> RecoveryChoice {
    RecoveryChoice { id: id.into(), label: label.into(), touches_data }
}

/// Fuehrt die vom Nutzer gewaehlte Aufloesung aus.
pub fn resolve(conn: &Connection, action_id: i64, choice: &str) -> Result<String> {
    let _mutation = journal::begin_mutation()?;
    let mut e = journal::by_id(conn, action_id)?;
    if e.state().is_terminal() {
        return Err(AppError::Invalid("Dieser Fall wurde bereits geklärt".into()));
    }
    let aktuell = diagnose(&e);
    if !aktuell.choices.iter().any(|candidate| candidate.id == choice) {
        return Err(AppError::Invalid(
            "Diese Recovery-Wahl ist für den aktuellen Dateizustand nicht sicher".into(),
        ));
    }

    match choice {
        "acknowledge" | "keep_both" | "keep_temp" => {
            journal::fail(
                conn,
                action_id,
                OpState::Failed,
                "Vom Nutzer als geklärt markiert",
            )?;
            Ok("Vermerk geschlossen. Am Dateisystem wurde nichts verändert.".into())
        }

        "delete_temp" => {
            let temp = e
                .temp
                .as_deref()
                .ok_or_else(|| AppError::Invalid("Zu diesem Fall gehört keine Zwischendatei".into()))?;
            // Nur entfernen, wenn es nachweislich unsere eigene Zwischendatei
            // ist **und** das Original noch existiert.
            let name = Path::new(temp)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !journal::is_temp_name(&name) {
                return Err(AppError::Invalid(
                    "Das ist keine Zwischendatei von FileTriage - sie wird nicht angefasst".into(),
                ));
            }
            if !temp_belongs_to_action(&e) {
                return Err(AppError::Changed(
                    "Die Zwischendatei hat nicht mehr die im Journal gespeicherte Identität".into(),
                ));
            }
            let mut source=fsmeta::LockedFile::open(Path::new(&e.src))?;
            let mut locked=fsmeta::LockedFile::open(Path::new(temp))?;
            if !locked_identity_matches(&source,e.file_id(),e.source_strong_id())
                || !locked_identity_matches(&locked,e.temp_file_id(),e.temp_strong_id()) {
                return Err(AppError::Changed("Quelle oder Zwischendatei besitzt nicht mehr die persistierte Identität".into()))
            }
            let source_hash=e.src_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Quelle; manuelle Recovery erforderlich".into()))?;
            let temp_hash=e.temp_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Zwischendatei; manuelle Recovery erforderlich".into()))?;
            if source.full_hash()?!=source_hash||locked.full_hash()?!=temp_hash{
                return Err(AppError::Changed("Quelle oder Zwischendatei wurde verändert; manuelle Recovery erforderlich".into()))
            }
            locked.delete()?;
            journal::set_temp(conn, action_id, None)?;
            journal::fail(conn, action_id, OpState::RolledBack, "Zwischendatei entfernt")?;
            Ok("Die unvollständige Kopie wurde entfernt. Das Original ist unverändert.".into())
        }

        "finish_move" => {
            let dst = e
                .dst
                .clone()
                .ok_or_else(|| AppError::Invalid("Zu diesem Fall gehört kein Ziel".into()))?;

            let mut target=if let Some(temp)=e.temp.as_deref().filter(|temp|existiert(temp)&&!existiert(&dst)){
                let mut locked=fsmeta::LockedFile::open(Path::new(temp))?;
                if !locked_identity_matches(&locked,e.temp_file_id(),e.temp_strong_id()){return Err(AppError::Changed("Zwischendatei wurde ausgetauscht".into()))}
                let hash=e.temp_hash.as_deref().filter(|h|e.verified_hash.as_deref()==Some(*h)).ok_or_else(||AppError::Changed("Kein eindeutiger persistierter Vollhash der Kopie".into()))?;
                if locked.full_hash()?!=hash{return Err(AppError::Changed("Zwischendatei wurde inhaltlich verändert".into()))}
                locked.rename_to(Path::new(&dst))?;
                journal::set_target_evidence(conn,action_id,Some(locked.id()),locked.strong_id())?;
                journal::set_temp(conn,action_id,None)?;
                e=journal::by_id(conn,action_id)?;
                locked
            }else{
                if !existiert(&dst){return Err(AppError::Invalid("Am Ziel liegt nichts. Das Verschieben lässt sich nicht abschließen.".into()))}
                fsmeta::LockedFile::open(Path::new(&dst))?
            };
            if existiert(&e.src){
                let mut source=fsmeta::LockedFile::open(Path::new(&e.src))?;
                if !locked_identity_matches(&source,e.file_id(),e.source_strong_id())||!locked_identity_matches(&target,e.dst_file_id(),e.dst_strong_id()){
                    return Err(AppError::Changed("Quelle oder Ziel wurde ausgetauscht".into()))
                }
                let expected=e.verified_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Kopie".into()))?;
                let source_hash=e.src_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Quelle".into()))?;
                if source.full_hash()?!=source_hash||target.full_hash()?!=expected||source_hash!=expected{return Err(AppError::Invalid("Original und Zieldatei unterscheiden sich inhaltlich. Das Original bleibt erhalten.".into()))}
                crate::fileops::run_recovery_test_hook("after_target_verify_before_source_delete",Path::new(&e.src),Path::new(&dst));
                source.delete()?;
            }
            drop(target);

            let fp = fsmeta::inspect(Path::new(&dst)).ok().map(|s| s.fingerprint);
            journal::complete(conn, action_id, fp, journal::VerifyLevel::Full)?;
            let _ = crate::fileops::index_after_move(conn, Path::new(&e.src), Path::new(&dst));
            Ok("Das Verschieben wurde abgeschlossen.".into())
        }

        "undo_move" => {
            let dst = e
                .dst
                .as_deref()
                .ok_or_else(|| AppError::Invalid("Zu diesem Fall gehört kein Ziel".into()))?;
            if !existiert(&e.src) {
                return Err(AppError::Invalid(
                    "Das Original fehlt. Die Kopie am Ziel wird nicht entfernt.".into(),
                ));
            }
            if existiert(dst) {
                let mut source=fsmeta::LockedFile::open(Path::new(&e.src))?;
                let mut locked=fsmeta::LockedFile::open(Path::new(dst))?;
                let hash=e.verified_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Kopie".into()))?;
                let source_hash=e.src_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Vollhash der Quelle".into()))?;
                if !locked_identity_matches(&source,e.file_id(),e.source_strong_id())||!locked_identity_matches(&locked,e.dst_file_id(),e.dst_strong_id())||source.full_hash()?!=source_hash||locked.full_hash()?!=hash{return Err(AppError::Changed("Quelle oder Zielkopie wurde ausgetauscht oder verändert; manuelle Recovery erforderlich".into()))}
                locked.delete()?;
            }
            journal::fail(conn, action_id, OpState::RolledBack, "Vom Nutzer zurückgenommen")?;
            Ok("Die Kopie wurde entfernt, das Original bleibt an seinem Platz.".into())
        }

        "finish_recycle" => {
            let stage=e.temp.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Papierkorb-Stagingpfad".into()))?;
            let mut locked=fsmeta::LockedFile::open(Path::new(stage))?;
            let hash=e.temp_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Staging-Vollhash".into()))?;
            if !locked_identity_matches(&locked,e.temp_file_id(),e.temp_strong_id())||locked.full_hash()?!=hash{return Err(AppError::Changed("Staging-Datei wurde ausgetauscht oder verändert".into()))}
            let von=crate::db::now_ts()-2;
            drop(locked);
            trash::delete(Path::new(stage)).map_err(|error|AppError::Trash(error.to_string()))?;
            journal::set_trash_window(conn,action_id,von,crate::db::now_ts()+2)?;
            journal::complete(conn,action_id,None,journal::VerifyLevel::Full)?;
            Ok("Die sicher zwischengelagerte Datei wurde in den Papierkorb verschoben.".into())
        }

        "restore_staged" => {
            if paths::extended(Path::new(&e.src)).exists(){return Err(AppError::Invalid("Am ursprünglichen Pfad liegt bereits eine Datei; Staging bleibt unangetastet".into()))}
            let stage=e.temp.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Papierkorb-Stagingpfad".into()))?;
            let mut locked=fsmeta::LockedFile::open(Path::new(stage))?;
            let hash=e.temp_hash.as_deref().ok_or_else(||AppError::Changed("Kein persistierter Staging-Vollhash".into()))?;
            if !locked_identity_matches(&locked,e.temp_file_id(),e.temp_strong_id())||locked.full_hash()?!=hash{return Err(AppError::Changed("Staging-Datei wurde ausgetauscht oder verändert".into()))}
            locked.rename_to(Path::new(&e.src))?;
            journal::set_temp(conn,action_id,None)?;
            journal::fail(conn,action_id,OpState::RolledBack,"Papierkorb-Staging zurückgelegt")?;
            Ok("Die Datei wurde sicher an ihren ursprünglichen Pfad zurückgelegt.".into())
        }

        andere => Err(AppError::Invalid(format!("Unbekannte Wahl: {andere}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::fsmeta::Fingerprint;
    use crate::journal::Intent;
    use crate::testutil::TempDir;

    fn setup() -> (TempDir, Connection) {
        let dir = TempDir::new("recovery");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let c = db.conn().unwrap();
        (dir, c)
    }

    fn plane(c: &Connection, kind: &str, src: &Path, dst: Option<&Path>) -> i64 {
        let fp = fsmeta::inspect(src).map(|s| s.fingerprint).unwrap_or(Fingerprint {
            size: 4,
            modified: 1,
            created: 1,
            modified_nanos: 0,
        });
        let src_s = paths::display_path(src);
        let dst_s = dst.map(paths::display_path);
        let hash=crate::dup::full_hash(src).ok();
        journal::plan(
            c,
            &Intent {
                kind,
                src: &src_s,
                dst: dst_s.as_deref(),
                original_name: &src.file_name().unwrap().to_string_lossy(),
                fingerprint: fp,
                file_id: fsmeta::file_id(src),
                strong_file_id: fsmeta::strong_file_id(src),
                src_hash: hash.as_deref(),
                cross_volume: true,
                undoable: true,
                batch_id: None,
                session_id: "test",
            },
        )
        .unwrap()
    }

    fn verified(c:&Connection,id:i64,dst:&Path){
        journal::set_target_evidence(c,id,fsmeta::file_id(dst),fsmeta::strong_file_id(dst)).unwrap();
        journal::set_verified_hash(c,id,&crate::dup::full_hash(dst).unwrap()).unwrap();
    }

    #[cfg(windows)]
    fn restore_write_time(path:&Path,value:u64){
        use std::os::windows::fs::{MetadataExt,OpenOptionsExt};use std::os::windows::io::AsRawHandle;
        #[repr(C)]struct FileTime{low:u32,high:u32}
        #[link(name="kernel32")]unsafe extern "system"{fn SetFileTime(h:isize,c:*const FileTime,a:*const FileTime,w:*const FileTime)->i32;}
        let _=MetadataExt::last_write_time(&std::fs::metadata(path).unwrap());
        let f=std::fs::OpenOptions::new().access_mode(0x0100).share_mode(7).open(path).unwrap();let t=FileTime{low:value as u32,high:(value>>32)as u32};assert_ne!(unsafe{SetFileTime(f.as_raw_handle() as isize,std::ptr::null(),std::ptr::null(),&t)},0);
    }

    #[test]
    fn geplant_und_nichts_passiert_wird_erkannt() {
        let (dir, c) = setup();
        let src = dir.file("a.txt", b"x");
        let dst = dir.path().join("ziel").join("a.txt");
        plane(&c, "move", &src, Some(&dst));

        let bericht = inspect(&c, false).unwrap();
        assert_eq!(bericht.items.len(), 1);
        assert_eq!(bericht.items[0].diagnosis, "nothing_happened");
        assert_eq!(bericht.critical, 0);
    }

    #[test]
    fn beide_kopien_vorhanden_wird_niemals_automatisch_aufgeloest() {
        let (dir, c) = setup();
        let src = dir.file("a.txt", b"inhalt");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("a.txt");
        std::fs::write(&dst, b"inhalt").unwrap();
        let id = plane(&c, "move", &src, Some(&dst));
        verified(&c,id,&dst);
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();

        let bericht = inspect(&c, true).unwrap();
        assert_eq!(bericht.auto_resolved, 0, "hier darf nichts automatisch entschieden werden");
        assert_eq!(bericht.items.len(), 1);
        assert_eq!(bericht.items[0].diagnosis, "both_copies_present");
        assert!(src.exists() && dst.exists(), "keine Datei darf verschwinden");
        assert!(bericht.items[0].choices.iter().any(|w| w.id == "finish_move"));
    }

    #[cfg(windows)]
    #[test]
    fn finish_move_haelt_beide_objekte_bis_zur_quellloeschung_gebunden(){
        use std::{cell::Cell,rc::Rc};let(dir,c)=setup();let src=dir.file("source.bin",b"same");let dst=dir.dir("ziel").join("source.bin");std::fs::write(&dst,b"same").unwrap();
        let id=plane(&c,"move",&src,Some(&dst));verified(&c,id,&dst);journal::set_state(&c,id,OpState::SourceRemovalPending).unwrap();
        let blocked=Rc::new(Cell::new(false));let seen=blocked.clone();crate::fileops::crashpoint::set_hook(move|point,source,target|if point=="after_target_verify_before_source_delete"{
            seen.set(std::fs::OpenOptions::new().write(true).open(source).is_err()&&std::fs::remove_file(source).is_err()&&std::fs::OpenOptions::new().write(true).open(target).is_err()&&std::fs::remove_file(target).is_err());
        });let _reset=crate::fileops::crashpoint::Reset;resolve(&c,id,"finish_move").unwrap();assert!(blocked.get());assert!(!src.exists());assert_eq!(std::fs::read(dst).unwrap(),b"same");
    }

    #[test]
    fn verschollene_datei_ist_kritisch() {
        let (dir, c) = setup();
        let src = dir.file("weg.txt", b"x");
        let dst = dir.path().join("ziel").join("weg.txt");
        let id = plane(&c, "move", &src, Some(&dst));
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();
        std::fs::remove_file(&src).unwrap();

        let bericht = inspect(&c, true).unwrap();
        assert_eq!(bericht.critical, 1);
        assert_eq!(bericht.items[0].diagnosis, "data_missing");
    }

    #[test]
    fn zwischendatei_wird_nur_mit_intaktem_original_entfernt() {
        let (dir, c) = setup();
        let src = dir.file("a.bin", b"originaldaten");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("a.bin");
        let id = plane(&c, "move", &src, Some(&dst));
        let temp = ziel.join(journal::temp_name(id, "a.bin"));
        std::fs::write(&temp, b"halbe").unwrap();
        journal::set_temp(&c, id, Some(&paths::display_path(&temp))).unwrap();
        journal::set_temp_evidence(&c,id,fsmeta::file_id(&temp),fsmeta::strong_file_id(&temp),&crate::dup::full_hash(&temp).unwrap()).unwrap();
        journal::set_state(&c, id, OpState::Copying).unwrap();

        let bericht = inspect(&c, true).unwrap();
        assert_eq!(bericht.items[0].diagnosis, "leftover_temp_file");
        assert!(temp.exists(), "ohne Anweisung wird nichts entfernt");

        // Ohne Original wird die Zwischendatei nicht angetastet.
        std::fs::remove_file(&src).unwrap();
        assert!(resolve(&c, id, "delete_temp").is_err());
        assert!(temp.exists());

        // Eine am selben Pfad neu angelegte Quelle ist kein Beweis für das
        // persistierte Original, auch wenn die Bytes gleich sind.
        std::fs::write(&src, b"originaldaten").unwrap();
        assert!(resolve(&c, id, "delete_temp").is_err());
        assert!(temp.exists());
        assert!(src.exists());
    }

    #[test]
    fn zwischendatei_darf_nur_neben_dem_unveraenderten_original_entfernt_werden(){
        let(dir,c)=setup();let src=dir.file("a.bin",b"originaldaten");let dst=dir.dir("ziel").join("a.bin");let id=plane(&c,"move",&src,Some(&dst));
        let temp=dst.parent().unwrap().join(journal::temp_name(id,"a.bin"));std::fs::write(&temp,b"halbe").unwrap();
        journal::set_temp(&c,id,Some(&paths::display_path(&temp))).unwrap();journal::set_temp_evidence(&c,id,fsmeta::file_id(&temp),fsmeta::strong_file_id(&temp),&crate::dup::full_hash(&temp).unwrap()).unwrap();journal::set_state(&c,id,OpState::Copying).unwrap();
        resolve(&c,id,"delete_temp").unwrap();assert!(!temp.exists());assert_eq!(std::fs::read(src).unwrap(),b"originaldaten");
    }

    #[test]
    fn alter_journaleintrag_ohne_vollhash_bleibt_manuell(){
        let(dir,c)=setup();let src=dir.file("a.bin",b"original");let dst=dir.dir("ziel").join("a.bin");let id=plane(&c,"move",&src,Some(&dst));let temp=dst.parent().unwrap().join(journal::temp_name(id,"a.bin"));std::fs::write(&temp,b"copy").unwrap();
        journal::set_temp(&c,id,Some(&paths::display_path(&temp))).unwrap();journal::set_temp_identity(&c,id,fsmeta::file_id(&temp)).unwrap();journal::set_state(&c,id,OpState::Copying).unwrap();
        let report=inspect(&c,false).unwrap();assert_eq!(report.items[0].diagnosis,Diagnosis::Inconclusive.as_str());assert!(resolve(&c,id,"delete_temp").is_err());assert!(temp.exists());assert!(src.exists());
    }

    #[test]
    fn abschluss_verweigert_bei_abweichendem_inhalt() {
        let (dir, c) = setup();
        let src = dir.file("a.bin", b"originaldaten");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("a.bin");
        // Gleiche Groesse, anderer Inhalt.
        std::fs::write(&dst, b"faelschungxxx").unwrap();
        let id = plane(&c, "move", &src, Some(&dst));
        verified(&c,id,&dst);
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();

        let fehler = resolve(&c, id, "finish_move").unwrap_err();
        assert!(fehler.to_string().contains("unterscheiden sich"), "{fehler}");
        assert!(src.exists(), "das Original darf nicht entfernt werden");
    }

    #[test]
    fn abschluss_entfernt_original_nur_nach_inhaltsvergleich() {
        let (dir, c) = setup();
        let src = dir.file("a.bin", b"gleicher inhalt");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("a.bin");
        std::fs::write(&dst, b"gleicher inhalt").unwrap();
        let id = plane(&c, "move", &src, Some(&dst));
        verified(&c,id,&dst);
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();

        let meldung = resolve(&c, id, "finish_move").unwrap();
        assert!(meldung.contains("abgeschlossen"));
        assert!(!src.exists());
        assert!(dst.exists());
        assert_eq!(journal::by_id(&c, id).unwrap().state(), OpState::Completed);
    }

    #[test]
    fn ruecknahme_entfernt_kopie_nur_bei_vorhandenem_original() {
        let (dir, c) = setup();
        let src = dir.file("a.bin", b"inhalt");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("a.bin");
        std::fs::write(&dst, b"inhalt").unwrap();
        let id = plane(&c, "move", &src, Some(&dst));
        verified(&c,id,&dst);
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();

        resolve(&c, id, "undo_move").unwrap();
        assert!(src.exists());
        assert!(!dst.exists());
    }

    #[test]
    fn fremde_datei_wird_nicht_als_zwischendatei_entfernt() {
        let (dir, c) = setup();
        let src = dir.file("a.bin", b"x");
        let ziel = dir.dir("ziel");
        let fremd = ziel.join("wichtige-fremde-datei.txt");
        std::fs::write(&fremd, b"nicht anfassen").unwrap();

        let id = plane(&c, "move", &src, Some(&ziel.join("a.bin")));
        // Ein manipuliertes Journal zeigt auf eine fremde Datei.
        journal::set_temp(&c, id, Some(&paths::display_path(&fremd))).unwrap();
        journal::set_state(&c, id, OpState::Copying).unwrap();

        assert!(resolve(&c, id, "delete_temp").is_err());
        assert!(fremd.exists(), "nur eigene Zwischendateien duerfen entfernt werden");
    }

    #[test]
    fn geplante_aber_nie_ausgefuehrte_aktion_uebernimmt_keine_fremde_zieldatei() {
        let (dir, c) = setup();
        let src = dir.file("quelle.bin", b"original");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("quelle.bin");
        let id = plane(&c, "move", &src, Some(&dst));

        std::fs::remove_file(&src).unwrap();
        std::fs::write(&dst, b"fremd!!!").unwrap();

        let bericht = inspect(&c, true).unwrap();
        assert_eq!(bericht.auto_resolved, 0);
        assert_eq!(bericht.items.len(), 1);
        assert_eq!(bericht.items[0].diagnosis, "inconclusive");
        assert!(!journal::by_id(&c, id).unwrap().state().is_terminal());
        assert_eq!(std::fs::read(&dst).unwrap(), b"fremd!!!");
    }

    #[test]
    fn undo_move_loescht_keine_ersetzte_zieldatei_gleicher_groesse() {
        let (dir, c) = setup();
        let src = dir.file("quelle.bin", b"AAAAAAAA");
        let ziel = dir.dir("ziel");
        let dst = ziel.join("quelle.bin");
        std::fs::write(&dst, b"AAAAAAAA").unwrap();
        let id = plane(&c, "move", &src, Some(&dst));
        verified(&c,id,&dst);
        journal::set_state(&c, id, OpState::SourceRemovalPending).unwrap();

        std::fs::remove_file(&dst).unwrap();
        std::fs::write(&dst, b"AAAAAAAA").unwrap();

        assert!(resolve(&c, id, "undo_move").is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), b"AAAAAAAA");
        let bericht = inspect(&c, false).unwrap();
        assert_eq!(bericht.items[0].diagnosis, "inconclusive");
    }

    #[cfg(windows)]
    #[test]
    fn undo_move_loescht_keine_in_place_geaenderte_kopie_mit_gleicher_file_id(){
        use std::os::windows::fs::MetadataExt;let(dir,c)=setup();let src=dir.file("source.bin",b"AAAAAAAA");let dst=dir.dir("ziel").join("source.bin");std::fs::write(&dst,b"AAAAAAAA").unwrap();let original_time=std::fs::metadata(&dst).unwrap().last_write_time();let id=plane(&c,"move",&src,Some(&dst));verified(&c,id,&dst);journal::set_state(&c,id,OpState::SourceRemovalPending).unwrap();let before=fsmeta::file_id(&dst).unwrap();std::fs::write(&dst,b"BBBBBBBB").unwrap();restore_write_time(&dst,original_time);assert_eq!(before.index,fsmeta::file_id(&dst).unwrap().index);assert!(resolve(&c,id,"undo_move").is_err());assert_eq!(std::fs::read(&dst).unwrap(),b"BBBBBBBB");
    }

    #[test]
    fn hardlink_aenderung_der_kopie_verhindert_desktruktiven_rollback(){
        let(dir,c)=setup();let src=dir.file("source.bin",b"AAAAAAAA");let dst=dir.dir("ziel").join("source.bin");std::fs::write(&dst,b"AAAAAAAA").unwrap();let link=dir.path().join("link.bin");if !crate::testutil::make_hardlink(&link,&dst){return}let id=plane(&c,"move",&src,Some(&dst));verified(&c,id,&dst);journal::set_state(&c,id,OpState::SourceRemovalPending).unwrap();std::fs::write(&link,b"CCCCCCCC").unwrap();assert!(resolve(&c,id,"undo_move").is_err());assert!(dst.exists());
    }

    #[test]
    fn abgeschlossene_faelle_tauchen_nicht_im_bericht_auf() {
        let (dir, c) = setup();
        let src = dir.file("a.txt", b"x");
        let id = plane(&c, "move", &src, Some(&dir.path().join("z").join("a.txt")));
        journal::complete(&c, id, None, journal::VerifyLevel::NotNeeded).unwrap();
        assert!(inspect(&c, true).unwrap().items.is_empty());
    }
}

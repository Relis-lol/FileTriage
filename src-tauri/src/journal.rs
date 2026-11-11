//! Das Operationsjournal.
//!
//! Kernidee: **Erst schreiben, dann handeln.** Vor jeder Veraenderung am
//! Dateisystem steht ein festgeschriebener Journaleintrag mit der Absicht.
//! Danach wandert der Eintrag durch klar benannte Zustaende. Stirbt der Prozess
//! an irgendeiner Stelle, laesst sich beim naechsten Start aus Journal plus
//! Dateisystem eindeutig ableiten, was passiert ist - oder zumindest
//! feststellen, dass es *nicht* eindeutig ist.
//!
//! Ein Eintrag im Zustand [`OpState::Completed`] ist die einzige Aussage
//! "fertig". Alles andere ist im Zweifel unfertig und wird beim Start gemeldet.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Condvar, Mutex, OnceLock};

use crate::db;
use crate::error::{AppError, Result};
use crate::fsmeta::{FileId, Fingerprint, StrongFileId};
use crate::paths;

#[derive(Default)]
struct GateState {
    shutting_down: bool,
    active: usize,
}

/// Serialisiert den letzten Journal-Check beim Beenden gegen jede neue
/// Dateisystemmutation. Ein Permit lebt fuer die gesamte Operation, nicht nur
/// fuer das INSERT der Absicht.
pub(crate) struct MutationGate {
    state: Mutex<GateState>,
    idle: Condvar,
}

impl MutationGate {
    pub(crate) const fn new() -> Self {
        Self { state: Mutex::new(GateState { shutting_down: false, active: 0 }), idle: Condvar::new() }
    }

    pub(crate) fn acquire(&self) -> Result<MutationPermit<'_>> {
        let mut state = self.state.lock().map_err(|_| AppError::JournalUnavailable("Mutationssperre ist vergiftet".into()))?;
        if state.shutting_down {
            return Err(AppError::JournalUnavailable("FileTriage wird beendet; neue Dateioperationen sind gesperrt".into()));
        }
        state.active += 1;
        Ok(MutationPermit { gate: self })
    }

    pub(crate) fn begin_shutdown(&self) -> Result<ShutdownPermit<'_>> {
        let mut state = self.state.lock().map_err(|_| AppError::JournalUnavailable("Mutationssperre ist vergiftet".into()))?;
        state.shutting_down = true;
        while state.active != 0 {
            state = self.idle.wait(state).map_err(|_| AppError::JournalUnavailable("Mutationssperre ist vergiftet".into()))?;
        }
        Ok(ShutdownPermit { _gate: self })
    }
}

pub(crate) struct MutationPermit<'a> { gate: &'a MutationGate }
impl Drop for MutationPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.active = state.active.saturating_sub(1);
            if state.active == 0 { self.gate.idle.notify_all(); }
        }
    }
}
pub(crate) struct ShutdownPermit<'a> { _gate: &'a MutationGate }

fn mutation_gate() -> &'static MutationGate {
    static GATE: OnceLock<MutationGate> = OnceLock::new();
    GATE.get_or_init(MutationGate::new)
}

pub(crate) fn begin_mutation() -> Result<MutationPermit<'static>> { mutation_gate().acquire() }
pub(crate) fn begin_shutdown() -> Result<ShutdownPermit<'static>> { mutation_gate().begin_shutdown() }

/// Zustaende einer Dateioperation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    /// Absicht festgeschrieben, am Dateisystem ist noch nichts passiert.
    Planned,
    /// Kopie ueber Volumegrenzen laeuft; es existiert eine Zwischendatei.
    Copying,
    /// Kopie geschrieben, aber noch nicht geprueft.
    Copied,
    /// Kopie geprueft und an ihren endgueltigen Platz gebracht.
    Verified,
    /// Ziel ist vollstaendig; die Quelle wird gerade entfernt.
    SourceRemovalPending,
    /// Abgeschlossen. Nur dieser Zustand bedeutet Erfolg.
    Completed,
    /// Etwas ist schiefgegangen; die Ruecknahme laeuft.
    RollbackPending,
    /// Ruecknahme abgeschlossen, Ausgangszustand wiederhergestellt.
    RolledBack,
    /// Fehlgeschlagen, ohne dass Nutzerdaten veraendert wurden.
    Failed,
}

impl OpState {
    pub fn as_str(self) -> &'static str {
        match self {
            OpState::Planned => "planned",
            OpState::Copying => "copying",
            OpState::Copied => "copied",
            OpState::Verified => "verified",
            OpState::SourceRemovalPending => "source_removal_pending",
            OpState::Completed => "completed",
            OpState::RollbackPending => "rollback_pending",
            OpState::RolledBack => "rolled_back",
            OpState::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> OpState {
        match s {
            "planned" => OpState::Planned,
            "copying" => OpState::Copying,
            "copied" => OpState::Copied,
            "verified" => OpState::Verified,
            "source_removal_pending" => OpState::SourceRemovalPending,
            "completed" => OpState::Completed,
            "rollback_pending" => OpState::RollbackPending,
            "rolled_back" => OpState::RolledBack,
            _ => OpState::Failed,
        }
    }

    /// Endzustaende brauchen beim Start keine Untersuchung mehr.
    pub fn is_terminal(self) -> bool {
        matches!(self, OpState::Completed | OpState::Failed | OpState::RolledBack)
    }
}

/// Wie gruendlich eine Kopie geprueft wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyLevel {
    /// Nicht noetig - Umbenennen innerhalb eines Volumes kopiert keine Daten.
    NotNeeded,
    /// Groesse plus Hash ueber Anfang und Ende.
    Partial,
    /// Vollstaendiger Hash ueber den gesamten Inhalt.
    Full,
}

impl VerifyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            VerifyLevel::NotNeeded => "not_needed",
            VerifyLevel::Partial => "partial",
            VerifyLevel::Full => "full",
        }
    }
}

/// Was eine Operation vorhat.
#[derive(Debug, Clone)]
pub struct Intent<'a> {
    pub kind: &'a str,
    pub src: &'a str,
    pub dst: Option<&'a str>,
    pub original_name: &'a str,
    pub fingerprint: Fingerprint,
    pub file_id: Option<FileId>,
    pub strong_file_id: Option<StrongFileId>,
    pub src_hash: Option<&'a str>,
    pub cross_volume: bool,
    pub undoable: bool,
    pub batch_id: Option<&'a str>,
    pub session_id: &'a str,
}

/// Ein Journaleintrag, wie ihn Recovery und Undo lesen.
#[derive(Debug, Clone, Serialize)]
pub struct OpRecord {
    pub id: i64,
    pub batch_id: Option<String>,
    pub session_id: String,
    pub ts: i64,
    pub finished_at: Option<i64>,
    pub kind: String,
    pub state: String,
    pub src: String,
    pub dst: Option<String>,
    pub temp: Option<String>,
    pub original_name: String,
    pub size: i64,
    pub src_mtime: i64,
    pub src_mtime_ns: i64,
    pub dst_size: Option<i64>,
    pub dst_mtime: Option<i64>,
    pub dst_mtime_ns: Option<i64>,
    pub temp_volume: Option<i64>,
    pub temp_index: Option<i64>,
    pub dst_volume: Option<i64>,
    pub dst_index: Option<i64>,
    pub file_volume: Option<i64>,
    pub file_index: Option<i64>,
    pub cross_volume: bool,
    pub verify: Option<String>,
    pub src_hash: Option<String>,
    pub verified_hash: Option<String>,
    pub temp_hash: Option<String>,
    pub source_strong_id: Option<String>,
    pub temp_strong_id: Option<String>,
    pub dst_strong_id: Option<String>,
    pub undone: bool,
    pub undoable: bool,
    pub error: Option<String>,
}

impl OpRecord {
    pub fn state(&self) -> OpState {
        OpState::from_str(&self.state)
    }

    /// Fingerabdruck der Quelle zum Zeitpunkt der Planung.
    pub fn src_fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            modified: self.src_mtime,
            modified_nanos: self.src_mtime_ns as u32,
            created: 0,
        }
    }

    /// Fingerabdruck des Ziels nach Abschluss, sofern erfasst.
    pub fn dst_fingerprint(&self) -> Option<Fingerprint> {
        Some(Fingerprint {
            size: self.dst_size?,
            modified: self.dst_mtime?,
            modified_nanos: self.dst_mtime_ns.unwrap_or(0) as u32,
            created: 0,
        })
    }

    pub fn file_id(&self) -> Option<FileId> {
        Some(FileId {
            volume: self.file_volume? as u32,
            index: self.file_index? as u64,
            links: 1,
        })
    }

    pub fn temp_file_id(&self) -> Option<FileId> {
        Some(FileId {
            volume: self.temp_volume? as u32,
            index: self.temp_index? as u64,
            links: 1,
        })
    }

    pub fn dst_file_id(&self) -> Option<FileId> {
        Some(FileId {
            volume: self.dst_volume? as u32,
            index: self.dst_index? as u64,
            links: 1,
        })
    }

    pub fn source_strong_id(&self) -> Option<StrongFileId> { self.source_strong_id.as_deref().and_then(StrongFileId::decode) }
    pub fn temp_strong_id(&self) -> Option<StrongFileId> { self.temp_strong_id.as_deref().and_then(StrongFileId::decode) }
    pub fn dst_strong_id(&self) -> Option<StrongFileId> { self.dst_strong_id.as_deref().and_then(StrongFileId::decode) }
}

const SPALTEN: &str = "id, batch_id, session_id, ts, finished_at, kind, state, src, dst, temp,
     original_name, size, src_mtime, src_mtime_ns, dst_size, dst_mtime, dst_mtime_ns,
     temp_volume, temp_index, dst_volume, dst_index,
     file_volume, file_index, cross_volume, verify, src_hash, verified_hash, temp_hash,
     source_strong_id, temp_strong_id, dst_strong_id, undone, undoable, error";

fn lies(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpRecord> {
    Ok(OpRecord {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        session_id: row.get(2)?,
        ts: row.get(3)?,
        finished_at: row.get(4)?,
        kind: row.get(5)?,
        state: row.get(6)?,
        src: row.get(7)?,
        dst: row.get(8)?,
        temp: row.get(9)?,
        original_name: row.get(10)?,
        size: row.get(11)?,
        src_mtime: row.get(12)?,
        src_mtime_ns: row.get(13)?,
        dst_size: row.get(14)?,
        dst_mtime: row.get(15)?,
        dst_mtime_ns: row.get(16)?,
        temp_volume: row.get(17)?,
        temp_index: row.get(18)?,
        dst_volume: row.get(19)?,
        dst_index: row.get(20)?,
        file_volume: row.get(21)?,
        file_index: row.get(22)?,
        cross_volume: row.get::<_, i32>(23)? != 0,
        verify: row.get(24)?,
        src_hash: row.get(25)?,
        verified_hash: row.get(26)?,
        temp_hash: row.get(27)?,
        source_strong_id: row.get(28)?,
        temp_strong_id: row.get(29)?,
        dst_strong_id: row.get(30)?,
        undone: row.get::<_, i32>(31)? != 0,
        undoable: row.get::<_, i32>(32)? != 0,
        error: row.get(33)?,
    })
}

/// Schreibt die Absicht fest. Ab hier weiss ein spaeterer Start, dass etwas
/// vorhatte zu passieren.
/// Schreibt die Absicht ins Journal, **bevor** eine Datei angefasst wird.
///
/// Hier steht die einzige Lizenzsperre des Programms. Diese Stelle ist mit
/// Bedacht gewaehlt: Der Grundsatz "erst schreiben, dann handeln" heisst, dass
/// jede veraendernde Operation zwingend hier vorbeikommt. Wer die Sperre
/// umgehen will, muss die Zustandsmaschine umgehen - und die ist es, die
/// beweist, wo die Dateien liegen.
///
/// Rueckgaengig und Wiederherstellung legen keine neue Absicht an und sind
/// deshalb nicht betroffen. Das ist Absicht: Wem waehrend der Arbeit die
/// Testphase auslaeuft, der muss seine letzte Aktion noch zuruecknehmen
/// koennen, und eine Wiederherstellung nach einem Absturz darf an einer
/// Lizenzfrage niemals scheitern.
pub fn plan(conn: &Connection, intent: &Intent<'_>) -> Result<i64> {
    // Auch direkte Plan-Aufrufer koennen den Shutdown nicht unterlaufen. Die
    // aeussere Operation haelt zusaetzlich ihr Permit bis zum letzten
    // Dateisystemschritt.
    let _plan_permit = begin_mutation()?;
    // Ein In-Memory-Index ist fuer Scans und Diagnose brauchbar, aber niemals
    // als Aktionsjournal: Nach einem Prozessabbruch waere die Absicht weg und
    // Recovery koennte nicht mehr beweisen, wo die Datei liegt.
    ensure_persistent_journal(conn)?;
    crate::license::ensure_may_modify(conn)?;

    conn.execute(
        "INSERT INTO actions
            (batch_id, session_id, ts, kind, state, src, dst, original_name, size, ext,
             src_mtime, src_mtime_ns, file_volume, file_index, src_hash, source_strong_id, cross_volume, undoable)
         VALUES (?1, ?2, ?3, ?4, 'planned', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            intent.batch_id,
            intent.session_id,
            db::now_ts(),
            intent.kind,
            intent.src,
            intent.dst,
            intent.original_name,
            intent.fingerprint.size,
            crate::classify::extension_of(intent.original_name),
            intent.fingerprint.modified,
            intent.fingerprint.modified_nanos as i64,
            intent.file_id.map(|i| i.volume as i64),
            intent.file_id.map(|i| i.index as i64),
            intent.src_hash,
            intent.strong_file_id.map(|v|v.encode()),
            intent.cross_volume as i32,
            intent.undoable as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Beweist, dass `main` eine Datei auf einem dauerhaften Datentraeger ist.
/// Schreibschutz wird anschliessend durch das INSERT selbst fail-closed
/// erkannt, noch bevor der Aufrufer eine Datei anfasst.
fn ensure_persistent_journal(conn: &Connection) -> Result<()> {
    let datei: String = conn
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| AppError::JournalUnavailable(format!("Datenbanktyp unbekannt: {e}")))?;
    if datei.trim().is_empty() {
        return Err(AppError::JournalUnavailable(
            "Der aktuelle Index liegt nur im Arbeitsspeicher".into(),
        ));
    }
    Ok(())
}

pub fn set_state(conn: &Connection, id: i64, state: OpState) -> Result<()> {
    conn.execute(
        "UPDATE actions SET state = ?1 WHERE id = ?2",
        rusqlite::params![state.as_str(), id],
    )?;
    Ok(())
}

pub fn set_temp(conn: &Connection, id: i64, temp: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE actions SET temp = ?1,
             temp_volume = CASE WHEN ?1 IS NULL THEN NULL ELSE temp_volume END,
             temp_index = CASE WHEN ?1 IS NULL THEN NULL ELSE temp_index END
         WHERE id = ?2",
        rusqlite::params![temp, id],
    )?;
    Ok(())
}

pub fn set_temp_identity(conn: &Connection, id: i64, file_id: Option<FileId>) -> Result<()> {
    let (volume, index) = file_id
        .map(|v| (Some(v.volume as i64), Some(v.index as i64)))
        .unwrap_or((None, None));
    conn.execute(
        "UPDATE actions SET temp_volume = ?1, temp_index = ?2 WHERE id = ?3",
        rusqlite::params![volume, index, id],
    )?;
    Ok(())
}

pub fn set_temp_evidence(conn:&Connection,id:i64,file_id:Option<FileId>,strong:Option<StrongFileId>,hash:&str)->Result<()> {
    set_temp_identity(conn,id,file_id)?;
    conn.execute("UPDATE actions SET temp_strong_id=?1,temp_hash=?2 WHERE id=?3",rusqlite::params![strong.map(|v|v.encode()),hash,id])?;
    Ok(())
}

pub fn set_target_identity(conn: &Connection, id: i64, file_id: Option<FileId>) -> Result<()> {
    let (volume, index) = file_id
        .map(|v| (Some(v.volume as i64), Some(v.index as i64)))
        .unwrap_or((None, None));
    conn.execute(
        "UPDATE actions SET dst_volume = ?1, dst_index = ?2 WHERE id = ?3",
        rusqlite::params![volume, index, id],
    )?;
    Ok(())
}

pub fn set_target_evidence(conn:&Connection,id:i64,file_id:Option<FileId>,strong:Option<StrongFileId>)->Result<()> {
    set_target_identity(conn,id,file_id)?;
    conn.execute("UPDATE actions SET dst_strong_id=?1 WHERE id=?2",rusqlite::params![strong.map(|v|v.encode()),id])?;
    Ok(())
}

pub fn set_verified_hash(conn: &Connection, id: i64, hash: &str) -> Result<()> {
    conn.execute("UPDATE actions SET verified_hash=?1 WHERE id=?2", rusqlite::params![hash,id])?;
    Ok(())
}

pub fn set_source_strong_identity(conn:&Connection,id:i64,strong:Option<StrongFileId>)->Result<()> {
    conn.execute("UPDATE actions SET source_strong_id=?1 WHERE id=?2",rusqlite::params![strong.map(|v|v.encode()),id])?;
    Ok(())
}

pub fn set_cross_volume(conn: &Connection, id: i64, cross: bool) -> Result<()> {
    conn.execute(
        "UPDATE actions SET cross_volume = ?1 WHERE id = ?2",
        rusqlite::params![cross as i32, id],
    )?;
    Ok(())
}

/// Merkt sich das Zeitfenster, in dem die Datei in den Papierkorb ging.
/// Ohne dieses Fenster laesst sich spaeter nicht beweisen, welcher
/// Papierkorbeintrag zu dieser Aktion gehoert.
pub fn set_trash_window(conn: &Connection, id: i64, von: i64, bis: i64) -> Result<()> {
    conn.execute(
        "UPDATE actions SET trash_from = ?1, trash_to = ?2 WHERE id = ?3",
        rusqlite::params![von, bis, id],
    )?;
    Ok(())
}

pub fn trash_window(conn: &Connection, id: i64) -> Result<Option<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT trash_from, trash_to FROM actions WHERE id = ?1")?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(r) => {
            let von: Option<i64> = r.get(0)?;
            let bis: Option<i64> = r.get(1)?;
            Ok(von.zip(bis))
        }
        None => Ok(None),
    }
}

/// Schliesst die Operation erfolgreich ab - der einzige Weg zu `completed`.
pub fn complete(
    conn: &Connection,
    id: i64,
    dst_fingerprint: Option<Fingerprint>,
    verify: VerifyLevel,
) -> Result<()> {
    conn.execute(
        "UPDATE actions SET state = 'completed', finished_at = ?1,
                            temp = CASE WHEN kind = 'delete_recycle' THEN temp ELSE NULL END,
                            dst_size = ?2, dst_mtime = ?3, dst_mtime_ns = ?4, verify = ?5
         WHERE id = ?6",
        rusqlite::params![
            db::now_ts(),
            dst_fingerprint.map(|f| f.size),
            dst_fingerprint.map(|f| f.modified),
            dst_fingerprint.map(|f| f.modified_nanos as i64),
            verify.as_str(),
            id
        ],
    )?;
    Ok(())
}

pub fn fail(conn: &Connection, id: i64, state: OpState, fehler: &str) -> Result<()> {
    conn.execute(
        "UPDATE actions SET state = ?1, finished_at = ?2, error = ?3 WHERE id = ?4",
        rusqlite::params![state.as_str(), db::now_ts(), fehler, id],
    )?;
    Ok(())
}

pub fn by_id(conn: &Connection, id: i64) -> Result<OpRecord> {
    let sql = format!("SELECT {SPALTEN} FROM actions WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(r) => Ok(lies(r)?),
        None => Err(AppError::NotFound(format!("Aktion {id}"))),
    }
}

/// Alle Eintraege, die beim Start untersucht werden muessen.
pub fn unfinished(conn: &Connection) -> Result<Vec<OpRecord>> {
    let sql = format!(
        "SELECT {SPALTEN} FROM actions
         WHERE state NOT IN ('completed', 'failed', 'rolled_back')
         ORDER BY id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], lies)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Abgeschlossene, noch nicht zurueckgenommene Aktionen - Grundlage fuer Undo.
pub fn undoable(conn: &Connection, limit: i64) -> Result<Vec<OpRecord>> {
    let sql = format!(
        "SELECT {SPALTEN} FROM actions
         WHERE state = 'completed' AND undone = 0 AND undoable = 1
         ORDER BY id DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit], lies)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn history(conn: &Connection, limit: i64) -> Result<Vec<OpRecord>> {
    let sql = format!("SELECT {SPALTEN} FROM actions ORDER BY id DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit], lies)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_undone(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("UPDATE actions SET undone = 1 WHERE id = ?1", [id])?;
    Ok(())
}

/// Kennung der laufenden Sitzung: ein Programmstart.
pub fn new_session_id() -> String {
    format!(
        "s{}-{}",
        db::now_ts(),
        std::process::id()
    )
}

/// Name der Zwischendatei fuer eine Kopie ueber Volumegrenzen.
///
/// Der Name enthaelt die Journal-ID, damit die Recovery beim naechsten Start
/// jede Zwischendatei genau einer Operation zuordnen kann. Er ist bewusst
/// erkennbar und nicht zufaellig.
pub fn temp_name(action_id: i64, original: &str) -> String {
    // Der Originalname bleibt als Suffix erhalten, damit im Explorer sichtbar
    // ist, worum es geht. Laenge begrenzen, damit MAX_PATH nicht sprengt.
    let kurz: String = original.chars().take(60).collect();
    format!(".filetriage-part-{action_id}-{kurz}")
}

/// Erkennt eine Zwischendatei von FileTriage am Namen.
pub fn is_temp_name(name: &str) -> bool {
    name.starts_with(".filetriage-part-")
}

/// Zusammenfassung einer Sitzung fuer das Session-Undo.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub moved: i64,
    pub renamed: i64,
    pub trashed: i64,
    pub deleted: i64,
    pub undoable: i64,
    pub undone: i64,
}

pub fn sessions(conn: &Connection, limit: i64) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, min(ts), max(ts),
                sum(kind = 'move'), sum(kind = 'rename'),
                sum(kind = 'delete_recycle'), sum(kind = 'delete_permanent'),
                sum(state = 'completed' AND undone = 0 AND undoable = 1),
                sum(undone = 1)
         FROM actions
         WHERE session_id <> ''
         GROUP BY session_id
         ORDER BY max(ts) DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit.max(1)], |r| {
            Ok(SessionSummary {
                session_id: r.get(0)?,
                started_at: r.get(1)?,
                ended_at: r.get(2)?,
                moved: r.get(3)?,
                renamed: r.get(4)?,
                trashed: r.get(5)?,
                deleted: r.get(6)?,
                undoable: r.get(7)?,
                undone: r.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Aktionen einer Sitzung, juengste zuerst - die sichere Reihenfolge fuer Undo.
pub fn session_actions(conn: &Connection, session_id: &str) -> Result<Vec<OpRecord>> {
    let sql = format!(
        "SELECT {SPALTEN} FROM actions
         WHERE session_id = ?1 AND state = 'completed' AND undone = 0
         ORDER BY id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([session_id], lies)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Praezise Pfadangabe fuer Meldungen.
pub fn kurz(pfad: &str) -> String {
    paths::path_key(pfad).rsplit('\\').next().unwrap_or(pfad).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::license::LicenseState;
    use crate::testutil::TempDir;

    fn conn() -> (TempDir, Connection) {
        let dir = TempDir::new("journal");
        let db = Db::open(dir.path().join("i.db")).unwrap();
        let c = db.conn().unwrap();
        (dir, c)
    }

    fn intent<'a>(src: &'a str, dst: &'a str, session: &'a str) -> Intent<'a> {
        Intent {
            kind: "move",
            src,
            dst: Some(dst),
            original_name: "a.txt",
            fingerprint: Fingerprint { size: 10, modified: 100, created: 90, modified_nanos: 5 },
            file_id: Some(FileId { volume: 7, index: 42, links: 1 }),
            strong_file_id: None,
            src_hash: None,
            cross_volume: false,
            undoable: true,
            batch_id: None,
            session_id: session,
        }
    }

    #[test]
    fn geplante_operation_gilt_nicht_als_erfolg() {
        let (_d, c) = conn();
        let id = plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s1")).unwrap();
        let r = by_id(&c, id).unwrap();
        assert_eq!(r.state(), OpState::Planned);
        assert!(!r.state().is_terminal());
        assert_eq!(unfinished(&c).unwrap().len(), 1);
        assert!(undoable(&c, 10).unwrap().is_empty(), "nur abgeschlossene Aktionen sind ruecknehmbar");
    }

    #[test]
    fn abschluss_erfasst_zielfingerabdruck() {
        let (_d, c) = conn();
        let id = plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s1")).unwrap();
        let ziel = Fingerprint { size: 10, modified: 200, created: 0, modified_nanos: 1 };
        complete(&c, id, Some(ziel), VerifyLevel::Full).unwrap();

        let r = by_id(&c, id).unwrap();
        assert_eq!(r.state(), OpState::Completed);
        assert_eq!(r.dst_fingerprint().unwrap().modified, 200);
        assert_eq!(r.verify.as_deref(), Some("full"));
        assert!(unfinished(&c).unwrap().is_empty());
        assert_eq!(undoable(&c, 10).unwrap().len(), 1);
    }

    #[test]
    fn fehlgeschlagene_operation_ist_endzustand_aber_kein_undo_kandidat() {
        let (_d, c) = conn();
        let id = plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s1")).unwrap();
        fail(&c, id, OpState::Failed, "Platte voll").unwrap();
        assert!(unfinished(&c).unwrap().is_empty());
        assert!(undoable(&c, 10).unwrap().is_empty());
        assert_eq!(by_id(&c, id).unwrap().error.as_deref(), Some("Platte voll"));
    }

    #[test]
    fn zwischenzustaende_bleiben_als_unfertig_sichtbar() {
        let (_d, c) = conn();
        for state in [
            OpState::Copying,
            OpState::Copied,
            OpState::Verified,
            OpState::SourceRemovalPending,
            OpState::RollbackPending,
        ] {
            let id = plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s1")).unwrap();
            set_state(&c, id, state).unwrap();
            assert!(!state.is_terminal(), "{state:?} darf kein Endzustand sein");
        }
        assert_eq!(unfinished(&c).unwrap().len(), 5);
    }

    #[test]
    fn sitzungen_werden_zusammengefasst() {
        let (_d, c) = conn();
        for _ in 0..3 {
            let id = plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s-alt")).unwrap();
            complete(&c, id, None, VerifyLevel::NotNeeded).unwrap();
        }
        let id = plan(&c, &intent("C:\\a\\b.txt", "C:\\b\\b.txt", "s-neu")).unwrap();
        complete(&c, id, None, VerifyLevel::NotNeeded).unwrap();

        let s = sessions(&c, 10).unwrap();
        assert_eq!(s.len(), 2);
        let alt = s.iter().find(|x| x.session_id == "s-alt").unwrap();
        assert_eq!(alt.moved, 3);
        assert_eq!(alt.undoable, 3);
        assert_eq!(session_actions(&c, "s-alt").unwrap().len(), 3);
    }

    #[test]
    fn zwischendateiname_ist_zuordenbar() {
        let name = temp_name(42, "Rechnung.pdf");
        assert!(is_temp_name(&name));
        assert!(name.contains("42"));
        assert!(!is_temp_name("Rechnung.pdf"));
    }

    #[test]
    fn papierkorb_staging_enthaelt_128_bit_aktionstoken(){
        let name=trash_stage_name(42,"bericht.txt").unwrap();
        let rest=name.strip_prefix(".filetriage-trash-42-").unwrap();
        let token=rest.split('-').next().unwrap();
        assert_eq!(token.len(),32);
        assert!(token.bytes().all(|b|b.is_ascii_hexdigit()));
    }

    #[test]
    fn papierkorb_tokens_sind_im_testsample_eindeutig(){
        let mut seen=std::collections::HashSet::new();
        for id in 0..1024 {
            let name=trash_stage_name(id,"x").unwrap();
            let token=name.split('-').nth(3).unwrap().to_owned();
            assert!(seen.insert(token));
        }
    }

    #[test]
    fn helper_shutdown_blockiert_plan_nach_laufender_mutation(){
        if std::env::var_os("FILETRIAGE_TEST_SHUTDOWN_GATE").is_none(){return}
        let(_d,c)=conn();let running=begin_mutation().unwrap();let t=std::thread::spawn(||begin_shutdown().unwrap());drop(running);let _shutdown=t.join().unwrap();assert!(plan(&c,&intent("C:\\a","C:\\b","shutdown")).is_err());
    }

    #[test]
    fn journal_plan_kann_shutdown_nicht_ueberholen(){
        let status=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","journal::tests::helper_shutdown_blockiert_plan_nach_laufender_mutation"]).env("FILETRIAGE_TEST_SHUTDOWN_GATE","1").status().unwrap();assert!(status.success());
    }

    #[test]
    fn in_memory_datenbank_kann_niemals_aktionsjournal_sein() {
        let db = Db::open_memory_for_reading().unwrap();
        let c = db.conn().unwrap();
        let fehler = crate::license::mit_zustand(LicenseState::Purchased, || {
            plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s-memory"))
                .unwrap_err()
        });
        assert_eq!(fehler.kind(), "journal_unavailable");
        let count: i64 = c.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn nicht_beschreibbare_db_stoppt_vor_der_planung() {
        let dir = TempDir::new("journal-readonly");
        let pfad = dir.path().join("i.db");
        let db = Db::open(&pfad).unwrap();
        drop(db);
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let c = Connection::open_with_flags(&pfad, flags).unwrap();
        let fehler = crate::license::mit_zustand(LicenseState::Purchased, || {
            plan(&c, &intent("C:\\a\\a.txt", "C:\\b\\a.txt", "s-readonly"))
                .unwrap_err()
        });
        assert_eq!(fehler.kind(), "db");
        let count: i64 = c.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}

pub fn mark_manual(conn:&Connection,id:i64,reason:&str)->Result<()> {
    conn.execute("UPDATE actions SET undoable=0, error=?1 WHERE id=?2",rusqlite::params![reason,id])?;
    Ok(())
}

pub fn trash_stage_name(action_id:i64,original:&str)->Result<String>{
    let mut nonce=[0u8;16];
    fill_os_random(&mut nonce)?;
    let token=nonce.iter().map(|b|format!("{b:02x}")).collect::<String>();
    Ok(format!(".filetriage-trash-{action_id}-{token}-{original}"))
}

#[cfg(windows)]
fn fill_os_random(bytes:&mut [u8])->Result<()> {
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG:u32=2;
    #[link(name="bcrypt")]
    unsafe extern "system" { fn BCryptGenRandom(algorithm:isize,buffer:*mut u8,size:u32,flags:u32)->i32; }
    let status=unsafe{BCryptGenRandom(0,bytes.as_mut_ptr(),bytes.len() as u32,BCRYPT_USE_SYSTEM_PREFERRED_RNG)};
    if status<0 { return Err(AppError::Io(format!("Windows-CSPRNG fehlgeschlagen (NTSTATUS 0x{:08x})",status as u32))) }
    Ok(())
}

#[cfg(not(windows))]
fn fill_os_random(bytes:&mut [u8])->Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(bytes)?;
    Ok(())
}

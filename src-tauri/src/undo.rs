//! Rueckgaengig machen von Dateiaktionen.
//!
//! Undo ist hier kein Komfortfeature, sondern eine Sicherheitsfunktion. Deshalb
//! gilt durchgehend: **Undo ueberschreibt nie eine neuere Fassung.**
//!
//! Vor jeder Ruecknahme wird geprueft,
//!
//! - ob am Zielort ueberhaupt noch die Datei liegt, die damals dorthin kam
//!   (ueber Dateikennung beziehungsweise Fingerabdruck aus dem Journal),
//! - ob der urspruengliche Platz noch frei ist.
//!
//! Ist eine der beiden Antworten unklar, bricht die Ruecknahme ab und meldet
//! den Konflikt. Lieber ein nicht ausgefuehrtes Undo als eine verlorene Datei.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::error::{AppError, Result};
use crate::fsmeta;
use crate::journal::{self, OpRecord, OpState};
use crate::paths;

/// So viele Aktionen bietet die Oberflaeche zum Rueckgaengigmachen an.
pub const HISTORY_LIMIT: i64 = 100;

#[derive(Debug, Clone, Serialize)]
pub struct ActionRecord {
    pub id: i64,
    pub batch_id: Option<String>,
    pub session_id: String,
    pub ts: i64,
    pub kind: String,
    pub state: String,
    pub src: String,
    pub dst: Option<String>,
    pub original_name: String,
    pub size: i64,
    pub undone: bool,
    pub undoable: bool,
    pub verify: Option<String>,
}

impl From<OpRecord> for ActionRecord {
    fn from(o: OpRecord) -> ActionRecord {
        ActionRecord {
            id: o.id,
            batch_id: o.batch_id,
            session_id: o.session_id,
            ts: o.ts,
            kind: o.kind,
            state: o.state,
            src: o.src,
            dst: o.dst,
            original_name: o.original_name,
            size: o.size,
            undone: o.undone,
            undoable: o.undoable,
            verify: o.verify,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UndoResult {
    pub action_id: i64,
    pub ok: bool,
    pub restored_to: Option<String>,
    pub error_kind: Option<String>,
    pub message: Option<String>,
}

impl UndoResult {
    fn err(id: i64, e: AppError) -> UndoResult {
        UndoResult {
            action_id: id,
            ok: false,
            restored_to: None,
            error_kind: Some(e.kind().to_string()),
            message: Some(e.to_string()),
        }
    }
}

pub fn history(conn: &Connection, limit: i64) -> Result<Vec<ActionRecord>> {
    Ok(journal::history(conn, limit.clamp(1, HISTORY_LIMIT))?
        .into_iter()
        .map(ActionRecord::from)
        .collect())
}

/// Letzte noch nicht zurueckgenommene Aktion.
pub fn last_undoable(conn: &Connection) -> Result<Option<ActionRecord>> {
    Ok(journal::undoable(conn, 1)?.into_iter().next().map(ActionRecord::from))
}

pub fn undo_last(conn: &Connection) -> Result<Option<UndoResult>> {
    match journal::undoable(conn, 1)?.into_iter().next() {
        Some(a) => Ok(Some(undo(conn, a.id))),
        None => Ok(None),
    }
}

/// Nimmt alle Aktionen eines Stapels zurueck, juengste zuerst.
pub fn undo_batch(conn: &Connection, batch_id: &str) -> Result<Vec<UndoResult>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM actions
         WHERE batch_id = ?1 AND state = 'completed' AND undone = 0 AND undoable = 1
         ORDER BY id DESC",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([batch_id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids.into_iter().map(|id| undo(conn, id)).collect())
}

pub fn undo(conn: &Connection, id: i64) -> UndoResult {
    match undo_inner(conn, id) {
        Ok(r) => r,
        Err(e) => UndoResult::err(id, e),
    }
}

fn undo_inner(conn: &Connection, id: i64) -> Result<UndoResult> {
    let _mutation = journal::begin_mutation()?;
    let a = journal::by_id(conn, id)?;
    pruefe_ruecknehmbar(&a)?;

    let ziel = PathBuf::from(&a.src);
    match a.kind.as_str() {
        "move" | "rename" => {
            let von = PathBuf::from(
                a.dst
                    .as_ref()
                    .ok_or_else(|| AppError::Invalid("Aktion ohne Ziel".into()))?,
            );
            restore_move(conn, &a, &von, &ziel)?;
        }
        "delete_recycle" => {
            restore_from_trash(conn, &a, &ziel)?;
            conn.execute(
                "UPDATE files SET state = 'pending', decided_at = NULL WHERE path_key = ?1",
                [paths::path_key(&a.src)],
            )?;
        }
        other => {
            return Err(AppError::Invalid(format!(
                "Aktion \"{other}\" kann nicht zurückgenommen werden"
            )));
        }
    }

    journal::mark_undone(conn, id)?;
    Ok(UndoResult {
        action_id: id,
        ok: true,
        restored_to: Some(paths::display_path(&ziel)),
        error_kind: None,
        message: None,
    })
}

fn pruefe_ruecknehmbar(a: &OpRecord) -> Result<()> {
    if a.undone {
        return Err(AppError::Invalid("Diese Aktion wurde bereits zurückgenommen".into()));
    }
    if !a.undoable {
        return Err(AppError::Invalid(
            "Endgültig gelöschte Dateien lassen sich nicht wiederherstellen".into(),
        ));
    }
    if a.state() != OpState::Completed {
        return Err(AppError::Invalid(format!(
            "Die Aktion ist nicht abgeschlossen (Zustand: {}). Sie gehoert in die \
             Wiederherstellung, nicht ins Rueckgaengigmachen.",
            a.state
        )));
    }
    Ok(())
}

fn restore_move(conn: &Connection, a: &OpRecord, von: &Path, nach: &Path) -> Result<()> {
    if !paths::extended(von).exists() {
        return Err(AppError::NotFound(format!(
            "{} liegt nicht mehr dort - die Datei wurde inzwischen anderweitig bewegt",
            von.display()
        )));
    }
    if paths::extended(nach).exists() {
        return Err(AppError::Invalid(format!(
            "Am ursprünglichen Platz liegt bereits eine Datei: {}",
            nach.display()
        )));
    }

    // Ist es ueberhaupt noch die Datei, die wir dorthin gebracht haben?
    let mut locked=fsmeta::LockedFile::open(von)?;
    // Bei einem abgeschlossenen Move ist die Datei am Ziel logisch weiterhin
    // das Benutzerobjekt. Bearbeitungen nach dem Move werden beim Undo
    // absichtlich mit zurueckgenommen. Die stabile File-ID beweist die
    // Objektidentitaet; die Mutation erfolgt anschliessend ueber denselben
    // Handle, sodass kein Austausch dazwischen moeglich ist.
    let erwartete_identitaet = a.dst_file_id().or_else(|| (!a.cross_volume).then(|| a.file_id()).flatten());
    let erwartete_starke_identitaet=a.dst_strong_id().or_else(||(!a.cross_volume).then(||a.source_strong_id()).flatten());
    let identitaet_passt = erwartete_identitaet.is_some_and(|erwartet|locked.id().volume==erwartet.volume&&locked.id().index==erwartet.index)
        && erwartete_starke_identitaet.is_some_and(|erwartet|locked.strong_id()==Some(erwartet));
    if !identitaet_passt {
        return Err(AppError::Changed(format!(
            "{} wurde seit dem Verschieben veraendert. Die neuere Fassung wird nicht \
             angetastet - bitte von Hand entscheiden.",
            von.display()
        )));
    }

    if let Some(parent) = nach.parent() {
        let eparent = paths::extended(parent);
        if !eparent.exists() {
            std::fs::create_dir_all(&eparent).map_err(|e| {
                AppError::from_io(&e, &format!("Ordner {} anlegen", parent.display()))
            })?;
        }
    }
    locked.rename_to(nach)?;

    crate::fileops::index_after_move(conn, von, nach)?;
    conn.execute(
        "UPDATE files SET state = 'pending', decided_at = NULL WHERE path_key = ?1",
        [paths::path_key(&paths::display_path(nach))],
    )?;
    Ok(())
}

/// Holt eine Datei aus dem Windows-Papierkorb an ihren alten Platz zurueck.
///
/// Die Zuordnung ist der heikle Teil: Der Papierkorb liefert nur einen
/// Anzeigenamen (bei Windows-Standardeinstellung ohne Endung) und den
/// Ursprungsordner. Mehrere Dateien koennen darauf passen. Deshalb wird
/// zusaetzlich das im Journal festgehaltene Zeitfenster verwendet - und wenn
/// danach immer noch mehr als ein Kandidat bleibt, wird **nicht** geraten.
fn restore_from_trash(conn: &Connection, a: &OpRecord, original: &Path) -> Result<()> {
    let items=crate::recycle::list_tolerant()?;
    restore_from_trash_with(conn,a,original,items,|item|{
        trash::os_limited::restore_all([item])?;
        Ok(())
    })
}

fn restore_from_trash_with<F>(conn:&Connection,a:&OpRecord,original:&Path,items:Vec<crate::recycle::RecycleItem>,restore:F)->Result<()>
where F:FnOnce(trash::TrashItem)->Result<()> {
    if paths::extended(original).exists() {
        return Err(AppError::Invalid(format!(
            "Am ursprünglichen Platz liegt bereits eine Datei: {}",
            original.display()
        )));
    }
    let staged = a.temp.as_deref().map(Path::new).ok_or_else(||
        AppError::Changed("Papierkorb-Aktion ohne persistierten eindeutigen Staging-Pfad".into()))?;
    let name = staged
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| AppError::Invalid("Aktion ohne Dateinamen".into()))?;
    let parent = staged
        .parent()
        .ok_or_else(|| AppError::Invalid("Aktion ohne Ordner".into()))?;

    let fenster = journal::trash_window(conn, a.id)?;
    let parent_key = paths::canonical_key(parent);
    let expected=a.temp_hash.as_deref().or(a.src_hash.as_deref()).ok_or_else(||
        AppError::Changed("Papierkorb-Aktion ohne persistierten Vollhash".into()))?;
    let kandidat=waehle_papierkorb_kandidat(
        items,
        &parent_key,
        &name,
        fenster,
        expected,
    )?;

    let item=kandidat.item;

    // Die Wiederherstellung legt die Datei unter dem Anzeigenamen ab. Fehlt
    // dort die Endung, muss der richtige Name aus dem Journal kommen.
    let anzeigename = item.name.to_string_lossy().to_string();
    let ersatzpfad = parent.join(&anzeigename);
    let ersatz_war_vorher_da = paths::extended(&ersatzpfad).exists();

    restore(item)?;

    if !ersatz_war_vorher_da && paths::extended(&ersatzpfad).exists() {
        let mut locked=fsmeta::LockedFile::open(&ersatzpfad)?;
        if locked.full_hash()? != expected {
            journal::mark_manual(conn,a.id,"recycle_post_restore_hash_mismatch_manual")?;
            return Err(AppError::Changed("Wiederhergestellter Papierkorb-Inhalt stimmt nicht mit dem Journal überein; die Datei bleibt isoliert am Stagingpfad und erfordert manuelle Klärung".into()));
        }
        locked.rename_to(original)?;
        return pruefe_wiederhergestellte(a, original);
    }
    Err(AppError::NotFound(format!(
        "\"{name}\" konnte nicht eindeutig im Papierkorb gefunden werden"
    )))
}

fn waehle_papierkorb_kandidat(
    items: Vec<crate::recycle::RecycleItem>,
    parent_key: &str,
    name: &str,
    fenster: Option<(i64,i64)>,
    expected_hash: &str,
) -> Result<crate::recycle::RecycleItem> {
    let ohne_endung = name
        .rfind('.')
        .filter(|i| *i > 0)
        .map(|i| &name[..i])
        .unwrap_or(name)
        .to_lowercase();
    let name_lower = name.to_lowercase();

    let mut kandidaten = Vec::new();
    for item in items {
        if paths::canonical_key(&item.item.original_parent) != parent_key {
            continue;
        }
        // Zeitfenster: nur Eintraege, die zu genau dieser Aktion passen koennen.
        if let Some((von, bis)) = fenster {
            if item.item.time_deleted < von || item.item.time_deleted > bis {
                continue;
            }
        }
        let n = item.item.name.to_string_lossy().to_lowercase();
        if n == name_lower || n == ohne_endung {
            kandidaten.push(item);
        }
    }

    // Schon mehrere passende Token/Metadaten sind widerspruechlich. Auch ein
    // Hash-Treffer unter mehreren Kandidaten darf nicht automatisch gewinnen.
    if kandidaten.len() > 1 {
        return Err(AppError::Invalid(format!(
            "Im Papierkorb passen mehrere Eintraege auf \"{name}\". FileTriage stellt \
             nichts wieder her, wenn nicht eindeutig ist, welche Datei gemeint ist. \
             Bitte im Papierkorb von Hand wiederherstellen."
        )));
    }

    if kandidaten.is_empty() {
        return Err(AppError::NotFound(format!(
            "\"{name}\" liegt nicht mehr im Papierkorb - er wurde vermutlich geleert"
        )));
    }

    let kandidat=kandidaten.pop().unwrap();
    if kandidat.full_hash.as_deref().is_some_and(|hash|hash!=expected_hash){
        return Err(AppError::Changed("Papierkorb-Kandidat stimmt bereits vor der Wiederherstellung nicht mit dem Journal überein; manuelle Klärung erforderlich".into()))
    }
    Ok(kandidat)
}

/// Prueft nach dem Zurueckholen, ob die Datei zur protokollierten passt.
///
/// Schlaegt das fehl, bleibt die Datei liegen - sie ist ja wiederhergestellt.
/// Gemeldet wird es trotzdem, damit niemand faelschlich annimmt, alles sei wie
/// zuvor.
fn pruefe_wiederhergestellte(a: &OpRecord, original: &Path) -> Result<()> {
    let jetzt = fsmeta::inspect(original)?;
    if jetzt.fingerprint.size != a.size {
        return Err(AppError::Changed(format!(
            "Die wiederhergestellte Datei hat {} Bytes, protokolliert waren {}. \
             Sie liegt jetzt unter {} - bitte pruefen.",
            jetzt.fingerprint.size,
            a.size,
            original.display()
        )));
    }
    let expected=a.temp_hash.as_deref().or(a.src_hash.as_deref()).ok_or_else(||
        AppError::Changed("Papierkorb-Aktion ohne persistierten Vollhash".into()))?;
    if crate::dup::full_hash(original)? != expected {
        return Err(AppError::Changed(format!("Die wiederhergestellte Datei unter {} stimmt nicht mit dem Journal überein",original.display())));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sitzungsweites Undo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SessionUndoPlan {
    pub session_id: String,
    pub total: usize,
    pub safe: usize,
    pub conflicted: usize,
    pub manual: usize,
    pub items: Vec<SessionUndoItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionUndoItem {
    pub action_id: i64,
    pub kind: String,
    pub original_name: String,
    pub src: String,
    pub dst: Option<String>,
    /// "safe" | "conflict" | "manual"
    pub verdict: String,
    pub reason: Option<String>,
}

/// Prueft ohne jede Veraenderung, was ein Session-Undo bewirken wuerde.
///
/// Das ist der Dry Run, der vor "Sitzung zurueckrollen" gezeigt wird.
pub fn plan_session_undo(conn: &Connection, session_id: &str) -> Result<SessionUndoPlan> {
    let aktionen = journal::session_actions(conn, session_id)?;
    let mut items = Vec::with_capacity(aktionen.len());
    let (mut safe, mut conflicted, mut manual) = (0, 0, 0);

    for a in &aktionen {
        let (verdict, reason) = beurteile(a);
        match verdict {
            "safe" => safe += 1,
            "conflict" => conflicted += 1,
            _ => manual += 1,
        }
        items.push(SessionUndoItem {
            action_id: a.id,
            kind: a.kind.clone(),
            original_name: a.original_name.clone(),
            src: a.src.clone(),
            dst: a.dst.clone(),
            verdict: verdict.to_string(),
            reason,
        });
    }

    Ok(SessionUndoPlan {
        session_id: session_id.to_string(),
        total: aktionen.len(),
        safe,
        conflicted,
        manual,
        items,
    })
}

/// Beurteilt eine einzelne Aktion, ohne etwas zu veraendern.
fn beurteile(a: &OpRecord) -> (&'static str, Option<String>) {
    if !a.undoable {
        return ("manual", Some("permanent_delete".into()));
    }
    match a.kind.as_str() {
        "move" | "rename" => {
            let Some(dst) = a.dst.as_deref() else {
                return ("manual", Some("missing_target".into()));
            };
            let dst_pfad = Path::new(dst);
            if !paths::extended(dst_pfad).exists() {
                return ("conflict", Some("missing_at_target".into()));
            }
            if paths::extended(Path::new(&a.src)).exists() {
                return (
                    "conflict",
                    Some("source_occupied".into()),
                );
            }
            match fsmeta::inspect(dst_pfad) {
                Ok(jetzt) => {
                    let passt = match a.dst_fingerprint() {
                        Some(fp) => jetzt.fingerprint.matches(&fp),
                        None => jetzt.fingerprint.size == a.size,
                    };
                    if passt {
                        ("safe", None)
                    } else {
                        ("conflict", Some("file_changed".into()))
                    }
                }
                Err(_) => ("conflict", Some("inspect_failed".into())),
            }
        }
        "delete_recycle" => {
            if paths::extended(Path::new(&a.src)).exists() {
                return (
                    "conflict",
                    Some("source_occupied".into()),
                );
            }
            // Ob der Papierkorbeintrag noch existiert, laesst sich hier
            // guenstig nicht sicher sagen; die Ausfuehrung prueft es erneut.
            ("safe", None)
        }
        _ => ("manual", Some("unsupported_action".into())),
    }
}

/// Rollt eine Sitzung zurueck - nur die als sicher beurteilten Aktionen.
///
/// Konflikte werden uebersprungen und gemeldet, niemals ueberschrieben.
pub fn undo_session(conn: &Connection, session_id: &str) -> Result<Vec<UndoResult>> {
    let plan = plan_session_undo(conn, session_id)?;
    let mut ergebnisse = Vec::new();
    for item in plan.items {
        if item.verdict != "safe" {
            ergebnisse.push(UndoResult {
                action_id: item.action_id,
                ok: false,
                restored_to: None,
                error_kind: Some("conflict".into()),
                message: item.reason.or_else(|| Some("skipped".into())),
            });
            continue;
        }
        ergebnisse.push(undo(conn, item.action_id));
    }
    Ok(ergebnisse)
}

pub fn sessions(conn: &Connection, limit: i64) -> Result<Vec<journal::SessionSummary>> {
    journal::sessions(conn, limit)
}

/// Haelt das Journal klein: alles jenseits der letzten `keep` Eintraege faellt
/// weg - unfertige Operationen bleiben aber immer erhalten, sonst ginge die
/// Grundlage der Wiederherstellung verloren.
pub fn prune(conn: &Connection, keep: i64) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM actions
         WHERE state IN ('completed', 'failed', 'rolled_back')
           AND id NOT IN (SELECT id FROM actions ORDER BY id DESC LIMIT ?1)",
        [keep.max(HISTORY_LIMIT)],
    )?;
    Ok(n)
}

/// Zeitpunkt der letzten Aktion - fuer die Anzeige "zuletzt aufgeraeumt".
pub fn last_activity(conn: &Connection) -> Result<Option<i64>> {
    let ts: Option<i64> = conn.query_row("SELECT max(ts) FROM actions", [], |r| r.get(0))?;
    Ok(ts.filter(|t| *t > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeleteMode;
    use crate::db::Db;
    use crate::fileops::{CollisionPolicy, OpStatus, delete_file, move_file, rename_file};
    use crate::testutil::TempDir;

    fn setup() -> (TempDir, Db, Connection) {
        let dir = TempDir::new("undo");
        let db = Db::open(dir.path().join("idx").join("i.db")).unwrap();
        let conn = db.conn().unwrap();
        (dir, db, conn)
    }

    fn papierkorb_item(parent:&Path,name:&str,hash:Option<&str>)->crate::recycle::RecycleItem{
        crate::recycle::RecycleItem{
            item:trash::TrashItem{
                id:format!("shell:{name}").into(),
                name:name.into(),
                original_parent:parent.to_path_buf(),
                time_deleted:100,
            },
            full_hash:hash.map(str::to_owned),
        }
    }

    fn mock_papierkorb_aktion(conn:&Connection,parent:&Path,name:&str,original:&str,expected:&str)->OpRecord{
        let src=parent.join(original);let stage=parent.join(name);
        conn.execute("INSERT INTO actions(session_id,ts,kind,state,src,temp,original_name,size,src_hash,temp_hash,undoable,trash_from,trash_to) VALUES('test',100,'delete_recycle','completed',?1,?2,?3,?4,?5,?5,1,90,110)",rusqlite::params![paths::display_path(&src),paths::display_path(&stage),original,7,expected]).unwrap();
        journal::by_id(conn,conn.last_insert_rowid()).unwrap()
    }

    #[test]
    fn recycle_produktpfad_ohne_stream_mit_korrektem_post_hash(){
        let (dir,_db,conn)=setup();let parent=dir.dir("restore-ok");let name=".filetriage-trash-1-0123456789abcdef0123456789abcdef-a.txt";let bytes=b"richtig";let hash=blake3::hash(bytes).to_hex().to_string();let a=mock_papierkorb_aktion(&conn,&parent,name,"a.txt",&hash);
        restore_from_trash_with(&conn,&a,&parent.join("a.txt"),vec![papierkorb_item(&parent,name,None)],|item|{std::fs::write(parent.join(item.name),bytes)?;Ok(())}).unwrap();
        assert_eq!(std::fs::read(parent.join("a.txt")).unwrap(),bytes);assert!(!parent.join(name).exists());
    }

    #[test]
    fn recycle_produktpfad_falscher_post_hash_bleibt_staging_und_manuell(){
        let (dir,_db,conn)=setup();let parent=dir.dir("restore-bad");let name=".filetriage-trash-2-0123456789abcdef0123456789abcdef-a.txt";let hash=blake3::hash(b"richtig").to_hex().to_string();let a=mock_papierkorb_aktion(&conn,&parent,name,"a.txt",&hash);
        let result=restore_from_trash_with(&conn,&a,&parent.join("a.txt"),vec![papierkorb_item(&parent,name,None)],|item|{std::fs::write(parent.join(item.name),b"falsch")?;Ok(())});
        assert!(result.is_err());assert!(!parent.join("a.txt").exists());assert_eq!(std::fs::read(parent.join(name)).unwrap(),b"falsch");let after=journal::by_id(&conn,a.id).unwrap();assert!(!after.undoable);assert_eq!(after.error.as_deref(),Some("recycle_post_restore_hash_mismatch_manual"));
    }

    #[test]
    fn recycle_produktpfad_falscher_pre_hash_restored_nicht(){
        let (dir,_db,conn)=setup();let parent=dir.dir("restore-prehash");let name=".filetriage-trash-3-0123456789abcdef0123456789abcdef-a.txt";let a=mock_papierkorb_aktion(&conn,&parent,name,"a.txt","richtig");let called=std::cell::Cell::new(false);
        let result=restore_from_trash_with(&conn,&a,&parent.join("a.txt"),vec![papierkorb_item(&parent,name,Some("falsch"))],|_|{called.set(true);Ok(())});assert!(result.is_err());assert!(!called.get());assert!(!parent.join("a.txt").exists());
    }

    #[test]
    fn recycle_produktpfad_mehrdeutig_restored_nicht(){
        let (dir,_db,conn)=setup();let parent=dir.dir("restore-ambiguous");let name=".filetriage-trash-4-0123456789abcdef0123456789abcdef-a.txt";let a=mock_papierkorb_aktion(&conn,&parent,name,"a.txt","hash");let called=std::cell::Cell::new(false);
        let result=restore_from_trash_with(&conn,&a,&parent.join("a.txt"),vec![papierkorb_item(&parent,name,None),papierkorb_item(&parent,name,None)],|_|{called.set(true);Ok(())});assert!(result.is_err());assert!(!called.get());
    }

    #[test]
    fn fremder_action_token_wird_nicht_als_papierkorb_kandidat_gewaehlt(){
        let dir=TempDir::new("trash-token");let parent=dir.path();
        let expected=".filetriage-trash-7-0123456789abcdef0123456789abcdef-report.txt";
        let foreign=".filetriage-trash-7-fedcba9876543210fedcba9876543210-report.txt";
        let selected=waehle_papierkorb_kandidat(
            vec![papierkorb_item(parent,foreign,Some("hash")),papierkorb_item(parent,expected,Some("hash"))],
            &paths::canonical_key(parent),expected,Some((90,110)),"hash",
        ).unwrap();
        assert_eq!(selected.item.name,expected);
    }

    #[test]
    fn mehrere_identische_papierkorb_kandidaten_bleiben_manuell(){
        let dir=TempDir::new("trash-ambiguous");let parent=dir.path();
        let name=".filetriage-trash-8-0123456789abcdef0123456789abcdef-a.bin";
        let result=waehle_papierkorb_kandidat(
            vec![papierkorb_item(parent,name,Some("hash")),papierkorb_item(parent,name,Some("hash"))],
            &paths::canonical_key(parent),name,Some((90,110)),"hash",
        );
        assert!(result.is_err());
    }

    #[test]
    fn falscher_vorab_hash_wird_vor_shell_restore_abgelehnt(){
        let dir=TempDir::new("trash-hash");let parent=dir.path();
        let name=".filetriage-trash-9-0123456789abcdef0123456789abcdef-a.bin";
        let result=waehle_papierkorb_kandidat(
            vec![papierkorb_item(parent,name,Some("falsch"))],
            &paths::canonical_key(parent),name,Some((90,110)),"richtig",
        );
        assert!(result.is_err());
    }

    #[test]
    fn fehlender_shell_stream_darf_nur_eindeutigen_token_fallback_nutzen(){
        let dir=TempDir::new("trash-no-stream");let parent=dir.path();
        let name=".filetriage-trash-10-0123456789abcdef0123456789abcdef-a.bin";
        let selected=waehle_papierkorb_kandidat(
            vec![papierkorb_item(parent,name,None)],
            &paths::canonical_key(parent),name,Some((90,110)),"hash",
        ).unwrap();
        assert_eq!(selected.item.name,name);
    }

    #[test]
    fn verschieben_laesst_sich_zuruecknehmen() {
        let (dir, _db, conn) = setup();
        let src = dir.file("a.txt", b"inhalt");
        let ziel = dir.dir("ziel");

        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Done);
        assert!(!src.exists());

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(u.ok, "{:?}", u.message);
        assert!(src.exists());
        assert_eq!(std::fs::read(&src).unwrap(), b"inhalt");
        assert!(!ziel.join("a.txt").exists());
    }

    #[test]
    fn undo_ueberschreibt_keine_neue_datei_am_alten_platz() {
        let (dir, _db, conn) = setup();
        let src = dir.file("a.txt", b"alt");
        let ziel = dir.dir("ziel");
        move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        std::fs::write(&src, b"neu").unwrap();

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(!u.ok);
        assert_eq!(std::fs::read(&src).unwrap(), b"neu");
        assert!(ziel.join("a.txt").exists(), "verschobene Datei bleibt liegen");
    }

    /// Bei einem abgeschlossenen Same-Volume-Move bleibt es logisch dieselbe
    /// Datei. Undo nimmt spaetere Bearbeitungen mit zurueck und erhaelt sie.
    #[test]
    fn undo_erhaelt_bearbeitung_eines_abgeschlossenen_same_volume_moves() {
        let (dir, _db, conn) = setup();
        let src = dir.file("bericht.txt", b"fassung eins");
        let ziel = dir.dir("ziel");
        let r = move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        assert_eq!(r.status, OpStatus::Done);

        // Der Nutzer arbeitet am Zielort weiter.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ziel.join("bericht.txt"), b"fassung zwei, viel besser").unwrap();

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(u.ok,"{u:?}");
        assert_eq!(std::fs::read(&src).unwrap(),b"fassung zwei, viel besser");
        assert!(!ziel.join("bericht.txt").exists());
    }

    #[test]
    fn umbenennen_laesst_sich_zuruecknehmen() {
        let (dir, _db, conn) = setup();
        let src = dir.file("alt.txt", b"x");
        let r = rename_file(&conn, &src, "neu.txt");
        assert_eq!(r.status, OpStatus::Done);

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(u.ok, "{:?}", u.message);
        assert!(src.exists());
    }

    #[test]
    fn endgueltig_geloeschtes_ist_nicht_wiederherstellbar() {
        let (dir, _db, conn) = setup();
        let src = dir.file("weg.txt", b"x");
        let r = delete_file(&conn, &src, DeleteMode::Permanent, None);
        assert_eq!(r.status, OpStatus::Done);
        assert!(!src.exists());

        assert!(last_undoable(&conn).unwrap().is_none());
        let u = undo(&conn, r.action_id.unwrap());
        assert!(!u.ok);
    }

    #[test]
    fn papierkorb_loeschung_kommt_zurueck() {
        let (dir, _db, conn) = setup();
        let src = dir.file("papierkorb.txt", b"inhalt");
        let r = delete_file(&conn, &src, DeleteMode::Recycle, None);
        assert_eq!(r.status, OpStatus::Done, "{:?}", r.message);
        assert!(!src.exists());

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(u.ok, "{:?}", u.message);
        assert!(src.exists());
        assert_eq!(std::fs::read(&src).unwrap(), b"inhalt");
    }

    #[test]
    fn papierkorb_undo_bricht_ab_wenn_der_platz_wieder_belegt_ist() {
        let (dir, _db, conn) = setup();
        let src = dir.file("belegt.txt", b"alt");
        delete_file(&conn, &src, DeleteMode::Recycle, None);
        std::fs::write(&src, b"etwas voellig anderes").unwrap();

        let u = undo_last(&conn).unwrap().unwrap();
        assert!(!u.ok, "es darf nichts überschrieben werden");
        assert_eq!(std::fs::read(&src).unwrap(), b"etwas voellig anderes");
    }

    #[test]
    fn stapel_wird_komplett_zurueckgenommen() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("ziel");
        for i in 0..5 {
            let src = dir.file(&format!("s{i}.txt"), b"x");
            move_file(&conn, &src, &ziel, CollisionPolicy::Ask, Some("stapel-1"));
        }
        let ergebnisse = undo_batch(&conn, "stapel-1").unwrap();
        assert_eq!(ergebnisse.len(), 5);
        assert!(ergebnisse.iter().all(|r| r.ok), "{ergebnisse:?}");
        for i in 0..5 {
            assert!(dir.path().join(format!("s{i}.txt")).exists());
        }
    }

    #[test]
    fn zweimal_zuruecknehmen_wird_abgelehnt() {
        let (dir, _db, conn) = setup();
        let src = dir.file("a.txt", b"x");
        let r = move_file(&conn, &src, &dir.dir("ziel"), CollisionPolicy::Ask, None);
        let id = r.action_id.unwrap();
        assert!(undo(&conn, id).ok);
        assert!(!undo(&conn, id).ok);
    }

    #[test]
    fn journal_haelt_mindestens_hundert_aktionen_vor() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("ziel");
        for i in 0..120 {
            let src = dir.file(&format!("f{i}.txt"), b"x");
            move_file(&conn, &src, &ziel, CollisionPolicy::Rename, None);
        }
        prune(&conn, HISTORY_LIMIT).unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert!(n >= HISTORY_LIMIT, "es müssen mindestens 100 Aktionen bleiben, waren {n}");
        assert_eq!(history(&conn, HISTORY_LIMIT).unwrap().len(), 100);
    }

    #[test]
    fn beschneiden_laesst_unfertige_operationen_stehen() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("ziel");
        // Eine unfertige Operation ganz am Anfang.
        let offen = crate::journal::plan(
            &conn,
            &crate::journal::Intent {
                kind: "move",
                src: "C:\\alt\\datei.txt",
                dst: Some("C:\\neu\\datei.txt"),
                original_name: "datei.txt",
                fingerprint: Default::default(),
                file_id: None,
                strong_file_id: None,
                src_hash: None,
                cross_volume: false,
                undoable: true,
                batch_id: None,
                session_id: "alt",
            },
        )
        .unwrap();
        for i in 0..150 {
            let src = dir.file(&format!("f{i}.txt"), b"x");
            move_file(&conn, &src, &ziel, CollisionPolicy::Rename, None);
        }
        prune(&conn, HISTORY_LIMIT).unwrap();
        assert!(
            crate::journal::by_id(&conn, offen).is_ok(),
            "eine unfertige Operation darf nie weggeraeumt werden"
        );
    }

    #[test]
    fn sitzungs_undo_zeigt_konflikte_vorher_an() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("ziel");
        let sitzung = crate::session_id();

        let a = dir.file("sauber.txt", b"x");
        move_file(&conn, &a, &ziel, CollisionPolicy::Ask, None);
        let b = dir.file("verändert.txt", b"x");
        move_file(&conn, &b, &ziel, CollisionPolicy::Ask, None);

        // Eine der beiden wird nach der Aktion angefasst.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ziel.join("verändert.txt"), b"jetzt anders").unwrap();

        let plan = plan_session_undo(&conn, &sitzung).unwrap();
        assert_eq!(plan.total, 2);
        assert_eq!(plan.safe, 1);
        assert_eq!(plan.conflicted, 1);

        let ergebnisse = undo_session(&conn, &sitzung).unwrap();
        assert_eq!(ergebnisse.iter().filter(|r| r.ok).count(), 1);
        assert!(dir.path().join("sauber.txt").exists());
        assert_eq!(
            std::fs::read(ziel.join("verändert.txt")).unwrap(),
            b"jetzt anders",
            "die veränderte Datei bleibt, wo sie ist"
        );
    }

    #[test]
    fn sitzungsuebersicht_zaehlt_richtig() {
        let (dir, _db, conn) = setup();
        let ziel = dir.dir("ziel");
        for i in 0..3 {
            let src = dir.file(&format!("m{i}.txt"), b"x");
            move_file(&conn, &src, &ziel, CollisionPolicy::Ask, None);
        }
        let r = dir.file("um.txt", b"x");
        rename_file(&conn, &r, "umbenannt.txt");

        let s = sessions(&conn, 10).unwrap();
        let aktuelle = s.iter().find(|x| x.session_id == crate::session_id()).unwrap();
        assert_eq!(aktuelle.moved, 3);
        assert_eq!(aktuelle.renamed, 1);
        assert_eq!(aktuelle.undoable, 4);
    }
}

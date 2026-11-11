//! Dauerhafter Clean/Unclean-Marker fuer den Datenbankstart.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const ACTIVE: &[u8] = b"active-v2\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSignature {
    id: Option<crate::fsmeta::FileId>,
    size: u64,
    modified: i64,
    modified_nanos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleSignature {
    main: Option<FileSignature>,
    wal: Option<FileSignature>,
    shm: Option<FileSignature>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CleanMarker { version: u32, state: String, clean_epoch:String, db: BundleSignature }

pub struct RunMarker { path: PathBuf, db_path: PathBuf }

impl RunMarker {
    pub fn begin(path: PathBuf, db_path: PathBuf) -> std::io::Result<(Self, bool)> {
        let previous_clean = std::fs::read(&path).ok()
            .and_then(|v| serde_json::from_slice::<CleanMarker>(&v).ok())
            .is_some_and(|m| m.version == 3 && m.state == "clean"
                && m.db == bundle_signature(&db_path)
                && read_clean_epoch_readonly(&db_path).as_deref()==Some(m.clean_epoch.as_str()));
        let recovery_required = crate::db::bundle_exists(&db_path) && !previous_clean;
        atomic_write(&path, ACTIVE)?;
        Ok((Self { path, db_path }, recovery_required))
    }

    pub fn mark_clean_if_journal_clear(
        &self,
        db: &crate::db::Db,
        _shutdown: &crate::journal::ShutdownPermit<'_>,
    ) -> crate::error::Result<bool> {
        let conn = db.conn()?;
        if !crate::journal::unfinished(&conn)?.is_empty() { return Ok(false); }
        let clean_epoch=new_clean_epoch()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        if let Err(error)=crate::db::set_setting(&conn,"db_clean_epoch",&clean_epoch){let _=conn.execute_batch("ROLLBACK");return Err(error)}
        conn.execute_batch("COMMIT")?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        drop(conn);
        let signature = bundle_signature(&self.db_path);
        if signature.main.as_ref().and_then(|s|s.id).is_none() || signature.wal.is_some() || signature.shm.is_some() { return Ok(false); }
        let marker = CleanMarker { version: 3, state: "clean".into(), clean_epoch, db: signature };
        atomic_write(&self.path, &serde_json::to_vec(&marker)?)?;
        // Only the incident snapshot attached to this exact Db startup can be
        // retired. A clean, unrelated database must never erase evidence from
        // another recovery incident.
        db.resolve_own_recovery_bundle();
        Ok(true)
    }
}

fn read_clean_epoch_readonly(path:&Path)->Option<String>{
    use rusqlite::OpenFlags;
    let conn=rusqlite::Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    conn.query_row("SELECT value FROM settings WHERE key='db_clean_epoch'",[],|row|row.get(0)).ok()
}

fn new_clean_epoch()->crate::error::Result<String>{
    let mut bytes=[0u8;32];
    #[cfg(windows)]{
        const SYSTEM_PREFERRED_RNG:u32=2;
        #[link(name="bcrypt")]unsafe extern "system"{fn BCryptGenRandom(h:isize,b:*mut u8,n:u32,f:u32)->i32;}
        let status=unsafe{BCryptGenRandom(0,bytes.as_mut_ptr(),bytes.len() as u32,SYSTEM_PREFERRED_RNG)};
        if status!=0{return Err(crate::error::AppError::Io(format!("Sicherer Zufall für DB-Generation fehlgeschlagen: NTSTATUS {status:#x}")))}
    }
    #[cfg(not(windows))]{
        bytes.copy_from_slice(blake3::hash(format!("{}:{}:{:?}",std::process::id(),crate::db::now_ts(),std::time::SystemTime::now()).as_bytes()).as_bytes());
    }
    Ok(bytes.iter().map(|b|format!("{b:02x}")).collect())
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || crate::fsmeta::is_reparse_point(&meta) { return None; }
    let fp = crate::fsmeta::Fingerprint::of(&meta);
    Some(FileSignature { id: crate::fsmeta::file_id(path), size: meta.len(), modified: fp.modified, modified_nanos: fp.modified_nanos })
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf { let mut p=path.as_os_str().to_os_string(); p.push(suffix); PathBuf::from(p) }
fn bundle_signature(path: &Path) -> BundleSignature {
    BundleSignature { main: file_signature(path), wal: file_signature(&sidecar(path,"-wal")), shm: file_signature(&sidecar(path,"-shm")) }
}

fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent=path.parent().unwrap_or_else(||Path::new(".")); std::fs::create_dir_all(parent)?;
    let temp=parent.join(format!(".run-state-{}-{}.tmp",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_nanos()).unwrap_or(0)));
    let mut file=std::fs::File::create(&temp)?; file.write_all(data)?; file.sync_all()?; drop(file); replace(&temp,path)
}

#[cfg(windows)]
fn move_file_ex(from:&Path,to:&Path,replace_existing:bool)->std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const REPLACE:u32=1; const WRITE_THROUGH:u32=8;
    #[link(name="kernel32")] unsafe extern "system" { fn MoveFileExW(a:*const u16,b:*const u16,f:u32)->i32; }
    let wide=|p:&Path|p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>(); let a=wide(from);let b=wide(to);
    let flags=WRITE_THROUGH|if replace_existing{REPLACE}else{0};
    if unsafe{MoveFileExW(a.as_ptr(),b.as_ptr(),flags)}==0{Err(std::io::Error::last_os_error())}else{Ok(())}
}
#[cfg(not(windows))]
fn move_file_ex(from:&Path,to:&Path,_:bool)->std::io::Result<()>{std::fs::rename(from,to)}
fn replace(from:&Path,to:&Path)->std::io::Result<()>{move_file_ex(from,to,true)}
pub(crate) fn publish_new(from:&Path,to:&Path)->std::io::Result<()>{move_file_ex(from,to,false)}

#[cfg(test)]
mod tests {
    use super::*;
    fn create_clean(d:&crate::testutil::TempDir)->(PathBuf,PathBuf){let dbp=d.path().join("index.db");let db=crate::db::Db::open(&dbp).unwrap();let mp=d.path().join("run-state");let(m,_)=RunMarker::begin(mp.clone(),dbp.clone()).unwrap();let g=crate::journal::MutationGate::new();let s=g.begin_shutdown().unwrap();assert!(m.mark_clean_if_journal_clear(&db,&s).unwrap());drop(db);(mp,dbp)}
    #[test]
    fn clean_marker_mit_spaeterem_wal_nimmt_keinen_fastpath(){
        let d=crate::testutil::TempDir::new("marker-wal");let dbp=d.path().join("index.db");let db=crate::db::Db::open(&dbp).unwrap();let mp=d.path().join("run-state");
        let(m,_)=RunMarker::begin(mp.clone(),dbp.clone()).unwrap();let g=crate::journal::MutationGate::new();let s=g.begin_shutdown().unwrap();assert!(m.mark_clean_if_journal_clear(&db,&s).unwrap());drop(db);
        std::fs::write(sidecar(&dbp,"-wal"),b"later").unwrap();let(_,recovery)=RunMarker::begin(mp,dbp).unwrap();assert!(recovery);
    }
    #[test]
    fn offenes_journal_verhindert_clean_marker(){
        let d=crate::testutil::TempDir::new("marker-open");let dbp=d.path().join("index.db");let db=crate::db::Db::open(&dbp).unwrap();db.conn().unwrap().execute("INSERT INTO actions(ts,kind,state,src,original_name,size)VALUES(1,'move','planned','a','a',1)",[]).unwrap();
        let(m,_)=RunMarker::begin(d.path().join("run-state"),dbp).unwrap();let g=crate::journal::MutationGate::new();let s=g.begin_shutdown().unwrap();assert!(!m.mark_clean_if_journal_clear(&db,&s).unwrap());
    }
    #[test]
    fn shutdown_wartet_und_weist_neue_mutation_ab(){
        let g=std::sync::Arc::new(crate::journal::MutationGate::new());let p=g.acquire().unwrap();let g2=g.clone();let t=std::thread::spawn(move||{let _s=g2.begin_shutdown().unwrap();g2.acquire().is_err()});drop(p);assert!(t.join().unwrap());
    }
    #[test]
    fn geaenderte_db_nimmt_keinen_fastpath(){let d=crate::testutil::TempDir::new("marker-changed");let(mp,dbp)=create_clean(&d);let db=crate::db::Db::open(&dbp).unwrap();crate::db::set_setting(&db.conn().unwrap(),"after-clean","1").unwrap();drop(db);let(_,r)=RunMarker::begin(mp,dbp).unwrap();assert!(r);}
    #[test]
    fn ersetzte_restore_kopie_nimmt_keinen_fastpath(){let d=crate::testutil::TempDir::new("marker-replaced");let(mp,dbp)=create_clean(&d);let copy=d.path().join("copy.db");std::fs::copy(&dbp,&copy).unwrap();std::fs::remove_file(&dbp).unwrap();std::fs::rename(copy,&dbp).unwrap();let(_,r)=RunMarker::begin(mp,dbp).unwrap();assert!(r);}
    #[cfg(windows)]
    #[test]
    fn in_place_restore_mit_erhaltener_id_und_mtime_erkennt_alte_generation(){
        use std::{io::{Read,Seek,SeekFrom,Write},os::windows::io::AsRawHandle};
        #[repr(C)]struct Ft{lo:u32,hi:u32}#[link(name="kernel32")]unsafe extern "system"{fn GetFileTime(h:isize,c:*mut Ft,a:*mut Ft,w:*mut Ft)->i32;fn SetFileTime(h:isize,c:*const Ft,a:*const Ft,w:*const Ft)->i32;}
        let d=crate::testutil::TempDir::new("marker-in-place-old-generation");let(mp,dbp)=create_clean(&d);let mut old=Vec::new();std::fs::File::open(&dbp).unwrap().read_to_end(&mut old).unwrap();
        let db=crate::db::Db::open(&dbp).unwrap();let(m,_)=RunMarker::begin(mp.clone(),dbp.clone()).unwrap();let gate=crate::journal::MutationGate::new();let permit=gate.begin_shutdown().unwrap();assert!(m.mark_clean_if_journal_clear(&db,&permit).unwrap());drop(db);
        let mut f=std::fs::OpenOptions::new().read(true).write(true).open(&dbp).unwrap();let mut c=Ft{lo:0,hi:0};let mut w=Ft{lo:0,hi:0};assert_ne!(unsafe{GetFileTime(f.as_raw_handle() as isize,&mut c,std::ptr::null_mut(),&mut w)},0);f.seek(SeekFrom::Start(0)).unwrap();f.write_all(&old).unwrap();f.set_len(old.len() as u64).unwrap();f.sync_all().unwrap();assert_ne!(unsafe{SetFileTime(f.as_raw_handle() as isize,&c,std::ptr::null(),&w)},0);drop(f);
        let(_,recovery)=RunMarker::begin(mp,dbp).unwrap();assert!(recovery);
    }
    #[test]
    fn nachtraeglich_offenes_journal_entwertet_clean_marker(){let d=crate::testutil::TempDir::new("marker-late-plan");let(mp,dbp)=create_clean(&d);let db=crate::db::Db::open(&dbp).unwrap();db.conn().unwrap().execute("INSERT INTO actions(ts,kind,state,src,original_name,size)VALUES(1,'move','planned','a','a',1)",[]).unwrap();drop(db);let(_,r)=RunMarker::begin(mp,dbp).unwrap();assert!(r);}
}

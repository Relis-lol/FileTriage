//! Prozessweiter Besitz genau der privaten FileTriage-Datenbank.

#[derive(Debug)]
pub enum AcquireError { AlreadyRunning, IdentityUnavailable, AccessDenied, Os(u32) }

fn mutex_name(path: &std::path::Path) -> Result<String,AcquireError> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    // Der primaere Schluessel darf sich durch die Erzeugung der DB nicht
    // aendern. Deshalb basiert er immer auf dem physischen Elternverzeichnis
    // und dem stabilen Dateinamen, nie auf der erst spaeter existierenden DB.
    // Fuer einen existierenden 8.3-Alias liefert canonicalize den langen Namen;
    // vor der Erzeugung bleibt der vom Aufrufer angegebene feste Name erhalten.
    let parent_path=absolute.parent().unwrap_or(&absolute);
    // Ein Pfad-Fallback waere hier fail-open: Falls der Identitaetsabruf bei A
    // scheitert und bei B spaeter gelingt, hielten beide verschiedene Locks.
    // Ohne stabile physische Identitaet darf die App deshalb nicht starten.
    let parent=crate::fsmeta::physical_file_id(parent_path)
        .map(|v|v.encode())
        .ok_or(AcquireError::IdentityUnavailable)?;
    let basename=std::fs::canonicalize(&absolute).ok()
        .and_then(|p|p.file_name().map(|v|v.to_string_lossy().to_lowercase()))
        .or_else(||absolute.file_name().map(|v|v.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let resource=format!("parent:{parent}\0{basename}");
    let hash = blake3::hash(resource.as_bytes()).to_hex();
    // Global ist absichtlich: dieselbe private DB darf auch aus einer zweiten
    // Windows-Session desselben Benutzers keinen parallelen Writer bekommen.
    // Der ressourcenbezogene Hash vermeidet die fruehere systemweite Sperre
    // unabhaengiger Benutzer/DBs. Named-object squatting bleibt lokaler DoS.
    Ok(format!("Global\\FileTriage.Database.{}", &hash[..32]))
}

#[cfg(windows)] pub struct InstanceGuard(isize);
#[cfg(windows)]
impl InstanceGuard {
    pub fn acquire(path: &std::path::Path) -> Result<Self,AcquireError> { Self::acquire_named(&mutex_name(path)?) }
    fn acquire_named(name:&str)->Result<Self,AcquireError>{
        use std::os::windows::ffi::OsStrExt; const ALREADY:u32=183; const DENIED:u32=5;
        #[link(name="kernel32")] unsafe extern "system"{fn CreateMutexW(a:*mut core::ffi::c_void,o:i32,n:*const u16)->isize;fn GetLastError()->u32;fn CloseHandle(h:isize)->i32;}
        let w=std::ffi::OsStr::new(name).encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
        let h=unsafe{CreateMutexW(std::ptr::null_mut(),1,w.as_ptr())};let code=unsafe{GetLastError()};
        if h==0{return Err(if code==DENIED{AcquireError::AccessDenied}else{AcquireError::Os(code)})}
        if code==ALREADY{unsafe{CloseHandle(h)};return Err(AcquireError::AlreadyRunning)}
        Ok(Self(h))
    }
}
#[cfg(windows)] impl Drop for InstanceGuard{fn drop(&mut self){#[link(name="kernel32")]unsafe extern "system"{fn CloseHandle(h:isize)->i32;}unsafe{CloseHandle(self.0)};}}

#[cfg(not(windows))] pub struct InstanceGuard;
#[cfg(not(windows))] impl InstanceGuard{pub fn acquire(_: &std::path::Path)->Result<Self,AcquireError>{Ok(Self)}}

pub fn show_error(error:&AcquireError){
    let message=match error{AcquireError::AlreadyRunning=>"FileTriage läuft bereits. Die vorhandene Instanz besitzt diese Datenbank.".to_string(),AcquireError::IdentityUnavailable=>"FileTriage kann die physische Identität des Datenbankordners nicht sicher bestimmen. Der Start wurde sicherheitshalber abgebrochen.".to_string(),AcquireError::AccessDenied=>"FileTriage kann den exklusiven Datenbankzugriff nicht prüfen (Zugriff verweigert). Der Start wurde sicherheitshalber abgebrochen.".to_string(),AcquireError::Os(code)=>format!("FileTriage kann den exklusiven Datenbankzugriff nicht prüfen (Windows-Fehler {code}). Der Start wurde sicherheitshalber abgebrochen.")};
    #[cfg(windows)] {use std::os::windows::ffi::OsStrExt;#[link(name="user32")]unsafe extern "system"{fn MessageBoxW(h:isize,t:*const u16,c:*const u16,k:u32)->i32;}let w=|s:&str|std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();let t=w(&message);let c=w("FileTriage");unsafe{MessageBoxW(0,t.as_ptr(),c.as_ptr(),0x10)};}
    #[cfg(not(windows))] eprintln!("{message}");
}

#[cfg(all(test,windows))]
mod tests{
 use super::*;
 fn unique(s:&str)->String{format!("Local\\FileTriage.Test.{s}.{}.{}",std::process::id(),crate::db::now_ts())}
 #[test]fn gleiche_db_blockiert_andere_db_nicht(){let d=crate::testutil::TempDir::new("instance");let a=mutex_name(&d.path().join("a.db")).unwrap();let b=mutex_name(&d.path().join("b.db")).unwrap();assert_ne!(a,b);let g=InstanceGuard::acquire_named(&a).unwrap();assert!(matches!(InstanceGuard::acquire_named(&a),Err(AcquireError::AlreadyRunning)));assert!(InstanceGuard::acquire_named(&b).is_ok());drop(g);assert!(InstanceGuard::acquire_named(&a).is_ok());}
 #[test]fn existierende_gleiche_db_blockiert_zweite_instanz(){let d=crate::testutil::TempDir::new("instance-existing");let p=d.file("index.db",b"db");let g=InstanceGuard::acquire(&p).unwrap();assert!(matches!(InstanceGuard::acquire(&p),Err(AcquireError::AlreadyRunning)));drop(g);}
 #[test]fn helper_haelt_mutex(){let Ok(name)=std::env::var("FILETRIAGE_TEST_MUTEX")else{return};let ready=std::env::var("FILETRIAGE_TEST_MUTEX_READY").unwrap();let _g=InstanceGuard::acquire_named(&name).unwrap();std::fs::write(ready,b"ready").unwrap();loop{std::thread::sleep(std::time::Duration::from_secs(1));}}
 #[test]fn crash_gibt_besitz_frei(){let d=crate::testutil::TempDir::new("instance-crash");let ready=d.path().join("ready");let name=unique("crash");let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","instance::tests::helper_haelt_mutex"]).env("FILETRIAGE_TEST_MUTEX",&name).env("FILETRIAGE_TEST_MUTEX_READY",&ready).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();for _ in 0..100{if ready.exists(){break}std::thread::sleep(std::time::Duration::from_millis(20));}assert!(ready.exists());assert!(matches!(InstanceGuard::acquire_named(&name),Err(AcquireError::AlreadyRunning)));child.kill().unwrap();child.wait().unwrap();assert!(InstanceGuard::acquire_named(&name).is_ok());}
 #[test]fn api_fehler_werden_unterschieden(){fn classify(handle:isize,code:u32)->Result<(),AcquireError>{if handle!=0{Ok(())}else if code==5{Err(AcquireError::AccessDenied)}else{Err(AcquireError::Os(code))}}assert!(matches!(classify(0,5),Err(AcquireError::AccessDenied)));assert!(matches!(classify(0,87),Err(AcquireError::Os(87))));let _=unique("classification");}
 #[test]fn junction_alias_teilt_denselben_mutex(){let d=crate::testutil::TempDir::new("instance-junction");let real=d.dir("real");let alias=d.path().join("alias");if !crate::testutil::make_dir_symlink(&alias,&real){return}assert_eq!(mutex_name(&real.join("index.db")).unwrap(),mutex_name(&alias.join("index.db")).unwrap());std::fs::write(real.join("index.db"),b"db").unwrap();assert_eq!(mutex_name(&real.join("index.db")).unwrap(),mutex_name(&alias.join("index.db")).unwrap());}
 #[test]fn kurzer_8_3_alias_teilt_falls_verfuegbar_denselben_mutex(){use std::os::windows::ffi::{OsStrExt,OsStringExt};#[link(name="kernel32")]unsafe extern "system"{fn GetShortPathNameW(long:*const u16,short:*mut u16,n:u32)->u32;}let d=crate::testutil::TempDir::new("instance-short-alias");let p=d.file("database-with-long-name.db",b"db");let wide=p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();let mut out=vec![0u16;32768];let n=unsafe{GetShortPathNameW(wide.as_ptr(),out.as_mut_ptr(),out.len() as u32)};if n==0{return}let short=std::path::PathBuf::from(std::ffi::OsString::from_wide(&out[..n as usize]));assert_eq!(mutex_name(&p).unwrap(),mutex_name(&short).unwrap());}
 #[test]
 fn primaerer_mutex_bleibt_bei_db_erzeugung_identisch(){
     let d=crate::testutil::TempDir::new("instance-create-transition");
     let path=d.path().join("index.db");
     let before=mutex_name(&path).unwrap();
     let guard=InstanceGuard::acquire_named(&before).unwrap();
     std::fs::write(&path,b"db").unwrap();
     let after=mutex_name(&path).unwrap();
     assert_eq!(before,after,"die Lock-Identitaet darf bei DB-Erzeugung nicht wechseln");
     assert!(matches!(InstanceGuard::acquire(&path),Err(AcquireError::AlreadyRunning)));
     drop(guard);
 }
}

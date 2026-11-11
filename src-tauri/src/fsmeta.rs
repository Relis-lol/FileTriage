//! Windows-Dateimerkmale, auf denen die Sicherheitsentscheidungen beruhen.
//!
//! Drei Dinge werden hier beantwortet, bevor irgendetwas angefasst wird:
//!
//! 1. **Ist das ueberhaupt eine echte Datei?** Reparse Points (Symlinks,
//!    Junctions, Cloud-Platzhalter) sehen aus wie Dateien, verhalten sich aber
//!    anders. FileTriage fasst sie in Version 0.1 nicht an.
//! 2. **Liegt der Inhalt lokal vor?** Ein OneDrive-Platzhalter wuerde beim
//!    Lesen Gigabytes aus der Cloud holen. Das passiert nie ungefragt.
//! 3. **Ist es noch dieselbe Datei wie bei der Pruefung?** Dafuer gibt es den
//!    [`Fingerprint`] - die Antwort auf Time-of-check-to-time-of-use.

use std::fs::Metadata;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths;

// Windows-Dateiattribute. Bewusst hier als Konstanten statt als Abhaengigkeit
// zur windows-Kiste - es sind genau diese sieben Bits, die uns interessieren.
pub const ATTR_READONLY: u32 = 0x0000_0001;
pub const ATTR_HIDDEN: u32 = 0x0000_0002;
pub const ATTR_SYSTEM: u32 = 0x0000_0004;
pub const ATTR_SPARSE: u32 = 0x0000_0200;
pub const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
pub const ATTR_COMPRESSED: u32 = 0x0000_0800;
pub const ATTR_OFFLINE: u32 = 0x0000_1000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

#[cfg(windows)]
pub fn attributes(meta: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
pub fn attributes(_meta: &Metadata) -> u32 {
    0
}

/// Symlink, Junction, Mount Point oder Cloud-Platzhalter mit Reparse-Daten.
pub fn is_reparse_point(meta: &Metadata) -> bool {
    attributes(meta) & ATTR_REPARSE_POINT != 0 || meta.file_type().is_symlink()
}

/// Inhalt liegt nicht lokal vor - Lesen wuerde einen Download ausloesen.
///
/// Betrifft OneDrive "Files on Demand" und vergleichbare Cloud-Anbieter.
pub fn is_cloud_placeholder(meta: &Metadata) -> bool {
    let a = attributes(meta);
    a & (ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
        // OFFLINE allein reicht nicht: das Attribut setzen auch Bandarchive
        // und aeltere Sicherungsprogramme. Zusammen mit einem Reparse Point
        // ist es aber der klassische Platzhalter.
        || (a & ATTR_OFFLINE != 0 && a & ATTR_REPARSE_POINT != 0)
}

pub fn is_readonly(meta: &Metadata) -> bool {
    attributes(meta) & ATTR_READONLY != 0 || meta.permissions().readonly()
}

pub fn is_hidden_or_system(meta: &Metadata) -> bool {
    attributes(meta) & (ATTR_HIDDEN | ATTR_SYSTEM) != 0
}

pub fn is_sparse(meta: &Metadata) -> bool {
    attributes(meta) & ATTR_SPARSE != 0
}

/// Merkmale einer Datei zum Zeitpunkt der Pruefung.
///
/// Windows bietet mit `nFileIndex` zwar eine echte Dateikennung, sie ist ueber
/// die stabile Rust-Standardbibliothek aber nicht erreichbar (`file_index` ist
/// unstable). Deshalb hier die Kombination aus Groesse und den beiden
/// Zeitstempeln: sie erkennt zuverlaessig, ob eine Datei zwischen Pruefung und
/// Ausfuehrung ausgetauscht oder veraendert wurde. Sie kann nicht erkennen, ob
/// zwei Pfade auf dieselben Daten zeigen (Hardlink) - das ist dokumentiert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Fingerprint {
    pub size: i64,
    /// Unix-Sekunden.
    pub modified: i64,
    pub created: i64,
    /// Nanosekundenanteil der Aenderungszeit, soweit das Dateisystem ihn liefert.
    pub modified_nanos: u32,
}

impl Fingerprint {
    pub fn of(meta: &Metadata) -> Fingerprint {
        let (modified, modified_nanos) = split_time(meta.modified().ok());
        Fingerprint {
            size: meta.len() as i64,
            modified,
            modified_nanos,
            created: split_time(meta.created().ok()).0,
        }
    }

    /// Wahr, wenn es sich mit hoher Sicherheit noch um dieselbe Datei handelt.
    pub fn matches(&self, other: &Fingerprint) -> bool {
        // Der Erstellungszeitpunkt wird bewusst weich behandelt: Beim
        // Verschieben innerhalb eines Volumes bleibt er erhalten, beim Kopieren
        // ueber Volumegrenzen setzt Windows ihn neu.
        self.size == other.size
            && self.modified == other.modified
            && self.modified_nanos == other.modified_nanos
    }
}

fn split_time(t: Option<std::time::SystemTime>) -> (i64, u32) {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
        .unwrap_or((0, 0))
}

/// Wie eine Datei fuer FileTriage einzustufen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    /// Normale Datei mit lokalem Inhalt.
    Regular,
    /// Symlink, Junction oder Mount Point - wird nicht angefasst.
    Reparse,
    /// Cloud-Platzhalter - Inhalt liegt nicht lokal.
    CloudPlaceholder,
    /// Verzeichnis.
    Directory,
    /// Weder Datei noch Verzeichnis (Geraet, Pipe, ...).
    Other,
}

pub fn classify(meta: &Metadata) -> FileClass {
    if is_reparse_point(meta) {
        // Ein Cloud-Platzhalter ist technisch ebenfalls ein Reparse Point.
        // Die genauere Aussage gewinnt, weil sie zu einer anderen Empfehlung
        // fuehrt (herunterladen statt "nicht anfassen").
        return if is_cloud_placeholder(meta) {
            FileClass::CloudPlaceholder
        } else {
            FileClass::Reparse
        };
    }
    if is_cloud_placeholder(meta) {
        return FileClass::CloudPlaceholder;
    }
    if meta.is_dir() {
        return FileClass::Directory;
    }
    if meta.is_file() {
        return FileClass::Regular;
    }
    FileClass::Other
}

/// Echte Dateikennung von NTFS.
///
/// `volume` + `index` identifizieren die Daten eindeutig - unabhaengig vom
/// Pfad. Damit laesst sich beweisen, dass eine Datei zwischen Pruefung und
/// Ausfuehrung *nicht* ausgetauscht wurde, und dass zwei Pfade auf dieselben
/// Daten zeigen (Hardlink).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    pub volume: u32,
    pub index: u64,
    /// Anzahl der Verzeichniseintraege, die auf diese Daten zeigen.
    /// Groesser als 1 bedeutet Hardlink.
    pub links: u32,
}

/// Starke, ueber einen offenen Windows-Handle gelesene Objektidentitaet.
/// Die Creation-Time trennt eine spaeter wiederverwendete File-ID von dem
/// Objekt, das FileTriage urspruenglich gesehen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrongFileId {
    pub volume: u64,
    pub id: [u8; 16],
    pub created: u64,
}

impl StrongFileId {
    pub fn encode(&self) -> String {
        format!("{:016x}:{}:{:016x}", self.volume, self.id.iter().map(|b| format!("{b:02x}")).collect::<String>(), self.created)
    }

    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split(':');
        let volume = u64::from_str_radix(parts.next()?, 16).ok()?;
        let raw = parts.next()?;
        let created = u64::from_str_radix(parts.next()?, 16).ok()?;
        if parts.next().is_some() || raw.len() != 32 { return None; }
        let mut id = [0u8; 16];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self { volume, id, created })
    }
}

impl FileId {
    pub fn is_hardlinked(&self) -> bool {
        self.links > 1
    }
}

#[cfg(windows)]
#[repr(C)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: [u32; 2],
    last_access_time: [u32; 2],
    last_write_time: [u32; 2],
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

/// Liest die Dateikennung.
///
/// Geoeffnet wird nur mit `FILE_READ_ATTRIBUTES` und
/// `FILE_FLAG_OPEN_REPARSE_POINT`: Das sperrt die Datei fuer niemanden,
/// folgt keiner Verknuepfung und laedt insbesondere **keinen**
/// Cloud-Platzhalter herunter.
#[cfg(windows)]
pub fn file_id(path: &Path) -> Option<FileId> {
    use std::os::windows::ffi::OsStrExt;

    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const SHARE_ALL: u32 = 0x0000_0007; // READ | WRITE | DELETE
    const OPEN_EXISTING: u32 = 3;
    const FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const INVALID_HANDLE: isize = -1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *mut core::ffi::c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: isize,
        ) -> isize;
        fn GetFileInformationByHandle(hFile: isize, lpInfo: *mut ByHandleFileInformation) -> i32;
        fn CloseHandle(hObject: isize) -> i32;
    }

    let mut wide: Vec<u16> = paths::extended(path).as_os_str().encode_wide().collect();
    wide.push(0);

    // SAFETY: `wide` ist nullterminiert und lebt bis nach dem Aufruf. Der
    // zurueckgegebene Handle wird auf jedem Pfad genau einmal geschlossen.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FLAG_BACKUP_SEMANTICS | FLAG_OPEN_REPARSE_POINT,
            0,
        )
    };
    if handle == INVALID_HANDLE || handle == 0 {
        return None;
    }

    let mut info = ByHandleFileInformation {
        file_attributes: 0,
        creation_time: [0; 2],
        last_access_time: [0; 2],
        last_write_time: [0; 2],
        volume_serial_number: 0,
        file_size_high: 0,
        file_size_low: 0,
        number_of_links: 0,
        file_index_high: 0,
        file_index_low: 0,
    };
    // SAFETY: gueltiger Handle, `info` ist korrekt ausgerichtet und gross genug.
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    // SAFETY: derselbe Handle, genau einmal geschlossen.
    unsafe { CloseHandle(handle) };

    if ok == 0 {
        return None;
    }
    Some(FileId {
        volume: info.volume_serial_number,
        index: ((info.file_index_high as u64) << 32) | info.file_index_low as u64,
        links: info.number_of_links,
    })
}

#[cfg(windows)]
pub fn strong_file_id(path: &Path) -> Option<StrongFileId> {
    open_identity_handle(path, true).and_then(|handle| {
        let result = strong_file_id_from_handle(handle);
        unsafe { CloseHandleForIdentity(handle) };
        result
    })
}

/// Identitaet der physischen Ressource. Reparse Points werden hier bewusst
/// verfolgt; diese Variante dient der Single-Instance-Sperre, nicht der
/// Klassifizierung von Nutzerdaten.
#[cfg(windows)]
pub fn physical_file_id(path: &Path) -> Option<StrongFileId> {
    open_identity_handle(path, false).and_then(|handle| {
        let result = strong_file_id_from_handle(handle);
        unsafe { CloseHandleForIdentity(handle) };
        result
    })
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" { #[link_name = "CloseHandle"] fn CloseHandleForIdentity(handle: isize) -> i32; }

#[cfg(windows)]
fn open_identity_handle(path: &Path, open_reparse: bool) -> Option<isize> {
    use std::os::windows::ffi::OsStrExt;
    const FILE_READ_ATTRIBUTES:u32=0x80; const SHARE_ALL:u32=7; const OPEN_EXISTING:u32=3;
    const BACKUP:u32=0x0200_0000; const OPEN_REPARSE:u32=0x0020_0000; const INVALID:isize=-1;
    #[link(name="kernel32")] unsafe extern "system" { fn CreateFileW(n:*const u16,a:u32,s:u32,x:*mut core::ffi::c_void,d:u32,f:u32,t:isize)->isize; }
    let wide=paths::extended(path).as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
    let flags=BACKUP|if open_reparse{OPEN_REPARSE}else{0};
    let handle=unsafe{CreateFileW(wide.as_ptr(),FILE_READ_ATTRIBUTES,SHARE_ALL,std::ptr::null_mut(),OPEN_EXISTING,flags,0)};
    (handle!=INVALID&&handle!=0).then_some(handle)
}

#[cfg(not(windows))]
pub fn strong_file_id(_path: &Path) -> Option<StrongFileId> { None }
#[cfg(not(windows))]
pub fn physical_file_id(_path: &Path) -> Option<StrongFileId> { None }

#[cfg(not(windows))]
pub fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// Exklusiv fuer Schreiben/Loeschen geoeffnete Datei. Identitaet, optionaler
/// Inhaltshash und die destruktive Operation benutzen denselben Kernel-Handle.
#[cfg(windows)]
pub struct LockedFile {
    file: std::fs::File,
    id: FileId,
    strong: Option<StrongFileId>,
}

#[cfg(all(test,windows))]
thread_local! {
    static FORCE_RENAME_CROSS_VOLUME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FORCE_COPY_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test,windows))]
pub fn force_next_locked_rename_cross_volume() { FORCE_RENAME_CROSS_VOLUME.with(|v|v.set(true)); }
#[cfg(all(test,windows))]
pub fn force_next_locked_copy_failure() { FORCE_COPY_FAILURE.with(|v|v.set(true)); }

#[cfg(windows)]
impl LockedFile {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        const GENERIC_READ:u32=0x8000_0000; const DELETE:u32=0x0001_0000;
        const FILE_READ_ATTRIBUTES:u32=0x80; const SHARE_READ:u32=1; const OPEN_EXISTING:u32=3;
        const OPEN_REPARSE:u32=0x0020_0000; const INVALID:isize=-1;
        #[link(name="kernel32")] unsafe extern "system"{fn CreateFileW(n:*const u16,a:u32,s:u32,x:*mut core::ffi::c_void,d:u32,f:u32,t:isize)->isize;}
        let wide=paths::extended(path).as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
        let handle=unsafe{CreateFileW(wide.as_ptr(),GENERIC_READ|DELETE|FILE_READ_ATTRIBUTES,SHARE_READ,std::ptr::null_mut(),OPEN_EXISTING,OPEN_REPARSE,0)};
        if handle==INVALID||handle==0{return Err(AppError::from_io(&std::io::Error::last_os_error(),&format!("Datei {} exklusiv öffnen",path.display())))}
        let file=unsafe{std::fs::File::from_raw_handle(handle as *mut core::ffi::c_void)};
        let id=file_id_from_handle(handle).ok_or_else(||AppError::Changed(format!("Identität von {} ist nicht lesbar",path.display())))?;
        let strong=strong_file_id_from_handle(handle);
        Ok(Self{file,id,strong})
    }
    pub fn id(&self)->FileId{self.id}
    pub fn strong_id(&self)->Option<StrongFileId>{self.strong}
    pub fn fingerprint(&self)->Result<Fingerprint>{Ok(Fingerprint::of(&self.file.metadata()?))}
    pub fn full_hash(&mut self)->Result<String>{
        use std::io::{Read,Seek,SeekFrom}; self.file.seek(SeekFrom::Start(0))?;let mut h=blake3::Hasher::new();let mut b=[0u8;1024*1024];loop{let n=self.file.read(&mut b)?;if n==0{break}h.update(&b[..n]);}self.file.seek(SeekFrom::Start(0))?;Ok(h.finalize().to_hex().to_string())
    }
    pub fn delete(self)->Result<()> {
        #[repr(C)]struct Disposition{delete_file:i32}
        #[link(name="kernel32")]unsafe extern "system"{fn SetFileInformationByHandle(h:isize,c:u32,p:*const core::ffi::c_void,n:u32)->i32;}
        use std::os::windows::io::AsRawHandle;let info=Disposition{delete_file:1};
        let ok=unsafe{SetFileInformationByHandle(self.file.as_raw_handle() as isize,4,&info as *const _ as *const _,std::mem::size_of::<Disposition>() as u32)};
        if ok==0{return Err(AppError::from_io(&std::io::Error::last_os_error(),"Datei über verifizierten Handle entfernen"))}drop(self);Ok(())
    }
    pub fn rename_to(&mut self,target:&Path)->Result<()> {
        #[cfg(test)]
        if FORCE_RENAME_CROSS_VOLUME.with(|value| value.replace(false)) {
            return Err(AppError::from_io(&std::io::Error::from_raw_os_error(17),"erzwungener Volumewechsel"));
        }
        #[repr(C)]struct RenameInfo{flags:u32,root:isize,name_len:u32,name:[u16;1]}
        #[link(name="kernel32")]unsafe extern "system"{fn SetFileInformationByHandle(h:isize,c:u32,p:*const core::ffi::c_void,n:u32)->i32;}
        use std::os::windows::{ffi::OsStrExt,io::AsRawHandle};
        let name=target.as_os_str().encode_wide().collect::<Vec<_>>();let off=std::mem::offset_of!(RenameInfo,name);let bytes=off+(name.len()+1)*2;let words=bytes.div_ceil(std::mem::size_of::<usize>());let mut storage=vec![0usize;words];
        let info=storage.as_mut_ptr() as *mut RenameInfo;unsafe{(*info).flags=0;(*info).root=0;(*info).name_len=(name.len()*2) as u32;std::ptr::copy_nonoverlapping(name.as_ptr(),(info as *mut u8).add(off) as *mut u16,name.len());}
        let ok=unsafe{SetFileInformationByHandle(self.file.as_raw_handle() as isize,3,info as *const _,bytes as u32)};
        if ok==0{return Err(AppError::from_io(&std::io::Error::last_os_error(),&format!("Datei über verifizierten Handle nach {} umbenennen",target.display())))}Ok(())
    }

    /// Kopiert aus diesem gesperrten Source-Handle in eine exklusiv neu
    /// angelegte Temp-Datei und gibt deren weiterhin offenen Handle zurueck.
    pub fn copy_to_new(&mut self,target:&Path)->Result<(LockedFile,u64)> {
        #[cfg(test)]
        if FORCE_COPY_FAILURE.with(|value| value.replace(false)) {
            return Err(AppError::Io("erzwungener Kopierfehler".into()));
        }
        use std::io::{Read,Seek,SeekFrom,Write};
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        const GENERIC_READ:u32=0x8000_0000; const GENERIC_WRITE:u32=0x4000_0000; const DELETE:u32=0x0001_0000;
        const ATTR:u32=0x80; const SHARE_READ:u32=1; const CREATE_NEW:u32=1; const INVALID:isize=-1;
        #[link(name="kernel32")] unsafe extern "system" { fn CreateFileW(n:*const u16,a:u32,s:u32,x:*mut core::ffi::c_void,d:u32,f:u32,t:isize)->isize; }
        let wide=paths::extended(target).as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<_>>();
        let handle=unsafe{CreateFileW(wide.as_ptr(),GENERIC_READ|GENERIC_WRITE|DELETE|ATTR,SHARE_READ,std::ptr::null_mut(),CREATE_NEW,0,0)};
        if handle==INVALID||handle==0{return Err(AppError::from_io(&std::io::Error::last_os_error(),&format!("Zwischendatei {} exklusiv anlegen",target.display())))}
        let file=unsafe{std::fs::File::from_raw_handle(handle as *mut core::ffi::c_void)};
        let id=file_id_from_handle(handle).ok_or_else(||AppError::Changed("Neue Zwischendatei hat keine stabile Identität".into()))?;
        let strong=strong_file_id_from_handle(handle);
        let mut target_locked=LockedFile{file,id,strong};
        self.file.seek(SeekFrom::Start(0))?; target_locked.file.seek(SeekFrom::Start(0))?;
        let bytes=std::io::copy(&mut self.file,&mut target_locked.file)?;
        target_locked.file.flush()?; target_locked.file.sync_all()?;
        self.file.seek(SeekFrom::Start(0))?; target_locked.file.seek(SeekFrom::Start(0))?;
        Ok((target_locked,bytes))
    }
}

#[cfg(windows)]
fn file_id_from_handle(handle:isize)->Option<FileId>{
    #[link(name="kernel32")]unsafe extern "system"{fn GetFileInformationByHandle(h:isize,p:*mut ByHandleFileInformation)->i32;}
    let mut i=ByHandleFileInformation{file_attributes:0,creation_time:[0;2],last_access_time:[0;2],last_write_time:[0;2],volume_serial_number:0,file_size_high:0,file_size_low:0,number_of_links:0,file_index_high:0,file_index_low:0};
    if unsafe{GetFileInformationByHandle(handle,&mut i)}==0{return None}Some(FileId{volume:i.volume_serial_number,index:((i.file_index_high as u64)<<32)|i.file_index_low as u64,links:i.number_of_links})
}

#[cfg(windows)]
fn strong_file_id_from_handle(handle:isize)->Option<StrongFileId>{
    #[repr(C)] struct FileIdInfo { volume:u64, id:[u8;16] }
    #[link(name="kernel32")] unsafe extern "system" { fn GetFileInformationByHandleEx(h:isize,c:u32,p:*mut core::ffi::c_void,n:u32)->i32; }
    let mut info=FileIdInfo{volume:0,id:[0;16]};
    // FileIdInfo = FILE_INFO_BY_HANDLE_CLASS 18.
    if unsafe{GetFileInformationByHandleEx(handle,18,&mut info as *mut _ as *mut _,std::mem::size_of::<FileIdInfo>() as u32)}==0{return None}
    let mut legacy=ByHandleFileInformation{file_attributes:0,creation_time:[0;2],last_access_time:[0;2],last_write_time:[0;2],volume_serial_number:0,file_size_high:0,file_size_low:0,number_of_links:0,file_index_high:0,file_index_low:0};
    #[link(name="kernel32")] unsafe extern "system" { fn GetFileInformationByHandle(h:isize,p:*mut ByHandleFileInformation)->i32; }
    if unsafe{GetFileInformationByHandle(handle,&mut legacy)}==0{return None}
    let created=((legacy.creation_time[1] as u64)<<32)|legacy.creation_time[0] as u64;
    Some(StrongFileId{volume:info.volume,id:info.id,created})
}

#[cfg(not(windows))]
pub struct LockedFile;
#[cfg(not(windows))]
impl LockedFile { pub fn open(path:&Path)->Result<Self>{Err(AppError::Invalid(format!("Sichere Handle-Mutation ist auf diesem System nicht verfügbar: {}",path.display())))} }

/// Namen der alternativen Datenstroeme einer Datei (ohne den Hauptstrom).
///
/// NTFS erlaubt es, an eine Datei weitere Datenstroeme zu haengen
/// (`bericht.docx:Zone.Identifier`). Sie tragen echte Nutzdaten, sind im
/// Explorer unsichtbar und gehen bei einem gewoehnlichen Kopiervorgang
/// verloren. Bevor FileTriage eine Datei ueber eine Volumegrenze kopiert, muss
/// es wissen, ob dabei etwas verschwinden wuerde.
///
/// Leerer Vektor heisst "keine zusaetzlichen Stroeme", `None` heisst "nicht
/// feststellbar" - dann wird im Zweifel gewarnt.
#[cfg(windows)]
pub fn alternate_streams(path: &Path) -> Option<Vec<String>> {
    use std::os::windows::ffi::OsStrExt;

    const FIND_STREAM_INFO_STANDARD: u32 = 0;
    const INVALID_HANDLE: isize = -1;
    const ERROR_HANDLE_EOF: u32 = 38;

    #[repr(C)]
    struct Win32FindStreamData {
        stream_size: i64,
        // MAX_PATH + 36 Zeichen, so verlangt es die API.
        stream_name: [u16; 296],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FindFirstStreamW(
            lpFileName: *const u16,
            InfoLevel: u32,
            lpFindStreamData: *mut core::ffi::c_void,
            dwFlags: u32,
        ) -> isize;
        fn FindNextStreamW(hFindStream: isize, lpFindStreamData: *mut core::ffi::c_void) -> i32;
        fn FindClose(hFindFile: isize) -> i32;
        fn GetLastError() -> u32;
    }

    let mut wide: Vec<u16> = paths::extended(path).as_os_str().encode_wide().collect();
    wide.push(0);

    let mut daten = Win32FindStreamData { stream_size: 0, stream_name: [0u16; 296] };
    // SAFETY: `wide` ist nullterminiert; `daten` hat das von der API erwartete
    // Layout und lebt ueber alle Aufrufe hinweg. Der Handle wird genau einmal
    // geschlossen.
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FIND_STREAM_INFO_STANDARD,
            &mut daten as *mut _ as *mut core::ffi::c_void,
            0,
        )
    };
    if handle == INVALID_HANDLE {
        return None;
    }

    let mut gefunden = Vec::new();
    loop {
        let name = {
            let len = daten.stream_name.iter().position(|c| *c == 0).unwrap_or(0);
            String::from_utf16_lossy(&daten.stream_name[..len])
        };
        // Der Hauptstrom heisst "::$DATA" und zaehlt nicht.
        if !name.is_empty() && name != "::$DATA" {
            gefunden.push(name);
        }
        // SAFETY: gueltiger Handle aus FindFirstStreamW.
        let weiter =
            unsafe { FindNextStreamW(handle, &mut daten as *mut _ as *mut core::ffi::c_void) };
        if weiter == 0 {
            // SAFETY: Fehlercode gehoert zum letzten Aufruf in diesem Thread.
            let fehler = unsafe { GetLastError() };
            // SAFETY: derselbe Handle, genau einmal geschlossen.
            unsafe { FindClose(handle) };
            return if fehler == ERROR_HANDLE_EOF { Some(gefunden) } else { None };
        }
    }
}

#[cfg(not(windows))]
pub fn alternate_streams(_path: &Path) -> Option<Vec<String>> {
    Some(Vec::new())
}

/// Zustand einer Datei, wie ihn die sicherheitskritischen Pfade brauchen.
#[derive(Debug, Clone)]
pub struct FileState {
    pub class: FileClass,
    pub fingerprint: Fingerprint,
    pub readonly: bool,
    pub attributes: u32,
    /// `None`, wenn die Kennung nicht ermittelt werden konnte (etwa auf
    /// Dateisystemen ohne stabile Kennung oder bei fehlenden Rechten).
    pub id: Option<FileId>,
}

impl FileState {
    /// Wahr, wenn beide Zustaende beweisbar dieselbe Datei beschreiben.
    ///
    /// Liegt eine Dateikennung vor, ist das ein Beweis. Sonst bleibt nur der
    /// Fingerabdruck, der ein sehr starkes Indiz ist.
    pub fn same_file_as(&self, other: &FileState) -> bool {
        match (self.id, other.id) {
            (Some(a), Some(b)) => a.volume == b.volume && a.index == b.index,
            _ => self.fingerprint.matches(&other.fingerprint),
        }
    }
}

/// Liest den Zustand, **ohne** einem Link zu folgen.
pub fn inspect(path: &Path) -> Result<FileState> {
    let meta = std::fs::symlink_metadata(paths::extended(path))
        .map_err(|e| AppError::from_io(&e, &format!("{}", path.display())))?;
    let class = classify(&meta);
    Ok(FileState {
        // Die Kennung nur fuer echte Dateien holen - bei Platzhaltern und
        // Verknuepfungen wird ohnehin nichts angefasst.
        id: if matches!(class, FileClass::Regular) { file_id(path) } else { None },
        class,
        fingerprint: Fingerprint::of(&meta),
        readonly: is_readonly(&meta),
        attributes: attributes(&meta),
    })
}

/// Prueft, dass der Pfad eine gewoehnliche, lokal vorliegende Datei ist.
///
/// Das ist die Eintrittsbedingung fuer jede veraendernde Operation.
pub fn require_regular_file(path: &Path) -> Result<FileState> {
    let state = inspect(path)?;
    match state.class {
        FileClass::Regular => Ok(state),
        FileClass::Reparse => Err(AppError::Invalid(format!(
            "{} ist eine Verknuepfung (Symlink oder Junction). FileTriage fasst \
             Verknuepfungen nicht an, weil dabei unklar bleibt, welche Datei \
             tatsaechlich betroffen waere.",
            path.display()
        ))),
        FileClass::CloudPlaceholder => Err(AppError::Invalid(format!(
            "{} liegt nur in der Cloud und nicht lokal vor. FileTriage laedt \
             keine Dateien ungefragt herunter.",
            path.display()
        ))),
        FileClass::Directory => Err(AppError::Invalid(format!(
            "{} ist ein Ordner, keine Datei",
            path.display()
        ))),
        FileClass::Other => Err(AppError::Invalid(format!(
            "{} ist keine gewoehnliche Datei",
            path.display()
        ))),
    }
}

/// Freier Platz auf dem Volume des Pfades, in Bytes.
///
/// Wird vor jedem Kopiervorgang geprueft: lieber vorher ablehnen als eine
/// halbe Datei schreiben.
#[cfg(windows)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    // Auf das naechste existierende Elternverzeichnis zurueckgehen - das Ziel
    // selbst muss noch nicht angelegt sein.
    let mut dir = path.to_path_buf();
    while !dir.exists() {
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => return None,
        }
    }

    let mut wide: Vec<u16> = paths::extended(&dir).as_os_str().encode_wide().collect();
    wide.push(0);
    let mut verfuegbar: u64 = 0;
    let mut gesamt: u64 = 0;
    let mut frei: u64 = 0;
    // SAFETY: `wide` ist nullterminiert und lebt ueber den Aufruf hinaus; die
    // drei Ausgabezeiger verweisen auf gueltige lokale Variablen.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut verfuegbar, &mut gesamt, &mut frei)
    };
    if ok == 0 { None } else { Some(verfuegbar) }
}

#[cfg(not(windows))]
pub fn free_space(_path: &Path) -> Option<u64> {
    None
}

/// Volume-Kennung (Laufwerksbuchstabe oder UNC-Freigabe) fuer die Frage,
/// ob ein Verschieben die Volumegrenze ueberschreitet.
pub fn volume_of(path: &Path) -> String {
    let s = paths::path_key(&paths::display_path(path));
    if let Some(rest) = s.strip_prefix("\\\\") {
        // \\server\share\... -> \\server\share
        let teile: Vec<&str> = rest.splitn(3, '\\').collect();
        if teile.len() >= 2 {
            return format!("\\\\{}\\{}", teile[0], teile[1]);
        }
        return format!("\\\\{rest}");
    }
    match s.find(':') {
        Some(i) => s[..=i].to_string(),
        None => String::new(),
    }
}

/// Liegen Quelle und Ziel auf demselben Volume?
pub fn same_volume(a: &Path, b: &Path) -> bool {
    let va = volume_of(a);
    let vb = volume_of(b);
    !va.is_empty() && va == vb
}

/// Netzwerkpfad (UNC oder verbundenes Netzlaufwerk kann nicht sicher erkannt
/// werden - erkannt wird die UNC-Form).
pub fn is_unc(path: &Path) -> bool {
    let s = paths::display_path(path);
    s.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn gewoehnliche_datei_wird_akzeptiert() {
        let dir = TempDir::new("fsmeta");
        let p = dir.file("normal.txt", b"x");
        let state = require_regular_file(&p).unwrap();
        assert_eq!(state.class, FileClass::Regular);
        assert_eq!(state.fingerprint.size, 1);
    }

    #[test]
    fn ordner_wird_abgelehnt() {
        let dir = TempDir::new("fsmeta-dir");
        let p = dir.dir("unterordner");
        assert!(require_regular_file(&p).is_err());
    }

    #[test]
    fn fingerabdruck_erkennt_veraenderung() {
        let dir = TempDir::new("fsmeta-fp");
        let p = dir.file("a.txt", b"eins");
        let vorher = inspect(&p).unwrap().fingerprint;

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, b"zwei-anders").unwrap();
        let nachher = inspect(&p).unwrap().fingerprint;

        assert!(!vorher.matches(&nachher), "geänderte Datei muss auffallen");
        assert!(vorher.matches(&vorher));
    }

    #[test]
    fn fingerabdruck_erkennt_austausch_bei_gleicher_groesse() {
        let dir = TempDir::new("fsmeta-swap");
        let p = dir.file("a.bin", b"AAAA");
        let vorher = inspect(&p).unwrap().fingerprint;

        std::thread::sleep(std::time::Duration::from_millis(20));
        // Gleiche Groesse, anderer Inhalt - nur der Zeitstempel verraet es.
        std::fs::write(&p, b"BBBB").unwrap();
        let nachher = inspect(&p).unwrap().fingerprint;
        assert_eq!(vorher.size, nachher.size);
        assert!(!vorher.matches(&nachher));
    }

    #[test]
    fn dateikennung_ist_pfadunabhaengig_und_erkennt_hardlinks() {
        let dir = TempDir::new("fsmeta-id");
        let a = dir.file("a.bin", b"inhalt");
        let id_a = file_id(&a).expect("Dateikennung muss auf NTFS verfügbar sein");
        assert!(!id_a.is_hardlinked(), "eine einzelne Datei hat genau einen Eintrag");

        let b = dir.path().join("b.bin");
        if !crate::testutil::make_hardlink(&b, &a) {
            eprintln!("Hardlink nicht möglich - Teil des Tests übersprungen");
            return;
        }
        let id_b = file_id(&b).unwrap();
        assert_eq!(id_a.index, id_b.index, "Hardlinks zeigen auf dieselben Daten");
        assert_eq!(id_a.volume, id_b.volume);
        assert!(file_id(&a).unwrap().is_hardlinked(), "jetzt sind es zwei Eintraege");
    }

    #[test]
    fn dateikennung_unterscheidet_verschiedene_dateien() {
        let dir = TempDir::new("fsmeta-id2");
        let a = dir.file("a.bin", b"gleich");
        let b = dir.file("b.bin", b"gleich");
        assert_ne!(file_id(&a).unwrap().index, file_id(&b).unwrap().index);
    }

    #[test]
    fn dateikennung_ueberlebt_umbenennen() {
        let dir = TempDir::new("fsmeta-id3");
        let a = dir.file("vorher.bin", b"x");
        let vorher = file_id(&a).unwrap();
        let b = dir.path().join("nachher.bin");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(vorher.index, file_id(&b).unwrap().index);
    }

    #[cfg(windows)]
    #[test]
    fn handle_sperrt_deterministischen_austausch_bis_zur_mutation() {
        let dir=TempDir::new("locked-race");let path=dir.file("victim.bin",b"original");let locked=LockedFile::open(&path).unwrap();
        // Deterministischer Race-Hook: genau nach Handle-Verifikation versucht
        // ein anderer Pfadnutzer, das Objekt zu entfernen/zu ersetzen.
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(dir.file("injected.bin",b"fremd"),&path).is_err());
        locked.delete().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn dateikennung_fehlt_bei_fehlender_datei() {
        let dir = TempDir::new("fsmeta-id4");
        assert!(file_id(&dir.path().join("gibtsnicht")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn starke_dateikennung_enthaelt_128_bit_id_und_erstellzeit() {
        let d=TempDir::new("strong-id");let p=d.file("a.bin",b"x");
        let id=strong_file_id(&p).expect("Windows FileIdInfo");
        assert_ne!(id.id,[0;16]);assert_ne!(id.created,0);
        assert_eq!(StrongFileId::decode(&id.encode()),Some(id));
    }

    #[test]
    fn same_file_as_erkennt_austausch() {
        let dir = TempDir::new("fsmeta-same");
        let p = dir.file("a.bin", b"AAAA");
        let vorher = inspect(&p).unwrap();
        // Loeschen und neu anlegen erzeugt eine andere Datei, auch bei
        // gleichem Namen und gleicher Groesse.
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, b"BBBB").unwrap();
        let nachher = inspect(&p).unwrap();
        assert!(!vorher.same_file_as(&nachher));
    }

    #[test]
    fn volume_wird_erkannt() {
        assert_eq!(volume_of(Path::new("C:\\a\\b")), "c:");
        assert_eq!(volume_of(Path::new("D:\\x")), "d:");
        assert_eq!(volume_of(Path::new("\\\\server\\share\\datei")), "\\\\server\\share");
        assert!(same_volume(Path::new("C:\\a"), Path::new("C:\\b\\c")));
        assert!(!same_volume(Path::new("C:\\a"), Path::new("D:\\a")));
    }

    #[test]
    fn unc_wird_erkannt() {
        assert!(is_unc(Path::new("\\\\server\\share\\x")));
        assert!(!is_unc(Path::new("C:\\x")));
    }

    #[test]
    fn freier_platz_wird_ermittelt() {
        let dir = TempDir::new("fsmeta-space");
        let frei = free_space(dir.path()).expect("freier Platz muss ermittelbar sein");
        assert!(frei > 0);
        // Auch fuer einen noch nicht existierenden Unterordner.
        assert!(free_space(&dir.path().join("gibtsnochnicht")).is_some());
    }

    #[test]
    fn schreibgeschuetzte_datei_wird_erkannt() {
        let dir = TempDir::new("fsmeta-ro");
        let p = dir.file("ro.txt", b"x");
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();

        let state = inspect(&p).unwrap();
        assert!(state.readonly);
        assert_eq!(state.class, FileClass::Regular, "schreibgeschuetzt bleibt eine normale Datei");

        // Aufraeumen, sonst scheitert das Loeschen des Testordners.
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&p, perms).unwrap();
    }
}

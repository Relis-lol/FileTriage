//! Tolerant Windows recycle-bin enumeration.
//!
//! `trash::os_limited::list` aborts the complete listing when any unrelated
//! Shell item has malformed metadata. Undo must instead ignore that one item.

#[derive(Debug)]
pub struct RecycleItem {
    pub item: trash::TrashItem,
    /// Vollhash des noch im Papierkorb liegenden Shell-Items, sofern dessen
    /// Namespace-Handler den dokumentierten BHID_Stream anbietet.
    pub full_hash: Option<String>,
}

#[cfg(windows)]
#[allow(unsafe_op_in_unsafe_fn)]
pub fn list_tolerant() -> Result<Vec<RecycleItem>, trash::Error> {
    use std::{ffi::{c_void, OsString}, os::windows::ffi::OsStringExt, path::PathBuf};
    use windows::core::{Interface, PWSTR};
    use windows::Win32::{Foundation::*, System::Com::*, System::Com::StructuredStorage::PropVariantToBSTR, UI::Shell::PropertiesSystem::*, UI::Shell::*};

    const ORIGINAL: PROPERTYKEY = PROPERTYKEY { fmtid: PSGUID_DISPLACED, pid: PID_DISPLACED_FROM };
    const DELETED: PROPERTYKEY = PROPERTYKEY { fmtid: PSGUID_DISPLACED, pid: PID_DISPLACED_DATE };
    unsafe fn os(p: PWSTR) -> OsString { let mut n=0; while *p.0.offset(n)!=0 { n+=1; } let v=OsString::from_wide(std::slice::from_raw_parts(p.0,n as usize)); CoTaskMemFree(Some(p.0 as *const c_void)); v }
    unsafe fn name(i:&IShellItem, kind:SIGDN)->windows::core::Result<OsString>{Ok(os(i.GetDisplayName(kind)?))}
    unsafe fn stream_hash(i:&IShellItem)->Option<String>{
        let stream:IStream=i.BindToHandler(None,&BHID_Stream).ok()?;
        let mut hasher=blake3::Hasher::new();
        let mut buffer=[0u8;1024*1024];
        loop {
            let mut read=0u32;
            if stream.Read(buffer.as_mut_ptr() as *mut c_void,buffer.len() as u32,Some(&mut read)).is_err(){return None}
            if read==0{break}
            hasher.update(&buffer[..read as usize]);
        }
        Some(hasher.finalize().to_hex().to_string())
    }
    unsafe fn one(i:&IShellItem)->windows::core::Result<RecycleItem>{
        let id=name(i,SIGDN_DESKTOPABSOLUTEPARSING)?;
        let item_name=name(i,SIGDN_PARENTRELATIVE)?;
        let i2:IShellItem2=i.cast()?;
        let pv=i2.GetProperty(&ORIGINAL)?; let b=PropVariantToBSTR(&pv)?;
        let parent=PathBuf::from(OsString::from_wide(b.as_wide()));
        let ft=i2.GetFileTime(&DELETED)?; let raw=((ft.dwHighDateTime as u64)<<32)|ft.dwLowDateTime as u64;
        let secs=raw.saturating_sub(116444736000000000)/10000000;
        let full_hash=stream_hash(i);
        Ok(RecycleItem{item:trash::TrashItem{id,name:item_name,original_parent:parent,time_deleted:secs as i64},full_hash})
    }
    struct ComGuard(bool);
    impl Drop for ComGuard{fn drop(&mut self){if self.0{unsafe{CoUninitialize()}}}}
    unsafe {
        // `trash` verwendet standardmaessig STA. Dieselbe Apartment-Art
        // vermeidet einen RPC_E_CHANGED_MODE beim anschliessenden Restore.
        let hr=CoInitializeEx(None,COINIT_APARTMENTTHREADED|COINIT_DISABLE_OLE1DDE);
        if hr.is_err() && hr != RPC_E_CHANGED_MODE { return Err(trash::Error::Os{code:hr.0,description:format!("windows error: {hr:?}")}); }
        let _com=ComGuard(hr.is_ok());
        let bin:IShellItem=SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder,KF_FLAG_DEFAULT,HANDLE::default()).map_err(winerr)?;
        let en:IEnumShellItems=bin.BindToHandler(None,&BHID_EnumItems).map_err(winerr)?;
        collect_tolerant(|| {
            let mut got=0; let mut arr=[None];
            en.Next(&mut arr,Some(&mut got)).map_err(winerr)?;
            if got==0 { return Ok(None) }
            Ok(arr[0].as_ref().map(|i|one(i).map_err(winerr)))
        })
    }
}

#[cfg(windows)]
fn winerr(e:windows::core::Error)->trash::Error{trash::Error::Os{code:e.code().0,description:format!("windows error: {e}")}}

fn collect_tolerant<T,E:std::fmt::Display>(mut next:impl FnMut()->Result<Option<Result<T,E>>,E>)->Result<Vec<T>,E>{
    let mut out=Vec::new();
    while let Some(item)=next()? { match item { Ok(value)=>out.push(value),Err(error)=>tracing::warn!("Papierkorb-Eintrag mit ungültigen Metadaten übersprungen: {error}") } }
    Ok(out)
}

#[cfg(not(windows))]
pub fn list_tolerant() -> Result<Vec<RecycleItem>, trash::Error> { Ok(trash::os_limited::list()?.into_iter().map(|item|RecycleItem{item,full_hash:None}).collect()) }

#[cfg(test)] mod tests {
    #[test] fn malformed_unrelated_item_does_not_hide_valid_item(){let mut seq=vec![Err("property"),Ok(7)].into_iter();let got=super::collect_tolerant(||Ok::<_,&str>(seq.next())).unwrap();assert_eq!(got,vec![7]);}
    #[test] fn enumerator_error_fails_closed(){let mut calls=0;let result=super::collect_tolerant::<i32,&str>(||{calls+=1;if calls==1{Ok(Some(Ok(7)))}else{Err("next")}});assert_eq!(result.unwrap_err(),"next");}
}

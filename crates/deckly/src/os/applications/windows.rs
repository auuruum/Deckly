//! The Windows AppsFolder includes desktop and packaged (Store) applications.
//! Shell COM is needed because packaged apps do not have executable paths.

use super::{Application, Catalog, MAX_APPLICATIONS};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
    SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: created only after successful initialization, dropped on
        // the same worker thread after all its COM interfaces have gone.
        unsafe { CoUninitialize() };
    }
}

pub(super) fn discover() -> Result<Catalog, String> {
    // SAFETY: a fresh discovery thread owns this apartment. No interface
    // leaves this thread; all API failures are returned or skipped.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
        .map_err(|e| format!("Cannot initialize application discovery: {e}"))?;
    let _apartment = Apartment;
    collect().map_err(|e| format!("Cannot list installed applications: {e}"))
}

fn collect() -> windows::core::Result<Catalog> {
    // SAFETY: COM is initialized for this thread. The requested interfaces
    // are owned by Rust wrappers and released before the apartment closes.
    let folder: IShellItem =
        unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)? };
    // SAFETY: the live Shell folder supplies the enumerator, no bind context
    // is required by BHID_EnumItems.
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None, &BHID_EnumItems)? };
    let mut catalog = Catalog::default();
    for _ in 0..MAX_APPLICATIONS {
        let mut next = [None];
        let mut fetched = 0;
        // SAFETY: one initialized output slot and a valid fetched-count
        // pointer remain alive throughout this call.
        unsafe { items.Next(&mut next, Some(&mut fetched))? };
        let Some(item) = next.into_iter().next().flatten().filter(|_| fetched != 0) else {
            return Ok(catalog);
        };
        if let (Ok(name), Ok(id)) = (
            display_name(&item, SIGDN_NORMALDISPLAY),
            display_name(&item, SIGDN_PARENTRELATIVEPARSING),
        ) {
            catalog.add(Application {
                name,
                target: format!("shell:AppsFolder\\{id}"),
                detail: "Installed application".into(),
                keywords: String::new(),
            });
        }
    }
    catalog.truncated = true;
    Ok(catalog)
}

fn display_name(item: &IShellItem, kind: SIGDN) -> windows::core::Result<String> {
    // SAFETY: Shell returns a null-terminated, COM-allocated string. Copy it
    // while alive, and free it even if conversion fails.
    unsafe {
        let text = item.GetDisplayName(kind)?;
        let result = text.to_string();
        CoTaskMemFree(Some(text.0.cast()));
        Ok(result?)
    }
}

//! The Windows AppsFolder includes desktop and packaged (Store) applications.
//! Shell COM is needed because packaged apps do not have executable paths.

use super::{Application, Catalog, MAX_APPLICATIONS};
use windows::Win32::Storage::EnhancedStorage::PKEY_Link_TargetParsingPath;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2, KF_FLAG_DEFAULT,
    SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows::core::Interface;

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
            let process = process_name(&id, link_target(&item).as_deref());
            catalog.add(Application {
                name,
                target: format!("shell:AppsFolder\\{id}"),
                process,
                detail: "Installed application".into(),
                keywords: String::new(),
                icon: format!("shell:AppsFolder\\{id}"),
            });
        }
    }
    catalog.truncated = true;
    Ok(catalog)
}

/// What a Start menu shortcut leads to; packaged apps have nothing here.
fn link_target(item: &IShellItem) -> Option<String> {
    let item: IShellItem2 = item.cast().ok()?;
    // SAFETY: the key is a static. Shell returns a null-terminated,
    // COM-allocated string; copy it while alive and free it either way.
    unsafe {
        let text = item.GetString(&PKEY_Link_TargetParsingPath).ok()?;
        let result = text.to_string().ok();
        CoTaskMemFree(Some(text.0.cast()));
        result
    }
}

/// A shortcut's program, else the package id the entry goes by. A shortcut to
/// a link or a document says nothing about what will run.
fn process_name(id: &str, target: Option<&str>) -> String {
    match target.filter(|t| !t.is_empty()) {
        Some(path) if path.to_lowercase().ends_with(".exe") => path.to_owned(),
        Some(_) => String::new(),
        None => id.to_owned(),
    }
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

/// What discovery and a chosen file give. A rule's source comes from a profile
/// file, and a shared one must not send Shell to a computer on the network.
fn local_icon(target: &str) -> bool {
    target.starts_with(r"shell:AppsFolder\")
        || matches!(target.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

pub(super) fn icon(target: &str) -> Option<image::RgbaImage> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
    };
    use windows::core::HSTRING;

    struct Bitmap(HBITMAP);
    impl Drop for Bitmap {
        fn drop(&mut self) {
            // SAFETY: GetImage transferred this bitmap to us, and the bitmap
            // has never been selected into a device context.
            unsafe {
                let _ = DeleteObject(self.0.into());
            }
        }
    }
    struct Dc(HDC);
    impl Drop for Dc {
        fn drop(&mut self) {
            // SAFETY: this handle came from CreateCompatibleDC and is owned
            // exclusively by this stack frame.
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
    if !local_icon(target) {
        return None;
    }
    // SAFETY: this function runs only on the icon worker. Every successful
    // initialization is balanced after all local COM interfaces are released.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
    }
    let _apartment = Apartment;
    // SAFETY: target is a live, nul-terminated HSTRING during the call. Shell
    // owns the parsing and validation; no interface leaves this thread.
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(target), None).ok()? };
    let factory: IShellItemImageFactory = item.cast().ok()?;
    // SAFETY: the COM interface is valid; GetImage returns an owned HBITMAP.
    let bitmap = Bitmap(unsafe {
        factory
            .GetImage(SIZE { cx: 64, cy: 64 }, SIIGBF_ICONONLY)
            .ok()?
    });
    let mut info = BITMAP::default();
    // SAFETY: info is a correctly sized, writable BITMAP, alive for the call.
    let copied = unsafe {
        GetObjectW(
            bitmap.0.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some((&mut info as *mut BITMAP).cast()),
        )
    };
    if copied != std::mem::size_of::<BITMAP>() as i32 {
        return None;
    }
    let width = u32::try_from(info.bmWidth).ok()?;
    let height = u32::try_from(info.bmHeight.checked_abs()?).ok()?;
    if width == 0 || height == 0 || width > 256 || height > 256 {
        return None;
    }
    // SAFETY: a null source requests a compatible memory DC, owned below.
    let dc = Dc(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return None;
    }
    let mut dib = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bytes = vec![0; width as usize * height as usize * 4];
    // SAFETY: dimensions were bounded above. bytes has exactly the size
    // required for a top-down 32-bit DIB, and no bitmap is selected into dc.
    let rows = unsafe {
        GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height,
            Some(bytes.as_mut_ptr().cast()),
            &mut dib,
            DIB_RGB_COLORS,
        )
    };
    if rows != height as i32 {
        return None;
    }
    super::icons::bgra(&mut bytes);
    image::RgbaImage::from_raw(width, height, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values seen in the Start menu listing of a Windows 10 machine.
    #[test]
    fn a_process_is_named_by_the_shortcuts_program_or_the_package() {
        let discord = r"C:\Users\me\AppData\Local\Discord\Discord.exe";
        assert_eq!(
            process_name("com.squirrel.Discord.Discord", Some(discord)),
            discord
        );
        let calculator = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(process_name(calculator, None), calculator);
        assert_eq!(process_name(calculator, Some("")), calculator);
        assert_eq!(
            process_name("steam://rungameid/240", Some("steam://rungameid/240")),
            ""
        );
        assert_eq!(process_name("manual", Some(r"C:\Docs\manual.pdf")), "");
    }

    #[test]
    fn icons_come_from_this_computer_only() {
        assert!(local_icon(r"shell:AppsFolder\com.squirrel.Discord.Discord"));
        assert!(local_icon(r"C:\Games\game.exe"));
        assert!(local_icon("D:/Games/game.exe"));
        for remote in [
            r"\\host\share\game.exe",
            "//host/share/game.exe",
            r"/\host\share\game.exe",
            r"\\?\UNC\host\share\game.exe",
            "file://host/share/game.exe",
            "https://host/game.exe",
            r"shell:NetworkPlacesFolder\host",
            "game.exe",
            "",
        ] {
            assert!(!local_icon(remote), "{remote}");
            assert!(icon(remote).is_none(), "{remote}");
        }
    }

    #[test]
    fn a_chosen_program_gives_its_own_picture() {
        let windows = std::env::var("WINDIR").unwrap();
        let picture = icon(&format!(r"{windows}\explorer.exe")).unwrap();
        assert!(picture.pixels().any(|pixel| pixel.0[3] != 0));
    }
}

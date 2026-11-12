//! Windows Credential Manager. Needs the OS: the credentials are kept and
//! protected by Windows for the signed-in user, reached only through the
//! `Cred*` functions of advapi32.
//!
//! Each secret is a generic credential named `Deckly/<plugin>/<id>`, kept
//! on this computer only (not roamed).

use super::Name;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

fn not_found(e: &windows::core::Error) -> bool {
    e.code() == ERROR_NOT_FOUND.to_hresult()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn probe() -> Result<(), String> {
    // Credential Manager is part of every Windows Deckly runs on; reading
    // a name that is not there proves it answers.
    read(Name {
        plugin: "deckly",
        id: "probe",
    })
    .map(drop)
}

pub fn read(name: Name<'_>) -> Result<Option<String>, String> {
    let target = HSTRING::from(name.target());
    let mut found: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `target` is a NUL-terminated wide string that outlives the
    // call, and `found` is a valid place for the returned pointer.
    let read = unsafe { CredReadW(&target, CRED_TYPE_GENERIC, None, &mut found) };
    match read {
        Ok(()) => {}
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(format!("Credential Manager failed: {e}")),
    }
    if found.is_null() {
        return Ok(None);
    }
    // SAFETY: CredReadW succeeded, so `found` points to a credential that
    // stays valid until CredFree; its blob holds `CredentialBlobSize` bytes.
    let bytes = unsafe {
        let c = &*found;
        let len = c.CredentialBlobSize as usize;
        let bytes = if c.CredentialBlob.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, len).to_vec()
        };
        CredFree(found.cast());
        bytes
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "a stored secret is not text".into())
}

pub fn write(name: Name<'_>, value: &str) -> Result<(), String> {
    let mut blob = value.as_bytes().to_vec();
    if blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(format!(
            "the secret is longer than Credential Manager allows ({} bytes)",
            CRED_MAX_CREDENTIAL_BLOB_SIZE
        ));
    }
    let mut target = wide(&name.target());
    let mut user = wide("Deckly");
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..CREDENTIALW::default()
    };
    // SAFETY: every pointer in `credential` points into a buffer above
    // that lives until after the call; the strings are NUL-terminated.
    unsafe { CredWriteW(&credential, 0) }.map_err(|e| format!("Credential Manager failed: {e}"))
}

pub fn delete(name: Name<'_>) -> Result<(), String> {
    let target = wide(&name.target());
    // SAFETY: `target` is a NUL-terminated wide string that outlives the call.
    match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(()),
        Err(e) if not_found(&e) => Ok(()),
        Err(e) => Err(format!("Credential Manager failed: {e}")),
    }
}

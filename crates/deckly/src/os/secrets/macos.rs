//! The macOS Keychain. Needs the OS: the login keychain is kept and
//! protected by macOS for the user, reached only through Security.framework.
//!
//! Each secret is a generic password with the service `Deckly` and the
//! account `<plugin>/<id>`. The few Core Foundation and Security calls
//! needed are declared here directly, so no crate is added for them.

use super::Name;
use std::ffi::c_void;

type CFTypeRef = *const c_void;
type OSStatus = i32;

const UTF8: u32 = 0x0800_0100;
const NOT_FOUND: OSStatus = -25300;
const SERVICE: &str = "Deckly";
/// Larger values are refused rather than stored.
const MAX_SECRET_BYTES: usize = 64 * 1024;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: [usize; 6];
    static kCFTypeDictionaryValueCallBacks: [usize; 5];
    static kCFBooleanTrue: CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, count: isize) -> CFTypeRef;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFDataGetTypeID() -> usize;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CFTypeRef;
    static kSecClassGenericPassword: CFTypeRef;
    static kSecAttrService: CFTypeRef;
    static kSecAttrAccount: CFTypeRef;
    static kSecAttrLabel: CFTypeRef;
    static kSecValueData: CFTypeRef;
    static kSecReturnData: CFTypeRef;
    static kSecMatchLimit: CFTypeRef;
    static kSecMatchLimitOne: CFTypeRef;
    fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemUpdate(query: CFTypeRef, attributes: CFTypeRef) -> OSStatus;
    fn SecItemDelete(query: CFTypeRef) -> OSStatus;
}

/// A Core Foundation object this code created and releases.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the object was created (or copied) by this code with
            // one reference, which is given up here exactly once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn string(text: &str) -> Result<Owned, String> {
    // SAFETY: the bytes are valid UTF-8 for the given length; the new
    // string copies them.
    let s = unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            text.as_ptr(),
            text.len() as isize,
            UTF8,
            0,
        )
    };
    (!s.is_null())
        .then_some(Owned(s))
        .ok_or_else(|| "the Keychain query could not be built".into())
}

fn data(bytes: &[u8]) -> Result<Owned, String> {
    // SAFETY: the slice is valid for its length; the new data copies it.
    let d = unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
    (!d.is_null())
        .then_some(Owned(d))
        .ok_or_else(|| "the Keychain query could not be built".into())
}

fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Result<Owned, String> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
    // SAFETY: both arrays hold `pairs.len()` valid Core Foundation objects
    // that live through the call; the type callbacks retain them.
    let d = unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as isize,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        )
    };
    (!d.is_null())
        .then_some(Owned(d))
        .ok_or_else(|| "the Keychain query could not be built".into())
}

fn failed(status: OSStatus) -> String {
    format!("the Keychain failed (error {status})")
}

/// The pairs that find one secret.
fn query_pairs(service: &Owned, account: &Owned) -> Vec<(CFTypeRef, CFTypeRef)> {
    // SAFETY: the Security constants are immutable objects that live as
    // long as the framework is loaded, which is the whole process.
    unsafe {
        vec![
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, service.0),
            (kSecAttrAccount, account.0),
        ]
    }
}

fn names(name: Name<'_>) -> Result<(Owned, Owned), String> {
    Ok((
        string(SERVICE)?,
        string(&format!("{}/{}", name.plugin, name.id))?,
    ))
}

pub fn probe() -> Result<(), String> {
    read(Name {
        plugin: "deckly",
        id: "probe",
    })
    .map(drop)
}

pub fn read(name: Name<'_>) -> Result<Option<String>, String> {
    let (service, account) = names(name)?;
    let mut pairs = query_pairs(&service, &account);
    // SAFETY: as in `query_pairs`.
    unsafe {
        pairs.push((kSecReturnData, kCFBooleanTrue));
        pairs.push((kSecMatchLimit, kSecMatchLimitOne));
    }
    let query = dictionary(&pairs)?;
    let mut found: CFTypeRef = std::ptr::null();
    // SAFETY: `query` is a valid dictionary; `found` receives a new
    // reference that `Owned` releases.
    let status = unsafe { SecItemCopyMatching(query.0, &mut found) };
    let found = Owned(found);
    match status {
        0 => {}
        NOT_FOUND => return Ok(None),
        s => return Err(failed(s)),
    }
    // SAFETY: `found` is a valid object; it is read as data only after its
    // type is checked, and its bytes are copied before it is released.
    let bytes = unsafe {
        if found.0.is_null() || CFGetTypeID(found.0) != CFDataGetTypeID() {
            return Err("the Keychain returned something other than data".into());
        }
        let len = usize::try_from(CFDataGetLength(found.0)).unwrap_or(0);
        if len > MAX_SECRET_BYTES {
            return Err("a stored secret is too large".into());
        }
        let ptr = CFDataGetBytePtr(found.0);
        if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, len).to_vec()
        }
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "a stored secret is not text".into())
}

pub fn write(name: Name<'_>, value: &str) -> Result<(), String> {
    if value.len() > MAX_SECRET_BYTES {
        return Err("the secret is too large".into());
    }
    let (service, account) = names(name)?;
    let value = data(value.as_bytes())?;
    let query = dictionary(&query_pairs(&service, &account))?;
    // SAFETY: as in `query_pairs`.
    let update = dictionary(&[(unsafe { kSecValueData }, value.0)])?;
    // SAFETY: both dictionaries are valid for the call.
    match unsafe { SecItemUpdate(query.0, update.0) } {
        0 => return Ok(()),
        NOT_FOUND => {}
        s => return Err(failed(s)),
    }
    let label = string(&name.label())?;
    let mut pairs = query_pairs(&service, &account);
    // SAFETY: as in `query_pairs`.
    unsafe {
        pairs.push((kSecValueData, value.0));
        pairs.push((kSecAttrLabel, label.0));
    }
    let add = dictionary(&pairs)?;
    // SAFETY: `add` is a valid dictionary; no result is asked for.
    match unsafe { SecItemAdd(add.0, std::ptr::null_mut()) } {
        0 => Ok(()),
        s => Err(failed(s)),
    }
}

pub fn delete(name: Name<'_>) -> Result<(), String> {
    let (service, account) = names(name)?;
    let query = dictionary(&query_pairs(&service, &account))?;
    // SAFETY: `query` is a valid dictionary.
    match unsafe { SecItemDelete(query.0) } {
        0 | NOT_FOUND => Ok(()),
        s => Err(failed(s)),
    }
}

//! Keys through the kernel's uinput device: a keyboard the system takes for
//! a real one, under any desktop. It presses places on the keyboard, not
//! symbols, so the active layout decides what comes out, and it needs write
//! access to /dev/uinput (assets/linux/60-deckly.rules grants it).

use super::Failure;
use super::keys::{Key, SHIFT, Stroke};
use rustix::ioctl::{self, IntegerSetter, NoArg, Setter, opcode};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsFd;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const PATH: &str = "/dev/uinput";
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
const BUS_VIRTUAL: u16 = 6;
/// The highest key this device reports; keyboards end at 248.
const MAX_KEY: i32 = 248;
/// Between key events.
const KEY_GAP: Duration = Duration::from_millis(2);
/// The desktop needs a moment to take up a new device; earlier keys are lost.
const SETTLE: Duration = Duration::from_millis(300);
const ACCESS_HINT: &str = "no access to /dev/uinput; install assets/linux/60-deckly.rules \
                           from Deckly's repository";
const MODULE_HINT: &str = "the uinput kernel module is not loaded; run \"modprobe uinput\", \
                           or restart if the kernel was updated";
/// The error for a device node whose driver is not there.
const ENODEV: i32 = 19;

/// `struct uinput_setup` from `linux/uinput.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Setup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

const UI_DEV_CREATE: ioctl::Opcode = opcode::none(b'U', 1);
const UI_DEV_SETUP: ioctl::Opcode = opcode::write::<Setup>(b'U', 3);
const UI_SET_EVBIT: ioctl::Opcode = opcode::write::<i32>(b'U', 100);
const UI_SET_KEYBIT: ioctl::Opcode = opcode::write::<i32>(b'U', 101);

/// The device lives as long as Deckly: making one per press would lose keys
/// to the settling time.
static DEVICE: Mutex<Option<File>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<File>> {
    DEVICE.lock().unwrap_or_else(|e| e.into_inner())
}

fn create() -> Result<File, Failure> {
    let file = OpenOptions::new().write(true).open(PATH).map_err(|e| {
        Failure::Unavailable(match e.kind() {
            ErrorKind::PermissionDenied => ACCESS_HINT.into(),
            _ if e.raw_os_error() == Some(ENODEV) => MODULE_HINT.into(),
            ErrorKind::NotFound => MODULE_HINT.into(),
            _ => e.to_string(),
        })
    })?;
    let unavailable = |e: rustix::io::Errno| Failure::Unavailable(format!("{PATH}: {e}"));
    let mut name = [0u8; 80];
    let label = b"Deckly virtual keyboard";
    name[..label.len()].copy_from_slice(label);
    let setup = Setup {
        bustype: BUS_VIRTUAL,
        vendor: 0x1d6b,
        product: 0x0104,
        version: 1,
        name,
        ff_effects_max: 0,
    };
    let fd = file.as_fd();
    // SAFETY: each opcode matches its argument's type in linux/uinput.h: the
    // two SET requests take an integer by value, UI_DEV_SETUP reads one
    // `Setup` that lives through the call, and UI_DEV_CREATE takes nothing.
    unsafe {
        ioctl::ioctl(fd, IntegerSetter::<UI_SET_EVBIT>::new_usize(EV_KEY.into()))
            .map_err(unavailable)?;
        for key in 1..=MAX_KEY {
            ioctl::ioctl(fd, IntegerSetter::<UI_SET_KEYBIT>::new_usize(key as usize))
                .map_err(unavailable)?;
        }
        ioctl::ioctl(fd, Setter::<UI_DEV_SETUP, Setup>::new(setup)).map_err(unavailable)?;
        ioctl::ioctl(fd, NoArg::<UI_DEV_CREATE>::new()).map_err(unavailable)?;
    }
    std::thread::sleep(SETTLE);
    Ok(file)
}

/// `struct input_event`: a time the kernel fills in, then type, code, value.
fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT_BYTES] {
    let mut out = [0u8; EVENT_BYTES];
    let mut fields = [0u8; 8];
    fields[..2].copy_from_slice(&kind.to_ne_bytes());
    fields[2..4].copy_from_slice(&code.to_ne_bytes());
    fields[4..].copy_from_slice(&value.to_ne_bytes());
    out[TIME_BYTES..].copy_from_slice(&fields);
    out
}

const TIME_BYTES: usize = 2 * size_of::<std::ffi::c_long>();
const EVENT_BYTES: usize = TIME_BYTES + 8;

fn key_event(file: &mut File, code: u16, down: bool) -> std::io::Result<()> {
    let mut report = [0u8; 2 * EVENT_BYTES];
    report[..EVENT_BYTES].copy_from_slice(&event(EV_KEY, code, i32::from(down)));
    report[EVENT_BYTES..].copy_from_slice(&event(EV_SYN, SYN_REPORT, 0));
    file.write_all(&report)?;
    std::thread::sleep(KEY_GAP);
    Ok(())
}

/// The input codes held for a stroke, in press order, and the key itself.
/// `None` for a symbol that has no place on a US keyboard.
fn places(stroke: &Stroke) -> Option<(Vec<u16>, u16)> {
    let code = |key: Key| key.code;
    let mut held: Vec<u16> = stroke
        .mods
        .iter()
        .map(|&m| code(m))
        .collect::<Option<_>>()?;
    let shift = SHIFT.code?;
    if stroke.key.shifted && !held.contains(&shift) {
        held.push(shift);
    }
    Some((held, stroke.key.code?))
}

fn press(file: &mut File, held: &[u16], key: u16) -> std::io::Result<()> {
    for &code in held {
        key_event(file, code, true)?;
    }
    key_event(file, key, true)?;
    key_event(file, key, false)?;
    for &code in held.iter().rev() {
        key_event(file, code, false)?;
    }
    Ok(())
}

pub fn send(strokes: &[Stroke]) -> Result<(), Failure> {
    // Checked first: a text must not stop halfway at a letter it cannot type.
    let plan: Vec<(Vec<u16>, u16)> = strokes
        .iter()
        .map(places)
        .collect::<Option<_>>()
        .ok_or_else(|| {
            Failure::Unavailable("this text has characters outside a US keyboard".into())
        })?;
    let mut device = lock();
    if device.is_none() {
        *device = Some(create()?);
    }
    let Some(file) = device.as_mut() else {
        return Err(Failure::Unavailable(ACCESS_HINT.into()));
    };
    for (held, key) in plan {
        if let Err(e) = press(file, &held, key) {
            // Next time starts with a fresh device.
            *device = None;
            return Err(Failure::Failed(e.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::keys::{self, CTRL};
    use super::*;

    #[test]
    fn events_have_the_kernel_layout() {
        let e = event(EV_KEY, 30, 1);
        assert_eq!(e.len(), TIME_BYTES + 8);
        assert_eq!(e[TIME_BYTES..TIME_BYTES + 2], EV_KEY.to_ne_bytes());
        assert_eq!(e[TIME_BYTES + 2..TIME_BYTES + 4], 30u16.to_ne_bytes());
        assert_eq!(e[TIME_BYTES + 4..], 1i32.to_ne_bytes());
    }

    #[test]
    fn setup_matches_the_kernel_struct() {
        assert_eq!(size_of::<Setup>(), 92);
    }

    #[test]
    fn capitals_press_shift() {
        let strokes = keys::text("aB");
        assert_eq!(places(&strokes[0]), Some((vec![], 30)));
        assert_eq!(places(&strokes[1]), Some((vec![42], 48)));
    }

    #[test]
    fn shift_is_held_once() {
        let stroke = Stroke {
            mods: vec![CTRL, SHIFT],
            key: Key::for_char('+').unwrap(),
        };
        assert_eq!(places(&stroke), Some((vec![29, 42], 13)));
    }

    #[test]
    fn symbols_without_a_place_are_refused_up_front() {
        assert_eq!(places(&keys::text("я")[0]), None);
        assert!(matches!(
            send(&keys::text("aя")),
            Err(Failure::Unavailable(_))
        ));
    }
}

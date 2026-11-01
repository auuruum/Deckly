//! Application bundles in the user, local and system Applications folders.

use super::{Application, Catalog, walk};
use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub(super) fn discover() -> Catalog {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots.extend(["/Applications", "/System/Applications"].map(PathBuf::from));
    discover_in(&roots)
}

fn discover_in(roots: &[PathBuf]) -> Catalog {
    let mut catalog = Catalog::default();
    let mut budget = 20_000;
    for root in roots {
        walk(
            root,
            &mut |path| {
                if path
                    .extension()
                    .is_none_or(|e| !e.eq_ignore_ascii_case("app"))
                {
                    return true;
                }
                if path.is_dir()
                    && let (Some(name), Some(target)) =
                        (path.file_stem().and_then(|v| v.to_str()), path.to_str())
                {
                    catalog.add(Application {
                        name: name.into(),
                        target: target.into(),
                        detail: path
                            .parent()
                            .map_or_else(String::new, |p| p.display().to_string()),
                        keywords: String::new(),
                        icon: target.into(),
                    });
                }
                // Do not expose helper applications inside a bundle.
                false
            },
            &mut budget,
        );
    }
    catalog.truncated |= budget == 0;
    catalog
}

#[cfg(target_os = "macos")]
pub(super) fn icon(target: &str) -> Option<image::RgbaImage> {
    use objc2::{AnyThread, rc::autoreleasepool};
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_core_graphics::CGImage;
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

    autoreleasepool(|_| {
        let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(target));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(64.0, 64.0));
        // SAFETY: rect is a live writable rectangle; no graphics context or
        // hints are supplied. The retained image stays on this worker thread.
        let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None)? };
        if CGImage::width(Some(&cg)) > 2048 || CGImage::height(Some(&cg)) > 2048 {
            return None;
        }
        let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
        // SAFETY: an empty, correctly typed properties dictionary uses the
        // system encoder defaults; bitmap and returned data stay alive here.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )?
        };
        if png.len() > 4 * 1024 * 1024 {
            return None;
        }
        // SAFETY: AppKit returned immutable NSData, retained for the entire
        // decode; nothing mutates its bytes while the borrowed slice is used.
        super::icons::decode(unsafe { png.as_bytes_unchecked() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_utilities_but_does_not_descend_into_bundles() {
        let root = std::env::temp_dir().join(format!("deckly-bundles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Music Player.app/Contents/Helper.app")).unwrap();
        std::fs::create_dir_all(root.join("Utilities/Terminal.app")).unwrap();
        let c = discover_in(std::slice::from_ref(&root));
        assert_eq!(c.applications.len(), 2);
        assert!(c.applications.iter().any(|a| a.name == "Music Player"));
        assert!(c.applications.iter().any(|a| a.name == "Terminal"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

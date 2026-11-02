//! macOS: the shared workspace posts a notification when an application
//! becomes active. It reaches the main thread, whose run loop the app keeps
//! running, also with the window closed to the tray.

use super::Report;
use block2::RcBlock;
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::NSNotification;
use std::ptr::NonNull;

pub fn watch(report: Report) {
    let workspace = NSWorkspace::sharedWorkspace();
    let notified = report.clone();
    let activated = RcBlock::new(move |_: NonNull<NSNotification>| {
        let names = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map_or_else(Vec::new, |app| names(&app));
        notified(Ok(names));
    });
    // SAFETY: the name is a constant of AppKit, set when the library loads.
    let name = unsafe { NSWorkspaceDidActivateApplicationNotification };
    // SAFETY: no object or queue is given, so the block runs where the
    // notification is posted, on the main thread. It ignores its argument
    // and the center keeps a copy of it.
    let observer = unsafe {
        workspace
            .notificationCenter()
            .addObserverForName_object_queue_usingBlock(Some(name), None, None, &activated)
    };
    // The center stops calling the block when the observer goes. It stays
    // for as long as Deckly runs.
    std::mem::forget(observer);
    // Read after registering, so a change between the two is not lost.
    if let Some(app) = workspace.frontmostApplication() {
        report(Ok(names(&app)));
    }
}

fn names(app: &NSRunningApplication) -> Vec<String> {
    let path = |url: Option<objc2::rc::Retained<objc2_foundation::NSURL>>| {
        url.and_then(|url| url.path()).map(|path| path.to_string())
    };
    [
        app.bundleIdentifier().map(|id| id.to_string()),
        path(app.executableURL()),
        path(app.bundleURL()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

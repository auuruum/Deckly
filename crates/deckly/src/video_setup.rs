//! The "Turn on video import" window, shown when video import needs FFmpeg
//! 7 or newer and finds none. It offers four routes: install through the
//! system's package manager, copy that command, download FFmpeg for Deckly,
//! or choose an FFmpeg already on this computer. After each route Deckly
//! looks for FFmpeg again and says which version it found.
//!
//! Every process and download runs on a worker thread; nothing goes to the
//! network until the user clicks Install or Download.

use crate::ffmpeg::process::Job;
use crate::ffmpeg::system::{Plan, Repository};
use crate::ffmpeg::{self, Found, Missing, Source, pins};
use crate::image_editor::Editor;
use crate::logo;
use crate::theme::*;
use eframe::egui::{self, Color32, RichText, Ui, vec2};
use egui_phosphor::regular as icon;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

const SIZE: egui::Vec2 = vec2(640.0, 720.0);

/// A key editor waiting for FFmpeg before it asks for a video.
pub struct Pending {
    pub editor: Editor,
    pub profile: String,
}

/// What the app does for the window.
pub enum Event {
    /// FFmpeg is ready: ask for the video and convert it with `ffmpeg`.
    Import {
        pending: Box<Pending>,
        ffmpeg: PathBuf,
    },
    /// Remember this FFmpeg in the settings.
    Choose(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    System,
    Download,
}

#[derive(Clone, PartialEq, Eq)]
enum Lookup {
    /// Before video import: open the window only if FFmpeg is missing.
    Gate,
    /// Look again and show the result.
    Recheck,
    /// Check an FFmpeg the user chose.
    Chosen(PathBuf),
}

type LookupResult = Result<Found, Missing>;

#[derive(Default)]
pub struct VideoSetup {
    open: bool,
    lookup: Option<(Lookup, Receiver<LookupResult>)>,
    status: Option<LookupResult>,
    plan_rx: Option<Receiver<Option<Plan>>>,
    /// `Some(None)`: no supported package manager.
    plan: Option<Option<Plan>>,
    job: Option<(Route, Job)>,
    /// The finished job's outcome was handled.
    job_reported: bool,
    /// The outcome of the last route; `true` for an error.
    message: Option<(String, bool)>,
    pending: Option<Pending>,
}

fn repaint_fn(ctx: &egui::Context) -> Arc<dyn Fn() + Send + Sync> {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

/// Runs `work` on a worker thread and returns where its result arrives.
fn background<T: Send + 'static>(
    ctx: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("deckly-ffmpeg-lookup".into())
        .spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    // Without a thread the receiver reports a disconnect, which is shown.
    drop(spawned);
    rx
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

impl VideoSetup {
    /// Video import was chosen: imports at once when FFmpeg is ready,
    /// otherwise opens the window.
    pub fn begin_import(&mut self, ctx: &egui::Context, pending: Pending, chosen: Option<PathBuf>) {
        if self.job_running() {
            self.pending = Some(pending);
            self.open = true;
            return;
        }
        self.pending = Some(pending);
        self.look(ctx, Lookup::Gate, chosen);
    }

    /// Opens the window from Preferences.
    pub fn open(&mut self, ctx: &egui::Context, chosen: Option<PathBuf>) {
        if !self.open {
            self.message = None;
            self.open = true;
            self.look(ctx, Lookup::Recheck, chosen);
            self.plan_system(ctx);
        }
        ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Focus);
    }

    fn look(&mut self, ctx: &egui::Context, lookup: Lookup, chosen: Option<PathBuf>) {
        let what = lookup.clone();
        let rx = background(ctx, move || match what {
            Lookup::Chosen(path) => ffmpeg::locate_in(
                vec![(Source::Chosen, path)],
                &|p: &Path| p.is_file(),
                &ffmpeg::probe,
            ),
            _ => ffmpeg::locate(chosen.as_deref()),
        });
        self.lookup = Some((lookup, rx));
    }

    fn plan_system(&mut self, ctx: &egui::Context) {
        if self.plan.is_none() && self.plan_rx.is_none() {
            self.plan_rx = Some(background(ctx, ffmpeg::system::plan));
        }
    }

    fn job_running(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|(_, job)| job.outcome().is_none())
    }

    fn busy(&self) -> bool {
        self.job_running() || self.lookup.is_some()
    }

    /// Runs every frame. `chosen` is the FFmpeg path from the settings.
    pub fn show(&mut self, ctx: &egui::Context, chosen: Option<&Path>) -> Option<Event> {
        let mut event = self.poll(ctx, chosen);
        if !self.open {
            return event;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Turn on video import - Deckly")
            .with_icon(logo::window_icon())
            .with_inner_size(SIZE)
            .with_min_inner_size(SIZE)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(false);
        let mut close = false;
        ctx.show_viewport_immediate(viewport_id(), builder, |ui, _class| {
            close |= ui.ctx().input(|i| i.viewport().close_requested());
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(PREF_BODY).inner_margin(20))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let (closed, chose) = self.contents(ui, chosen);
                            close |= closed;
                            if chose.is_some() {
                                event = chose;
                            }
                        });
                });
        });
        if self.job_running() || self.lookup.is_some() || self.plan_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if close {
            self.close();
        }
        event
    }

    fn close(&mut self) {
        // A running install or download is stopped; one that cannot be
        // stopped finishes on its own.
        if let Some((_, job)) = &self.job
            && job.outcome().is_none()
        {
            job.cancel();
        }
        *self = Self {
            plan: self.plan.take(),
            ..Self::default()
        };
    }

    fn poll(&mut self, ctx: &egui::Context, chosen: Option<&Path>) -> Option<Event> {
        let mut event = None;
        if let Some((lookup, rx)) = &self.lookup {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(Missing::Unusable {
                    path: PathBuf::new(),
                    error: "the check stopped unexpectedly.".into(),
                })),
            };
            if let Some(result) = result {
                let lookup = lookup.clone();
                self.lookup = None;
                event = self.looked(ctx, lookup, result);
            }
        }
        if let Some(rx) = &self.plan_rx {
            match rx.try_recv() {
                Ok(plan) => {
                    self.plan = Some(plan);
                    self.plan_rx = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.plan = Some(None);
                    self.plan_rx = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let finished = self
            .job
            .as_ref()
            .filter(|_| !self.job_reported)
            .and_then(|(_, job)| job.outcome());
        if let Some(outcome) = finished {
            self.job_reported = true;
            match outcome {
                Ok(text) => {
                    self.message = Some((text, false));
                    self.look(ctx, Lookup::Recheck, chosen.map(Path::to_path_buf));
                }
                Err(error) => self.message = Some((error, true)),
            }
        }
        event
    }

    fn looked(
        &mut self,
        ctx: &egui::Context,
        lookup: Lookup,
        result: LookupResult,
    ) -> Option<Event> {
        match lookup {
            Lookup::Gate => match result {
                Ok(found) => {
                    let pending = self.pending.take()?;
                    return Some(Event::Import {
                        pending: Box::new(pending),
                        ffmpeg: found.path,
                    });
                }
                Err(missing) => {
                    self.status = Some(Err(missing));
                    self.message = None;
                    self.open = true;
                    self.plan_system(ctx);
                }
            },
            Lookup::Recheck => {
                if let (Err(missing), Some((_, false))) = (&result, &self.message) {
                    // The route ran, but there is still no usable FFmpeg.
                    self.message = Some((
                        format!("That did not make video import work. {}", missing.message()),
                        true,
                    ));
                }
                self.status = Some(result);
            }
            Lookup::Chosen(path) => match result {
                Ok(found) => {
                    self.message = Some((
                        format!(
                            "Deckly will use FFmpeg {} at {}.",
                            found.version,
                            path.display()
                        ),
                        false,
                    ));
                    self.status = Some(Ok(found));
                    return Some(Event::Choose(path));
                }
                Err(missing) => self.message = Some((missing.message(), true)),
            },
        }
        None
    }

    /// Draws the window's contents. Returns whether it was closed, and an
    /// event for the app.
    fn contents(&mut self, ui: &mut Ui, chosen: Option<&Path>) -> (bool, Option<Event>) {
        let mut event = None;
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new("Turn on video import").size(20.0).color(TEXT));
        ui.label(
            RichText::new(format!(
                "Deckly turns a video into an animated key with FFmpeg {} or newer. \
                 FFmpeg runs as a separate program and is only needed while importing.",
                ffmpeg::version::MINIMUM_MAJOR
            ))
            .color(TEXT_DIM),
        );
        ui.add_space(4.0);
        self.status_line(ui);
        ui.add_space(4.0);
        ui.separator();

        let busy = self.busy();
        self.system_route(ui, busy);
        ui.separator();
        self.download_route(ui, busy);
        ui.separator();
        section(
            ui,
            icon::FOLDER_OPEN,
            "Use an FFmpeg on this computer",
            false,
        );
        if let Some(path) = chosen {
            dim(ui, &format!("Chosen in Preferences: {}", path.display()));
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Choose ffmpeg…"))
            .clicked()
            && let Some(path) = pick_ffmpeg()
        {
            self.message = None;
            self.look(ui.ctx(), Lookup::Chosen(path), None);
        }

        self.job_area(ui);
        if let Some((text, error)) = &self.message {
            ui.add_space(4.0);
            let (glyph, color) = if *error {
                (icon::WARNING, WARNING)
            } else {
                (icon::CHECK_CIRCLE, SUCCESS)
            };
            ui.label(RichText::new(format!("{glyph}  {text}")).color(color));
        }

        ui.add_space(8.0);
        let mut close = false;
        ui.horizontal(|ui| {
            let ready = matches!(self.status, Some(Ok(_)));
            if ready
                && self.pending.is_some()
                && let Some(Ok(found)) = &self.status
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(format!("{}  Choose Video…", icon::FILM_STRIP)),
                    )
                    .clicked()
                && let Some(pending) = self.pending.take()
            {
                event = Some(Event::Import {
                    pending: Box::new(pending),
                    ffmpeg: found.path.clone(),
                });
                close = true;
            }
            let label = if self.job_running() {
                "Cancel and Close"
            } else {
                "Close"
            };
            close |= ui.button(label).clicked();
        });
        (close, event)
    }

    fn status_line(&self, ui: &mut Ui) {
        if self.lookup.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for FFmpeg…");
            });
            return;
        }
        match &self.status {
            Some(Ok(found)) => {
                let text = format!(
                    "{}  FFmpeg {} is ready: {}",
                    icon::CHECK_CIRCLE,
                    found.version,
                    found.path.display()
                );
                ui.label(RichText::new(text).color(SUCCESS));
            }
            Some(Err(missing)) => {
                let text = format!("{}  {}", icon::WARNING, missing.message());
                ui.label(RichText::new(text).color(WARNING));
            }
            None => {}
        }
    }

    fn recommended(&self) -> Option<Route> {
        let download = pins::for_this_system().is_some() && ffmpeg::own_dir().is_some();
        recommend(self.plan.as_ref(), download)
    }

    fn system_route(&mut self, ui: &mut Ui, busy: bool) {
        let plan = match &self.plan {
            None => {
                section(ui, icon::PACKAGE, "Install through the system", false);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking your package manager…");
                });
                return;
            }
            Some(None) => {
                section(ui, icon::PACKAGE, "Install through the system", false);
                dim(
                    ui,
                    "Deckly found no package manager it knows (apt, dnf, pacman, zypper, \
                     winget or Homebrew).",
                );
                return;
            }
            Some(Some(plan)) => plan.clone(),
        };
        let recommended = self.recommended() == Some(Route::System);
        section(
            ui,
            icon::PACKAGE,
            &format!("Install through the system with {}", plan.manager.label()),
            recommended,
        );
        if recommended {
            dim(
                ui,
                "Other programs can use this FFmpeg too, and your system keeps it updated.",
            );
        }
        match &plan.repository {
            Repository::Has(v) => dim(ui, &format!("Your repositories have FFmpeg {v}.")),
            Repository::NotChecked => dim(
                ui,
                &format!(
                    "Installs the {} package; Deckly checks its version afterwards.",
                    plan.package
                ),
            ),
            _ => {}
        }
        match plan.shown_command() {
            Some(shown) => {
                command_box(ui, &shown);
                dim(
                    ui,
                    "Your system asks for administrator rights in its own window when it \
                     needs them. Deckly never sees your password.",
                );
                if route_button(ui, !busy, recommended, "Install")
                    .on_hover_text("Run the command above")
                    .clicked()
                {
                    self.start_install(ui.ctx(), &plan);
                }
            }
            None => {
                let reason = plan.install.as_ref().err().map_or("", String::as_str);
                ui.label(RichText::new(reason).color(WARNING));
            }
        }

        ui.separator();
        section(ui, icon::TERMINAL_WINDOW, "Copy the command", false);
        dim(ui, "Run it yourself in a terminal:");
        command_box(ui, &plan.terminal);
        if ui.button(format!("{}  Copy", icon::COPY)).clicked() {
            ui.ctx().copy_text(plan.terminal.clone());
            self.message = Some(("The command is on the clipboard.".into(), false));
        }
    }

    fn download_route(&mut self, ui: &mut Ui, busy: bool) {
        let recommended = self.recommended() == Some(Route::Download);
        section(
            ui,
            icon::DOWNLOAD_SIMPLE,
            "Download for Deckly",
            recommended,
        );
        let Some(pin) = pins::for_this_system() else {
            dim(ui, pins::unavailable_reason());
            return;
        };
        let Some(dir) = ffmpeg::own_dir() else {
            dim(ui, "Deckly has no data folder to download into.");
            return;
        };
        let host = pin.url.split('/').nth(2).unwrap_or("github.com");
        dim(
            ui,
            &format!(
                "FFmpeg {} ({}), built by the {} project. Downloads {} from {host} into {}. \
                 Needs no administrator rights; the download is checked against its pinned \
                 SHA-256.",
                pins::VERSION,
                pins::LICENSE,
                pins::PROJECT,
                megabytes(pin.size),
                dir.display()
            ),
        );
        ui.horizontal(|ui| {
            let text = format!("Download {}", megabytes(pin.size));
            if route_button(ui, !busy, recommended, &text).clicked() {
                self.start_download(ui.ctx(), pin, dir.clone());
            }
            ui.hyperlink_to(
                "Build, source and license",
                format!("{}/releases/tag/{}", pins::PROJECT_PAGE, pins::RELEASE),
            );
        });
    }

    fn job_area(&mut self, ui: &mut Ui) {
        let Some((route, job)) = &self.job else {
            return;
        };
        ui.separator();
        let running = job.outcome().is_none();
        let title = match (route, running) {
            (Route::System, true) => "Installing…",
            (Route::Download, true) => "Downloading…",
            (Route::System, false) => "Install output",
            (Route::Download, false) => "Download log",
        };
        ui.label(RichText::new(title).strong().color(TEXT));
        if *route == Route::Download
            && running
            && let Some((done, total)) = job.progress()
            && total > 0
        {
            let fraction = done as f32 / total as f32;
            ui.add(egui::ProgressBar::new(fraction).text(format!(
                "{} of {}",
                megabytes(done),
                megabytes(total)
            )));
        }
        egui::Frame::new()
            .fill(PREF_PANEL)
            .stroke(egui::Stroke::new(1.0, PREF_PANEL_BORDER))
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("video-setup-log")
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        job.with_log(|log| {
                            for line in log.lines() {
                                ui.label(
                                    RichText::new(line).monospace().size(11.0).color(TEXT_DIM),
                                );
                            }
                        });
                    });
            });
        if running {
            let cancelling = job.cancelling();
            let label = if cancelling {
                "Cancelling…"
            } else {
                "Cancel"
            };
            if ui
                .add_enabled(!cancelling, egui::Button::new(label))
                .clicked()
            {
                job.cancel();
            }
        }
    }

    fn start_install(&mut self, ctx: &egui::Context, plan: &Plan) {
        let Some(mut command) = plan.command() else {
            return;
        };
        let shown = plan.shown_command().unwrap_or_default();
        let label = plan.manager.label();
        self.message = None;
        let job = Job::start(repaint_fn(ctx), move |reporter| {
            reporter.note(&format!("$ {shown}"));
            // Keep the window's own environment out of the way of tools
            // that read it, but keep what the system's prompt needs.
            command.env_remove("FFREPORT");
            ffmpeg::process::stream(command, reporter)?;
            Ok(format!("{label} finished installing FFmpeg."))
        });
        self.job = Some((Route::System, job));
        self.job_reported = false;
    }

    fn start_download(&mut self, ctx: &egui::Context, pin: &'static pins::Pin, dir: PathBuf) {
        self.message = None;
        let job = Job::start(repaint_fn(ctx), move |reporter| {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
            let path = ffmpeg::download::install(pin, &dir, reporter)?;
            Ok(format!(
                "FFmpeg {} is in {}.",
                pins::VERSION,
                path.display()
            ))
        });
        self.job = Some((Route::Download, job));
        self.job_reported = false;
    }
}

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("video-setup")
}

fn section(ui: &mut Ui, glyph: &str, title: &str, recommended: bool) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{glyph}  {title}"))
                .size(15.0)
                .strong()
                .color(TEXT),
        );
        if recommended {
            egui::Frame::new()
                .fill(ACCENT)
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Recommended")
                            .size(11.0)
                            .color(Color32::WHITE),
                    );
                });
        }
    });
}

/// The route's main button; the recommended one stands out.
fn route_button(ui: &mut Ui, enabled: bool, recommended: bool, text: &str) -> egui::Response {
    let button = if recommended {
        egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(ACCENT)
    } else {
        egui::Button::new(text)
    };
    ui.add_enabled(enabled, button)
}

/// Which route the window recommends: the package manager when it can
/// install FFmpeg 7+, since other programs can use that FFmpeg too and the
/// system keeps it updated; otherwise Deckly's own download. Nothing while
/// the package manager is still being checked.
fn recommend(plan: Option<&Option<Plan>>, download: bool) -> Option<Route> {
    match plan? {
        Some(plan) if plan.install.is_ok() => Some(Route::System),
        _ if download => Some(Route::Download),
        _ => None,
    }
}

fn dim(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(TEXT_DIM));
}

fn command_box(ui: &mut Ui, command: &str) {
    egui::Frame::new()
        .fill(PREF_FIELD)
        .corner_radius(4)
        .inner_margin(6)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut text = command;
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(1)
                    .desired_width(f32::INFINITY)
                    .frame(egui::Frame::NONE)
                    .text_color(Color32::from_rgb(0xE6, 0xE6, 0xE6)),
            );
        });
}

/// Asks for an FFmpeg program.
fn pick_ffmpeg() -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new().set_title("Choose the ffmpeg program");
    let dialog = if cfg!(windows) {
        dialog.add_filter("ffmpeg", &["exe"])
    } else {
        dialog
    };
    dialog.pick_file().filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_route_is_recommended_when_it_can_install() {
        use crate::ffmpeg::system::{Manager, plan_with};
        use crate::os::elevate::Os;
        let brew = Path::new("/opt/homebrew/bin/brew");
        let installs = Some(plan_with(Os::Mac, Manager::Brew, brew, None, None));
        assert_eq!(recommend(Some(&installs), true), Some(Route::System));
        assert_eq!(recommend(Some(&installs), false), Some(Route::System));
        // Without pkexec only Copy works, so the download is recommended.
        let apt = Path::new("/usr/bin/apt-get");
        let blocked = Some(plan_with(
            Os::Linux,
            Manager::Apt,
            apt,
            Some(Err("x".into())),
            None,
        ));
        assert_eq!(recommend(Some(&blocked), true), Some(Route::Download));
        assert_eq!(recommend(Some(&blocked), false), None);
        assert_eq!(recommend(Some(&None), true), Some(Route::Download));
        // Still checking the package manager: no recommendation yet.
        assert_eq!(recommend(None, true), None);
    }

    #[test]
    fn sizes_are_shown_in_whole_megabytes() {
        assert_eq!(megabytes(136_027_168), "137 MB");
        assert_eq!(megabytes(1), "1 MB");
        assert_eq!(megabytes(0), "0 MB");
    }
}

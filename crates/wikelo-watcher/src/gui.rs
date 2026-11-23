//! The settings window: what opens when `wikelink` runs without arguments.
//!
//! One screen with everything a player needs: the account on the server (sign in / out),
//! start / stop of the background watcher, the game channel it follows, "start with
//! Windows", the game-data upload switch, and what the watcher is doing right now. The
//! window never does the watching itself: it drives the same background process the
//! command line uses and reads the small status file that process keeps current.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::agent::{self, AgentConfig, DaemonStatus};
use crate::i18n::{self, Lang};
use crate::palette::{ACCENT, BAD, BORDER, MUTED, NAVY, NAVY_DEEP, NAVY_RAISED, OK, TEXT, WARN};
use crate::update;
use crate::{tr, trf};

/// Lines of the watcher log shown in the window.
const LOG_TAIL_BYTES: u64 = 4000;
/// Window height: just the sign-in box, or every panel once signed in.
const SIZE_SIGNED_OUT: [f32; 2] = [560.0, 360.0];
const SIZE_SIGNED_IN: [f32; 2] = [560.0, 820.0];

/// Result of a background action (network or waiting for the process).
enum Done {
    /// The site gave a code for sign-in attempt number `.0`, or why not.
    LinkStarted(u64, Result<agent::LinkCode, agent::LinkError>),
    /// Sign-in attempt number `.0` ended (from its polling thread, not `App::spawn`).
    Linked(u64, agent::LinkOutcome),
    /// Saved session re-checked at start: display name when still valid.
    Session(Result<Option<String>, String>),
    Started(Result<u32, String>),
    Stopped(Result<bool, String>),
    Autostart(bool, Result<(), String>),
    /// The sign-out ended: the saved session was removed, or why it could not be (then this
    /// PC is still signed in).
    LoggedOut(Result<(), String>),
    /// A newer release the server knows of: (version, download page).
    Update(Option<(String, String)>),
    /// The overlay was opened or closed.
    Overlay(Result<(), String>),
    /// Where a confirmed update is (the text of `update::Stage`).
    UpdateProgress(String),
    /// The update ended: done, or why not.
    Updated(Result<update::Finished, String>),
}

/// The session token of a sign-in that ended for attempt `attempt` while `current` is the
/// window's current attempt: `Some` only for a session whose attempt was cancelled or replaced
/// since, which the window does not keep. Pure.
fn orphan_token(attempt: u64, current: u64, outcome: &agent::LinkOutcome) -> Option<String> {
    match outcome {
        agent::LinkOutcome::SignedIn(s) if attempt != current => Some(s.token.clone()),
        _ => None,
    }
}

/// The "Update" button of the new-version banner, step by step. Nothing is downloaded before
/// the player chooses "Update now" in `Confirm`.
#[derive(Clone)]
enum UpdateStep {
    /// The banner offers the button.
    Offer,
    /// The confirmation: versions and source, "Update now" / "Cancel".
    Confirm,
    /// Downloading, checking, replacing (the stage is in `busy`).
    Running,
    /// It failed, and why.
    Failed(String),
    /// The file was replaced, but this window stays open: what the player should know.
    Finished(String),
}

pub fn run() -> anyhow::Result<()> {
    let signed_in = agent::load_config().is_some_and(|c| !c.token.is_empty() && !c.server.is_empty());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Wikelink")
            .with_inner_size(if signed_in { SIZE_SIGNED_IN } else { SIZE_SIGNED_OUT })
            .with_min_inner_size([480.0, 300.0])
            .with_icon(std::sync::Arc::new(icon())),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        // eframe's app id (its storage), not a visible text: unchanged by the rename.
        "Wikelo Watcher",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!(trf!("could not open the window: {e}", "não foi possível abrir a janela: {e}")))
}

/// The site's mark (web/public/favicon.svg) at 32 px, straight RGBA; the window's and the tray's icon.
/// `assets/wikelist.ico` holds the same drawing for the executable (see build.rs).
pub(crate) fn icon() -> egui::IconData {
    const N: u32 = 32;
    egui::IconData { rgba: include_bytes!("../assets/icon-32.rgba").to_vec(), width: N, height: N }
}

pub(crate) fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = NAVY;
    v.window_fill = v.panel_fill;
    v.extreme_bg_color = NAVY_DEEP;
    v.faint_bg_color = NAVY_RAISED;
    v.widgets.noninteractive.bg_fill = NAVY_RAISED;
    v.widgets.noninteractive.bg_stroke.color = BORDER;
    v.widgets.noninteractive.fg_stroke.color = TEXT;
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x15, 0x22, 0x30);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x15, 0x22, 0x30);
    v.widgets.inactive.fg_stroke.color = TEXT;
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x1c, 0x2e, 0x40);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x1c, 0x2e, 0x40);
    v.widgets.hovered.bg_stroke.color = ACCENT;
    v.widgets.active.bg_fill = Color32::from_rgb(0x15, 0x4a, 0x60);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x15, 0x4a, 0x60);
    v.selection.bg_fill = Color32::from_rgb(0x12, 0x4a, 0x5e);
    v.selection.stroke.color = ACCENT;
    v.hyperlink_color = ACCENT;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.interact_size.y = 26.0;
    });
}

/// A sign-in through the site waiting for the player's confirmation in the browser.
struct Linking {
    /// What the player compares with the site.
    code: String,
    /// `<server>/link` (see `agent::link_page`), for "Open the page again".
    page: String,
    /// Set by "Cancel": the polling thread stops at its next look.
    cancel: Arc<AtomicBool>,
    /// When "Copy code" last put the code on the clipboard ("copied" shows for a moment).
    copied: Option<Instant>,
}

/// How long "copied" shows after "Copy code".
const COPIED_FOR: Duration = Duration::from_secs(2);

struct App {
    cfg: AgentConfig,
    /// The sign-in through the site under way, once the site gave its code.
    linking: Option<Linking>,
    /// Number of the latest sign-in attempt: what comes back for an older (cancelled) one is
    /// ignored.
    link_attempt: u64,
    installs: Vec<wikelo_install::InstallInfo>,
    running: Option<u32>,
    /// The overlay is on (its mutex exists): over the game, or waiting for it.
    overlay_open: bool,
    /// This window started the running overlay with `--preview`.
    overlay_preview: bool,
    /// The game's window is there (looked at only while the overlay is on).
    game_up: bool,
    autostart: bool,
    status: Option<DaemonStatus>,
    log_tail: String,
    last_poll: Instant,
    /// What a background action is doing right now (disables the buttons).
    busy: Option<String>,
    /// (is error, text) shown under the account box.
    msg: Option<(bool, String)>,
    /// A newer release than this executable, as the daemon or the start-up check reported it.
    update: Option<(String, String)>,
    update_step: UpdateStep,
    /// The update started the new version's window: this one closes.
    quit: bool,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    /// Signed-in state the window is currently sized for.
    sized_for: bool,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = channel();
        let cfg = agent::load_config().unwrap_or_default();
        let sized_for = !cfg.token.is_empty() && !cfg.server.is_empty();
        // "Start with Windows" set by `wikelo-watcher.exe` (before the rename) moves to this
        // executable, so the old program no longer starts at sign-in next to this one.
        let migrated = agent::migrate_autostart().map_err(|e| format!("{e:#}"));
        let mut app = Self {
            cfg,
            linking: None,
            link_attempt: 0,
            installs: wikelo_install::discover_installs(&[]).into_iter().filter(|i| i.game_log_path.exists() || i.has_p4k()).collect(),
            running: None,
            overlay_open: false,
            overlay_preview: false,
            game_up: false,
            autostart: agent::autostart_enabled(),
            status: None,
            log_tail: String::new(),
            last_poll: Instant::now() - Duration::from_secs(10),
            busy: None,
            msg: None,
            update: None,
            update_step: UpdateStep::Offer,
            quit: false,
            tx,
            rx,
            sized_for,
        };
        if let Err(e) = migrated {
            app.msg = Some((true, e));
        }
        app.poll();
        if app.signed_in() {
            // Saved session: make sure the server still accepts it, and ask it for the newest
            // release while there (the daemon does the same daily; this covers a stopped one).
            let cfg = app.cfg.clone();
            let tx = app.tx.clone();
            app.busy = Some(tr!("checking the session…", "verificando a sessão…").into());
            std::thread::spawn(move || {
                let _ = tx.send(Done::Session(agent::session_name(&cfg).map_err(|e| format!("{e:?}"))));
                let _ = tx.send(Done::Update(agent::update_available(&cfg)));
            });
        }
        app
    }

    fn signed_in(&self) -> bool {
        !self.cfg.token.is_empty() && !self.cfg.server.is_empty()
    }

    /// Re-read what the background process left on disk (cheap: three small files).
    fn poll(&mut self) {
        self.last_poll = Instant::now();
        self.running = agent::running_pid();
        let overlay_open = agent::overlay_running();
        if self.overlay_open && !overlay_open {
            self.overlay_preview = false;
        }
        self.overlay_open = overlay_open;
        self.game_up = overlay_open && crate::overlay_win::game_window().is_some();
        self.status =if self.running.is_some() { agent::read_status() } else { None };
        // Only a release newer than this executable: an older program still running in the
        // background may report the very version this one is.
        if let Some(s) = &self.status
            && let (Some(v), Some(u)) = (&s.latest_version, &s.update_url)
            && wikelo_core::version::is_newer(v, wikelo_core::PKG_VERSION)
        {
            self.update = Some((v.clone(), u.clone()));
        }
        self.log_tail = tail(&agent::log_path(), LOG_TAIL_BYTES);
    }

    fn drain(&mut self) {
        while let Ok(done) = self.rx.try_recv() {
            let before = self.busy.take();
            match done {
                Done::LinkStarted(attempt, started) => {
                    if attempt == self.link_attempt {
                        match started {
                            Ok(link) => self.follow_link(attempt, link),
                            Err(e) => self.msg = Some((true, e.to_string())),
                        }
                    }
                }
                Done::Linked(attempt, outcome) => {
                    // Not from `spawn`: whatever else is busy right now stays so.
                    self.busy = before;
                    if let Some(token) = orphan_token(attempt, self.link_attempt, &outcome) {
                        // A session for an attempt cancelled or replaced since: it is not kept
                        // here, so it is ended on the server too (best effort, in the
                        // background), then dropped.
                        std::thread::spawn(move || {
                            let _ = agent::sign_out_token(&token);
                        });
                    }
                    if attempt == self.link_attempt {
                        self.linking = None;
                        match outcome {
                            agent::LinkOutcome::SignedIn(s) => self.signed_in_as(s),
                            other => {
                                let is_err = other != agent::LinkOutcome::Cancelled;
                                self.msg = Some((is_err, other.to_string()));
                            }
                        }
                    }
                }
                Done::Session(Ok(Some(name))) => {
                    if self.cfg.name.as_deref() != Some(&name) {
                        self.cfg.name = Some(name);
                        let mine = &self.cfg;
                        // Only for the account it checked: a sign-in since then keeps its name.
                        let _ = agent::update_config(mine, |d| {
                            if d.token == mine.token {
                                d.name = mine.name.clone();
                            }
                        });
                    }
                }
                Done::Session(Ok(None)) => {
                    self.cfg.token.clear();
                    self.msg = Some((true, tr!("the session expired; sign in again", "a sessão expirou; entre de novo").into()));
                }
                Done::Session(Err(e)) => {
                    let server = agent::server_address();
                    self.msg = Some((true, trf!("no contact with {server}: {e}", "sem contato com {server}: {e}")));
                }
                Done::Started(Ok(_)) => self.msg = None,
                Done::Started(Err(e)) => self.msg = Some((true, e)),
                Done::Stopped(Ok(_)) => {}
                Done::Stopped(Err(e)) => self.msg = Some((true, e)),
                Done::Autostart(on, Ok(())) => self.autostart = on,
                Done::Autostart(_, Err(e)) => {
                    self.autostart = agent::autostart_enabled();
                    self.msg = Some((true, e));
                }
                Done::LoggedOut(Ok(())) => {
                    // The language outlives the sign-out (see `agent::remove_config`).
                    self.cfg = AgentConfig { handle: self.cfg.handle.clone(), lang: self.cfg.lang.clone(), ..Default::default() };
                    self.msg = None;
                }
                // The saved session is still there: the window stays signed in and says why.
                Done::LoggedOut(Err(e)) => self.msg = Some((true, e)),
                Done::Overlay(Ok(())) => {}
                Done::Overlay(Err(e)) => self.msg = Some((true, e)),
                Done::Update(u) => {
                    if u.is_some() {
                        self.update = u;
                    }
                    // An update under way stays busy.
                    if matches!(self.update_step, UpdateStep::Running) {
                        self.busy = before;
                    }
                    // Not a user action: the spinner belongs to the session check that preceded it.
                    continue;
                }
                Done::UpdateProgress(stage) => {
                    // The update goes on: still busy, with the new stage as the label.
                    self.busy = Some(stage);
                    continue;
                }
                Done::Updated(Ok(f)) => {
                    if f.window_opened {
                        self.quit = true;
                    } else {
                        let v = &f.version;
                        self.update_step = UpdateStep::Finished(match &f.problem {
                            Some(p) => trf!("Wikelink was updated to {v}, but: {p}", "O Wikelink foi atualizado para a {v}, mas: {p}"),
                            None => trf!("Wikelink was updated to {v}: open it again.", "O Wikelink foi atualizado para a {v}: abra-o de novo."),
                        });
                    }
                }
                Done::Updated(Err(e)) => self.update_step = UpdateStep::Failed(e),
            }
            self.poll();
        }
    }

    fn spawn(&mut self, label: &str, work: impl FnOnce() -> Done + Send + 'static) {
        self.busy = Some(label.to_string());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    /// "Sign in": ask the site for a code; `follow_link` takes it from there.
    fn sign_in(&mut self) {
        self.msg = None;
        self.link_attempt += 1;
        let attempt = self.link_attempt;
        self.spawn(tr!("asking the site for a code…", "pedindo um código ao site…"), move || Done::LinkStarted(attempt, agent::link_start()));
    }

    /// The site gave a code: open its page in the browser and poll on a thread of its own,
    /// which sends `Done::Linked` at the end. The page is built from Wikelink's own address.
    fn follow_link(&mut self, attempt: u64, link: agent::LinkCode) {
        let Some(page) = agent::link_page(&link.code) else {
            self.msg = Some((true, tr!("unexpected answer from the server", "resposta inesperada do servidor").into()));
            return;
        };
        if let Err(e) = open::that_detached(&page) {
            self.msg = Some((true, trf!("could not open the browser ({e}); open {page}", "não deu para abrir o navegador ({e}); abra {page}")));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.linking = Some(Linking { code: link.code.clone(), page, cancel: cancel.clone(), copied: None });
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done::Linked(attempt, agent::wait_for_link(&link, &cancel)));
        });
    }

    /// "Cancel" while waiting for the browser: back to the button at once; the polling thread
    /// stops at its next look at the switch, and what it sends then is ignored.
    fn cancel_link(&mut self) {
        if let Some(l) = self.linking.take() {
            l.cancel.store(true, Ordering::Relaxed);
        }
        self.link_attempt += 1;
        self.msg = Some((false, agent::LinkOutcome::Cancelled.to_string()));
    }

    /// The site confirmed the sign-in: keep the session and start the watcher.
    fn signed_in_as(&mut self, s: agent::SignIn) {
        // The address the new token belongs to (see `agent::with_token_rule`).
        self.cfg.server = agent::server_address().to_string();
        self.cfg.handle = s.handle;
        self.cfg.token = s.token;
        self.cfg.name = Some(s.name);
        if self.cfg.share_data.is_none() {
            self.cfg.share_data = Some(true);
        }
        self.msg = None;
        let mine = &self.cfg;
        let written = agent::update_config(mine, |d| {
            agent::copy_sign_in(d, mine);
            if d.share_data.is_none() {
                d.share_data = mine.share_data;
            }
        });
        match written {
            Err(e) => self.msg = Some((true, trf!("could not save the settings: {e}", "não deu para gravar a configuração: {e}"))),
            Ok(saved) => {
                self.cfg = saved;
                if self.running.is_none() {
                    self.start();
                }
            }
        }
    }

    /// The background watcher brings the overlay up itself (see `agent::OverlayKeeper`).
    fn start(&mut self) {
        self.spawn(tr!("starting…", "iniciando…"), || Done::Started(agent::start_background().map_err(|e| format!("{e:#}"))));
    }

    /// `preview`: shown now, without the game (`overlay --preview`); otherwise it waits for the game.
    fn open_overlay(&mut self, preview: bool) {
        self.overlay_preview = preview;
        self.spawn(tr!("opening the overlay…", "abrindo o overlay…"), move || {
            Done::Overlay(agent::launch_overlay_with(preview).map_err(|e| trf!("could not open the overlay: {e:#}", "não deu para abrir o overlay: {e:#}")))
        });
    }

    fn close_overlay(&mut self) {
        self.spawn(tr!("closing the overlay…", "fechando o overlay…"), || Done::Overlay(agent::close_overlay().map(|_| ()).map_err(|e| format!("{e:#}"))));
    }

    fn stop(&mut self) {
        self.spawn(tr!("stopping…", "parando…"), || {
            // The overlay goes too: without the watcher it would wait for the game forever.
            let closed = agent::close_overlay().map_err(|e| format!("{e:#}"));
            let stopped = agent::stop_background().map_err(|e| format!("{e:#}"));
            Done::Stopped(stopped.and_then(|s| closed.map(|_| s)))
        });
    }

    fn restart(&mut self) {
        self.spawn(tr!("restarting…", "reiniciando…"), || {
            if let Err(e) = agent::stop_background() {
                return Done::Started(Err(format!("{e:#}")));
            }
            Done::Started(agent::start_background().map_err(|e| format!("{e:#}")))
        });
    }

    /// Replace a background watcher of another version (say `wikelo-watcher.exe` from before the
    /// rename) with this executable: its overlay goes too, since an overlay restarts itself
    /// from the file it came from; the new watcher opens this one's at its start.
    fn restart_as_this_version(&mut self) {
        self.spawn(tr!("restarting…", "reiniciando…"), || {
            if let Err(e) = agent::close_overlay() {
                return Done::Started(Err(format!("{e:#}")));
            }
            if let Err(e) = agent::stop_background() {
                return Done::Started(Err(format!("{e:#}")));
            }
            Done::Started(agent::start_background().map_err(|e| format!("{e:#}")))
        });
    }

    /// "Update now", after the confirmation: `update::install` on a thread of its own, its
    /// stages coming back as `Done::UpdateProgress`, the end as `Done::Updated`.
    fn run_update(&mut self, version: String) {
        self.update_step = UpdateStep::Running;
        let tx = self.tx.clone();
        self.spawn(&update::Stage::Downloading { done: 0, total: None }.text(), move || {
            let report = |stage: update::Stage| {
                let _ = tx.send(Done::UpdateProgress(stage.text()));
            };
            Done::Updated(update::install(&version, true, &report))
        });
    }

    fn logout(&mut self) {
        self.spawn(tr!("signing out…", "saindo…"), || {
            // Before the session goes: an overlay left waiting would come up over the game
            // later without an account.
            let _ = agent::close_overlay();
            let _ = agent::stop_background();
            // The server ends the session too (best effort, a few seconds at most); the
            // session on this PC is forgotten whatever it answers.
            if let Some(cfg) = agent::load_config() {
                let _ = agent::sign_out_on_server(&cfg);
            }
            let p = agent::config_path();
            let shown = p.display();
            Done::LoggedOut(agent::remove_config().map_err(|e| {
                trf!(
                    "could not remove the saved session ({shown}): {e}; this PC is still signed in",
                    "não deu para remover a sessão salva ({shown}): {e}; este PC continua conectado"
                )
            }))
        });
    }

    fn set_autostart(&mut self, on: bool) {
        self.autostart = on;
        self.spawn(tr!("saving…", "gravando…"), move || Done::Autostart(on, agent::set_autostart(on).map_err(|e| format!("{e:#}"))));
    }

    /// Apply `change` here and to a fresh read of watcher.json under the config lock, which
    /// is written back: only the fields `change` sets are this window's (the overlay writes its
    /// cards and a sign-in its token into the same file meanwhile). The overlay fields of this
    /// copy are then refreshed from what was written.
    fn save_with(&mut self, change: impl Fn(&mut AgentConfig)) {
        change(&mut self.cfg);
        let written = agent::update_config(&self.cfg, &change);
        match written {
            Ok(disk) => agent::keep_overlay_fields(&mut self.cfg, &disk),
            Err(e) => self.msg = Some((true, trf!("could not save the settings: {e}", "não deu para gravar a configuração: {e}"))),
        }
    }

    /// Forget where the overlay's cards are; the overlay reads it at its next start.
    fn reset_overlay_pos(&mut self) {
        match agent::reset_overlay_layout() {
            Ok(_) => {
                self.cfg.overlay_pos = None;
                self.cfg.overlay_widgets.clear();
                self.msg = Some((false, tr!("overlay cards back to their default places the next time it opens", "cartões do overlay de volta ao lugar padrão na próxima vez que ele abrir").into()));
            }
            Err(e) => self.msg = Some((true, trf!("could not save the settings: {e}", "não deu para gravar a configuração: {e}"))),
        }
    }

    /// Store the language choice (`None`: follow Windows). It applies from the next start of
    /// every Wikelink process; this window keeps its language until then.
    fn set_lang(&mut self, choice: Option<Lang>) {
        let value = choice.map(|l| match l {
            Lang::En => "en".to_string(),
            Lang::Pt => "pt".to_string(),
        });
        self.msg = None;
        self.save_with(move |c| c.lang = value.clone());
        if self.msg.is_none() {
            self.msg = Some((false, tr!("Language saved: it applies the next time Wikelink opens.", "Idioma gravado: vale na próxima vez que o Wikelink abrir.").into()));
        }
    }

    // ----- panels -------------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("WIKELINK").color(ACCENT).strong().size(18.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(b) = &self.busy {
                    ui.spinner();
                    ui.label(RichText::new(b.clone()).color(MUTED));
                } else if self.running.is_some() {
                    if ui.button(tr!("Stop", "Parar")).clicked() {
                        self.stop();
                    }
                    ui.label(RichText::new(tr!("● running in the background", "● rodando em segundo plano")).color(OK));
                } else {
                    if ui.add_enabled(self.signed_in(), egui::Button::new(tr!("Start", "Iniciar"))).clicked() {
                        self.start();
                    }
                    ui.label(RichText::new(tr!("○ stopped", "○ parado")).color(MUTED));
                }
            });
        });
        if let Some((version, url)) = self.update.clone() {
            let mine = wikelo_core::PKG_VERSION;
            // `Some` only for a plain release number newer than this one: the button's condition.
            let target = update::target_version(&version, mine).ok();
            // Only a page of this project's releases on GitHub; anything else: no button. Without
            // one from the server, the release's own page (it is the way to update by hand).
            let link = agent::openable_url(&url, agent::Link::Release).or_else(|| target.as_deref().and_then(|v| agent::openable_url(&update::release_page(v), agent::Link::Release)));
            match self.update_step.clone() {
                UpdateStep::Confirm => {
                    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(update::confirm_text(mine, &version)).color(TEXT));
                        ui.label(
                            RichText::new(tr!(
                                "Wikelink in the background and the overlay stop for a moment; the new version opens by itself.",
                                "O Wikelink em segundo plano e o overlay param por um momento; a versão nova abre sozinha."
                            ))
                            .color(MUTED)
                            .small(),
                        );
                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.busy.is_none() && target.is_some(), egui::Button::new(tr!("Update now", "Atualizar agora"))).clicked()
                                && let Some(v) = target.clone()
                            {
                                self.run_update(v);
                            }
                            if ui.button(tr!("Cancel", "Cancelar")).clicked() {
                                self.update_step = UpdateStep::Offer;
                            }
                        });
                    });
                }
                UpdateStep::Running => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(self.busy.clone().unwrap_or_default()).color(WARN));
                    });
                }
                step => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(trf!("Wikelink {version} available (this is {mine})", "Versão {version} do Wikelink disponível (esta é a {mine})")).color(WARN));
                        if let Some(link) = &link
                            && ui.button(tr!("Open download", "Abrir download")).on_hover_text(link.clone()).clicked()
                        {
                            let _ = open::that_detached(link);
                        }
                        if target.is_some()
                            && !matches!(step, UpdateStep::Finished(_))
                            && ui
                                .add_enabled(self.busy.is_none(), egui::Button::new(tr!("Update", "Atualizar")))
                                .on_hover_text(tr!("Asks first; nothing is downloaded before you confirm", "Pergunta antes; nada é baixado antes de você confirmar"))
                                .clicked()
                        {
                            self.update_step = UpdateStep::Confirm;
                        }
                    });
                    match &step {
                        UpdateStep::Failed(e) => {
                            ui.label(RichText::new(e).color(BAD));
                            ui.label(RichText::new(tr!("To update by hand: Open download.", "Para atualizar à mão: Abrir download.")).color(MUTED).small());
                        }
                        UpdateStep::Finished(text) => {
                            ui.label(RichText::new(text).color(OK));
                        }
                        _ => {}
                    }
                }
            }
        }
        // Another version runs in the background: typically `wikelo-watcher.exe` still running
        // after the move to this executable. A restart runs this one instead.
        if let Some(theirs) = self.status.as_ref().map(|s| s.version.clone()).filter(|v| !v.is_empty() && v != wikelo_core::PKG_VERSION) {
            ui.horizontal(|ui| {
                let mine = wikelo_core::PKG_VERSION;
                ui.label(RichText::new(trf!("Version {theirs} is running in the background (this is {mine})", "A versão {theirs} está rodando em segundo plano (esta é a {mine})")).color(WARN));
                if ui.add_enabled(self.busy.is_none(), egui::Button::new(tr!("Restart", "Reiniciar"))).clicked() {
                    self.restart_as_this_version();
                }
            });
        }
    }

    fn account(&mut self, ui: &mut egui::Ui) {
        let signed_in = self.signed_in();
        group(ui, tr!("Account", "Conta"), |ui| {
            // Only for developers (`WIKELINK_DEV_SERVER`): which local server is in use.
            if let Some(dev) = agent::dev_server() {
                ui.label(RichText::new(trf!("Developer server: {dev}", "Servidor de desenvolvimento: {dev}")).color(WARN).small());
            }
            if signed_in {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(self.cfg.display_name()).strong());
                });
                ui.horizontal(|ui| {
                    // The site (or the developer server), when it keeps the rule the requests keep.
                    if let Some(site) = crate::tray::site_url()
                        && ui.button(tr!("Open the site", "Abrir o site")).clicked()
                    {
                        let _ = open::that_detached(&site);
                    }
                    if ui
                        .add_enabled(self.busy.is_none(), egui::Button::new(tr!("Sign out", "Sair da conta")))
                        .on_hover_text(tr!("Stops Wikelink and forgets the session on this PC", "Para o Wikelink e esquece a sessão neste PC"))
                        .clicked()
                    {
                        self.logout();
                    }
                });
            } else if let Some((code, page, copied)) = self.linking.as_ref().map(|l| (l.code.clone(), l.page.clone(), l.copied)) {
                // Waiting for the browser: the code to compare, and the two ways out.
                ui.label(RichText::new(tr!("Confirm this code on the site:", "Confirme este código no site:")).color(MUTED));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&code).monospace().strong().size(26.0).color(TEXT));
                    // The code exactly as shown, through egui's clipboard; nothing else is copied.
                    if ui.button(tr!("Copy code", "Copiar código")).clicked() {
                        ui.ctx().copy_text(code.clone());
                        if let Some(l) = self.linking.as_mut() {
                            l.copied = Some(Instant::now());
                        }
                        ui.ctx().request_repaint();
                    }
                    if let Some(left) = copied.and_then(|at| COPIED_FOR.checked_sub(at.elapsed())) {
                        ui.label(RichText::new(tr!("copied", "copiado")).color(OK));
                        // Gone once its time is up.
                        ui.ctx().request_repaint_after(left);
                    }
                });
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tr!("Type this code on the site, in the browser…", "Digite este código no site, no navegador…")).color(MUTED));
                });
                let mut cancel = false;
                ui.horizontal(|ui| {
                    if ui.button(tr!("Open the page again", "Abrir a página de novo")).on_hover_text(page.clone()).clicked() {
                        let _ = open::that_detached(&page);
                    }
                    cancel = ui.button(tr!("Cancel", "Cancelar")).clicked();
                });
                if cancel {
                    self.cancel_link();
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.busy.is_none(), egui::Button::new(tr!("Sign in", "Entrar"))).clicked() {
                        self.sign_in();
                    }
                    ui.label(RichText::new(tr!("Opens the site in the browser, where you type the code shown here. Wikelink starts as soon as you sign in.", "Abre o site no navegador, onde você digita o código mostrado aqui. O Wikelink começa assim que você entrar.")).color(MUTED).small());
                });
            }
            if let Some((is_err, text)) = &self.msg {
                ui.label(RichText::new(text).color(if *is_err { BAD } else { OK }));
            }
        });
    }

    fn game(&mut self, ui: &mut egui::Ui) {
        group(ui, tr!("Game", "Jogo"), |ui| {
            if self.installs.is_empty() {
                ui.label(RichText::new(tr!("No Star Citizen install was found on this PC.", "Nenhuma instalação do Star Citizen foi encontrada neste PC.")).color(WARN));
                ui.label(RichText::new(tr!("Wikelink has to run on the PC where the game is installed.", "O Wikelink precisa rodar no PC onde o jogo está instalado.")).color(MUTED).small());
                return;
            }
            let current = self
                .cfg
                .channel
                .clone()
                .or_else(|| self.installs.iter().find(|i| i.channel == "LIVE").or(self.installs.first()).map(|i| i.channel.clone()))
                .unwrap_or_default();
            let mut selected = current.clone();
            ui.horizontal(|ui| {
                ui.label(tr!("Channel", "Canal"));
                egui::ComboBox::from_id_salt("canal").selected_text(&selected).show_ui(ui, |ui| {
                    for i in &self.installs {
                        ui.selectable_value(&mut selected, i.channel.clone(), format!("{} · {}", i.channel, wikelo_core::version_label(&i.manifest.branch, &i.manifest.version)));
                    }
                });
                if let Some(i) = self.installs.iter().find(|i| i.channel == selected) {
                    let (label, build) = (wikelo_core::version_label(&i.manifest.branch, &i.manifest.version), &i.manifest.version);
                    ui.label(RichText::new(trf!("version {label} ({build})", "versão {label} ({build})")).color(MUTED));
                }
            });
            if let Some(i) = self.installs.iter().find(|i| i.channel == selected) {
                ui.label(RichText::new(i.root.display().to_string()).color(MUTED).small());
            }
            if selected != current {
                self.save_with(|c| c.channel = Some(selected.clone()));
                if self.running.is_some() {
                    self.restart();
                }
            }
        });
    }

    fn options(&mut self, ui: &mut egui::Ui) {
        group(ui, tr!("Options", "Opções"), |ui| {
            let mut auto = self.autostart;
            if ui.add_enabled(self.busy.is_none(), egui::Checkbox::new(&mut auto, tr!("Start with Windows", "Iniciar junto com o Windows"))).changed() {
                self.set_autostart(auto);
            }
            let mut share = self.cfg.share_data.unwrap_or(true);
            if ui.checkbox(&mut share, tr!("Also send the game data when a new version comes out", "Enviar também os dados do jogo quando sair uma versão nova")).changed() {
                self.save_with(|c| c.share_data = Some(share));
            }
            ui.label(
                RichText::new(tr!(
                    "Missions from the Game.log are sent whenever Wikelink is running; that does not change. \
                     This option is only for the game's contracts and images: once per new version, with the game closed, a few seconds.",
                    "As missões do Game.log são enviadas sempre que o Wikelink está rodando; isso não muda. \
                     Esta opção é só para os contratos e imagens do jogo: uma vez por versão nova, com o jogo fechado, uns segundos."
                ))
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                if ui.add_enabled(self.running.is_some() && self.busy.is_none(), egui::Button::new(tr!("Check versions now", "Verificar versões agora"))).clicked() {
                    let _ = agent::request_data_check();
                }
                if let Some(s) = self.status.as_ref().and_then(|s| s.data_check.clone()) {
                    ui.label(RichText::new(s).color(MUTED).small());
                }
            });
        });
    }

    fn overlay(&mut self, ui: &mut egui::Ui) {
        group(ui, "Overlay", |ui| {
            ui.horizontal(|ui| {
                let idle = self.busy.is_none();
                if self.overlay_open {
                    if ui.add_enabled(idle, egui::Button::new(tr!("Close overlay", "Fechar overlay"))).clicked() {
                        self.close_overlay();
                    }
                    if self.overlay_preview {
                        ui.label(RichText::new(tr!("● test, without the game", "● teste, sem o jogo")).color(OK));
                    } else if self.game_up {
                        ui.label(RichText::new(tr!("● over the game", "● sobre o jogo")).color(OK));
                    } else {
                        ui.label(RichText::new(tr!("● waiting for the game", "● esperando o jogo")).color(MUTED))
                            .on_hover_text(tr!("Shows when the game opens and goes when it closes", "Aparece quando o jogo abrir e some quando ele fechar"));
                    }
                } else {
                    if ui
                        .add_enabled(idle, egui::Button::new(tr!("Open overlay", "Abrir overlay")))
                        .on_hover_text(tr!(
                            "Cards over the game: pinned, what is missing, what is here, records. Shows when the game opens",
                            "Cartões por cima do jogo: fixados, o que falta, o que está aqui, registros. Aparece quando o jogo abrir"
                        ))
                        .clicked()
                    {
                        self.open_overlay(false);
                    }
                    if ui.add_enabled(idle, egui::Button::new(RichText::new(tr!("Test", "Testar")).small())).on_hover_text(tr!("Shows the overlay now, even without the game", "Mostra o overlay agora, mesmo sem o jogo")).clicked() {
                        self.open_overlay(true);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(idle, egui::Button::new(RichText::new(tr!("Reset positions", "Repor posições")).small()))
                        .on_hover_text(tr!("Overlay cards back to their default places the next time it opens", "Cartões do overlay de volta ao lugar padrão na próxima vez que ele abrir"))
                        .clicked()
                    {
                        self.reset_overlay_pos();
                    }
                });
            });
            let mut with_game = self.cfg.overlay_with_game();
            if ui
                .checkbox(&mut with_game, tr!("Show the overlay when the game opens", "Mostrar o overlay quando o jogo abrir"))
                .on_hover_text(tr!("With Wikelink running, the overlay shows up by itself with the game", "Com o Wikelink rodando, o overlay aparece sozinho com o jogo"))
                .changed()
            {
                self.save_with(|c| c.overlay_with_game = Some(with_game));
            }
            // The overlay's keys (see `hotkey`), written here so this window does not depend on
            // how the overlay names its own help text.
            ui.label(
                RichText::new(tr!(
                    "Shift+Tab opens/closes · Alt+Shift+F9 shows/hides · the game must be in borderless window mode",
                    "Shift+Tab abre/fecha · Alt+Shift+F9 mostra/esconde · o jogo precisa estar em janela sem bordas"
                ))
                .color(MUTED)
                .small(),
            );
        });
    }

    fn activity(&mut self, ui: &mut egui::Ui) {
        group(ui, tr!("Activity", "Atividade"), |ui| {
            match (&self.running, &self.status) {
                (None, _) => {
                    ui.label(RichText::new(tr!("Wikelink is stopped. Nothing is read or sent.", "O Wikelink está parado. Nada é lido nem enviado.")).color(MUTED));
                }
                (Some(_), None) => {
                    ui.label(RichText::new(tr!("starting…", "iniciando…")).color(MUTED));
                }
                (Some(_), Some(s)) => {
                    ui.horizontal_wrapped(|ui| {
                        if s.paused {
                            ui.label(RichText::new(tr!("paused from the site", "pausado pelo site")).color(WARN));
                        } else if let Some(e) = &s.last_error {
                            ui.label(RichText::new(e).color(BAD));
                        } else if s.last_ok.is_some() {
                            ui.label(RichText::new(tr!("connected to the server", "conectado ao servidor")).color(OK));
                        } else {
                            ui.label(RichText::new(tr!("connecting…", "conectando…")).color(MUTED));
                        }
                        let channel = &s.channel;
                        ui.label(RichText::new(trf!("· channel {channel}", "· canal {channel}")).color(MUTED));
                        if let Some(t) = &s.last_ok {
                            let at = hhmmss(t);
                            ui.label(RichText::new(trf!("· last contact {at}", "· último contato {at}")).color(MUTED));
                        }
                    });
                    if let Some(e) = &s.last_event {
                        ui.label(RichText::new(e).small());
                    }
                }
            }
            egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(170.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    let text = if self.log_tail.trim().is_empty() { tr!("(no records yet)", "(sem registros ainda)").to_string() } else { self.log_tail.clone() };
                    ui.add(egui::Label::new(RichText::new(text).monospace().size(11.0).color(Color32::from_rgb(0xb8, 0xc2, 0xcf))).wrap());
                });
            });
            ui.horizontal(|ui| {
                if ui.button(tr!("Open the full log", "Abrir o registro completo")).clicked() {
                    let _ = open::that_detached(agent::log_path());
                }
                ui.label(RichText::new(agent::log_path().display().to_string()).color(MUTED).small());
            });
        });
    }

    /// The language choice, shown signed in or not: saved as `lang` in watcher.json.
    fn language(&mut self, ui: &mut egui::Ui) {
        let current = i18n::parse(self.cfg.lang.as_deref());
        let name = |l: Option<Lang>| match l {
            None => tr!("Automatic (Windows)", "Automático (Windows)"),
            Some(Lang::En) => "English",
            Some(Lang::Pt) => "Português",
        };
        let mut chosen = current;
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Language", "Idioma")).color(MUTED));
            ui.add_enabled_ui(self.busy.is_none(), |ui| {
                egui::ComboBox::from_id_salt("lang").selected_text(name(chosen)).show_ui(ui, |ui| {
                    for l in [None, Some(Lang::En), Some(Lang::Pt)] {
                        ui.selectable_value(&mut chosen, l, name(l));
                    }
                });
            });
        });
        if chosen != current {
            self.set_lang(chosen);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.quit {
            // The update opened the new version's window.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if matches!(self.update_step, UpdateStep::Running) && ctx.input(|i| i.viewport().close_requested()) {
            // Closing now could leave the files half swapped: the window waits for the update.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.last_poll.elapsed() > Duration::from_secs(1) {
            self.poll();
        }
        if self.signed_in() != self.sized_for {
            self.sized_for = self.signed_in();
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::from(if self.sized_for { SIZE_SIGNED_IN } else { SIZE_SIGNED_OUT })));
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(14.0)).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                self.account(ui);
                if self.signed_in() {
                    self.game(ui);
                    self.options(ui);
                    self.overlay(ui);
                    self.activity(ui);
                }
                ui.add_space(4.0);
                self.language(ui);
            });
        });
    }
}

/// Framed box with an uppercase eyebrow title, like the panels on the site.
fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).color(MUTED).small().strong());
        add(ui);
    });
}

/// `12:34:56` from an RFC 3339 timestamp (local time).
fn hhmmss(rfc3339: &str) -> String {
    let Ok(t) = time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339) else { return rfc3339.to_string() };
    let t = time::UtcOffset::current_local_offset().map(|o| t.to_offset(o)).unwrap_or(t);
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// Last `max` bytes of a text file, starting at a line boundary.
fn tail(path: &std::path::Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let mut s = String::from_utf8_lossy(&buf).to_string();
    if start > 0
        && let Some(nl) = s.find('\n')
    {
        s = s[nl + 1..].to_string();
    }
    s.replace("\r\n", "\n").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_session_of_an_older_attempt_is_ended() {
        let signed = agent::LinkOutcome::SignedIn(agent::SignIn { token: "tok-1".into(), handle: "TestPilot".into(), name: "Test Pilot".into() });
        // Cancelled (the attempt number moved on) or replaced by a newer attempt.
        assert_eq!(orphan_token(1, 2, &signed), Some("tok-1".into()));
        assert_eq!(orphan_token(1, 5, &signed), Some("tok-1".into()));
        // The current attempt keeps its session; other outcomes carry none.
        assert_eq!(orphan_token(2, 2, &signed), None);
        for other in [agent::LinkOutcome::Cancelled, agent::LinkOutcome::Denied, agent::LinkOutcome::Expired] {
            assert_eq!(orphan_token(1, 2, &other), None);
        }
    }
}

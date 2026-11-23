//! The in-game overlay (`wikelink overlay`), after the idea of Discord's game
//! overlay: one transparent, always-on-top window over the whole monitor the game is on,
//! with the Wikelist cards drawn inside it ("Fixados", "Falta", "Aqui", "Registros",
//! "Missões", and "Blueprints", drawn only while open and shown or hidden by its button in the
//! bar: how many of the channel's blueprints the account has, a search field and a filter of
//! its own (Todas / Adquiridas / Não adquiridas), and every blueprint of the index by name,
//! each row saying first whether the account has it; a click on a name opens its craft, from
//! `GET .../overlay/blueprint/{blueprint}`: what the server says of the crafted item (type,
//! size, grade, class, manufacturer), per slot the material in use with what the player has
//! against what one craft needs and a quality control, and what the crafted item gets,
//! computed here with `wikelo_core::craft`; "Onde conseguir" shows the missions that give it
//! (no percentage: every blueprint of a pool has the same odds and none is given twice; only
//! a reward that does not always give one says so), "não disponível" one nothing gives; a
//! mission opens its pools, from `GET .../overlay/mission/{contract}/blueprints`, each with
//! how many of its blueprints the account has and the blueprints themselves, each opening its
//! craft. The card's screens form a stack: "Voltar" always returns to the previous one.
//! Nothing about blueprints is pinned).
//!
//! Closed (the normal state while playing) the window ignores the mouse and never takes the
//! focus: only the cards the player pinned ("fixar na tela") are drawn, at their places and
//! with the chosen opacity and text size, plus short toasts: the alerts (each new notice and
//! a change of location, a few seconds each, while "Alertas" is on in the widgets menu), a
//! reward waiting to be chosen, and the overlay's own warnings.
//! Shift+Tab opens it: a dark scrim over the screen, every card (dragged by its header,
//! resized by its lower-right corner, hidden by the ✕ of its header), the reward choices
//! waiting, a bar with the channel, the location, the opacity, the text size (the bar and the
//! cards' headers grow with it too), the blueprints card's button, the widgets menu (a switch
//! per card but the blueprints one, for the search and for the alerts; "mostrar todos" /
//! "ocultar todos"; a card or the search switched off is not drawn open or closed, its pin
//! kept) and "Ocultar" (hides everything,
//! as Alt+Shift+F9 does; the process stays, and the hotkey or the tray icon's "Mostrar o
//! overlay" brings it back: nothing in the overlay ends its process), and the search strip:
//! right under the bar until dragged elsewhere by its grip, one big field (Ctrl+F). Once
//! something is typed, filter chips under it ("Tudo", and each group that has results with
//! its count; never saved) and the results, grouped under headings: "Inventário", every item
//! of the channel's index that matches (owned or not; the ones owned first, then by name),
//! each with what the player has (from `GET /api/inventory`, 0 for an item it does not list,
//! no number before it answered: read while the overlay is open, every
//! [`widgets::INVENTORY_EVERY`] and after a write settles), then "Missões". The index
//! (`GET .../overlay/index`, also the blueprints card's list) is searched on this machine,
//! fetched once and revalidated with its
//! ETag at most every [`INDEX_REVALIDATE`]; an older server without it is searched per query.
//! The results unfold under the strip (above it when there is more room there), over the
//! cards, as tall as they are up to the edge of the screen; an empty field shows nothing.
//! Every item row then has - and + (Shift held as the button goes down: 10) and folds out
//! where the item is kept, one place per line, each with "move": a small picker (how many,
//! all at first; a filter field with the keyboard over the channel's locations from
//! `GET .../dataset`; under it where the player is and the home, the places already holding
//! it, and the locations matching the filter, like the site's picker: not the ones the game
//! hides from its star map or never named, unless the player is there, keeps the home there or
//! has something there, see `model::offered`) that moves it between places, or sets
//! the place of what has none (`POST /api/inventory/{item}/move`, one per quality, see
//! `Local::Move`). Pinned items and missions can be unpinned from their rows, and under a
//! pinned item each item that converts into it can be pinned to show while closed. A change
//! shows at once and settles with the next read (see `overlay_sync`: the clicks on one item
//! go out as one write, an unpinned row or an answered choice stays in place, marked, until
//! then); a write the server refuses puts it back and says so in a toast, one without an
//! answer (or a move applied only in part) is marked as not confirmed until the next read
//! says what the server has. Every row and button is keyed by the server's ids, so a row that
//! comes or goes never hands a click to another one, and everything shown and every change
//! belongs to one endpoint (server, account, channel): another one in watcher.json drops it
//! all. The window takes the focus from the game and hands it back on close (Shift+Tab
//! again, Esc once the move picker and the widgets menu are closed and the field empty, the
//! ×, or a click on the scrim). Alt+Shift+F9 hides / shows everything.
//!
//! The overlay belongs to the game's window, the visible top-level window titled exactly
//! `Star Citizen` (found by its title, never through the game's process), and lives and dies
//! with it (see `overlay_game`): no window at all until the game's is usable, gone again
//! (and the process replaced by a fresh one that waits) when the game closes, crashes or
//! hangs. While on the game the overlay covers its monitor (looked at again on opening and
//! every [`PLACE_CHECK_EVERY`]), and closed it draws only while the game is the window in
//! front: Alt+Tab to a browser shows nothing over the browser. With `--preview` (trying the
//! overlay without the game) it shows at once, covers the monitor of the window in front and
//! always draws. Opening first moves the window to that monitor and clamps the cards into it,
//! then takes the mouse and the keyboard; when Windows refuses the keyboard it stays closed
//! and says so in a toast.
//!
//! It is a separate window of its own process, nothing goes into the game. The data comes
//! from `GET /api/channels/{channel}/overlay` every few seconds while it is shown (with
//! `If-None-Match`: a 304 keeps what is on screen);
//! `overlay_sync::Engine` picks every request (reads, writes, searches: one at a time, within
//! a budget per minute, with the back-off of `overlay_sync::next_delay`) and a thread of its
//! own makes them, so the window never waits on the network. One overlay at a time: it holds
//! a named mutex for its whole life (`agent::overlay_lock`), and `overlay.stop` in the data
//! dir asks it to close (`agent::close_overlay`: the settings window's "Fechar overlay", the
//! tray icon's "Overlay" and "Sair", and stopping Wikelink or signing out), also while it
//! waits for the game; the overlay that acts on that file removes it (`overlay.show`, from
//! the tray, shows it again after "Ocultar"). Wikelink's background agent keeps one running so it
//! is there as soon as the game is (see `agent::OverlayKeeper`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, RichText};

use crate::agent::{self, ApiRequest, Client, OverlayWidget};
use crate::hotkey::{self, HotkeyEvent, Hotkeys};
use crate::overlay_game::{self, Life};
use crate::overlay_log;
use crate::overlay_model::{self as model, Local, OverlayData, OverlayIndex};
use crate::overlay_sync::{self as sync, Job, Limit, Outcome, WriteEffect, WriteResult};
use crate::overlay_widgets::{self as widgets, BpPanel, BpScreen, CardPlace, DetailAnswer, DetailFetch, EscStep, Filter, Group, InventoryFetch, Layout, Listing, LocationsFetch, MissionAnswer, MissionFetch, Part, ScreenStack, Search, SearchAnswer, Shown, Toast, Toasts, Widget};
use crate::overlay_win::{self as os, Monitor, PrevFocus, Window};
use crate::palette::{ACCENT, BORDER, BRASS, FIELD, FIELD_BORDER, FIELD_HINT, MUTED, NAVY, OK, TEXT, TEXT_BRIGHT, WARN};
use crate::{tr, trf};

const TITLE: &str = "Wikelink overlay";
/// Most a card's body grows before it scrolls (logical px).
const CARD_BODY_MAX: f32 = 420.0;
const CARD_MARGIN: f32 = 10.0;
/// How many frames `place` waits for the window to cover the monitor.
const PLACE_FRAMES: u32 = 30;
/// How often the window is checked to still cover its monitor (a resolution change).
const PLACE_CHECK_EVERY: Duration = Duration::from_secs(2);
/// Darkness of the scrim behind the cards while the overlay is open.
const SCRIM_ALPHA: u8 = 110;
const TOAST_WIDTH: f32 = 280.0;
/// Room between the toasts and the right / bottom edges of the screen, and between two toasts.
const TOAST_EDGE: f32 = 24.0;
const TOAST_BOTTOM: f32 = 120.0;
const TOAST_GAP: f32 = 8.0;
/// Notices listed in the "Registros" card.
const NOTICES_SHOWN: usize = 5;
/// How often watcher.json is looked at for another endpoint (a sign-in or out, another
/// account, server or channel).
const CONFIG_CHECK_EVERY: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The search index is asked again (with its ETag: usually a 304) at most this often; an
/// older server without it is asked again as often, in case it was updated.
const INDEX_REVALIDATE: Duration = Duration::from_secs(10 * 60);
/// A changed layout (a card dragged or pinned, the opacity) is saved once it has stayed put this long.
const SAVE_LAYOUT_AFTER: Duration = Duration::from_millis(800);
/// Least time between two layout writes (cards pushed back on screen by a size change
/// included), so watcher.json is never rewritten in a loop.
const MIN_SAVE_GAP: Duration = Duration::from_secs(1);
/// How often the closed overlay looks at which window is in front while the game's exists.
const FOREGROUND_CHECK_EVERY: Duration = Duration::from_millis(250);
/// Toast when Windows does not let the overlay take the keyboard from the game.
fn focus_refused() -> &'static str {
    tr!("could not open over the game", "não foi possível abrir sobre o jogo")
}
/// Title of the toast for a write that did not go through.
fn write_failed() -> &'static str {
    tr!("Not saved", "Não salvo")
}
/// Title of the toast for a write that got no answer.
fn write_unknown() -> &'static str {
    tr!("Not confirmed", "Sem confirmação")
}
/// Hover text of a number whose write got no answer.
fn unconfirmed_hint() -> &'static str {
    tr!("not confirmed: checking with the server", "não confirmado: conferindo com o servidor")
}
/// Hover text of a row whose unpin is on its way.
fn leaving_hint() -> &'static str {
    tr!("unpinning…", "desafixando…")
}
/// Hover text of a control waiting for the changes on their way.
fn busy_hint() -> &'static str {
    tr!("wait: changes on their way", "aguarde: alterações a caminho")
}
/// Hover text of a place's "move" while a move of the item is on its way.
fn moving_hint() -> &'static str {
    tr!("wait: moving", "aguarde: movendo")
}
/// The blueprints card when the site sends no list (an older server).
fn no_blueprint_list() -> &'static str {
    tr!("The site has no blueprint list yet.", "O site ainda não tem a lista de blueprints.")
}
/// Width of a blueprint row's first cell (acquired or not), before the text size.
const BP_STATE_WIDTH: f32 = 112.0;
/// Width of the move picker, before the text size.
const MOVER_WIDTH: f32 = 300.0;
/// Hover text of "Hide": what it does and how the overlay comes back.
fn hide_hint() -> &'static str {
    tr!("Hides the overlay (like Alt+Shift+F9). Comes back with Alt+Shift+F9 or from the tray icon: Show the overlay", "Esconde o overlay (como Alt+Shift+F9). Volta com Alt+Shift+F9 ou pelo ícone da bandeja: Mostrar o overlay")
}
/// After a layout write that did not go through, the next try waits this long.
const SAVE_RETRY: Duration = Duration::from_secs(5);
/// Room kept between a panel and the edges of the screen.
const SCREEN_GAP: f32 = 8.0;
/// Narrowest the reward choices get on a small screen.
const CHOICES_MIN_WIDTH: f32 = 200.0;
/// Height of the title of the reward choices, before the text size.
const CHOICES_HEADER: f32 = 30.0;
const BODY: f32 = 13.5;
const SMALL: f32 = 12.0;
const INDENT: f32 = 12.0;
/// The corner a card is resized by (logical px).
const RESIZE_GRIP: f32 = 14.0;
/// Least room for the rows of a resized card.
const MIN_BODY: f32 = 24.0;
/// Height of a row's progress bar, before the text size.
const PROGRESS_HEIGHT: f32 = 3.0;
const CHOICES_WIDTH: f32 = 420.0;
/// How many frames after a mode change `show_layered` is repeated (the style change lands a frame or two later).
const RELAYER_FRAMES: u8 = 6;
/// Inner margin of the bar and of the search strip (logical px).
const BAR_MARGIN: f32 = 8.0;
/// Room between the bar and the search strip in its first place.
const STRIP_GAP: f32 = 6.0;
/// Width of the grip the search strip is dragged by.
const STRIP_GRIP: f32 = 14.0;
/// Room between the search strip and its results unfolded upwards.
const UNFOLD_GAP: f32 = 4.0;
/// Narrowest the widgets menu gets.
const MENU_WIDTH: f32 = 240.0;
/// Size of a switch of the widgets menu.
const SWITCH_SIZE: [f32; 2] = [30.0, 16.0];
/// Narrowest the search strip gets (a narrow bar).
const STRIP_MIN_WIDTH: f32 = 440.0;
/// Height of the search field, before the text size (a normal row is 20).
const SEARCH_FIELD_HEIGHT: f32 = 32.0;
/// The search field's text against the overlay's.
const SEARCH_FIELD_TEXT: f32 = 1.35;

/// The search field, for the keyboard focus (Ctrl+F, Esc).
fn search_id() -> egui::Id {
    egui::Id::new("wikelist-search")
}

/// The blueprints card's search field (Esc empties it).
fn bp_search_id() -> egui::Id {
    egui::Id::new("wikelist-bp-search")
}

/// The area of the search strip.
fn strip_id() -> egui::Id {
    egui::Id::new("wikelist-search-strip")
}

/// The area of the search results unfolded upwards (downwards they are in the strip's).
fn results_up_id() -> egui::Id {
    egui::Id::new("wikelist-search-up")
}

/// The area of the widgets menu.
fn menu_id() -> egui::Id {
    egui::Id::new("wikelist-widgets-menu")
}

/// The area of the move picker.
fn mover_id() -> egui::Id {
    egui::Id::new("wikelist-mover")
}

/// The move picker's filter field: one fixed id, so the keyboard focus stays on it however
/// many rows the picker lists (an automatic id would follow the widgets added before it).
fn mover_filter_id() -> egui::Id {
    egui::Id::new("wikelist-mover-filter")
}

/// A layer of the open overlay's top order.
fn top_layer(id: egui::Id) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, id)
}

/// The area of a card.
fn card_id(w: Widget) -> egui::Id {
    egui::Id::new(("wikelist-card", w.key()))
}

/// The layer of a card's area (its place in the stacking, see `widgets::ZOrder`).
fn card_layer(w: Widget) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Middle, card_id(w))
}

/// One request for the data thread (chosen by `sync::Engine::next_job`), made while the
/// overlay showed the data of endpoint generation `generation`. `etag`: the ETag of what the
/// window holds for a read or an index request (sent as `If-None-Match`, and only to that
/// same generation): a 304 then keeps it.
struct Cmd {
    generation: u64,
    job: Job,
    etag: Option<String>,
}

/// An answer, with the generation of the endpoint it came from.
struct Reply {
    generation: u64,
    what: Answer,
}

// One of these crosses a channel every few seconds and is taken apart at once: boxing the
// read would only add an allocation per answer.
#[allow(clippy::large_enum_variant)]
enum Answer {
    Read(ReadAnswer),
    /// What became of the write of change `id`.
    Write(u64, WriteResult),
    /// A search's number, its answer, and whether it was a 429.
    Search(u64, SearchAnswer, Option<Limit>),
    Index(IndexAnswer),
    /// The inventory request's number, what the player owns, and whether it was a 429.
    Inventory(u64, Result<HashMap<String, model::Owned>, String>, Option<Limit>),
    /// The channel's locations, and whether it was a 429.
    Locations(Result<Vec<model::Location>, String>, Option<Limit>),
    /// The detail of the blueprint with this key.
    Detail(String, DetailAnswer),
    /// The blueprints of the mission with this contract id.
    Mission(String, MissionAnswer),
}

/// What a request for the search index got.
#[derive(Debug, PartialEq)]
enum IndexAnswer {
    /// The index and its ETag (none when the server sent none: nothing to revalidate with).
    Data(OverlayIndex, Option<String>),
    /// 304: the index the window holds is still the channel's.
    NotModified,
    /// A 404 without a message: a server older than the index; search it per query.
    OldServer,
    /// No answer or an error (and whether it was a 429): tried again later.
    Failed(String, Option<Limit>),
}

/// What a read of the overlay route got.
// One short-lived message per read: boxing the data would buy nothing.
#[allow(clippy::large_enum_variant)]
enum ReadAnswer {
    /// The answer and its ETag (none from a server without them).
    Data(OverlayData, Option<String>),
    /// 304: what the window shows is still the answer.
    NotModified,
    SignedOut,
    /// The server answered 404 without a message: it is older than the overlay.
    NoRoute,
    /// No answer, an error, or a 429 (too many requests, with the server's wait): back off.
    Unreachable(String, Option<Limit>),
}

impl ReadAnswer {
    fn outcome(&self) -> Outcome {
        match self {
            ReadAnswer::Data(..) | ReadAnswer::NotModified => Outcome::Good,
            ReadAnswer::SignedOut => Outcome::SignedOut,
            ReadAnswer::NoRoute => Outcome::NoRoute,
            ReadAnswer::Unreachable(..) => Outcome::Unreachable,
        }
    }

    fn limit(&self) -> Option<Limit> {
        match self {
            ReadAnswer::Unreachable(_, limit) => *limit,
            _ => None,
        }
    }
}

/// How much longer a changed layout waits before it is written: until it has stayed put for
/// [`SAVE_LAYOUT_AFTER`] (`changed_ago`) and the last write (`saved_ago`, `None` = none yet)
/// is at least [`MIN_SAVE_GAP`] old. Zero: write now.
fn save_wait(changed_ago: Duration, saved_ago: Option<Duration>) -> Duration {
    let settle = SAVE_LAYOUT_AFTER.saturating_sub(changed_ago);
    settle.max(commit_wait(saved_ago))
}

/// How much longer a change the player finished (see `Overlay::commit`) waits before it is
/// written: no settling, only until the last write (`saved_ago`, `None` = none yet) is at
/// least [`MIN_SAVE_GAP`] old. Zero: write now.
fn commit_wait(saved_ago: Option<Duration>) -> Duration {
    saved_ago.map_or(Duration::ZERO, |ago| MIN_SAVE_GAP.saturating_sub(ago))
}

/// The monitor the overlay belongs on: the game window's when there is one, else the one of
/// the window in front.
fn target_monitor(game: Option<Window>) -> Option<Monitor> {
    game.and_then(os::window_monitor).or_else(os::foreground_monitor)
}

#[derive(Debug, Clone, PartialEq)]
enum Conn {
    Loading,
    Ok,
    SignedOut,
    NoRoute,
    Unreachable(String),
}

/// `preview`: show at once, without the game (`overlay --preview`); otherwise wait for the
/// game's window and leave with it (see `overlay_game`).
pub fn run(preview: bool) -> anyhow::Result<()> {
    // Elsewhere there is no game window to look for: always the preview.
    let preview = preview || !cfg!(windows);
    let respawned = overlay_game::respawned();
    let (life, ran) = {
        // One overlay at a time: a second launch just leaves (a replacement waits for the one
        // it replaces). The lock lives to the end of this block, so it is free again before a
        // replacement starts.
        let Some(_lock) = overlay_game::lock(respawned)? else { return Ok(()) };
        // A stop request nobody read (an overlay that crashed) must not close this one at
        // once; a replacement keeps it: it was meant for the overlay it continues.
        if !respawned {
            let _ = std::fs::remove_file(agent::overlay_stop_path());
        }
        // A fresh overlay starts shown: a show request left over says nothing.
        let _ = std::fs::remove_file(agent::overlay_show_path());
        // No window, no hotkeys, no reads until the game's window is usable.
        if !preview && !overlay_game::wait_for_game() {
            // Ended by the stop request: it is used up.
            let _ = std::fs::remove_file(agent::overlay_stop_path());
            return Ok(());
        }
        show(preview)
    };
    if life.game_left() {
        // A fresh process waits for the game with a new window and device (none when a stop
        // request is pending: `respawn_overlay` uses it up instead). No error box: a backend
        // error after the game took the device down is expected here.
        life.respawn();
        return Ok(());
    }
    // Only a request this overlay acted on is removed: one written while it was closing for
    // another reason stays for whoever comes next (a fresh launch clears it).
    if life.stopped() {
        let _ = std::fs::remove_file(agent::overlay_stop_path());
    }
    match ran {
        Ok(result) => result.map_err(|e| anyhow::anyhow!(trf!("could not open the overlay: {e}", "não foi possível abrir o overlay: {e}"))),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// How the event loop ended, a panic inside it included.
type Ran = std::thread::Result<Result<(), eframe::Error>>;

/// Create the window and run it until it closes: what it shared with the watch thread, and
/// how the event loop ended.
fn show(preview: bool) -> (Arc<Life>, Ran) {
    let cfg = agent::load_config().unwrap_or_default();
    // Before any thread starts (`time` refuses to read the local offset afterwards on Unix).
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    // The game's monitor (failing that, the one of the window in front now), before this
    // window exists.
    let game = os::game_window();
    // The window the watch thread follows: a new game window later is another game.
    let followed = game.map(os::follow);
    let monitor = target_monitor(game);
    overlay_log::line(format_args!(
        "overlay started: Wikelink {} ({}) pid={} elevated={:?} preview={preview} | game window: {} | monitor: {:?}",
        wikelo_core::PKG_VERSION,
        wikelo_core::GIT_COMMIT,
        std::process::id(),
        os::elevated(),
        os::describe_window(game),
        os::monitor_rect(monitor)
    ));
    let viewport = egui::ViewportBuilder::default()
        .with_title(TITLE)
        // Small until the first frames make it cover the monitor (`Overlay::place`).
        .with_inner_size([widgets::CARD_WIDTH, 120.0])
        .with_transparent(true)
        .with_decorations(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_resizable(false)
        .with_mouse_passthrough(true)
        // Never take the focus from the game when appearing.
        .with_active(false);
    // Glow: the wgpu backends on Windows do not composite a transparent window.
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, ..Default::default() };
    let life = Arc::new(Life::default());
    let app_life = life.clone();
    // winit hands a panic inside the event loop back here once the loop is over: with the game
    // gone (a lost device can make the GL backend fail) it must not stop the replacement.
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        eframe::run_native(
            "Wikelist Overlay",
            options,
            Box::new(move |cc| {
                style(&cc.egui_ctx);
                if !preview {
                    overlay_game::start_watch(app_life.clone(), cc.egui_ctx.clone(), followed);
                }
                Ok(Box::new(Overlay::new(&cc.egui_ctx, &cfg, game, monitor, offset, preview, app_life)))
            }),
        )
    }));
    life.set_ui_done();
    (life, ran)
}

/// The settings window's style, with text sized for reading over the game.
fn style(ctx: &egui::Context) {
    crate::gui::setup_style(ctx);
    ctx.style_mut(|s| {
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(BODY));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(BODY));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.spacing.item_spacing = egui::vec2(6.0, 3.0);
        s.spacing.button_padding = egui::vec2(7.0, 1.0);
        s.spacing.interact_size.y = 20.0;
        // Plain labels: no text cursor, no selection drag while the overlay is open.
        s.interaction.selectable_labels = false;
    });
}

// ----- data thread ----------------------------------------------------------------------

/// Where the requests go for `cfg`: the signed-in server with its token, and the channel
/// (LIVE without one). The flag: a server is signed in; without it nothing is asked of anyone
/// (no server, no token) and the overlay says the sign-in is missing.
fn target(cfg: &agent::AgentConfig) -> (sync::Ident, bool) {
    let signed_in = !cfg.token.is_empty() && !cfg.server.is_empty();
    let (server, token) = if signed_in { (cfg.server.clone(), cfg.token.clone()) } else { (String::new(), String::new()) };
    let channel = cfg.channel.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "LIVE".into());
    ((server, token, channel), signed_in)
}

/// The endpoint of the requests (see [`target`]). One client (and its connection pool) per
/// server and token; the token only ever goes to the server it was given for.
#[derive(Default)]
struct Endpoint {
    client: Option<(String, String, Client)>,
    generations: sync::Generations,
}

impl Endpoint {
    /// The client, the channel, whether a server is signed in and the endpoint's generation,
    /// from a fresh read of `watcher.json`: signing in / out or a new channel is picked up at
    /// the next request.
    fn get(&mut self) -> (Client, String, bool, u64) {
        let cfg = agent::load_config().unwrap_or_default();
        let ((server, token, channel), signed_in) = target(&cfg);
        let generation = self.generations.observe((server.clone(), token.clone(), channel.clone()));
        if self.client.as_ref().is_none_or(|(s, t, _)| *s != server || *t != token) {
            self.client = Some((server.clone(), token.clone(), Client::with_timeout(&server, &token, REQUEST_TIMEOUT)));
        }
        (self.client.as_ref().expect("set just above").2.clone(), channel, signed_in, generation)
    }
}

/// Make the requests the window hands over, one at a time, each to the endpoint watcher.json
/// names at that moment, and hand back the answers with that endpoint's generation. A write
/// made for another generation than the one it would go to is never sent.
fn data_thread(rx: Receiver<Cmd>, tx: Sender<Reply>, ctx: egui::Context) {
    let mut endpoint = Endpoint::default();
    while let Ok(cmd) = rx.recv() {
        let (c, channel, signed_in, generation) = endpoint.get();
        // An ETag only means something to the endpoint it came from.
        let etag = cmd.etag.as_deref().filter(|_| sync::accepts(cmd.generation, generation));
        let what = match cmd.job {
            // No account: there is no server to ask.
            Job::Read if !signed_in => Answer::Read(ReadAnswer::SignedOut),
            Job::Read => Answer::Read(fetch(&c, &channel, etag)),
            Job::Write(id, _) if !sync::accepts(cmd.generation, generation) => Answer::Write(id, WriteResult::Stale),
            Job::Write(id, change) => Answer::Write(id, write(&c, &channel, &change)),
            Job::Search(seq, q) => {
                let (answer, limit) = search(&c, &channel, &q);
                Answer::Search(seq, answer, limit)
            }
            Job::Index => Answer::Index(fetch_index(&c, &channel, etag)),
            Job::Inventory(seq) => {
                let (answer, limit) = fetch_inventory(&c);
                Answer::Inventory(seq, answer, limit)
            }
            Job::Locations => {
                let (answer, limit) = fetch_locations(&c, &channel);
                Answer::Locations(answer, limit)
            }
            Job::Detail(key) => {
                let answer = fetch_detail(&c, &channel, &key, etag);
                Answer::Detail(key, answer)
            }
            Job::Mission(contract) => {
                let answer = fetch_mission(&c, &channel, &contract, etag);
                Answer::Mission(contract, answer)
            }
        };
        if tx.send(Reply { generation, what }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// A 304 to a request that sent no `If-None-Match`: nothing it could keep.
fn unexpected_304() -> &'static str {
    tr!("unexpected answer from the server (HTTP 304)", "resposta inesperada do servidor (HTTP 304)")
}

/// A 429 on the inventory or the locations.
fn too_many_requests() -> &'static str {
    tr!("too many requests; wait a moment", "consultas demais; espere um pouco")
}

/// No answer from the server at all.
fn no_contact() -> &'static str {
    tr!("can't reach the server", "sem contato com o servidor")
}

/// Signed out (a 401, or no token): where to sign in.
fn sign_in_hint() -> &'static str {
    tr!("Sign in through Wikelink", "Entre pelo Wikelink")
}
/// Longest ETag kept (anything longer is not sent back).
const MAX_ETAG: usize = 256;

/// An ETag worth sending back: printable, not empty, not too long.
fn usable_etag(etag: Option<String>) -> Option<String> {
    etag.filter(|e| !e.trim().is_empty() && e.len() <= MAX_ETAG && e.bytes().all(|b| (0x20..0x7f).contains(&b)))
}

/// Read the overlay route (`etag`: the window's, sent as `If-None-Match`).
fn fetch(client: &Client, channel: &str, etag: Option<&str>) -> ReadAnswer {
    let path = format!("/api/channels/{}/overlay", model::path_segment(channel));
    match client.request(ApiRequest::Get(&path, etag)) {
        Err(e) => ReadAnswer::Unreachable(e, None),
        Ok(a) => read_answer(a.status, &a.body, etag.is_some(), a.etag, a.retry_after),
    }
}

/// What an answer of the overlay route means. A 304 counts only when an ETag was sent (an
/// older server never sends one, and never gets one back); a 200 without an ETag is taken
/// as it is (the next read then sends none).
fn read_answer(status: u16, body: &[u8], etag_sent: bool, etag: Option<String>, retry_after: Option<Duration>) -> ReadAnswer {
    match status {
        304 if etag_sent => ReadAnswer::NotModified,
        304 => ReadAnswer::Unreachable(unexpected_304().into(), None),
        401 => ReadAnswer::SignedOut,
        404 => {
            let msg = agent::server_error(404, body);
            if msg == "HTTP 404" { ReadAnswer::NoRoute } else { ReadAnswer::Unreachable(msg, None) }
        }
        429 => ReadAnswer::Unreachable(agent::server_error(429, body), Some(Limit(retry_after))),
        status if status >= 400 => ReadAnswer::Unreachable(agent::server_error(status, body), None),
        _ => match model::parse(body) {
            Ok(d) => ReadAnswer::Data(d, usable_etag(etag)),
            Err(e) => ReadAnswer::Unreachable(e, None),
        },
    }
}

/// The HTTP method of a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Patch,
    Put,
    Delete,
    Post,
}

/// One write as a request: method, path (every id in it percent-encoded), JSON body, what it
/// does (for the messages) and whether it is an unpin (see `sync::WriteResult::Gone`).
#[derive(Debug, PartialEq)]
struct WriteRequest {
    verb: Verb,
    path: String,
    body: Option<serde_json::Value>,
    what: &'static str,
    unpin: bool,
}

/// The requests of one write, in order: one for every change, except a move between places,
/// which takes one `POST /api/inventory/{item}/move` per quality it moves (the route moves
/// one stack; the quality stays).
fn write_requests(channel: &str, edit: &Local) -> Vec<WriteRequest> {
    let ch = model::path_segment(channel);
    let req = |verb: Verb, path: String, body: Option<serde_json::Value>, what: &'static str, unpin: bool| WriteRequest { verb, path, body, what, unpin };
    let one = match edit {
        Local::Adjust { item_id, delta } => req(Verb::Patch, format!("/api/inventory/{}", model::path_segment(item_id)), Some(serde_json::json!({ "delta": delta })), tr!("adjust the quantity", "ajustar a quantidade"), false),
        Local::PinItem { item_id, .. } => req(Verb::Put, format!("/api/channels/{ch}/item-pins/{}", model::path_segment(item_id)), Some(serde_json::json!({})), tr!("pin the item", "fixar o item"), false),
        Local::UnpinItem { item_id } => req(Verb::Delete, format!("/api/channels/{ch}/item-pins/{}", model::path_segment(item_id)), None, tr!("unpin the item", "desafixar o item"), true),
        Local::PinMission { mission_id, .. } => req(Verb::Put, format!("/api/channels/{ch}/mission-pins/{}", model::path_segment(mission_id)), None, tr!("pin the mission", "fixar a missão"), false),
        Local::UnpinMission { mission_id } => req(Verb::Delete, format!("/api/channels/{ch}/mission-pins/{}", model::path_segment(mission_id)), None, tr!("unpin the mission", "desafixar a missão"), true),
        Local::Choose { id, item_id } => req(Verb::Post, format!("/api/pending/{}/choose", model::path_segment(&id.to_string())), Some(serde_json::json!({ "itemId": item_id })), tr!("choose the reward", "escolher a recompensa"), false),
        Local::Move { item_id, from, to, parts } => {
            let path = format!("/api/inventory/{}/move", model::path_segment(item_id));
            let what = tr!("move the item", "mover o item");
            return parts
                .iter()
                .map(|&(quality, qty)| req(Verb::Post, path.clone(), Some(serde_json::json!({ "from": from, "to": to, "qty": qty, "quality": quality })), what, false))
                .collect();
        }
    };
    vec![one]
}

/// Run one write, its requests one after the other (see [`write_requests`]). Only an HTTP
/// error status is a failure; no answer at all is [`WriteResult::Unknown`] (see
/// `sync::classify_write`), and so is a write applied only in part (see
/// `sync::classify_part`).
fn write(client: &Client, channel: &str, edit: &Local) -> WriteResult {
    let empty = serde_json::json!({});
    let mut done = false;
    for w in write_requests(channel, edit) {
        let body = w.body.as_ref().unwrap_or(&empty);
        let req = match w.verb {
            Verb::Patch => ApiRequest::Patch(&w.path, body),
            Verb::Put => ApiRequest::Put(&w.path, w.body.as_ref()),
            Verb::Delete => ApiRequest::Delete(&w.path),
            Verb::Post => ApiRequest::Post(&w.path, body),
        };
        let answer = client.request(req);
        let result = sync::classify_write(answer.as_ref().map(|a| (a.status, a.body.as_slice(), a.retry_after)).map_err(|e| e.as_str()), w.unpin, w.what);
        if let Some(end) = sync::classify_part(done, result, w.what) {
            return end;
        }
        done = true;
    }
    WriteResult::Applied
}

/// The channel's locations, for the move picker's filter: the dataset's list
/// (`GET /api/channels/{channel}/dataset`, the route the site's location picker reads; public,
/// a few MiB), read once per endpoint, only its locations kept.
fn fetch_locations(client: &Client, channel: &str) -> (Result<Vec<model::Location>, String>, Option<Limit>) {
    let path = format!("/api/channels/{}/dataset", model::path_segment(channel));
    match client.request_limited(ApiRequest::Get(&path, None), agent::DATASET_BODY_LIMIT) {
        Err(e) => (Err(e), None),
        Ok(a) => locations_answer(a.status, &a.body, a.retry_after),
    }
}

/// What an answer of the dataset route means for the locations; `Some` when it was a 429.
fn locations_answer(status: u16, body: &[u8], retry_after: Option<Duration>) -> (Result<Vec<model::Location>, String>, Option<Limit>) {
    match status {
        429 => (Err(too_many_requests().into()), Some(Limit(retry_after))),
        status if status >= 400 => (Err(agent::server_error(status, body)), None),
        _ => (model::parse_locations(body), None),
    }
}

/// Run one search (`q` already trimmed and of a searched length, see
/// `widgets::search_query`); `Some` when it was a 429.
fn search(client: &Client, channel: &str, q: &str) -> (SearchAnswer, Option<Limit>) {
    let path = format!("/api/channels/{}/overlay/search?q={}", model::path_segment(channel), model::path_segment(q));
    match client.request(ApiRequest::Get(&path, None)) {
        Err(_) => (Err(no_contact().into()), None),
        Ok(a) => match a.status {
            401 => (Err(sign_in_hint().into()), None),
            404 if agent::server_error(404, &a.body) == "HTTP 404" => (Err(tr!("the server has no search yet", "o servidor ainda não tem a busca").into()), None),
            429 => (Err(tr!("too many searches; wait a moment", "buscas demais; espere um pouco").into()), Some(Limit(a.retry_after))),
            status if status >= 400 => (Err(agent::server_error(status, &a.body)), None),
            _ => (model::parse_search(&a.body), None),
        },
    }
}

/// Fetch the search index (`etag`: of the index the window holds, sent as `If-None-Match`).
fn fetch_index(client: &Client, channel: &str, etag: Option<&str>) -> IndexAnswer {
    let path = format!("/api/channels/{}/overlay/index", model::path_segment(channel));
    match client.request(ApiRequest::Get(&path, etag)) {
        Err(e) => IndexAnswer::Failed(e, None),
        Ok(a) => index_answer(a.status, &a.body, etag.is_some(), a.etag, a.retry_after),
    }
}

/// What an answer of the index route means: a 404 without a message is a server older than
/// the index (search it per query); a 304 keeps the index held, but only when an ETag was
/// sent; anything else that is not the index is tried again later.
fn index_answer(status: u16, body: &[u8], etag_sent: bool, etag: Option<String>, retry_after: Option<Duration>) -> IndexAnswer {
    match status {
        304 if etag_sent => IndexAnswer::NotModified,
        304 => IndexAnswer::Failed(unexpected_304().into(), None),
        404 if agent::server_error(404, body) == "HTTP 404" => IndexAnswer::OldServer,
        429 => IndexAnswer::Failed(agent::server_error(429, body), Some(Limit(retry_after))),
        status if status >= 400 => IndexAnswer::Failed(agent::server_error(status, body), None),
        _ => match model::parse_index(body) {
            Ok(index) => IndexAnswer::Data(index, usable_etag(etag)),
            Err(e) => IndexAnswer::Failed(e, None),
        },
    }
}

/// The path of a blueprint's detail: the channel and the blueprint's key percent-encoded, like
/// every path segment.
fn detail_path(channel: &str, blueprint: &str) -> String {
    format!("/api/channels/{}/overlay/blueprint/{}", model::path_segment(channel), model::path_segment(blueprint))
}

/// Read one blueprint's detail (`etag`: of the detail held, sent as `If-None-Match`). A key
/// the route does not take is never sent.
fn fetch_detail(client: &Client, channel: &str, blueprint: &str, etag: Option<&str>) -> DetailAnswer {
    if !model::blueprint_usable(blueprint) {
        return DetailAnswer::Failed(tr!("this blueprint cannot be asked for", "não dá para pedir esta blueprint").into(), None);
    }
    match client.request(ApiRequest::Get(&detail_path(channel, blueprint), etag)) {
        Err(e) => DetailAnswer::Failed(e, None),
        Ok(a) => detail_answer(a.status, &a.body, etag.is_some(), a.etag, a.retry_after),
    }
}

/// What an answer of the detail route means: a 304 keeps the detail held, but only when an
/// ETag was sent; a 404 without a message is a server without the route; anything else that
/// is not the detail is a failure with the server's sentence.
fn detail_answer(status: u16, body: &[u8], etag_sent: bool, etag: Option<String>, retry_after: Option<Duration>) -> DetailAnswer {
    match status {
        304 if etag_sent => DetailAnswer::NotModified,
        304 => DetailAnswer::Failed(unexpected_304().into(), None),
        401 => DetailAnswer::Failed(sign_in_hint().into(), None),
        404 if agent::server_error(404, body) == "HTTP 404" => DetailAnswer::Failed(tr!("the site cannot show this blueprint yet", "o site ainda não mostra esta blueprint").into(), None),
        429 => DetailAnswer::Failed(too_many_requests().into(), Some(Limit(retry_after))),
        status if status >= 400 => DetailAnswer::Failed(agent::server_error(status, body), None),
        _ => match model::parse_blueprint_detail(body) {
            Ok(detail) => DetailAnswer::Data(detail, usable_etag(etag)),
            Err(e) => DetailAnswer::Failed(e, None),
        },
    }
}

/// The blueprints card's mission screen against a server without the mission route.
fn no_mission_route() -> &'static str {
    tr!("The site has no blueprints per mission yet.", "O site ainda não tem as blueprints por missão.")
}

/// The path of a mission's blueprints: the channel and the contract id percent-encoded, like
/// every path segment.
fn mission_path(channel: &str, contract: &str) -> String {
    format!("/api/channels/{}/overlay/mission/{}/blueprints", model::path_segment(channel), model::path_segment(contract))
}

/// Read one mission's blueprints (`etag`: of what is held for it, sent as `If-None-Match`).
/// A contract id the route does not take is never sent.
fn fetch_mission(client: &Client, channel: &str, contract: &str, etag: Option<&str>) -> MissionAnswer {
    if !model::contract_usable(contract) {
        return MissionAnswer::Failed(tr!("this mission cannot be asked for", "não dá para pedir esta missão").into(), None);
    }
    match client.request(ApiRequest::Get(&mission_path(channel, contract), etag)) {
        Err(e) => MissionAnswer::Failed(e, None),
        Ok(a) => mission_answer(a.status, &a.body, etag.is_some(), a.etag, a.retry_after),
    }
}

/// What an answer of the mission route means, read like the detail's (see
/// [`detail_answer`]): a 404 without a message is a server without the route (the screen says
/// so); a 404 with one (a contract unknown, or one that gives no blueprint) is the server's
/// sentence.
fn mission_answer(status: u16, body: &[u8], etag_sent: bool, etag: Option<String>, retry_after: Option<Duration>) -> MissionAnswer {
    match status {
        304 if etag_sent => MissionAnswer::NotModified,
        304 => MissionAnswer::Failed(unexpected_304().into(), None),
        401 => MissionAnswer::Failed(sign_in_hint().into(), None),
        404 if agent::server_error(404, body) == "HTTP 404" => MissionAnswer::Failed(no_mission_route().into(), None),
        429 => MissionAnswer::Failed(too_many_requests().into(), Some(Limit(retry_after))),
        status if status >= 400 => MissionAnswer::Failed(agent::server_error(status, body), None),
        _ => match model::parse_mission_blueprints(body) {
            Ok(mission) => MissionAnswer::Data(mission, usable_etag(etag)),
            Err(e) => MissionAnswer::Failed(e, None),
        },
    }
}

/// What the player owns (`GET /api/inventory`, the route the site uses: the signed-in
/// account's inventory, the same on every channel): the quantities and the places of the
/// search's item rows, 0 for an item it does not list. Nothing else asks for quantities (the
/// per-ids `overlay/have` route is no longer used: this answer covers it).
fn fetch_inventory(client: &Client) -> (Result<HashMap<String, model::Owned>, String>, Option<Limit>) {
    match client.request(ApiRequest::Get("/api/inventory", None)) {
        Err(e) => (Err(e), None),
        Ok(a) => inventory_answer(a.status, &a.body, a.retry_after),
    }
}

/// What an answer of the inventory route means; `Some` when it was a 429.
fn inventory_answer(status: u16, body: &[u8], retry_after: Option<Duration>) -> (Result<HashMap<String, model::Owned>, String>, Option<Limit>) {
    match status {
        401 => (Err(sign_in_hint().into()), None),
        429 => (Err(too_many_requests().into()), Some(Limit(retry_after))),
        status if status >= 400 => (Err(agent::server_error(status, body)), None),
        _ => (model::parse_inventory(body), None),
    }
}

/// The widget places and the old single position, as last written to the config.
type SavedLayout = (BTreeMap<String, OverlayWidget>, Option<[f32; 2]>);

/// What the search lists for a query, or why not yet.
enum Results {
    /// The index (or, on an older server, the answer) is on its way: this says so.
    Loading(&'static str),
    Failed(String),
    Ready(Rc<model::Found>),
}

/// The blueprints card's list as last built: the search text, the filter and the revision it
/// was built for (see `Overlay::bp_rows`), and the rows (positions in the index, with whether
/// the account has each).
type BpListCache = (String, model::BpFilter, u64, Rc<Vec<(usize, model::BpHave)>>);

/// What the layout was last clamped for: the screen size, each card's size and the size of
/// the search strip's row (logical px).
type ClampKey = ([f32; 2], [[f32; 2]; widgets::COUNT], [f32; 2]);

// ----- window ---------------------------------------------------------------------------

struct Overlay {
    /// The server's last answer.
    data: Option<OverlayData>,
    /// Its ETag (sent with the next read while `data` is there).
    data_etag: Option<String>,
    /// What is drawn: `data` with the changes still on their way (see `refresh_view`).
    view: Option<OverlayData>,
    /// The changes made in the overlay until a read settles them, and the schedule of every
    /// request.
    engine: sync::Engine,
    /// The endpoint generation `data`, the search answer and every change belong to (0:
    /// none yet); an answer of a later one drops them all, of an earlier one is ignored.
    generation: u64,
    /// watcher.json named another endpoint since the last read: no changes until it is read.
    switching: bool,
    /// The endpoint watcher.json named when last looked at, and the file's time then.
    ident: sync::Ident,
    config_mtime: Option<SystemTime>,
    last_config_check: Instant,
    /// The data thread is gone (it could not start): nothing is ever sent.
    thread_gone: bool,
    conn: Conn,
    cmd_tx: Sender<Cmd>,
    fetch_rx: Receiver<Reply>,
    keys_rx: Receiver<HotkeyEvent>,
    /// Kept for its `Drop`, which stops the key threads and unregisters the keys.
    hotkeys: Hotkeys,
    visible: bool,
    /// Open: scrim, every card, mouse and keyboard. Closed: click-through, pinned cards only.
    open: bool,
    /// The monitor the overlay covers (`None`: the primary one): the game window's, else the
    /// one of the window in front when the overlay started or last opened.
    monitor: Option<Monitor>,
    /// The game's window, looked up at start, on opening and every [`PLACE_CHECK_EVERY`].
    game: Option<Window>,
    /// Opening, frames waited so far: the window is moving to its monitor and the mouse and
    /// keyboard are taken once it is there.
    opening: Option<u32>,
    /// The overlay's own window, once found.
    window: Option<Window>,
    /// The window covers its monitor (or the platform never said, see `place`).
    placed: bool,
    frames: u32,
    last_place_check: Instant,
    /// Frames left in which to give the layered window its attributes (see `os::show_layered`).
    relayer: u8,
    /// Where the cards are and which stay up while closed; `None` until the window covers
    /// its monitor (positions are fractions of it).
    layout: Option<Layout>,
    /// Size of each card the last time it was drawn (keeps a dragged card on screen).
    card_sizes: [egui::Vec2; widgets::COUNT],
    /// How the cards are stacked, back to front: a card pressed comes to the front.
    z: widgets::ZOrder,
    /// What the layout was last clamped for (see `reclamp`).
    clamped_for: Option<ClampKey>,
    /// Which cards and whether the search are shown (the widgets menu's switches; saved).
    shown: Shown,
    /// Where the player dragged the search strip (fractions of the screen; saved); `None`:
    /// under the bar.
    search_pos: Option<[f32; 2]>,
    /// Size of the search strip's row (its frame included) when last drawn: what is kept on
    /// screen and what the results unfold from.
    strip_row: egui::Vec2,
    /// The widgets menu is open.
    menu_open: bool,
    /// A press outside the widgets menu closed it: the click it ends in on the scrim does not
    /// also close the overlay.
    menu_swallow: bool,
    /// When the layout was last written to watcher.json (or tried to be).
    last_save: Option<Instant>,
    /// A layout write did not go through: the next try comes no sooner than this.
    save_retry: Option<Instant>,
    /// A toast already said a layout write failed (once until one goes through).
    save_warned: bool,
    /// From watcher.json until `layout` is built from it: the cards and the old window corner.
    saved: Option<SavedLayout>,
    /// Opacity of the pinned cards and the toasts while closed.
    opacity: f32,
    /// Text size of every card, open or closed (one of `widgets::TEXT_SCALES`).
    text_scale: f32,
    /// The search text, kept while the overlay runs (never saved); against an older server
    /// also its last answer.
    search: Search,
    /// The filter chip chosen under the search field ("Tudo" until another is; never saved).
    search_filter: Filter,
    /// Give the search field the keyboard before the strip is drawn (Ctrl+F).
    focus_search: bool,
    /// What the player owns: the quantities and places of the search's item rows, and the
    /// stacks a move between places goes by.
    inventory: InventoryFetch,
    /// The channel's locations, for the move picker's filter.
    locations: LocationsFetch,
    /// The move picker open, if any.
    mover: Option<Mover>,
    /// Give the move picker's filter field the keyboard before the picker is drawn (it has
    /// just opened).
    focus_mover: bool,
    /// The channel's search index and its ETag.
    index: Option<OverlayIndex>,
    index_etag: Option<String>,
    /// What the last query searched in the index found, with the query and the inventory
    /// revision it was ordered by (see `results`).
    found: RefCell<Option<(String, u64, Rc<model::Found>)>>,
    /// The server has no index (404): searched per query, as older overlays do.
    index_fallback: bool,
    index_in_flight: bool,
    /// When the index is asked for (again).
    index_due: Instant,
    /// Index requests in a row that failed (for the back-off).
    index_failures: u32,
    index_error: Option<String>,
    /// The blueprints card: its search text and filter (never saved), the screens opened over
    /// the list (the one on top is shown; "Voltar" goes back one), the details and the
    /// missions' blueprints read, the choices of the open blueprint's craft (its key and the
    /// choices), and the list for the text, the filter and `bp_rev` (which changes with the
    /// index and with the data), so it is built once per change, not per frame.
    bp_query: String,
    bp_filter: model::BpFilter,
    bp_stack: ScreenStack,
    details: DetailFetch,
    missions: MissionFetch,
    craft: Option<(String, model::CraftState)>,
    bp_rev: u64,
    bp_list: RefCell<Option<BpListCache>>,
    /// `(pinned item id, source item id)`: the items that convert into a pinned one that the
    /// closed overlay shows under it.
    source_pins: HashSet<(String, String)>,
    /// Rows unfolded to show where the item is kept (`card:item`).
    expanded: HashSet<String>,
    /// The opacity slider is under the mouse: the pinned cards show that opacity while open.
    preview_opacity: bool,
    /// A layout change not saved yet, and when it happened.
    dirty: Option<Instant>,
    /// Among the unsaved changes is one the player finished (see `commit`): written without
    /// waiting for it to settle.
    committed: bool,
    toasts: Toasts,
    /// Take the focus on the next frame (the window has just stopped ignoring the mouse).
    focus_next: bool,
    /// When Shift+Tab last asked to open (for `overlay.log`: how long until the focus was asked for).
    open_asked: Option<Instant>,
    /// The window that had the focus before the overlay opened: it gets it back on close.
    prev_focus: Option<PrevFocus>,
    /// The window has had the focus since it opened; losing it afterwards closes it.
    focus_seen: bool,
    last_stop_check: Instant,
    offset: time::UtcOffset,
    /// `--preview`: shown without the game, as on the desktop.
    preview: bool,
    /// Shared with the watch thread: says when the game has gone.
    life: Arc<Life>,
}

/// The move picker of one place line of an unfolded item row (see `Overlay::mover_popup`).
#[derive(Debug, Clone, PartialEq)]
struct Mover {
    /// The line it belongs to (the row's key and the place), so its button toggles it.
    key: String,
    item_id: String,
    /// The item's and the place's names, for its title.
    name: String,
    from: String,
    from_name: String,
    /// What the line holds (whole units): the most that can be moved.
    max: i64,
    /// How many to move (all of the line at first).
    qty: i64,
    /// The text filtering the channel's locations.
    filter: String,
    /// The button it opened from: it hangs under it, and a press there is not "elsewhere".
    anchor: egui::Rect,
}

/// A button press, applied after drawing.
enum Action {
    /// A change sent to the server and shown at once.
    Change(Local),
    /// Fold / unfold the places of a row.
    Toggle(String),
    SearchText(String),
    /// A filter chip under the search field was chosen.
    SearchFilter(Filter),
    ToggleSource(String, String),
    /// A place line's "mover": its picker opens (or closes, when it is the one open).
    OpenMover(Box<Mover>),
    /// The picker's quantity and filter.
    MoverQty(i64),
    MoverFilter(String),
    CloseMover,
    /// The blueprints card's search text and filter.
    BpQuery(String),
    BpFilter(model::BpFilter),
    /// A click on a blueprint's name (its craft), on its "Onde conseguir" or on a mission of
    /// it: that screen opens over the one shown in the blueprints card.
    BpGo(BpScreen),
    /// "Voltar" on a screen of the blueprints card: back to the previous screen.
    BpBack,
    /// The choices of the open blueprint's craft (a quality, an option, the manual switch).
    BpCraft(String, model::CraftState),
}

/// What happened to a card this frame.
#[derive(Default)]
struct CardOut {
    rect: Option<egui::Rect>,
    /// How far its header was dragged.
    drag: egui::Vec2,
    drag_stopped: bool,
    /// How far its corner was dragged.
    resize: egui::Vec2,
    resize_stopped: bool,
    /// The pin box was set to this.
    pin: Option<bool>,
    /// Its ✕ was clicked: the card is switched off, as in the widgets menu.
    close: bool,
    /// The mouse went down on it (it comes to the front).
    pressed: bool,
    actions: Vec<Action>,
}

/// What was pressed in the bar of the open overlay.
#[derive(Default)]
struct BarOut {
    close: bool,
    /// "Ocultar": everything hidden, as Alt+Shift+F9 does.
    hide: bool,
    /// The opacity slider is hovered or dragged.
    preview: bool,
    /// The opacity slider was let go, or changed without a drag (a click, the keyboard).
    opacity_done: bool,
    /// Where the bar is.
    rect: Option<egui::Rect>,
    /// The "Widgets" button was clicked.
    menu_toggle: bool,
    /// Where the "Widgets" button is.
    menu_button: Option<egui::Rect>,
}

/// What happened in the search strip this frame.
#[derive(Default)]
struct StripOut {
    actions: Vec<Action>,
    /// The bottom of the field's row and the chips under it (what is placed under the strip
    /// goes below it).
    bottom: f32,
    /// The strip's top-left corner and the size of its row (frame included).
    top_left: egui::Pos2,
    row: egui::Vec2,
    /// The results unfolded upwards, in an area of their own.
    up: bool,
    /// How far its grip was dragged, and whether it was let go.
    drag: egui::Vec2,
    drag_stopped: bool,
}

/// What was done in the widgets menu this frame.
#[derive(Default)]
struct MenuOut {
    /// A switch was flipped to this.
    switched: Vec<(Part, bool)>,
    /// "mostrar todos" (`true`) or "ocultar todos".
    all: Option<bool>,
    /// "voltar ao lugar" of the search.
    search_home: bool,
    /// Where the menu is.
    rect: Option<egui::Rect>,
}

/// One item row: the name, the number on the right, and what the row offers.
struct Row<'a> {
    item_id: &'a str,
    name: &'a str,
    name_hover: Option<String>,
    qty: String,
    qty_color: Color32,
    qty_hover: Option<&'a str>,
    /// A small note after the number (a material's quality), and its hover.
    qty_note: Option<(String, &'a str)>,
    /// `(have, goal)`: a progress bar under the row.
    goal: Option<(f64, f64)>,
    /// Where it is kept, unfolded on a click while open, and the key that remembers it.
    places: Option<(String, Vec<model::PlaceText>)>,
    /// A pinned item: × unpins it while open.
    unpin: bool,
    indent: bool,
}

impl<'a> Row<'a> {
    fn new(item_id: &'a str, name: &'a str, qty: String) -> Self {
        Self { item_id, name, name_hover: None, qty, qty_color: TEXT, qty_hover: None, qty_note: None, goal: None, places: None, unpin: false, indent: false }
    }
}

impl Overlay {
    fn new(ctx: &egui::Context, cfg: &agent::AgentConfig, game: Option<Window>, monitor: Option<Monitor>, offset: time::UtcOffset, preview: bool, life: Arc<Life>) -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (fetch_tx, fetch_rx) = channel();
        let (keys_tx, keys_rx) = channel();
        let spawned = {
            let ctx = ctx.clone();
            // Without the thread the overlay stays on "carregando…"; nothing better to do.
            std::thread::Builder::new().name("overlay-data".into()).spawn(move || data_thread(cmd_rx, fetch_tx, ctx)).is_ok()
        };
        let hotkeys = hotkey::start(keys_tx, ctx.clone());
        let now = Instant::now();
        Self {
            data: None,
            data_etag: None,
            view: None,
            engine: sync::Engine::new(now),
            generation: 0,
            switching: false,
            ident: target(cfg).0,
            config_mtime: agent::config_mtime(),
            last_config_check: now,
            thread_gone: !spawned,
            conn: Conn::Loading,
            cmd_tx,
            fetch_rx,
            keys_rx,
            hotkeys,
            visible: true,
            open: false,
            monitor,
            game,
            opening: None,
            window: None,
            placed: false,
            frames: 0,
            last_place_check: Instant::now(),
            relayer: RELAYER_FRAMES,
            layout: None,
            card_sizes: [egui::vec2(widgets::CARD_WIDTH, widgets::card_header_height(widgets::text_scale_from(cfg.overlay_text_scale))); widgets::COUNT],
            z: widgets::default_z(),
            clamped_for: None,
            shown: Shown::from_saved(&cfg.overlay_hidden),
            search_pos: widgets::search_pos_from(cfg.overlay_search_pos),
            strip_row: egui::vec2(STRIP_MIN_WIDTH, SEARCH_FIELD_HEIGHT + 2.0 * BAR_MARGIN),
            menu_open: false,
            menu_swallow: false,
            last_save: None,
            save_retry: None,
            save_warned: false,
            saved: Some((cfg.overlay_widgets.clone(), cfg.overlay_pos)),
            opacity: widgets::opacity_from(cfg.overlay_opacity),
            text_scale: widgets::text_scale_from(cfg.overlay_text_scale),
            search: Search::default(),
            search_filter: Filter::Tudo,
            focus_search: false,
            inventory: InventoryFetch::default(),
            locations: LocationsFetch::default(),
            mover: None,
            focus_mover: false,
            index: None,
            index_etag: None,
            found: RefCell::new(None),
            index_fallback: false,
            index_in_flight: false,
            index_due: now,
            index_failures: 0,
            index_error: None,
            bp_query: String::new(),
            bp_filter: model::BpFilter::All,
            bp_stack: ScreenStack::new(),
            details: DetailFetch::default(),
            missions: MissionFetch::default(),
            craft: None,
            bp_rev: 0,
            bp_list: RefCell::new(None),
            source_pins: widgets::source_pins_from(&cfg.overlay_source_pins),
            expanded: HashSet::new(),
            preview_opacity: false,
            dirty: None,
            committed: false,
            toasts: Toasts::default(),
            focus_next: false,
            open_asked: None,
            prev_focus: None,
            focus_seen: false,
            last_stop_check: Instant::now(),
            offset,
            preview,
            life,
        }
    }

    /// The game has gone (closed, crashed or hung): closed at once, the mouse released, the
    /// focus handed to nobody (the window that had it is the dead game's), keys pressed
    /// meanwhile dropped, and the window closes; `run` then starts a fresh overlay that waits.
    fn leave(&mut self, ctx: &egui::Context) {
        self.opening = None;
        self.prev_focus = None;
        self.set_open(ctx, false);
        while self.keys_rx.try_recv().is_ok() {}
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Open (mouse and keyboard, scrim, every card) or close (click-through, pinned cards).
    /// Opening takes the focus on the next frame; closing hands it back to the window that
    /// had it, so the game gets the mouse again.
    fn set_open(&mut self, ctx: &egui::Context, on: bool) {
        if self.open == on {
            return;
        }
        self.open = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!on));
        // The window becomes layered again on the way back to closed.
        self.relayer = RELAYER_FRAMES;
        self.focus_seen = false;
        self.preview_opacity = false;
        self.focus_next = on;
        self.menu_open = false;
        self.menu_swallow = false;
        self.mover = None;
        self.focus_mover = false;
        if !on && let Some(prev) = self.prev_focus.take() {
            os::give_focus(prev);
        }
        ctx.request_repaint();
    }

    /// Show or hide (reads happen only while shown; showing reads at once).
    fn set_visible(&mut self, on: bool) {
        if self.visible != on {
            self.visible = on;
            self.engine.set_visible(on, Instant::now());
        }
    }

    /// Alt+Shift+F9 and "Ocultar": closed, then nothing drawn at all; the process stays, and
    /// the hotkey or the tray icon's "Mostrar o overlay" brings it back.
    fn hide(&mut self, ctx: &egui::Context) {
        self.opening = None;
        self.set_open(ctx, false);
        self.set_visible(false);
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(key) = self.keys_rx.try_recv() {
            match key {
                HotkeyEvent::ToggleVisible => {
                    if self.visible {
                        self.hide(ctx);
                    } else {
                        self.set_visible(true);
                    }
                }
                HotkeyEvent::ToggleInteractive => {
                    overlay_log::line(format_args!("Shift+Tab: open={} opening={} visible={} | in front: {}", self.open, self.opening.is_some(), self.visible, os::describe_foreground()));
                    if self.open {
                        self.set_open(ctx, false);
                    } else if self.opening.is_some() {
                        // Pressed again before the window got there: never mind.
                        self.opening = None;
                    } else {
                        self.open_asked = Some(Instant::now());
                        self.start_opening(ctx);
                    }
                }
            }
        }
        let mut changed = false;
        while let Ok(r) = self.fetch_rx.try_recv() {
            let now = Instant::now();
            if r.generation < self.generation {
                // From an endpoint left behind: dropped.
                self.engine.abandon();
                match r.what {
                    Answer::Search(seq, ..) => self.search.cancel(seq, now),
                    Answer::Inventory(seq, ..) => self.inventory.cancel(seq, now),
                    Answer::Locations(..) => self.locations.cancel(now),
                    Answer::Index(_) => self.index_in_flight = false,
                    Answer::Detail(key, _) => {
                        let open = self.bp_is_open(&key);
                        self.details.cancel(&key, open);
                    }
                    Answer::Mission(contract, _) => {
                        let open = self.mission_is_open(&contract);
                        self.missions.cancel(&contract, open);
                    }
                    Answer::Read(_) | Answer::Write(..) => {}
                }
                continue;
            }
            if r.generation > self.generation {
                self.new_generation(r.generation, now);
            }
            changed = true;
            match r.what {
                Answer::Read(a) => self.on_read(a, now),
                Answer::Write(id, result) => match self.engine.on_write(id, result, now) {
                    WriteEffect::Nothing => {}
                    // The number goes back, and the toast says why.
                    WriteEffect::Failed(e) => self.toasts.warn(write_failed(), &e, now),
                    // Marked until the next read says what the server has.
                    WriteEffect::Unknown(e) => self.toasts.warn(write_unknown(), &e, now),
                },
                Answer::Search(seq, answer, limit) => {
                    self.engine.on_search(limit, now);
                    self.search.answer(seq, answer);
                }
                Answer::Inventory(seq, answer, limit) => {
                    let mark = self.engine.on_inventory(limit, now);
                    match answer {
                        Ok(owned) => {
                            self.inventory.answer(seq, owned, mark, now);
                        }
                        Err(e) => self.inventory.failed(seq, e, limit.is_some(), now),
                    }
                }
                Answer::Index(answer) => {
                    let limit = match &answer {
                        IndexAnswer::Failed(_, limit) => *limit,
                        _ => None,
                    };
                    self.engine.on_index(limit, now);
                    self.index_in_flight = false;
                    self.on_index(answer, now);
                }
                Answer::Locations(answer, limit) => {
                    self.engine.on_locations(limit, now);
                    match answer {
                        Ok(list) => self.locations.answer(list),
                        Err(e) => self.locations.failed(e, limit.is_some(), now),
                    }
                }
                Answer::Detail(key, answer) => {
                    let limit = match &answer {
                        DetailAnswer::Failed(_, limit) => *limit,
                        _ => None,
                    };
                    self.engine.on_detail(limit, now);
                    let open = self.bp_is_open(&key);
                    self.details.answer(&key, answer, open);
                }
                Answer::Mission(contract, answer) => {
                    let limit = match &answer {
                        MissionAnswer::Failed(_, limit) => *limit,
                        _ => None,
                    };
                    self.engine.on_mission(limit, now);
                    let open = self.mission_is_open(&contract);
                    self.missions.answer(&contract, answer, open);
                }
            }
        }
        if changed {
            self.refresh_view();
        }
    }

    /// Another server, account or channel answered: nothing shown, searched, seen or changed
    /// for the one before applies here.
    fn new_generation(&mut self, generation: u64, now: Instant) {
        self.generation = generation;
        self.data = None;
        self.data_etag = None;
        self.view = None;
        self.engine.reset(now);
        self.search.reset(now);
        self.set_index(None, None);
        self.index_fallback = false;
        self.index_in_flight = false;
        self.index_due = now;
        self.index_failures = 0;
        self.index_error = None;
        self.inventory.reset();
        self.locations.reset();
        self.details.reset();
        self.missions.reset();
        self.bp_stack.clear();
        self.craft = None;
        self.mover = None;
        self.toasts.reset_identity();
        self.expanded.clear();
        self.conn = Conn::Loading;
    }

    fn on_read(&mut self, a: ReadAnswer, now: Instant) {
        self.switching = false;
        let settled = self.engine.on_read(a.outcome(), a.limit(), now);
        match a {
            ReadAnswer::Data(d, etag) => {
                self.toasts.feed(&d, self.shown.get(Part::Alerts), now);
                // Open, the card shows the choices: no toast.
                self.toasts.feed_choices(&d.choices, !self.open, now);
                self.settled(&settled, &d, now);
                // The source pins of an item no longer pinned go, silently.
                if widgets::prune_source_pins(&mut self.source_pins, d.tracked.iter().map(|t| t.item_id.as_str())) {
                    self.dirty = Some(now);
                }
                self.data = Some(d);
                self.data_etag = etag;
                self.conn = Conn::Ok;
                // The blueprints had may have changed: the card's list is built again.
                self.bp_rev += 1;
            }
            // What is on screen is still the answer (the ETag goes only with data held).
            ReadAnswer::NotModified if self.data.is_some() => {
                if !settled.is_empty()
                    && let Some(d) = self.data.clone()
                {
                    self.settled(&settled, &d, now);
                }
                self.conn = Conn::Ok;
            }
            // Nothing held to keep: read it whole at once.
            ReadAnswer::NotModified => {
                self.data_etag = None;
                self.engine.read_now(now);
            }
            ReadAnswer::SignedOut => {
                self.data = None;
                self.data_etag = None;
                self.engine.clear_ops();
                self.conn = Conn::SignedOut;
                self.toasts.unprime();
            }
            ReadAnswer::NoRoute => {
                self.data = None;
                self.data_etag = None;
                self.engine.clear_ops();
                self.conn = Conn::NoRoute;
                self.toasts.unprime();
            }
            // Keep the last data: a hiccup must not blank the overlay.
            ReadAnswer::Unreachable(e, _) => self.conn = Conn::Unreachable(e),
        }
        if !settled.is_empty() {
            self.inventory.refetch(now);
        }
    }

    /// The changes read `d` settled: an unpin the server answered 404 that `d` still shows
    /// says so (once). An inventory answer older than a change gets it for good; one whose
    /// write got no answer is left to the inventory's next read (asked after every settled
    /// write anyway).
    fn settled(&mut self, ops: &[sync::Op], d: &OverlayData, now: Instant) {
        for op in ops {
            if op.verify && model::still_pinned(d, &op.change) {
                let body = match op.change {
                    Local::UnpinMission { .. } => tr!("the server did not unpin the mission", "o servidor não desafixou a missão"),
                    _ => tr!("the server did not unpin the item", "o servidor não desafixou o item"),
                };
                self.toasts.warn(write_failed(), body, now);
            }
            let Some(n) = op.finished() else { continue };
            let unknown = matches!(op.state, sync::OpState::Unknown(_));
            if n > self.inventory.mark() && !unknown {
                self.inventory.bake(&op.change);
            }
        }
    }

    /// The search index request came back (see `index_answer`).
    fn on_index(&mut self, answer: IndexAnswer, now: Instant) {
        match answer {
            IndexAnswer::Data(index, etag) => {
                self.set_index(Some(index), etag);
                self.index_fallback = false;
                self.index_failures = 0;
                self.index_error = None;
                self.index_due = now + INDEX_REVALIDATE;
            }
            IndexAnswer::NotModified if self.index.is_some() => {
                self.index_failures = 0;
                self.index_error = None;
                self.index_due = now + INDEX_REVALIDATE;
            }
            // Nothing held to keep (cannot happen: the ETag goes only with an index): ask anew.
            IndexAnswer::NotModified => {
                self.set_index(None, None);
                self.index_due = now;
            }
            IndexAnswer::OldServer => {
                self.set_index(None, None);
                self.index_fallback = true;
                self.index_failures = 0;
                self.index_error = None;
                // Looked at again now and then, in case the server is updated.
                self.index_due = now + INDEX_REVALIDATE;
            }
            IndexAnswer::Failed(error, limit) => {
                self.index_error = Some(error);
                if limit.is_some() {
                    // The engine holds the index back for the server's wait.
                    self.index_due = now;
                } else {
                    self.index_failures = self.index_failures.saturating_add(1);
                    self.index_due = now + sync::next_delay(Outcome::Unreachable, self.index_failures);
                }
            }
        }
    }

    /// Hold `index` (and its ETag); the cached results and the blueprints card's list go with
    /// the old one.
    fn set_index(&mut self, index: Option<OverlayIndex>, etag: Option<String>) {
        self.index_etag = if index.is_some() { etag } else { None };
        self.index = index;
        *self.found.borrow_mut() = None;
        self.bp_rev += 1;
    }

    /// The blueprints card shows a screen of blueprint `key` (its craft or its missions).
    fn bp_is_open(&self, key: &str) -> bool {
        widgets::open_blueprint(&self.bp_stack) == Some(key)
    }

    /// The blueprints card shows the screen of mission `contract`.
    fn mission_is_open(&self, contract: &str) -> bool {
        widgets::open_contract(&self.bp_stack) == Some(contract)
    }

    /// The blueprints card shows another screen (`forward`: one just opened; else "Voltar"),
    /// the one before it being about blueprint `before_bp` or mission `before_contract`. A
    /// blueprint's screen asks for its detail unless the screen before was about that same
    /// blueprint; a mission's screen asks for its blueprints. Going back, only what is not
    /// held is asked for. A screen about neither asks nothing more for the one left.
    fn screen_opened(&mut self, before_bp: Option<&str>, before_contract: Option<&str>, forward: bool) {
        match widgets::open_blueprint(&self.bp_stack).map(str::to_string) {
            Some(key) if before_bp == Some(key.as_str()) => {}
            Some(key) if forward || !self.details.has(&key) => self.details.open(&key),
            Some(_) | None => self.details.close(),
        }
        match widgets::open_contract(&self.bp_stack).map(str::to_string) {
            Some(contract) if !forward && before_contract == Some(contract.as_str()) => {}
            Some(contract) if forward || !self.missions.has(&contract) => self.missions.open(&contract),
            Some(_) | None => self.missions.close(),
        }
    }

    /// The rows as drawn: the server's answer with the changes still on their way.
    fn refresh_view(&mut self) {
        self.view = self.data.as_ref().map(|d| model::view(d, self.engine.changes()));
    }

    /// Changes may be made: the data shown belongs to the endpoint the requests go to.
    fn writable(&self) -> bool {
        !self.thread_gone && !self.switching && self.generation > 0
    }

    /// The controls that make changes are enabled: [`Overlay::writable`], with room for
    /// another change.
    fn can_change(&self) -> bool {
        self.writable() && !self.engine.full()
    }

    /// Now and then: watcher.json still names the endpoint the overlay shows. Another one
    /// (a sign-in or out, another account, server or channel) is read at once, and no change
    /// can be made until it is.
    fn watch_config(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_config_check) < CONFIG_CHECK_EVERY {
            return;
        }
        self.last_config_check = now;
        let mtime = agent::config_mtime();
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;
        let (ident, _) = target(&agent::load_config().unwrap_or_default());
        if ident != self.ident {
            self.ident = ident;
            self.switching = true;
            self.engine.read_now(now);
        }
    }

    /// Hand the data thread the next request (one at a time, see `sync::Engine`), and ask
    /// for a frame when the next one may be due.
    fn pump(&mut self, ctx: &egui::Context, now: Instant) {
        if self.thread_gone {
            return;
        }
        // Closed, nothing is listed. Open, the index (with the search or the blueprints card
        // on) and the inventory (the quantities and places of the search's item rows, the
        // stacks a move goes by) are kept ready; the detail of the blueprint whose screen is
        // open, or the blueprints of the mission whose screen is; the channel's locations once
        // something is typed in the move picker's filter.
        let searching = self.open && self.shown.get(Part::Search);
        // The blueprints card lists the index too, and asks for what its screen on top draws,
        // only while the card is on screen.
        let bp_card = self.open && self.shown.get(Part::Card(Widget::Blueprints));
        // An older server is searched per query.
        let fallback = searching && self.index_fallback;
        if fallback {
            self.search.wake(now);
        }
        let wants_index = (searching || bp_card) && now >= self.index_due && !self.index_in_flight;
        let wants_detail = bp_card && widgets::open_blueprint(&self.bp_stack).is_some();
        let wants_mission = bp_card && widgets::open_contract(&self.bp_stack).is_some();
        let wants_inventory = self.open;
        let wants_locations = self.open && self.mover.as_ref().is_some_and(|m| !m.filter.trim().is_empty());
        let index_in_flight = &mut self.index_in_flight;
        let details = &mut self.details;
        let missions = &mut self.missions;
        let inventory = &mut self.inventory;
        let locations = &mut self.locations;
        let search = &mut self.search;
        if let Some(job) = self.engine.next_job_with_mission(
            now,
            || {
                if wants_index {
                    *index_in_flight = true;
                }
                wants_index
            },
            || if wants_detail { details.due() } else { None },
            || if wants_mission { missions.due() } else { None },
            || if wants_inventory { inventory.due(now) } else { None },
            || wants_locations && locations.due(now),
            || if fallback { search.due(now) } else { None },
        ) {
            // The ETag of what the window holds, for a 304 to keep it; never on the read that
            // settles writes (its answer must have them, whatever the server's ETag says).
            let etag = match &job {
                Job::Read if self.engine.settling() => None,
                Job::Read => self.data.as_ref().and(self.data_etag.clone()),
                Job::Index => self.index.as_ref().and(self.index_etag.clone()),
                Job::Detail(key) => self.details.etag(key),
                Job::Mission(contract) => self.missions.etag(contract),
                _ => None,
            };
            if self.cmd_tx.send(Cmd { generation: self.generation, job, etag }).is_err() {
                // Without the thread nothing more can be sent; the overlay stays as it is.
                self.thread_gone = true;
                self.engine.abandon();
            }
        }
        if let Some(wait) = self.engine.wait(now) {
            ctx.request_repaint_after(wait);
        }
        // A search, the inventory or the index waiting for their turn: looked at again then
        // (the answer of a request on its way wakes the window anyway).
        if self.engine.idle() {
            let soon = |d: Duration| d.max(Duration::from_millis(30));
            if fallback && let Some(wait) = self.search.wait(now) {
                ctx.request_repaint_after(soon(wait.max(self.engine.search_ready_in(now))));
            }
            if wants_inventory && let Some(wait) = self.inventory.wait(now) {
                ctx.request_repaint_after(soon(wait.max(self.engine.inventory_ready_in(now))));
            }
            if wants_locations && let Some(wait) = self.locations.wait(now) {
                ctx.request_repaint_after(soon(wait.max(self.engine.locations_ready_in(now))));
            }
            if (wants_detail && self.details.waiting()) || (wants_mission && self.missions.waiting()) {
                ctx.request_repaint_after(soon(self.engine.detail_ready_in(now)));
            }
            if (searching || bp_card) && !self.index_in_flight {
                let wait = self.index_due.saturating_duration_since(now).max(self.engine.index_ready_in(now));
                ctx.request_repaint_after(soon(wait));
            }
        }
    }

    /// What the search lists for query `q` (see `widgets::listing`): searched here, on this
    /// very frame, in the index (from the cache while the query and the inventory it was
    /// ordered by are the same), or on an older server its answer to the last query; else why
    /// there is nothing to list yet.
    fn results(&self, q: &str) -> Results {
        let held = self.inventory.held();
        if let Some(index) = &self.index {
            let revision = self.inventory.revision();
            let mut cache = self.found.borrow_mut();
            if let Some((cached, rev, found)) = cache.as_ref()
                && cached.as_str() == q
                && *rev == revision
            {
                return Results::Ready(found.clone());
            }
            let found = Rc::new(model::search_index(index, q, held));
            *cache = Some((q.to_string(), revision, found.clone()));
            return Results::Ready(found);
        }
        if self.index_fallback {
            return match self.search.shown() {
                Some(Ok(answer)) => Results::Ready(Rc::new(model::found_from_answer(answer, held))),
                Some(Err(error)) => Results::Failed(error.clone()),
                None => Results::Loading(tr!("searching…", "buscando…")),
            };
        }
        match &self.index_error {
            Some(error) => Results::Failed(error.clone()),
            None => Results::Loading(tr!("loading search…", "carregando busca…")),
        }
    }

    /// The items and missions pinned per the data with the changes on their way.
    fn pinned(&self) -> (HashSet<String>, HashSet<String>) {
        let mut items = HashSet::new();
        let mut missions = HashSet::new();
        if let Some(d) = &self.view {
            items.extend(d.tracked.iter().map(|item| item.item_id.clone()));
            missions.extend(d.missions.iter().map(|mission| mission.mission_id.clone()));
        }
        (items, missions)
    }

    /// Now and then: the tray icon dropped `overlay.show` (shown again, the request used up),
    /// or the settings window or the tray dropped `overlay.stop` (`true`: close).
    fn watch_requests(&mut self) -> bool {
        if self.last_stop_check.elapsed() < Duration::from_millis(400) {
            return false;
        }
        self.last_stop_check = Instant::now();
        let show = agent::overlay_show_path();
        if show.exists() {
            let _ = std::fs::remove_file(&show);
            self.set_visible(true);
        }
        agent::overlay_stop_path().exists()
    }

    /// First frames: make the window cover the game's monitor. Gives up after
    /// [`PLACE_FRAMES`] (the platform never says); the cards then use whatever size it has.
    fn place(&mut self, ctx: &egui::Context) {
        if self.placed {
            return;
        }
        self.frames += 1;
        ctx.request_repaint();
        if self.cover_monitor(ctx) || self.frames > PLACE_FRAMES {
            self.placed = true;
            self.last_place_check = Instant::now();
        }
    }

    /// Move and size the window over its monitor; `true` once it is there. Windows works in
    /// physical pixels on the real window, so a monitor with another scale comes out right.
    #[cfg(windows)]
    fn cover_monitor(&mut self, _ctx: &egui::Context) -> bool {
        if self.window.is_none() {
            self.window = os::own_window();
        }
        let (Some(w), Some(m)) = (self.window, os::monitor_rect(self.monitor)) else { return false };
        let want = os::cover_rect(m);
        if os::window_rect(w) == Some(want) {
            return true;
        }
        os::cover(w, want);
        false
    }

    /// Elsewhere only the size of the window's monitor is known: cover it from the origin.
    #[cfg(not(windows))]
    fn cover_monitor(&mut self, ctx: &egui::Context) -> bool {
        let Some(m) = ctx.input(|i| i.viewport().monitor_size) else { return false };
        if (ctx.screen_rect().size() - m).length() < 2.0 {
            return true;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(m));
        false
    }

    /// The hotkey asked to open: look for the game's window again and start moving to the
    /// monitor the overlay belongs on (`advance_opening` finishes the job). Still
    /// click-through meanwhile.
    fn start_opening(&mut self, ctx: &egui::Context) {
        let game = os::game_window();
        // Off preview the overlay opens only over the game (without it, it is about to leave).
        if !self.preview && game.is_none() {
            overlay_log::line(format_args!("not opening: no game window"));
            return;
        }
        // Shift+Tab is everybody's key: it opens the overlay only while the game is in front.
        if !self.preview && !game.is_some_and(os::is_foreground) {
            overlay_log::line(format_args!("not opening: the game ({}) is not in front", os::describe_window(game)));
            return;
        }
        self.set_visible(true);
        self.game = game;
        self.monitor = target_monitor(self.game);
        self.opening = Some(0);
        ctx.request_repaint();
    }

    /// While opening: once the window covers its monitor and egui sees the new size (or after
    /// [`PLACE_FRAMES`], the platform never saying), clamp the cards into that screen, then
    /// open: the mouse now, the keyboard on the next frame.
    fn advance_opening(&mut self, ctx: &egui::Context) {
        let Some(frames) = self.opening else { return };
        let frames = frames + 1;
        let there = self.cover_monitor(ctx) && self.screen_matches(ctx);
        if !there && frames <= PLACE_FRAMES {
            self.opening = Some(frames);
            ctx.request_repaint();
            return;
        }
        self.opening = None;
        self.placed = true;
        self.last_place_check = Instant::now();
        self.ensure_layout(ctx);
        self.clamped_for = None;
        self.reclamp(ctx);
        self.set_open(ctx, true);
    }

    /// egui's screen is as big as the window (it learns a new size a frame after the move).
    #[cfg(windows)]
    fn screen_matches(&self, ctx: &egui::Context) -> bool {
        let Some(r) = self.window.and_then(os::window_rect) else { return false };
        let screen = ctx.screen_rect().size() * ctx.pixels_per_point();
        (screen - egui::vec2((r[2] - r[0]) as f32, (r[3] - r[1]) as f32)).length() < 2.0
    }

    #[cfg(not(windows))]
    fn screen_matches(&self, _ctx: &egui::Context) -> bool {
        true
    }

    /// Closed, the overlay draws only while the game is in front (without a game: only in
    /// preview, see [`overlay_game::closed_draws`]).
    fn closed_shown(&self) -> bool {
        overlay_game::closed_draws(self.preview, self.game.is_some(), self.game.is_some_and(os::is_foreground))
    }

    /// Now and then: the window still covers its monitor (a resolution or scale change), and
    /// that is still the game's monitor (the game was moved, or started after the overlay).
    fn keep_covering(&mut self) {
        if !self.placed || self.opening.is_some() || self.last_place_check.elapsed() < PLACE_CHECK_EVERY {
            return;
        }
        self.last_place_check = Instant::now();
        self.game = os::game_window();
        if let Some(m) = self.game.and_then(os::window_monitor) {
            self.monitor = Some(m);
        }
        if let (Some(w), Some(m)) = (self.window, os::monitor_rect(self.monitor)) {
            let want = os::cover_rect(m);
            if os::window_rect(w) != Some(want) {
                os::cover(w, want);
            }
        }
    }

    /// Once the window covers its monitor: the cards from watcher.json, clamped into it.
    fn ensure_layout(&mut self, ctx: &egui::Context) {
        if self.layout.is_some() || !self.placed {
            return;
        }
        let screen = ctx.screen_rect().size();
        let (saved, legacy) = self.saved.take().unwrap_or_default();
        // The old window's corner was in desktop coordinates (logical px): make it relative
        // to this monitor.
        let ppp = ctx.pixels_per_point();
        let origin = os::monitor_rect(self.monitor).map_or([0.0, 0.0], |r| [r[0] as f32 / ppp, r[1] as f32 / ppp]);
        let legacy = legacy.map(|p| [p[0] - origin[0], p[1] - origin[1]]);
        let size = [screen.x, screen.y];
        let mut layout = widgets::resolve(&saved, legacy, size);
        // Every header whole on screen at the text size in use (a larger text, a taller
        // header); the measured sizes take over from the first frame drawn.
        widgets::clamp_layout(&mut layout, &[[widgets::CARD_WIDTH, widgets::card_header_height(self.text_scale)]; widgets::COUNT], size);
        self.layout = Some(layout);
    }

    /// The frame after opening: bring the window to the front, remembering who had the focus.
    /// When Windows refuses (an elevated game, another foreground lock), the overlay must not
    /// keep the mouse without the keyboard: back to click-through and closed at once, with a
    /// toast saying so.
    fn take_focus_if_due(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.focus_next) || !self.open {
            return;
        }
        let got = os::take_focus(self.window);
        let waited = self.open_asked.take().map_or("?".to_string(), |t| format!("{}ms", t.elapsed().as_millis()));
        overlay_log::line(format_args!("focus {} {waited} after Shift+Tab | {}", if got.ok { "taken" } else { "REFUSED" }, got.detail));
        if got.ok {
            self.prev_focus = got.prev;
            return;
        }
        // Nobody to hand the focus back to: it never left.
        self.prev_focus = None;
        self.set_open(ctx, false);
        self.toasts.warn("Overlay", focus_refused(), Instant::now());
    }

    /// Open and then left (Alt+Tab, a click on another monitor): close, without handing the
    /// focus anywhere, since another window has it already.
    fn watch_focus(&mut self, ctx: &egui::Context) {
        if !self.open || self.focus_next {
            return;
        }
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => self.focus_seen = true,
            Some(false) if self.focus_seen => {
                overlay_log::line(format_args!("closing: the focus left | in front: {}", os::describe_foreground()));
                self.prev_focus = None;
                self.set_open(ctx, false);
            }
            _ => {}
        }
    }

    /// Save the layout once it has stopped changing for a moment, and not sooner than
    /// [`MIN_SAVE_GAP`] after the last write (see [`save_wait`]); a finished change (see
    /// `commit`) only waits for that gap ([`commit_wait`]).
    fn save_layout_if_due(&mut self, ctx: &egui::Context) {
        let Some(since) = self.dirty else { return };
        // A write that did not go through is tried again a little later.
        if let Some(at) = self.save_retry {
            let now = Instant::now();
            if now < at {
                ctx.request_repaint_after(at - now);
                return;
            }
        }
        let saved_ago = self.last_save.map(|t| t.elapsed());
        let wait = if self.committed { commit_wait(saved_ago) } else { save_wait(since.elapsed(), saved_ago) };
        if wait > Duration::ZERO {
            ctx.request_repaint_after(wait);
        } else {
            self.save_layout();
        }
    }

    /// A change the player finished (a card dropped, a pin box clicked, the opacity slider let
    /// go): written now, or once [`MIN_SAVE_GAP`] has passed since the last write, and not
    /// left to `Drop`: when the game takes the graphics device down, a stuck UI is ended
    /// without it (see `overlay_game`).
    fn commit(&mut self, ctx: &egui::Context) {
        self.dirty = Some(Instant::now());
        self.committed = true;
        self.save_layout_if_due(ctx);
    }

    /// Only the cards, the opacity, the text size, the source pins, the search strip's place
    /// and the widget switches are written, over a fresh read of watcher.json; nothing at all
    /// when that file cannot be read (it would lose the sign-in).
    /// The change counts as saved only once the file has it: otherwise it is kept and tried
    /// again after [`SAVE_RETRY`] (a failure says so once in a toast).
    fn save_layout(&mut self) {
        let Some(l) = &self.layout else {
            self.dirty = None;
            self.committed = false;
            return;
        };
        let now = Instant::now();
        self.last_save = Some(now);
        let source_pins = widgets::source_pins_config(&self.source_pins);
        let switches = agent::OverlaySwitches { search_pos: self.search_pos, shown: self.shown.to_config() };
        match agent::save_overlay_state(&widgets::to_config(l), self.opacity, self.text_scale, &source_pins, &switches) {
            Ok(agent::LayoutSave::Written | agent::LayoutSave::Unchanged) => {
                self.dirty = None;
                self.committed = false;
                self.save_retry = None;
                self.save_warned = false;
            }
            // No watcher.json (signed out on this PC), or one being written right now.
            Ok(agent::LayoutSave::NoConfig) => self.save_retry = Some(now + SAVE_RETRY),
            Err(e) => {
                self.save_retry = Some(now + SAVE_RETRY);
                if !std::mem::replace(&mut self.save_warned, true) {
                    self.toasts.warn("Overlay", &trf!("could not save the cards: {e}", "não deu para salvar os cartões: {e}"), now);
                }
            }
        }
    }

    /// Whenever the screen, a card's measured size or the search strip's changed: every card
    /// back inside the screen with its own size (a card that grew near the bottom, a smaller
    /// resolution), and the search strip's row too. The layout is marked changed only when a
    /// place actually moved.
    fn reclamp(&mut self, ctx: &egui::Context) {
        let Some(layout) = &mut self.layout else { return };
        let screen = ctx.screen_rect().size();
        let key = ([screen.x, screen.y], self.card_sizes.map(|s| [s.x, s.y]), [self.strip_row.x, self.strip_row.y]);
        if self.clamped_for == Some(key) {
            return;
        }
        self.clamped_for = Some(key);
        let search = widgets::clamp_search(self.search_pos, key.2, key.0);
        let search_moved = search != self.search_pos;
        self.search_pos = search;
        if widgets::clamp_layout(layout, &key.1, key.0) || search_moved {
            self.dirty = Some(Instant::now());
            ctx.request_repaint();
        }
    }

    /// Queue the changes made this frame and draw them at once (see `sync::Engine`).
    fn apply(&mut self, actions: Vec<Action>, now: Instant) {
        if actions.is_empty() {
            return;
        }
        for a in actions {
            match a {
                Action::Toggle(key) => {
                    if !self.expanded.remove(&key) {
                        self.expanded.insert(key);
                    }
                }
                // Neither the text nor the chip chosen is ever saved; an emptied field starts
                // again from "Tudo".
                Action::SearchText(text) => {
                    self.search.text = widgets::cut_search_text(text);
                    self.search.edited(now);
                    if self.search.text.trim().is_empty() {
                        self.search_filter = Filter::Tudo;
                    }
                }
                Action::SearchFilter(filter) => self.search_filter = filter,
                Action::OpenMover(m) => {
                    let same = self.mover.as_ref().is_some_and(|open| open.key == m.key);
                    self.mover = if same { None } else { Some(*m) };
                    // A picker that opens takes the keyboard for its filter at once.
                    self.focus_mover = self.mover.is_some();
                }
                Action::MoverQty(n) => {
                    if let Some(m) = &mut self.mover {
                        m.qty = n.clamp(1, m.max.max(1));
                    }
                }
                Action::MoverFilter(text) => {
                    if let Some(m) = &mut self.mover {
                        m.filter = widgets::cut_search_text(text);
                    }
                }
                Action::CloseMover => self.mover = None,
                // Neither the card's text nor its filter is ever saved.
                Action::BpQuery(text) => self.bp_query = widgets::cut_search_text(text),
                Action::BpFilter(filter) => self.bp_filter = filter,
                // A screen opens over the one shown. Opening a blueprint's screen from
                // anything but a screen of that same blueprint asks for its detail, and opening
                // a mission's screen for its blueprints (again, with the ETag, when read before).
                Action::BpGo(screen) => {
                    let (blueprint, contract) = (widgets::open_blueprint(&self.bp_stack).map(str::to_string), widgets::open_contract(&self.bp_stack).map(str::to_string));
                    widgets::screen_push(&mut self.bp_stack, screen);
                    self.screen_opened(blueprint.as_deref(), contract.as_deref(), true);
                }
                // Back one screen: what it draws is asked for again only when nothing is held
                // for it (it was drawn a moment ago).
                Action::BpBack => {
                    let (blueprint, contract) = (widgets::open_blueprint(&self.bp_stack).map(str::to_string), widgets::open_contract(&self.bp_stack).map(str::to_string));
                    widgets::screen_back(&mut self.bp_stack);
                    self.screen_opened(blueprint.as_deref(), contract.as_deref(), false);
                }
                Action::BpCraft(key, state) => self.craft = Some((key, state)),
                Action::ToggleSource(item, source) => {
                    if widgets::toggle_source_pin(&mut self.source_pins, &item, &source) {
                        self.mark_finished();
                    }
                }
                // Only for the endpoint on screen, and with ids the server can be sent whole.
                Action::Change(change) => {
                    if self.writable() && model::change_usable(&change) && !self.engine.click(change, now) {
                        self.toasts.warn(write_failed(), tr!("too many changes on their way; wait a moment", "alterações demais a caminho; espere um pouco"), now);
                    }
                }
            }
        }
        self.refresh_view();
    }

    /// A finished change of the source pins or a switch: saved without waiting for it to
    /// settle (`save_layout_if_due` keeps the one-second gap).
    fn mark_finished(&mut self) {
        self.dirty = Some(Instant::now());
        self.committed = true;
    }

    /// Open: Ctrl+F goes to the search field (switching the search on); Esc closes the
    /// widgets menu first, then empties the search field, and closes the overlay only after
    /// that (see `widgets::esc_step`).
    fn keys_while_open(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            if self.shown.set(Part::Search, true) {
                self.mark_finished();
            }
            self.focus_search = true;
            ctx.request_repaint();
        }
        // The move picker closes first.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.mover.take().is_none() {
            // A text kept in a search switched off is not on screen: nothing to empty.
            let text = if self.shown.get(Part::Search) { self.search.text.as_str() } else { "" };
            if ctx.memory(|m| m.has_focus(bp_search_id())) {
                // In the blueprints card's field: Esc empties it and leaves it.
                self.bp_query.clear();
                ctx.memory_mut(|m| m.surrender_focus(bp_search_id()));
            } else {
                match widgets::esc_step(self.menu_open, text) {
                    EscStep::Menu => self.menu_open = false,
                    EscStep::Search => {
                        self.search.clear();
                        self.search_filter = Filter::Tudo;
                        ctx.memory_mut(|m| m.surrender_focus(search_id()));
                    }
                    EscStep::Close => self.set_open(ctx, false),
                }
            }
        }
        // Before the strip is drawn: the field gets the focus this very frame.
        if self.open && std::mem::take(&mut self.focus_search) {
            ctx.memory_mut(|m| m.request_focus(search_id()));
        }
    }

    // ----- drawing ----------------------------------------------------------------------

    /// Whether a card has anything to draw (closed, an empty card is not drawn at all). The
    /// blueprints card is a tool to browse, not a status to glance at: never drawn closed.
    fn has_content(&self, w: Widget, here: &[model::HereRow]) -> bool {
        let Some(d) = &self.view else { return false };
        match w {
            Widget::Fixados => !d.tracked.is_empty(),
            Widget::Falta => !d.missing.is_empty(),
            Widget::Aqui => !here.is_empty(),
            Widget::Registros => !d.notices.is_empty() || d.pending > 0.0,
            Widget::Missoes => !d.missions.is_empty(),
            Widget::Blueprints => false,
        }
    }

    /// Every card switched on in the widgets menu while open, the pinned ones among them with
    /// something to show while closed (a card switched off is not drawn at all, its pin kept);
    /// applies the drags, resizes and pin changes and returns the row button presses.
    fn draw_cards(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let Some(layout) = self.layout else { return Vec::new() };
        let screen = ctx.screen_rect();
        let size = [screen.width(), screen.height()];
        let here = self.view.as_ref().map(model::here_rows).unwrap_or_default();
        let mut actions = Vec::new();
        let mut changed = false;
        let mut drawn: Vec<usize> = Vec::new();
        let mut pressed: Vec<usize> = Vec::new();
        for w in Widget::ALL {
            let i = w.index();
            let place = layout[i];
            if !self.shown.get(Part::Card(w)) || (!self.open && !(place.pinned && self.has_content(w, &here))) {
                continue;
            }
            let out = self.card(ctx, w, place, screen, &here);
            drawn.push(i);
            if out.pressed {
                pressed.push(i);
            }
            if let Some(r) = out.rect {
                self.card_sizes[i] = r.size();
            }
            let card = self.card_sizes[i];
            if let Some(l) = &mut self.layout {
                if out.drag != egui::Vec2::ZERO {
                    let moved = [l[i].frac[0] + out.drag.x / size[0], l[i].frac[1] + out.drag.y / size[1]];
                    l[i].frac = widgets::clamp_frac(moved, [card.x, card.y], size);
                }
                if out.resize != egui::Vec2::ZERO
                    && let Some(s) = widgets::resized(&l[i], [card.x, card.y], [out.resize.x, out.resize.y], size)
                {
                    l[i].size = Some(s);
                    // Saved once it stays put (or right away on release, below).
                    self.dirty = Some(Instant::now());
                }
                if let Some(p) = out.pin {
                    l[i].pinned = p;
                }
            }
            // Its ✕: switched off exactly as the widgets menu does it (its pin kept).
            let closed = out.close && self.shown.set(Part::Card(w), false);
            changed |= out.drag_stopped || out.resize_stopped || out.pin.is_some() || closed;
            actions.extend(out.actions);
        }
        // The card pressed comes to the front (where cards overlap, the one already in front
        // got the press), and every card is stacked in that order.
        if let Some(top) = self.z.iter().rev().find(|&&i| pressed.contains(&i)).copied() {
            widgets::raise(&mut self.z, top);
        }
        for i in self.z {
            if drawn.contains(&i) {
                ctx.memory_mut(|m| m.areas_mut().move_to_top(card_layer(Widget::ALL[i])));
            }
        }
        // A drop, a release of the corner, a pin box or a ✕: finished changes, saved right away.
        if changed {
            self.commit(ctx);
        }
        actions
    }

    fn card(&self, ctx: &egui::Context, w: Widget, place: CardPlace, screen: egui::Rect, here: &[model::HereRow]) -> CardOut {
        let mut out = CardOut::default();
        let pos = screen.min + egui::vec2(place.frac[0] * screen.width(), place.frac[1] * screen.height());
        let sized = place.size.and_then(|f| widgets::size_px(f, [screen.width(), screen.height()]));
        // Closed: the chosen opacity. Open: full, unless the slider is being tried out.
        let faded = !self.open || (self.preview_opacity && place.pinned);
        let shown = egui::Area::new(card_id(w))
            .order(egui::Order::Middle)
            .fixed_pos(pos)
            .interactable(self.open)
            .show(ctx, |ui| {
                if faded {
                    ui.set_opacity(self.opacity);
                }
                scale_text(ui, self.text_scale);
                let frame = card_frame(self.open).show(ui, |ui| {
                    ui.set_width(sized.map_or(widgets::CARD_WIDTH, |s| s[0]) - 2.0 * CARD_MARGIN);
                    let top = ui.cursor().top();
                    self.card_header(ui, w, place.pinned, &mut out);
                    // Above the rows, not scrolling with them: the blueprints card's search.
                    if w == Widget::Blueprints {
                        self.blueprints_top(ui, &mut out.actions);
                    }
                    // A scroll position per screen (its depth in the stack and the screen
                    // itself): every screen is where it was left when "Voltar" comes back to it.
                    let screen_key = match w {
                        Widget::Blueprints => (self.bp_stack.len(), self.bp_stack.last()),
                        _ => (0, None),
                    };
                    match sized {
                        // Resized: exactly that tall, the rows scroll inside (the header's
                        // height, at any text size, is measured above).
                        Some(s) => {
                            let body = (s[1] - 2.0 * CARD_MARGIN - (ui.cursor().top() - top)).max(MIN_BODY);
                            egui::ScrollArea::vertical().id_salt(("card-body", screen_key)).max_height(body).min_scrolled_height(body).auto_shrink([false, false]).show(ui, |ui| self.card_body(ui, w, here, &mut out.actions));
                        }
                        // Without the minimum a card in an auto-sized area stops at egui's 64 px default.
                        None => {
                            let max = CARD_BODY_MAX * self.text_scale;
                            egui::ScrollArea::vertical().id_salt(("card-body", screen_key)).max_height(max).min_scrolled_height(max).auto_shrink([false, true]).show(ui, |ui| self.card_body(ui, w, here, &mut out.actions));
                        }
                    }
                });
                if self.open {
                    resize_grip(ui, frame.response.rect, &mut out);
                }
            });
        let rect = shown.response.rect;
        out.pressed = self.open && ctx.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p)));
        out.rect = Some(rect);
        out
    }

    /// The card's title; while open also the drag handle, the "fixar na tela" box (not on the
    /// blueprints card, which is never drawn closed) and, at the top right corner, the ✕ that
    /// hides the card. Everything in it grows with the text size, like the body.
    fn card_header(&self, ui: &mut egui::Ui, w: Widget, pinned: bool, out: &mut CardOut) {
        let s = self.text_scale;
        let note = if w == Widget::Aqui { self.view.as_ref().and_then(model::current_place) } else { None };
        if !self.open {
            ui.horizontal(|ui| card_title(ui, w.title(), note.as_deref(), s));
            return;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0 * s), egui::Sense::hover());
        // The drag area goes first, so the pin box and the ✕ added on top of it win their clicks.
        let drag = ui.interact(rect, ui.id().with("card-drag"), egui::Sense::drag());
        if drag.dragged() {
            out.drag += drag.drag_delta();
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if drag.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if drag.drag_stopped() {
            out.drag_stopped = true;
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            // The pin box's square follows the text too, in the header only.
            let spacing = ui.spacing_mut();
            spacing.icon_width *= s;
            spacing.icon_width_inner *= s;
            spacing.icon_spacing *= s;
            card_title(ui, w.title(), note.as_deref(), s);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_button(ui, 14.0 * s).on_hover_text(tr!("Close this card", "Fechar este cartão")).clicked() {
                    out.close = true;
                }
                if w == Widget::Blueprints {
                    return;
                }
                let mut on = pinned;
                if ui.checkbox(&mut on, RichText::new(tr!("pin to screen", "fixar na tela")).color(MUTED).size(SMALL * s)).on_hover_text(tr!("Stays on screen with the overlay closed", "Continua na tela com o overlay fechado")).changed() {
                    out.pin = Some(on);
                }
            });
        });
        ui.add_space(2.0 * s);
    }

    /// The rows of a card, or one line saying there is nothing (only drawn while open).
    fn card_body(&self, ui: &mut egui::Ui, w: Widget, here: &[model::HereRow], actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let Some(d) = &self.view else {
            empty(ui, if self.conn == Conn::Loading { tr!("loading…", "carregando…") } else { tr!("no data", "sem dados") }, s);
            return;
        };
        match w {
            Widget::Fixados if d.tracked.is_empty() => empty(ui, tr!("No pinned items.", "Nenhum item fixado."), s),
            Widget::Fixados => self.fixados(ui, d, actions),
            Widget::Falta if d.missing.is_empty() => empty(ui, tr!("Nothing missing for the pinned contracts.", "Nada faltando nos contratos fixados."), s),
            Widget::Falta => self.falta(ui, d, actions),
            Widget::Aqui if here.is_empty() => empty(ui, if model::current_place(d).is_some() { tr!("Nothing pinned is stored here.", "Nada fixado guardado aqui.") } else { tr!("Current location unknown.", "Local atual desconhecido.") }, s),
            Widget::Aqui => self.aqui(ui, here, actions),
            Widget::Registros if d.notices.is_empty() && d.pending <= 0.0 => empty(ui, tr!("No records.", "Sem registros."), s),
            Widget::Registros => self.registros(ui, d),
            Widget::Missoes if d.missions.is_empty() => empty(ui, tr!("No pinned missions.", "Nenhuma missão fixada."), s),
            Widget::Missoes => self.missoes(ui, d, actions),
            Widget::Blueprints => self.blueprints(ui, d, actions),
        }
    }

    /// The blueprint whose screen the blueprints card shows, with which screen; `None` on the
    /// list, on a mission's screen, and when the open blueprint is not in the index (any more).
    fn bp_screen(&self) -> Option<(&model::IndexBlueprint, BpPanel)> {
        let Some(BpScreen::Blueprint(key, panel)) = self.bp_stack.last() else { return None };
        let b = self.index.as_ref()?.blueprints.iter().find(|b| b.blueprint == *key)?;
        Some((b, *panel))
    }

    /// The mission whose screen the blueprints card shows: its contract, and the title and
    /// giver of the line it was opened from.
    fn mission_screen(&self) -> Option<(&str, &str, Option<&str>)> {
        match self.bp_stack.last()? {
            BpScreen::Mission { contract, title, giver } => Some((contract.as_str(), title.as_str(), giver.as_deref())),
            BpScreen::Blueprint(..) => None,
        }
    }

    /// `blueprint` is in the index, so its screen can be shown.
    fn in_index(&self, blueprint: &str) -> bool {
        self.index.as_ref().is_some_and(|index| index.blueprints.iter().any(|b| b.blueprint == blueprint))
    }

    /// "‹ Voltar", its hover text saying where it leads (the previous screen, however deep),
    /// then `title`.
    fn back_row(&self, ui: &mut egui::Ui, title: &str, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        ui.horizontal(|ui| {
            let back = egui::Button::new(RichText::new(tr!("‹ Back", "‹ Voltar")).size(SMALL * s));
            if ui.add(back).on_hover_text(widgets::back_hint(&self.bp_stack)).clicked() {
                actions.push(Action::BpBack);
            }
            ui.add(egui::Label::new(RichText::new(title).color(TEXT_BRIGHT).strong()).truncate());
        });
    }

    /// The top of a blueprint's screen, in place of the list's: "Voltar", the blueprint's
    /// name, then whether the account has it and what it makes. The craft screen adds "Onde
    /// conseguir" (or "não disponível") and, under that line, what the server says of the
    /// crafted item (type, size, grade, class, manufacturer); the "Onde conseguir" screen says
    /// so as its title, with the one rule of the pools under it.
    fn bp_screen_top(&self, ui: &mut egui::Ui, b: &model::IndexBlueprint, panel: BpPanel, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        self.back_row(ui, &b.name, actions);
        let acquired = self.view.as_ref().and_then(|d| d.acquired_blueprints.as_deref());
        let keys: Option<HashSet<&str>> = acquired.map(|l| l.iter().map(String::as_str).collect());
        let have = model::blueprint_have(b, keys.as_ref());
        ui.horizontal(|ui| {
            ui.label(RichText::new(have.text()).color(if have.had() { OK } else { MUTED }).size(SMALL * s).strong());
            if !b.kind.is_empty() {
                ui.label(RichText::new(&b.kind).color(MUTED).size(SMALL * s));
            }
            if panel == BpPanel::Craft {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.bp_where_cell(ui, b, actions));
            }
        });
        if panel == BpPanel::Craft
            && let Some(line) = self.details.get(&b.blueprint).and_then(|d| model::facts_line(&d.facts))
        {
            ui.add(egui::Label::new(RichText::new(line).color(MUTED).size(SMALL * s)).wrap());
        }
        if panel == BpPanel::Where {
            ui.horizontal(|ui| card_title(ui, model::where_label(), None, s));
            empty(ui, model::never_twice_note(), s);
        }
        ui.add_space(2.0 * s);
    }

    /// The top of a mission's screen: "Voltar" and the mission's title (the answer's, else the
    /// line's it was opened from), who gives it, the screen's title and the one rule of the
    /// pools.
    fn mission_screen_top(&self, ui: &mut egui::Ui, contract: &str, title: &str, giver: Option<&str>, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let held = self.missions.get(contract);
        let title = held.map(|m| m.title.as_str()).filter(|t| !t.trim().is_empty()).unwrap_or(title);
        let title = if title.trim().is_empty() { tr!("Mission", "Missão") } else { title };
        self.back_row(ui, title, actions);
        let giver = match held {
            Some(m) => m.giver.as_deref(),
            None => giver,
        };
        if let Some(g) = giver.filter(|g| !g.trim().is_empty()) {
            ui.add(egui::Label::new(RichText::new(g).color(MUTED).size(SMALL * s)).truncate());
        }
        ui.horizontal(|ui| card_title(ui, model::mission_screen_title(), None, s));
        empty(ui, model::never_twice_note(), s);
        ui.add_space(2.0 * s);
    }

    /// "Onde conseguir" for a blueprint that can be obtained and is not a default one (a
    /// click shows its missions' screen), "não disponível" for one nothing gives, nothing for
    /// a default one or a key the detail route does not take.
    fn bp_where_cell(&self, ui: &mut egui::Ui, b: &model::IndexBlueprint, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        match model::where_cell(b) {
            model::WhereCell::Button if model::blueprint_usable(&b.blueprint) => {
                let button = egui::Button::new(RichText::new(model::where_label()).size(SMALL * s));
                if ui.add(button).on_hover_text(tr!("the missions that give it", "as missões que dão esta blueprint")).clicked() {
                    actions.push(Action::BpGo(BpScreen::Blueprint(b.blueprint.clone(), BpPanel::Where)));
                }
            }
            model::WhereCell::NotAvailable => {
                ui.label(RichText::new(model::not_available_label()).color(MUTED).size(SMALL * s)).on_hover_text(tr!("nothing in the game gives it", "nada no jogo dá esta blueprint"));
            }
            model::WhereCell::Button | model::WhereCell::Nothing => {}
        }
    }

    /// The top of the blueprints card, above what scrolls. On the list: "adquiridas N de M"
    /// (when the server sends the counts), the card's own search field (lit like the overlay's
    /// main one), and the filter Todas / Adquiridas / Não adquiridas. On a blueprint's screen:
    /// `bp_screen_top`; on a mission's: `mission_screen_top`.
    fn blueprints_top(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        if let Some((contract, title, giver)) = self.mission_screen() {
            self.mission_screen_top(ui, contract, title, giver, actions);
            return;
        }
        if let Some((b, panel)) = self.bp_screen() {
            self.bp_screen_top(ui, b, panel, actions);
            return;
        }
        if let Some(line) = self.view.as_ref().and_then(model::acquired_line) {
            ui.label(RichText::new(line).color(MUTED).size(SMALL * s)).on_hover_text(tr!("blueprints of this channel the account has, of those that can be obtained", "blueprints deste canal que a conta tem, das que dá para obter"));
        }
        let mut text = self.bp_query.clone();
        let changed = ui
            .scope(|ui| {
                light_field_strokes(ui);
                let field = light_field(&mut text, RichText::new(tr!("Search blueprints…", "Buscar blueprints…"))).id(bp_search_id()).desired_width(f32::INFINITY);
                ui.add(field).changed()
            })
            .inner;
        if changed {
            actions.push(Action::BpQuery(text));
        }
        ui.horizontal(|ui| {
            for f in model::BpFilter::ALL {
                let on = f == self.bp_filter;
                let label = RichText::new(f.label()).size(SMALL * s).color(if on { BRASS } else { MUTED });
                let b = if on { egui::Button::new(label).stroke(egui::Stroke::new(1.0_f32, BRASS)) } else { egui::Button::new(label) };
                if ui.add(b).clicked() && !on {
                    actions.push(Action::BpFilter(f));
                }
            }
        });
        ui.add_space(2.0 * s);
    }

    /// Why the `-` / `+` of an item are disabled, if they are: its unpin is on its way
    /// (`leaving`), its last write got no answer, or no change can be made now.
    fn adjust_off(&self, item_id: &str, leaving: bool) -> Option<&'static str> {
        if leaving {
            Some(leaving_hint())
        } else if self.engine.unconfirmed(item_id) {
            Some(unconfirmed_hint())
        } else if !self.can_change() {
            Some(busy_hint())
        } else {
            None
        }
    }

    /// Why the "mover" of an item's places is disabled, if it is: a move of it is on its way
    /// (until a read settles it), no change can be made now, or the inventory (whose stacks a
    /// move goes by) has not answered yet.
    fn move_off(&self, item_id: &str) -> Option<&'static str> {
        if self.engine.moving(item_id) {
            Some(moving_hint())
        } else if !self.can_change() {
            Some(busy_hint())
        } else if self.inventory.held().is_none() {
            Some(tr!("loading the inventory…", "carregando o inventário…"))
        } else {
            None
        }
    }

    /// The stacks of `item_id` as drawn: the inventory answer with the moves it does not
    /// include yet on top.
    fn stacks_now(&self, item_id: &str) -> Vec<model::Stack> {
        let mut stacks = self.inventory.held().and_then(|h| h.get(item_id)).map(|o| o.stacks.clone()).unwrap_or_default();
        for change in self.engine.changes_after(self.inventory.mark()) {
            if let Local::Move { item_id: id, from, to, parts } = change
                && id.as_str() == item_id
            {
                model::move_stacks(&mut stacks, from, to, parts);
            }
        }
        stacks
    }

    /// An unfolded row (`key`: the key that remembers it unfolded; the item's id and name):
    /// one line per place, its name and then its quantity (where the player stands in brass);
    /// open, each line with whole units in it has "mover", which opens the move picker under
    /// it (see [`Overlay::mover_popup`]).
    fn place_lines(&self, ui: &mut egui::Ui, key: &str, item_id: &str, item_name: &str, lines: &[model::PlaceText], actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let movable = self.open && model::id_usable(item_id);
        let off = if movable { self.move_off(item_id) } else { None };
        for (n, line) in lines.iter().enumerate() {
            ui.push_id(("place", n), |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(INDENT);
                    let color = if line.here { BRASS } else { MUTED };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if movable && line.amount >= 1.0 {
                            let line_key = format!("{key}@{}", line.location);
                            let open = self.mover.as_ref().is_some_and(|m| m.key == line_key);
                            let b = egui::Button::new(RichText::new(tr!("move", "mover")).size(SMALL * s)).selected(open);
                            let r = ui.add_enabled(off.is_none(), b).on_hover_text(tr!("move to another location", "mover para outro local"));
                            let r = disabled_why(r, off);
                            if activated(ui, &r) {
                                let max = line.amount.floor() as i64;
                                actions.push(Action::OpenMover(Box::new(Mover {
                                    key: line_key,
                                    item_id: item_id.to_string(),
                                    name: item_name.to_string(),
                                    from: line.location.clone(),
                                    from_name: line.name.clone(),
                                    max,
                                    qty: max,
                                    filter: String::new(),
                                    anchor: r.rect,
                                })));
                            }
                        }
                        ui.label(RichText::new(&line.qty).color(color).size(SMALL * s));
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&line.name).color(color).size(SMALL * s)).truncate());
                        });
                    });
                });
            });
        }
    }

    /// Open, the move picker of one place line, under its button: the item and the place it
    /// moves from; how many (- and +, Shift held: 10; "all"; all of the line at first); the
    /// filter field (it has the keyboard from the moment the picker opens); under it the places
    /// to move to, one button each: where the player is and the home, the places already
    /// holding the item, and, for a text typed in the filter, the channel's locations.
    /// A click moves at once (one change, see `Local::Move`). Mouse only, but for the filter.
    /// Returns its rect (a press elsewhere closes it).
    fn mover_popup(&self, ctx: &egui::Context, actions: &mut Vec<Action>) -> Option<egui::Rect> {
        let m = self.mover.as_ref()?;
        let s = self.text_scale;
        let no_names = HashMap::new();
        let d = self.view.as_ref();
        let names = d.map_or(&no_names, |d| &d.location_names);
        // Where the player is, the home and every place holding something stay offered even
        // when the game hides them or never named them (see `model::offered`).
        let kept = model::kept_places(d, self.inventory.held(), self.engine.changes());
        let stacks = self.stacks_now(&m.item_id);
        let holding = model::stack_places(&stacks);
        let locations = self.locations.held().unwrap_or(&[]);
        let targets = model::move_targets(&m.from, &kept, &holding, names, locations, &m.filter);
        let off = self.move_off(&m.item_id);
        let shown = egui::Area::new(mover_id()).order(egui::Order::Foreground).fixed_pos(m.anchor.left_bottom() + egui::vec2(0.0, 4.0)).show(ctx, |ui| {
            scale_text(ui, s);
            card_frame(true).show(ui, |ui| {
                ui.set_width(MOVER_WIDTH * s);
                let from = trf!("{} · from {}", "{} · de {}", m.name, m.from_name);
                ui.horizontal(|ui| card_title(ui, tr!("Move", "Mover"), Some(from.as_str()), s));
                ui.horizontal(|ui| {
                    let minus = ui.push_id("minus", |ui| small_button(ui, m.qty > 1, "-", s)).inner.on_hover_text("-1 (Shift: -10)");
                    if let Some(step) = press_step(ui, &minus) {
                        actions.push(Action::MoverQty(m.qty - step));
                    }
                    ui.label(RichText::new(m.qty.to_string()).color(TEXT_BRIGHT).strong());
                    let plus = ui.push_id("plus", |ui| small_button(ui, m.qty < m.max, "+", s)).inner.on_hover_text("+1 (Shift: +10)");
                    if let Some(step) = press_step(ui, &plus) {
                        actions.push(Action::MoverQty(m.qty + step));
                    }
                    ui.label(RichText::new(trf!("of {}", "de {}", m.max)).color(MUTED).size(SMALL * s));
                    if ui.add_enabled(m.qty < m.max, egui::Button::new(RichText::new(tr!("all", "tudo")).size(SMALL * s))).clicked() {
                        actions.push(Action::MoverQty(m.max));
                    }
                });
                ui.add_space(2.0 * s);
                // The filter above the list, which grows downwards from it. Its id is fixed
                // (`mover_filter_id`): the list changing length under it, or the locations
                // arriving, never moves the keyboard focus away from it.
                let mut filter = m.filter.clone();
                let field = egui::TextEdit::singleline(&mut filter).id(mover_filter_id()).hint_text(RichText::new(tr!("other location…", "outro local…")).color(FIELD_HINT)).desired_width(f32::INFINITY);
                if ui.add(field).changed() {
                    actions.push(Action::MoverFilter(filter));
                }
                ui.add_space(2.0 * s);
                for (n, t) in targets.iter().enumerate() {
                    ui.push_id(("target", n), |ui| {
                        let note = match t.kind {
                            model::TargetKind::Here => tr!(" · here", " · aqui"),
                            model::TargetKind::Home => tr!(" · home", " · casa"),
                            model::TargetKind::Holding => tr!(" · has it", " · já tem"),
                            model::TargetKind::Found => "",
                        };
                        let b = egui::Button::new(RichText::new(format!("→ {}{note}", t.name)).size(SMALL * s)).min_size(egui::vec2(ui.available_width(), 0.0));
                        let r = disabled_why(ui.add_enabled(off.is_none(), b), off);
                        if activated(ui, &r) {
                            let parts = model::split_move(&stacks, &m.from, m.qty);
                            actions.push(Action::Change(Local::Move { item_id: m.item_id.clone(), from: m.from.clone(), to: t.id.clone(), parts }));
                            actions.push(Action::CloseMover);
                        }
                    });
                }
                if !m.filter.trim().is_empty() && self.locations.held().is_none() {
                    match self.locations.error() {
                        Some(e) => {
                            ui.add(egui::Label::new(RichText::new(trf!("could not read the locations: {e}", "não deu para ler os locais: {e}")).color(WARN).size(SMALL * s)).wrap());
                        }
                        None => empty(ui, tr!("loading locations…", "carregando locais…"), s),
                    }
                } else if targets.is_empty() {
                    empty(ui, tr!("No locations; type to search.", "Nenhum local; digite para buscar."), s);
                }
            });
        });
        Some(shown.response.rect)
    }

    /// One item row: open, a fold for where it is kept, the name, the number, - + (and × for
    /// a pinned item); a progress bar under it when it has a goal, open or closed. A row
    /// whose id cannot be sent whole is read-only; one being unpinned stays, struck through
    /// and disabled; a number whose write got no answer is marked until the next read. The
    /// caller keys the row by its server id (`ui.push_id`).
    fn item_row(&self, ui: &mut egui::Ui, row: Row<'_>, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let places = row.places.as_ref().filter(|(_, p)| self.open && !p.is_empty());
        let unfolded = places.is_some_and(|(k, _)| self.expanded.contains(k));
        let usable = model::id_usable(row.item_id);
        let leaving = usable && row.unpin && self.engine.unpinning(row.item_id);
        let unconfirmed = usable && self.engine.unconfirmed(row.item_id);
        ui.horizontal(|ui| {
            if row.indent {
                ui.add_space(INDENT);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.open && usable {
                    if row.unpin {
                        let on = self.can_change() && !self.engine.pin_busy(row.item_id);
                        let r = ui.push_id("unpin", |ui| small_button(ui, on, "×", s)).inner.on_hover_text(tr!("unpin", "desafixar"));
                        let r = disabled_why(r, (!on).then_some(if leaving { leaving_hint() } else { busy_hint() }));
                        if activated(ui, &r) {
                            actions.push(Action::Change(Local::UnpinItem { item_id: row.item_id.to_string() }));
                        }
                    }
                    adjust_buttons(ui, row.item_id, self.adjust_off(row.item_id, leaving), s, actions);
                }
                // Right to left: the note goes first so it is drawn after the number.
                if let Some((note, hover)) = &row.qty_note {
                    ui.label(RichText::new(note.as_str()).color(MUTED).size(SMALL * s)).on_hover_text(*hover);
                }
                let q = ui.label(RichText::new(&row.qty).color(if unconfirmed { WARN } else { row.qty_color }).strong());
                if unconfirmed {
                    q.on_hover_text(unconfirmed_hint());
                } else if let Some(h) = row.qty_hover {
                    q.on_hover_text(h);
                }
                // What is left of the row, from the left: the fold and the name, cut to fit.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some((key, _)) = places
                        && fold_toggle(ui, unfolded, s).on_hover_text(fold_hint(unfolded)).clicked()
                    {
                        actions.push(Action::Toggle(key.clone()));
                    }
                    let text = RichText::new(row.name).color(if leaving { MUTED } else { TEXT });
                    let name = ui.add(egui::Label::new(if leaving { text.strikethrough() } else { text }).truncate());
                    if leaving {
                        name.on_hover_text(leaving_hint());
                    } else if let Some(h) = &row.name_hover {
                        name.on_hover_text(h.as_str());
                    }
                });
            });
        });
        if let Some((have, goal)) = row.goal {
            progress_bar(ui, have, goal, s);
        }
        if unfolded && let Some((key, lines)) = places {
            self.place_lines(ui, key, row.item_id, row.name, lines, actions);
        }
    }

    /// Pinned items with have / target and what converts into them.
    fn fixados(&self, ui: &mut egui::Ui, d: &OverlayData, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let at = d.at_location.as_deref();
        let keys = model::occurrences(d.tracked.iter().map(|t| t.item_id.as_str()));
        for (t, n) in d.tracked.iter().zip(keys) {
            ui.push_id(("fixados", t.item_id.as_str(), n), |ui| {
                let (q, reached) = model::tracked_qty(t);
                let mut row = Row::new(&t.item_id, &t.name, q);
                row.qty_color = if reached { ACCENT } else { TEXT };
                row.goal = t.target.filter(|g| *g > 0.0).map(|g| (t.have, g));
                row.places = Some((format!("fixados:{}", t.item_id), model::places(&t.places, &d.location_names, at)));
                row.unpin = true;
                self.item_row(ui, row, actions);
                // What converts into it: every one while open, each with its pin; closed,
                // only the pinned ones.
                let source_keys = model::occurrences(t.sources.iter().map(|src| src.item_id.as_str()));
                for (src, sn) in t.sources.iter().zip(source_keys) {
                    let pinned = self.source_pins.contains(&(t.item_id.clone(), src.item_id.clone()));
                    if !self.open && !pinned {
                        continue;
                    }
                    ui.push_id(("source", src.item_id.as_str(), sn), |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(INDENT);
                            if self.open
                                && model::id_usable(&t.item_id)
                                && model::id_usable(&src.item_id)
                                && pin_button(ui, true, pinned, s)
                                    .on_hover_text(if pinned { tr!("don't show with the overlay closed", "não mostrar com o overlay fechado") } else { tr!("show with the overlay closed", "mostrar com o overlay fechado") })
                                    .clicked()
                            {
                                actions.push(Action::ToggleSource(t.item_id.clone(), src.item_id.clone()));
                            }
                            ui.label(RichText::new(model::source_line(src)).color(MUTED).size(SMALL * s)).on_hover_text(model::rate_line(src, &t.name));
                        });
                    });
                }
            });
        }
    }

    /// What is missing for the pinned contracts; open, where each item is kept.
    fn falta(&self, ui: &mut egui::Ui, d: &OverlayData, actions: &mut Vec<Action>) {
        let at = d.at_location.as_deref();
        let keys = model::occurrences(d.missing.iter().map(|m| m.item_id.as_str()));
        for (m, n) in d.missing.iter().zip(keys) {
            ui.push_id(("falta", m.item_id.as_str(), n), |ui| {
                let mut row = Row::new(&m.item_id, &m.name, format!("{} / {}", model::qty(m.have), model::qty(m.need)));
                row.name_hover = Some(model::missing_hover(m));
                row.goal = Some((m.have, m.need));
                row.places = Some((format!("falta:{}", m.item_id), model::places(&m.places, &d.location_names, at)));
                self.item_row(ui, row, actions);
            });
        }
    }

    /// What is stored right here that pinned things need, and why.
    fn aqui(&self, ui: &mut egui::Ui, here: &[model::HereRow], actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let keys = model::occurrences(here.iter().map(|r| r.item_id.as_str()));
        for (r, n) in here.iter().zip(keys) {
            ui.push_id(("aqui", r.item_id.as_str(), n), |ui| {
                let mut row = Row::new(&r.item_id, &r.name, model::qty(r.qty));
                row.qty_color = BRASS;
                row.qty_hover = Some(tr!("stored here", "guardado aqui"));
                row.goal = r.goal;
                row.unpin = r.tracked.is_some();
                self.item_row(ui, row, actions);
                let detail = model::here_detail(r);
                if !detail.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(INDENT);
                        ui.label(RichText::new(detail).color(MUTED).size(SMALL * s));
                    });
                }
            });
        }
    }

    /// The pinned missions: title, giver, and each reward with what the player has of it.
    /// A mission being unpinned stays, struck through, until a read settles it.
    fn missoes(&self, ui: &mut egui::Ui, d: &OverlayData, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let keys = model::occurrences(d.missions.iter().map(|m| m.mission_id.as_str()));
        for (i, (m, n)) in d.missions.iter().zip(keys).enumerate() {
            if i > 0 {
                ui.add_space(4.0 * s);
            }
            ui.push_id(("mission", m.mission_id.as_str(), n), |ui| {
                let leaving = self.engine.unpinning_mission(&m.mission_id);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.open && model::id_usable(&m.mission_id) {
                            let on = self.can_change() && !self.engine.mission_busy(&m.mission_id);
                            let r = ui.push_id("unpin", |ui| small_button(ui, on, "×", s)).inner.on_hover_text(tr!("unpin", "desafixar"));
                            let r = disabled_why(r, (!on).then_some(if leaving { leaving_hint() } else { busy_hint() }));
                            if activated(ui, &r) {
                                actions.push(Action::Change(Local::UnpinMission { mission_id: m.mission_id.clone() }));
                            }
                        }
                        if let Some(g) = m.giver.as_deref().filter(|g| !g.trim().is_empty()) {
                            ui.label(RichText::new(g).color(MUTED).size(SMALL * s));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let title = RichText::new(&m.title).color(if leaving { MUTED } else { TEXT }).strong();
                            let shown = ui.add(egui::Label::new(if leaving { title.strikethrough() } else { title }).truncate());
                            if leaving {
                                shown.on_hover_text(leaving_hint());
                            }
                        });
                    });
                });
                if m.rewards.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(INDENT);
                        empty(ui, tr!("no known reward", "sem recompensa conhecida"), s);
                    });
                }
                let mut one_of_said = false;
                let reward_keys = model::occurrences(m.rewards.iter().map(|r| r.item_id.as_str()));
                for (r, rn) in m.rewards.iter().zip(reward_keys) {
                    if r.one_of && !one_of_said {
                        one_of_said = true;
                        ui.horizontal(|ui| {
                            ui.add_space(INDENT);
                            empty(ui, tr!("one of these:", "uma destas:"), s);
                        });
                    }
                    ui.push_id(("reward", r.item_id.as_str(), rn), |ui| {
                        let line = model::reward_line(r);
                        let mut row = Row::new(&r.item_id, &line, trf!("have {}", "tem {}", model::qty(r.have)));
                        row.qty_color = MUTED;
                        row.qty_hover = Some(how_many_hint());
                        row.indent = true;
                        self.item_row(ui, row, actions);
                    });
                }
            });
        }
    }

    /// The blueprints card's rows (see `model::blueprint_list`), built again only when the
    /// search text, the filter, the index or the data changed.
    fn bp_rows(&self, index: &OverlayIndex, d: &OverlayData) -> Rc<Vec<(usize, model::BpHave)>> {
        let mut cache = self.bp_list.borrow_mut();
        if let Some((query, filter, rev, rows)) = cache.as_ref()
            && *query == self.bp_query
            && *filter == self.bp_filter
            && *rev == self.bp_rev
        {
            return rows.clone();
        }
        let rows = Rc::new(model::blueprint_list(&index.blueprints, &self.bp_query, self.bp_filter, d.acquired_blueprints.as_deref()));
        *cache = Some((self.bp_query.clone(), self.bp_filter, self.bp_rev, rows.clone()));
        rows
    }

    /// What scrolls in the blueprints card (its top is `blueprints_top`). On a mission's
    /// screen: its pools. On a blueprint's screen: its craft, or the missions that give it. On
    /// the list: every blueprint of the
    /// index that passes the card's search and filter, by name, one fixed-height row each;
    /// only the rows inside the card's scroll view are laid out (`widgets::visible_rows`; the
    /// room of the others is left empty), so thousands of blueprints cost a screenful of rows
    /// per frame.
    fn blueprints(&self, ui: &mut egui::Ui, d: &OverlayData, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let Some(index) = &self.index else {
            if self.index_fallback {
                empty(ui, no_blueprint_list(), s);
            } else if let Some(error) = &self.index_error {
                ui.add(egui::Label::new(RichText::new(error.as_str()).color(WARN).size(SMALL * s)).wrap());
            } else {
                empty(ui, tr!("loading…", "carregando…"), s);
            }
            return;
        };
        if index.blueprints.is_empty() {
            empty(ui, no_blueprint_list(), s);
            return;
        }
        if let Some((contract, ..)) = self.mission_screen() {
            ui.push_id(("bp-mission", contract), |ui| self.bp_mission(ui, contract, actions));
            return;
        }
        if let Some((b, panel)) = self.bp_screen() {
            ui.push_id(("bp-screen", b.blueprint.as_str()), |ui| match panel {
                BpPanel::Craft => self.bp_craft(ui, &b.blueprint, actions),
                BpPanel::Where => self.bp_where(ui, &b.blueprint, actions),
            });
            return;
        }
        let rows = self.bp_rows(index, d);
        if rows.is_empty() {
            empty(ui, tr!("No blueprint found.", "Nenhuma blueprint encontrada."), s);
            return;
        }
        let row_h = ui.spacing().interact_size.y.max((BODY + 6.0) * s);
        let pitch = row_h + ui.spacing().item_spacing.y;
        self.bp_rows_in_view(ui, index, &rows, row_h, pitch, actions);
    }

    /// `rows`, each `row_h` tall (`pitch` with the room between two), from where the cursor
    /// is: those inside the scroll view drawn, the room of the others left empty.
    fn bp_rows_in_view(&self, ui: &mut egui::Ui, index: &OverlayIndex, rows: &[(usize, model::BpHave)], row_h: f32, pitch: f32, actions: &mut Vec<Action>) {
        let clip = ui.clip_rect();
        let shown = widgets::visible_rows(ui.cursor().top(), clip.top(), clip.bottom(), rows.len(), pitch);
        if shown.start > 0 {
            ui.add_space(shown.start as f32 * pitch);
        }
        for &(i, have) in &rows[shown.clone()] {
            ui.push_id(("bp-row", i), |ui| self.bp_row(ui, &index.blueprints[i], have, row_h, actions));
        }
        let after = rows.len() - shown.end;
        if after > 0 {
            ui.add_space(after as f32 * pitch);
        }
    }

    /// One row of the blueprints list, exactly `row_h` tall: first, whether the account has it
    /// (a mark and a word: never colour alone); then its name (a click shows its screen) and,
    /// small, what it makes; at the end what `bp_where_cell` draws. A key the detail route
    /// does not take leaves the row read-only.
    fn bp_row(&self, ui: &mut egui::Ui, b: &model::IndexBlueprint, have: model::BpHave, row_h: f32, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let key = b.blueprint.as_str();
        let usable = model::blueprint_usable(key);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            // A cell of its own width, so the names line up whatever the word.
            let (cell, _) = ui.allocate_exact_size(egui::vec2(BP_STATE_WIDTH * s, row_h), egui::Sense::hover());
            ui.scope_builder(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.label(RichText::new(have.text()).color(if have.had() { OK } else { MUTED }).size(SMALL * s).strong());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.bp_where_cell(ui, b, actions);
                if !b.kind.is_empty() {
                    ui.label(RichText::new(&b.kind).color(MUTED).size(SMALL * s));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let text = RichText::new(&b.name).color(TEXT);
                    if !usable {
                        ui.add(egui::Label::new(text).truncate());
                        return;
                    }
                    let name = ui.add(egui::Label::new(text).sense(egui::Sense::click()).truncate()).on_hover_text(tr!("open the blueprint: its craft", "abrir a blueprint: a fabricação"));
                    if name.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if name.clicked() {
                        actions.push(Action::BpGo(BpScreen::Blueprint(key.to_string(), BpPanel::Craft)));
                    }
                });
            });
        });
    }

    /// On a blueprint's screen, while its detail is not there: "carregando…", or the server's
    /// sentence when the request failed (the detail held, if any, is still drawn after it).
    /// `None` until there is a detail to draw.
    fn bp_detail(&self, ui: &mut egui::Ui, key: &str) -> Option<&model::BlueprintDetail> {
        let s = self.text_scale;
        if let Some(error) = self.details.error(key) {
            ui.add(egui::Label::new(RichText::new(error).color(WARN).size(SMALL * s)).wrap());
        }
        let detail = self.details.get(key);
        if detail.is_none() && self.details.error(key).is_none() {
            empty(ui, tr!("loading…", "carregando…"), s);
        }
        detail
    }

    /// The craft of the open blueprint `key`: its time, "Informar manualmente", per slot its
    /// name, the material in use (`have / need`, short ones drawn like the rows of "Falta"), a
    /// selector when the slot takes several, and a quality control (a slider over the
    /// material's tiers showing the chosen tier's quality, its single tier as text, or a
    /// number field when it has none or by hand); then what the crafted item gets, computed
    /// here (`model::craft_result`) at those qualities, and "pronta" when every material in
    /// use is there. A change goes out as the whole new set of choices (`Action::BpCraft`).
    fn bp_craft(&self, ui: &mut egui::Ui, key: &str, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        {
            let Some(d) = self.bp_detail(ui, key) else { return };
            let state = match &self.craft {
                Some((k, st)) if k == key && st.fits(d) => st.clone(),
                _ => model::CraftState::start(d),
            };
            let mut next = state.clone();
            if let Some(t) = model::craft_time(d.craft_time_sec) {
                ui.label(RichText::new(trf!("craft time {t}", "tempo de fabricação {t}")).color(MUTED).size(SMALL * s));
            }
            // The switch only when some slot has tiers to switch from.
            if d.slots.iter().any(|slot| slot.options.iter().any(|o| !o.tiers.bands.is_empty())) {
                ui.horizontal(|ui| {
                    if toggle_switch(ui, state.manual).on_hover_text(tr!("type each quality instead of choosing a tier", "digitar cada qualidade em vez de escolher a faixa")).clicked() {
                        next = model::with_manual(d, &next, !state.manual);
                    }
                    ui.label(RichText::new(tr!("Enter manually", "Informar manualmente")).color(TEXT).size(SMALL * s));
                });
            }
            for (n, (slot, choice)) in d.slots.iter().zip(&state.slots).enumerate() {
                ui.push_id(("slot", n), |ui| {
                    ui.add_space(2.0 * s);
                    ui.label(RichText::new(&slot.name).color(MUTED).size(SMALL * s).strong());
                    let Some(o) = slot.options.get(choice.option) else {
                        empty(ui, tr!("no known material", "sem material conhecido"), s);
                        return;
                    };
                    if slot.options.len() > 1 {
                        ui.horizontal_wrapped(|ui| {
                            for (j, option) in slot.options.iter().enumerate() {
                                let on = j == choice.option;
                                let b = egui::Button::new(RichText::new(&option.name).size(SMALL * s).color(if on { BRASS } else { MUTED })).selected(on);
                                if ui.push_id(("option", j), |ui| ui.add(b)).inner.on_hover_text(tr!("the material used in this slot", "o material usado nesta parte")).clicked() && !on {
                                    next = model::with_option(d, &next, n, j);
                                }
                            }
                        });
                    }
                    // No item id: the row is read-only (no - / +, no places).
                    let label = model::material_label(o);
                    let mut row = Row::new("", &label, model::material_amount(o));
                    row.qty_color = if model::material_short(o) { TEXT } else { ACCENT };
                    row.goal = (o.need > 0.0).then_some((o.have, o.need));
                    self.item_row(ui, row, actions);
                    // A quality control only where the quality changes something.
                    if !slot.effects.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(INDENT);
                            ui.label(RichText::new(tr!("quality", "qualidade")).color(MUTED).size(SMALL * s));
                            let tiers = o.tiers.bands.len();
                            if model::on_tiers(o, state.manual) && tiers == 1 {
                                ui.label(RichText::new(o.tiers.bands[0].value.to_string()).color(TEXT_BRIGHT).strong());
                            } else if model::on_tiers(o, state.manual) {
                                let mut stop = choice.stop.min(tiers - 1);
                                ui.spacing_mut().slider_width = (ui.available_width() - 48.0 * s).max(60.0);
                                if ui.add(egui::Slider::new(&mut stop, 0..=tiers - 1).show_value(false)).on_hover_text(tr!("the material's quality tiers", "as faixas de qualidade do material")).changed() {
                                    next = model::with_stop(&next, n, stop);
                                }
                                ui.label(RichText::new(o.tiers.bands[stop].value.to_string()).color(TEXT_BRIGHT).strong());
                            } else {
                                let mut q = model::slot_quality(o, choice, state.manual);
                                let min = model::clamp_quality(o.min_quality, 0);
                                if ui.add(egui::DragValue::new(&mut q).range(min..=wikelo_core::MAX_QUALITY)).changed() {
                                    next = model::with_quality(d, &next, n, q);
                                }
                            }
                        });
                    }
                });
            }
            ui.add_space(4.0 * s);
            let lines = model::craft_result(d, &next);
            if lines.is_empty() {
                empty(ui, tr!("The quality changes nothing in this craft.", "A qualidade não muda nada nesta fabricação."), s);
            } else {
                ui.horizontal(|ui| card_title(ui, tr!("Result", "Resultado"), None, s));
                for line in lines {
                    ui.add(egui::Label::new(RichText::new(line).color(TEXT).size(SMALL * s)).truncate());
                }
            }
            if model::craft_ready(d, &next) {
                ui.label(RichText::new(tr!("ready", "pronta")).color(OK).size(SMALL * s).strong()).on_hover_text(tr!("every material in use is there for one craft", "tem todo o material escolhido para fabricar uma"));
            }
            if next != state {
                actions.push(Action::BpCraft(key.to_string(), next));
            }
        }
    }

    /// "Onde conseguir" of the open blueprint `key`: one line per mission that gives it (its
    /// title, who gives it, when, and "nem sempre entrega (N%)" when its reward does not
    /// always give a blueprint), in the server's order, then "+N" for those the server did not
    /// list. A line with a contract (a newer server) opens the mission's blueprints.
    fn bp_where(&self, ui: &mut egui::Ui, key: &str, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        {
            let Some(d) = self.bp_detail(ui, key) else { return };
            if d.sources.is_empty() && d.sources_more == 0 {
                empty(ui, tr!("No known mission gives it.", "Nenhuma missão conhecida dá esta blueprint."), s);
            }
            for (n, source) in d.sources.iter().enumerate() {
                ui.push_id(("source", n), |ui| {
                    let text = RichText::new(model::mission_line(source)).color(TEXT).size(SMALL * s);
                    let Some(contract) = &source.contract_id else {
                        ui.add(egui::Label::new(text).wrap());
                        return;
                    };
                    let line = ui.add(egui::Label::new(text).sense(egui::Sense::click()).wrap()).on_hover_text(tr!("the blueprints this mission gives", "as blueprints que esta missão dá"));
                    if line.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if line.clicked() {
                        actions.push(Action::BpGo(BpScreen::Mission { contract: contract.clone(), title: source.title.clone(), giver: source.giver.clone() }));
                    }
                });
            }
            if let Some(more) = model::more_missions(d.sources_more) {
                empty(ui, &more, s);
            }
        }
    }

    /// A mission's blueprints (its screen's top is `mission_screen_top`): while they are not
    /// there, "carregando…" or the server's sentence (what was held, if any, is still drawn
    /// after it). Then per pool: when it pays and "N de M adquiridas", "nem sempre entrega
    /// (N%)" when its reward does not always give one, that the mission gives no more once
    /// every one is had, and its blueprints by name, each saying first whether the account has
    /// it, then its name (a click opens its craft) and, small, its kind.
    fn bp_mission(&self, ui: &mut egui::Ui, contract: &str, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        if let Some(error) = self.missions.error(contract) {
            ui.add(egui::Label::new(RichText::new(error).color(WARN).size(SMALL * s)).wrap());
        }
        let Some(m) = self.missions.get(contract) else {
            if self.missions.error(contract).is_none() {
                empty(ui, tr!("loading…", "carregando…"), s);
            }
            return;
        };
        if m.pools.is_empty() {
            empty(ui, tr!("This mission gives no blueprint.", "Esta missão não dá nenhuma blueprint."), s);
        }
        let row_h = ui.spacing().interact_size.y.max((BODY + 6.0) * s);
        for (n, pool) in m.pools.iter().enumerate() {
            ui.push_id(("pool", n), |ui| {
                if n > 0 {
                    ui.add_space(6.0 * s);
                }
                ui.add(egui::Label::new(RichText::new(model::pool_heading(pool)).color(TEXT_BRIGHT).size(SMALL * s).strong()).wrap());
                for note in model::pool_notes(pool) {
                    ui.add(egui::Label::new(RichText::new(note).color(MUTED).size(SMALL * s)).wrap());
                }
                for (i, b) in pool.blueprints.iter().enumerate() {
                    ui.push_id(("pool-bp", i), |ui| self.pool_row(ui, b, row_h, actions));
                }
            });
        }
    }

    /// One blueprint of a mission's pool, `row_h` tall, laid out like a row of the list: the
    /// mark and the word first, then its name (a click opens its craft, when the card can show
    /// it) and, small, its kind.
    fn pool_row(&self, ui: &mut egui::Ui, b: &model::PoolBlueprint, row_h: f32, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let have = b.have();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            let (cell, _) = ui.allocate_exact_size(egui::vec2(BP_STATE_WIDTH * s, row_h), egui::Sense::hover());
            ui.scope_builder(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.label(RichText::new(have.text()).color(if have.had() { OK } else { MUTED }).size(SMALL * s).strong());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !b.kind.is_empty() {
                    ui.label(RichText::new(&b.kind).color(MUTED).size(SMALL * s));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let text = RichText::new(&b.name).color(TEXT);
                    if !model::blueprint_usable(&b.blueprint) {
                        ui.add(egui::Label::new(text).truncate());
                        return;
                    }
                    let name = ui.add(egui::Label::new(text).sense(egui::Sense::click()).truncate()).on_hover_text(tr!("open the blueprint: its craft", "abrir a blueprint: a fabricação"));
                    if name.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    // Only a blueprint of the index has a screen (it is looked up on the click
                    // alone, not on every frame).
                    if name.clicked() && self.in_index(&b.blueprint) {
                        actions.push(Action::BpGo(BpScreen::Blueprint(b.blueprint.clone(), BpPanel::Craft)));
                    }
                });
            });
        });
    }

    /// Pending changes on the site and the latest notices (the records; the alerts pop up as
    /// toasts, see `Toasts::feed`).
    fn registros(&self, ui: &mut egui::Ui, d: &OverlayData) {
        let s = self.text_scale;
        if d.pending > 0.0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(WARN).size(SMALL * s));
                ui.label(RichText::new(model::pending_text(d.pending)).color(MUTED).size(SMALL * s));
            });
        }
        for n in d.notices.iter().take(NOTICES_SHOWN) {
            ui.add(egui::Label::new(RichText::new(model::notice_line(n, self.offset)).color(if n.is_pending() { WARN } else { TEXT }).size(SMALL * s)).wrap());
        }
    }

    /// Open, with reward choices waiting: one row per choice, its options as buttons (the
    /// suggested one marked, each with what it adds when the server says); a click answers
    /// it, and the choice then stays, its options disabled and the chosen one marked, until
    /// a read settles it. Drawn under `top`, over the cards, never wider or taller than the
    /// screen: the choices scroll inside.
    fn choices_card(&self, ctx: &egui::Context, top: f32) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(d) = self.view.as_ref().filter(|d| !d.choices.is_empty()) else { return actions };
        let s = self.text_scale;
        let screen = ctx.screen_rect();
        let width = (CHOICES_WIDTH * s).min(screen.width() - 2.0 * SCREEN_GAP).max(CHOICES_MIN_WIDTH);
        let body = (screen.height() - top - CHOICES_HEADER * s - 2.0 * CARD_MARGIN - SCREEN_GAP).max(3.0 * MIN_BODY);
        egui::Area::new(egui::Id::new("wikelist-choices")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top)).show(ctx, |ui| {
            scale_text(ui, s);
            card_frame(true).stroke(egui::Stroke::new(1.0_f32, WARN.gamma_multiply(0.8))).show(ui, |ui| {
                ui.set_width(width - 2.0 * CARD_MARGIN);
                ui.horizontal(|ui| card_title(ui, tr!("Choose the reward", "Escolha a recompensa"), None, s));
                egui::ScrollArea::vertical().max_height(body).auto_shrink([false, true]).show(ui, |ui| {
                    for c in &d.choices {
                        let Some(id) = c.id else { continue };
                        ui.push_id(("choice", id), |ui| self.choice(ui, c, id, &mut actions));
                    }
                });
            });
        });
        actions
    }

    /// One reward choice of [`Overlay::choices_card`] (keyed by its pending id; each option
    /// by its item id).
    fn choice(&self, ui: &mut egui::Ui, c: &model::Choice, id: i64, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let chosen = self.engine.choosing(id);
        // The options say what they add: the choice's own quantity is for older servers.
        let own_qty = c.options.iter().any(|o| o.qty.is_some());
        ui.add_space(4.0 * s);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(model::hhmm(&c.at, self.offset)).color(MUTED).size(SMALL * s));
                if c.qty > 1.0 && !own_qty {
                    ui.label(RichText::new(format!("{}×", model::qty(c.qty))).color(MUTED).size(SMALL * s));
                }
                if chosen.is_some() {
                    ui.label(RichText::new(tr!("choosing…", "escolhendo…")).color(WARN).size(SMALL * s));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let title = c.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(tr!("Mission", "Missão"));
                    ui.add(egui::Label::new(RichText::new(title).color(TEXT).strong()).truncate());
                });
            });
        });
        let can = self.can_change();
        let keys = model::occurrences(c.options.iter().map(|o| o.item_id.as_str()));
        ui.horizontal_wrapped(|ui| {
            for (o, n) in c.options.iter().zip(keys) {
                ui.push_id((o.item_id.as_str(), n), |ui| {
                    let suggested = c.suggested.as_deref() == Some(o.item_id.as_str());
                    let picked = chosen == Some(o.item_id.as_str());
                    let mut b = egui::Button::new(RichText::new(model::option_label(o)).color(if suggested || picked { BRASS } else { TEXT }));
                    if suggested || picked {
                        b = b.stroke(egui::Stroke::new(if picked { 2.0_f32 } else { 1.0_f32 }, BRASS));
                    }
                    let on = can && chosen.is_none() && model::id_usable(&o.item_id);
                    let r = ui.add_enabled(on, b);
                    let r = if suggested { r.on_hover_text(tr!("suggested", "sugerido")) } else { r };
                    let why = if picked {
                        Some(tr!("choosing…", "escolhendo…"))
                    } else if chosen.is_some() {
                        Some(tr!("another option was already chosen", "outra opção já foi escolhida"))
                    } else if !can && model::id_usable(&o.item_id) {
                        Some(busy_hint())
                    } else {
                        None
                    };
                    let r = disabled_why(r, why);
                    if activated(ui, &r) {
                        actions.push(Action::Change(Local::Choose { id, item_id: o.item_id.clone() }));
                    }
                });
            }
        });
    }

    /// The search strip while open, as wide as the bar (`bar`): right under it until the
    /// player drags its grip elsewhere, never off the screen. The grip and one big field; once
    /// something is typed, the filter chips under it (only the groups that have results, with
    /// their counts) and the results for the text as typed this very frame, grouped under
    /// headings (Inventário, then Missões), over the cards, as tall as they are up to the edge
    /// of the screen (scrolling inside beyond that): under the strip, or above it when there
    /// is more room there. An empty field shows no chips and unfolds nothing.
    fn search_strip(&self, ctx: &egui::Context, bar: egui::Rect) -> StripOut {
        let mut out = StripOut { bottom: bar.bottom(), ..Default::default() };
        let s = self.text_scale;
        let screen = ctx.screen_rect();
        let width = bar.width().max(STRIP_MIN_WIDTH).min(screen.width() - 2.0 * SCREEN_GAP);
        let row_size = self.strip_row;
        let pos = match self.search_pos {
            Some(f) => {
                let f = widgets::clamp_frac(f, [row_size.x, row_size.y], [screen.width(), screen.height()]);
                screen.min + egui::vec2(f[0] * screen.width(), f[1] * screen.height())
            }
            None => egui::pos2(bar.left(), bar.bottom() + STRIP_GAP),
        };
        let up = widgets::unfold_up(pos.y - screen.top(), screen.bottom() - (pos.y + row_size.y));
        let shown = egui::Area::new(strip_id()).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            scale_text(ui, s);
            strip_frame(s)
                .show(ui, |ui| {
                    ui.set_width(width - 2.0 * BAR_MARGIN * s);
                    let row = ui.horizontal(|ui| {
                        let grip = strip_grip(ui, SEARCH_FIELD_HEIGHT * s, s).on_hover_text(tr!("drag to move", "arrastar para mover"));
                        if grip.dragged() {
                            out.drag += grip.drag_delta();
                        }
                        if grip.drag_stopped() {
                            out.drag_stopped = true;
                        }
                        let size = BODY * SEARCH_FIELD_TEXT * s;
                        let mut text = self.search.text.clone();
                        // A field at a glance over a dark scene (see `light_field`). Only the
                        // field comes after this in the row.
                        light_field_strokes(ui);
                        let field = light_field(&mut text, RichText::new(tr!("Search items and missions…", "Buscar itens e missões…")).size(size))
                            .id(search_id())
                            .font(egui::FontId::proportional(size))
                            .margin(6.0 * s)
                            .min_size(egui::vec2(0.0, SEARCH_FIELD_HEIGHT * s))
                            .desired_width(f32::INFINITY);
                        if ui.add(field).changed() {
                            text = widgets::cut_search_text(text);
                            out.actions.push(Action::SearchText(text.clone()));
                        }
                        text
                    });
                    let mut bottom = row.response.rect.bottom();
                    // The text as typed this frame: the chips and the results follow at once.
                    let listing = widgets::listing(&row.inner);
                    let results = match &listing {
                        Listing::Query(q) => Some(self.results(q)),
                        Listing::Nothing | Listing::TooShort => None,
                    };
                    let (items, missions) = match &results {
                        Some(Results::Ready(found)) => (found.item_count, found.mission_count),
                        _ => (0, 0),
                    };
                    // A chip chosen this frame filters at once.
                    let mut filter = widgets::effective_filter(self.search_filter, items, missions);
                    let chips = widgets::chips(items, missions);
                    if !chips.is_empty() {
                        ui.add_space(2.0 * s);
                        let chips_row = ui.horizontal_wrapped(|ui| {
                            for (chip, label) in &chips {
                                let on = *chip == filter;
                                let text = RichText::new(label.as_str()).size(SMALL * s).color(if on { BRASS } else { MUTED });
                                let b = if on { egui::Button::new(text).stroke(egui::Stroke::new(1.0_f32, BRASS)) } else { egui::Button::new(text) };
                                if ui.add(b).clicked() && !on {
                                    filter = *chip;
                                    out.actions.push(Action::SearchFilter(*chip));
                                }
                            }
                        });
                        bottom = chips_row.response.rect.bottom();
                    }
                    out.bottom = bottom + BAR_MARGIN * s;
                    let unfold = listing != Listing::Nothing;
                    if unfold && !up {
                        ui.add_space(4.0 * s);
                        let max = (screen.bottom() - ui.cursor().top() - BAR_MARGIN * s - SCREEN_GAP).max(3.0 * MIN_BODY);
                        results_scroll(ui, max, |ui| self.search_results(ui, &listing, results.as_ref(), filter, &mut out.actions));
                    }
                    (listing, results, filter, unfold)
                })
                .inner
        });
        let rect = shown.response.rect;
        out.top_left = rect.min;
        out.row = egui::vec2(rect.width(), out.bottom - rect.top());
        let (listing, results, filter, unfold) = shown.inner;
        if unfold && up {
            // Above the strip, in an area of its own whose bottom stays put.
            let max = (rect.top() - UNFOLD_GAP - screen.top() - SCREEN_GAP - 2.0 * BAR_MARGIN * s).max(3.0 * MIN_BODY);
            egui::Area::new(results_up_id())
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(egui::pos2(rect.left(), rect.top() - UNFOLD_GAP))
                .show(ctx, |ui| {
                    scale_text(ui, s);
                    strip_frame(s).show(ui, |ui| {
                        ui.set_width(width - 2.0 * BAR_MARGIN * s);
                        results_scroll(ui, max, |ui| self.search_results(ui, &listing, results.as_ref(), filter, &mut out.actions));
                    });
                });
            out.up = true;
        }
        out
    }

    /// The results for what the field lists (see `widgets::listing`): why there are none yet,
    /// or each group with results under its heading (only the one of the chip `filter`).
    fn search_results(&self, ui: &mut egui::Ui, listing: &Listing, results: Option<&Results>, filter: Filter, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        match listing {
            Listing::Nothing => return,
            Listing::TooShort => {
                empty(ui, tr!("Type at least 2 letters.", "Digite ao menos 2 letras."), s);
                return;
            }
            Listing::Query(_) => {}
        }
        let found = match results {
            Some(Results::Ready(found)) => found.clone(),
            Some(Results::Failed(error)) => {
                ui.add(egui::Label::new(RichText::new(error.as_str()).color(WARN).size(SMALL * s)).wrap());
                return;
            }
            Some(Results::Loading(why)) => {
                empty(ui, why, s);
                return;
            }
            None => return,
        };
        let groups = widgets::groups(filter, found.item_count, found.mission_count);
        if groups.is_empty() {
            empty(ui, tr!("Nothing found.", "Nada encontrado."), s);
            return;
        }
        for (n, group) in groups.into_iter().enumerate() {
            if n > 0 {
                ui.add_space(6.0 * s);
            }
            ui.horizontal(|ui| card_title(ui, group.title(), None, s));
            match group {
                Group::Inventario => self.found_items(ui, &found, actions),
                Group::Missoes => self.found_missions(ui, &found, actions),
            }
        }
    }

    /// The group "Inventário": every item found, owned or not, owned first; each with what the
    /// player has (0 when nothing; no number until the inventory has answered), - +, pin, and
    /// where it is kept.
    fn found_items(&self, ui: &mut egui::Ui, found: &model::Found, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let held = self.inventory.held();
        if held.is_none()
            && let Some(error) = self.inventory.error()
        {
            ui.add(egui::Label::new(RichText::new(trf!("could not read the inventory: {error}", "não deu para ler o inventário: {error}")).color(WARN).size(SMALL * s)).wrap());
        }
        let (pinned_items, _) = self.pinned();
        let rows = model::item_rows(&found.items, held, &pinned_items, self.engine.changes(), self.engine.changes_after(self.inventory.mark()));
        let no_names = HashMap::new();
        let (names, at) = match &self.view {
            Some(d) => (&d.location_names, d.at_location.as_deref()),
            None => (&no_names, None),
        };
        let keys = model::occurrences(rows.iter().map(|row| row.item_id.as_str()));
        for (row, n) in rows.iter().zip(keys) {
            ui.push_id(("search-item", row.item_id.as_str(), n), |ui| {
                let places = (format!("busca:{}", row.item_id), model::places(&row.places, names, at));
                self.search_item(ui, row, places, actions);
            });
        }
        let more = found.item_count.saturating_sub(found.items.len());
        if more > 0 {
            empty(ui, &widgets::more_line(more, false), s);
        }
    }

    /// The group "Missões": every mission found, with its pin.
    fn found_missions(&self, ui: &mut egui::Ui, found: &model::Found, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let (_, pinned_missions) = self.pinned();
        let rows = model::mission_rows(&found.missions, &pinned_missions, self.engine.changes());
        let keys = model::occurrences(rows.iter().map(|row| row.mission_id.as_str()));
        for (row, n) in rows.iter().zip(keys) {
            ui.push_id(("search-mission", row.mission_id.as_str(), n), |ui| self.search_mission(ui, row, actions));
        }
        let more = found.mission_count.saturating_sub(found.missions.len());
        if more > 0 {
            empty(ui, &widgets::more_line(more, true), s);
        }
    }

    /// An item found by the search: name, what the player has, - +, pin / unpin (disabled,
    /// with the state asked for, while that change is on its way), and a fold for where it is
    /// kept (`places`, and the key that remembers it unfolded) when it is kept anywhere.
    /// Read-only when its id cannot be sent whole.
    fn search_item(&self, ui: &mut egui::Ui, i: &model::ItemRow, places: (String, Vec<model::PlaceText>), actions: &mut Vec<Action>) {
        let s = self.text_scale;
        let usable = model::id_usable(&i.item_id);
        let unconfirmed = usable && self.engine.unconfirmed(&i.item_id);
        let places = Some(places).filter(|(_, lines)| !lines.is_empty());
        let unfolded = places.as_ref().is_some_and(|(key, _)| self.expanded.contains(key));
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if usable {
                    let busy = self.engine.pin_busy(&i.item_id);
                    let on = self.can_change() && !busy && (i.pinned || i.qty.is_some());
                    let r = ui.push_id("pin", |ui| pin_button(ui, on, i.pinned, s)).inner;
                    let why = if busy {
                        tr!("saving…", "salvando…")
                    } else if i.qty.is_none() && !i.pinned {
                        tr!("loading quantity…", "carregando quantidade…")
                    } else {
                        busy_hint()
                    };
                    let r = disabled_why(r, (!on).then_some(why));
                    if activated(ui, &r) {
                        actions.push(Action::Change(if i.pinned {
                            Local::UnpinItem { item_id: i.item_id.clone() }
                        } else {
                            Local::PinItem { item_id: i.item_id.clone(), name: i.name.clone(), have: i.qty.unwrap_or(0.0) }
                        }));
                    }
                    adjust_buttons(ui, &i.item_id, self.adjust_off(&i.item_id, false), s, actions);
                }
                // 0 is shown: an item found but not owned; no number until the inventory answers.
                if let Some(have) = i.qty {
                    let color = if unconfirmed {
                        WARN
                    } else if have > 0.0 {
                        TEXT
                    } else {
                        MUTED
                    };
                    ui.label(RichText::new(model::qty(have)).color(color).strong()).on_hover_text(if unconfirmed { unconfirmed_hint() } else { how_many_hint() });
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some((key, _)) = &places
                        && fold_toggle(ui, unfolded, s).on_hover_text(fold_hint(unfolded)).clicked()
                    {
                        actions.push(Action::Toggle(key.clone()));
                    }
                    ui.add(egui::Label::new(RichText::new(&i.name).color(TEXT)).truncate());
                });
            });
        });
        if unfolded && let Some((key, lines)) = &places {
            self.place_lines(ui, key, &i.item_id, &i.name, lines, actions);
        }
    }

    /// A mission found by the search: title, giver, pin / unpin (disabled while that change
    /// is on its way).
    fn search_mission(&self, ui: &mut egui::Ui, m: &model::MissionRow, actions: &mut Vec<Action>) {
        let s = self.text_scale;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if model::id_usable(&m.mission_id) {
                    let busy = self.engine.mission_busy(&m.mission_id);
                    let on = self.can_change() && !busy;
                    let r = ui.push_id("pin", |ui| pin_button(ui, on, m.pinned, s)).inner;
                    let r = disabled_why(r, (!on).then_some(if busy { tr!("saving…", "salvando…") } else { busy_hint() }));
                    if activated(ui, &r) {
                        actions.push(Action::Change(if m.pinned {
                            Local::UnpinMission { mission_id: m.mission_id.clone() }
                        } else {
                            Local::PinMission { mission_id: m.mission_id.clone(), title: m.title.clone(), giver: m.giver.clone() }
                        }));
                    }
                }
                if let Some(g) = m.giver.as_deref().filter(|g| !g.trim().is_empty()) {
                    ui.label(RichText::new(g).color(MUTED).size(SMALL * s));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&m.title).color(TEXT)).truncate());
                });
            });
        });
    }

    /// Open, the widgets menu under its button (`button`): a switch per card (but the
    /// blueprints card, which has its own button in the bar), for the search and for the
    /// alerts, "voltar ao lugar" for the search, "mostrar todos" and "ocultar todos" (every
    /// widget, the blueprints card included).
    fn widgets_menu(&self, ctx: &egui::Context, button: egui::Rect) -> MenuOut {
        let mut out = MenuOut::default();
        let all_on = Part::ALL.iter().all(|p| self.shown.get(*p));
        let shown = egui::Area::new(menu_id()).order(egui::Order::Foreground).fixed_pos(button.left_bottom() + egui::vec2(0.0, 6.0)).show(ctx, |ui| {
            card_frame(true).show(ui, |ui| {
                ui.set_min_width(MENU_WIDTH);
                ui.horizontal(|ui| card_title(ui, "Widgets", None, 1.0));
                for part in Part::ALL.into_iter().filter(|p| p.in_menu()) {
                    ui.push_id(part.key(), |ui| {
                        ui.horizontal(|ui| {
                            let on = self.shown.get(part);
                            if toggle_switch(ui, on).on_hover_text(if on { tr!("hide", "ocultar") } else { tr!("show", "mostrar") }).clicked() {
                                out.switched.push((part, !on));
                            }
                            let title = ui.label(RichText::new(part.title()).color(if on { TEXT } else { MUTED }));
                            if part == Part::Alerts {
                                title.on_hover_text(tr!("Each new notice shows on screen for a few seconds, also with the overlay closed", "Cada aviso novo aparece na tela por alguns segundos, com o overlay fechado também"));
                            }
                            if part == Part::Search {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let home = ui.add_enabled(self.search_pos.is_some(), egui::Button::new(RichText::new(tr!("reset position", "voltar ao lugar")).size(SMALL)));
                                    if home.on_hover_text(tr!("Under the bar", "Debaixo da barra")).clicked() {
                                        out.search_home = true;
                                    }
                                });
                            }
                        });
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.add_enabled(!all_on, egui::Button::new(RichText::new(tr!("show all", "mostrar todos")).size(SMALL))).clicked() {
                        out.all = Some(true);
                    }
                    if ui.add_enabled(self.shown.any(), egui::Button::new(RichText::new(tr!("hide all", "ocultar todos")).size(SMALL))).clicked() {
                        out.all = Some(false);
                    }
                });
            });
        });
        out.rect = Some(shown.response.rect);
        out
    }

    /// What was done in the widgets menu: switches, all at once, the search back in its place;
    /// a finished change, saved right away.
    fn apply_menu(&mut self, ctx: &egui::Context, menu: MenuOut) {
        let mut changed = false;
        for (part, on) in menu.switched {
            changed |= self.shown.set(part, on);
        }
        if let Some(on) = menu.all {
            changed |= self.shown.set_all(on);
        }
        if menu.search_home && self.search_pos.take().is_some() {
            changed = true;
        }
        if changed {
            self.commit(ctx);
        }
    }

    /// The bar on top while open: wordmark, channel, location, opacity, text size, the
    /// blueprints card's button, the widgets menu, "Ocultar" (hides everything, never ends the
    /// process), close; the keys under it. Everything in it grows with the text size, like
    /// the cards.
    fn bar(&mut self, ctx: &egui::Context) -> BarOut {
        let mut out = BarOut::default();
        let mut opacity = self.opacity;
        let mut scale = self.text_scale;
        // The size this frame is drawn at (a step of "A-" / "A+" shows from the next frame).
        let s = self.text_scale;
        let bp_shown = self.shown.get(Part::Card(Widget::Blueprints));
        let mut bp_toggle = false;
        let frame = egui::Frame::new()
            .fill(NAVY.gamma_multiply(0.96))
            .stroke(egui::Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6)))
            .corner_radius(6.0)
            .inner_margin(BAR_MARGIN * s);
        let shown = egui::Area::new(egui::Id::new("wikelist-bar")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0)).show(ctx, |ui| {
            // The bar's own text and row height (the buttons, the place) follow the text size.
            scale_text(ui, s);
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Wikelist").color(ACCENT).strong().size(17.0 * s));
                    if let Some(d) = &self.view
                        && !d.channel.is_empty()
                    {
                        ui.label(RichText::new(&d.channel).color(MUTED).size(SMALL * s));
                    }
                    ui.separator();
                    match self.view.as_ref().and_then(model::current_place) {
                        Some(place) => ui.label(RichText::new(place).color(TEXT).strong()),
                        None => ui.label(RichText::new(tr!("unknown location", "local desconhecido")).color(MUTED).size(SMALL * s)),
                    };
                    ui.separator();
                    ui.label(RichText::new(tr!("opacity", "opacidade")).color(MUTED).size(SMALL * s));
                    ui.spacing_mut().slider_width = 90.0 * s;
                    let slider = ui.add(egui::Slider::new(&mut opacity, widgets::MIN_OPACITY..=1.0).show_value(false)).on_hover_text(tr!("Of the pinned cards, with the overlay closed", "Dos cartões fixados, com o overlay fechado"));
                    out.preview = slider.hovered() || slider.dragged();
                    out.opacity_done = slider.drag_stopped() || (slider.changed() && !slider.dragged());
                    ui.separator();
                    ui.label(RichText::new(tr!("text", "texto")).color(MUTED).size(SMALL * s));
                    let smaller = ui.add_enabled(scale > widgets::TEXT_SCALES[0], egui::Button::new(RichText::new("A-").size(SMALL * s))).on_hover_text(tr!("smaller text", "texto menor"));
                    if smaller.clicked() {
                        scale = widgets::text_scale_step(scale, false);
                    }
                    ui.label(RichText::new(widgets::text_scale_label(scale)).color(TEXT).size(SMALL * s));
                    let larger = ui.add_enabled(scale < widgets::TEXT_SCALES[widgets::TEXT_SCALES.len() - 1], egui::Button::new(RichText::new("A+").size(SMALL * s))).on_hover_text(tr!("larger text", "texto maior"));
                    if larger.clicked() {
                        scale = widgets::text_scale_step(scale, true);
                    }
                    ui.separator();
                    // The blueprints card is shown and hidden here, not in the widgets menu.
                    let hint = if bp_shown { tr!("Blueprints: click to hide the card", "Blueprints: clique para ocultar o cartão") } else { tr!("Blueprints: click to show the card", "Blueprints: clique para mostrar o cartão") };
                    bp_toggle = blueprints_button(ui, bp_shown, s).on_hover_text(hint).clicked();
                    let menu = ui.add(egui::Button::new(RichText::new("Widgets").size(SMALL * s)).selected(self.menu_open)).on_hover_text(tr!("Show / hide each card, the search and the alerts", "Mostrar / ocultar cada cartão, a busca e os alertas"));
                    out.menu_button = Some(menu.rect);
                    out.menu_toggle = menu.clicked();
                    ui.separator();
                    // Hides, never ends: the process stays and the hotkey or the tray brings it back.
                    if ui.add(egui::Button::new(RichText::new(tr!("Hide", "Ocultar")).size(SMALL * s))).on_hover_text(hide_hint()).clicked() {
                        out.hide = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("×").size(16.0 * s)).frame(false)).on_hover_text(tr!("close (Esc)", "fechar (Esc)")).clicked() {
                        out.close = true;
                    }
                });
                ui.label(RichText::new(hotkey::help()).color(MUTED).size(11.0 * s));
                self.status_lines(ui, true, s);
            });
        });
        out.rect = Some(shown.response.rect);
        if opacity != self.opacity {
            self.opacity = opacity;
            self.dirty = Some(Instant::now());
        }
        // A text size step is a finished change, like a released slider.
        let scaled = scale != self.text_scale;
        self.text_scale = scale;
        // The blueprints button flips the same switch as the card's ✕ and the menu did.
        let toggled = bp_toggle && self.shown.set(Part::Card(Widget::Blueprints), !bp_shown);
        if out.opacity_done || scaled || toggled {
            self.commit(ctx);
        }
        out
    }

    /// Connection problems and, with `keys`, the keys another program holds; `s` is the text
    /// size.
    fn status_lines(&self, ui: &mut egui::Ui, keys: bool, s: f32) {
        match &self.conn {
            Conn::Ok | Conn::Loading => {}
            Conn::SignedOut => {
                ui.label(RichText::new(sign_in_hint()).color(WARN));
            }
            Conn::NoRoute => {
                ui.label(RichText::new(tr!("the server has no overlay yet", "o servidor ainda não tem o overlay")).color(MUTED));
            }
            Conn::Unreachable(e) => {
                ui.label(RichText::new(no_contact()).color(MUTED).size(SMALL * s)).on_hover_text(e);
            }
        }
        if keys {
            for f in &self.hotkeys.failed {
                ui.add(egui::Label::new(RichText::new(f).color(WARN).size(11.0 * s)).wrap());
            }
        }
    }

    /// Closed: a problem nothing else on screen would tell about (a failed write has its toast).
    fn closed_problem(&self) -> bool {
        match &self.conn {
            Conn::Ok | Conn::Loading => false,
            Conn::SignedOut | Conn::NoRoute => true,
            // A hiccup with data on screen stays quiet: the cards keep the last answer.
            Conn::Unreachable(_) => self.data.is_none(),
        }
    }

    fn closed_status(&self, ctx: &egui::Context) {
        if !self.closed_problem() {
            return;
        }
        egui::Area::new(egui::Id::new("wikelist-status"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(self.opacity);
                card_frame(false).show(ui, |ui| self.status_lines(ui, false, 1.0));
            });
    }

    /// The toasts at the bottom-right corner, newest nearest the corner, sliding in from
    /// the edge and fading out.
    fn draw_toasts(&self, ctx: &egui::Context, now: Instant) {
        let screen = ctx.screen_rect();
        let mut bottom = screen.bottom() - TOAST_BOTTOM;
        for t in self.toasts.live().iter().rev() {
            let (slide, alpha) = widgets::toast_phase(now.saturating_duration_since(t.born));
            let x = screen.right() - TOAST_EDGE - TOAST_WIDTH + (1.0 - slide) * (TOAST_WIDTH + TOAST_EDGE);
            let shown = egui::Area::new(egui::Id::new(("wikelist-toast", t.id)))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(egui::pos2(x, bottom))
                .constrain(false)
                .interactable(false)
                .show(ctx, |ui| {
                    // Open, everything is at full opacity.
                    ui.set_opacity(if self.open { alpha } else { self.opacity * alpha });
                    toast(ui, t);
                });
            bottom -= shown.response.rect.height() + TOAST_GAP;
        }
    }
}

impl eframe::App for Overlay {
    /// Fully transparent outside the cards.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Before the keys: a hotkey pressed as the game went away does nothing.
        if self.life.game_left() {
            self.leave(ctx);
            return;
        }
        self.drain(ctx);
        if self.watch_requests() {
            self.life.set_stopped();
            self.set_open(ctx, false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.place(ctx);
        self.advance_opening(ctx);
        self.keep_covering();
        if self.relayer > 0 {
            self.relayer -= 1;
            os::show_layered();
            ctx.request_repaint();
        }
        self.take_focus_if_due(ctx);
        self.watch_focus(ctx);
        self.keys_while_open(ctx);
        self.ensure_layout(ctx);
        self.save_layout_if_due(ctx);
        let now = Instant::now();
        self.toasts.prune(now);
        self.watch_config(now);
        // Hidden too: changes already made still go out.
        self.pump(ctx, now);
        // The stop file, the window check and the write-error timeout need a look now and
        // then; data, hotkeys and input wake the window themselves, toasts ask below. Closed
        // with the game around, which window is in front decides what is drawn: looked at
        // more often.
        let watch_foreground = !self.open && self.game.is_some();
        ctx.request_repaint_after(if watch_foreground { FOREGROUND_CHECK_EVERY } else { Duration::from_millis(500) });
        // Hidden = nothing drawn: the window stays (a hidden window gets no more frames to
        // notice the hotkey), but it is fully transparent and lets every click through. The
        // same while closed and the game is not in front (Alt+Tab to another program).
        if !self.visible || self.layout.is_none() || (!self.open && !self.closed_shown()) {
            egui::CentralPanel::default().frame(egui::Frame::new()).show(ctx, |_ui| {});
            return;
        }
        if self.open {
            // Every press starts afresh; one outside the widgets menu sets it again below.
            if ctx.input(|i| i.pointer.primary_pressed()) {
                self.menu_swallow = false;
            }
            if scrim(ctx) {
                // A click on the scrim closes the widgets menu or the move picker first (its
                // press may have closed it already): only the next one closes the overlay.
                if self.menu_open || self.mover.is_some() || std::mem::take(&mut self.menu_swallow) {
                    self.menu_open = false;
                    self.mover = None;
                } else {
                    self.set_open(ctx, false);
                }
            }
        } else {
            egui::CentralPanel::default().frame(egui::Frame::new()).show(ctx, |_ui| {});
        }
        let mut actions = self.draw_cards(ctx);
        self.reclamp(ctx);
        if self.open {
            let bar = self.bar(ctx);
            self.preview_opacity = bar.preview;
            let screen = ctx.screen_rect();
            let bar_rect = bar.rect.unwrap_or_else(|| egui::Rect::from_min_size(screen.min, egui::vec2(0.0, 60.0 * self.text_scale)));
            // The reward choices go under the strip in its first place, else under the bar.
            let mut top = bar_rect.bottom() + 10.0 - screen.top();
            let mut strip_up = false;
            if self.shown.get(Part::Search) {
                let strip = self.search_strip(ctx, bar_rect);
                self.strip_row = strip.row;
                strip_up = strip.up;
                if self.search_pos.is_none() {
                    top = strip.bottom + 10.0 - screen.top();
                }
                if strip.drag != egui::Vec2::ZERO {
                    let at = strip.top_left - screen.min;
                    self.search_pos = Some(widgets::search_dragged([at.x, at.y], [strip.drag.x, strip.drag.y], [strip.row.x, strip.row.y], [screen.width(), screen.height()]));
                    // Saved once it stays put (or right away on release, below).
                    self.dirty = Some(Instant::now());
                }
                if strip.drag_stopped {
                    self.commit(ctx);
                }
                actions.extend(strip.actions);
            }
            actions.extend(self.choices_card(ctx, top));
            // The search results go over everything, the reward choices included, and the
            // widgets menu over them.
            ctx.memory_mut(|m| {
                let areas = m.areas_mut();
                areas.move_to_top(top_layer(strip_id()));
                if strip_up {
                    areas.move_to_top(top_layer(results_up_id()));
                }
            });
            // A picker that has just opened: its filter field gets the keyboard before it is
            // drawn, this very frame (the field's id is fixed, so it keeps it afterwards). Open,
            // the keyboard goes back to it whenever nothing has it (the first frame of a new
            // area is a sizing pass whose widgets cannot hold the focus).
            let just_opened = std::mem::take(&mut self.focus_mover);
            if self.mover.is_some() && (just_opened || ctx.memory(|m| m.focused().is_none())) {
                ctx.memory_mut(|m| m.request_focus(mover_filter_id()));
            }
            // The move picker over the cards and the results it was opened from.
            if let Some(rect) = self.mover_popup(ctx, &mut actions) {
                ctx.memory_mut(|m| m.areas_mut().move_to_top(top_layer(mover_id())));
                let anchor = self.mover.as_ref().map(|m| m.anchor);
                // A press anywhere else closes it (its own button toggles it).
                let outside = ctx.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && !anchor.is_some_and(|a| a.contains(p))));
                if outside {
                    self.mover = None;
                    self.menu_swallow = true;
                }
            }
            if bar.menu_toggle {
                self.menu_open = !self.menu_open;
            }
            if self.menu_open
                && let Some(button) = bar.menu_button
            {
                let menu = self.widgets_menu(ctx, button);
                let rect = menu.rect;
                self.apply_menu(ctx, menu);
                ctx.memory_mut(|m| m.areas_mut().move_to_top(top_layer(menu_id())));
                // A press anywhere else closes it (the button toggles it itself).
                let outside = ctx.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| !button.contains(p) && !rect.is_some_and(|r| r.contains(p))));
                if outside {
                    self.menu_open = false;
                    self.menu_swallow = true;
                }
            }
            if bar.hide {
                self.hide(ctx);
            } else if bar.close {
                self.set_open(ctx, false);
            }
        } else {
            self.closed_status(ctx);
        }
        // Open too: a write that failed says so.
        self.draw_toasts(ctx, now);
        if let Some(wait) = self.toasts.next_repaint(now) {
            ctx.request_repaint_after(wait);
        }
        self.apply(actions, now);
        // A change made just now goes out at once (or once its clicks pause).
        self.pump(ctx, now);
    }
}

impl Drop for Overlay {
    /// A layout change made just before closing is still saved, and an overlay closed while
    /// open hands the focus back.
    fn drop(&mut self) {
        if self.dirty.is_some() {
            self.save_layout();
        }
        if let Some(prev) = self.prev_focus.take() {
            os::give_focus(prev);
        }
    }
}

/// The dark layer under the cards while open; `true` when it was clicked.
fn scrim(ctx: &egui::Context) -> bool {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(Color32::from_black_alpha(SCRIM_ALPHA)))
        .show(ctx, |ui| ui.interact(ui.max_rect(), egui::Id::new("wikelist-scrim"), egui::Sense::click()).clicked())
        .inner
}

fn card_frame(open: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(NAVY.gamma_multiply(if open { 0.94 } else { 0.82 }))
        .stroke(egui::Stroke::new(1.0_f32, ACCENT.gamma_multiply(if open { 0.6 } else { 0.45 })))
        .corner_radius(6.0)
        .inner_margin(CARD_MARGIN)
}

/// `FIXADOS`, `FALTA`, … like the group titles of the settings window, and a short note;
/// `s` is the text size.
fn card_title(ui: &mut egui::Ui, title: &str, note: Option<&str>, s: f32) {
    ui.label(RichText::new(title.to_uppercase()).color(MUTED).size(11.0 * s).strong().extra_letter_spacing(1.0));
    if let Some(n) = note {
        ui.label(RichText::new(n).color(TEXT).size(SMALL * s));
    }
}

fn empty(ui: &mut egui::Ui, text: &str, s: f32) {
    ui.label(RichText::new(text).color(MUTED).size(SMALL * s));
}

/// A card's text at the chosen size: egui's text styles and the row height. The sizes set
/// by hand in the cards are multiplied by the same factor where they are set.
fn scale_text(ui: &mut egui::Ui, s: f32) {
    if s == 1.0 {
        return;
    }
    let style = ui.style_mut();
    for font in style.text_styles.values_mut() {
        font.size *= s;
    }
    style.spacing.interact_size.y *= s;
}

/// The lower-right corner of an open card: dragging it resizes the card.
fn resize_grip(ui: &mut egui::Ui, card: egui::Rect, out: &mut CardOut) {
    let corner = card.right_bottom();
    let rect = egui::Rect::from_min_max(corner - egui::vec2(RESIZE_GRIP, RESIZE_GRIP), corner);
    let r = ui.interact(rect, ui.id().with("card-resize"), egui::Sense::drag());
    if r.dragged() {
        out.resize += r.drag_delta();
    }
    if r.drag_stopped() {
        out.resize_stopped = true;
    }
    let lit = r.hovered() || r.dragged();
    if lit {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    let stroke = egui::Stroke::new(1.0_f32, if lit { ACCENT } else { MUTED.gamma_multiply(0.6) });
    for k in 1..=3 {
        let d = 3.0 + 3.5 * k as f32;
        ui.painter().line_segment([corner - egui::vec2(d, 3.0), corner - egui::vec2(3.0, d)], stroke);
    }
}

/// The frame of the search strip and of its results unfolded upwards (the bar's look, its
/// margin growing with the text size `s`).
fn strip_frame(s: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(NAVY.gamma_multiply(0.96))
        .stroke(egui::Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6)))
        .corner_radius(6.0)
        .inner_margin(BAR_MARGIN * s)
}

/// A search field read at a glance over a dark scene: lighter than the frame around it
/// (`FIELD`), bright text, a dim hint (`hint`, its colour set here). The overlay's main search
/// and the blueprints card's share it; see [`light_field_strokes`] for its border.
fn light_field(text: &mut String, hint: RichText) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).text_color(TEXT_BRIGHT).background_color(FIELD).hint_text(hint.color(FIELD_HINT))
}

/// The border of a [`light_field`]: visible at rest, cyan when hovered, thicker and cyan with
/// the keyboard. It applies to what `ui` adds after it: call it right before the field (in a
/// scope of its own when more follows).
fn light_field_strokes(ui: &mut egui::Ui) {
    let v = ui.visuals_mut();
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, FIELD_BORDER);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.8));
    v.selection.stroke = egui::Stroke::new(2.0_f32, ACCENT);
}

/// The ✕ of a card's header (`side` px square), drawn with two strokes (no glyph that a font
/// might lack): muted, bright when hovered.
fn close_button(ui: &mut egui::Ui, side: f32) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let color = if r.hovered() { TEXT_BRIGHT } else { MUTED };
    let c = rect.center();
    let h = side * 0.3;
    let stroke = egui::Stroke::new((side / 9.0).max(1.2), color);
    let painter = ui.painter();
    painter.line_segment([c + egui::vec2(-h, -h), c + egui::vec2(h, h)], stroke);
    painter.line_segment([c + egui::vec2(-h, h), c + egui::vec2(h, -h)], stroke);
    r
}

/// The bar's button of the blueprints card: a small blueprint sheet (a page with its top
/// right corner folded and three lines on it) drawn with the painter, `s` the text size.
/// Selected-looking (lit background, cyan sheet) while the card is shown (`on`).
fn blueprints_button(ui: &mut egui::Ui, on: bool, s: f32) -> egui::Response {
    let height = ui.spacing().interact_size.y.max(20.0 * s);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(26.0 * s, height), egui::Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    if on || r.hovered() {
        painter.rect_filled(rect, 3.0, ACCENT.gamma_multiply(if on { 0.22 } else { 0.1 }));
    }
    let color = if on { ACCENT } else if r.hovered() { TEXT_BRIGHT } else { MUTED };
    let stroke = egui::Stroke::new(1.2 * s.max(1.0), color);
    // The page: 12 x 15 px at the normal size, centred.
    let (w, h) = (12.0 * s, 15.0 * s);
    let page = egui::Rect::from_center_size(rect.center(), egui::vec2(w, h));
    let fold = 4.0 * s;
    let (l, t, rt, b) = (page.left(), page.top(), page.right(), page.bottom());
    let outline = [egui::pos2(l, t), egui::pos2(rt - fold, t), egui::pos2(rt, t + fold), egui::pos2(rt, b), egui::pos2(l, b)];
    painter.add(egui::Shape::convex_polygon(outline.to_vec(), NAVY, egui::Stroke::NONE));
    for (i, p) in outline.iter().enumerate() {
        painter.line_segment([*p, outline[(i + 1) % outline.len()]], stroke);
    }
    // The folded corner.
    painter.line_segment([egui::pos2(rt - fold, t), egui::pos2(rt - fold, t + fold)], stroke);
    painter.line_segment([egui::pos2(rt - fold, t + fold), egui::pos2(rt, t + fold)], stroke);
    // Three lines of the drawing.
    let thin = egui::Stroke::new(1.0_f32, color);
    for k in 1..=3 {
        let y = t + fold + k as f32 * (h - fold) / 4.0;
        painter.line_segment([egui::pos2(l + 2.5 * s, y), egui::pos2(rt - 2.5 * s, y)], thin);
    }
    r
}

/// The search results' scroll area: as tall as they are up to `max`, scrolling beyond.
/// Without the minimum it would stop at what the strip measured on the last frame (egui's
/// 64 px at first).
fn results_scroll(ui: &mut egui::Ui, max: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().max_height(max).min_scrolled_height(max).auto_shrink([false, true]).show(ui, add);
}

/// The grip the search strip is dragged by (`height` tall, as wide as the text size `s`
/// makes it): two columns of dots.
fn strip_grip(ui: &mut egui::Ui, height: f32, s: f32) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(STRIP_GRIP * s, height), egui::Sense::drag());
    if r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let color = if r.hovered() || r.dragged() { ACCENT } else { MUTED };
    let c = rect.center();
    for dx in [-2.5_f32, 2.5] {
        for dy in [-6.0_f32, 0.0, 6.0] {
            ui.painter().circle_filled(c + egui::vec2(dx, dy), 1.5, color);
        }
    }
    r
}

/// A small on / off switch (egui has none): a pill, cyan with the knob on the right when on,
/// dark with it on the left when off. A click flips it.
fn toggle_switch(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(SWITCH_SIZE[0], SWITCH_SIZE[1]), egui::Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let radius = rect.height() / 2.0;
    let (edge, track) = if on { (ACCENT, ACCENT) } else { (if r.hovered() { ACCENT } else { FIELD_BORDER }, FIELD) };
    let painter = ui.painter();
    painter.rect_filled(rect, radius, edge);
    painter.rect_filled(rect.shrink(1.0), radius - 1.0, track);
    let x = if on { rect.right() - radius } else { rect.left() + radius };
    painter.circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, if on { TEXT_BRIGHT } else { MUTED });
    r
}

/// Hover text of what the player has of an item.
fn how_many_hint() -> &'static str {
    tr!("how many you have", "quanto você tem")
}

/// Hover text of a row's fold.
fn fold_hint(unfolded: bool) -> &'static str {
    if unfolded { tr!("hide locations", "esconder locais") } else { tr!("where it is stored", "onde está guardado") }
}

/// A small triangle that folds a row's places out (pointing down) or in.
fn fold_toggle(ui: &mut egui::Ui, open: bool, s: f32) -> egui::Response {
    let side = 10.0 * s;
    let (rect, r) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    let color = if r.hovered() { ACCENT } else { MUTED };
    let c = rect.center();
    let h = side * 0.35;
    let points = if open {
        vec![c + egui::vec2(-h, -h * 0.6), c + egui::vec2(h, -h * 0.6), c + egui::vec2(0.0, h * 0.8)]
    } else {
        vec![c + egui::vec2(-h * 0.6, -h), c + egui::vec2(-h * 0.6, h), c + egui::vec2(h * 0.8, 0.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
    r
}

/// A thin bar under a row: how far `have` is toward `goal`, in green once it is reached.
fn progress_bar(ui: &mut egui::Ui, have: f64, goal: f64, s: f32) {
    let Some((frac, done)) = model::progress(have, Some(goal)) else { return };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), (PROGRESS_HEIGHT * s).round().max(2.0)), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 1.5, BORDER);
    if frac > 0.0 {
        let mut fill = rect;
        fill.set_width(rect.width() * frac);
        painter.rect_filled(fill, 1.5, if done { OK } else { ACCENT });
    }
}

/// A press of `r` that counts: a click of the mouse, or Space / Enter, but not the key
/// repeat of a held key (holding Enter on `+` must not add one per repeat).
fn activated(ui: &egui::Ui, r: &egui::Response) -> bool {
    if r.clicked_by(egui::PointerButton::Primary) {
        return true;
    }
    r.clicked() && !ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Space | egui::Key::Enter, pressed: true, repeat: true, .. })))
}

/// The step of a `+` / `-` press: ten when Shift was held as the mouse button went down on
/// it (what Shift does at the release does not matter), or as Space / Enter pressed it; else
/// one. `None`: not pressed.
fn press_step(ui: &egui::Ui, r: &egui::Response) -> Option<i64> {
    let key = r.id.with("shift-at-press");
    let (down, shift) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift));
    if down && r.hovered() {
        ui.ctx().data_mut(|d| d.insert_temp(key, shift));
    }
    if !activated(ui, r) {
        return None;
    }
    let at_press = if r.clicked_by(egui::PointerButton::Primary) { ui.ctx().data_mut(|d| d.get_temp::<bool>(key)) } else { None };
    ui.ctx().data_mut(|d| d.remove::<bool>(key));
    Some(if at_press.unwrap_or(shift) { 10 } else { 1 })
}

/// `r` with `why` as its hover text while it is disabled.
fn disabled_why(r: egui::Response, why: Option<&str>) -> egui::Response {
    match why {
        Some(w) => r.on_disabled_hover_text(w),
        None => r,
    }
}

/// `-` and `+` for an item (laid out right to left, each keyed within the row): one unit a
/// click, ten with Shift held as the button goes down (see [`press_step`]). `off`: both
/// disabled, and why.
fn adjust_buttons(ui: &mut egui::Ui, item_id: &str, off: Option<&str>, s: f32, actions: &mut Vec<Action>) {
    let mut adjust = |delta: i64| actions.push(Action::Change(Local::Adjust { item_id: item_id.to_string(), delta }));
    let plus = ui.push_id("plus", |ui| small_button(ui, off.is_none(), "+", s)).inner.on_hover_text(tr!("+1 in the inventory (Shift: +10)", "+1 no inventário (Shift: +10)"));
    let plus = disabled_why(plus, off);
    if let Some(step) = press_step(ui, &plus) {
        adjust(step);
    }
    let minus = ui.push_id("minus", |ui| small_button(ui, off.is_none(), "-", s)).inner.on_hover_text(tr!("-1 in the inventory (Shift: -10)", "-1 no inventário (Shift: -10)"));
    let minus = disabled_why(minus, off);
    if let Some(step) = press_step(ui, &minus) {
        adjust(-step);
    }
}

/// `pin` / `unpin`, small.
fn pin_button(ui: &mut egui::Ui, enabled: bool, pinned: bool, s: f32) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(if pinned { tr!("unpin", "desafixar") } else { tr!("pin", "fixar") }).size(SMALL * s)))
}

fn toast(ui: &mut egui::Ui, t: &Toast) {
    let color = if t.warn { WARN } else { ACCENT };
    egui::Frame::new()
        .fill(NAVY.gamma_multiply(0.9))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.6)))
        .corner_radius(6.0)
        .inner_margin(CARD_MARGIN)
        .show(ui, |ui| {
            ui.set_width(TOAST_WIDTH - 2.0 * CARD_MARGIN);
            ui.label(RichText::new(t.title.to_uppercase()).color(color).size(11.0).strong().extra_letter_spacing(1.0));
            ui.add(egui::Label::new(RichText::new(&t.body).color(TEXT)).wrap());
        });
}

fn small_button(ui: &mut egui::Ui, enabled: bool, text: &str, s: f32) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).size(BODY * s)).min_size(egui::vec2(20.0, 18.0) * s))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The schedule of the reads (`next_delay`) is tested with the rest of it in `overlay_sync`.

    #[test]
    fn layout_writes_wait_to_settle_and_never_come_twice_a_second() {
        let ms = Duration::from_millis;
        assert_eq!(save_wait(ms(0), None), ms(800), "a fresh change settles first");
        assert_eq!(save_wait(ms(800), None), Duration::ZERO, "settled, never written: now");
        assert_eq!(save_wait(ms(900), Some(ms(5000))), Duration::ZERO);
        assert_eq!(save_wait(ms(900), Some(ms(300))), ms(700), "written 0.3 s ago: wait for the full second");
        assert_eq!(save_wait(ms(100), Some(ms(950))), ms(700), "the longer of the two waits");
        assert_eq!(save_wait(ms(5000), Some(ms(1000))), Duration::ZERO);
        // A change every frame (a size that keeps moving): whatever the timing, a write comes
        // at least a second after the previous one.
        for since_save in [0, 1, 250, 999] {
            assert!(save_wait(ms(10_000), Some(ms(since_save))) + ms(since_save) >= MIN_SAVE_GAP);
        }
    }

    #[test]
    fn finished_changes_skip_the_settling_but_not_the_one_second_gap() {
        let ms = Duration::from_millis;
        assert_eq!(commit_wait(None), Duration::ZERO, "never written: now");
        assert_eq!(commit_wait(Some(ms(5000))), Duration::ZERO);
        assert_eq!(commit_wait(Some(MIN_SAVE_GAP)), Duration::ZERO);
        assert_eq!(commit_wait(Some(ms(300))), ms(700), "written 0.3 s ago: wait for the full second");
        assert_eq!(commit_wait(Some(Duration::ZERO)), MIN_SAVE_GAP);
        for since_save in [0, 1, 250, 999, 1000, 4000] {
            assert!(commit_wait(Some(ms(since_save))) + ms(since_save) >= MIN_SAVE_GAP, "never two writes within a second");
            // Never later than a change left to settle.
            assert!(commit_wait(Some(ms(since_save))) <= save_wait(Duration::ZERO, Some(ms(since_save))));
        }
    }

    #[test]
    fn every_answer_maps_to_its_schedule() {
        assert_eq!(ReadAnswer::Data(OverlayData::default(), None).outcome(), Outcome::Good);
        assert_eq!(ReadAnswer::NotModified.outcome(), Outcome::Good, "a 304 is a good read");
        assert_eq!(ReadAnswer::SignedOut.outcome(), Outcome::SignedOut);
        assert_eq!(ReadAnswer::NoRoute.outcome(), Outcome::NoRoute);
        assert_eq!(ReadAnswer::Unreachable("HTTP 429".into(), Some(Limit(None))).outcome(), Outcome::Unreachable);
        // A 429 carries the server's wait to the schedule; nothing else does.
        let wait = Some(Limit(Some(Duration::from_secs(3))));
        assert_eq!(ReadAnswer::Unreachable("HTTP 429".into(), wait).limit(), wait);
        assert_eq!(ReadAnswer::Unreachable("timeout".into(), None).limit(), None);
        assert_eq!(ReadAnswer::SignedOut.limit(), None);
    }

    #[test]
    fn every_id_in_a_request_path_is_encoded_and_never_cut() {
        let odd = "a b/../c?d#e";
        let enc = "a%20b%2F..%2Fc%3Fd%23e";
        let ch = "LI VE";
        let w = |edit: Local| {
            let mut all = write_requests(ch, &edit);
            assert_eq!(all.len(), 1, "one request");
            all.remove(0)
        };
        let r = w(Local::Adjust { item_id: odd.into(), delta: 12 });
        assert_eq!((r.verb, r.path, r.body, r.unpin), (Verb::Patch, format!("/api/inventory/{enc}"), Some(serde_json::json!({ "delta": 12 })), false), "one PATCH with the sum");
        let r = w(Local::PinItem { item_id: odd.into(), name: "x".into(), have: 1.0 });
        assert_eq!((r.verb, r.path, r.body), (Verb::Put, format!("/api/channels/LI%20VE/item-pins/{enc}"), Some(serde_json::json!({}))));
        let r = w(Local::UnpinItem { item_id: odd.into() });
        assert_eq!((r.verb, r.path, r.body, r.unpin), (Verb::Delete, format!("/api/channels/LI%20VE/item-pins/{enc}"), None, true));
        let r = w(Local::PinMission { mission_id: odd.into(), title: "t".into(), giver: None });
        assert_eq!((r.verb, r.path, r.body), (Verb::Put, format!("/api/channels/LI%20VE/mission-pins/{enc}"), None));
        let r = w(Local::UnpinMission { mission_id: odd.into() });
        assert_eq!((r.verb, r.path, r.unpin), (Verb::Delete, format!("/api/channels/LI%20VE/mission-pins/{enc}"), true));
        let r = w(Local::Choose { id: 41, item_id: odd.into() });
        assert_eq!((r.verb, r.path, r.body, r.unpin), (Verb::Post, "/api/pending/41/choose".to_string(), Some(serde_json::json!({ "itemId": odd })), false), "the body carries the id as it is");
        // A long id goes whole (the overlay only sends ids within `model::MAX_ID_CHARS`).
        let long = "x".repeat(model::MAX_ID_CHARS);
        assert_eq!(w(Local::UnpinItem { item_id: long.clone() }).path, format!("/api/channels/LI%20VE/item-pins/{long}"));
    }

    #[test]
    fn a_blueprint_detail_is_asked_for_with_its_key_percent_encoded() {
        assert_eq!(detail_path("LIVE", "BP_CRAFT_behr_rifle_ballistic_01"), "/api/channels/LIVE/overlay/blueprint/BP_CRAFT_behr_rifle_ballistic_01");
        assert_eq!(detail_path("LI VE", "BP_a.b-c"), "/api/channels/LI%20VE/overlay/blueprint/BP_a.b-c", "the channel encoded like every segment");
        // `..` is a key the route's pattern allows (see `model::blueprint_usable`): encoded
        // whole, so no URL normalisation turns it into a step up.
        assert!(model::blueprint_usable(".."));
        assert_eq!(detail_path("LIVE", ".."), "/api/channels/LIVE/overlay/blueprint/%2E%2E");
        assert_eq!(detail_path("LIVE", "a b/c"), "/api/channels/LIVE/overlay/blueprint/a%20b%2Fc", "never sent (see `fetch_detail`), still encoded");
    }

    #[test]
    fn the_detail_answer_is_read_like_the_others() {
        let body = br#"{"blueprint":"BP_a","name":"A","craftTimeSec":90,"slots":[],"sources":[{"title":"T","when":"accept","chance":0.5}],"sourcesMore":0}"#;
        match detail_answer(200, body, false, Some("\"d1\"".into()), None) {
            DetailAnswer::Data(d, etag) => assert_eq!((d.craft_time_sec, d.sources.len(), etag.as_deref()), (90.0, 1, Some("\"d1\""))),
            other => panic!("{other:?}"),
        }
        assert!(matches!(detail_answer(200, body, false, Some(" ".into()), None), DetailAnswer::Data(_, None)), "a blank ETag is none");
        assert_eq!(detail_answer(304, b"", true, None, None), DetailAnswer::NotModified);
        assert_eq!(detail_answer(304, b"", false, None, None), DetailAnswer::Failed(unexpected_304().into(), None), "a 304 without an ETag sent keeps nothing");
        assert!(matches!(detail_answer(401, b"", true, None, None), DetailAnswer::Failed(e, None) if e.contains("Entre")));
        assert!(matches!(detail_answer(404, r#"{"error":"blueprint desconhecida"}"#.as_bytes(), false, None, None), DetailAnswer::Failed(e, None) if e == "blueprint desconhecida"), "the server's sentence");
        assert!(matches!(detail_answer(404, b"", false, None, None), DetailAnswer::Failed(e, None) if e.contains("ainda não")), "a server without the route");
        assert!(matches!(detail_answer(400, r#"{"error":"nome inválido"}"#.as_bytes(), false, None, None), DetailAnswer::Failed(e, None) if e == "nome inválido"));
        let wait = Some(Duration::from_secs(5));
        assert!(matches!(detail_answer(429, b"", true, None, wait), DetailAnswer::Failed(_, Some(Limit(w))) if w == wait), "a 429 holds the details back");
        assert!(matches!(detail_answer(200, b"[]", false, None, None), DetailAnswer::Failed(_, None)), "not a detail");
    }

    #[test]
    fn a_missions_blueprints_are_asked_for_with_its_contract_percent_encoded() {
        assert_eq!(mission_path("LIVE", "rayari_research_01"), "/api/channels/LIVE/overlay/mission/rayari_research_01/blueprints");
        assert_eq!(mission_path("LI VE", "a b?c#d"), "/api/channels/LI%20VE/overlay/mission/a%20b%3Fc%23d/blueprints", "every segment encoded");
        assert_eq!(mission_path("LIVE", ".."), "/api/channels/LIVE/overlay/mission/%2E%2E/blueprints", "never a step up");
        assert!(!model::contract_usable("a/b") && !model::contract_usable(""), "never sent (see `fetch_mission`)");
    }

    #[test]
    fn the_mission_answer_is_read_like_the_detail() {
        let body = br#"{"title":"T","giver":null,"pools":[{"when":"complete","poolChance":0.25,"total":2,"acquired":1,"blueprints":[{"blueprint":"BP_a","name":"A","kind":"Weapon","acquired":true}]}]}"#;
        match mission_answer(200, body, false, Some("\"m1\"".into()), None) {
            MissionAnswer::Data(m, etag) => assert_eq!((m.title.as_str(), m.pools.len(), m.pools[0].pool_chance, etag.as_deref()), ("T", 1, Some(0.25), Some("\"m1\""))),
            other => panic!("{other:?}"),
        }
        assert_eq!(mission_answer(304, b"", true, None, None), MissionAnswer::NotModified);
        assert_eq!(mission_answer(304, b"", false, None, None), MissionAnswer::Failed(unexpected_304().into(), None), "a 304 without an ETag sent keeps nothing");
        assert!(matches!(mission_answer(401, b"", true, None, None), MissionAnswer::Failed(e, None) if e.contains("Entre")));
        assert!(matches!(mission_answer(404, b"", false, None, None), MissionAnswer::Failed(e, None) if e == "O site ainda não tem as blueprints por missão."), "a server without the route");
        assert!(matches!(mission_answer(404, r#"{"error":"missão desconhecida"}"#.as_bytes(), false, None, None), MissionAnswer::Failed(e, None) if e == "missão desconhecida"), "the server's sentence");
        assert!(matches!(mission_answer(400, r#"{"error":"contrato inválido"}"#.as_bytes(), false, None, None), MissionAnswer::Failed(e, None) if e == "contrato inválido"));
        let wait = Some(Duration::from_secs(5));
        assert!(matches!(mission_answer(429, b"", true, None, wait), MissionAnswer::Failed(_, Some(Limit(w))) if w == wait), "a 429 holds the details and missions back");
        assert!(matches!(mission_answer(200, b"[]", false, None, None), MissionAnswer::Failed(_, None)), "not a mission");
    }

    #[test]
    fn a_move_is_one_request_per_quality_with_the_places_in_the_body() {
        let odd = "a b/../c?d#e";
        let mv = Local::Move { item_id: odd.into(), from: "Stanton2_Orison".into(), to: "".into(), parts: vec![(0, 3), (500, 1)] };
        let reqs = write_requests("LIVE", &mv);
        assert_eq!(reqs.len(), 2, "one per quality, in order");
        for r in &reqs {
            assert_eq!((r.verb, r.path.as_str(), r.unpin, r.what), (Verb::Post, "/api/inventory/a%20b%2F..%2Fc%3Fd%23e/move", false, "mover o item"), "the item id encoded; no channel (the inventory is the account's)");
        }
        assert_eq!(reqs[0].body, Some(serde_json::json!({ "from": "Stanton2_Orison", "to": "", "qty": 3, "quality": 0 })));
        assert_eq!(reqs[1].body, Some(serde_json::json!({ "from": "Stanton2_Orison", "to": "", "qty": 1, "quality": 500 })), "the quality stays: no toQuality");
        let none = Local::Move { item_id: "x".into(), from: "A".into(), to: "B".into(), parts: vec![] };
        assert!(write_requests("LIVE", &none).is_empty(), "never sent: `change_usable` refuses it");
    }

    #[test]
    fn the_locations_answer_is_read_like_the_others() {
        let (list, limit) = locations_answer(200, br#"{"version":"x","locations":[{"id":"a","name":"Alpha"}],"contracts":[]}"#, None);
        assert_eq!((list.map(|l| l.len()), limit), (Ok(1), None));
        let wait = Some(Duration::from_secs(3));
        assert!(matches!(locations_answer(429, b"", wait), (Err(_), Some(Limit(w))) if w == wait), "a 429 holds everything back");
        assert!(matches!(locations_answer(404, r#"{"error":"sem dados extraídos"}"#.as_bytes(), None), (Err(e), None) if e == "sem dados extraídos"));
        assert!(matches!(locations_answer(200, b"[]", None), (Err(_), None)), "not a dataset");
    }

    #[test]
    fn requests_go_to_the_signed_in_server_only() {
        let signed_in = agent::AgentConfig { server: "https://x".into(), token: "t".into(), channel: Some("PTU".into()), ..Default::default() };
        assert_eq!(target(&signed_in), (("https://x".to_string(), "t".to_string(), "PTU".to_string()), true));
        // Signed out: no server and no token.
        let out = agent::AgentConfig { server: "https://x".into(), channel: Some(" ".into()), ..Default::default() };
        assert_eq!(target(&out), ((String::new(), String::new(), "LIVE".to_string()), false));
        let no_server = agent::AgentConfig { token: "t".into(), ..Default::default() };
        let (ident, signed_in) = target(&no_server);
        assert_eq!((ident.1.as_str(), signed_in), ("", false), "a token without its server is not sent anywhere");
    }

    #[test]
    fn a_304_keeps_the_data_only_when_an_etag_was_sent() {
        let etag = || Some("\"v7\"".to_string());
        assert!(matches!(read_answer(304, b"", true, None, None), ReadAnswer::NotModified));
        match read_answer(304, b"", false, None, None) {
            ReadAnswer::Unreachable(e, None) => assert_eq!(e, unexpected_304()),
            _ => panic!("a 304 to a request without an ETag keeps nothing"),
        }
        match read_answer(200, br#"{"channel":"LIVE"}"#, true, etag(), None) {
            ReadAnswer::Data(d, tag) => assert_eq!((d.channel.as_str(), tag), ("LIVE", etag())),
            _ => panic!("a 200 is the data, with its ETag"),
        }
        // An older server: no ETag at all; the data is taken as it is, and nothing is sent next.
        assert!(matches!(read_answer(200, b"{}", false, None, None), ReadAnswer::Data(_, None)));
        assert!(matches!(read_answer(200, b"{}", false, Some(" ".into()), None), ReadAnswer::Data(_, None)), "a blank ETag is none");
        // The rest as before.
        assert!(matches!(read_answer(401, b"", true, None, None), ReadAnswer::SignedOut));
        assert!(matches!(read_answer(404, b"", false, None, None), ReadAnswer::NoRoute));
        let wait = Some(Duration::from_secs(4));
        assert!(matches!(read_answer(429, b"", true, None, wait), ReadAnswer::Unreachable(_, Some(Limit(w))) if w == wait));
        assert!(matches!(read_answer(500, b"", true, None, None), ReadAnswer::Unreachable(_, None)));
    }

    #[test]
    fn etags_are_sent_back_only_when_sane() {
        assert_eq!(usable_etag(Some("\"abc\"".into())), Some("\"abc\"".to_string()));
        assert_eq!(usable_etag(Some("W/\"abc\"".into())), Some("W/\"abc\"".to_string()), "a weak one too");
        assert_eq!(usable_etag(None), None);
        assert_eq!(usable_etag(Some(String::new())), None);
        assert_eq!(usable_etag(Some("x".repeat(MAX_ETAG + 1))), None, "too long");
        assert_eq!(usable_etag(Some("a\u{7f}b".into())), None, "not printable");
    }

    #[test]
    fn an_older_server_without_the_index_falls_back_to_the_per_query_search() {
        assert_eq!(index_answer(404, b"", false, None, None), IndexAnswer::OldServer, "a plain 404: the route does not exist");
        assert_eq!(index_answer(404, br#"<html>not found</html>"#, false, None, None), IndexAnswer::OldServer);
        assert!(matches!(index_answer(404, br#"{"error":"canal sem dados"}"#, false, None, None), IndexAnswer::Failed(e, None) if e == "canal sem dados"), "a 404 with a message is the server's answer, not a missing route");
        assert_eq!(index_answer(304, b"", true, None, None), IndexAnswer::NotModified);
        assert_eq!(index_answer(304, b"", false, None, None), IndexAnswer::Failed(unexpected_304().into(), None));
        let wait = Some(Duration::from_secs(9));
        assert!(matches!(index_answer(429, b"", true, None, wait), IndexAnswer::Failed(_, Some(Limit(w))) if w == wait));
        assert!(matches!(index_answer(503, b"", true, None, None), IndexAnswer::Failed(_, None)));
        assert!(matches!(index_answer(200, b"[]", false, None, None), IndexAnswer::Failed(_, None)), "not an index");
        match index_answer(200, br#"{"version":"v7","items":[{"itemId":"a","name":"Carinite","used":true}],"missions":[]}"#, false, Some("\"v7\"".into()), None) {
            IndexAnswer::Data(index, etag) => {
                assert_eq!((index.version.as_str(), index.items.len(), etag.as_deref()), ("v7", 1, Some("\"v7\"")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_inventory_answer_is_read_like_the_others() {
        let (owned, limit) = inventory_answer(200, br#"[{"itemId":"a","qty":2,"places":[{"location":"X","qty":2,"quality":0}],"new":1}]"#, None);
        assert_eq!((owned.map(|o| o["a"].qty), limit), (Ok(2.0), None));
        assert!(matches!(inventory_answer(401, b"", None), (Err(e), None) if e.contains("Entre")));
        let wait = Some(Duration::from_secs(3));
        assert!(matches!(inventory_answer(429, b"", wait), (Err(_), Some(Limit(w))) if w == wait), "a 429 holds the inventory back");
        assert!(matches!(inventory_answer(500, br#"{"error":"falhou"}"#, None), (Err(e), None) if e == "falhou"));
        assert!(matches!(inventory_answer(200, b"{}", None), (Err(_), None)), "not a list");
    }
}

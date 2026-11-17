//! The overlay lives and dies with the game, like Discord's overlay: on screen only while the
//! game's window is usable (there, visible, not hung), and out of the way otherwise.
//!
//! - Before the game is there, the overlay process has no window at all: it holds its mutex
//!   and looks for the game's window every [`CHECK_EVERY`] ([`wait_for_game`]). No hotkeys,
//!   no reads from the server, nothing drawn.
//! - Once the game is usable, the window is created over the game's monitor (closed, without
//!   taking the focus) and a watch thread keeps looking every [`CHECK_EVERY`] at that very
//!   game window ([`start_watch`], [`os::Followed`]).
//! - When the game goes away (closed, crashed, replaced by a new window, or hung for
//!   [`HUNG_LOOKS`] looks in a row, see [`judge`]), the watch thread marks it
//!   ([`Life::game_left`]) and wakes the UI, which closes the overlay (mouse released, focus
//!   handed to nobody) and ends the event loop; the process then starts a fresh copy of
//!   itself and exits ([`agent::respawn_overlay`]), and the copy waits for the game again.
//!   If the UI does not get there in [`UI_CLOSE_WAIT`] (a crash of the game often resets the
//!   graphics device, and a GL window on a lost device can stop drawing or show black), the
//!   watch thread starts the copy and ends the process at once from its own thread: the
//!   window goes with the process, without anything to wait for from a stuck UI thread.
//!
//! Why a new process rather than showing the same window again: nothing in eframe / glow
//! recovers a GL context lost in a device reset, and winit does not redraw a hidden window, so
//! the old window could come back opaque black or not at all. A new process gets a new window,
//! a new GL context and a new device, which are transparent by construction; it costs one
//! process start per game session. The watch does not depend on the UI thread running: it
//! is a plain thread that only makes window-manager queries on the game's window (owner,
//! visibility, title, `IsHungAppWindow`), none of which waits on the overlay's own window.
//!
//! `--preview` skips all of this: the overlay shows at once, without the game, as before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::agent;
use crate::overlay_win::{self as os, GameState};

/// How often the game's window is looked at, while waiting for it and while on it.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How long the UI gets to close the overlay by itself once the game is gone, before the
/// watch thread replaces the process itself.
const UI_CLOSE_WAIT: Duration = Duration::from_millis(500);
/// How many looks in a row must find the game's window hung before it counts as gone: Windows
/// calls a window hung once it has not taken a message for 5 s, and a long loading stall
/// that passes must not cost the overlay its process.
const HUNG_LOOKS: u32 = 3;
/// How long a replacement waits for its predecessor to let go of the overlay's mutex.
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_RETRY: Duration = Duration::from_millis(100);
/// How often the watch thread looks whether the UI has finished, while waiting for it.
const UI_DONE_POLL: Duration = Duration::from_millis(50);

/// Whether the overlay belongs on screen: always in preview, else only on a usable game
/// (a hung game counts as gone).
pub fn shown(preview: bool, game: GameState) -> bool {
    preview || game == GameState::Usable
}

/// Closed, whether the overlay draws its cards and toasts: with the game's window around,
/// only while that window is in front (Alt+Tab to a browser shows nothing over it); without
/// it, only in preview.
pub fn closed_draws(preview: bool, game_found: bool, game_in_front: bool) -> bool {
    if game_found { game_in_front } else { preview }
}

/// What waiting for the game does after one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Look again in a moment.
    Keep,
    /// The game is usable: create the window.
    Show,
    /// The overlay was asked to close (`overlay.stop`): end.
    Stop,
}

pub fn wait_step(stop_requested: bool, game: GameState) -> Wait {
    if stop_requested {
        Wait::Stop
    } else if shown(false, game) {
        Wait::Show
    } else {
        Wait::Keep
    }
}

/// One look of the watch thread at the game's window the overlay is on. `hung_before`: the
/// looks in a row just before this one that found it hung. Returns whether the game counts as
/// gone now, and that count for the next look. Gone (closed, or no longer the window the
/// overlay was shown on): at once. Hung: only at the [`HUNG_LOOKS`]th look in a row; a
/// usable look starts the count again.
pub fn judge(game: GameState, hung_before: u32) -> (bool, u32) {
    match game {
        GameState::Gone => (true, 0),
        GameState::Usable => (false, 0),
        GameState::Hung => {
            let hung = hung_before.saturating_add(1);
            (hung >= HUNG_LOOKS, hung)
        }
    }
}

/// Without any window: look at the game's every [`CHECK_EVERY`] until it is usable (`true`)
/// or the overlay is asked to close (`false`).
pub fn wait_for_game() -> bool {
    loop {
        match wait_step(agent::overlay_stop_path().exists(), os::game_state().0) {
            Wait::Show => return true,
            Wait::Stop => return false,
            Wait::Keep => std::thread::sleep(CHECK_EVERY),
        }
    }
}

/// This process replaces an overlay that left with the game (see [`agent::respawn_overlay`]).
pub fn respawned() -> bool {
    std::env::var_os(agent::OVERLAY_RESPAWN_ENV).is_some_and(|v| v == "1")
}

/// The overlay's mutex. A replacement waits (up to [`LOCK_WAIT`]) for its predecessor to let
/// it go; anyone else leaves at once when another overlay holds it (`Ok(None)`).
pub fn lock(respawned: bool) -> anyhow::Result<Option<agent::OverlayLock>> {
    let started = Instant::now();
    loop {
        if let Some(l) = agent::overlay_lock()? {
            return Ok(Some(l));
        }
        if !respawned || started.elapsed() >= LOCK_WAIT {
            return Ok(None);
        }
        std::thread::sleep(LOCK_RETRY);
    }
}

/// What the UI, `overlay::run` and the watch thread share.
#[derive(Default)]
pub struct Life {
    /// The game went away: the overlay closes and the process replaces itself.
    left: AtomicBool,
    /// The event loop has returned: the watch thread has nothing more to do.
    ui_done: AtomicBool,
    /// The overlay closed because of a stop request (`overlay.stop`): it acted on the request,
    /// so `overlay::run` removes it.
    stopped: AtomicBool,
    /// The replacement was started (whoever comes first does it, once; the lock also makes
    /// the watch thread wait for a start in progress on the main thread before it exits).
    respawned: Mutex<bool>,
}

impl Life {
    pub fn game_left(&self) -> bool {
        self.left.load(Ordering::SeqCst)
    }

    /// The event loop has returned.
    pub fn set_ui_done(&self) {
        self.ui_done.store(true, Ordering::SeqCst);
    }

    fn is_ui_done(&self) -> bool {
        self.ui_done.load(Ordering::SeqCst)
    }

    /// The UI is closing because of a stop request.
    pub fn set_stopped(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// The overlay closed because of a stop request.
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Start the replacement process, once.
    pub fn respawn(&self) {
        let mut done = self.respawned.lock().unwrap_or_else(|e| e.into_inner());
        if !*done {
            *done = true;
            // Nothing better to do on failure: the next `start` or the tray brings it back.
            let _ = agent::respawn_overlay();
        }
    }

    /// Wait up to `d` for the event loop to return; `true` once it has.
    fn wait_ui(&self, d: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < d {
            if self.is_ui_done() {
                return true;
            }
            std::thread::sleep(UI_DONE_POLL);
        }
        self.is_ui_done()
    }
}

/// Start the thread that watches the game's window the overlay was shown on (`game`, see
/// [`os::follow`]; `None`: it was already gone) while the overlay is on it (not in preview).
pub fn start_watch(life: Arc<Life>, ctx: egui::Context, game: Option<os::Followed>) {
    // Without the thread the overlay stays until closed by hand, as before; nothing better to do.
    let _ = std::thread::Builder::new().name("overlay-game".into()).spawn(move || watch(&life, &ctx, game));
}

fn watch(life: &Life, ctx: &egui::Context, game: Option<os::Followed>) {
    let mut hung = 0;
    loop {
        std::thread::sleep(CHECK_EVERY);
        if life.is_ui_done() {
            return;
        }
        let (gone, streak) = judge(game.map_or(GameState::Gone, os::followed_state), hung);
        hung = streak;
        if !gone {
            continue;
        }
        life.left.store(true, Ordering::SeqCst);
        // The UI closes the overlay on its next frame (see `Overlay::leave`).
        ctx.request_repaint();
        if life.wait_ui(UI_CLOSE_WAIT) {
            return;
        }
        // The UI is stuck (a lost device, a window that gets no more frames): replace the
        // process from here, now. The window goes with the process; nothing is asked of the
        // stuck UI thread. Layout changes were saved when made (see `Overlay::commit`).
        life.respawn();
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_usable_game_shows_the_overlay_unless_previewing() {
        assert!(shown(false, GameState::Usable));
        assert!(!shown(false, GameState::Gone), "no game: hidden");
        assert!(!shown(false, GameState::Hung), "a frozen game counts as gone");
        for g in [GameState::Gone, GameState::Hung, GameState::Usable] {
            assert!(shown(true, g), "preview always shows: {g:?}");
        }
    }

    #[test]
    fn closed_it_draws_over_the_game_in_front_or_in_preview() {
        // With the game's window: only while it is in front, preview or not.
        assert!(closed_draws(false, true, true));
        assert!(!closed_draws(false, true, false), "Alt+Tab away from the game");
        assert!(closed_draws(true, true, true));
        assert!(!closed_draws(true, true, false));
        // Without it: only the preview draws (the desktop behaviour of before).
        assert!(!closed_draws(false, false, false));
        assert!(closed_draws(true, false, false));
    }

    #[test]
    fn waiting_ends_on_a_usable_game_or_a_stop_request() {
        assert_eq!(wait_step(false, GameState::Gone), Wait::Keep);
        assert_eq!(wait_step(false, GameState::Hung), Wait::Keep, "a hung game is not shown over");
        assert_eq!(wait_step(false, GameState::Usable), Wait::Show);
        for g in [GameState::Gone, GameState::Hung, GameState::Usable] {
            assert_eq!(wait_step(true, g), Wait::Stop, "a stop request wins: {g:?}");
        }
    }

    #[test]
    fn a_fresh_life_has_not_seen_the_game_leave() {
        let life = Life::default();
        assert!(!life.game_left());
        assert!(!life.wait_ui(Duration::ZERO));
        life.set_ui_done();
        assert!(life.wait_ui(Duration::ZERO));
    }

    #[test]
    fn only_a_stop_request_marks_the_overlay_as_stopped() {
        let life = Life::default();
        assert!(!life.stopped(), "closed any other way: the request file is not touched");
        life.set_stopped();
        assert!(life.stopped());
        assert!(!life.game_left(), "a stop is not the game leaving");
    }

    #[test]
    fn a_game_that_is_gone_counts_at_once() {
        assert_eq!(judge(GameState::Gone, 0), (true, 0));
        assert_eq!(judge(GameState::Gone, 2), (true, 0), "gone while hung: at once too");
        assert_eq!(judge(GameState::Usable, 0), (false, 0));
    }

    #[test]
    fn a_hung_game_counts_as_gone_only_after_three_looks_in_a_row() {
        // Look by look, a game that stays hung.
        let mut hung = 0;
        let mut verdicts = Vec::new();
        for _ in 0..4 {
            let (gone, next) = judge(GameState::Hung, hung);
            verdicts.push(gone);
            hung = next;
        }
        assert_eq!(verdicts, [false, false, true, true]);
        // A stall that ends in time: the count starts over.
        let (gone, hung) = judge(GameState::Hung, 0);
        assert!(!gone);
        let (gone, hung) = judge(GameState::Hung, hung);
        assert!(!gone);
        let (gone, hung) = judge(GameState::Usable, hung);
        assert!(!gone);
        assert_eq!(hung, 0);
        assert!(!judge(GameState::Hung, hung).0, "one hung look after a usable one");
        assert_eq!(judge(GameState::Hung, u32::MAX), (true, u32::MAX), "no overflow");
    }
}

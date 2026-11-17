//! Global hotkeys for the overlay: Alt+Shift+F9 shows / hides it, Shift+Tab opens /
//! closes it. They must work while the game has the focus, which is the point.
//! Shift+Tab is only watched, never registered: registering it would take the reverse-Tab
//! away from every other program while the overlay runs. The overlay acts on it only over
//! the game.
//! Never Ctrl+Alt+letter: Windows reports AltGr as Ctrl+Alt, so such a key would swallow
//! AltGr+letter system-wide (`?` and `°` on an ABNT2 keyboard).
//!
//! Two sources feed the same channel:
//! - `RegisterHotKey` with no window posts `WM_HOTKEY` to the registering thread, so a
//!   dedicated thread registers both keys and runs a plain message loop. It also swallows
//!   the key over normal apps. A game that reads raw input with `RIDEV_NOHOTKEYS` (Star
//!   Citizen) never lets `WM_HOTKEY` through, though.
//! - So a second small thread samples `GetAsyncKeyState` every [`POLL_EVERY`] and turns the
//!   readings into presses ([`EdgeDetector`]). Reading the key state is neither a hook nor
//!   input injection: it asks the system for the state it keeps for every process, and
//!   nothing is sent to or read from the game.
//!
//! Both may see the same press: [`Dedupe`] drops a report of a key when the OTHER source
//! reported that key within [`DEDUPE_WINDOW`]; two reports from the same source are two
//! presses (each source reports a press once). Each press that gets through goes to the UI
//! thread, followed by a
//! repaint request. Dropping [`Hotkeys`] stops the polling thread and posts `WM_QUIT` to the
//! message thread, which unregisters the keys; both are joined.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum HotkeyEvent {
    ToggleVisible,
    ToggleInteractive,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl HotkeyEvent {
    /// Its place in [`KEYS`].
    fn slot(self) -> usize {
        match self {
            HotkeyEvent::ToggleVisible => 0,
            HotkeyEvent::ToggleInteractive => 1,
        }
    }
}

/// `VK_F9` / `VK_TAB`.
#[cfg_attr(not(windows), allow(dead_code))]
const VK_F9: u32 = 0x78;
#[cfg_attr(not(windows), allow(dead_code))]
const VK_TAB: u32 = 0x09;

/// The two keys, their labels and what they do. Both need Shift and no Ctrl (a movement key
/// in the game; AltGr is Ctrl+Alt) nor Windows key.
#[cfg_attr(not(windows), allow(dead_code))]
const KEYS: [(HotkeyEvent, u32, &str); 2] = [(HotkeyEvent::ToggleVisible, VK_F9, "Alt+Shift+F9"), (HotkeyEvent::ToggleInteractive, VK_TAB, "Shift+Tab")];
/// Per key, whether Alt must be held (`true`) or must not be (`false`: Alt+Shift+Tab is the
/// system's window switch). A key with Alt is also registered with `RegisterHotKey`; one
/// without is only polled.
#[cfg_attr(not(windows), allow(dead_code))]
const WITH_ALT: [bool; 2] = [true, false];

/// Text naming the hotkeys (overlay bar, settings window).
pub fn help() -> &'static str {
    tr!("Shift+Tab opens/closes · Alt+Shift+F9 shows/hides", "Shift+Tab abre/fecha · Alt+Shift+F9 mostra/esconde")
}

/// How often the polling thread samples the keys.
#[cfg_attr(not(windows), allow(dead_code))]
const POLL_EVERY: Duration = Duration::from_millis(25);
/// A report of a key within this long after the OTHER source reported it is the same press
/// (a few polling intervals: the poll sees a press at most [`POLL_EVERY`] after WM_HOTKEY).
#[cfg_attr(not(windows), allow(dead_code))]
const DEDUPE_WINDOW: Duration = Duration::from_millis(150);

/// Where a press was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Source {
    /// `WM_HOTKEY` from `RegisterHotKey`.
    Message,
    /// The `GetAsyncKeyState` polling thread.
    Poll,
}

/// One reading of the keys that make up the hotkeys; `keys` follows [`KEYS`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct KeyState {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Either Windows key.
    pub win: bool,
    pub keys: [bool; 2],
}

/// Turns key readings into presses: a key fires on the reading where it goes down while
/// Shift is held, Alt is as [`WITH_ALT`] says for that key, and neither Ctrl (AltGr is
/// Ctrl+Alt) nor a Windows key is down.
/// Holding it fires nothing more; it fires again only after it was seen up.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct EdgeDetector {
    /// The keys on the previous reading; `None` before the first one.
    prev: Option<[bool; 2]>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl EdgeDetector {
    pub fn feed(&mut self, s: KeyState) -> Vec<HotkeyEvent> {
        // The first reading only sets the baseline: a key already held is not a press.
        let Some(prev) = self.prev.replace(s.keys) else { return Vec::new() };
        let mods = s.shift && !s.ctrl && !s.win;
        KEYS.iter().enumerate().filter(|(i, _)| mods && s.alt == WITH_ALT[*i] && s.keys[*i] && !prev[*i]).map(|(_, (event, _, _))| *event).collect()
    }
}

/// Drops a report of a key when the other source reported the same key within
/// [`DEDUPE_WINDOW`] before it (both sources seeing one press). A report from the same source
/// as the last one is always a new press. Once a report has been paired with the other
/// source's, the pair is done: the next report of that key starts afresh.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Dedupe {
    /// Per key: the last accepted report not yet paired with the other source's.
    last: [Option<(Source, Instant)>; 2],
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Dedupe {
    pub fn accept(&mut self, event: HotkeyEvent, source: Source, now: Instant) -> bool {
        let last = &mut self.last[event.slot()];
        if let Some((from, at)) = *last
            && from != source
            && now.saturating_duration_since(at) < DEDUPE_WINDOW
        {
            *last = None;
            return false;
        }
        *last = Some((source, now));
        true
    }
}

/// What both sources send through: drops the second sighting of a press, wakes the UI.
#[derive(Clone)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Forward {
    tx: Sender<HotkeyEvent>,
    ctx: egui::Context,
    dedupe: std::sync::Arc<std::sync::Mutex<Dedupe>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Forward {
    /// `false` once the overlay is gone (nobody receives any more).
    fn send(&self, event: HotkeyEvent, source: Source) -> bool {
        let fresh = self.dedupe.lock().map(|mut d| d.accept(event, source, Instant::now())).unwrap_or(true);
        if !fresh {
            return true;
        }
        if self.tx.send(event).is_err() {
            return false;
        }
        self.ctx.request_repaint();
        true
    }
}

pub struct Hotkeys {
    /// Labels of the keys another program already holds (or the reason none work).
    pub failed: Vec<String>,
    #[cfg(windows)]
    thread_id: u32,
    #[cfg(windows)]
    join: Option<std::thread::JoinHandle<()>>,
    /// Tells the polling thread to end.
    #[cfg(windows)]
    poll_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(windows)]
    poll_join: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
pub fn start(tx: Sender<HotkeyEvent>, ctx: egui::Context) -> Hotkeys {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};

    use crate::trf;

    let fwd = Forward { tx, ctx, dedupe: Arc::new(Mutex::new(Dedupe::default())) };
    let poll_stop = Arc::new(AtomicBool::new(false));
    let poll_join = {
        let fwd = fwd.clone();
        let stop = poll_stop.clone();
        // Without it the keys still work wherever WM_HOTKEY arrives.
        std::thread::Builder::new().name("hotkeys-poll".into()).spawn(move || win::poll(fwd, stop)).ok()
    };
    // The thread reports its id (needed to post WM_QUIT) and which keys it could not take.
    let (ready_tx, ready_rx) = channel::<(u32, Vec<String>)>();
    let (thread_id, join, failed) = match std::thread::Builder::new().name("hotkeys".into()).spawn(move || win::run(fwd, ready_tx)) {
        Err(e) => (0, None, vec![trf!("hotkeys unavailable ({e})", "atalhos indisponíveis ({e})")]),
        Ok(j) => match ready_rx.recv() {
            Ok((thread_id, failed)) => (thread_id, Some(j), failed),
            Err(_) => (0, None, vec![tr!("hotkeys unavailable", "atalhos indisponíveis").into()]),
        },
    };
    Hotkeys { failed, thread_id, join, poll_stop, poll_join }
}

#[cfg(not(windows))]
pub fn start(_tx: Sender<HotkeyEvent>, _ctx: egui::Context) -> Hotkeys {
    Hotkeys { failed: vec![tr!("global hotkeys only on Windows", "atalhos globais só no Windows").into()] }
}

#[cfg(windows)]
impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.poll_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(join) = self.poll_join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.join.take() {
            win::quit(self.thread_id);
            let _ = join.join();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MOD_ALT, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT, WM_USER};

    use super::{EdgeDetector, Forward, KEYS, KeyState, POLL_EVERY, Source, WITH_ALT};
    use crate::trf;

    /// `ERROR_HOTKEY_ALREADY_REGISTERED`.
    const ALREADY_TAKEN: u32 = 1409;
    /// `VK_SHIFT`, `VK_CONTROL`, `VK_MENU` (either side of each).
    const VK_SHIFT: u32 = 0x10;
    const VK_CONTROL: u32 = 0x11;
    const VK_MENU: u32 = 0x12;
    /// `VK_LWIN`, `VK_RWIN` (no combined code exists for the Windows keys).
    const VK_LWIN: u32 = 0x5B;
    const VK_RWIN: u32 = 0x5C;

    pub(super) fn run(fwd: Forward, ready: Sender<(u32, Vec<String>)>) {
        // SAFETY: an all-zero MSG is a valid value (plain integers and a POINT).
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: `msg` is a valid, writable MSG; peeking creates this thread's message queue
        // so that PostThreadMessageW(WM_QUIT) from the UI thread cannot be lost.
        unsafe { PeekMessageW(&mut msg, 0, WM_USER, WM_USER, PM_NOREMOVE) };
        // SAFETY: no arguments, no pointers.
        let thread_id = unsafe { GetCurrentThreadId() };

        let mut registered: Vec<i32> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for (i, (_, key, label)) in KEYS.iter().enumerate() {
            // Without Alt (Shift+Tab) the key is left to the other programs: polled only.
            if !WITH_ALT[i] {
                continue;
            }
            let id = i as i32 + 1;
            // SAFETY: no window (0): WM_HOTKEY goes to this thread's queue; plain integers.
            let ok = unsafe { RegisterHotKey(0, id, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, *key) } != 0;
            if ok {
                registered.push(id);
            } else {
                // SAFETY: no arguments; read right after the failed call on the same thread.
                let err = unsafe { GetLastError() };
                failed.push(if err == ALREADY_TAKEN {
                    trf!("{label} is already in use by another program", "{label} já está em uso por outro programa")
                } else {
                    trf!("{label} unavailable (error {err})", "{label} indisponível (erro {err})")
                });
            }
        }
        let _ = ready.send((thread_id, failed));

        loop {
            // SAFETY: `msg` is a valid, writable MSG; no window filter.
            let got = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
            // 0 = WM_QUIT, -1 = error: either way the loop is over.
            if got <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY {
                let id: usize = msg.wParam;
                if let Some((event, _, _)) = id.checked_sub(1).and_then(|i| KEYS.get(i))
                    && !fwd.send(*event, Source::Message)
                {
                    break;
                }
            }
        }
        for id in registered {
            // SAFETY: unregisters a hotkey this thread registered, with the same (null) window.
            unsafe { UnregisterHotKey(0, id) };
        }
    }

    /// Ask the hotkey thread to end its message loop.
    pub(super) fn quit(thread_id: u32) {
        // SAFETY: posting a message to a thread id carries no pointers; a dead id just fails.
        unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
    }

    /// Whether a key is down right now (the high bit of `GetAsyncKeyState`).
    fn down(vk: u32) -> bool {
        // SAFETY: plain integer in and out, no pointers. This only reads the physical key state
        // the system keeps for every process: it is not a hook and injects no input.
        let state = unsafe { GetAsyncKeyState(vk as i32) };
        (state as u16 & 0x8000) != 0
    }

    /// The second source: sample the keys until `stop`, sending each press.
    pub(super) fn poll(fwd: Forward, stop: Arc<AtomicBool>) {
        let mut edges = EdgeDetector::default();
        while !stop.load(Ordering::Relaxed) {
            let s = KeyState { ctrl: down(VK_CONTROL), shift: down(VK_SHIFT), alt: down(VK_MENU), win: down(VK_LWIN) || down(VK_RWIN), keys: KEYS.map(|(_, vk, _)| down(vk)) };
            for event in edges.feed(s) {
                if !fwd.send(event, Source::Poll) {
                    return;
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_alt_shift_f9_and_shift_tab_and_the_help_names_them() {
        assert_eq!(KEYS.map(|(_, vk, _)| vk), [0x78, 0x09]);
        assert_eq!(KEYS.map(|(_, _, label)| label), ["Alt+Shift+F9", "Shift+Tab"]);
        let help = help();
        for (event, _, label) in KEYS {
            assert!(help.contains(label), "{help}");
            assert_eq!(KEYS[event.slot()].0, event, "slot follows KEYS");
        }
        assert!(!help.contains("Ctrl"), "AltGr is Ctrl+Alt on Windows: no Ctrl+Alt keys");
    }

    fn state(alt: bool, shift: bool, ctrl: bool, f9: bool, tab: bool) -> KeyState {
        KeyState { ctrl, shift, alt, win: false, keys: [f9, tab] }
    }

    #[test]
    fn a_press_fires_once_on_the_way_down() {
        let mut e = EdgeDetector::default();
        assert!(e.feed(state(true, true, false, false, false)).is_empty(), "first reading: baseline");
        assert_eq!(e.feed(state(true, true, false, true, false)), vec![HotkeyEvent::ToggleVisible]);
        assert!(e.feed(state(true, true, false, true, false)).is_empty(), "held: nothing more");
        assert!(e.feed(state(true, true, false, true, false)).is_empty());
        assert!(e.feed(state(true, true, false, false, false)).is_empty(), "release fires nothing");
        assert_eq!(e.feed(state(true, true, false, true, false)), vec![HotkeyEvent::ToggleVisible], "pressed again");
        assert!(e.feed(state(true, true, false, false, false)).is_empty());
        assert_eq!(e.feed(state(false, true, false, false, true)), vec![HotkeyEvent::ToggleInteractive], "Shift+Tab");
        assert!(e.feed(state(false, true, false, false, true)).is_empty(), "held");
        assert!(e.feed(state(false, true, false, false, false)).is_empty());
        assert!(e.feed(state(true, true, false, false, true)).is_empty(), "Alt+Shift+Tab is the system's window switch");
        assert!(e.feed(KeyState::default()).is_empty());
        assert!(e.feed(state(false, false, false, false, true)).is_empty(), "plain Tab");
    }

    #[test]
    fn modifiers_must_be_exact() {
        let mut e = EdgeDetector::default();
        e.feed(KeyState::default());
        assert!(e.feed(state(false, true, false, true, false)).is_empty(), "no Alt");
        e.feed(KeyState::default());
        assert!(e.feed(state(true, false, false, true, false)).is_empty(), "no Shift");
        e.feed(KeyState::default());
        assert!(e.feed(state(true, true, true, true, false)).is_empty(), "Ctrl (or AltGr) held");
        e.feed(KeyState::default());
        assert!(e.feed(state(false, false, false, false, true)).is_empty(), "plain Tab");
        e.feed(KeyState::default());
        assert!(e.feed(state(false, false, false, true, false)).is_empty(), "plain F9");
        // F9 already down when Alt+Shift arrive: not a press of the hotkey.
        assert!(e.feed(state(true, true, false, true, false)).is_empty(), "the key went down before");
        e.feed(KeyState::default());
        assert!(e.feed(KeyState { win: true, ..state(false, true, false, false, true) }).is_empty(), "Win+Shift+Tab");
        e.feed(KeyState::default());
        assert!(e.feed(state(false, true, true, false, true)).is_empty(), "Ctrl+Shift+Tab");
        e.feed(KeyState::default());
        assert_eq!(e.feed(state(false, true, false, false, true)), vec![HotkeyEvent::ToggleInteractive], "without the Windows key it fires");
    }

    #[test]
    fn a_key_held_at_start_is_not_a_press() {
        let mut e = EdgeDetector::default();
        assert!(e.feed(state(true, true, false, true, false)).is_empty());
        assert!(e.feed(state(true, true, false, true, false)).is_empty());
        assert!(e.feed(state(true, true, false, false, false)).is_empty());
        assert_eq!(e.feed(state(true, true, false, true, false)), vec![HotkeyEvent::ToggleVisible]);
    }

    #[test]
    fn the_same_press_from_both_sources_counts_once() {
        use HotkeyEvent::{ToggleInteractive as F10, ToggleVisible as F9};
        use Source::{Message as Msg, Poll};
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut d = Dedupe::default();
        assert!(d.accept(F10, Msg, t0), "WM_HOTKEY");
        assert!(!d.accept(F10, Poll, ms(20)), "the poll saw the same press");
        assert!(d.accept(F9, Poll, ms(30)), "the other key is independent");
        assert!(!d.accept(F9, Msg, ms(35)), "and has its own pairing");
        // The poll first this time (either source may win the race).
        assert!(d.accept(F10, Poll, ms(1000)));
        assert!(!d.accept(F10, Msg, ms(1149)), "within the window");
        // The other source past the window: a new press.
        assert!(d.accept(F10, Msg, ms(2000)));
        assert!(d.accept(F10, Poll, ms(2150)), "150 ms later is no longer the same press");
    }

    #[test]
    fn two_quick_presses_are_two_presses() {
        use HotkeyEvent::ToggleInteractive as F10;
        use Source::{Message as Msg, Poll};
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Only the poll sees the keys (the game swallows WM_HOTKEY): every report counts.
        let mut d = Dedupe::default();
        assert!(d.accept(F10, Poll, t0));
        assert!(d.accept(F10, Poll, ms(60)), "open, then close right away");
        assert!(d.accept(F10, Poll, ms(90)));
        // Only WM_HOTKEY (no poll thread): the same.
        let mut d = Dedupe::default();
        assert!(d.accept(F10, Msg, t0));
        assert!(d.accept(F10, Msg, ms(50)));
        // Both sources, two presses 100 ms apart: two get through, one per press.
        let mut d = Dedupe::default();
        let got: Vec<bool> = [(Msg, 0), (Poll, 20), (Msg, 100), (Poll, 115)].iter().map(|&(s, t)| d.accept(F10, s, ms(t))).collect();
        assert_eq!(got, vec![true, false, true, false]);
        // The second press reported by the other source first: the pair of the first press is
        // already complete, so it is not mistaken for a duplicate.
        let mut d = Dedupe::default();
        let got: Vec<bool> = [(Poll, 0), (Msg, 5), (Msg, 100), (Poll, 110)].iter().map(|&(s, t)| d.accept(F10, s, ms(t))).collect();
        assert_eq!(got, vec![true, false, true, false]);
    }
}

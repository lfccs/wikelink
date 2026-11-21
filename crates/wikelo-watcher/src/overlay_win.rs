//! Window-manager glue for the overlay: finding its own window and the game's (and whether
//! the game's still answers), making the layered window visible, hiding it, covering the
//! game's monitor, and moving the keyboard focus in when the overlay opens and back out when
//! it closes.
//!
//! Only window-manager calls: on this process's own window, plus reading the title, the
//! owner thread / process id, the hung flag and the monitor of other top-level windows (to find the game's
//! window by its title), and `SetForegroundWindow` on the window that had the focus before
//! (to hand it back). No handle to another process is ever opened; nothing here reads, hooks
//! or injects into another process. No key press or mouse movement is ever synthesized: that
//! is also why the overlay never uses egui's `ViewportCommand::Focus`, which on Windows makes
//! winit send a fake Alt press to get around the foreground rules, and that press would land
//! in the game. The one input event this process sends is an empty one (a mouse event with
//! no movement and no button), and only when Windows refused to bring the overlay to the
//! front on Shift+Tab (see [`take_focus`]).
//!
//! Elsewhere every call is a no-op that answers "unknown".

/// A top-level window (`HWND`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Window(isize);

/// A monitor (`HMONITOR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Monitor(isize);

/// The window that had the focus before the overlay opened, with the thread and process that
/// owned it then: a window handle Windows reuses for another window after this one closed
/// does not get the focus by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct PrevFocus {
    window: Window,
    thread: u32,
    pid: u32,
}

/// What [`take_focus`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct FocusTaken {
    /// The overlay is the foreground window now.
    pub ok: bool,
    /// The window to hand the focus back to on close (when `ok`).
    pub prev: Option<PrevFocus>,
    /// What the window manager answered, for `overlay.log`: the call's result and error
    /// code, the window in front before and after, and this process's own window.
    pub detail: String,
}

/// The exact title of the game's window.
#[cfg_attr(not(windows), allow(dead_code))]
pub const GAME_TITLE: &str = "Star Citizen";

/// What the game's window looks like right now (see [`game_state`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum GameState {
    /// No visible top-level window titled [`GAME_TITLE`].
    Gone,
    /// There, but its thread has stopped taking messages (a frozen game, a crash dialog
    /// behind it): treated like a game that is gone.
    Hung,
    /// There, visible and answering.
    Usable,
}

/// The game's window the overlay was shown on, with the thread and process that owned it then
/// (see [`follow`]): while the overlay is on the game, the watch thread looks at this very
/// window, not at whichever window has the title now, so a game that crashed and came back
/// under a new window between two looks still counts as having gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Followed {
    window: Window,
    thread: u32,
    pid: u32,
}

/// Whether a window still is the game's window the overlay was shown on: owned now (`now`,
/// thread and process) by the same as then (`then`; a window that is gone has no owner, and
/// a handle Windows gave to another window since has another one), visible, and still
/// titled [`GAME_TITLE`].
#[cfg_attr(not(windows), allow(dead_code))]
pub fn still_the_game(then: (u32, u32), now: (u32, u32), visible: bool, title: &str) -> bool {
    then.0 != 0 && then == now && visible && title.trim() == GAME_TITLE
}

/// The rectangle the overlay window takes on a monitor, `[left, top, right, bottom]` in
/// physical pixels: the whole monitor plus one row past its bottom edge. Not exactly the
/// monitor: a topmost window that covers a whole monitor exactly is taken by the driver /
/// the compositor for an exclusive fullscreen surface, and an OpenGL window promoted like
/// that loses its transparency (the screen goes black behind the cards). Bigger rather than
/// smaller, so no strip of the game is left uncovered: a click on such a strip while the
/// overlay is open would reach the game. The extra row lands on whatever is below the
/// monitor (another monitor's top row, or nothing).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn cover_rect(monitor: [i32; 4]) -> [i32; 4] {
    let [left, top, right, bottom] = monitor;
    [left, top, right, bottom.saturating_add(1)]
}

#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
pub use other::*;

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{BOOL, CloseHandle, GetLastError, HANDLE, HWND, LPARAM, POINT, RECT, SetLastError};
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromWindow};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, INPUT, INPUT_MOUSE, LASTINPUTINFO, SendInput};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow,
        IsWindowVisible, LWA_ALPHA, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos, WS_EX_LAYERED,
    };

    use super::{FocusTaken, Followed, GAME_TITLE, GameState, Monitor, PrevFocus, Window};

    /// `HWND_TOPMOST`, `SWP_NOACTIVATE`, `SWP_NOOWNERZORDER`.
    const HWND_TOPMOST: HWND = -1;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_NOOWNERZORDER: u32 = 0x0200;

    /// Top-level windows of this process: the overlay's own, plus helpers such as the IME's.
    /// Found by process id, never by title, which another program could share.
    fn own_windows() -> Vec<HWND> {
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let mut pid = 0u32;
            // SAFETY: `hwnd` comes from EnumWindows; `pid` is a valid, writable u32.
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if pid == std::process::id() {
                // SAFETY: `lparam` is the address of `found` in `own_windows`, which outlives the
                // EnumWindows call and is not touched elsewhere while it runs (the callback runs on
                // the calling thread, one window at a time).
                let found = unsafe { &mut *(lparam as *mut Vec<HWND>) };
                found.push(hwnd);
            }
            // Keep enumerating.
            1
        }

        let mut found: Vec<HWND> = Vec::new();
        // SAFETY: `collect` has the WNDENUMPROC signature and uses `lparam` only as the
        // `&mut found` passed here, alive for the whole call.
        unsafe { EnumWindows(Some(collect), &mut found as *mut Vec<HWND> as LPARAM) };
        found
    }

    /// This process's top-level windows with `WS_EX_LAYERED`: the overlay window while it
    /// ignores the mouse (the IME's helper windows are not layered).
    fn own_layered_windows() -> Vec<HWND> {
        own_windows()
            .into_iter()
            .filter(|&hwnd| {
                // SAFETY: a window handle EnumWindows just listed; a window gone since only reads 0.
                let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
                ex & WS_EX_LAYERED != 0
            })
            .collect()
    }

    /// Whether `hwnd` belongs to this process.
    fn is_own(hwnd: HWND) -> bool {
        let mut pid = 0u32;
        // SAFETY: any window handle (a stale one only fails); `pid` is a valid, writable u32.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid == std::process::id()
    }

    /// The thread and process that own `hwnd` (`(0, 0)` for a window that is gone).
    fn owner(hwnd: HWND) -> (u32, u32) {
        let mut pid = 0u32;
        // SAFETY: any window handle (a stale one only returns 0); `pid` is a valid, writable u32.
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        (thread, pid)
    }

    /// The overlay's window. Only found while it ignores the mouse (it is layered then), which
    /// is how it starts; the caller keeps the answer.
    pub fn own_window() -> Option<Window> {
        own_layered_windows().first().map(|&h| Window(h))
    }

    /// A window's title, enough of it to compare with [`GAME_TITLE`]: room for a few more
    /// characters than that, and a longer title comes back cut at 31 characters, which never
    /// equals the 12 of the game's. Empty for a window that is gone. For a window of another
    /// process Windows answers from its own copy of the title, without sending it a message.
    fn short_title(hwnd: HWND) -> String {
        let mut buf = [0u16; 32];
        // SAFETY: `buf` is a writable buffer of `buf.len()` u16; the call writes at most that
        // many, the terminating NUL included, and returns how many characters it copied (0 for
        // a stale handle).
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    /// The game's window: a visible top-level window of another process whose title is
    /// exactly [`GAME_TITLE`]. Only titles are read (for a window of another process Windows
    /// answers from its own copy of the title, without sending the window a message).
    pub fn game_window() -> Option<Window> {
        unsafe extern "system" fn find(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: a window handle EnumWindows passes; a plain query.
            if unsafe { IsWindowVisible(hwnd) } == 0 || is_own(hwnd) {
                return 1;
            }
            // The game writes its title with a space at the end.
            if short_title(hwnd).trim() == GAME_TITLE {
                // SAFETY: `lparam` is the address of `found` in `game_window`, alive for the whole
                // EnumWindows call and not touched elsewhere while it runs.
                unsafe { *(lparam as *mut HWND) = hwnd };
                // Found: stop enumerating.
                return 0;
            }
            1
        }

        let mut found: HWND = 0;
        // SAFETY: `find` has the WNDENUMPROC signature and uses `lparam` only as the `&mut found`
        // passed here, alive for the whole call. EnumWindows "fails" when the callback stops it.
        unsafe { EnumWindows(Some(find), &mut found as *mut HWND as LPARAM) };
        (found != 0).then_some(Window(found))
    }

    /// The game's window and whether the overlay may sit on it: there, visible ([`game_window`]
    /// only finds visible ones) and not hung. `IsHungAppWindow` is a window-manager query (when
    /// the window's thread last took a message); it sends the window nothing and opens nothing.
    pub fn game_state() -> (GameState, Option<Window>) {
        let Some(w) = game_window() else { return (GameState::Gone, None) };
        // SAFETY: a window handle EnumWindows just listed; a stale one only answers "not hung".
        if unsafe { IsHungAppWindow(w.0) } != 0 {
            return (GameState::Hung, Some(w));
        }
        (GameState::Usable, Some(w))
    }

    /// Start following the game's window `w` (see [`Followed`]): who owns it now.
    pub fn follow(w: Window) -> Followed {
        let (thread, pid) = owner(w.0);
        Followed { window: w, thread, pid }
    }

    /// What became of the game's window the overlay was shown on: [`GameState::Gone`] once
    /// it is no longer that window ([`super::still_the_game`]: closed, hidden, renamed, or its
    /// handle now another window's), else hung or usable. Only window-manager queries on that
    /// one window (owner, visibility, title, hung flag); none sends it a message.
    pub fn followed_state(f: Followed) -> GameState {
        let h = f.window.0;
        // The owner first, and nothing more is read of a window with another one: a handle
        // Windows gave since to one of this process's own windows would have its title asked
        // with a message to this process's UI thread, which may be the stuck one.
        let now = owner(h);
        // SAFETY: a window handle that may have gone since; a plain query (0 for a stale one).
        let visible = now == (f.thread, f.pid) && unsafe { IsWindowVisible(h) } != 0;
        let title = if visible { short_title(h) } else { String::new() };
        if !super::still_the_game((f.thread, f.pid), now, visible, &title) {
            return GameState::Gone;
        }
        // SAFETY: as above; a stale handle only answers "not hung".
        if unsafe { IsHungAppWindow(h) } != 0 { GameState::Hung } else { GameState::Usable }
    }

    /// The monitor a window is on (the one it overlaps most; the nearest one when it is on none).
    pub fn window_monitor(w: Window) -> Option<Monitor> {
        // SAFETY: any window handle, a stale one included, with a plain flag; the monitor handle
        // needs no release.
        let m = unsafe { MonitorFromWindow(w.0, MONITOR_DEFAULTTONEAREST) };
        (m != 0).then_some(Monitor(m))
    }

    /// A click-through window is `WS_EX_LAYERED` (winit sets it for the mouse passthrough), and
    /// Windows draws a layered window only once it has layer attributes, which winit never sets:
    /// without this the closed overlay is there but invisible.
    pub fn show_layered() {
        for hwnd in own_layered_windows() {
            // SAFETY: a layered window of this process EnumWindows just listed; plain values otherwise.
            unsafe { SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA) };
        }
    }

    /// The monitor of the window in front right now; the primary one when there is none.
    pub fn foreground_monitor() -> Option<Monitor> {
        // SAFETY: no arguments; the handle is only passed on.
        let fg = unsafe { GetForegroundWindow() };
        // SAFETY: any window handle, or 0, with a plain flag; the monitor handle needs no release.
        let m = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY) };
        (m != 0).then_some(Monitor(m))
    }

    /// `[left, top, right, bottom]` of `monitor` in physical pixels (asked again each time: the
    /// answer depends on this process's DPI awareness, which winit sets after start-up), or of
    /// the primary monitor when that one is gone.
    pub fn monitor_rect(monitor: Option<Monitor>) -> Option<[i32; 4]> {
        let read = |h: isize| -> Option<[i32; 4]> {
            // SAFETY: an all-zero MONITORINFO is a valid value (integers and RECTs).
            let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            // SAFETY: `info` is a valid, writable MONITORINFO with its size set; a stale monitor
            // handle only makes the call fail.
            if unsafe { GetMonitorInfoW(h, &mut info) } == 0 {
                return None;
            }
            let r = info.rcMonitor;
            Some([r.left, r.top, r.right, r.bottom]).filter(|r| r[2] > r[0] && r[3] > r[1])
        };
        monitor.and_then(|m| read(m.0)).or_else(|| {
            // SAFETY: plain values in; the monitor handle out needs no release.
            let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
            if primary == 0 { None } else { read(primary) }
        })
    }

    /// `[left, top, right, bottom]` of a window in physical pixels.
    pub fn window_rect(w: Window) -> Option<[i32; 4]> {
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: a window of this process; `r` is a valid, writable RECT.
        if unsafe { GetWindowRect(w.0, &mut r) } == 0 {
            return None;
        }
        Some([r.left, r.top, r.right, r.bottom])
    }

    /// Move and size the overlay window to `r` (physical pixels), topmost, without
    /// activating it.
    pub fn cover(w: Window, r: [i32; 4]) {
        // SAFETY: this process's own window; plain values. NOACTIVATE: the focus stays put.
        unsafe { SetWindowPos(w.0, HWND_TOPMOST, r[0], r[1], r[2] - r[0], r[3] - r[1], SWP_NOACTIVATE | SWP_NOOWNERZORDER) };
    }

    /// Bring the overlay to the foreground (it opened) and say whether it got there, with the
    /// window that had the focus, to hand it back on close. Windows lets a background process
    /// do this only in some cases (it got the last input, e.g. the WM_HOTKEY). After a refusal
    /// one empty input event is sent ([`empty_input`]) and the request made once more; a
    /// second refusal is final: the caller stays closed and says so. Nothing is done to the other window's
    /// thread to force the switch (no AttachThreadInput: the game runs under an anti-cheat).
    /// Without its own window (`None`) nothing is tried and the answer is "not in front".
    pub fn take_focus(w: Option<Window>) -> FocusTaken {
        let Some(w) = w else { return FocusTaken { ok: false, prev: None, detail: "own window not found".into() } };
        // SAFETY: no arguments.
        let before = unsafe { GetForegroundWindow() };
        let prev = (before != 0 && before != w.0 && !is_own(before)).then(|| {
            let (thread, pid) = owner(before);
            PrevFocus { window: Window(before), thread, pid }
        });
        // Described before the call: what the window manager saw when it decided.
        let (front, own, idle) = (describe(before), describe_own(w), idle_ms());
        // SAFETY: plain integer; clears this thread's error code so the one read below is the call's.
        unsafe { SetLastError(0) };
        // SAFETY: this process's own window.
        let answer = unsafe { SetForegroundWindow(w.0) };
        // SAFETY: no arguments; this thread's error code.
        let err = unsafe { GetLastError() };
        let mut ok = is_foreground(w);
        // Refused: the player's key went to the window in front, so this process did not get
        // the last input. One empty input event of its own makes it the last one, and the
        // same request is made once more.
        let mut second = String::new();
        if !ok {
            let sent = empty_input();
            // SAFETY: as above.
            unsafe { SetLastError(0) };
            // SAFETY: this process's own window.
            let answer = unsafe { SetForegroundWindow(w.0) };
            // SAFETY: no arguments; this thread's error code.
            let err = unsafe { GetLastError() };
            ok = is_foreground(w);
            second = format!(" | after an empty input (sent={sent}): SetForegroundWindow={answer} error={err}");
        }
        // SAFETY: no arguments.
        let after = unsafe { GetForegroundWindow() };
        let idle = idle.map_or("?".to_string(), |ms| format!("{ms}ms"));
        let detail = format!("SetForegroundWindow={answer} error={err} last-input={idle} ago{second} | in front before: {front} | after: {} | own: {own}", describe(after));
        FocusTaken { ok, prev, detail }
    }

    /// One empty input event from this process: a mouse event with no movement, no button
    /// and no wheel. It moves and presses nothing; it only makes this process the one the
    /// last input came from, which is what Windows asks of a background process before it
    /// may bring its own window to the front. Says whether the system took the event.
    fn empty_input() -> bool {
        // SAFETY: INPUT is plain data and all zeroes is a valid value of it: type 0 is
        // INPUT_MOUSE, with zero movement and no flags.
        let input: INPUT = unsafe { std::mem::zeroed() };
        debug_assert_eq!(input.r#type, INPUT_MOUSE);
        // SAFETY: one valid INPUT, with its count and size.
        unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
    }

    /// A window for `overlay.log`: handle, owner process and thread, class and the first
    /// characters of the title, with a mark when it is the game's or this process's own.
    /// Window-manager queries only; nothing is sent to the window.
    fn describe(hwnd: HWND) -> String {
        if hwnd == 0 {
            return "none".into();
        }
        let (thread, pid) = owner(hwnd);
        if pid == std::process::id() {
            return format!("{hwnd:#x} (this overlay)");
        }
        let mut class = [0u16; 64];
        // SAFETY: `class` is a writable buffer of `class.len()` u16; the call writes at most that
        // many and returns how many characters it copied (0 for a stale handle).
        let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0).min(class.len())]);
        let title = short_title(hwnd);
        let game = if title.trim() == GAME_TITLE { " (the game)" } else { "" };
        format!("{hwnd:#x} pid={pid} thread={thread} class={class:?} title={title:?}{game}")
    }

    /// This process's own window for `overlay.log`: its extended style (click-through,
    /// no-activate and layered are bits of it), and whether it is visible and minimized.
    fn describe_own(w: Window) -> String {
        // SAFETY: this process's own window; plain queries.
        let (ex, visible, iconic) = unsafe { (GetWindowLongW(w.0, GWL_EXSTYLE) as u32, IsWindowVisible(w.0) != 0, IsIconic(w.0) != 0) };
        format!("{:#x} exstyle={ex:#010x} visible={visible} minimized={iconic} rect={:?}", w.0, window_rect(w))
    }

    /// The window in front right now, for `overlay.log`.
    pub fn describe_foreground() -> String {
        // SAFETY: no arguments.
        describe(unsafe { GetForegroundWindow() })
    }

    /// A window, for `overlay.log`.
    pub fn describe_window(w: Option<Window>) -> String {
        w.map_or("none".into(), |w| describe(w.0))
    }

    /// How long ago the system last saw any input (keyboard or mouse), in milliseconds.
    fn idle_ms() -> Option<u32> {
        let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        // SAFETY: `info` is a valid, writable LASTINPUTINFO with its size set.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return None;
        }
        // SAFETY: no arguments. Both are the same wrapping millisecond counter.
        Some(unsafe { GetTickCount() }.wrapping_sub(info.dwTime))
    }

    /// Whether this process runs elevated ("as administrator"), read from its own token.
    pub fn elevated() -> Option<bool> {
        let mut token: HANDLE = 0;
        // SAFETY: this process's own pseudo-handle; `token` is a valid, writable HANDLE.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        // SAFETY: `info` is a writable TOKEN_ELEVATION of the size passed; `len` a writable u32.
        let ok = unsafe { GetTokenInformation(token, TokenElevation, &mut info as *mut TOKEN_ELEVATION as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len) };
        // SAFETY: the token handle opened above, closed once.
        unsafe { CloseHandle(token) };
        (ok != 0).then_some(info.TokenIsElevated != 0)
    }

    /// Give the focus back to the window that had it before the overlay opened, if that is
    /// still the same window: alive, and owned by the same thread of the same process.
    pub fn give_focus(prev: PrevFocus) {
        // SAFETY: a window handle that may have gone since; IsWindow only reads.
        if unsafe { IsWindow(prev.window.0) } == 0 {
            return;
        }
        if owner(prev.window.0) != (prev.thread, prev.pid) {
            return;
        }
        // SAFETY: a live window of the same owner as before; a refusal only returns 0.
        unsafe { SetForegroundWindow(prev.window.0) };
    }

    pub fn is_foreground(w: Window) -> bool {
        // SAFETY: no arguments.
        unsafe { GetForegroundWindow() == w.0 }
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
mod other {
    use super::{FocusTaken, Followed, GameState, Monitor, PrevFocus, Window};

    pub fn own_window() -> Option<Window> {
        None
    }

    pub fn game_window() -> Option<Window> {
        None
    }

    pub fn game_state() -> (GameState, Option<Window>) {
        (GameState::Gone, None)
    }

    pub fn follow(w: Window) -> Followed {
        Followed { window: w, thread: 0, pid: 0 }
    }

    pub fn followed_state(_f: Followed) -> GameState {
        GameState::Gone
    }

    pub fn window_monitor(_w: Window) -> Option<Monitor> {
        None
    }

    pub fn show_layered() {}

    pub fn foreground_monitor() -> Option<Monitor> {
        None
    }

    pub fn monitor_rect(_monitor: Option<Monitor>) -> Option<[i32; 4]> {
        None
    }

    pub fn window_rect(_w: Window) -> Option<[i32; 4]> {
        None
    }

    pub fn cover(_w: Window, _r: [i32; 4]) {}

    /// The platform moves the focus itself here: take it as done.
    pub fn take_focus(_w: Option<Window>) -> FocusTaken {
        FocusTaken { ok: true, prev: None, detail: String::new() }
    }

    pub fn describe_foreground() -> String {
        String::new()
    }

    pub fn describe_window(_w: Option<Window>) -> String {
        String::new()
    }

    pub fn elevated() -> Option<bool> {
        None
    }

    pub fn give_focus(_prev: PrevFocus) {}

    pub fn is_foreground(_w: Window) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_covers_the_monitor_and_one_row_past_its_bottom() {
        assert_eq!(cover_rect([0, 0, 1920, 1080]), [0, 0, 1920, 1081]);
        assert_eq!(cover_rect([-2560, -200, 0, 1240]), [-2560, -200, 0, 1241], "a monitor left of and above the primary");
        assert_eq!(cover_rect([0, 0, 10, 1]), [0, 0, 10, 2], "never empty");
        let [l, t, r, b] = [1920, 0, 4480, 1440];
        let got = cover_rect([l, t, r, b]);
        assert!(got[0] <= l && got[1] <= t && got[2] >= r && got[3] >= b, "no strip of the monitor left out: {got:?}");
        assert_ne!(got, [l, t, r, b], "never exactly the monitor");
    }

    #[test]
    fn the_followed_window_stays_the_game_only_while_it_is_the_same_window() {
        let then = (1200, 4400);
        assert!(still_the_game(then, then, true, "Star Citizen "), "the game writes a trailing space");
        assert!(still_the_game(then, then, true, "Star Citizen"));
        assert!(!still_the_game(then, (0, 0), false, ""), "closed: the handle has no owner any more");
        assert!(!still_the_game(then, (1300, 5100), true, "Star Citizen "), "a new game window got the old handle");
        assert!(!still_the_game(then, (1200, 4400), false, "Star Citizen "), "hidden");
        assert!(!still_the_game(then, then, true, "Star Citizen Launcher"), "renamed");
        assert!(!still_the_game((0, 0), (0, 0), true, "Star Citizen"), "never had an owner: not a window to follow");
    }
}

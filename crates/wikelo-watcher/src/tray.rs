//! The background watcher's icon in the notification area (Windows): it shows Wikelink is
//! running and how it is doing (the tooltip, from the status file the watcher keeps), and
//! leads to it:
//! - double-click: the settings window (this executable without arguments);
//! - right-click: "Open Wikelink", "Open the site" (wikelist.net, or a developer server),
//!   "Show the overlay" (while one runs: shows it again after "Hide" or Alt+Shift+F9,
//!   through `overlay.show`), "Overlay" (checked while one runs: starts it, or asks it to
//!   close through `overlay.stop`, the way the settings window does), "Quit" (closes the
//!   overlay too, then stops the watcher through its stop file, like `wikelink stop`).
//!
//! A thread of its own owns a window that is never shown (a plain top-level one: a
//! message-only window would miss the `TaskbarCreated` broadcast, which says Explorer
//! restarted and the icon must be added again) and runs its message loop. Dropping [`Tray`]
//! closes that window, which removes the icon. Failing to set any of it up is logged and the
//! watcher goes on without an icon. Elsewhere than Windows there is no icon at all.
#![cfg_attr(not(windows), allow(dead_code))]

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::agent::{self, DaemonStatus};
use crate::{tr, trf};

/// One timestamped line in the watcher's log.
macro_rules! note {
    ($($arg:tt)*) => { $crate::agent::emit(format_args!($($arg)*)) };
}

/// The tooltip's first words (the program's name: never translated).
pub const NAME: &str = "Wikelink";
/// Most UTF-16 units of a tooltip (`szTip` holds 128 with the closing NUL).
const TIP_MAX: usize = 127;
/// Most characters of an error in the tooltip.
const ERROR_MAX: usize = 60;
/// How often the tooltip is brought up to date (and a missing icon added again).
const REFRESH_EVERY_MS: u32 = 2000;

/// The short state in the tooltip: what the status file says.
pub fn state_text(s: Option<&DaemonStatus>) -> String {
    let Some(s) = s else { return tr!("starting", "iniciando").into() };
    if s.paused {
        return tr!("paused", "pausado").into();
    }
    if let Some(e) = &s.last_error {
        return shorten(e, ERROR_MAX);
    }
    if s.last_ok.is_some() { tr!("connected", "conectado").into() } else { tr!("connecting", "conectando").into() }
}

/// `Wikelink · connected`, short enough for the notification area.
pub fn tooltip(s: Option<&DaemonStatus>) -> String {
    fit_utf16(&format!("{NAME} · {}", state_text(s)), TIP_MAX)
}

/// The first line of `s`, trimmed, at most `max` characters (the last one `…` when cut).
fn shorten(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect::<String>().trim_end().to_string();
    out.push('…');
    out
}

/// `s` cut to at most `max` UTF-16 units, never inside a character.
fn fit_utf16(s: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The site's address (`agent::server_address`: wikelist.net, or a developer's local server),
/// when it may be handed to the browser (see `agent::openable_url`); else none, and no menu item.
pub fn site_url() -> Option<String> {
    agent::openable_url(agent::server_address(), agent::Link::Site)
}

/// What a menu item (or the double-click) does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    OpenSettings,
    OpenSite,
    ShowOverlay,
    Overlay,
    Quit,
}

impl Command {
    const ALL: [Command; 5] = [Command::OpenSettings, Command::OpenSite, Command::ShowOverlay, Command::Overlay, Command::Quit];

    /// The menu item's id; never 0, which `TrackPopupMenu` answers for "nothing chosen".
    pub fn id(self) -> usize {
        match self {
            Command::OpenSettings => 1,
            Command::OpenSite => 2,
            Command::Overlay => 3,
            Command::Quit => 4,
            Command::ShowOverlay => 5,
        }
    }

    pub fn from_id(id: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// One line of the right-click menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub command: Command,
    pub label: &'static str,
    pub checked: bool,
    pub enabled: bool,
    /// Drawn in bold: what the double-click does.
    pub default: bool,
    pub separator_before: bool,
}

/// The right-click menu, for an overlay that runs or not and a site address that is known or not.
pub fn menu(overlay_on: bool, has_site: bool) -> Vec<MenuItem> {
    let item = |command: Command, label: &'static str| MenuItem { command, label, checked: false, enabled: true, default: false, separator_before: false };
    vec![
        MenuItem { default: true, ..item(Command::OpenSettings, tr!("Open Wikelink", "Abrir o Wikelink")) },
        MenuItem { enabled: has_site, ..item(Command::OpenSite, tr!("Open the site", "Abrir o site")) },
        MenuItem { enabled: overlay_on, ..item(Command::ShowOverlay, tr!("Show the overlay", "Mostrar o overlay")) },
        MenuItem { checked: overlay_on, ..item(Command::Overlay, "Overlay") },
        MenuItem { separator_before: true, ..item(Command::Quit, tr!("Quit", "Sair")) },
    ]
}

/// Least time between two settings windows started from the icon: a second double-click (or
/// the menu item right after one) while the first window is still coming up does nothing.
const SETTINGS_DEBOUNCE: Duration = Duration::from_secs(3);

/// Whether the icon may start a settings window now, the last one having been started at
/// `last` (`None`: never).
pub fn settings_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= SETTINGS_DEBOUNCE)
}

thread_local! {
    /// When the icon last started a settings window (only the icon's thread opens it).
    static SETTINGS_STARTED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Carry out a choice. What waits (closing the overlay takes up to a few seconds) runs on a
/// thread of its own, so the icon keeps answering.
fn perform(c: Command) {
    match c {
        Command::OpenSettings => {
            let now = Instant::now();
            if !settings_due(SETTINGS_STARTED.get(), now) {
                return;
            }
            match agent::open_settings_window() {
                Ok(()) => SETTINGS_STARTED.set(Some(now)),
                Err(e) => note!("{}", trf!("could not open Wikelink: {e:#}", "não deu para abrir o Wikelink: {e:#}")),
            }
        }
        Command::OpenSite => {
            if let Some(url) = site_url()
                && let Err(e) = open::that_detached(&url)
            {
                note!("{}", trf!("could not open {url}: {e}", "não deu para abrir {url}: {e}"));
            }
        }
        Command::ShowOverlay => {
            if let Err(e) = agent::request_overlay_show() {
                note!("{}", trf!("could not show the overlay: {e}", "não deu para mostrar o overlay: {e}"));
            }
        }
        Command::Overlay => in_background(|| {
            let result = if agent::overlay_running() { agent::close_overlay().map(|_| ()) } else { agent::launch_overlay() };
            if let Err(e) = result {
                note!("overlay: {e:#}");
            }
        }),
        // The icon stays until the watcher really ends (dropping `Tray` removes it): when the
        // request cannot be made, the failure is logged and "Quit" can be chosen again.
        Command::Quit => in_background(|| {
            note!("{}", tr!("quitting from the tray icon", "saindo pelo ícone da bandeja"));
            if let Err(e) = agent::close_overlay() {
                note!("{e:#}");
            }
            // The same request `wikelink stop` makes; the watcher sees it within a second.
            if let Err(e) = agent::request_stop() {
                note!("{}", trf!("could not ask Wikelink to stop: {e}", "não deu para pedir a parada do Wikelink: {e}"));
            }
        }),
    }
}

fn in_background(work: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name("tray-action".into()).spawn(work) {
        note!("{}", trf!("tray icon: {e}", "ícone da bandeja: {e}"));
    }
}

/// The icon; dropping it removes it from the notification area.
pub struct Tray {
    #[cfg(windows)]
    hwnd: windows_sys::Win32::Foundation::HWND,
    /// Says the icon's thread has ended.
    #[cfg(windows)]
    done: std::sync::mpsc::Receiver<()>,
    #[cfg(windows)]
    join: Option<std::thread::JoinHandle<()>>,
}

/// How long [`start`] waits for the icon's thread to set up its window.
#[cfg(windows)]
const READY_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long dropping [`Tray`] waits for the icon's thread to end.
#[cfg(windows)]
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Put the icon in the notification area; `None` (logged) when that is not possible.
#[cfg(windows)]
pub fn start() -> Option<Tray> {
    use std::sync::mpsc::channel;
    let (ready_tx, ready_rx) = channel::<Result<windows_sys::Win32::Foundation::HWND, String>>();
    let (done_tx, done_rx) = channel::<()>();
    let spawned = std::thread::Builder::new().name("tray".into()).spawn(move || {
        win::run(&ready_tx);
        let _ = done_tx.send(());
    });
    let join = match spawned {
        Ok(j) => j,
        Err(e) => {
            note!("{}", trf!("no tray icon: {e}", "sem ícone na bandeja: {e}"));
            return None;
        }
    };
    match ready_rx.recv_timeout(READY_WAIT) {
        Ok(Ok(hwnd)) => Some(Tray { hwnd, done: done_rx, join: Some(join) }),
        Ok(Err(e)) => {
            note!("{}", trf!("no tray icon: {e}", "sem ícone na bandeja: {e}"));
            None
        }
        Err(_) => {
            note!("{}", tr!("no tray icon: the icon's thread did not answer", "sem ícone na bandeja: a thread do ícone não respondeu"));
            None
        }
    }
}

#[cfg(not(windows))]
pub fn start() -> Option<Tray> {
    None
}

#[cfg(windows)]
impl Drop for Tray {
    fn drop(&mut self) {
        win::close(self.hwnd);
        // The thread ends right after removing the icon; a stuck one is not waited for.
        if self.done.recv_timeout(CLOSE_WAIT).is_ok()
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::cell::{Cell, RefCell};
    use std::sync::mpsc::Sender;

    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HICON, IDI_APPLICATION, KillTimer,
        LoadIconW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    };

    use super::{Command, NAME, REFRESH_EVERY_MS};

    /// The message the shell sends the window about the icon (clicks).
    const WM_TRAY: u32 = WM_APP + 1;
    const ICON_ID: u32 = 1;
    const TIMER_ID: usize = 1;
    /// Not shared with anything: only this window uses it.
    const CLASS: &str = "WikelinkTray";

    // The window procedure runs on the icon's thread only, so its state is that thread's.
    // Only `Cell`s and short `RefCell` borrows: the procedure is re-entered while the menu is
    // up (a timer tick), and no borrow is ever held across a call into Windows.
    thread_local! {
        static WINDOW: Cell<HWND> = const { Cell::new(0) };
        static ICON: Cell<HICON> = const { Cell::new(0) };
        /// The icon is in the notification area now.
        static ADDED: Cell<bool> = const { Cell::new(false) };
        static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
        static TIP: RefCell<String> = const { RefCell::new(String::new()) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The icon's description for `Shell_NotifyIconW`.
    fn icon_data(tip: &str) -> NOTIFYICONDATAW {
        // SAFETY: an all-zero NOTIFYICONDATAW is a valid value (integers, handles, UTF-16
        // arrays, a GUID and a union of integers).
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = WINDOW.get();
        d.uID = ICON_ID;
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = ICON.get();
        // The last unit stays 0: the closing NUL.
        let room = d.szTip.len() - 1;
        for (slot, unit) in d.szTip.iter_mut().take(room).zip(tip.encode_utf16()) {
            *slot = unit;
        }
        d
    }

    fn notify(message: u32, tip: &str) -> bool {
        let d = icon_data(tip);
        // SAFETY: `d` is a filled-in NOTIFYICONDATAW with its size set, alive for the call; the
        // shell copies what it keeps.
        unsafe { Shell_NotifyIconW(message, &d) != 0 }
    }

    /// Put the icon in (again). Explorer not running yet (a sign-in still starting) makes it
    /// fail: the timer and `TaskbarCreated` try again.
    fn add() {
        let tip = TIP.with_borrow(|t| t.clone());
        // An icon still there under the same id (Explorer kept it) takes a modify instead.
        let ok = notify(NIM_ADD, &tip) || notify(NIM_MODIFY, &tip);
        ADDED.set(ok);
    }

    fn remove() {
        if ADDED.replace(false) {
            notify(NIM_DELETE, "");
        }
    }

    /// Timer tick: the tooltip from the status file, and the icon back if it is missing.
    fn refresh() {
        // A status file caught half-written reads as nothing: the last tooltip stays then.
        let tip = crate::agent::read_status().map(|s| super::tooltip(Some(&s)));
        let changed = tip.filter(|tip| TIP.with_borrow(|t| t != tip));
        if let Some(tip) = &changed {
            TIP.set(tip.clone());
        }
        if !ADDED.get() {
            add();
        } else if let Some(tip) = &changed {
            notify(NIM_MODIFY, tip);
        }
    }

    fn choose(c: Command) {
        super::perform(c);
    }

    /// The right-click menu at the mouse, and what was chosen in it.
    fn show_menu(hwnd: HWND) {
        let items = super::menu(crate::agent::overlay_running(), super::site_url().is_some());
        // SAFETY: no arguments; the menu is destroyed below.
        let menu = unsafe { CreatePopupMenu() };
        if menu == 0 {
            return;
        }
        let labels: Vec<Vec<u16>> = items.iter().map(|i| wide(i.label)).collect();
        for (item, label) in items.iter().zip(&labels) {
            if item.separator_before {
                // SAFETY: the live menu created above; a separator takes no text.
                unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
            }
            let mut flags = MF_STRING;
            if item.checked {
                flags |= MF_CHECKED;
            }
            if !item.enabled {
                flags |= MF_GRAYED;
            }
            // SAFETY: the live menu created above; `label` is NUL-terminated UTF-16 that outlives
            // the call (the menu keeps its own copy).
            unsafe { AppendMenuW(menu, flags, item.command.id(), label.as_ptr()) };
            if item.default {
                // SAFETY: the live menu created above and an item id just added (by id, not position).
                unsafe { SetMenuDefaultItem(menu, item.command.id() as u32, 0) };
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: `pt` is a valid, writable POINT.
        unsafe { GetCursorPos(&mut pt) };
        // SAFETY: this thread's own window and the live menu created above, destroyed here once
        // the menu is over. The window goes in front first, or the menu would not close on a
        // click elsewhere, and the WM_NULL after it lets the menu end cleanly (both from the
        // TrackPopupMenu documentation). TPM_RETURNCMD: the chosen id comes back as the result.
        let chosen = unsafe {
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            chosen
        };
        if let Some(c) = usize::try_from(chosen).ok().and_then(Command::from_id) {
            choose(c);
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_TRAY => {
                // Without NIM_SETVERSION the low word of lParam is the mouse message.
                match (lparam as u32) & 0xFFFF {
                    WM_LBUTTONDBLCLK => choose(Command::OpenSettings),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                0
            }
            WM_TIMER => {
                refresh();
                0
            }
            WM_CLOSE => {
                // SAFETY: this thread's own window.
                unsafe { DestroyWindow(hwnd) };
                0
            }
            WM_DESTROY => {
                remove();
                // SAFETY: the timer `run` set on this window; then the end of this thread's loop.
                unsafe {
                    KillTimer(hwnd, TIMER_ID);
                    PostQuitMessage(0);
                }
                0
            }
            m if m != 0 && m == TASKBAR_CREATED.get() => {
                // Explorer restarted: every icon is gone and is added again.
                ADDED.set(false);
                add();
                0
            }
            // SAFETY: the arguments this window procedure was called with.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The icon of the settings window (drawn in code: the executable carries no icon
    /// resource) as an HICON, else the stock application icon; `true` when it is ours to
    /// destroy.
    fn make_icon() -> (HICON, bool) {
        let img = crate::gui::icon();
        let (w, h) = (img.width as usize, img.height as usize);
        if w > 0 && h > 0 && img.rgba.len() == w * h * 4 {
            let mut bgra = img.rgba.clone();
            for px in bgra.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            // 1 bit per pixel, rows padded to 16 bits; set = transparent (for a 32-bit icon the
            // alpha channel decides anyway).
            let stride = w.div_ceil(16) * 2;
            let mut mask = vec![0u8; stride * h];
            for (i, px) in img.rgba.as_chunks::<4>().0.iter().enumerate() {
                if px[3] == 0 {
                    let (x, y) = (i % w, i / w);
                    mask[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
            // SAFETY: `mask` holds `h` rows of `stride` bytes (1 bit per pixel, WORD-aligned rows)
            // and `bgra` `w * h` 32-bit pixels, both alive for the call, which copies them.
            let icon = unsafe { CreateIcon(0, w as i32, h as i32, 1, 32, mask.as_ptr(), bgra.as_ptr()) };
            if icon != 0 {
                return (icon, true);
            }
        }
        // SAFETY: a stock icon (no module); shared by the system, never destroyed.
        (unsafe { LoadIconW(0, IDI_APPLICATION) }, false)
    }

    /// The icon's thread: its window, the icon, the message loop until the window closes.
    pub(super) fn run(ready: &Sender<Result<HWND, String>>) {
        let class = wide(CLASS);
        let title = wide(NAME);
        // SAFETY: null asks for this executable's own module; nothing to release.
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        // SAFETY: an all-zero WNDCLASSW is a valid value (no procedure, null pointers, zero
        // handles); the fields that matter are set right below.
        let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        // SAFETY: `wc` is filled in and `class` outlives the call (the class keeps a copy).
        if unsafe { RegisterClassW(&wc) } == 0 {
            // SAFETY: no arguments; read right after the failed call on the same thread.
            let err = unsafe { GetLastError() };
            let _ = ready.send(Err(crate::trf!("RegisterClassW failed (error {err})", "RegisterClassW falhou (erro {err})")));
            return;
        }
        // SAFETY: the class registered above; `class` and `title` outlive the call; no styles,
        // no parent, no menu, no creation data. The window is never shown.
        let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, 0, 0, instance, std::ptr::null()) };
        if hwnd == 0 {
            // SAFETY: no arguments; read right after the failed call on the same thread.
            let err = unsafe { GetLastError() };
            let _ = ready.send(Err(crate::trf!("CreateWindowExW failed (error {err})", "CreateWindowExW falhou (erro {err})")));
            return;
        }
        WINDOW.set(hwnd);
        let created = wide("TaskbarCreated");
        // SAFETY: `created` is NUL-terminated UTF-16 that outlives the call.
        TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(created.as_ptr()) });
        let (icon, owned) = make_icon();
        ICON.set(icon);
        TIP.set(super::tooltip(crate::agent::read_status().as_ref()));
        add();
        if !ADDED.get() {
            let secs = REFRESH_EVERY_MS / 1000;
            note!("{}", crate::trf!("tray icon not available yet; trying again every {secs} s", "ícone na bandeja ainda indisponível; tento de novo a cada {secs} s"));
        }
        // SAFETY: this thread's own window; no callback (WM_TIMER arrives through the loop below).
        unsafe { SetTimer(hwnd, TIMER_ID, REFRESH_EVERY_MS, None) };
        let _ = ready.send(Ok(hwnd));

        // SAFETY: an all-zero MSG is a valid value (plain integers and a POINT).
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: `msg` is a valid, writable MSG; every window of this thread, and its thread messages.
            let got = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
            // 0 = WM_QUIT (posted on WM_DESTROY), -1 = error: either way the loop is over.
            if got <= 0 {
                break;
            }
            // SAFETY: the message GetMessageW just filled in.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // After an error of the loop the window may still be there: the icon goes regardless.
        remove();
        if owned {
            // SAFETY: the icon `make_icon` created for this thread; the notification area no
            // longer uses it (removed above).
            unsafe { DestroyIcon(icon) };
        }
    }

    /// Ask the icon's window to close (from any thread); it removes the icon and ends the loop.
    pub(super) fn close(hwnd: HWND) {
        // SAFETY: posting carries no pointers; a window already gone only makes the call fail.
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(paused: bool, last_ok: bool, error: Option<&str>) -> DaemonStatus {
        DaemonStatus { paused, last_ok: last_ok.then(|| "2026-11-16T12:00:00Z".to_string()), last_error: error.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn the_tooltip_says_how_the_watcher_is_doing() {
        assert_eq!(tooltip(Some(&status(false, true, None))), "Wikelink · conectado");
        assert_eq!(tooltip(Some(&status(false, false, None))), "Wikelink · conectando");
        assert_eq!(tooltip(Some(&status(true, true, Some("x")))), "Wikelink · pausado", "paused wins over an error");
        assert_eq!(tooltip(Some(&status(false, true, Some("sessão expirada: entre de novo")))), "Wikelink · sessão expirada: entre de novo");
        assert_eq!(tooltip(None), "Wikelink · iniciando", "no status file yet");
    }

    #[test]
    fn long_errors_are_cut_to_one_short_line() {
        let long = format!("sem contato com o servidor ({})\nsegunda linha", "x".repeat(200));
        let tip = tooltip(Some(&status(false, true, Some(&long))));
        assert!(tip.starts_with("Wikelink · sem contato com o servidor ("), "{tip}");
        assert!(tip.ends_with('…') && !tip.contains("segunda"), "{tip}");
        assert_eq!(state_text(Some(&status(false, true, Some(&long)))).chars().count(), ERROR_MAX);
        assert_eq!(shorten("  curto  ", 10), "curto");
        assert_eq!(shorten("abcdef", 4), "abc…");
    }

    #[test]
    fn the_tooltip_always_fits_the_notification_area() {
        // Characters outside the BMP take two UTF-16 units each.
        let wide_chars = "𝔴".repeat(100);
        for e in ["ã".repeat(300), wide_chars] {
            let tip = tooltip(Some(&status(false, true, Some(&e))));
            assert!(tip.encode_utf16().count() <= TIP_MAX, "{} units", tip.encode_utf16().count());
        }
        assert_eq!(fit_utf16("𝔴𝔴", 3), "𝔴", "never half a character");
        assert_eq!(fit_utf16("abc", 3), "abc");
    }

    #[test]
    fn the_menu_has_the_five_entries_with_the_overlay_checked_while_it_runs() {
        let labels: Vec<&str> = menu(false, true).iter().map(|i| i.label).collect();
        assert_eq!(labels, ["Abrir o Wikelink", "Abrir o site", "Mostrar o overlay", "Overlay", "Sair"]);
        let overlay = |on: bool| menu(on, true).into_iter().find(|i| i.command == Command::Overlay).unwrap();
        assert!(overlay(true).checked);
        assert!(!overlay(false).checked);
        let show = |on: bool| menu(on, true).into_iter().find(|i| i.command == Command::ShowOverlay).unwrap();
        assert!(show(true).enabled && !show(true).checked, "brings back a hidden overlay while one runs");
        assert!(!show(false).enabled, "no overlay: nothing to show");
        let m = menu(true, true);
        assert!(m.iter().all(|i| i.enabled), "everything usable with a site address and an overlay");
        assert_eq!(m.iter().filter(|i| i.default).map(|i| i.command).collect::<Vec<_>>(), [Command::OpenSettings], "bold = what the double-click does");
        assert_eq!(m.iter().filter(|i| i.separator_before).map(|i| i.command).collect::<Vec<_>>(), [Command::Quit]);
        let site = menu(false, false).into_iter().find(|i| i.command == Command::OpenSite).unwrap();
        assert!(!site.enabled, "no address: greyed out");
    }

    #[test]
    fn menu_ids_round_trip_and_zero_is_nothing() {
        for c in Command::ALL {
            assert_ne!(c.id(), 0);
            assert_eq!(Command::from_id(c.id()), Some(c));
        }
        assert_eq!(Command::from_id(0), None, "the menu was dismissed");
        assert_eq!(Command::from_id(99), None);
    }

    #[test]
    fn a_settings_window_is_started_at_most_once_every_3_s() {
        let t0 = Instant::now();
        assert!(settings_due(None, t0), "never started: go");
        assert!(!settings_due(Some(t0), t0), "a double-click right after");
        assert!(!settings_due(Some(t0), t0 + Duration::from_millis(2999)));
        assert!(settings_due(Some(t0), t0 + SETTINGS_DEBOUNCE));
        assert!(settings_due(Some(t0), t0 + Duration::from_secs(60)));
        // A clock that went backwards (`last` in the future) reads as "just now", never as "long ago".
        assert!(!settings_due(Some(t0 + Duration::from_secs(1)), t0));
    }

}

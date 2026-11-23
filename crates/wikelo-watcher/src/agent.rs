//! Wikelink's watcher proper: tail this PC's `Game.log` and send mission events to Wikelist
//! ([`SITE`]), which applies them to the signed-in account (completions, automatic
//! inventory). Optionally it also keeps the server's game data current: once an hour
//! it compares the installed builds (LIVE, PTU…) with what the server already knows
//! and, when the user allowed it, extracts and uploads a signed bundle for a new one.
//!
//! Runs in the foreground (`watch`) or detached in the background (`start`, which
//! spawns `watch --daemon` without a console; `stop` asks it to quit through a
//! stop file; `status` checks the pid file). In the background it also keeps an icon in
//! the notification area (`tray`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use wikelo_core::{GIT_COMMIT, PKG_VERSION};
use wikelo_log::{Tailer, TailerConfig, TrackerOutput};

use crate::{tr, trf};

/// How often the watcher tells the server it is alive when nothing happens.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// How often installed game versions are compared with the server's.
const DATA_CHECK_EVERY: Duration = Duration::from_secs(3600);
/// How often the server is asked which watcher release is the newest.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// A Game.log written more recently than this means the game is running.
const GAME_ACTIVE_WINDOW: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(750);
/// `watcher.log` is rotated to `watcher.log.1` past this size.
const LOG_MAX: u64 = 2 * 1024 * 1024;
/// The daemon looks at the log size every this many lines, and every five minutes regardless.
const ROTATE_EVERY_LINES: u32 = 200;
const ROTATE_EVERY: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// Never an address to talk to (that is always [`server_address`]): which address the
    /// session `token` belongs to, for the rule of [`with_token_rule`]. On disk it is written
    /// only for a developer server (see [`DEV_SERVER_ENV`]); absent means [`SITE`]. As
    /// [`load_config`] returns it: the active address while a token is kept, else empty.
    #[serde(default, skip_serializing_if = "is_site_or_empty")]
    pub server: String,
    pub token: String,
    pub handle: String,
    /// Display name of the account (shown in the window).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    /// Upload new game versions automatically (`None` = not asked yet).
    #[serde(default)]
    pub share_data: Option<bool>,
    /// The settings window's "Mostrar o overlay quando o jogo abrir": the background watcher
    /// keeps an overlay process running (see [`OverlayKeeper`]), so it is on screen as soon as
    /// the game is. `None` (absent: every existing setup) is on; only `false` turns it off.
    /// A new key on purpose: the old opt-in (`overlay_with_watcher`, `false` by default) is
    /// ignored, so its stored `false` does not keep it off; it goes on the next write.
    #[serde(default, deserialize_with = "lenient_flag", skip_serializing_if = "Option::is_none")]
    pub overlay_with_game: Option<bool>,
    /// Where the player left the old single-window overlay: top-left corner in logical
    /// pixels. Read once as the place of the `fixados` card; dropped when the cards are saved.
    #[serde(default)]
    pub overlay_pos: Option<[f32; 2]>,
    /// The overlay's cards by name (`fixados`, `falta`, `aqui`, `avisos`): place and pin.
    /// Read leniently: a bad entry is skipped, never the whole file (it holds the sign-in).
    #[serde(default, deserialize_with = "lenient_widgets", skip_serializing_if = "BTreeMap::is_empty")]
    pub overlay_widgets: BTreeMap<String, OverlayWidget>,
    /// Opacity of the pinned cards while the overlay is closed; `None` = the default.
    #[serde(default, deserialize_with = "lenient_opacity", skip_serializing_if = "Option::is_none")]
    pub overlay_opacity: Option<f32>,
    /// Text size of the overlay's cards (1 = normal); `None` = the default.
    #[serde(default, deserialize_with = "lenient_opacity", skip_serializing_if = "Option::is_none")]
    pub overlay_text_scale: Option<f32>,
    /// `(tracked item id, source item id)` rows chosen for the closed overlay.
    #[serde(default, deserialize_with = "lenient_source_pins", skip_serializing_if = "Vec::is_empty")]
    pub overlay_source_pins: Vec<[String; 2]>,
    /// Where the player dragged the overlay's search strip: its top-left corner as fractions
    /// of the monitor; `None` = under the overlay's bar, its first place.
    #[serde(default, deserialize_with = "lenient_pos", skip_serializing_if = "Option::is_none")]
    pub overlay_search_pos: Option<[f32; 2]>,
    /// The overlay's widgets switched off in its widgets menu, by name (`fixados`, …,
    /// `pesquisa`); every other one is shown.
    #[serde(default, deserialize_with = "lenient_hidden", skip_serializing_if = "Vec::is_empty")]
    pub overlay_hidden: Vec<String>,
    /// The language Wikelink shows: `"en"` / `"pt"`; absent follows Windows (see `i18n`).
    /// Set in the settings window; read once at the start of every process.
    #[serde(default, deserialize_with = "lenient_text", skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

/// `server` is left out of `watcher.json` when it says nothing: no token, or [`SITE`]'s.
fn is_site_or_empty(s: &str) -> bool {
    s.is_empty() || s == SITE
}

/// A string, else `None` (never an error: the file holds the sign-in).
fn lenient_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_str().map(str::to_string))
}

/// The `lang` of `watcher.json`, read on its own so that a file this version cannot read as a
/// whole still gives the language (for `i18n::set`, before anything is shown).
pub fn lang_pref() -> Option<String> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("lang")?.as_str().map(str::to_string)
}

/// Longest widget name kept.
const OVERLAY_TAB_CHARS: usize = 32;
const OVERLAY_SOURCE_PINS_MAX: usize = 400;
const OVERLAY_ID_CHARS: usize = 200;
/// Most widget names kept as switched off.
const OVERLAY_HIDDEN_MAX: usize = 32;
/// What earlier versions saved that this one dropped: the texts of the per-text search tabs,
/// the search tab of the three-tab search, the free "Busca" card, and the old opt-in to open
/// the overlay with the watcher (replaced by `overlay_with_game`). An older file carrying
/// them loads as it is; they go on the overlay's next write (and on any other writer's,
/// which writes only the fields it knows).
const RETIRED_FIELDS: [&str; 3] = ["overlay_searches", "overlay_search_tab", "overlay_with_watcher"];
const RETIRED_CARDS: [&str; 1] = ["busca"];

/// One card of the overlay as saved: its top-left corner as fractions of the monitor,
/// whether it stays on screen while the overlay is closed ("fixar na tela"), and the size
/// the player dragged it to, as fractions of the monitor (none: the normal size).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OverlayWidget {
    pub x: f32,
    pub y: f32,
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<f32>,
}

impl OverlayWidget {
    /// `None` unless `x` and `y` are finite numbers; a missing or odd `pinned` is `false`, a
    /// missing or odd `w` / `h` is none (the overlay clamps the rest).
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).map(|n| n as f32).filter(|n| n.is_finite());
        Some(Self { x: num("x")?, y: num("y")?, pinned: v.get("pinned").and_then(serde_json::Value::as_bool).unwrap_or(false), w: num("w"), h: num("h") })
    }
}

/// The cards that read well; anything that is not an object is no cards at all.
fn lenient_widgets<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, OverlayWidget>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Object(map) => map.iter().filter_map(|(k, v)| Some((k.clone(), OverlayWidget::from_value(v)?))).collect(),
        _ => BTreeMap::new(),
    })
}

/// `true` / `false`, else `None` (the default).
fn lenient_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_bool())
}

/// A finite number, else `None`.
fn lenient_opacity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_f64().map(|n| n as f32).filter(|n| n.is_finite()))
}

/// A short, non-blank name, trimmed; anything else is none.
fn tab_name(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.chars().count() <= OVERLAY_TAB_CHARS).then(|| s.to_string())
}

fn lenient_source_pins<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<[String; 2]>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| {
            let pair = pair.as_array()?;
            let item = pair.first()?.as_str()?;
            let source = pair.get(1)?.as_str()?;
            ((!item.trim().is_empty() && !source.trim().is_empty())
                && item.chars().count() <= OVERLAY_ID_CHARS
                && source.chars().count() <= OVERLAY_ID_CHARS)
                .then(|| [item.to_string(), source.to_string()])
        })
        .take(OVERLAY_SOURCE_PINS_MAX)
        .collect())
}

/// Two finite numbers, else `None`.
fn pos_value(v: &serde_json::Value) -> Option<[f32; 2]> {
    let a = v.as_array().filter(|a| a.len() == 2)?;
    let num = |i: usize| a[i].as_f64().map(|n| n as f32).filter(|n| n.is_finite());
    Some([num(0)?, num(1)?])
}

fn lenient_pos<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<[f32; 2]>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(pos_value(&v))
}

/// The widget names (see [`tab_name`]) of a list, each once, sorted, at most
/// [`OVERLAY_HIDDEN_MAX`]; anything else is left out.
fn hidden_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = names.into_iter().filter_map(tab_name).collect();
    out.sort();
    out.dedup();
    out.truncate(OVERLAY_HIDDEN_MAX);
    out
}

fn lenient_hidden<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(hidden_names(v.as_array().into_iter().flatten().filter_map(serde_json::Value::as_str)))
}

impl AgentConfig {
    pub fn display_name(&self) -> String {
        self.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| self.handle.clone())
    }

    /// The overlay comes up with the game (see `overlay_with_game`): on unless switched off.
    pub fn overlay_with_game(&self) -> bool {
        self.overlay_with_game != Some(false)
    }
}

fn data_dir() -> PathBuf {
    wikelo_watcher::data_dir()
}

/// `<data dir>/watcher.json`.
pub fn config_path() -> PathBuf {
    data_dir().join("watcher.json")
}

pub fn pid_path() -> PathBuf {
    data_dir().join("watcher.pid")
}

pub fn stop_path() -> PathBuf {
    data_dir().join("watcher.stop")
}

/// Dropped by the window to ask the running watcher for a game-data check right now.
pub fn check_path() -> PathBuf {
    data_dir().join("watcher.check")
}

pub fn log_path() -> PathBuf {
    data_dir().join("watcher.log")
}

/// Dropped by [`close_overlay`] to ask the overlay to close; removed by the overlay that acts
/// on it (or by a fresh launch, see [`launch_overlay_with`]).
pub fn overlay_stop_path() -> PathBuf {
    data_dir().join("overlay.stop")
}

/// Dropped by the tray icon's "Mostrar o overlay" to show an overlay hidden with "Ocultar" or
/// Alt+Shift+F9; removed by the overlay that acts on it (or by one starting).
pub fn overlay_show_path() -> PathBuf {
    data_dir().join("overlay.show")
}

/// Written by [`close_overlay`] (the tray's "Overlay", the settings window's "Fechar overlay",
/// stopping the watcher, signing out): the player closed the overlay, so the background
/// watcher does not start it again (see [`OverlayKeeper`]) until a launch on purpose, the
/// next game session or the next start of the watcher.
pub fn overlay_closed_path() -> PathBuf {
    data_dir().join("overlay.closed")
}

/// Ask the running overlay to show itself again (see [`overlay_show_path`]).
pub fn request_overlay_show() -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(overlay_show_path(), "show")
}

/// Rename `watcher.log` to `watcher.log.1` once it is past `max` (the previous `.1` is
/// replaced) and point this process's stdout/stderr at a fresh file, so the daemon's own
/// `say!` lines and the tracing output keep going into the new one. Rust re-reads the
/// standard handles on every write, which is what makes the switch invisible to the
/// callers. The rename works while the file is open because Rust opens with
/// `FILE_SHARE_DELETE`. Returns whether it rotated.
pub fn rotate_log_if_big(path: &Path, max: u64) -> std::io::Result<bool> {
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) <= max {
        return Ok(false);
    }
    let old = path.with_extension("log.1");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, &old)?;
    let fresh = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    // Only the daemon's std streams are the log; anything else (tests, `watch` in a console)
    // keeps writing where it was.
    #[cfg(windows)]
    if DAEMON.load(Ordering::Relaxed) {
        redirect_std(&fresh)?;
    }
    drop(fresh);
    Ok(true)
}

/// Make `file` the process's stdout and stderr (the daemon has no console; they were the log).
#[cfg(windows)]
fn redirect_std(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    // The handles must outlive this call: one clone per stream, deliberately leaked.
    let out = file.try_clone()?;
    let err = file.try_clone()?;
    // SAFETY: plain Win32 calls on handles this process owns and keeps open.
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, out.as_raw_handle() as _);
        SetStdHandle(STD_ERROR_HANDLE, err.as_raw_handle() as _);
    }
    std::mem::forget(out);
    std::mem::forget(err);
    Ok(())
}

/// Small JSON the background watcher keeps current for the window.
pub fn status_path() -> PathBuf {
    data_dir().join("watcher.status.json")
}

/// `watcher.json` as this program uses it: after [`with_token_rule`] for the active address.
pub fn load_config() -> Option<AgentConfig> {
    load_config_from(&config_path()).map(|c| with_token_rule(c, server_address()))
}

fn load_config_from(p: &Path) -> Option<AgentConfig> {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// Write `watcher.json` as given. Several processes write it (settings window, watcher,
/// overlay), so it is never written in place: the text goes to a temporary file in the same
/// directory, which `std::fs::rename` then moves over `watcher.json` (it replaces an existing
/// destination on Windows too), and a reader sees either the old file or the new one. That
/// prevents a half-written file, not a lost update: every caller holds the [`ConfigLock`]
/// from its fresh read of the file to here.
fn save_config_to(p: &Path, c: &AgentConfig) -> anyhow::Result<()> {
    write_config_text(p, &serde_json::to_string_pretty(c)?)
}

/// [`save_config_to`] for text already made (the overlay patches the file's own JSON).
fn write_config_text(p: &Path, text: &str) -> anyhow::Result<()> {
    // Tells apart two writes of this process (threads), on top of the pid.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension(format!("json.{}-{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, p) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// The named mutex held around every read-modify-write of `watcher.json`, in every process
/// that writes it (settings window, watcher, overlay).
#[cfg(windows)]
const CONFIG_MUTEX: &str = "Local\\WikelistConfig";
/// Longest wait for [`CONFIG_MUTEX`]. A writer holds it for a file read and a rename, so a
/// longer wait means its holder hangs; the write then goes ahead without it (see [`ConfigLock`]).
#[cfg_attr(not(windows), allow(dead_code))]
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Held from the fresh read of `watcher.json` to the rename of the new one, so two writers
/// (say the overlay saving its cards and the settings window saving a sign-in) never
/// interleave and one never puts back what the other just changed. Cross-process on Windows
/// (a named mutex), within this process elsewhere.
///
/// When the lock cannot be had (the mutex cannot be created, or another process holds it
/// longer than [`CONFIG_LOCK_WAIT`]) the write goes ahead without it, as before the lock
/// existed: still an atomic rename, only no longer safe from a lost update. Refusing to
/// write instead would lose a sign-in for good over a hung process.
struct ConfigLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(windows)]
    owned: bool,
    #[cfg(not(windows))]
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ConfigLock {
    #[cfg(windows)]
    fn take() -> Self {
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        /// `WAIT_OBJECT_0`, `WAIT_ABANDONED` (the holder died holding it: the file was written
        /// by a rename or not at all, so it is fine to go on).
        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_ABANDONED: u32 = 0x80;
        let name = wide(CONFIG_MUTEX);
        // SAFETY: default security (null), not initially owned, `name` is NUL-terminated UTF-16
        // that outlives the call.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Self { handle, owned: false };
        }
        let wait = u32::try_from(CONFIG_LOCK_WAIT.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: the mutex handle CreateMutexW just returned, open until `drop`; a plain timeout.
        let got = unsafe { WaitForSingleObject(handle, wait) };
        Self { handle, owned: got == WAIT_OBJECT_0 || got == WAIT_ABANDONED }
    }

    #[cfg(not(windows))]
    fn take() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A panic while it was held left nothing half-done on disk (the write is a rename).
        Self { _guard: LOCK.lock().unwrap_or_else(|e| e.into_inner()) }
    }
}

#[cfg(windows)]
impl Drop for ConfigLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        if self.owned {
            // SAFETY: a mutex this thread owns (the wait above succeeded on this thread, and the
            // lock never leaves the function that took it).
            unsafe { ReleaseMutex(self.handle) };
        }
        if self.handle != 0 {
            // SAFETY: the handle CreateMutexW returned; this struct owns it and closes it only here.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// A read-modify-write of `watcher.json` under the [`ConfigLock`]: `change` edits a fresh read
/// of the file (or `base` when there is none or it does not parse) and must set only the
/// fields its caller owns; the result is written and returned (after [`with_token_rule`], as
/// [`load_config`] gives it), so the caller can refresh its own copy with what the other
/// writers changed.
pub fn update_config(base: &AgentConfig, change: impl FnOnce(&mut AgentConfig)) -> anyhow::Result<AgentConfig> {
    update_config_at(&config_path(), base, change).map(|c| with_token_rule(c, server_address()))
}

fn update_config_at(p: &Path, base: &AgentConfig, change: impl FnOnce(&mut AgentConfig)) -> anyhow::Result<AgentConfig> {
    let _lock = ConfigLock::take();
    let mut cfg = load_config_from(p).unwrap_or_else(|| base.clone());
    change(&mut cfg);
    save_config_to(p, &cfg)?;
    Ok(cfg)
}

/// Sign-out: delete `watcher.json` under the [`ConfigLock`], so no writer in the middle of
/// a read-modify-write puts the old sign-in back. A missing file is not an error. A chosen
/// language outlives the sign-out: the file then holds only that (no token).
pub fn remove_config() -> std::io::Result<()> {
    remove_config_at(&config_path())
}

fn remove_config_at(p: &Path) -> std::io::Result<()> {
    let _lock = ConfigLock::take();
    let lang = load_config_from(p).and_then(|c| c.lang);
    if lang.is_some() {
        let kept = AgentConfig { lang, ..Default::default() };
        return save_config_to(p, &kept).map_err(std::io::Error::other);
    }
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The fields a sign-in sets (token, the address it belongs to, account), copied from `from`.
pub fn copy_sign_in(to: &mut AgentConfig, from: &AgentConfig) {
    to.server = from.server.clone();
    to.token = from.token.clone();
    to.handle = from.handle.clone();
    to.name = from.name.clone();
}

/// Take the overlay's fields from `disk`: the overlay (its cards, opacity) and the settings
/// window (all of them) change them behind the back of whoever loaded `c` earlier.
pub fn keep_overlay_fields(c: &mut AgentConfig, disk: &AgentConfig) {
    c.overlay_pos = disk.overlay_pos;
    c.overlay_with_game = disk.overlay_with_game;
    c.overlay_widgets = disk.overlay_widgets.clone();
    c.overlay_opacity = disk.overlay_opacity;
    c.overlay_text_scale = disk.overlay_text_scale;
    c.overlay_source_pins = disk.overlay_source_pins.clone();
    c.overlay_search_pos = disk.overlay_search_pos;
    c.overlay_hidden = disk.overlay_hidden.clone();
}

/// What [`save_overlay_layout`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSave {
    Written,
    /// The file holds this layout already.
    Unchanged,
    /// No readable `watcher.json` (missing, or half-written by another program right now):
    /// nothing written; try again later.
    NoConfig,
}

/// `watcher.json` as its own JSON object, and as read by this watcher; `None` unless both
/// read (a file this version cannot read is never rewritten by the overlay).
fn load_config_object(p: &Path) -> Option<(serde_json::Map<String, serde_json::Value>, AgentConfig)> {
    let text = std::fs::read_to_string(p).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let cfg: AgentConfig = serde_json::from_value(value.clone()).ok()?;
    match value {
        serde_json::Value::Object(map) => Some((map, cfg)),
        _ => None,
    }
}

/// A number for watcher.json, written as short as it reads back (`0.07`, not
/// `0.07000000029802322`); not a number: `null`.
fn json_f32(v: f32) -> serde_json::Value {
    v.to_string().parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// One card's entry: `old` (what the file had under that name) with the place, pin and size
/// set; anything else it held stays.
fn widget_json(old: Option<&serde_json::Value>, w: &OverlayWidget) -> serde_json::Value {
    let mut m = match old {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    m.insert("x".into(), json_f32(w.x));
    m.insert("y".into(), json_f32(w.y));
    m.insert("pinned".into(), serde_json::Value::Bool(w.pinned));
    for (key, size) in [("w", w.w), ("h", w.h)] {
        match size {
            Some(v) => m.insert(key.into(), json_f32(v)),
            None => m.remove(key),
        };
    }
    serde_json::Value::Object(m)
}

/// Where the overlay's search strip sits and which of its widgets are shown, for
/// [`save_overlay_state`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlaySwitches {
    /// The strip's top-left corner as fractions of the monitor; `None`: under the bar.
    pub search_pos: Option<[f32; 2]>,
    /// Every widget the overlay has, by name, and whether it is shown. Names saved as
    /// switched off that are not here (a newer overlay's) stay as they are.
    pub shown: BTreeMap<String, bool>,
}

/// Store every display choice owned by the overlay through the same locked JSON patch.
pub fn save_overlay_state(widgets: &BTreeMap<String, OverlayWidget>, opacity: f32, text_scale: f32, source_pins: &[[String; 2]], switches: &OverlaySwitches) -> anyhow::Result<LayoutSave> {
    save_overlay_state_at(&config_path(), widgets, opacity, text_scale, source_pins, switches)
}

#[cfg(test)]
fn save_overlay_layout_at(p: &Path, widgets: &BTreeMap<String, OverlayWidget>, opacity: f32, text_scale: f32) -> anyhow::Result<LayoutSave> {
    let old = load_config_from(p).unwrap_or_default();
    let switches = OverlaySwitches { search_pos: old.overlay_search_pos, shown: BTreeMap::new() };
    save_overlay_state_at(p, widgets, opacity, text_scale, &old.overlay_source_pins, &switches)
}

/// The widgets switched off after `shown` is applied to `saved` (what the file has).
fn merge_hidden(saved: &[String], shown: &BTreeMap<String, bool>) -> Vec<String> {
    let kept = saved.iter().map(String::as_str).filter(|k| !shown.contains_key(*k));
    hidden_names(kept.chain(shown.iter().filter(|(_, on)| !**on).map(|(k, _)| k.as_str())))
}

/// The file holds something an earlier overlay saved that this one dropped (see
/// [`RETIRED_FIELDS`], [`RETIRED_CARDS`]).
fn has_retired(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    let cards = obj.get("overlay_widgets").and_then(serde_json::Value::as_object);
    RETIRED_FIELDS.iter().any(|k| obj.contains_key(*k)) || cards.is_some_and(|m| RETIRED_CARDS.iter().any(|k| m.contains_key(*k)))
}

fn save_overlay_state_at(p: &Path, widgets: &BTreeMap<String, OverlayWidget>, opacity: f32, text_scale: f32, source_pins: &[[String; 2]], switches: &OverlaySwitches) -> anyhow::Result<LayoutSave> {
    let _lock = ConfigLock::take();
    let Some((mut obj, cfg)) = load_config_object(p) else { return Ok(LayoutSave::NoConfig) };
    let search_pos = switches.search_pos.filter(|p| p.iter().all(|v| v.is_finite()));
    let hidden = merge_hidden(&cfg.overlay_hidden, &switches.shown);
    let source_pins: Vec<[String; 2]> = source_pins
        .iter()
        .filter(|[item, source]| !item.trim().is_empty() && !source.trim().is_empty() && item.chars().count() <= OVERLAY_ID_CHARS && source.chars().count() <= OVERLAY_ID_CHARS)
        .take(OVERLAY_SOURCE_PINS_MAX)
        .cloned()
        .collect();
    // The old single position lives on in the `fixados` card: gone once the cards are saved.
    let pos = if widgets.is_empty() { cfg.overlay_pos } else { None };
    let same_cards = widgets.iter().all(|(k, w)| cfg.overlay_widgets.get(k) == Some(w));
    if same_cards
        && cfg.overlay_opacity == Some(opacity)
        && cfg.overlay_text_scale == Some(text_scale)
        && cfg.overlay_pos == pos
        && cfg.overlay_source_pins == source_pins
        && cfg.overlay_search_pos == search_pos
        && cfg.overlay_hidden == hidden
        && !has_retired(&obj)
    {
        return Ok(LayoutSave::Unchanged);
    }
    let mut cards = match obj.remove("overlay_widgets") {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    for k in RETIRED_CARDS {
        cards.remove(k);
    }
    for k in RETIRED_FIELDS {
        obj.remove(k);
    }
    for (k, w) in widgets {
        let entry = widget_json(cards.get(k), w);
        cards.insert(k.clone(), entry);
    }
    if !cards.is_empty() {
        obj.insert("overlay_widgets".into(), serde_json::Value::Object(cards));
    }
    obj.insert("overlay_opacity".into(), json_f32(opacity));
    obj.insert("overlay_text_scale".into(), json_f32(text_scale));
    if source_pins.is_empty() {
        obj.remove("overlay_source_pins");
    } else {
        obj.insert("overlay_source_pins".into(), serde_json::to_value(source_pins)?);
    }
    match search_pos {
        Some([x, y]) => obj.insert("overlay_search_pos".into(), serde_json::Value::Array(vec![json_f32(x), json_f32(y)])),
        None => obj.remove("overlay_search_pos"),
    };
    if hidden.is_empty() {
        obj.remove("overlay_hidden");
    } else {
        obj.insert("overlay_hidden".into(), serde_json::to_value(&hidden)?);
    }
    if pos.is_none() && obj.contains_key("overlay_pos") {
        obj.insert("overlay_pos".into(), serde_json::Value::Null);
    }
    write_config_text(p, &serde_json::to_string_pretty(&serde_json::Value::Object(obj))?)?;
    Ok(LayoutSave::Written)
}

/// "Repor posições": every card back to its default place and pin, and the search strip under
/// the bar, at the overlay's next start (the opacity and the widget switches stay). Same rules as [`save_overlay_layout`]; `Ok(true)` when the
/// file was written.
pub fn reset_overlay_layout() -> anyhow::Result<bool> {
    reset_overlay_layout_at(&config_path())
}

fn reset_overlay_layout_at(p: &Path) -> anyhow::Result<bool> {
    let _lock = ConfigLock::take();
    let Some((mut obj, cfg)) = load_config_object(p) else { return Ok(false) };
    if cfg.overlay_pos.is_none() && cfg.overlay_widgets.is_empty() && cfg.overlay_search_pos.is_none() {
        return Ok(false);
    }
    obj.remove("overlay_widgets");
    obj.remove("overlay_search_pos");
    if obj.contains_key("overlay_pos") {
        obj.insert("overlay_pos".into(), serde_json::Value::Null);
    }
    write_config_text(p, &serde_json::to_string_pretty(&serde_json::Value::Object(obj))?)?;
    Ok(true)
}

/// When `watcher.json` last changed (the overlay waits on it while signed out).
pub fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}

/// What the background watcher is doing, for `status` and the window.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonStatus {
    pub pid: u32,
    pub started_at: String,
    pub server: String,
    pub channel: String,
    /// Last successful exchange with the server (RFC 3339).
    pub last_ok: Option<String>,
    /// Paused from the site (Dados → Watcher).
    pub paused: bool,
    pub last_error: Option<String>,
    /// Last mission event seen in the Game.log (one line).
    pub last_event: Option<String>,
    /// Last game-data check result (one line).
    pub data_check: Option<String>,
    pub share_data: bool,
    /// What this daemon runs.
    #[serde(default)]
    pub version: String,
    /// A newer release the server told us about (version and its download page).
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_url: Option<String>,
}

impl DaemonStatus {
    fn write(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(status_path(), s);
        }
    }
}

pub fn read_status() -> Option<DaemonStatus> {
    std::fs::read_to_string(status_path()).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// Ask the running watcher to compare game versions with the server now.
pub fn request_data_check() -> anyhow::Result<()> {
    std::fs::write(check_path(), "check")?;
    Ok(())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

pub fn prompt(label: &str) -> anyhow::Result<String> {
    eprint!("{label}");
    std::io::stderr().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn yes_no(label: &str, default_yes: bool) -> anyhow::Result<bool> {
    let choices = if default_yes { tr!("Y/n", "S/n") } else { tr!("y/N", "s/N") };
    let a = prompt(&format!("{label} [{choices}] "))?.to_ascii_lowercase();
    Ok(match a.as_str() {
        "" => default_yes,
        "s" | "sim" | "y" | "yes" => true,
        _ => false,
    })
}

/// Local wall clock for the log and the window.
fn now_local() -> time::OffsetDateTime {
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc())
}

fn time_now() -> String {
    let now = now_local();
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

/// Lines written so far, and whether this process is the daemon (its stderr is the log file).
static LINES: AtomicU32 = AtomicU32::new(0);
static DAEMON: AtomicBool = AtomicBool::new(false);

/// One timestamped line on stderr (the console, or `watcher.log` in the daemon, which also
/// rotates the log every [`ROTATE_EVERY_LINES`] lines).
pub(crate) fn emit(args: std::fmt::Arguments<'_>) {
    eprintln!("[{}] {}", time_now(), args);
    if DAEMON.load(Ordering::Relaxed) && LINES.fetch_add(1, Ordering::Relaxed) % ROTATE_EVERY_LINES == ROTATE_EVERY_LINES - 1 {
        let _ = rotate_log_if_big(&log_path(), LOG_MAX);
    }
}

macro_rules! say {
    ($($arg:tt)*) => { $crate::agent::emit(format_args!($($arg)*)) };
}

// ----- HTTP client ------------------------------------------------------------------

#[derive(Serialize)]
struct AgentEvent<'a> {
    replay: bool,
    event: &'a TrackerOutput,
}

#[derive(Serialize)]
struct Batch<'a> {
    channel: &'a str,
    host: &'a str,
    share_data: bool,
    /// So the site can say when this watcher is older than the latest release.
    version: &'static str,
    commit: &'static str,
    events: Vec<AgentEvent<'a>>,
}

/// `GET /api/watcher/latest`: the newest release the server knows of (its `url` is the
/// release page, never a file name, so the rename to `wikelink.exe` changes nothing here).
#[derive(Deserialize)]
struct LatestResponse {
    latest: Option<LatestRelease>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatestRelease {
    pub version: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Deserialize)]
struct Ack {
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
    account: AccountInfo,
}

#[derive(Deserialize)]
struct AccountInfo {
    handle: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
}

#[derive(Deserialize)]
struct Me {
    account: Option<AccountInfo>,
}

/// The name shown for `a`: its display name, or its handle when it has none (through
/// [`server_text`]).
fn account_name(a: &AccountInfo) -> String {
    let name = server_text(&a.display_name);
    if name.trim().is_empty() { server_text(&a.handle) } else { name }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct KnownDataset {
    channel: String,
    version: String,
    /// Version of the extractor that made the server's copy (`None` when the server never
    /// recorded one, or is too old to say).
    #[serde(default)]
    extractor: Option<String>,
    /// `EXTRACTOR_REVISION` of the newest copy there (0: made before revisions existed;
    /// `None`: the server does not say).
    #[serde(default)]
    revision: Option<u32>,
}

/// Why a build installed here goes to the server.
#[derive(Debug, PartialEq, Eq)]
enum UploadReason {
    /// The server has no dataset for this game version at all.
    New,
    /// It has one, but every copy came from an older extractor than this watcher (the newest
    /// of those versions, or `None` when the server could not say). A fresh extraction
    /// brings the fields newer extractors add, e.g. mission rewards.
    OlderExtractor(Option<String>),
}

/// Whether `channel version`, installed here, should be extracted and sent given what the
/// server already has. One copy made by this extractor or a newer one, in use yet or not,
/// settles it: our own earlier upload counts, so there is no re-sending loop. An extractor
/// version that does not parse counts as current, never as old.
///
/// What is compared is the extractor's revision (`wikelo_core::EXTRACTOR_REVISION`), which only
/// changes when the data it writes does; the program's version decides only when the answer
/// carries no revision at all.
fn upload_reason(known: &[KnownDataset], channel: &str, version: &str, mine: &str, mine_revision: u32) -> Option<UploadReason> {
    let copies: Vec<&KnownDataset> = known.iter().filter(|k| k.channel.eq_ignore_ascii_case(channel) && k.version == version).collect();
    if copies.is_empty() {
        return Some(UploadReason::New);
    }
    if copies.iter().any(|k| k.revision.is_some()) {
        if copies.iter().any(|k| k.revision.is_some_and(|r| r >= mine_revision)) {
            return None;
        }
    } else if copies.iter().any(|k| k.extractor.as_deref().is_some_and(|v| !wikelo_core::version::is_newer(mine, v))) {
        return None;
    }
    let newest = copies.iter().filter_map(|k| k.extractor.as_deref()).fold(None::<&str>, |best, v| match best {
        Some(b) if !wikelo_core::version::is_newer(v, b) => Some(b),
        _ => Some(v),
    });
    Some(UploadReason::OlderExtractor(newest.map(str::to_string)))
}

#[derive(Deserialize)]
struct KnownDatasets {
    known: Vec<KnownDataset>,
}

#[derive(Deserialize)]
struct UploadReport {
    status: String,
}

#[derive(Debug)]
pub enum SendError {
    Unauthorized,
    /// The server has no dataset for the channel (409).
    NoData,
    /// The server refused the request and said why (4xx with a message).
    Rejected(String),
    /// The server could not read the event batch (422): a server older than this watcher that
    /// does not know one of the event kinds (`AtLocation` before 1.1). Only `Client::send`.
    Unprocessable(String),
    /// Nothing was sent: the address breaks the transport rule, or the session token belongs
    /// to another address (see [`Client::url`]). The text says why.
    Unusable(String),
    Other(String),
}

/// `AtLocation`, the event kind servers before watcher 1.1 do not know.
fn is_location_event(e: &TrackerOutput) -> bool {
    matches!(e, TrackerOutput::AtLocation { .. })
}

impl From<ureq::Error> for SendError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(401) => SendError::Unauthorized,
            ureq::Error::StatusCode(409) => SendError::NoData,
            ureq::Error::StatusCode(code) => SendError::Other(format!("HTTP {code}")),
            other => SendError::Other(other.to_string()),
        }
    }
}

/// The HTTP agent of every request, giving up after `timeout`. Statuses are not errors here:
/// `check` reads the server's own message out of a refusal. Redirects are not followed: a
/// request only ever goes to the address [`Client::url`] checked, never to one an answer names.
fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).max_redirects(0).build().into()
}

fn http() -> ureq::Agent {
    http_agent(Duration::from_secs(60))
}

/// Most characters kept of a text from the server that is printed or saved (a handle, a
/// display name, an error sentence, a version).
const SERVER_TEXT_MAX: usize = 200;

/// `s`, a text from the server, as it may be printed in the terminal, shown or saved to
/// `watcher.json`: control characters removed and at most [`SERVER_TEXT_MAX`] characters.
pub(crate) fn server_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(SERVER_TEXT_MAX).collect()
}

/// The server's `{ "error": … }` text (through [`server_text`]), or `HTTP <status>` when the
/// body has none.
pub(crate) fn server_error(status: u16, body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(server_text))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("HTTP {status}"))
}

/// 401 → `Unauthorized`, 409 → `NoData`, any other 4xx/5xx → `Rejected` with the server's words.
fn check(mut res: ureq::http::Response<ureq::Body>) -> Result<ureq::http::Response<ureq::Body>, SendError> {
    let status = res.status().as_u16();
    if status < 400 {
        return Ok(res);
    }
    let body = read_body(&mut res, ERROR_BODY_LIMIT).unwrap_or_default();
    Err(match status {
        401 => SendError::Unauthorized,
        409 => SendError::NoData,
        _ => SendError::Rejected(server_error(status, &body)),
    })
}

/// Most bytes the overlay reads of one answer (its whole data is a few dozen KiB). Anything
/// bigger is refused before it can take the memory.
pub(crate) const OVERLAY_BODY_LIMIT: u64 = 2 * 1024 * 1024;
/// Most bytes of the one bigger answer the overlay reads: a channel's dataset, for its list
/// of locations (the move picker's filter), once per endpoint.
pub(crate) const DATASET_BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// Most bytes read of a refusal's body: only its `{ "error": … }` text is used.
const ERROR_BODY_LIMIT: u64 = 64 * 1024;
/// Most bytes of the small answers outside the overlay: the sign-in, the account, the
/// acknowledgement of a batch of events, the newest release. Each is a few hundred bytes.
const SMALL_BODY_LIMIT: u64 = 64 * 1024;
/// Most bytes of the game versions the server knows (`/api/agent/datasets`): one short entry
/// per channel and version ever uploaded, so it grows with time; 1 MiB holds thousands.
const DATASETS_BODY_LIMIT: u64 = 1024 * 1024;
/// Most bytes of the server's report on an uploaded bundle: it carries the dataset's
/// warnings, which can run to hundreds of lines.
const UPLOAD_REPORT_LIMIT: u64 = 1024 * 1024;

/// Whether a `Content-Length` refuses the answer before reading it: a number over `limit`.
/// Missing or unreadable: no (the read itself stops at the limit).
fn declared_too_big(content_length: Option<&str>, limit: u64) -> bool {
    content_length.and_then(|v| v.trim().parse::<u64>().ok()).is_some_and(|n| n > limit)
}

fn body_too_big(limit: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    let size = if limit >= MIB { format!("{} MiB", limit / MIB) } else { format!("{} KiB", limit / 1024) };
    trf!("answer from the server too big (over {size})", "resposta grande demais do servidor (mais de {size})")
}

/// The body of `res`, refused past `limit` bytes: on its `Content-Length` before reading, and
/// in any case as soon as the bytes read (after decompression) go past it.
fn read_body(res: &mut ureq::http::Response<ureq::Body>, limit: u64) -> Result<Vec<u8>, String> {
    let declared = res.headers().get("content-length").and_then(|v| v.to_str().ok());
    if declared_too_big(declared, limit) {
        return Err(body_too_big(limit));
    }
    read_limited(res.body_mut().as_reader(), limit)
}

/// [`read_body`], then the JSON in it.
fn read_json_body<T: serde::de::DeserializeOwned>(res: &mut ureq::http::Response<ureq::Body>, limit: u64) -> Result<T, SendError> {
    let body = read_body(res, limit).map_err(SendError::Other)?;
    serde_json::from_slice(&body).map_err(|e| SendError::Other(e.to_string()))
}

/// Read `r` to the end, but never more than `limit + 1` bytes: one byte past the limit is
/// enough to know the answer is too big, whatever its framing (chunked, or a lying length).
fn read_limited(r: impl std::io::Read, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut body = Vec::new();
    r.take(limit.saturating_add(1)).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > limit {
        return Err(body_too_big(limit));
    }
    Ok(body)
}

/// What `Client::request` got back: the status, the body, and the wait a 429 asked for.
pub(crate) struct ApiAnswer {
    pub status: u16,
    pub body: Vec<u8>,
    /// Verbatim ETag response header, when the server sent a valid text value.
    pub etag: Option<String>,
    /// `Retry-After` in seconds (the date form is not read).
    pub retry_after: Option<Duration>,
}

/// Longest `Retry-After` taken as it is.
const MAX_RETRY_AFTER: u64 = 3600;

/// A `Retry-After` value in seconds; `None` for anything else.
fn parse_retry_after(v: &str) -> Option<Duration> {
    v.trim().parse::<u64>().ok().map(|s| Duration::from_secs(s.min(MAX_RETRY_AFTER)))
}

/// A request `Client::request` makes: path under the server, and the JSON body for writes
/// (`Put` without one sends an empty body).
pub(crate) enum ApiRequest<'a> {
    Get(&'a str, Option<&'a str>),
    Delete(&'a str),
    Patch(&'a str, &'a serde_json::Value),
    Put(&'a str, Option<&'a serde_json::Value>),
    Post(&'a str, &'a serde_json::Value),
}

#[derive(Clone)]
pub struct Client {
    /// The base address every request of this client goes to, or why nothing may be sent;
    /// requests get their address from [`Client::url`] only.
    base: Result<String, String>,
    token: String,
    http: ureq::Agent,
}

impl Client {
    /// A client of [`server_address`]. `owner` is the address `token` belongs to
    /// (`AgentConfig::server` as [`load_config`] gives it): a token that belongs elsewhere is
    /// never sent (see [`with_token_rule`]).
    pub fn new(owner: &str, token: &str) -> Self {
        Self::at(server_address(), owner, token, http())
    }

    /// Like `new`, with a shorter overall timeout (the overlay polls every few seconds and
    /// must not hang on a dead server for a minute).
    pub fn with_timeout(owner: &str, token: &str, timeout: Duration) -> Self {
        Self::at(server_address(), owner, token, http_agent(timeout))
    }

    /// A client of `active` (tests give it; the program always [`server_address`]).
    fn at(active: &str, owner: &str, token: &str, http: ureq::Agent) -> Self {
        let base = transport_base(active).and_then(|base| {
            if token.is_empty() || token_belongs_to(owner, &base) {
                Ok(base)
            } else {
                Err(tr!("this session was made for another server; sign in again", "esta sessão foi feita para outro servidor; entre de novo").to_string())
            }
        });
        Self { base, token: token.to_string(), http }
    }

    /// One authenticated request with the status and body as they came back (the caller
    /// reads 401, 404 and 429 itself). Only transport failures are errors, and an answer over
    /// [`OVERLAY_BODY_LIMIT`]: refused on its `Content-Length` before reading, or as soon as
    /// the body (a chunked one, say) goes past it. Only the overlay makes these requests.
    pub(crate) fn request(&self, req: ApiRequest<'_>) -> Result<ApiAnswer, String> {
        self.request_limited(req, OVERLAY_BODY_LIMIT)
    }

    /// [`Client::request`] with another limit on the answer's size.
    pub(crate) fn request_limited(&self, req: ApiRequest<'_>, limit: u64) -> Result<ApiAnswer, String> {
        let path = match &req {
            ApiRequest::Get(p, _) | ApiRequest::Delete(p) | ApiRequest::Patch(p, _) | ApiRequest::Put(p, _) | ApiRequest::Post(p, _) => *p,
        };
        let url = self.url(path)?;
        let auth = self.auth();
        let res = match req {
            ApiRequest::Get(_, etag) => {
                let request = self.http.get(&url).header("Authorization", &auth);
                match etag {
                    Some(etag) => request.header("If-None-Match", etag).call(),
                    None => request.call(),
                }
            }
            ApiRequest::Delete(_) => self.http.delete(&url).header("Authorization", &auth).call(),
            ApiRequest::Patch(_, body) => self.http.patch(&url).header("Authorization", &auth).send_json(body),
            ApiRequest::Put(_, Some(body)) => self.http.put(&url).header("Authorization", &auth).send_json(body),
            ApiRequest::Put(_, None) => self.http.put(&url).header("Authorization", &auth).send_empty(),
            ApiRequest::Post(_, body) => self.http.post(&url).header("Authorization", &auth).send_json(body),
        };
        let mut res = res.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let etag = res.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(parse_retry_after);
        let body = read_body(&mut res, limit)?;
        Ok(ApiAnswer { status, body, etag, retry_after })
    }

    /// The address of `path` on the server. Every request of this program (the watcher, the
    /// sign-in, the overlay) gets its address here, and nothing else builds one. When the base
    /// breaks the transport rule of [`transport_base`] (https, or plain http to this PC only),
    /// or the token belongs to another address, the answer is the reason, and no token is sent
    /// anywhere.
    fn url(&self, path: &str) -> Result<String, String> {
        self.base.as_ref().map(|base| format!("{base}{path}")).map_err(Clone::clone)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Display name of the signed-in account (through [`server_text`]), `None` when the token
    /// is not accepted.
    fn me(&self) -> Result<Option<String>, SendError> {
        let url = self.url("/api/auth/me").map_err(SendError::Unusable)?;
        if self.token.is_empty() {
            return Ok(None);
        }
        let mut res = check(self.http.get(&url).header("Authorization", &self.auth()).call()?)?;
        let me: Me = read_json_body(&mut res, SMALL_BODY_LIMIT)?;
        Ok(me.account.map(|a| account_name(&a)))
    }

    fn send(&self, channel: &str, host: &str, share_data: bool, events: &[(bool, TrackerOutput)]) -> Result<Ack, SendError> {
        self.send_with_status(channel, host, share_data, events).map_err(|(_, e)| e)
    }

    /// [`Client::send`], keeping the HTTP status of the answer with an error (`None` when no
    /// answer came: no network, a timeout, an address that may not be used).
    fn send_with_status(&self, channel: &str, host: &str, share_data: bool, events: &[(bool, TrackerOutput)]) -> Result<Ack, (Option<u16>, SendError)> {
        let body = Batch {
            channel,
            host,
            share_data,
            version: PKG_VERSION,
            commit: GIT_COMMIT,
            events: events.iter().map(|(replay, event)| AgentEvent { replay: *replay, event }).collect(),
        };
        let url = self.url("/api/agent/events").map_err(|e| (None, SendError::Unusable(e)))?;
        let mut res = self.http.post(&url).header("Authorization", &self.auth()).send_json(&body).map_err(|e| {
            let status = match &e {
                ureq::Error::StatusCode(code) => Some(*code),
                _ => None,
            };
            (status, SendError::from(e))
        })?;
        let status = res.status().as_u16();
        if status == 422 {
            let body = read_body(&mut res, ERROR_BODY_LIMIT).unwrap_or_default();
            return Err((Some(422), SendError::Unprocessable(server_error(422, &body))));
        }
        let mut res = check(res).map_err(|e| (Some(status), e))?;
        read_json_body(&mut res, SMALL_BODY_LIMIT).map_err(|e| (Some(status), e))
    }

    fn bye(&self) {
        if let Ok(url) = self.url("/api/agent/bye") {
            let _ = self.http.post(&url).header("Authorization", &self.auth()).send_empty();
        }
    }

    fn known_datasets(&self) -> Result<Vec<KnownDataset>, SendError> {
        let url = self.url("/api/agent/datasets").map_err(SendError::Unusable)?;
        let mut res = check(self.http.get(&url).header("Authorization", &self.auth()).call()?)?;
        let k: KnownDatasets = read_json_body(&mut res, DATASETS_BODY_LIMIT)?;
        Ok(k.known
            .into_iter()
            .map(|d| KnownDataset { channel: d.channel.to_ascii_uppercase(), ..d })
            .collect())
    }

    /// The newest watcher release the server knows of (`None` when it has not looked yet). Its
    /// `version` goes through [`server_text`]; its `url` is kept only when it is a page of this
    /// project's releases (see [`openable_url`]); anything else becomes empty, and no button
    /// opens it.
    fn latest_release(&self) -> Result<Option<LatestRelease>, SendError> {
        let url = self.url("/api/watcher/latest").map_err(SendError::Unusable)?;
        let mut res = check(self.http.get(&url).call()?)?;
        let r: LatestResponse = read_json_body(&mut res, SMALL_BODY_LIMIT)?;
        Ok(r.latest.map(|l| LatestRelease { version: server_text(&l.version), url: openable_url(&l.url, Link::Release).unwrap_or_default() }))
    }

    /// Upload a bundle; a refusal comes back as `Rejected` with the server's own words.
    fn upload_bundle(&self, bytes: &[u8]) -> Result<UploadReport, SendError> {
        let url = self.url("/api/bundles").map_err(SendError::Unusable)?;
        let mut res = check(self.http.post(&url).header("Authorization", &self.auth()).header("Content-Type", "application/zip").send(bytes)?)?;
        read_json_body(&mut res, UPLOAD_REPORT_LIMIT)
    }
}

/// A release newer than this executable, as the server reports it: `(version, download page)`.
pub fn update_available(cfg: &AgentConfig) -> Option<(String, String)> {
    let r = Client::new(&cfg.server, &cfg.token).latest_release().ok().flatten()?;
    wikelo_core::version::is_newer(&r.version, PKG_VERSION).then_some((r.version, r.url))
}

/// The version number of the newest release the server knows of (`None`: it has not looked
/// yet), for `wikelink update`. Asked without the session token; only the number is used.
pub fn latest_version() -> Result<Option<String>, String> {
    Client::new("", "").latest_release().map(|r| r.map(|l| l.version)).map_err(|e| format!("{e:?}"))
}

// ----- sign-in through the site -------------------------------------------------------
//
// Wikelink asks the server for a short code (`POST /api/auth/link/start`, with this PC's
// name), opens `<server>/link` in the browser, where the player, signed in on the
// site, types the code Wikelink shows and confirms, and meanwhile asks every few seconds
// (`POST /api/auth/link/poll`, with the secret only this client got) whether it was confirmed.
// Once it is, the answer carries the session token, as a sign-in did before.

/// Overall timeout of each request of the sign-in through the site.
const LINK_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest pause between two polls this client takes from an answer.
const LINK_MAX_INTERVAL_SECS: u64 = 30;
/// Longest life of a code this client takes from an answer.
const LINK_MAX_WAIT_SECS: u64 = 3600;
/// How often a waiting sign-in looks at its cancel switch.
const LINK_CANCEL_STEP: Duration = Duration::from_millis(100);
/// Longest secret taken from an answer (it is sent back as it came).
const LINK_SECRET_MAX: usize = 256;
/// The sign-out request gives up after this: signing out on this PC never waits longer.
const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(5);

/// A sign-in through the site that was started: the code the player compares in the browser,
/// the secret only this client knows (sent back on every poll), how long the code lives and
/// how long to wait between polls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCode {
    pub code: String,
    pub secret: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

/// A session the server handed over: the same data the sign-in always gave (token, account).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    pub token: String,
    pub handle: String,
    /// Display name of the account, or its handle when it has none.
    pub name: String,
}

/// One answer to a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPoll {
    /// Nobody confirmed nor refused it yet.
    Pending,
    /// Confirmed in the browser: the session for this PC.
    SignedIn(SignIn),
    /// Refused in the browser.
    Denied,
    /// The code is no longer valid (it expired, or the server does not know it).
    Expired,
}

/// Why a request of the sign-in through the site gave no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// 404: this server has no sign-in through the site.
    Unsupported,
    /// 429: the server's own sentence, and the wait it asked for.
    RateLimited { message: String, retry_after: Option<Duration> },
    /// No answer (network), another refusal (the server's words) or an answer that does not
    /// read as it should.
    Failed(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Unsupported => f.write_str(tr!("this server does not support Wikelink (update the server)", "este servidor não tem suporte ao Wikelink (atualize o servidor)")),
            LinkError::RateLimited { message, .. } => f.write_str(message),
            LinkError::Failed(m) => f.write_str(m),
        }
    }
}

/// How a sign-in through the site ended (see [`wait_for_link`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    SignedIn(SignIn),
    Denied,
    Expired,
    /// The player cancelled it here (the window's "Cancel", Ctrl+C in `watch`).
    Cancelled,
    Failed(LinkError),
}

impl std::fmt::Display for LinkOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkOutcome::SignedIn(s) => {
                let name = &s.name;
                f.write_str(&trf!("signed in as {name}", "conectado como {name}"))
            }
            LinkOutcome::Denied => f.write_str(tr!("the sign-in was refused on the site", "a entrada foi recusada no site")),
            LinkOutcome::Expired => f.write_str(tr!("the code expired before it was confirmed; sign in again", "o código expirou antes da confirmação; entre de novo")),
            LinkOutcome::Cancelled => f.write_str(tr!("sign-in cancelled", "entrada cancelada")),
            LinkOutcome::Failed(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkStartResponse {
    code: String,
    secret: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct LinkStatus {
    #[serde(default)]
    status: String,
}

/// `code` as the server must give it: two groups of four capital letters or digits 2 to 9,
/// joined by a dash (`^[A-Z2-9]{4}-[A-Z2-9]{4}$`). Anything else is refused, so a code can
/// only ever be those nine plain characters in an address.
fn is_link_code(code: &str) -> bool {
    let b = code.as_bytes();
    b.len() == 9 && b.iter().enumerate().all(|(i, &c)| if i == 4 { c == b'-' } else { c.is_ascii_uppercase() || (b'2'..=b'9').contains(&c) })
}

/// The page where the player types the code and confirms a sign-in: `<origin>/link`. The
/// address carries no code: the player reads it in Wikelink and types it on the site. The origin is
/// Wikelink's own server address ([`server_address`], as [`openable_url`] lets it be opened as a
/// [`Link::Site`]), never one from an answer. `None`, and nothing is opened, unless `code` is
/// [`is_link_code`].
pub fn link_page(code: &str) -> Option<String> {
    link_page_at(server_address(), code)
}

/// [`link_page`] for the address `active` (tests give it). Pure.
fn link_page_at(active: &str, code: &str) -> Option<String> {
    if !is_link_code(code) {
        return None;
    }
    openable_url(active, Link::Site).map(|origin| format!("{origin}/link"))
}

fn unexpected_answer() -> LinkError {
    LinkError::Failed(tr!("unexpected answer from the server", "resposta inesperada do servidor").to_string())
}

/// A refusal (4xx/5xx) of a sign-in request.
fn link_refusal(status: u16, body: &[u8], retry_after: Option<Duration>) -> LinkError {
    match status {
        404 => LinkError::Unsupported,
        429 => LinkError::RateLimited { message: server_error(429, body), retry_after },
        _ => LinkError::Failed(server_error(status, body)),
    }
}

/// What an answer to `link/start` means. Pure. The code must pass [`is_link_code`] and the
/// secret be printable ASCII of reasonable length; the times are kept within
/// [`LINK_MAX_WAIT_SECS`] and [`LINK_MAX_INTERVAL_SECS`], and at least one second.
fn link_start_answer(status: u16, body: &[u8], retry_after: Option<Duration>) -> Result<LinkCode, LinkError> {
    if status != 200 {
        return Err(if status >= 400 { link_refusal(status, body, retry_after) } else { unexpected_answer() });
    }
    let r: LinkStartResponse = serde_json::from_slice(body).map_err(|_| unexpected_answer())?;
    let secret_ok = !r.secret.is_empty() && r.secret.len() <= LINK_SECRET_MAX && r.secret.bytes().all(|b| b.is_ascii_graphic());
    if !is_link_code(&r.code) || !secret_ok {
        return Err(unexpected_answer());
    }
    Ok(LinkCode {
        code: r.code,
        secret: r.secret,
        expires_in: Duration::from_secs(r.expires_in.clamp(1, LINK_MAX_WAIT_SECS)),
        interval: Duration::from_secs(r.interval.clamp(1, LINK_MAX_INTERVAL_SECS)),
    })
}

/// What an answer to `link/poll` means. Pure. 202: still waiting; 200: the session (a token
/// that could not go in a header is refused); 410: refused or expired (anything but `denied`
/// reads as expired).
fn link_poll_answer(status: u16, body: &[u8], retry_after: Option<Duration>) -> Result<LinkPoll, LinkError> {
    match status {
        202 => Ok(LinkPoll::Pending),
        200 => {
            let r: LoginResponse = serde_json::from_slice(body).map_err(|_| unexpected_answer())?;
            if r.token.is_empty() || !r.token.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(unexpected_answer());
            }
            // The handle and the name are printed and saved to `watcher.json`: plain text only.
            let name = account_name(&r.account);
            Ok(LinkPoll::SignedIn(SignIn { token: r.token, handle: server_text(&r.account.handle), name }))
        }
        410 => {
            let said = serde_json::from_slice::<LinkStatus>(body).map(|s| s.status).unwrap_or_default();
            Ok(if said == "denied" { LinkPoll::Denied } else { LinkPoll::Expired })
        }
        s if s >= 400 => Err(link_refusal(s, body, retry_after)),
        _ => Err(unexpected_answer()),
    }
}

impl Client {
    /// The same client with another session token (same address, same timeout).
    fn with_token(&self, token: &str) -> Client {
        Client { base: self.base.clone(), token: token.to_string(), http: self.http.clone() }
    }

    /// Status, body and `Retry-After` of an answer to a sign-in request; no answer at all is
    /// [`LinkError::Failed`] with why.
    fn link_answer(&self, res: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<ApiAnswer, LinkError> {
        let server = self.base.clone().unwrap_or_default();
        let mut res = res.map_err(|e| LinkError::Failed(trf!("could not reach {server}: {e}", "não foi possível falar com {server}: {e}")))?;
        let status = res.status().as_u16();
        let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(parse_retry_after);
        let body = read_body(&mut res, SMALL_BODY_LIMIT).map_err(LinkError::Failed)?;
        Ok(ApiAnswer { status, body, etag: None, retry_after })
    }

    /// `POST /api/auth/link/start` with this PC's name; no session token goes with it.
    fn link_start(&self, device: &str) -> Result<LinkCode, LinkError> {
        let url = self.url("/api/auth/link/start").map_err(LinkError::Failed)?;
        let res = self.http.post(&url).send_json(serde_json::json!({ "device": device }));
        let a = self.link_answer(res)?;
        link_start_answer(a.status, &a.body, a.retry_after)
    }

    /// `POST /api/auth/link/poll` with the secret of [`Client::link_start`]'s answer.
    fn link_poll(&self, secret: &str) -> Result<LinkPoll, LinkError> {
        let url = self.url("/api/auth/link/poll").map_err(LinkError::Failed)?;
        let res = self.http.post(&url).send_json(serde_json::json!({ "secret": secret }));
        let a = self.link_answer(res)?;
        link_poll_answer(a.status, &a.body, a.retry_after)
    }

    /// Poll every `link.interval` until the code is confirmed, refused or expired (its
    /// `expires_in` counted from now), a request fails, or `cancel` is set (looked at every
    /// [`LINK_CANCEL_STEP`]). A 429 only makes the next pause longer (its `Retry-After`).
    fn wait_for_link(&self, link: &LinkCode, cancel: &AtomicBool) -> LinkOutcome {
        let deadline = Instant::now() + link.expires_in;
        let mut pause = link.interval;
        loop {
            let until = (Instant::now() + pause).min(deadline);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return LinkOutcome::Cancelled;
                }
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                std::thread::sleep(left.min(LINK_CANCEL_STEP));
            }
            if Instant::now() >= deadline {
                return LinkOutcome::Expired;
            }
            pause = link.interval;
            match self.link_poll(&link.secret) {
                Ok(LinkPoll::Pending) => {}
                Ok(LinkPoll::SignedIn(s)) => {
                    if cancel.load(Ordering::Relaxed) {
                        // Cancelled while this answer was on its way: the session is not kept
                        // here, so it is ended on the server too.
                        let _ = self.with_token(&s.token).sign_out();
                        return LinkOutcome::Cancelled;
                    }
                    return LinkOutcome::SignedIn(s);
                }
                Ok(LinkPoll::Denied) => return LinkOutcome::Denied,
                Ok(LinkPoll::Expired) => return LinkOutcome::Expired,
                Err(LinkError::RateLimited { retry_after, .. }) => pause = retry_after.unwrap_or(link.interval * 2).max(link.interval),
                Err(e) => return LinkOutcome::Failed(e),
            }
        }
    }

    /// `POST /api/auth/logout` with the session token, so the server ends that session. `Ok`
    /// for a 2xx answer, and when there is no token (nothing to end).
    fn sign_out(&self) -> Result<(), String> {
        let url = self.url("/api/auth/logout")?;
        if self.token.is_empty() {
            return Ok(());
        }
        let res = self.http.post(&url).header("Authorization", &self.auth()).send_empty().map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        if (200..300).contains(&status) { Ok(()) } else { Err(format!("HTTP {status}")) }
    }
}

/// The client of the sign-in through the site: [`server_address`], no session token yet.
fn link_client() -> Client {
    Client::with_timeout("", "", LINK_TIMEOUT)
}

/// Start a sign-in through the site at [`server_address`], naming this PC as the heartbeat
/// does ([`pc_name`]), so the site can show which PC asks.
pub fn link_start() -> Result<LinkCode, LinkError> {
    link_client().link_start(&pc_name())
}

/// Wait for the player's decision on the site about `link` (see [`Client::wait_for_link`]).
/// Blocks: the window runs it on a thread of its own.
pub fn wait_for_link(link: &LinkCode, cancel: &AtomicBool) -> LinkOutcome {
    link_client().wait_for_link(link, cancel)
}

/// Best effort, before the session is forgotten here: ask the server to end the session `cfg`
/// holds, giving up after [`SIGN_OUT_TIMEOUT`]. Its answer never stops the sign-out on this PC.
pub fn sign_out_on_server(cfg: &AgentConfig) -> Result<(), String> {
    if cfg.token.is_empty() {
        return Ok(());
    }
    Client::with_timeout(&cfg.server, &cfg.token, SIGN_OUT_TIMEOUT).sign_out()
}

/// [`sign_out_on_server`] for a session `token` that [`server_address`] just gave (a sign-in
/// the window no longer keeps), giving up after [`SIGN_OUT_TIMEOUT`]. Best effort.
pub fn sign_out_token(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Ok(());
    }
    Client::with_timeout(server_address(), token, SIGN_OUT_TIMEOUT).sign_out()
}

/// This PC's name as the heartbeat sends it (`COMPUTERNAME`, else `HOSTNAME`, else `pc`).
fn pc_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "pc".into())
}

/// The public repository Wikelink's releases are published from (the README's Releases
/// link): the only place a "new version" link from the server may lead, and the only place the
/// updater (`update`) downloads from.
pub(crate) const RELEASES_REPO: &str = "lfccs/wikelink";

/// A web address taken apart: what Wikelink checks before it sends a request or hands a link
/// to the system.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WebUrl {
    /// `https` or `http`.
    scheme: &'static str,
    /// Lowercase; an IPv6 address keeps its brackets.
    host: String,
    port: Option<u16>,
    /// From the first `/` after the host; empty when there is none.
    path: String,
}

impl WebUrl {
    /// `scheme://host[:port]`.
    fn origin(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }

    /// This PC, in its three usual spellings (and nothing that merely starts like one).
    fn is_loopback(&self) -> bool {
        matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    }

    /// The transport rule: https, or plain http to this PC only.
    fn secure_enough(&self) -> bool {
        self.scheme == "https" || self.is_loopback()
    }
}

/// A DNS name or an IPv4 address: dot-separated labels of 1 to 63 letters, digits and `-`
/// (never first nor last), 253 characters at most, no empty label (so no trailing dot).
fn is_host_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// `s` as an absolute `http://` / `https://` address (the scheme in any case), read strictly:
/// a host name (or an IPv6 address in brackets), an optional port from 1 to 65535, and a path
/// of letters, digits and `-._~/+`. Anything else is `None`: other schemes, user info
/// (`user@`), a query or a fragment, backslashes, spaces, control characters, `%` escapes,
/// `.` and `..` segments.
fn parse_web_url(s: &str) -> Option<WebUrl> {
    let (scheme, rest) = s.split_once("://")?;
    let scheme = if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else {
        return None;
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let plain_path = path.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~/+".contains(&b));
    if !plain_path || path.split('/').any(|seg| seg == "." || seg == "..") {
        return None;
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (inner, after) = v6.split_once(']')?;
            if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
                return None;
            }
            (format!("[{}]", inner.to_ascii_lowercase()), after)
        }
        None => {
            let (name, after) = authority.split_at(authority.find(':').unwrap_or(authority.len()));
            if !is_host_name(name) {
                return None;
            }
            (name.to_ascii_lowercase(), after)
        }
    };
    let port = match port {
        "" => None,
        p => {
            let digits = p.strip_prefix(':')?;
            if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let number = digits.parse::<u16>().ok().filter(|&n| n != 0)?;
            Some(number)
        }
    };
    Some(WebUrl { scheme, host, port, path: path.to_string() })
}

/// `s` begins with a scheme and `://` (`https://`, `ftp://`, …).
fn has_scheme(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| {
        scheme.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()) && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
    })
}

/// The one address Wikelink talks to and opens.
pub const SITE: &str = "https://wikelist.net";

/// For developers only: a server on this PC to use instead of [`SITE`] (see [`dev_base`]).
pub const DEV_SERVER_ENV: &str = "WIKELINK_DEV_SERVER";

/// The developer server a value of [`DEV_SERVER_ENV`] names: `Ok(None)` when unset or blank,
/// `Ok(Some(origin))` for an address on this PC (`localhost`, `127.0.0.1`, `[::1]`, any port,
/// `http://` or `https://`; without a scheme it is `http://`), else `Err` with the value as
/// it may be shown (it is then ignored). Pure: the caller reads the environment.
fn dev_base(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(s) = value.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(None) };
    let full = if has_scheme(s) { s.to_string() } else { format!("http://{s}") };
    match parse_web_url(full.trim_end_matches('/')).filter(|u| u.path.is_empty() && u.is_loopback()) {
        Some(u) => Ok(Some(u.origin())),
        None => Err(s.chars().filter(|c| !c.is_control()).take(120).collect()),
    }
}

/// The address every request of this process goes to: [`SITE`], or the developer server
/// [`DEV_SERVER_ENV`] names when that is an address on this PC. Any other value is ignored
/// (one line in the log). Read once per process.
pub fn server_address() -> &'static str {
    static ACTIVE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| {
        let value = std::env::var_os(DEV_SERVER_ENV).map(|v| v.to_string_lossy().into_owned());
        match dev_base(value.as_deref()) {
            Ok(Some(dev)) => dev,
            Ok(None) => SITE.to_string(),
            Err(shown) => {
                say!("{}", trf!("{DEV_SERVER_ENV} ignored: {shown} is not an address on this PC; using {SITE}", "{DEV_SERVER_ENV} ignorada: {shown} não é um endereço deste PC; uso {SITE}"));
                SITE.to_string()
            }
        }
    })
}

/// The developer server in use, if any (the settings window says so).
pub fn dev_server() -> Option<&'static str> {
    Some(server_address()).filter(|a| *a != SITE)
}

/// `active` as a base for requests: `scheme://host[:port]`, when it keeps the transport rule
/// (https, or plain http to this PC only; no user info, path, query or fragment); else why
/// nothing may be sent there.
fn transport_base(active: &str) -> Result<String, String> {
    parse_web_url(active).filter(|u| u.path.is_empty() && u.secure_enough()).map(|u| u.origin()).ok_or_else(|| {
        let shown: String = active.chars().filter(|c| !c.is_control()).take(120).collect();
        trf!("{shown} is not an address Wikelink sends anything to", "{shown} não é um endereço para onde o Wikelink mande algo")
    })
}

/// Whether a session token saved for `owner` (the `server` key of `watcher.json`; empty
/// means [`SITE`]) may be sent to `active`: only when both are the same address. Pure.
fn token_belongs_to(owner: &str, active: &str) -> bool {
    let owner = owner.trim();
    let owner = if owner.is_empty() { Some(SITE.to_string()) } else { parse_web_url(owner.trim_end_matches('/')).filter(|u| u.path.is_empty()).map(|u| u.origin()) };
    owner.as_deref() == Some(active)
}

/// The rule for a saved session token: a token is issued by one server and goes to that one
/// only. `watcher.json` records which (`server`: absent for [`SITE`], the address for a
/// developer server). When a token exists and that address is not `active` (a session from a
/// developer server while Wikelink now talks to the site, the other way round, or one saved
/// by a version that let players type an address), the token is dropped and the player signs
/// in again, instead of sending it somewhere new. A kept token gets `server` = `active`; no
/// token, an empty `server`. Pure: `active` is the caller's.
fn with_token_rule(mut c: AgentConfig, active: &str) -> AgentConfig {
    if !c.token.is_empty() && token_belongs_to(&c.server, active) {
        c.server = active.to_string();
    } else {
        c.token.clear();
        c.server.clear();
    }
    c
}

/// What a link handed to the system is (see [`openable_url`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// The site Wikelink talks to ([`server_address`]).
    Site,
    /// The page of a newer release, as the server reported it.
    Release,
}

/// `url` when it may be handed to the system to open, else `None` (and nothing is offered):
/// an address [`parse_web_url`] reads exactly as given that keeps the transport rule (https,
/// or plain http to this PC). A [`Link::Site`] is a bare origin; a [`Link::Release`] is
/// `https://github.com/` + [`RELEASES_REPO`] + `/releases`, or a page under it. The answer is
/// rebuilt from the parts checked. The sign-in page (`/link`, a path this reading
/// refuses) is built by [`link_page`] on top of the [`Link::Site`] origin.
pub fn openable_url(url: &str, link: Link) -> Option<String> {
    let u = parse_web_url(url)?;
    if !u.secure_enough() {
        return None;
    }
    match link {
        Link::Site => (u.path.is_empty() || u.path == "/").then(|| u.origin()),
        Link::Release => {
            let releases = format!("/{RELEASES_REPO}/releases");
            let under = u.path.strip_prefix(releases.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            (u.scheme == "https" && u.host == "github.com" && u.port.is_none() && under).then(|| format!("{}{}", u.origin(), u.path))
        }
    }
}

/// Is the saved session still accepted by the server? `Ok(None)` = no / expired.
pub fn session_name(cfg: &AgentConfig) -> Result<Option<String>, SendError> {
    Client::new(&cfg.server, &cfg.token).me()
}

/// The terminal's sign-in through the site: print the page and the code, open the browser
/// there, and wait (see [`wait_for_link`]) until the player confirms or refuses it on the site,
/// the code expires, or `stop` is set (Ctrl+C in a running `watch`; before that, Ctrl+C simply
/// ends the program).
fn terminal_sign_in(stop: &AtomicBool) -> anyhow::Result<SignIn> {
    let link = link_start().map_err(|e| anyhow::anyhow!("{e}"))?;
    let page = link_page(&link.code).with_context(|| tr!("unexpected answer from the server", "resposta inesperada do servidor"))?;
    let code = &link.code;
    eprintln!();
    eprintln!("{}", trf!("Sign in on the site: {page}", "Entre pelo site: {page}"));
    eprintln!("{}", trf!("Code: {code} (type it on the site)", "Código: {code} (digite-o no site)"));
    if let Err(e) = open::that_detached(&page) {
        eprintln!("{}", trf!("could not open the browser ({e}); open the address above", "não deu para abrir o navegador ({e}); abra o endereço acima"));
    }
    eprintln!("{}", tr!("Waiting for the confirmation in the browser… (Ctrl+C to cancel)", "Aguardando a confirmação no navegador… (Ctrl+C para cancelar)"));
    match wait_for_link(&link, stop) {
        LinkOutcome::SignedIn(s) => Ok(s),
        other => bail!("{other}"),
    }
}

// ----- setup (interactive) ------------------------------------------------------------

pub struct SetupOptions {
    pub channel: Option<String>,
    /// `Some(x)` sets the automatic data upload without asking.
    pub share_data: Option<bool>,
}

/// Make sure there is a valid session and an install to follow; when `interactive`, a missing
/// session is made through the site (see [`terminal_sign_in`]) and the upload switch is asked.
/// Returns the saved config and the account name.
pub fn ensure_setup(opts: SetupOptions, interactive: bool) -> anyhow::Result<(AgentConfig, String)> {
    // A token saved for another address is gone already (see `with_token_rule`).
    let loaded = load_config().unwrap_or_default();
    let mut cfg = loaded.clone();
    if let Some(c) = &opts.channel {
        cfg.channel = Some(c.to_ascii_uppercase());
    }
    if let Some(s) = opts.share_data {
        cfg.share_data = Some(s);
    }

    let client = Client::new(&cfg.server, &cfg.token);
    let name = match client.me() {
        Ok(Some(n)) => n,
        Ok(None) | Err(SendError::Unauthorized) => {
            if !interactive {
                bail!(tr!("server session no longer valid; open Wikelink to sign in again", "sessão no servidor inválida; abra o Wikelink para entrar de novo"));
            }
            // No Ctrl+C handler is set yet: Ctrl+C ends the program, so this switch stays off.
            let s = terminal_sign_in(&AtomicBool::new(false))?;
            cfg.server = server_address().to_string();
            cfg.token = s.token;
            cfg.handle = s.handle;
            cfg.name = Some(s.name.clone());
            s.name
        }
        Err(e) => {
            let server = server_address();
            bail!(trf!("could not reach {server}: {e:?}", "não foi possível falar com {server}: {e:?}"))
        }
    };
    if cfg.share_data.is_none() {
        cfg.share_data = Some(if interactive {
            eprintln!();
            match crate::i18n::lang() {
                crate::i18n::Lang::En => {
                    eprintln!("Missions from the Game.log are sent whenever Wikelink is running; that does not change.");
                    eprintln!("Besides that, it can send the game data (contracts and images) when a new version comes out:");
                    eprintln!("once an hour it compares the installed versions with the server's and, only when a new");
                    eprintln!("version shows up, extracts and sends it (a few seconds, with the game closed).");
                }
                crate::i18n::Lang::Pt => {
                    eprintln!("As missões do Game.log são enviadas sempre que o Wikelink está rodando; isso não muda.");
                    eprintln!("Além disso, ele pode enviar os dados do jogo (contratos e imagens) quando sair uma versão nova:");
                    eprintln!("uma vez por hora compara as versões instaladas com as do servidor e, só se aparecer uma versão");
                    eprintln!("nova, extrai e envia (uns segundos, com o jogo fechado).");
                }
            }
            yes_no(tr!("Also send the game data when a new version comes out?", "Enviar também os dados do jogo quando sair uma versão nova?"), true)?
        } else {
            false
        });
    }
    // The sign-in is this function's; the channel and the upload switch only when it set them
    // (the settings window may have changed them while the prompts above waited).
    let saved = update_config(&cfg, |d| {
        copy_sign_in(d, &cfg);
        if cfg.channel != loaded.channel {
            d.channel = cfg.channel.clone();
        }
        if cfg.share_data != loaded.share_data {
            d.share_data = cfg.share_data;
        }
    })?;
    Ok((saved, name))
}

/// Pick the install to follow (channel from the config, else LIVE, else the first one).
fn pick_install(cfg: &AgentConfig) -> anyhow::Result<wikelo_install::InstallInfo> {
    let installs: Vec<_> = wikelo_install::discover_installs(&[]).into_iter().filter(|i| i.game_log_path.exists() || i.has_p4k()).collect();
    match &cfg.channel {
        Some(c) => installs.iter().find(|i| i.channel.eq_ignore_ascii_case(c)),
        None => installs.iter().find(|i| i.channel == "LIVE").or(installs.first()),
    }
    .cloned()
    .with_context(|| match &cfg.channel {
        Some(c) => trf!("no Star Citizen install found for channel {c}; use --channel or SC_INSTALL_ROOTS", "nenhuma instalação do Star Citizen encontrada para o canal {c}; use --channel ou SC_INSTALL_ROOTS"),
        None => tr!("no Star Citizen install found; use --channel or SC_INSTALL_ROOTS", "nenhuma instalação do Star Citizen encontrada; use --channel ou SC_INSTALL_ROOTS").to_string(),
    })
}

// ----- automatic game data ------------------------------------------------------------

fn game_active(install: &wikelo_install::InstallInfo) -> bool {
    std::fs::metadata(&install.game_log_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < GAME_ACTIVE_WINDOW)
}

fn clock() -> String {
    let now = now_local();
    format!("{:02}:{:02}", now.hour(), now.minute())
}

/// Compare installed builds with the server's and upload a bundle for each one the server
/// lacks, or has only from an older extractor (see [`upload_reason`]). The bundle is not
/// signed. Skips while the game runs (it is using the files and the CPU). Returns a
/// one-line summary.
fn data_check(client: &Client, name: &str, handle: &str) -> String {
    let known = match client.known_datasets() {
        Ok(k) => k,
        Err(e) => {
            say!("{}", trf!("could not ask the server for its versions ({e:?}); trying again in 1 h", "não deu para consultar as versões do servidor ({e:?}); tento de novo em 1 h"));
            let at = clock();
            return trf!("{at}: server did not answer", "{at}: servidor não respondeu");
        }
    };
    let installs: Vec<_> = wikelo_install::discover_installs(&[]).into_iter().filter(|i| i.has_p4k()).collect();
    let mut lines: Vec<String> = Vec::new();
    for install in installs {
        let channel = install.channel.to_ascii_uppercase();
        let version = install.manifest.version.clone();
        let Some(reason) = upload_reason(&known, &channel, &version, PKG_VERSION, wikelo_core::EXTRACTOR_REVISION) else { continue };
        let why = match reason {
            UploadReason::New => trf!("{channel} {version} is not on the server", "{channel} {version} não existe no servidor"),
            UploadReason::OlderExtractor(Some(old)) => trf!(
                "{channel} {version} is on the server, but came from extractor {old} (this one is {PKG_VERSION})",
                "{channel} {version} está no servidor, mas veio do extrator {old} (este é o {PKG_VERSION})"
            ),
            UploadReason::OlderExtractor(None) => trf!(
                "{channel} {version} is on the server, but came from an older extractor (this one is {PKG_VERSION})",
                "{channel} {version} está no servidor, mas veio de um extrator antigo (este é o {PKG_VERSION})"
            ),
        };
        if game_active(&install) {
            say!("{}", trf!("{why}, and the game is open; sending later", "{why}, e o jogo está aberto; envio depois"));
            lines.push(trf!("{channel} {version}: upload postponed, game open", "{channel} {version}: envio adiado, jogo aberto"));
            continue;
        }
        say!("{}", trf!("{why}: extracting the game data…", "{why}: extraindo os dados do jogo…"));
        let quiet = |_p: wikelo_watcher::ExtractProgress| {};
        match crate::bundling::make_bundle(&install, name, handle, &quiet) {
            Ok(built) => match client.upload_bundle(&built.bytes) {
                Ok(r) => {
                    let outcome = match r.status.as_str() {
                        "activated" => tr!("already in use", "já em uso").to_string(),
                        "pending" => tr!("received, not in use yet", "recebido, ainda não em uso").to_string(),
                        "stored" => tr!("received; the build in use is newer", "recebido; o build em uso é mais novo").to_string(),
                        "invalid" => tr!("not accepted", "não aceito").to_string(),
                        other => server_text(other),
                    };
                    let (label, contracts, mb, hash) = (&built.label, built.contracts, built.bytes.len() as f64 / 1048576.0, &built.content_hash);
                    say!(
                        "{}",
                        trf!(
                            "{channel} {label}: {contracts} contracts, {mb:.1} MB sent ({outcome}) · hash {hash}",
                            "{channel} {label}: {contracts} contratos, {mb:.1} MB enviados ({outcome}) · hash {hash}"
                        )
                    );
                    lines.push(trf!("{channel} {label} sent: {outcome}", "{channel} {label} enviado: {outcome}"));
                }
                Err(SendError::Rejected(m)) => {
                    say!("{}", trf!("the server refused the data of {channel}: {m}", "o servidor recusou os dados de {channel}: {m}"));
                    lines.push(trf!("{channel}: refused ({m})", "{channel}: recusado ({m})"));
                }
                Err(e) => {
                    say!("{}", trf!("could not send the data of {channel}: {e:?}", "falha ao enviar os dados de {channel}: {e:?}"));
                    lines.push(trf!("{channel}: upload failed", "{channel}: falha ao enviar"));
                }
            },
            Err(e) => {
                say!("{}", trf!("could not extract {channel}: {e:#}", "falha ao extrair {channel}: {e:#}"));
                lines.push(trf!("{channel}: extraction failed", "{channel}: falha ao extrair"));
            }
        }
    }
    let at = clock();
    if lines.is_empty() {
        trf!("{at}: server versions up to date", "{at}: versões em dia no servidor")
    } else {
        let all = lines.join(" · ");
        format!("{at}: {all}")
    }
}

// ----- blueprints in old logs ---------------------------------------------------------

/// The channel whose blueprints count: the test channels are separate environments, where a
/// blueprint received says nothing about the live one, so their old logs are not read.
const BACKLOG_CHANNEL: &str = "LIVE";
/// How often the `logbackups\` folders are looked at for old logs not read yet (the first
/// check runs when the watcher starts).
const BACKLOG_EVERY: Duration = Duration::from_secs(3600);
/// Most old logs read in one pass over one folder, the newest first; the others wait for the
/// next pass, [`BACKLOG_PAUSE`] later.
const BACKLOG_FILES_PER_PASS: usize = 200;
/// The pause between two passes of one check while old logs are still waiting, so a first
/// run over a thousand files is spread out instead of read in one go.
const BACKLOG_PAUSE: Duration = Duration::from_secs(5);
/// Most events in one request of a pass.
const BACKLOG_BATCH: usize = 200;

/// `<data dir>/logbackups-read.json`: the old logs already read (see [`BacklogState`]).
fn backlog_state_path() -> PathBuf {
    data_dir().join("logbackups-read.json")
}

/// The old logs already read: their blueprints were accepted, or got a final refusal (see
/// [`backlog_pass`]). Per folder (under the key [`backlog_folders`] gives it: the channel for
/// the followed installation's), file name → its size when read. A file whose size changed
/// is read again. It belongs to one account: signed in as another one, every file is read
/// again for that one.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BacklogState {
    #[serde(default)]
    account: String,
    #[serde(default)]
    channels: BTreeMap<String, BTreeMap<String, u64>>,
}

impl BacklogState {
    /// The list saved at `p` for `account`; empty when it is missing, unreadable or another
    /// account's.
    fn load(p: &Path, account: &str) -> Self {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str::<BacklogState>(&s).ok())
            .filter(|s| s.account == account)
            .unwrap_or_else(|| BacklogState { account: account.to_string(), ..Default::default() })
    }

    /// Written like `watcher.json`: a temporary file in the same folder renamed over the old
    /// one, so a quit halfway never leaves half a file. Only the watcher writes this one.
    fn save(&self, p: &Path) -> anyhow::Result<()> {
        write_config_text(p, &serde_json::to_string_pretty(self)?)
    }

    fn is_read(&self, channel: &str, name: &str, size: u64) -> bool {
        self.channels.get(channel).and_then(|files| files.get(name)) == Some(&size)
    }

    fn mark_read(&mut self, channel: &str, name: &str, size: u64) {
        self.channels.entry(channel.to_string()).or_default().insert(name.to_string(), size);
    }

    /// Forget the files of `channel` no longer in its folder, so the list stays small.
    /// Whether anything was forgotten.
    fn keep_only(&mut self, channel: &str, present: &BTreeSet<String>) -> bool {
        let Some(files) = self.channels.get_mut(channel) else { return false };
        let before = files.len();
        files.retain(|name, _| present.contains(name));
        files.len() != before
    }
}

/// What one pass over the old logs did.
#[derive(Debug, Default, PartialEq, Eq)]
struct BacklogPass {
    /// Files read whose events the server accepted (files without blueprints included).
    read: usize,
    /// Blueprints sent.
    blueprints: usize,
    /// Files with a batch that got [`BacklogSendError::Refused`]: counted as read, never
    /// sent again.
    refused: usize,
    /// Files that could not be read, or whose events were not accepted; tried again next pass.
    failed: usize,
    /// Files not read yet left for the next pass by [`BACKLOG_FILES_PER_PASS`].
    left: usize,
}

/// Why a batch of events read from an old log did not go out.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BacklogSendError {
    /// An answer this client treats as a final refusal of the request (see
    /// [`is_final_refusal`]): the file counts as read and the pass goes on.
    Refused(String),
    /// No answer, a timeout, or an answer this client tries again later (401, 408, 409, 429, 5xx,
    /// the watcher paused from the site): the pass ends and the file stays unread.
    Later(String),
}

/// Whether this client treats an HTTP status as a final refusal of the request: a 4xx other
/// than 401 (the session), 408 (a timeout), 409 (no data for the channel yet) and 429 (too many
/// requests). `None` (no answer)
/// is not one.
fn is_final_refusal(status: Option<u16>) -> bool {
    matches!(status, Some(s) if (400..500).contains(&s) && !matches!(s, 401 | 408 | 409 | 429))
}

/// Sends one batch of events read from an old log.
type BacklogSend<'a> = dyn FnMut(&[(bool, TrackerOutput)]) -> Result<(), BacklogSendError> + 'a;

/// One pass over `dir` (a channel's `logbackups\`): every `*.log` not in the list at
/// `state_path` with its current size, the newest first and at most
/// [`BACKLOG_FILES_PER_PASS`] of them, is read for its blueprints, which go to `send` in
/// batches of [`BACKLOG_BATCH`] (an event repeated in the same file, same timestamp and same
/// name, goes once). A file joins the list once `send` accepted all its events, or once
/// `send` gave [`BacklogSendError::Refused`] for one of its batches (that file is set aside
/// and the pass goes on with the next one). [`BacklogSendError::Later`] ends the pass (the
/// server is out of reach; the rest waits for the next one); a file that cannot be read is
/// only skipped. Each failure is one log line.
///
/// Nothing in `dir` is written: the list lives at `state_path`, in the data folder.
fn backlog_pass(
    state_path: &Path,
    account: &str,
    channel: &str,
    dir: &Path,
    parser: &wikelo_log::LineParser,
    stop: &AtomicBool,
    send: &mut BacklogSend<'_>,
) -> BacklogPass {
    let mut state = BacklogState::load(state_path, account);
    let mut files = wikelo_log::list_logbackups(dir);
    // `list_logbackups` gives them oldest first.
    files.reverse();
    let present: BTreeSet<String> = files.iter().filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string)).collect();
    // An empty listing may be a folder that cannot be read right now: forget nothing then.
    let mut dirty = !present.is_empty() && state.keep_only(channel, &present);
    let mut pass = BacklogPass::default();
    let mut tried = 0usize;
    let save = |state: &BacklogState| {
        if let Err(e) = state.save(state_path) {
            say!("{}", trf!("could not save the list of old logs read ({e:#})", "não deu para salvar a lista de logs antigos lidos ({e:#})"));
        }
    };
    for path in &files {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(e) => {
                say!("{}", trf!("could not read the old log {name} ({e}); trying again in 1 h", "não deu para ler o log antigo {name} ({e}); tento de novo em 1 h"));
                pass.failed += 1;
                continue;
            }
        };
        if state.is_read(channel, name, size) {
            continue;
        }
        if tried >= BACKLOG_FILES_PER_PASS {
            pass.left += 1;
            continue;
        }
        tried += 1;
        let scan = match wikelo_log::blueprints_in(path, parser) {
            Ok(s) => s,
            Err(e) => {
                say!("{}", trf!("could not read the old log {name} ({e}); trying again in 1 h", "não deu para ler o log antigo {name} ({e}); tento de novo em 1 h"));
                pass.failed += 1;
                continue;
            }
        };
        if scan.truncated {
            let mib = wikelo_log::BACKLOG_MAX_BYTES / (1024 * 1024);
            say!("{}", trf!("old log {name} is over {mib} MiB: only its start was read", "o log antigo {name} passa de {mib} MiB: só o começo foi lido"));
        }
        if scan.capped {
            let most = wikelo_log::BACKLOG_MAX_BLUEPRINTS;
            say!("{}", trf!("old log {name} has over {most} blueprints: only the first {most} were read", "o log antigo {name} tem mais de {most} blueprints: só os {most} primeiros foram lidos"));
        }
        // The same event twice in one file (same timestamp, same name) goes once.
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        // Old sessions are history: sent as replayed events, like the start of `Game.log`.
        let events: Vec<(bool, TrackerOutput)> = scan
            .blueprints
            .into_iter()
            .filter(|b| seen.insert((b.at.clone(), b.name.clone())))
            .map(|b| (true, TrackerOutput::BlueprintReceived { at: b.at, name: b.name, mission_id: None }))
            .collect();
        let mut sent = Ok(());
        let mut accepted = 0usize;
        for chunk in events.chunks(BACKLOG_BATCH) {
            sent = send(chunk);
            if sent.is_err() {
                break;
            }
            accepted += chunk.len();
        }
        match sent {
            Ok(()) => {
                pass.read += 1;
                pass.blueprints += events.len();
            }
            Err(BacklogSendError::Refused(e)) => {
                // A final refusal: the file is set aside as read, so it never holds up the
                // older files nor is sent again every hour.
                say!("{}", trf!("the server refused the blueprints of the old log {name} ({e}); it is not sent again", "o servidor recusou os blueprints do log antigo {name} ({e}); ele não é enviado de novo"));
                pass.refused += 1;
                pass.blueprints += accepted;
            }
            Err(BacklogSendError::Later(e)) => {
                say!("{}", trf!("could not send the blueprints of the old logs ({e}); trying again in 1 h", "não deu para enviar os blueprints dos logs antigos ({e}); tento de novo em 1 h"));
                pass.failed += 1;
                break;
            }
        }
        state.mark_read(channel, name, size);
        dirty = true;
        // Saved right after every file whose events went out, so a quit never sends them twice.
        if !events.is_empty() {
            save(&state);
            dirty = false;
        }
    }
    if dirty {
        save(&state);
    }
    pass
}

/// What one answer to a batch of old-log events means for [`backlog_pass`]: accepted only
/// when the server takes events (not while the watcher is paused on the site); a final
/// refusal of the request ([`is_final_refusal`]) is [`BacklogSendError::Refused`]; anything
/// else is [`BacklogSendError::Later`]. Pure.
fn backlog_verdict(answer: Result<Ack, (Option<u16>, SendError)>) -> Result<(), BacklogSendError> {
    match answer {
        Ok(ack) if ack.enabled => Ok(()),
        Ok(_) => Err(BacklogSendError::Later(tr!("paused from the site", "pausado pelo site").to_string())),
        Err((status, e)) if is_final_refusal(status) => {
            let code = status.unwrap_or_default();
            Err(BacklogSendError::Refused(format!("HTTP {code}: {e:?}")))
        }
        Err((_, e)) => Err(BacklogSendError::Later(format!("{e:?}"))),
    }
}

/// The `logbackups\` folders a check reads, each with the key its files are listed under in
/// [`BacklogState`]: the followed installation's first, under its channel (the key it always
/// had), then the folder of every other installation found (`others`: channel and folder),
/// under `channel|folder` in lower case. Two installations of one channel (the same channel
/// on two drives) are two folders with two keys; the same folder is listed once.
fn backlog_folders(channel: &str, followed: &Path, others: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    let same = |a: &Path, b: &Path| a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase();
    let mut out = vec![(channel.to_string(), followed.to_path_buf())];
    for (ch, dir) in others {
        if out.iter().any(|(_, d)| same(d, dir)) {
            continue;
        }
        out.push((format!("{ch}|{}", dir.to_string_lossy().to_lowercase()), dir.clone()));
    }
    out
}

/// The sum of the passes of one round over every folder.
fn backlog_sum(passes: &[BacklogPass]) -> BacklogPass {
    let mut sum = BacklogPass::default();
    for p in passes {
        sum.read += p.read;
        sum.blueprints += p.blueprints;
        sum.refused += p.refused;
        sum.failed += p.failed;
        sum.left += p.left;
    }
    sum
}

/// Whether a check goes on with one more round after `round`: files are still waiting, the
/// round took some of them, and nothing failed (a failure waits for the next hourly check).
fn backlog_goes_on(round: &BacklogPass) -> bool {
    round.left > 0 && round.failed == 0 && round.read + round.refused > 0
}

/// One check of the old logs: [`backlog_pass`] over each of `folders` (see
/// [`backlog_folders`]), round after round with [`BACKLOG_PAUSE`] between two while files are
/// still waiting ([`backlog_goes_on`]). Everything goes out through `client` with the same
/// request as the live events, under the followed `channel` (see [`backlog_verdict`] for what
/// an answer means).
fn backlog_check(client: &Client, channel: &str, host: &str, share: bool, account: &str, folders: &[(String, PathBuf)], stop: &AtomicBool) {
    let parser = wikelo_log::LineParser::new();
    let mut send = |chunk: &[(bool, TrackerOutput)]| backlog_verdict(client.send_with_status(channel, host, share, chunk));
    loop {
        let mut passes = Vec::with_capacity(folders.len());
        for (key, dir) in folders {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let pass = backlog_pass(&backlog_state_path(), account, key, dir, &parser, stop, &mut send);
            // The server is out of reach: the other folders would fail the same way.
            let failed = pass.failed > 0;
            passes.push(pass);
            if failed {
                break;
            }
        }
        let round = backlog_sum(&passes);
        let BacklogPass { read, blueprints, refused, failed, left } = round;
        if read > 0 || refused > 0 || left > 0 || failed > 0 {
            let n = folders.len();
            say!(
                "{}",
                trf!(
                    "old logs ({n} folders): {read} read, {blueprints} blueprints sent, {refused} refused, {failed} failed, {left} still to read",
                    "logs antigos ({n} pastas): {read} lidos, {blueprints} blueprints enviados, {refused} recusados, {failed} com falha, {left} ainda por ler"
                )
            );
        }
        if !backlog_goes_on(&round) {
            return;
        }
        // In short steps, so quitting never waits for the pause.
        let until = Instant::now() + BACKLOG_PAUSE;
        while Instant::now() < until {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

// ----- main loop --------------------------------------------------------------------

fn describe(out: &TrackerOutput) -> Option<String> {
    Some(match out {
        TrackerOutput::SessionStart { .. } => tr!("new game session", "nova sessão do jogo").into(),
        TrackerOutput::Identity { nickname, .. } => trf!("player: {nickname}", "jogador: {nickname}"),
        TrackerOutput::MissionLinked { mission } => {
            let name = mission.title.clone().or(mission.debug_name.clone()).unwrap_or_default();
            trf!("→ mission accepted: {name}", "→ missão aceita: {name}")
        }
        TrackerOutput::ObjectiveUpdated { mission, state, item_id, .. } if state.ends_with("COMPLETED") => {
            let name = mission.title.clone().or(mission.debug_name.clone()).unwrap_or_default();
            let item = item_id.clone().unwrap_or_default();
            trf!("  ✓ objective completed in {name} ({item})", "  ✓ objetivo concluído em {name} ({item})")
        }
        TrackerOutput::MissionEnded { mission } => {
            let name = mission.title.clone().or(mission.debug_name.clone()).unwrap_or_default();
            let how = mission.completion.as_ref().map(|c| format!("{c:?}")).unwrap_or_default();
            let whose = if mission.shared_by.is_some() { tr!(" (another player's)", " (de outro jogador)") } else { "" };
            if mission.is_wikelo() { format!("✔ Wikelo {how}: {name}{whose}") } else { trf!("✔ mission {how}: {name}{whose}", "✔ missão {how}: {name}{whose}") }
        }
        TrackerOutput::BlueprintReceived { name, .. } => trf!("blueprint received: {name}", "blueprint recebido: {name}"),
        TrackerOutput::AtEmporium { location_class, .. } => trf!("at Wikelo: {location_class}", "no Wikelo: {location_class}"),
        _ => return None,
    })
}

#[cfg(windows)]
fn lower_priority() {
    use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass};
    // SAFETY: plain Win32 calls on our own process handle.
    unsafe {
        SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

#[cfg(not(windows))]
fn lower_priority() {}

pub struct WatchOptions {
    pub setup: SetupOptions,
    pub no_replay: bool,
    /// Detached background mode: pid file, stop file, lower priority, no prompts.
    pub daemon: bool,
}

/// Run the watcher until Ctrl+C or the stop file appears.
pub fn run(opts: WatchOptions) -> anyhow::Result<()> {
    let (mut cfg, name) = ensure_setup(opts.setup, !opts.daemon)?;
    let install = pick_install(&cfg)?;
    cfg.channel = Some(install.channel.clone());
    update_config(&cfg, |d| d.channel = cfg.channel.clone())?;

    if opts.daemon {
        lower_priority();
        DAEMON.store(true, Ordering::Relaxed);
        // The default hook prints the panic; this one dates it and keeps it in the log's format.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            say!("panic: {info}");
        }));
        std::fs::write(pid_path(), std::process::id().to_string())?;
        let _ = std::fs::remove_file(stop_path());
    }
    // The notification-area icon, for the background process only (a console `watch` has its
    // window). Kept to the end of this function: dropping it removes the icon, on every way out.
    let _tray = if opts.daemon { crate::tray::start() } else { None };
    let _ = std::fs::remove_file(check_path());
    say!("Wikelink {PKG_VERSION} ({GIT_COMMIT})");
    let server = server_address();
    say!("{}", trf!("connected to {server} as {name}", "conectado a {server} como {name}"));
    let (channel, log) = (&install.channel, install.game_log_path.display());
    say!("{}", trf!("following {channel} in {log}", "acompanhando {channel} em {log}"));
    let upload = if cfg.share_data == Some(true) { tr!("on (checked every hour)", "ativado (checagem a cada hora)") } else { tr!("off", "desativado") };
    say!("{}", trf!("game data upload when a new version comes out: {upload}", "envio dos dados do jogo quando sair uma versão nova: {upload}"));
    if !opts.daemon {
        eprintln!("{}", tr!("Leave this window open while you play. Ctrl+C to quit.\n", "Deixe esta janela aberta enquanto joga. Ctrl+C para sair.\n"));
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = stop.clone();
        let _ = ctrlc::set_handler(move || s.store(true, Ordering::Relaxed));
    }
    let host = pc_name();
    let channel = install.channel.clone();
    let mut share = cfg.share_data == Some(true);
    let author_name = cfg.display_name();
    let author_handle = cfg.handle.clone();
    let mut client = Client::new(&cfg.server, &cfg.token);
    let mut tailer = Tailer::new(TailerConfig { path: install.game_log_path.clone(), from_start: !opts.no_replay, poll_interval: POLL });

    let mut status = DaemonStatus {
        pid: std::process::id(),
        started_at: now_rfc3339(),
        server: server.to_string(),
        channel: channel.clone(),
        share_data: share,
        version: PKG_VERSION.to_string(),
        ..Default::default()
    };
    status.write();

    let mut queue: Vec<(bool, TrackerOutput)> = Vec::new();
    // Cleared for the rest of the run when the server turns down a batch with `AtLocation`
    // events in it (a server older than this watcher): they are not queued any more.
    let mut server_takes_locations = true;
    let mut last_try = Instant::now() - Duration::from_secs(3600);
    let mut backoff = Duration::from_secs(0);
    let mut last_ok = Instant::now() - HEARTBEAT;
    let mut replayed = 0usize;
    let mut announced_replay = false;
    let mut paused = false;
    let mut last_data_check: Option<Instant> = None;
    let mut last_update_check: Option<Instant> = None;
    let mut last_backlog: Option<Instant> = None;
    let backlog_busy = Arc::new(AtomicBool::new(false));
    let mut last_rotate = Instant::now();
    let checking: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let check_result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut cfg_mtime = config_mtime();
    let stop_file = stop_path();
    let check_file = check_path();
    let stop_loop = stop.clone();
    // The background watcher keeps the overlay up (the settings window can switch it off).
    let mut overlay_wanted = opts.daemon && cfg.overlay_with_game();
    let mut keeper = OverlayKeeper::default();
    let mut last_keep: Option<Instant> = None;

    tailer.run_batched(stop_loop.clone(), &mut |batch| {
        let mut dirty = false;
        if opts.daemon && stop_file.exists() {
            stop_loop.store(true, Ordering::Relaxed);
        }
        // The window rewrote watcher.json: pick up the data-upload and overlay switches
        // without a restart.
        let mt = config_mtime();
        if mt != cfg_mtime {
            cfg_mtime = mt;
            if let Some(c) = load_config() {
                overlay_wanted = opts.daemon && c.overlay_with_game();
                let s = c.share_data == Some(true);
                if s != share {
                    share = s;
                    status.share_data = s;
                    dirty = true;
                    say!("{}", if share { tr!("game data upload on", "envio dos dados do jogo ativado") } else { tr!("game data upload off", "envio dos dados do jogo desativado") });
                    if share {
                        last_data_check = None;
                    }
                }
            }
        }
        for (replay, out) in &batch {
            if *replay {
                replayed += 1;
            } else if let Some(line) = describe(out) {
                say!("{line}");
                status.last_event = Some(format!("{} {line}", time_now()));
                dirty = true;
            }
        }
        if !announced_replay && replayed > 0 && batch.iter().all(|(r, _)| !*r) {
            say!("{}", trf!("current session's history read ({replayed} events)", "histórico da sessão atual lido ({replayed} eventos)"));
            announced_replay = true;
        }
        if !paused {
            queue.extend(batch.into_iter().filter(|(_, e)| server_takes_locations || !is_location_event(e)));
        }
        // Not once the watcher is stopping: closing it closes the overlay first.
        if opts.daemon && !stop_loop.load(Ordering::Relaxed) && last_keep.is_none_or(|t| t.elapsed() >= OVERLAY_KEEP_EVERY) {
            last_keep = Some(Instant::now());
            if let Some(problem) = keep_overlay(&mut keeper, overlay_wanted) {
                say!("{problem}");
            }
        }

        // Hourly (first check right after start), or now when the window asks for it.
        let forced = check_file.exists();
        if forced {
            let _ = std::fs::remove_file(&check_file);
        }
        if forced || (share && last_data_check.is_none_or(|t| t.elapsed() > DATA_CHECK_EVERY)) {
            last_data_check = Some(Instant::now());
            if let Ok(mut busy) = checking.try_lock()
                && !*busy
            {
                *busy = true;
                drop(busy);
                let c = client.clone();
                let (n, h) = (author_name.clone(), author_handle.clone());
                let flag = checking.clone();
                let result = check_result.clone();
                std::thread::spawn(move || {
                    // A panic inside the extractor (a new game build, say) must not leave the
                    // check marked as running forever, nor take the daemon down with it.
                    let summary = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data_check(&c, &n, &h))).unwrap_or_else(|p| {
                        let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                        say!("{}", trf!("internal failure in the data check: {msg}", "falha interna na checagem de dados: {msg}"));
                        let at = clock();
                        trf!("{at}: internal failure in the check ({msg})", "{at}: falha interna na checagem ({msg})")
                    });
                    *result.lock().unwrap() = Some(summary);
                    *flag.lock().unwrap() = false;
                });
            }
        }
        if let Some(summary) = check_result.lock().unwrap().take() {
            status.data_check = Some(summary);
            dirty = true;
        }

        // Blueprints in the old logs of `logbackups\`, of this installation and of every other
        // LIVE one found on this PC: right after start, then hourly, and only while the
        // followed channel is LIVE (see `BACKLOG_CHANNEL`). In a thread of its own, so reading
        // many files never holds up the live tailing.
        if channel.eq_ignore_ascii_case(BACKLOG_CHANNEL) && last_backlog.is_none_or(|t| t.elapsed() > BACKLOG_EVERY) && !backlog_busy.swap(true, Ordering::Relaxed) {
            last_backlog = Some(Instant::now());
            let c = client.clone();
            let (ch, h, account, dir) = (channel.clone(), host.clone(), cfg.handle.clone(), install.logbackups_dir.clone());
            let (busy, halt) = (backlog_busy.clone(), stop_loop.clone());
            std::thread::spawn(move || {
                // A panic in there must not leave the pass marked as running forever.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let others: Vec<(String, PathBuf)> = wikelo_install::discover_installs(&[])
                        .into_iter()
                        .filter(|i| i.channel.eq_ignore_ascii_case(BACKLOG_CHANNEL) && i.logbackups_dir.is_dir())
                        .map(|i| (i.channel, i.logbackups_dir))
                        .collect();
                    backlog_check(&c, &ch, &h, share, &account, &backlog_folders(&ch, &dir, &others), &halt)
                }));
                busy.store(false, Ordering::Relaxed);
            });
        }

        // Once a day (and right after start): is there a newer release? The server knows.
        if last_update_check.is_none_or(|t| t.elapsed() > UPDATE_CHECK_EVERY) {
            last_update_check = Some(Instant::now());
            if let Ok(Some(r)) = client.latest_release()
                && wikelo_core::version::is_newer(&r.version, PKG_VERSION)
            {
                if status.latest_version.as_deref() != Some(r.version.as_str()) {
                    let (v, u) = (&r.version, &r.url);
                    say!("{}", trf!("Wikelink {v} available: {u}", "versão {v} do Wikelink disponível: {u}"));
                }
                status.latest_version = Some(r.version);
                status.update_url = Some(r.url);
                dirty = true;
            }
        }
        if opts.daemon && last_rotate.elapsed() > ROTATE_EVERY {
            last_rotate = Instant::now();
            let _ = rotate_log_if_big(&log_path(), LOG_MAX);
        }

        let heartbeat_due = last_ok.elapsed() > HEARTBEAT;
        if (queue.is_empty() && !heartbeat_due) || last_try.elapsed() < backoff {
            if dirty {
                status.write();
            }
            return;
        }
        last_try = Instant::now();
        let mut sent = client.send(&channel, &host, share, &queue);
        if matches!(sent, Err(SendError::Unprocessable(_))) && queue.iter().any(|(_, e)| is_location_event(e)) {
            // An older server: drop the location events (and stop queueing them) and send the
            // rest right away, so mission events never wait behind a kind it cannot read.
            queue.retain(|(_, e)| !is_location_event(e));
            if server_takes_locations {
                server_takes_locations = false;
                say!("{}", tr!("the server does not know location events (older version); sending only the others", "o servidor não conhece eventos de local (versão anterior); envio só os demais"));
            }
            sent = client.send(&channel, &host, share, &queue);
        }
        match sent {
            Ok(ack) => {
                queue.clear();
                backoff = Duration::from_secs(0);
                last_ok = Instant::now();
                status.last_ok = Some(now_rfc3339());
                status.last_error = None;
                if ack.enabled == paused {
                    paused = !ack.enabled;
                    status.paused = paused;
                    if paused {
                        say!("{}", tr!("paused from the site (Data → Watcher); events are ignored until it is turned back on", "pausado pelo site (Dados → Watcher); os eventos são ignorados até reativar"));
                    } else {
                        say!("{}", tr!("turned back on from the site", "reativado pelo site"));
                    }
                }
            }
            Err(SendError::Unauthorized) => {
                if opts.daemon {
                    say!("{}", tr!("the server session expired; open Wikelink to sign in again", "a sessão no servidor expirou; abra o Wikelink para entrar de novo"));
                    status.last_error = Some(tr!("session expired: sign in again", "sessão expirada: entre de novo").into());
                    stop_loop.store(true, Ordering::Relaxed);
                } else {
                    say!("{}", tr!("the server session expired; sign in again on the site.", "a sessão no servidor expirou; entre de novo pelo site."));
                    // Ctrl+C sets `stop_loop` here (its handler is set): it cancels the wait too.
                    match terminal_sign_in(&stop_loop) {
                        Ok(s) => {
                            cfg.server = server.to_string();
                            cfg.token = s.token;
                            cfg.handle = s.handle;
                            cfg.name = Some(s.name);
                            let _ = update_config(&cfg, |d| copy_sign_in(d, &cfg));
                            cfg_mtime = config_mtime();
                            client = Client::new(&cfg.server, &cfg.token);
                        }
                        Err(e) => {
                            say!("{}", trf!("sign-in failed: {e}", "login falhou: {e}"));
                            backoff = Duration::from_secs(30);
                        }
                    }
                }
            }
            Err(SendError::NoData) => {
                say!("{}", trf!("the server has no data for channel {channel} yet; trying again in 20 s", "o servidor ainda não tem os dados do canal {channel}; tento de novo em 20 s"));
                status.last_error = Some(trf!("the server has no data for channel {channel} yet", "o servidor ainda não tem os dados do canal {channel}"));
                backoff = Duration::from_secs(20);
            }
            Err(SendError::Rejected(m) | SendError::Unprocessable(m)) => {
                // The server answered and said no: its own words, and a growing wait.
                backoff = (if backoff.is_zero() { Duration::from_secs(2) } else { backoff * 2 }).min(Duration::from_secs(60));
                let secs = backoff.as_secs();
                say!("{}", trf!("the server refused the upload ({m}); trying again in {secs} s", "o servidor recusou o envio ({m}); nova tentativa em {secs} s"));
                status.last_error = Some(trf!("the server refused: {m}", "o servidor recusou: {m}"));
            }
            Err(SendError::Unusable(m)) => {
                // Nothing was sent, and nothing would be on another try.
                say!("{m}");
                status.last_error = Some(m);
                stop_loop.store(true, Ordering::Relaxed);
            }
            Err(SendError::Other(m)) => {
                backoff = (if backoff.is_zero() { Duration::from_secs(2) } else { backoff * 2 }).min(Duration::from_secs(60));
                let secs = backoff.as_secs();
                say!("{}", trf!("upload failed ({m}); trying again in {secs} s", "falha ao enviar ({m}); nova tentativa em {secs} s"));
                status.last_error = Some(trf!("no contact with the server ({m})", "sem contato com o servidor ({m})"));
            }
        }
        status.write();
        if queue.len() > 5000 {
            let drop_n = queue.len() - 5000;
            queue.drain(..drop_n);
        }
    });

    client.bye();
    let _ = std::fs::remove_file(status_path());
    if opts.daemon {
        let _ = std::fs::remove_file(pid_path());
        let _ = std::fs::remove_file(stop_path());
    }
    say!("{}", tr!("Wikelink stopped", "Wikelink encerrado"));
    Ok(())
}

// ----- background process control -----------------------------------------------------

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Pid of the running background watcher, if any.
pub fn running_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_path()).ok()?.trim().parse().ok()?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(pid_path());
        None
    }
}

/// Spawn `watch --daemon` detached from this console.
pub fn start_background() -> anyhow::Result<u32> {
    if let Some(pid) = running_pid() {
        bail!(trf!("Wikelink is already running in the background (pid {pid})", "o Wikelink já está rodando em segundo plano (pid {pid})"));
    }
    // Every way this program starts its watcher passes here: an autostart entry left by
    // `wikelo-watcher.exe` moves to this executable now (see [`migrate_autostart`]). Not a
    // reason to stay stopped when it cannot.
    if let Err(e) = migrate_autostart() {
        eprintln!("{e:#}");
    }
    let exe = std::env::current_exe()?;
    let log = log_path();
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d)?;
    }
    // The previous run's log survives as `watcher.log.1` instead of being thrown away.
    if std::fs::metadata(&log).map(|m| m.len() > LOG_MAX).unwrap_or(false) {
        let old = log.with_extension("log.1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&log, &old);
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let err = out.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["watch", "--daemon"]).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    // Our own std handles must not leak into the daemon: a script that captured this
    // process's output would otherwise wait for the daemon to exit.
    #[cfg(windows)]
    unset_std_inherit();
    let child = cmd.spawn()?;
    // The pid file is written here too, so `status` is right before the daemon finishes its setup.
    std::fs::write(pid_path(), child.id().to_string())?;
    Ok(child.id())
}

#[cfg(windows)]
fn unset_std_inherit() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    // SAFETY: plain Win32 calls on the process's own standard handles.
    unsafe {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(which);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Ask the background watcher to quit and wait for it.
pub fn stop_background() -> anyhow::Result<bool> {
    let Some(pid) = running_pid() else { return Ok(false) };
    std::fs::write(stop_path(), "stop")?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        if !pid_alive(pid) {
            let _ = std::fs::remove_file(pid_path());
            let _ = std::fs::remove_file(stop_path());
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(trf!("Wikelink (pid {pid}) did not answer the request to stop", "o Wikelink (pid {pid}) não respondeu ao pedido de parada"))
}

// ----- overlay process control ----------------------------------------------------------

/// The named mutex the overlay holds for its whole life: one overlay per Windows session.
/// Unlike a pid file it cannot go stale, since Windows drops it with the process.
#[cfg(windows)]
const OVERLAY_MUTEX: &str = "Local\\WikelistOverlay";

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Proof that this process is the overlay; dropping it lets another one start.
pub struct OverlayLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

/// Take the overlay's mutex; `Ok(None)` when another overlay holds it already.
#[cfg(windows)]
pub fn overlay_lock() -> anyhow::Result<Option<OverlayLock>> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = wide(OVERLAY_MUTEX);
    // SAFETY: default security (null), not initially owned, `name` is NUL-terminated UTF-16
    // that outlives the call.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: no arguments; read right after CreateMutexW on the same thread.
    let err = unsafe { GetLastError() };
    if handle == 0 {
        bail!(trf!("could not create the overlay's mutex (error {err})", "não foi possível criar o mutex do overlay (erro {err})"));
    }
    if err == ERROR_ALREADY_EXISTS {
        // SAFETY: the handle CreateMutexW just returned; closed once, here.
        unsafe { CloseHandle(handle) };
        return Ok(None);
    }
    Ok(Some(OverlayLock { handle }))
}

#[cfg(not(windows))]
pub fn overlay_lock() -> anyhow::Result<Option<OverlayLock>> {
    Ok(Some(OverlayLock {}))
}

#[cfg(windows)]
impl Drop for OverlayLock {
    fn drop(&mut self) {
        // SAFETY: the handle CreateMutexW returned; this struct owns it and closes it only here.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

/// Is an overlay open (its mutex exists)?
#[cfg(windows)]
pub fn overlay_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::Threading::OpenMutexW;
    // `SYNCHRONIZE`: the least access right that opens a mutex.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let name = wide(OVERLAY_MUTEX);
    // SAFETY: `name` is NUL-terminated UTF-16 that outlives the call; the handle is not inherited.
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle == 0 {
        // SAFETY: no arguments; read right after the failed OpenMutexW on the same thread.
        let err = unsafe { GetLastError() };
        // Access denied: it exists, held at another integrity level.
        return err == ERROR_ACCESS_DENIED;
    }
    // SAFETY: the handle OpenMutexW just returned; closed once, here.
    unsafe { CloseHandle(handle) };
    true
}

#[cfg(not(windows))]
pub fn overlay_running() -> bool {
    false
}

/// Set (to `1`) in the environment of an overlay that replaces itself (see
/// [`respawn_overlay`]): the new process waits for the old one's mutex instead of leaving, and
/// keeps a pending stop request.
pub const OVERLAY_RESPAWN_ENV: &str = "WIKELIST_OVERLAY_RESPAWN";

/// Start this executable again with `args`, detached (its own process, no console, no std
/// handles of ours), optionally with one extra environment variable.
fn spawn_self_detached<I, S>(args: I, env: Option<(&str, &str)>) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = std::env::current_exe()?;
    spawn_detached(&exe, args, env)
}

/// [`spawn_self_detached`] for the executable at `exe` (the updater starts the file it just put
/// in place by its path).
pub(crate) fn spawn_detached<I, S>(exe: &Path, args: I, env: Option<(&str, &str)>) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    // The daemon's std handles are the log file: they must not leak into the child.
    #[cfg(windows)]
    unset_std_inherit();
    cmd.spawn()?;
    Ok(())
}

/// Spawn `overlay` detached (its own process, no console), unless one is open already. Two
/// launches racing each other are harmless: the overlay that does not get the mutex leaves.
/// It stays hidden until the game's window is there.
pub fn launch_overlay() -> anyhow::Result<()> {
    launch_overlay_with(false)
}

/// [`launch_overlay`]; `preview` adds `--preview` (shown at once, even without the game).
pub fn launch_overlay_with(preview: bool) -> anyhow::Result<()> {
    if overlay_running() {
        return Ok(());
    }
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    // A stop request nobody read (an overlay that crashed) must not close the new one at once,
    // and an overlay opened on purpose is one the background watcher keeps up again.
    let _ = std::fs::remove_file(overlay_stop_path());
    let _ = std::fs::remove_file(overlay_closed_path());
    let args: &[&str] = if preview { &["overlay", "--preview"] } else { &["overlay"] };
    spawn_self_detached(args, None)
}

/// The overlay replaces itself with a fresh process (same arguments), which waits for the
/// game with a new window and a new GL device. Never clears a stop request: when one is
/// pending the overlay was asked to close, so nothing starts, it just ends, and the request
/// is used up (this is the overlay acting on it).
pub fn respawn_overlay() -> anyhow::Result<()> {
    if overlay_stop_path().exists() {
        let _ = std::fs::remove_file(overlay_stop_path());
        return Ok(());
    }
    spawn_self_detached(std::env::args_os().skip(1), Some((OVERLAY_RESPAWN_ENV, "1")))
}

/// Open the settings window (this executable without arguments), detached.
pub fn open_settings_window() -> anyhow::Result<()> {
    spawn_self_detached(std::iter::empty::<&str>(), None)
}

/// Ask the background watcher to quit, like `stop` does, without waiting for it (the
/// watcher itself calls this from its tray icon).
pub fn request_stop() -> std::io::Result<()> {
    std::fs::write(stop_path(), "stop")
}

/// Ask the overlay to close and wait for it (a few seconds at most); `true` when one was
/// running and has ended. The request (`overlay.stop`) is written even when no overlay holds
/// the mutex right now: one that is replacing itself after the game left lets the mutex go
/// before its replacement takes it, and either of them must still see the request. It is
/// never deleted here: the overlay that acts on it removes it, and a fresh launch on
/// purpose ([`launch_overlay_with`]) clears one left over. Before it, [`overlay_closed_path`]
/// says the player closed it, so the background watcher does not bring it straight back.
pub fn close_overlay() -> anyhow::Result<bool> {
    let running = overlay_running();
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(overlay_closed_path(), "closed")?;
    std::fs::write(overlay_stop_path(), "stop")?;
    if !running {
        return Ok(false);
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if !overlay_running() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(tr!("the overlay did not answer the request to close", "o overlay não respondeu ao pedido para fechar"))
}

/// How often the background watcher looks after the overlay (see [`OverlayKeeper`]).
const OVERLAY_KEEP_EVERY: Duration = Duration::from_secs(2);
/// Least time between two overlay starts by the background watcher.
const OVERLAY_START_GAP: Duration = Duration::from_secs(10);

/// What the background watcher sees on one look at the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayLook {
    /// "Mostrar o overlay quando o jogo abrir" is on (see `AgentConfig::overlay_with_game`).
    pub wanted: bool,
    /// An overlay process holds the overlay's mutex (one waiting for the game included).
    pub running: bool,
    /// The game's window is there (found by its title, see `overlay_win::game_window`).
    pub game: bool,
    /// `overlay.stop` waits for an overlay to act on it.
    pub stop_pending: bool,
    /// The player closed the overlay (`overlay.closed`, see [`close_overlay`]).
    pub closed: bool,
}

/// What the background watcher does after one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeeperStep {
    /// Start an overlay process.
    pub start: bool,
    /// The start is a fresh one (the watcher just started, or a new game session): what an
    /// earlier session left (a stop request nobody read, the player's close) is cleared, as a
    /// launch on purpose does. Otherwise a pending stop request and a close are respected.
    pub fresh: bool,
}

/// Keeps an overlay process running while the background watcher runs, so the overlay is on
/// screen as soon as the game is (the process waits without a window until then): one at
/// the watcher's start, and again (no sooner than [`OVERLAY_START_GAP`] after the last
/// start) whenever none runs while the game's window exists: it crashed, or was ended from
/// outside. Never while the player has it closed (the tray, the settings window) or a stop
/// request waits; a new game session (the game's window appearing again) forgets that close.
/// "Ocultar" and Alt+Shift+F9 only hide the overlay, so this never fights the player.
#[derive(Debug, Default)]
pub struct OverlayKeeper {
    last_start: Option<Instant>,
    /// The first look (at the watcher's start) was made.
    looked: bool,
    /// The game's window was there on the last look.
    game_was_up: bool,
}

impl OverlayKeeper {
    pub fn step(&mut self, look: OverlayLook, now: Instant) -> KeeperStep {
        let first = !std::mem::replace(&mut self.looked, true);
        let new_session = look.game && !self.game_was_up && !first;
        self.game_was_up = look.game;
        let fresh = first || new_session;
        let held_back = !fresh && (look.stop_pending || look.closed);
        let gap_over = self.last_start.is_none_or(|t| now.saturating_duration_since(t) >= OVERLAY_START_GAP);
        let start = look.wanted && !look.running && !held_back && gap_over && (first || look.game);
        if start {
            self.last_start = Some(now);
        }
        KeeperStep { start, fresh }
    }
}

/// One look of the background watcher at the overlay (see [`OverlayKeeper`]), and the start
/// it calls for. Returns a problem to log.
fn keep_overlay(keeper: &mut OverlayKeeper, wanted: bool) -> Option<String> {
    let look = OverlayLook {
        // Only on Windows: elsewhere there is no game window to wait for, and the overlay
        // would show at once.
        wanted: wanted && cfg!(windows),
        running: overlay_running(),
        game: crate::overlay_win::game_window().is_some(),
        stop_pending: overlay_stop_path().exists(),
        closed: overlay_closed_path().exists(),
    };
    let step = keeper.step(look, Instant::now());
    if !step.start {
        return None;
    }
    let started = if step.fresh { launch_overlay() } else { spawn_self_detached(["overlay"], None) };
    started.err().map(|e| trf!("could not open the overlay: {e:#}", "não deu para abrir o overlay: {e:#}"))
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// This program's value under [`RUN_KEY`]: `"<this exe>" start --quiet`.
#[cfg(windows)]
const RUN_VALUE: &str = "Wikelink";
/// The value `wikelo-watcher.exe` (the program before its rename) wrote there. Removed
/// whenever this program writes its own, so one sign-in never starts two programs; a
/// copy of the old exe still on disk is left alone.
#[cfg(windows)]
const OLD_RUN_VALUE: &str = "WikeloWatcher";

/// `reg.exe` without a console window popping up (we have no console of our own).
#[cfg(windows)]
fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).stdin(std::process::Stdio::null()).output()
}

#[cfg(windows)]
fn run_value_exists(name: &str) -> bool {
    reg(&["query", RUN_KEY, "/v", name]).map(|o| o.status.success()).unwrap_or(false)
}

/// Register / remove Wikelink in the user's Windows autostart (HKCU Run key). Either way the
/// old program's entry goes too; when turning on, only after the new one is written.
#[cfg(windows)]
pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    if on {
        let exe = std::env::current_exe()?;
        let value = format!("\"{}\" start --quiet", exe.display());
        let status = reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])?;
        if !status.status.success() {
            let detail = String::from_utf8_lossy(&status.stderr).trim().to_string();
            bail!(trf!("could not write the startup entry: {detail}", "não foi possível gravar a entrada de inicialização: {detail}"));
        }
    } else {
        reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?;
    }
    if run_value_exists(OLD_RUN_VALUE) {
        reg(&["delete", RUN_KEY, "/v", OLD_RUN_VALUE, "/f"])?;
    }
    Ok(())
}

/// On when either this program's entry or the old program's is there (the old one is moved
/// over by [`migrate_autostart`]).
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    run_value_exists(RUN_VALUE) || run_value_exists(OLD_RUN_VALUE)
}

/// An install from before the rename started `wikelo-watcher.exe` with Windows under
/// [`OLD_RUN_VALUE`]: that choice moves to this executable (new entry written, old one
/// removed). Nothing happens without the old entry. `Ok(true)` when it moved.
#[cfg(windows)]
pub fn migrate_autostart() -> anyhow::Result<bool> {
    if !run_value_exists(OLD_RUN_VALUE) {
        return Ok(false);
    }
    set_autostart(true)?;
    Ok(true)
}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) -> anyhow::Result<()> {
    bail!(tr!("start with the system: Windows only", "inicialização automática só no Windows"))
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn migrate_autostart() -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_error_prefers_the_json_message() {
        assert_eq!(server_error(429, br#"{"error":"tente de novo mais tarde","code":"x"}"#), "tente de novo mais tarde");
        assert_eq!(server_error(403, br#"{"error":"recusado"}"#), "recusado");
    }

    #[test]
    fn server_error_falls_back_to_http_code() {
        assert_eq!(server_error(502, b"<html>bad gateway</html>"), "HTTP 502");
        assert_eq!(server_error(500, br#"{"error":""}"#), "HTTP 500");
        assert_eq!(server_error(404, b""), "HTTP 404");
    }

    #[test]
    fn uploads_new_versions_and_copies_from_older_extractors() {
        let k = |channel: &str, version: &str, extractor: Option<&str>| KnownDataset { channel: channel.into(), version: version.into(), extractor: extractor.map(str::to_string), revision: None };
        let known = vec![k("LIVE", "4.10.1", None), k("LIVE", "4.10.2", Some("0.2.0")), k("PTU", "4.11.0", Some("0.2.1")), k("LIVE", "4.10.3", Some("0.2.0")), k("LIVE", "4.10.3", Some("0.2.1"))];
        assert_eq!(upload_reason(&known, "LIVE", "4.10.9", "0.2.1", 2), Some(UploadReason::New), "a version the server lacks");
        assert_eq!(upload_reason(&known, "LIVE", "4.10.1", "0.2.1", 2), Some(UploadReason::OlderExtractor(None)), "the server cannot say which extractor made its copy");
        assert_eq!(upload_reason(&known, "LIVE", "4.10.2", "0.2.1", 2), Some(UploadReason::OlderExtractor(Some("0.2.0".into()))));
        assert_eq!(upload_reason(&known, "live", "4.10.2", "0.2.0", 2), None, "same extractor: nothing to add (channel case does not matter)");
        assert_eq!(upload_reason(&known, "PTU", "4.11.0", "0.2.1", 2), None);
        assert_eq!(upload_reason(&known, "PTU", "4.11.0", "0.1.9", 2), None, "the server is ahead: never downgrade it");
        assert_eq!(upload_reason(&known, "LIVE", "4.10.3", "0.2.1", 2), None, "one up-to-date copy (our own pending upload) is enough");
        assert_eq!(upload_reason(&known, "LIVE", "4.10.3", "0.2.2", 2), Some(UploadReason::OlderExtractor(Some("0.2.1".into()))), "names the newest copy");
        // A server that tells revisions: only they decide, whatever the program versions say.
        let r = |extractor: &str, revision: u32| vec![KnownDataset { revision: Some(revision), ..k("LIVE", "4.10.2", Some(extractor)) }];
        assert_eq!(upload_reason(&r("1.1.0", 0), "LIVE", "4.10.2", "0.1.0", 2), Some(UploadReason::OlderExtractor(Some("1.1.0".into()))), "an older revision from a higher program version is still older");
        assert_eq!(upload_reason(&r("0.1.0", 2), "LIVE", "4.10.2", "0.1.0", 2), None, "same revision: nothing to add");
        assert_eq!(upload_reason(&r("0.0.9", 3), "LIVE", "4.10.2", "0.1.0", 2), None, "the server is ahead: never downgrade it");
        assert_eq!(upload_reason(&r("0.1.0", 2), "LIVE", "4.10.2", "9.0.0", 2), None, "a newer program with the same revision sends nothing");
        let odd = vec![k("LIVE", "4.10.2", Some("dev"))];
        assert_eq!(upload_reason(&odd, "LIVE", "4.10.2", "0.2.1", 2), None, "an unparsable version counts as current, never as old");
    }

    #[test]
    fn old_config_files_still_load() {
        let old = r#"{"server":"https://wikelo.exemplo.com","token":"t","handle":"TestPilot","name":"Test Pilot","channel":"LIVE","share_data":true}"#;
        let c: AgentConfig = serde_json::from_str(old).unwrap();
        assert_eq!((c.server.as_str(), c.channel.as_deref(), c.share_data), ("https://wikelo.exemplo.com", Some("LIVE"), Some(true)));
        assert!(c.overlay_with_game(), "the overlay comes up with the game for an existing setup too");
        assert_eq!(c.overlay_pos, None, "no saved position: the default corner");
        assert!(c.overlay_widgets.is_empty() && c.overlay_opacity.is_none(), "no cards saved: the default layout");
        let text = serde_json::to_string_pretty(&c).unwrap();
        assert!(!text.contains("overlay_widgets") && !text.contains("overlay_opacity") && !text.contains("overlay_with_game"), "nothing new written until something saves: {text}");
    }

    #[test]
    fn the_overlay_comes_with_the_game_unless_switched_off_with_the_new_key() {
        let parse = |extra: &str| -> AgentConfig { serde_json::from_str(&format!(r#"{{"server":"https://x","token":"secret","handle":"TestPilot"{extra}}}"#)).unwrap() };
        assert!(parse("").overlay_with_game(), "absent: on");
        // The old opt-in, off by default, no longer keeps it off (and is not written back).
        let old = parse(r#","overlay_with_watcher":false"#);
        assert!(old.overlay_with_game());
        assert!(!serde_json::to_string(&old).unwrap().contains("overlay_with_watcher"));
        assert!(parse(r#","overlay_with_watcher":true"#).overlay_with_game());
        // The settings window's checkbox.
        assert!(!parse(r#","overlay_with_game":false"#).overlay_with_game());
        assert!(parse(r#","overlay_with_game":true"#).overlay_with_game());
        assert!(!parse(r#","overlay_with_watcher":true,"overlay_with_game":false"#).overlay_with_game(), "the new key decides");
        // An odd value never loses the sign-in, and reads as on.
        for odd in ["1", r#""no""#, "null", "[false]"] {
            let c = parse(&format!(r#","overlay_with_game":{odd}"#));
            assert_eq!((c.token.as_str(), c.overlay_with_game()), ("secret", true), "{odd}");
        }
        // Round trip, and a fresh config (the first sign-in) is on.
        let off = AgentConfig { token: "t".into(), overlay_with_game: Some(false), ..Default::default() };
        let back: AgentConfig = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!back.overlay_with_game());
        assert!(AgentConfig::default().overlay_with_game());
    }

    fn look(wanted: bool, running: bool, game: bool) -> OverlayLook {
        OverlayLook { wanted, running, game, stop_pending: false, closed: false }
    }

    #[test]
    fn the_watcher_starts_the_overlay_at_its_start_and_keeps_it_up_while_the_game_is_there() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut k = OverlayKeeper::default();
        // The watcher's start: at once, game or not (the overlay waits for it without a window).
        assert_eq!(k.step(look(true, false, false), t0), KeeperStep { start: true, fresh: true });
        // Still not running (it is starting, or crashed) and no game: nothing.
        assert!(!k.step(look(true, false, false), t0 + s(2)).start);
        // Running: nothing.
        assert!(!k.step(look(true, true, false), t0 + s(4)).start);
        assert!(!k.step(look(true, true, true), t0 + s(6)).start);
        // Gone (crashed, ended from outside) while the game is there: again, but never twice
        // within 10 s.
        assert_eq!(k.step(look(true, false, true), t0 + s(12)), KeeperStep { start: true, fresh: false });
        assert!(!k.step(look(true, false, true), t0 + s(14)).start, "one start attempt every 10 s");
        assert!(!k.step(look(true, false, true), t0 + s(21)).start);
        assert!(k.step(look(true, false, true), t0 + s(22)).start);
        // Switched off in the settings window: never.
        let mut off = OverlayKeeper::default();
        assert!(!off.step(look(false, false, false), t0).start);
        assert!(!off.step(look(false, false, true), t0 + s(30)).start);
    }

    #[test]
    fn the_watcher_respects_a_stop_request_and_the_players_close_until_the_next_game() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut k = OverlayKeeper::default();
        assert!(!k.step(look(true, true, true), t0).start, "already running at the watcher's start");
        // The player closed it from the tray while playing: not brought back.
        let closed = OverlayLook { closed: true, ..look(true, false, true) };
        for t in [2, 12, 30, 60] {
            assert!(!k.step(closed, t0 + s(t)).start, "closed by the player: {t} s");
        }
        // A stop request waiting for an overlay: not either.
        let stopping = OverlayLook { stop_pending: true, ..look(true, false, true) };
        assert!(!k.step(stopping, t0 + s(70)).start);
        // The game closes and opens again: a new session, the overlay comes back, fresh.
        assert!(!k.step(OverlayLook { closed: true, ..look(true, false, false) }, t0 + s(80)).start);
        assert_eq!(k.step(closed, t0 + s(82)), KeeperStep { start: true, fresh: true });
        // At the watcher's start what an earlier run left says nothing either.
        let mut k = OverlayKeeper::default();
        assert_eq!(k.step(OverlayLook { closed: true, stop_pending: true, ..look(true, false, false) }, t0), KeeperStep { start: true, fresh: true });
    }

    fn widgets(list: &[(&str, f32, f32, bool)]) -> BTreeMap<String, OverlayWidget> {
        list.iter().map(|&(k, x, y, pinned)| (k.to_string(), OverlayWidget { x, y, pinned, w: None, h: None })).collect()
    }

    #[test]
    fn overlay_fields_round_trip() {
        let c = AgentConfig {
            server: "https://x".into(),
            token: "t".into(),
            handle: "h".into(),
            overlay_with_game: Some(false),
            overlay_pos: Some([1556.0, 80.5]),
            overlay_widgets: widgets(&[("fixados", 0.8, 0.07, true), ("aqui", 0.25, 0.5, false)]),
            overlay_opacity: Some(0.6),
            ..Default::default()
        };
        let text = serde_json::to_string_pretty(&c).unwrap();
        assert!(text.contains("\"overlay_with_game\": false") && text.contains("\"overlay_pos\""), "{text}");
        let back: AgentConfig = serde_json::from_str(&text).unwrap();
        assert!(!back.overlay_with_game());
        assert_eq!(back.overlay_pos, Some([1556.0, 80.5]));
        assert_eq!(back.overlay_widgets, c.overlay_widgets);
        assert_eq!(back.overlay_opacity, Some(0.6));
        assert_eq!((back.server, back.token, back.handle), (c.server, c.token, c.handle));
    }

    #[test]
    fn odd_overlay_fields_never_lose_the_sign_in() {
        // A hand-edited or future file: bad cards are skipped, a bad opacity is the default,
        // and the rest of the file (the token) still loads.
        let text = r#"{"server":"https://x","token":"secret","handle":"TestPilot",
            "overlay_widgets":{"fixados":{"x":0.5,"y":0.1,"pinned":true},"falta":{"x":"a","y":0.2},"aqui":{"x":0.1,"y":0.2,"pinned":"yes"},"avisos":7,"novo":{"x":0.3,"y":0.4,"pinned":false,"w":3}},
            "overlay_opacity":"muito"}"#;
        let c: AgentConfig = serde_json::from_str(text).unwrap();
        assert_eq!(c.token, "secret");
        let mut want = widgets(&[("aqui", 0.1, 0.2, false), ("fixados", 0.5, 0.1, true), ("novo", 0.3, 0.4, false)]);
        if let Some(n) = want.get_mut("novo") {
            n.w = Some(3.0);
        }
        assert_eq!(c.overlay_widgets, want, "a size is read as it is; the overlay clamps it");
        assert_eq!(c.overlay_opacity, None);
        for odd in [r#""overlay_widgets":[1,2]"#, r#""overlay_widgets":null"#, r#""overlay_opacity":null"#, r#""overlay_opacity":[0.5]"#] {
            let text = format!(r#"{{"server":"https://x","token":"secret","handle":"TestPilot",{odd}}}"#);
            let c: AgentConfig = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{odd}: {e}"));
            assert_eq!((c.token.as_str(), c.overlay_widgets.len(), c.overlay_opacity), ("secret", 0, None), "{odd}");
        }
    }

    #[test]
    fn saving_the_overlay_layout_keeps_the_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        let cards = widgets(&[("fixados", 0.8, 0.07, true), ("falta", 0.6, 0.07, true)]);
        // No config: the overlay writes nothing (never a config without the token).
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.9, 1.0).unwrap(), LayoutSave::NoConfig);
        assert!(!reset_overlay_layout_at(&p).unwrap());
        assert!(!p.exists());
        // An unreadable (half-written) file is left alone too, and the overlay tries again later.
        let half = "{\"server\":\"https://x\",\"tok";
        std::fs::write(&p, half).unwrap();
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.9, 1.0).unwrap(), LayoutSave::NoConfig);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), half);

        // Signed in, with a position from the old single-window overlay.
        let signed_in = AgentConfig { server: "https://x".into(), token: "secret".into(), handle: "TestPilot".into(), overlay_with_game: Some(false), overlay_pos: Some([1556.0, 80.0]), ..Default::default() };
        save_config_to(&p, &signed_in).unwrap();
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.7, 1.0).unwrap(), LayoutSave::Written);
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.handle.as_str(), back.overlay_with_game()), ("secret", "TestPilot", false));
        assert_eq!((&back.overlay_widgets, back.overlay_opacity), (&cards, Some(0.7)));
        assert_eq!(back.overlay_pos, None, "the old position was read once, into the cards");
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.7, 1.0).unwrap(), LayoutSave::Unchanged, "nothing changed: nothing to write");
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.5, 1.0).unwrap(), LayoutSave::Written, "the opacity alone is a change");
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.5, 1.3).unwrap(), LayoutSave::Written, "the text size alone is a change");
        assert_eq!(load_config_from(&p).unwrap().overlay_text_scale, Some(1.3));
        // Sizes go with the cards; a card without one writes none.
        let mut sized = cards.clone();
        if let Some(c) = sized.get_mut("falta") {
            (c.w, c.h) = (Some(0.25), Some(0.5));
        }
        assert_eq!(save_overlay_layout_at(&p, &sized, 0.5, 1.3).unwrap(), LayoutSave::Written);
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("\"w\"").count(), 1, "{text}");
        assert_eq!(load_config_from(&p).unwrap().overlay_widgets, sized);
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.5, 1.0).unwrap(), LayoutSave::Written);
        assert_eq!(std::fs::read_to_string(&p).unwrap().matches("\"w\"").count(), 0, "a size taken away is gone from the file");
        // "Repor posições": the cards go, the opacity stays.
        assert!(reset_overlay_layout_at(&p).unwrap());
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_widgets.len(), back.overlay_pos, back.overlay_opacity), ("secret", 0, None, Some(0.5)));
        assert!(!reset_overlay_layout_at(&p).unwrap(), "already the default");
        // Only watcher.json is left: every temporary file was renamed over it.
        let names: Vec<std::ffi::OsString> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("watcher.json")]);
    }

    #[test]
    fn other_writers_never_bring_back_stale_overlay_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        // Loaded by the settings window before the overlay moved and the option changed.
        let mut stale = AgentConfig { server: "https://x".into(), token: "old".into(), handle: "TestPilot".into(), overlay_pos: Some([5.0, 6.0]), ..Default::default() };
        save_config_to(&p, &stale).unwrap();
        let cards = widgets(&[("fixados", 0.5, 0.25, true)]);
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.6, 1.0).unwrap(), LayoutSave::Written);
        let disk = update_config_at(&p, &stale, |d| d.overlay_with_game = Some(false)).unwrap();
        assert!(!disk.overlay_with_game());
        // A sign-in written from the stale copy: the new token lands, the overlay fields survive.
        stale.token = "new".into();
        let current = update_config_at(&p, &stale, |d| copy_sign_in(d, &stale)).unwrap();
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_pos, back.overlay_with_game()), ("new", None, false));
        assert_eq!((&back.overlay_widgets, back.overlay_opacity), (&cards, Some(0.6)));
        assert_eq!((current.overlay_pos, current.overlay_with_game(), current.overlay_opacity), (None, false, Some(0.6)), "the caller's copy is current too");
        assert_eq!(current.overlay_widgets, cards);
        // First sign-in on this PC: nothing on disk, the caller's values are written as they are
        // (the overlay with the game on: nothing written for it).
        let fresh = dir.path().join("sub").join("watcher.json");
        let first = AgentConfig { token: "t".into(), ..Default::default() };
        update_config_at(&fresh, &first, |d| copy_sign_in(d, &first)).unwrap();
        assert!(load_config_from(&fresh).unwrap().overlay_with_game());
    }

    #[test]
    fn each_writer_changes_only_its_own_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        let first = AgentConfig { server: "https://x".into(), token: "old".into(), handle: "TestPilot".into(), channel: Some("LIVE".into()), share_data: Some(false), ..Default::default() };
        save_config_to(&p, &first).unwrap();
        // The settings window loaded the file, then a sign-in elsewhere wrote a new token.
        let window_copy = load_config_from(&p).unwrap();
        let signed_in = AgentConfig { token: "new".into(), name: Some("Test Pilot".into()), ..first.clone() };
        update_config_at(&p, &first, |d| copy_sign_in(d, &signed_in)).unwrap();
        // The window's stale copy changes the channel: the new token stays.
        let after = update_config_at(&p, &window_copy, |d| d.channel = Some("PTU".into())).unwrap();
        assert_eq!((after.token.as_str(), after.name.as_deref(), after.channel.as_deref(), after.share_data), ("new", Some("Test Pilot"), Some("PTU"), Some(false)));
        assert_eq!(load_config_from(&p).unwrap().token, "new");
        // The overlay saves its cards over that: nothing else moves.
        let cards = widgets(&[("fixados", 0.5, 0.25, true)]);
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.8, 1.0).unwrap(), LayoutSave::Written);
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.channel.as_deref(), &back.overlay_widgets), ("new", Some("PTU"), &cards));
        // An unreadable file: the caller's copy is the base (never stuck on a broken file).
        std::fs::write(&p, "{\"server\":").unwrap();
        let fixed = update_config_at(&p, &first, |d| d.share_data = Some(true)).unwrap();
        assert_eq!((fixed.token.as_str(), fixed.share_data), ("old", Some(true)));
        assert_eq!(load_config_from(&p).unwrap().share_data, Some(true));
    }

    #[test]
    fn saving_the_overlay_layout_keeps_what_it_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        // A file a newer watcher wrote: a field this one does not know, a card of a newer
        // overlay, and more in a card this one knows.
        let text = r#"{"server":"https://x","token":"secret","handle":"TestPilot","future":{"a":[1,2]},
            "overlay_widgets":{"fixados":{"x":0.1,"y":0.1,"pinned":false,"cor":"azul"},"novo":{"x":0.3,"y":0.4,"pinned":true}}}"#;
        std::fs::write(&p, text).unwrap();
        let cards = widgets(&[("fixados", 0.5, 0.25, true), ("falta", 0.6, 0.25, true)]);
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.7, 1.15).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["future"], serde_json::json!({ "a": [1, 2] }), "an unknown field stays");
        assert_eq!(v["overlay_widgets"]["novo"], serde_json::json!({ "x": 0.3, "y": 0.4, "pinned": true }), "a newer overlay's card stays");
        assert_eq!(v["overlay_widgets"]["fixados"], serde_json::json!({ "x": 0.5, "y": 0.25, "pinned": true, "cor": "azul" }), "a known card is patched");
        assert_eq!(v["overlay_widgets"]["falta"], serde_json::json!({ "x": 0.6, "y": 0.25, "pinned": true }));
        assert_eq!((v["overlay_opacity"].clone(), v["overlay_text_scale"].clone(), v["token"].clone()), (serde_json::json!(0.7), serde_json::json!(1.15), serde_json::json!("secret")), "short numbers");
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.overlay_widgets.get("fixados"), back.overlay_opacity), (cards.get("fixados"), Some(0.7)));
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.7, 1.15).unwrap(), LayoutSave::Unchanged, "the unknown card does not count as a change");
        // "Repor posições" keeps the unknown field as well.
        assert!(reset_overlay_layout_at(&p).unwrap());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!((v["future"].clone(), v.get("overlay_widgets").is_none(), v["overlay_opacity"].clone()), (serde_json::json!({ "a": [1, 2] }), true, serde_json::json!(0.7)));
    }

    #[test]
    fn the_source_pins_are_saved_bounded_with_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        std::fs::write(&p, r#"{"server":"https://x","token":"secret","handle":"TestPilot","future":1}"#).unwrap();
        let cards = widgets(&[("fixados", 0.5, 0.25, true)]);
        let pins = vec![["favor".to_string(), "scrip".to_string()], ["favor".to_string(), String::new()]];
        let sw = OverlaySwitches::default();
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &pins, &sw).unwrap(), LayoutSave::Written);
        let back = load_config_from(&p).unwrap();
        assert_eq!(back.token, "secret");
        assert_eq!(back.overlay_source_pins, vec![["favor".to_string(), "scrip".to_string()]], "a pair without an id is dropped");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["future"], serde_json::json!(1), "an unknown field stays");
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &pins, &sw).unwrap(), LayoutSave::Unchanged, "the same again: nothing written");
        // The layout alone keeps them.
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.5, 1.0).unwrap(), LayoutSave::Written);
        assert_eq!(load_config_from(&p).unwrap().overlay_source_pins.len(), 1);
        // No pins: the field goes.
        assert_eq!(save_overlay_state_at(&p, &cards, 0.5, 1.0, &[], &sw).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("overlay_source_pins").is_none(), "{v}");
        assert_eq!(save_overlay_state_at(&p, &cards, 0.5, 1.0, &[], &sw).unwrap(), LayoutSave::Unchanged);
        // Odd values in the file never lose the sign-in.
        std::fs::write(&p, r#"{"server":"https://x","token":"secret","handle":"TestPilot","overlay_source_pins":[["a"],["a","b"],"x",[1,2]]}"#).unwrap();
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_source_pins.clone()), ("secret", vec![["a".to_string(), "b".to_string()]]));
    }

    #[test]
    fn an_older_overlays_search_texts_tab_and_busca_card_load_and_go_on_the_next_save() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        let old = r#"{"server":"https://x","token":"secret","handle":"TestPilot","future":1,
            "overlay_searches":["carinite","favor"],"overlay_search_tab":"missoes",
            "overlay_widgets":{"fixados":{"x":0.5,"y":0.25,"pinned":true},"busca":{"x":0.1,"y":0.4,"pinned":false,"w":0.3,"h":0.4},"novo":{"x":0.3,"y":0.4,"pinned":true}},
            "overlay_opacity":0.7,"overlay_text_scale":1.0,"overlay_source_pins":[["favor","scrip"]]}"#;
        std::fs::write(&p, old).unwrap();
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_opacity), ("secret", Some(0.7)), "loads without complaint");
        assert_eq!(back.overlay_source_pins.len(), 1);
        // The same layout, but the file still has what is gone: the next save writes, without it.
        let cards = widgets(&[("fixados", 0.5, 0.25, true)]);
        let pins = vec![["favor".to_string(), "scrip".to_string()]];
        let sw = OverlaySwitches::default();
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &pins, &sw).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("overlay_searches").is_none() && v.get("overlay_search_tab").is_none() && v["overlay_widgets"].get("busca").is_none(), "{v}");
        assert_eq!(v["overlay_widgets"]["novo"], serde_json::json!({ "x": 0.3, "y": 0.4, "pinned": true }), "a newer overlay's card stays");
        assert_eq!((v["future"].clone(), v["token"].clone(), v["overlay_source_pins"].clone()), (serde_json::json!(1), serde_json::json!("secret"), serde_json::json!([["favor", "scrip"]])), "nothing else is lost");
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &pins, &sw).unwrap(), LayoutSave::Unchanged, "gone: nothing more to write");
        // A tab alone (the three-tab overlay's last save) goes too.
        std::fs::write(&p, r#"{"server":"https://x","token":"secret","handle":"TestPilot","overlay_search_tab":[1],"overlay_opacity":0.7,"overlay_text_scale":1.0}"#).unwrap();
        assert_eq!(load_config_from(&p).unwrap().token, "secret", "an odd old value never loses the sign-in");
        assert_eq!(save_overlay_state_at(&p, &BTreeMap::new(), 0.7, 1.0, &[], &sw).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("overlay_search_tab").is_none(), "{v}");
    }

    #[test]
    fn the_search_place_and_the_widget_switches_are_saved_with_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        // An older file: neither field. Everything shown, the search in its first place.
        std::fs::write(&p, r#"{"server":"https://x","token":"secret","handle":"TestPilot","overlay_hidden":["novo"]}"#).unwrap();
        let old = load_config_from(&p).unwrap();
        assert_eq!((old.overlay_search_pos, old.overlay_hidden.clone()), (None, vec!["novo".to_string()]));
        let cards = widgets(&[("fixados", 0.5, 0.25, true)]);
        let shown: BTreeMap<String, bool> = [("fixados".to_string(), false), ("falta".to_string(), true), ("pesquisa".to_string(), false)].into();
        let sw = OverlaySwitches { search_pos: Some([0.25, 0.75]), shown: shown.clone() };
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &[], &sw).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["overlay_search_pos"], serde_json::json!([0.25, 0.75]));
        assert_eq!(v["overlay_hidden"], serde_json::json!(["fixados", "novo", "pesquisa"]), "a newer overlay's switch stays");
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_search_pos), ("secret", Some([0.25, 0.75])));
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &[], &sw).unwrap(), LayoutSave::Unchanged, "the same again: nothing written");
        // Back in its place and everything shown: the fields go (the unknown name stays).
        let all: BTreeMap<String, bool> = shown.keys().map(|k| (k.clone(), true)).collect();
        let sw = OverlaySwitches { search_pos: None, shown: all };
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &[], &sw).unwrap(), LayoutSave::Written);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!((v.get("overlay_search_pos"), v["overlay_hidden"].clone()), (None, serde_json::json!(["novo"])));
        // A layout write that does not know them keeps both.
        let sw = OverlaySwitches { search_pos: Some([0.1, 0.2]), shown };
        assert_eq!(save_overlay_state_at(&p, &cards, 0.7, 1.0, &[], &sw).unwrap(), LayoutSave::Written);
        assert_eq!(save_overlay_layout_at(&p, &cards, 0.5, 1.0).unwrap(), LayoutSave::Written);
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.overlay_search_pos, back.overlay_hidden.len()), (Some([0.1, 0.2]), 3));
        // "Repor posições" puts the search back in its place; the switches stay.
        assert!(reset_overlay_layout_at(&p).unwrap());
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.overlay_search_pos, back.overlay_hidden.len()), (None, 3));
        // Odd values never lose the sign-in.
        for (odd_pos, odd_hidden) in [("7", "7"), ("null", "null"), ("[1]", r#"[1, "", "  "]"#), (r#"[0.1, "a"]"#, r#"{"a":1}"#), ("[0.1, 0.2, 0.3]", r#""fixados""#)] {
            std::fs::write(&p, format!(r#"{{"server":"https://x","token":"secret","handle":"TestPilot","overlay_search_pos":{odd_pos},"overlay_hidden":{odd_hidden}}}"#)).unwrap();
            let back = load_config_from(&p).unwrap();
            assert_eq!((back.token.as_str(), back.overlay_search_pos, back.overlay_hidden.len()), ("secret", None, 0), "{odd_pos} {odd_hidden}");
        }
        std::fs::write(&p, r#"{"server":"https://x","token":"secret","handle":"TestPilot","overlay_hidden":[" falta ","falta","aqui"]}"#).unwrap();
        assert_eq!(load_config_from(&p).unwrap().overlay_hidden, vec!["aqui".to_string(), "falta".to_string()], "trimmed, once each");
    }

    #[test]
    fn the_language_key_is_optional_lenient_and_outlives_the_sign_out() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        // Absent: follow Windows, and nothing is written for it.
        let c: AgentConfig = serde_json::from_str(r#"{"server":"https://x","token":"secret","handle":"TestPilot"}"#).unwrap();
        assert_eq!(c.lang, None);
        assert!(!serde_json::to_string(&c).unwrap().contains("lang"));
        // An odd value never loses the sign-in.
        for odd in ["7", "null", "[\"en\"]", "{}"] {
            let c: AgentConfig = serde_json::from_str(&format!(r#"{{"server":"https://x","token":"secret","handle":"TestPilot","lang":{odd}}}"#)).unwrap();
            assert_eq!((c.token.as_str(), c.lang.as_deref()), ("secret", None), "{odd}");
        }
        // Set through the locked patch, then signing out keeps only the language.
        let signed_in = AgentConfig { server: "https://x".into(), token: "secret".into(), handle: "TestPilot".into(), ..Default::default() };
        save_config_to(&p, &signed_in).unwrap();
        update_config_at(&p, &signed_in, |d| d.lang = Some("en".into())).unwrap();
        assert_eq!(load_config_from(&p).unwrap().lang.as_deref(), Some("en"));
        remove_config_at(&p).unwrap();
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.server.as_str(), back.lang.as_deref()), ("", "", Some("en")));
        // Without a language, the file goes; a missing file is fine.
        update_config_at(&p, &back, |d| d.lang = None).unwrap();
        remove_config_at(&p).unwrap();
        assert!(!p.exists());
        remove_config_at(&p).unwrap();
    }

    #[test]
    fn the_dev_server_is_only_ever_this_pc() {
        let dev = |s: &str| dev_base(Some(s));
        let ok = |s: &str| dev(s).unwrap_or_else(|e| panic!("{s:?}: {e}")).unwrap_or_else(|| panic!("{s:?}: none"));
        // Unset or blank: the site.
        assert_eq!(dev_base(None), Ok(None));
        assert_eq!(dev("   "), Ok(None));
        // This PC in its three spellings, any port, http or https; without a scheme, http.
        assert_eq!(ok("http://localhost:8787"), "http://localhost:8787");
        assert_eq!(ok(" http://127.0.0.1:8787/ "), "http://127.0.0.1:8787");
        assert_eq!(ok("http://[::1]:8787"), "http://[::1]:8787");
        assert_eq!(ok("https://localhost:8443"), "https://localhost:8443");
        assert_eq!(ok("HTTP://LOCALHOST"), "http://localhost");
        assert_eq!(ok("localhost:8787"), "http://localhost:8787");
        assert_eq!(ok("127.0.0.1"), "http://127.0.0.1");
        // Anything else is ignored (the site is used): other machines, names that merely start
        // like this PC, user info, a path, a query, bad ports, other schemes.
        for s in [
            "https://wikelist.net",
            "https://evil.example",
            "http://192.168.1.20:8787",
            "http://10.0.0.5",
            "http://127.0.0.2:8787",
            "http://[::2]:8787",
            "localhost.evil.example",
            "http://localhost.evil.example",
            "http://localhost.:8787",
            "http://user@localhost:8787",
            "http://localhost:8787/api",
            "http://localhost:8787?x=1",
            "http://localhost:0",
            "http://localhost:65536",
            "ftp://localhost",
            "file:///C:/x",
            "javascript:alert(1)",
        ] {
            assert!(dev(s).is_err(), "{s:?}");
        }
        // The value as shown in the log line: no control characters, not endless.
        assert_eq!(dev("http://evil\u{7}.example"), Err("http://evil.example".to_string()));
        assert_eq!(dev(&"x".repeat(500)).unwrap_err().chars().count(), 120);
    }

    #[test]
    fn a_token_only_goes_to_the_address_it_was_issued_by() {
        let dev = "http://localhost:8787";
        let cfg = |server: &str, token: &str| AgentConfig { server: server.into(), token: token.into(), handle: "TestPilot".into(), ..Default::default() };
        // Saved for the site (no key, or the site's address as an older version wrote it).
        for owner in ["", SITE, "https://wikelist.net/", "HTTPS://WIKELIST.NET"] {
            let c = with_token_rule(cfg(owner, "t"), SITE);
            assert_eq!((c.token.as_str(), c.server.as_str()), ("t", SITE), "{owner:?}");
            // Never to a developer server.
            let c = with_token_rule(cfg(owner, "t"), dev);
            assert_eq!((c.token.as_str(), c.server.as_str(), c.handle.as_str()), ("", "", "TestPilot"), "{owner:?}: the handle stays for the sign-in");
        }
        // Saved for a developer server: kept there, never sent to the site nor to another port.
        let c = with_token_rule(cfg(dev, "t"), dev);
        assert_eq!((c.token.as_str(), c.server.as_str()), ("t", dev));
        assert_eq!(with_token_rule(cfg(dev, "t"), SITE).token, "");
        assert_eq!(with_token_rule(cfg(dev, "t"), "http://localhost:9999").token, "");
        // An address an older version let the player type: the session goes.
        for owner in ["https://wikelo.exemplo.com", "http://192.168.1.20:8787", "wikelist.net", "https://wikelist.net/x", "?"] {
            assert_eq!(with_token_rule(cfg(owner, "t"), SITE).token, "", "{owner:?}");
        }
        // No token: no address either.
        let c = with_token_rule(cfg(dev, ""), dev);
        assert_eq!((c.token.as_str(), c.server.as_str()), ("", ""));
    }

    #[test]
    fn the_server_key_is_written_only_for_a_developer_server() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        let json = || -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap() };
        // A sign-in on the site: no key.
        let site = AgentConfig { server: SITE.into(), token: "t".into(), handle: "TestPilot".into(), ..Default::default() };
        save_config_to(&p, &site).unwrap();
        assert!(json().get("server").is_none(), "{}", json());
        let back = with_token_rule(load_config_from(&p).unwrap(), SITE);
        assert_eq!((back.token.as_str(), back.server.as_str()), ("t", SITE));
        // A sign-in on a developer server records it, so the site never gets that token.
        let dev = "http://localhost:8787";
        save_config_to(&p, &AgentConfig { server: dev.into(), ..site.clone() }).unwrap();
        assert_eq!(json()["server"], serde_json::json!(dev));
        assert_eq!(with_token_rule(load_config_from(&p).unwrap(), SITE).token, "");
        assert_eq!(with_token_rule(load_config_from(&p).unwrap(), dev).token, "t");
        // Signed out: no key.
        let out = AgentConfig { handle: "TestPilot".into(), ..Default::default() };
        assert!(!serde_json::to_string(&out).unwrap().contains("server"));
    }

    #[test]
    fn nothing_is_sent_where_the_rule_or_the_token_says_no() {
        let dev = "http://localhost:8787";
        let at = |active: &str, owner: &str, token: &str| Client::at(active, owner, token, http_agent(Duration::from_millis(1)));
        // The one place requests get their address from.
        assert_eq!(at(SITE, SITE, "t").url("/api/auth/me").as_deref(), Ok("https://wikelist.net/api/auth/me"));
        assert_eq!(at(SITE, "", "t").url("/x").as_deref(), Ok("https://wikelist.net/x"), "no key: a token of the site");
        assert_eq!(at(dev, dev, "t").url("/x").as_deref(), Ok("http://localhost:8787/x"));
        assert_eq!(at(dev, "", "").url("/x").as_deref(), Ok("http://localhost:8787/x"), "no token (the sign-in): nothing to keep apart");
        // A token issued by another address: refused before the network, on every path.
        for (active, owner) in [(SITE, dev), (dev, ""), (dev, SITE), (SITE, "https://wikelo.exemplo.com")] {
            let c = at(active, owner, "secret");
            assert!(c.url("/x").is_err(), "{active} {owner:?}");
            assert!(c.request(ApiRequest::Get("/api/inventory", None)).is_err());
            assert!(matches!(c.me(), Err(SendError::Unusable(_))));
            assert!(matches!(c.known_datasets(), Err(SendError::Unusable(_))));
        }
        // A base that breaks the transport rule (https, or plain http to this PC only):
        // nothing is sent, with a token or without.
        for active in ["http://wikelist.net", "http://192.168.1.20:8787", "https://wikelist.net/api", "https://user@wikelist.net", "ftp://wikelist.net", ""] {
            for token in ["", "secret"] {
                let c = at(active, active, token);
                assert!(c.url("/x").is_err(), "{active:?}");
                assert!(matches!(c.latest_release(), Err(SendError::Unusable(_))), "{active:?}");
            }
        }
        // The fixed address keeps the rule, and is what "open the site" opens.
        assert_eq!(transport_base(SITE).as_deref(), Ok(SITE));
        assert_eq!(openable_url(SITE, Link::Site).as_deref(), Some(SITE));
    }

    #[test]
    fn only_https_links_are_opened_and_releases_only_from_this_project() {
        let release = |s: &str| openable_url(s, Link::Release);
        let site = |s: &str| openable_url(s, Link::Site);
        let tag = "https://github.com/lfccs/wikelink/releases/tag/v1.0.0";
        assert_eq!(release(tag).as_deref(), Some(tag));
        assert_eq!(release("HTTPS://GitHub.com/lfccs/wikelink/releases/tag/v1.0.0").as_deref(), Some(tag), "scheme and host in any case");
        let all = "https://github.com/lfccs/wikelink/releases";
        assert_eq!(release(all).as_deref(), Some(all));
        for bad in [
            "http://github.com/lfccs/wikelink/releases/tag/v1.0.0",
            "https://github.com.evil.example/lfccs/wikelink/releases/tag/v1.0.0",
            "https://evil.example/lfccs/wikelink/releases/tag/v1.0.0",
            "https://github.com:8443/lfccs/wikelink/releases",
            "https://user@github.com/lfccs/wikelink/releases",
            "https://github.com/someone/else/releases/tag/v1.0.0",
            "https://github.com/lfccs/wikelink/releasesx",
            "https://github.com/lfccs/wikelink/archive/main.zip",
            "https://github.com/lfccs/wikelink/releases/../../other/repo",
            "https://github.com/lfccs/wikelink/releases/%2e%2e/x",
            "https://github.com/lfccs/wikelink/releases/tag/v1?x=1",
            "https://github.com/lfccs/wikelink/releases/tag/v1 x",
            "https://github.com/lfccs/wikelink/releases/tag/v1\n",
            " https://github.com/lfccs/wikelink/releases",
            "ms-settings:privacy",
            "steam://run/1",
            "file:///C:/Windows/System32/calc.exe",
            "\\\\server\\share\\wikelink.exe",
            "C:\\Users\\x\\Downloads\\wikelink.exe",
            "wikelink.exe",
            "javascript:alert(1)",
            "",
        ] {
            assert_eq!(release(bad), None, "{bad:?}");
        }
        // The site: a bare origin, https or plain http to this PC.
        assert_eq!(site("https://wikelist.net").as_deref(), Some("https://wikelist.net"));
        assert_eq!(site("https://wikelist.net/").as_deref(), Some("https://wikelist.net"));
        assert_eq!(site("http://localhost:8787").as_deref(), Some("http://localhost:8787"));
        for bad in [
            "http://wikelist.net",
            "wikelist.net",
            "https://wikelist.net/somewhere",
            "https://wiki list.net",
            "https://wikelist.net\u{0}",
            "file:///C:/x",
            "\\\\server\\share",
            "C:\\x\\wikelink.exe",
            "javascript:alert(1)",
            "ms-settings:",
        ] {
            assert_eq!(site(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn retry_after_is_read_in_seconds() {
        assert_eq!(parse_retry_after(" 7 "), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("99999999"), Some(Duration::from_secs(MAX_RETRY_AFTER)), "never longer than an hour");
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None, "the date form is not read");
        assert_eq!(parse_retry_after("-3"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn concurrent_writers_lose_no_update() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watcher.json");
        let base = AgentConfig { server: "https://x".into(), token: "t".into(), ..Default::default() };
        save_config_to(&p, &base).unwrap();
        // Many threads each add their own card, every one from a stale copy: with the lock
        // around the read-modify-write, all of them end up in the file.
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                let base = base.clone();
                std::thread::spawn(move || {
                    for j in 0..5 {
                        update_config_at(&p, &base, |d| {
                            d.overlay_widgets.insert(format!("c{i}-{j}"), OverlayWidget { x: 0.1, y: 0.2, pinned: false, w: None, h: None });
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let back = load_config_from(&p).unwrap();
        assert_eq!((back.token.as_str(), back.overlay_widgets.len()), ("t", 40));
    }

    #[test]
    fn the_overlay_refuses_oversized_answers() {
        let limit = 2 * 1024 * 1024;
        assert_eq!(OVERLAY_BODY_LIMIT, limit);
        // Content-Length: refused before reading when over the limit.
        assert!(!declared_too_big(None, limit), "no length (chunked): the read decides");
        assert!(!declared_too_big(Some("1234"), limit));
        assert!(!declared_too_big(Some(" 2097152 "), limit), "exactly the limit is fine");
        assert!(declared_too_big(Some("2097153"), limit));
        assert!(declared_too_big(Some("99999999999999"), limit));
        assert!(!declared_too_big(Some("abc"), limit), "unreadable: the read decides");
        assert!(!declared_too_big(Some("-5"), limit));
        // The read stops one byte past the limit, whatever the length said.
        assert_eq!(read_limited(&b"hello"[..], 5).unwrap(), b"hello");
        assert_eq!(read_limited(&b""[..], 5).unwrap(), b"");
        let err = read_limited(&b"hello!"[..], 5).unwrap_err();
        assert!(err.contains("grande demais"), "{err}");
        // An endless body is not read to the end.
        let endless = std::io::repeat(b'x');
        assert!(read_limited(endless, 1024).is_err());
        assert!(body_too_big(limit).contains("2 MiB"));
    }

    #[test]
    fn every_other_answer_has_a_limit_too() {
        // The small ones say their size in KiB, never "0 MiB".
        assert!(body_too_big(SMALL_BODY_LIMIT).contains("64 KiB"), "{}", body_too_big(SMALL_BODY_LIMIT));
        assert!(body_too_big(DATASETS_BODY_LIMIT).contains("1 MiB"));
        assert_eq!((ERROR_BODY_LIMIT, SMALL_BODY_LIMIT, UPLOAD_REPORT_LIMIT), (64 * 1024, 64 * 1024, 1024 * 1024));
        // An answer one byte over is an ordinary error; exactly the limit is read.
        let small = vec![b'x'; SMALL_BODY_LIMIT as usize];
        assert_eq!(read_limited(&small[..], SMALL_BODY_LIMIT).unwrap().len(), small.len());
        let over = vec![b'x'; SMALL_BODY_LIMIT as usize + 1];
        assert!(read_limited(&over[..], SMALL_BODY_LIMIT).unwrap_err().contains("grande demais"));
    }

    #[test]
    fn rotates_once_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("watcher.log");
        std::fs::write(&log, vec![b'x'; 2049]).unwrap();
        assert!(rotate_log_if_big(&log, 2048).unwrap(), "past the limit: rotated");
        let old = dir.path().join("watcher.log.1");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 2049, "the old lines moved to .1");
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0, "a fresh, empty log");
        assert!(!rotate_log_if_big(&log, 2048).unwrap(), "under the limit: nothing happens");
        // A second rotation replaces the previous `.1`.
        std::fs::write(&log, vec![b'y'; 3000]).unwrap();
        assert!(rotate_log_if_big(&log, 2048).unwrap());
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 3000);
        assert!(rotate_log_if_big(dir.path().join("missing.log").as_path(), 10).map(|r| !r).unwrap_or(true), "no file: nothing to rotate");
    }

    // ----- sign-in through the site ---------------------------------------------------

    /// One request the fake server below received.
    #[derive(Debug)]
    struct Seen {
        /// E.g. `POST /api/auth/link/poll HTTP/1.1`.
        line: String,
        authorization: Option<String>,
        body: String,
    }

    /// A fake server on this PC in place of the real one: it answers each connection, in
    /// order, with the next of `answers` (status, extra header lines each ending in `\r\n`,
    /// body) and closes it. Joining the handle gives what each request was.
    fn fake_server(answers: Vec<(u16, &'static str, &'static str)>) -> (String, std::thread::JoinHandle<Vec<Seen>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, headers, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut chunked, mut authorization) = (0usize, false, None);
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        let v = v.trim();
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.parse().unwrap();
                        } else if k.eq_ignore_ascii_case("transfer-encoding") {
                            chunked = v.eq_ignore_ascii_case("chunked");
                        } else if k.eq_ignore_ascii_case("authorization") {
                            authorization = Some(v.to_string());
                        }
                    }
                }
                let mut got = Vec::new();
                if chunked {
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).unwrap();
                        let mut chunk = vec![0u8; n + 2];
                        reader.read_exact(&mut chunk).unwrap();
                        got.extend_from_slice(&chunk[..n]);
                        if n == 0 {
                            break;
                        }
                    }
                } else {
                    got.resize(length, 0);
                    reader.read_exact(&mut got).unwrap();
                }
                seen.push(Seen { line: line.trim_end().to_string(), authorization, body: String::from_utf8_lossy(&got).into_owned() });
                let answer = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len());
                stream.write_all(answer.as_bytes()).unwrap();
            }
            seen
        });
        (base, handle)
    }

    /// An address on this PC where nothing listens.
    fn closed_port_base() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    const START_OK: &str = r#"{"code":"ABCD-EFGH","secret":"c2VjcmV0LXNlY3JldC1zZWNyZXQtc2VjcmV0LXNlY3I","expiresIn":600,"interval":3}"#;
    const SIGNED_IN: &str = r#"{"token":"tok-123","expiresAt":"2027-11-22T00:00:00Z","account":{"id":7,"handle":"TestPilot","displayName":"Test Pilot"}}"#;

    #[test]
    fn link_codes_are_two_groups_of_four() {
        // The pattern is `^[A-Z2-9]{4}-[A-Z2-9]{4}$` (the letters the site avoids still pass it).
        for good in ["ABCD-EFGH", "2345-6789", "ZZ99-A2B3", "IOLA-LOIL"] {
            assert!(is_link_code(good), "{good:?}");
        }
        for bad in [
            "",
            "ABCD-EFG",
            "ABCDE-FGH",
            "ABCDEFGHJ",
            "ABCD_EFGH",
            "ABCD EFGH",
            "abcd-efgh",
            "ABCd-EFGH",
            "ABC0-EFGH",
            "ABC1-EFGH",
            " ABCD-EFGH",
            "ABCD-EFGH ",
            "ABCD-EFGH\n",
            "ABCD-EFGH-JKMN",
            "ABÇ-EFGH",
            "AB/D-EFGH",
            "AB.D-EFGH",
            "ABCD-EF@H",
            "ABCD-EF%H",
            "ABCD--EFG",
        ] {
            assert!(!is_link_code(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_link_page_is_built_from_this_clients_own_address() {
        assert_eq!(link_page_at(SITE, "ABCD-EFGH").as_deref(), Some("https://wikelist.net/link"));
        assert_eq!(link_page_at("http://localhost:8787", "XY23-Z9AB").as_deref(), Some("http://localhost:8787/link"));
        // Whatever a code from an answer holds, it cannot move the page elsewhere: anything but
        // the nine plain characters opens nothing.
        for bad in [
            "ABCD-EFGH@evil.example",
            "@evil.example",
            "//evil.example",
            ".evil.example",
            "ABCD-EFGH/../../x",
            "ABCD-EFGH#x",
            "ABCD-EFGH&next=https://evil.example",
            "ABCD-EFGH?x",
            "ABCD%2FEFGH",
            "abcd-efgh",
            " ABCD-EFGH",
            "ABCD-EFGH\r\nX: y",
            "",
        ] {
            assert_eq!(link_page_at(SITE, bad), None, "{bad:?}");
        }
        // Only an address Wikelink may open is a base.
        for origin in ["http://wikelist.net", "https://wikelist.net/somewhere", "https://user@wikelist.net", "javascript:alert(1)", "file:///C:/x", ""] {
            assert_eq!(link_page_at(origin, "ABCD-EFGH"), None, "{origin:?}");
        }
        // The page is always on the site, at `/link`, and the code is not in the address.
        let page = link_page_at(SITE, "ZZZZ-2222").unwrap();
        assert!(!page.contains('?') && !page.contains("ZZZZ"), "{page}");
        let u = parse_web_url(&page).unwrap();
        assert_eq!((u.scheme, u.host.as_str(), u.port, u.path.as_str()), ("https", "wikelist.net", None, "/link"));
    }

    #[test]
    fn every_answer_to_link_start_has_a_meaning() {
        let s = Duration::from_secs;
        let start = |status: u16, body: &str| link_start_answer(status, body.as_bytes(), None);
        assert_eq!(
            start(200, START_OK),
            Ok(LinkCode { code: "ABCD-EFGH".into(), secret: "c2VjcmV0LXNlY3JldC1zZWNyZXQtc2VjcmV0LXNlY3I".into(), expires_in: s(600), interval: s(3) })
        );
        // Times out of reason are kept within bounds.
        let t = start(200, r#"{"code":"ABCD-EFGH","secret":"x","expiresIn":0,"interval":0}"#).unwrap();
        assert_eq!((t.expires_in, t.interval), (s(1), s(1)));
        let t = start(200, r#"{"code":"ABCD-EFGH","secret":"x","expiresIn":99999999,"interval":99999}"#).unwrap();
        assert_eq!((t.expires_in, t.interval), (s(LINK_MAX_WAIT_SECS), s(LINK_MAX_INTERVAL_SECS)));
        // A code or a secret that is not as it must be: refused, nothing is opened.
        let long_secret = format!(r#"{{"code":"ABCD-EFGH","secret":"{}","expiresIn":600,"interval":3}}"#, "x".repeat(LINK_SECRET_MAX + 1));
        for bad in [
            r#"{"code":"abcd-efgh","secret":"x","expiresIn":600,"interval":3}"#,
            r#"{"code":"ABCD-EFGH.evil.example","secret":"x","expiresIn":600,"interval":3}"#,
            r#"{"code":"../../x","secret":"x","expiresIn":600,"interval":3}"#,
            r#"{"secret":"x","expiresIn":600,"interval":3}"#,
            r#"{"code":"ABCD-EFGH","secret":"","expiresIn":600,"interval":3}"#,
            r#"{"code":"ABCD-EFGH","secret":"a b","expiresIn":600,"interval":3}"#,
            r#"{"code":"ABCD-EFGH","secret":"x","interval":3}"#,
            r#"{"code":"ABCD-EFGH","secret":"x","expiresIn":-1,"interval":3}"#,
            "<html>",
            "",
            long_secret.as_str(),
        ] {
            assert_eq!(start(200, bad), Err(unexpected_answer()), "{bad:?}");
        }
        // Refusals.
        assert_eq!(start(404, r#"{"error":"não encontrado"}"#), Err(LinkError::Unsupported));
        assert!(LinkError::Unsupported.to_string().contains("não tem suporte ao Wikelink"), "the existing message");
        assert_eq!(
            link_start_answer(429, br#"{"error":"muitas tentativas; tente de novo em 10 min"}"#, Some(s(600))),
            Err(LinkError::RateLimited { message: "muitas tentativas; tente de novo em 10 min".into(), retry_after: Some(s(600)) })
        );
        assert_eq!(start(429, ""), Err(LinkError::RateLimited { message: "HTTP 429".into(), retry_after: None }));
        assert_eq!(start(403, r#"{"error":"recusado"}"#), Err(LinkError::Failed("recusado".into())));
        assert_eq!(start(500, "<html>"), Err(LinkError::Failed("HTTP 500".into())));
        // Anything else that is not a 200 is not a code.
        for status in [201, 202, 204, 302] {
            assert_eq!(start(status, START_OK), Err(unexpected_answer()), "{status}");
        }
    }

    #[test]
    fn every_answer_to_link_poll_has_a_meaning() {
        let poll = |status: u16, body: &str| link_poll_answer(status, body.as_bytes(), None);
        assert_eq!(poll(202, r#"{"status":"pending"}"#), Ok(LinkPoll::Pending));
        assert_eq!(poll(202, ""), Ok(LinkPoll::Pending));
        // Confirmed: the same data the sign-in always gave.
        let signed = |handle: &str, name: &str| Ok(LinkPoll::SignedIn(SignIn { token: "tok-123".into(), handle: handle.into(), name: name.into() }));
        assert_eq!(poll(200, SIGNED_IN), signed("TestPilot", "Test Pilot"));
        assert_eq!(poll(200, r#"{"token":"tok-123","account":{"handle":"TestPilot","displayName":""}}"#), signed("TestPilot", "TestPilot"), "no display name: the handle");
        assert_eq!(poll(200, r#"{"token":"tok-123","account":{"handle":"TestPilot"}}"#), signed("TestPilot", "TestPilot"));
        for bad in [
            r#"{"token":"","account":{"handle":"TestPilot"}}"#,
            r#"{"token":"a b","account":{"handle":"TestPilot"}}"#,
            r#"{"token":"tok\r\nX-Other: 1","account":{"handle":"TestPilot"}}"#,
            r#"{"token":"tok-123"}"#,
            r#"{"account":{"handle":"TestPilot"}}"#,
            "<html>",
            "",
        ] {
            assert_eq!(poll(200, bad), Err(unexpected_answer()), "{bad:?}");
        }
        // Decided or gone.
        assert_eq!(poll(410, r#"{"status":"denied"}"#), Ok(LinkPoll::Denied));
        assert_eq!(poll(410, r#"{"status":"expired"}"#), Ok(LinkPoll::Expired));
        for other in [r#"{"status":"other"}"#, "{}", "", "<html>"] {
            assert_eq!(poll(410, other), Ok(LinkPoll::Expired), "{other:?}");
        }
        // Refusals.
        assert_eq!(poll(404, ""), Err(LinkError::Unsupported));
        assert_eq!(
            link_poll_answer(429, br#"{"error":"muitas tentativas; tente de novo em 1 min"}"#, Some(Duration::from_secs(60))),
            Err(LinkError::RateLimited { message: "muitas tentativas; tente de novo em 1 min".into(), retry_after: Some(Duration::from_secs(60)) })
        );
        assert_eq!(poll(401, r#"{"error":"faça login"}"#), Err(LinkError::Failed("faça login".into())));
        assert_eq!(poll(502, "bad gateway"), Err(LinkError::Failed("HTTP 502".into())));
        for status in [201, 204, 302] {
            assert_eq!(poll(status, SIGNED_IN), Err(unexpected_answer()), "{status}");
        }
    }

    #[test]
    fn server_text_is_plain_and_bounded() {
        assert_eq!(server_text("Test Pilot"), "Test Pilot");
        assert_eq!(server_text("bad\u{1b}[2J\r\nname\u{7}\u{9b}"), "bad[2Jname");
        assert_eq!(server_text(&"é".repeat(500)).chars().count(), SERVER_TEXT_MAX);
        // Error sentences, wherever they are printed or shown.
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(1000));
        assert_eq!(server_error(400, long.as_bytes()).chars().count(), SERVER_TEXT_MAX);
        assert_eq!(server_error(403, br#"{"error":"no\u001b[31m\nway"}"#), "no[31mway");
        assert_eq!(server_error(403, br#"{"error":"\u0007\r\n"}"#), "HTTP 403", "nothing left: the status");
    }

    #[test]
    fn handle_and_name_from_a_sign_in_are_plain_and_bounded() {
        let body = format!(r#"{{"token":"tok-123","account":{{"handle":"Test\u001bPilot","displayName":"{}\r\n"}}}}"#, "N".repeat(300));
        match link_poll_answer(200, body.as_bytes(), None) {
            Ok(LinkPoll::SignedIn(s)) => {
                assert_eq!(s.handle, "TestPilot");
                assert_eq!(s.name, "N".repeat(SERVER_TEXT_MAX));
            }
            other => panic!("unexpected {other:?}"),
        }
        // A display name of control characters only: the handle.
        let only = r#"{"token":"tok-123","account":{"handle":"TestPilot","displayName":"\u0007\u0008"}}"#;
        assert!(matches!(link_poll_answer(200, only.as_bytes(), None), Ok(LinkPoll::SignedIn(s)) if s.name == "TestPilot"));
    }

    #[test]
    fn a_sign_in_through_the_site_against_a_fake_server() {
        let (base, server) = fake_server(vec![(200, "", START_OK), (202, "", r#"{"status":"pending"}"#), (200, "", SIGNED_IN)]);
        let client = Client::at(&base, "", "", http_agent(Duration::from_secs(5)));
        let link = client.link_start("TEST-PC").unwrap();
        assert_eq!(link.code, "ABCD-EFGH");
        // Faster than the 3 s the answer asks for, for the test.
        let link = LinkCode { interval: Duration::from_millis(20), ..link };
        let outcome = client.wait_for_link(&link, &AtomicBool::new(false));
        assert_eq!(outcome, LinkOutcome::SignedIn(SignIn { token: "tok-123".into(), handle: "TestPilot".into(), name: "Test Pilot".into() }));
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0].line, "POST /api/auth/link/start HTTP/1.1");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&seen[0].body).unwrap(), serde_json::json!({ "device": "TEST-PC" }));
        for s in &seen[1..] {
            assert_eq!(s.line, "POST /api/auth/link/poll HTTP/1.1");
            assert_eq!(serde_json::from_str::<serde_json::Value>(&s.body).unwrap(), serde_json::json!({ "secret": link.secret }));
        }
        assert!(seen.iter().all(|s| s.authorization.is_none()), "no session token goes with a sign-in: {seen:?}");

        // A server without the sign-in through the site, and one that asks to wait.
        let (base, server) = fake_server(vec![(404, "", ""), (429, "Retry-After: 600\r\n", r#"{"error":"muitas tentativas; tente de novo em 10 min"}"#)]);
        let client = Client::at(&base, "", "", http_agent(Duration::from_secs(5)));
        assert_eq!(client.link_start("TEST-PC"), Err(LinkError::Unsupported));
        assert_eq!(client.link_start("TEST-PC"), Err(LinkError::RateLimited { message: "muitas tentativas; tente de novo em 10 min".into(), retry_after: Some(Duration::from_secs(600)) }));
        server.join().unwrap();
        // No server at all: the network error, with the address.
        let closed = closed_port_base();
        match Client::at(&closed, "", "", http_agent(Duration::from_secs(5))).link_start("TEST-PC") {
            Err(LinkError::Failed(m)) => assert!(m.contains(&closed), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn waiting_ends_on_a_refusal_an_expiry_a_cancel_or_a_failure() {
        let fast = || LinkCode { code: "ABCD-EFGH".into(), secret: "s3cr3t".into(), expires_in: Duration::from_secs(60), interval: Duration::from_millis(20) };
        let at = |base: &str| Client::at(base, "", "", http_agent(Duration::from_secs(5)));
        let off = AtomicBool::new(false);
        // Refused in the browser.
        let (base, server) = fake_server(vec![(202, "", ""), (410, "", r#"{"status":"denied"}"#)]);
        assert_eq!(at(&base).wait_for_link(&fast(), &off), LinkOutcome::Denied);
        assert_eq!(server.join().unwrap().len(), 2);
        // Expired, or unknown, on the server.
        let (base, server) = fake_server(vec![(410, "", r#"{"status":"expired"}"#)]);
        assert_eq!(at(&base).wait_for_link(&fast(), &off), LinkOutcome::Expired);
        server.join().unwrap();
        // A 429 only makes the next pause longer (here `Retry-After: 0`: the interval).
        let (base, server) = fake_server(vec![(429, "Retry-After: 0\r\n", r#"{"error":"muitas tentativas"}"#), (200, "", SIGNED_IN)]);
        assert!(matches!(at(&base).wait_for_link(&fast(), &off), LinkOutcome::SignedIn(_)));
        server.join().unwrap();
        // Any other refusal ends it, with the server's words.
        let (base, server) = fake_server(vec![(500, "", r#"{"error":"falhou"}"#)]);
        assert_eq!(at(&base).wait_for_link(&fast(), &off), LinkOutcome::Failed(LinkError::Failed("falhou".into())));
        server.join().unwrap();
        // The code's own time is up: nothing more is asked (nothing listens there).
        let closed = closed_port_base();
        let short = LinkCode { expires_in: Duration::from_millis(50), interval: Duration::from_secs(30), ..fast() };
        let started = Instant::now();
        assert_eq!(at(&closed).wait_for_link(&short, &off), LinkOutcome::Expired);
        assert!(started.elapsed() < Duration::from_secs(5));
        // Cancelled: nothing is asked either, and it ends at once.
        let started = Instant::now();
        assert_eq!(at(&closed).wait_for_link(&fast(), &AtomicBool::new(true)), LinkOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(1));
        // No server: the network error ends it.
        assert!(matches!(at(&closed).wait_for_link(&fast(), &off), LinkOutcome::Failed(LinkError::Failed(_))));
    }

    #[test]
    fn signing_out_tells_the_server_with_the_token_and_never_waits_long() {
        let (base, server) = fake_server(vec![(204, "", "")]);
        assert_eq!(Client::at(&base, &base, "tok-123", http_agent(Duration::from_secs(5))).sign_out(), Ok(()));
        let seen = server.join().unwrap();
        assert_eq!(seen[0].line, "POST /api/auth/logout HTTP/1.1");
        assert_eq!(seen[0].authorization.as_deref(), Some("Bearer tok-123"));
        // A refusal, or no server: an error the callers ignore, and soon.
        let (base, server) = fake_server(vec![(401, "", r#"{"error":"faça login"}"#)]);
        assert!(Client::at(&base, &base, "old", http_agent(Duration::from_secs(5))).sign_out().is_err());
        server.join().unwrap();
        let closed = closed_port_base();
        let started = Instant::now();
        assert!(Client::at(&closed, &closed, "tok", http_agent(SIGN_OUT_TIMEOUT)).sign_out().is_err());
        assert!(started.elapsed() <= SIGN_OUT_TIMEOUT + Duration::from_secs(2));
        // Nothing to end without a token, and a token of another address is never sent.
        assert_eq!(Client::at(&closed, "", "", http_agent(SIGN_OUT_TIMEOUT)).sign_out(), Ok(()));
        assert!(Client::at(&closed, SITE, "tok", http_agent(SIGN_OUT_TIMEOUT)).sign_out().is_err());
        assert_eq!(sign_out_on_server(&AgentConfig::default()), Ok(()));
    }

    #[test]
    fn no_code_path_asks_for_a_secret_word_any_more() {
        // Spelt in pieces, so that this test does not find itself.
        let word = concat!("pass", "word");
        let sources = [("agent.rs", include_str!("agent.rs")), ("main.rs", include_str!("main.rs")), ("gui.rs", include_str!("gui.rs")), ("tray.rs", include_str!("tray.rs"))];
        for (name, text) in sources {
            assert!(!text.to_ascii_lowercase().contains(word), "{name} still mentions it");
        }
        // Nor the old sign-in route.
        assert!(!include_str!("agent.rs").contains(concat!("agent", "-login")));
        assert!(!include_str!("main.rs").contains(concat!("WIKELINK_", "PASS")));
    }

    // ----- blueprints in old logs ---------------------------------------------------------

    fn blueprint_line(at: &str, name: &str) -> String {
        format!(
            "<{at}> [Notice] <SHUDEvent_OnNotification> Added notification \"Received Blueprint: {name}: \" [3] to queue. New queue size: 1, MissionId: [00000000-0000-0000-0000-000000000000], ObjectiveId: [] [Team_CoreGameplayFeatures][Missions][Comms]\n"
        )
    }

    /// The name the game gives its `i`-th backup (a higher `i` is a newer one).
    fn backup_name(i: usize) -> String {
        format!("Game Build(12881860) {:02} Oct 26 (13 27 {:02}).log", 1 + i / 60, i % 60)
    }

    /// A backup log with these blueprints, after a line that is not one.
    fn write_backup(dir: &Path, i: usize, blueprints: &[&str]) -> PathBuf {
        let p = dir.join(backup_name(i));
        let mut text = String::from("<2026-10-01T13:27:00.000Z> [Notice] <StatObjLoad 0x800 Format> whatever\n");
        for name in blueprints {
            text.push_str(&blueprint_line("2026-10-01T14:00:00.000Z", name));
        }
        std::fs::write(&p, text).unwrap();
        p
    }

    /// One pass as the LIVE channel of `account`; `ok: false` refuses every batch. The
    /// accepted events go into `sent`.
    fn pass_with(state: &Path, dir: &Path, account: &str, ok: bool, sent: &mut Vec<(bool, TrackerOutput)>) -> BacklogPass {
        let parser = wikelo_log::LineParser::new();
        let stop = AtomicBool::new(false);
        let mut send = |chunk: &[(bool, TrackerOutput)]| -> Result<(), BacklogSendError> {
            if !ok {
                return Err(BacklogSendError::Later("offline".into()));
            }
            sent.extend(chunk.iter().cloned());
            Ok(())
        };
        backlog_pass(state, account, "LIVE", dir, &parser, &stop, &mut send)
    }

    fn sent_names(sent: &[(bool, TrackerOutput)]) -> Vec<String> {
        sent.iter()
            .map(|(_, e)| match e {
                TrackerOutput::BlueprintReceived { name, .. } => name.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn listing(dir: &Path) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> =
            std::fs::read_dir(dir).unwrap().flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap().len())).collect();
        v.sort();
        v
    }

    #[test]
    fn a_pass_sends_the_blueprints_of_old_logs_newest_first_and_writes_nothing_there() {
        let tmp = tempfile::tempdir().unwrap();
        let (game, data) = (tmp.path().join("logbackups"), tmp.path().join("data"));
        std::fs::create_dir_all(&game).unwrap();
        write_backup(&game, 0, &["Old One"]);
        write_backup(&game, 1, &[]);
        write_backup(&game, 2, &["New One", "New Two"]);
        std::fs::write(game.join("notes.txt"), "not a log").unwrap();
        let before = listing(&game);
        let state = data.join("logbackups-read.json");
        let mut sent = Vec::new();
        let pass = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!(pass, BacklogPass { read: 3, blueprints: 3, refused: 0, failed: 0, left: 0 });
        assert_eq!(sent_names(&sent), vec!["New One", "New Two", "Old One"]);
        assert!(sent.iter().all(|(replay, e)| *replay
            && matches!(e, TrackerOutput::BlueprintReceived { at, mission_id: None, .. } if at == "2026-10-01T14:00:00.000Z")));
        assert_eq!(listing(&game), before, "the game's folder is left as it was");
        // Only the list is in the data folder: every temporary file was renamed over it.
        assert_eq!(listing(&data).into_iter().map(|(n, _)| n).collect::<Vec<_>>(), vec!["logbackups-read.json"]);
    }

    #[test]
    fn files_already_read_are_skipped_and_a_changed_size_is_read_again() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        write_backup(&game, 0, &["First"]);
        let grows = write_backup(&game, 1, &["Second"]);
        let mut sent = Vec::new();
        assert_eq!(pass_with(&state, &game, "TestPilot", true, &mut sent).read, 2);
        sent.clear();
        let again = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!(again, BacklogPass::default(), "nothing new");
        assert!(sent.is_empty());
        std::fs::write(&grows, format!("{}{}", std::fs::read_to_string(&grows).unwrap(), blueprint_line("2026-10-01T15:00:00.000Z", "Third"))).unwrap();
        let changed = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!((changed.read, changed.blueprints), (1, 2));
        assert_eq!(sent_names(&sent), vec!["Second", "Third"]);
    }

    #[test]
    fn the_list_of_read_files_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        let p = write_backup(&game, 0, &["First"]);
        let mut sent = Vec::new();
        pass_with(&state, &game, "TestPilot", true, &mut sent);
        // A new process: only what is on disk.
        let size = std::fs::metadata(&p).unwrap().len();
        let loaded = BacklogState::load(&state, "TestPilot");
        assert!(loaded.is_read("LIVE", &backup_name(0), size));
        assert!(!loaded.is_read("PTU", &backup_name(0), size), "per channel");
        sent.clear();
        assert_eq!(pass_with(&state, &game, "TestPilot", true, &mut sent).read, 0);
        // Another account starts from nothing; an unreadable list too.
        assert_eq!(BacklogState::load(&state, "Someone"), BacklogState { account: "Someone".into(), ..Default::default() });
        std::fs::write(&state, "{ half").unwrap();
        assert_eq!(pass_with(&state, &game, "TestPilot", true, &mut sent).read, 1);
        // A file gone from the folder leaves the list.
        std::fs::remove_file(&p).unwrap();
        write_backup(&game, 1, &[]);
        pass_with(&state, &game, "TestPilot", true, &mut sent);
        let loaded = BacklogState::load(&state, "TestPilot");
        assert_eq!(loaded.channels["LIVE"].keys().cloned().collect::<Vec<_>>(), vec![backup_name(1)]);
    }

    #[test]
    fn a_failed_send_leaves_the_file_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        write_backup(&game, 0, &["Older"]);
        write_backup(&game, 1, &["Newer"]);
        let mut sent = Vec::new();
        let failed = pass_with(&state, &game, "TestPilot", false, &mut sent);
        assert_eq!(failed, BacklogPass { read: 0, blueprints: 0, refused: 0, failed: 1, left: 0 }, "the pass ends at the first refusal");
        assert!(BacklogState::load(&state, "TestPilot").channels.is_empty());
        let next = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!(next.read, 2);
        assert_eq!(sent_names(&sent), vec!["Newer", "Older"]);
    }

    #[test]
    fn a_pass_reads_at_most_200_files_the_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        let total = BACKLOG_FILES_PER_PASS + 3;
        for i in 0..total {
            write_backup(&game, i, &[]);
        }
        let mut sent = Vec::new();
        let first = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!(first, BacklogPass { read: BACKLOG_FILES_PER_PASS, blueprints: 0, refused: 0, failed: 0, left: 3 });
        let loaded = BacklogState::load(&state, "TestPilot");
        let read = &loaded.channels["LIVE"];
        assert!(read.contains_key(&backup_name(total - 1)) && !read.contains_key(&backup_name(0)), "the oldest wait");
        let second = pass_with(&state, &game, "TestPilot", true, &mut sent);
        assert_eq!(second, BacklogPass { read: 3, blueprints: 0, refused: 0, failed: 0, left: 0 });
    }

    #[test]
    fn events_go_out_in_bounded_batches() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let names: Vec<String> = (0..BACKLOG_BATCH + 5).map(|i| format!("Blueprint {i}")).collect();
        write_backup(&game, 0, &names.iter().map(String::as_str).collect::<Vec<_>>());
        let parser = wikelo_log::LineParser::new();
        let stop = AtomicBool::new(false);
        let mut sizes = Vec::new();
        let mut send = |chunk: &[(bool, TrackerOutput)]| -> Result<(), BacklogSendError> {
            sizes.push(chunk.len());
            Ok(())
        };
        let pass = backlog_pass(&tmp.path().join("logbackups-read.json"), "TestPilot", "LIVE", &game, &parser, &stop, &mut send);
        assert_eq!(pass.blueprints, BACKLOG_BATCH + 5);
        assert_eq!(sizes, vec![BACKLOG_BATCH, 5]);
    }

    /// A pass whose `send` answers `refuse` for a batch with a blueprint named `bad` and
    /// accepts every other one into `sent`.
    fn pass_refusing(state: &Path, dir: &Path, bad: &str, refuse: BacklogSendError, sent: &mut Vec<(bool, TrackerOutput)>) -> BacklogPass {
        let parser = wikelo_log::LineParser::new();
        let stop = AtomicBool::new(false);
        let mut send = |chunk: &[(bool, TrackerOutput)]| -> Result<(), BacklogSendError> {
            if chunk.iter().any(|(_, e)| matches!(e, TrackerOutput::BlueprintReceived { name, .. } if name == bad)) {
                return Err(refuse.clone());
            }
            sent.extend(chunk.iter().cloned());
            Ok(())
        };
        backlog_pass(state, "TestPilot", "LIVE", dir, &parser, &stop, &mut send)
    }

    #[test]
    fn a_refused_file_is_set_aside_and_the_next_one_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        write_backup(&game, 0, &["Older"]);
        let bad = write_backup(&game, 1, &["Refused"]);
        let mut sent = Vec::new();
        let pass = pass_refusing(&state, &game, "Refused", BacklogSendError::Refused("HTTP 400".into()), &mut sent);
        assert_eq!(pass, BacklogPass { read: 1, blueprints: 1, refused: 1, failed: 0, left: 0 });
        assert_eq!(sent_names(&sent), vec!["Older"], "the older file is still read");
        // The refused file counts as read: the next pass sends nothing again.
        let size = std::fs::metadata(&bad).unwrap().len();
        assert!(BacklogState::load(&state, "TestPilot").is_read("LIVE", &backup_name(1), size));
        sent.clear();
        assert_eq!(pass_refusing(&state, &game, "Refused", BacklogSendError::Refused("HTTP 400".into()), &mut sent), BacklogPass::default());
        assert!(sent.is_empty());
    }

    #[test]
    fn a_network_error_ends_the_pass_and_leaves_the_files_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let state = tmp.path().join("logbackups-read.json");
        write_backup(&game, 0, &["Older"]);
        write_backup(&game, 1, &["Unlucky"]);
        let mut sent = Vec::new();
        let pass = pass_refusing(&state, &game, "Unlucky", BacklogSendError::Later("timeout".into()), &mut sent);
        assert_eq!(pass, BacklogPass { read: 0, blueprints: 0, refused: 0, failed: 1, left: 0 });
        assert!(sent.is_empty(), "the older file waits for the next pass");
        assert!(BacklogState::load(&state, "TestPilot").channels.is_empty());
    }

    #[test]
    fn a_repeated_event_in_one_file_goes_once() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("logbackups");
        std::fs::create_dir_all(&game).unwrap();
        let p = write_backup(&game, 0, &["Twice", "Twice", "Other"]);
        // The same name at another time is another event.
        std::fs::write(&p, format!("{}{}", std::fs::read_to_string(&p).unwrap(), blueprint_line("2026-10-01T15:00:00.000Z", "Twice"))).unwrap();
        let mut sent = Vec::new();
        let pass = pass_with(&tmp.path().join("logbackups-read.json"), &game, "TestPilot", true, &mut sent);
        assert_eq!(pass.blueprints, 3);
        assert_eq!(sent_names(&sent), vec!["Twice", "Other", "Twice"]);
    }

    #[test]
    fn every_installation_has_its_own_list_of_old_logs() {
        let live = PathBuf::from(r"C:\Games\StarCitizen\LIVE\logbackups");
        let ptu_c = PathBuf::from(r"C:\Games\StarCitizen\PTU\logbackups");
        let ptu_g = PathBuf::from(r"G:\Games\StarCitizen\PTU\logbackups");
        let others = vec![("LIVE".to_string(), PathBuf::from(r"c:\games\starcitizen\live\LOGBACKUPS")), ("PTU".to_string(), ptu_c.clone()), ("PTU".to_string(), ptu_g.clone()), ("PTU".to_string(), ptu_g.clone())];
        let folders = backlog_folders("LIVE", &live, &others);
        // The followed one first, under the key it always had; itself (whatever the case) and a repeat listed once.
        assert_eq!(folders.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(), vec![live.clone(), ptu_c, ptu_g]);
        assert_eq!(folders[0].0, "LIVE");
        assert_eq!(folders[1].0, r"PTU|c:\games\starcitizen\ptu\logbackups");
        assert_ne!(folders[1].0, folders[2].0, "the same channel on two drives: two lists");
        assert_eq!(backlog_folders("LIVE", &live, &[]), vec![("LIVE".to_string(), live)]);
    }

    #[test]
    fn a_check_of_old_logs_goes_on_while_files_wait_and_nothing_failed() {
        let pass = |read, refused, failed, left| BacklogPass { read, blueprints: 0, refused, failed, left };
        assert_eq!(backlog_sum(&[pass(200, 1, 0, 853), pass(8, 0, 0, 0), BacklogPass { read: 1, blueprints: 7, refused: 0, failed: 1, left: 2 }]), BacklogPass { read: 209, blueprints: 7, refused: 1, failed: 1, left: 855 });
        assert!(backlog_goes_on(&pass(200, 0, 0, 853)));
        assert!(backlog_goes_on(&pass(0, 3, 0, 5)), "set aside counts as taken");
        assert!(!backlog_goes_on(&pass(200, 0, 0, 0)), "nothing left");
        assert!(!backlog_goes_on(&pass(200, 0, 1, 853)), "a failure waits for the next hourly check");
        assert!(!backlog_goes_on(&pass(0, 0, 0, 853)), "a round that took nothing never loops");
    }

    #[test]
    fn backlog_answers_are_classified() {
        assert_eq!(backlog_verdict(Ok(Ack { enabled: true })), Ok(()));
        assert!(matches!(backlog_verdict(Ok(Ack { enabled: false })), Err(BacklogSendError::Later(_))), "paused from the site");
        let refusal = |status: u16| backlog_verdict(Err((Some(status), SendError::Rejected("no".into()))));
        for status in [400, 403, 404, 413, 422, 451] {
            assert!(matches!(refusal(status), Err(BacklogSendError::Refused(_))), "{status}");
        }
        for status in [401, 408, 409, 429, 500, 502, 503] {
            assert!(matches!(refusal(status), Err(BacklogSendError::Later(_))), "{status}");
        }
        assert!(matches!(backlog_verdict(Err((None, SendError::Other("timeout".into())))), Err(BacklogSendError::Later(_))), "no answer");
        assert!(matches!(backlog_verdict(Err((Some(200), SendError::Other("bad json".into())))), Err(BacklogSendError::Later(_))));
        assert!(!is_final_refusal(None) && !is_final_refusal(Some(399)) && !is_final_refusal(Some(500)));
    }

    #[test]
    fn a_refusal_of_the_events_request_keeps_its_status() {
        for (status, kind) in [(400u16, "refused"), (413, "refused"), (422, "refused"), (429, "later"), (503, "later")] {
            let (base, server) = fake_server(vec![(status, "", r#"{"error":"no"}"#)]);
            let client = Client::at(&base, &base, "tok", http_agent(Duration::from_secs(5)));
            let answer = client.send_with_status("LIVE", "pc", false, &[]);
            assert!(matches!(answer, Err((Some(s), _)) if s == status), "{status}");
            let verdict = backlog_verdict(answer);
            match kind {
                "refused" => assert!(matches!(verdict, Err(BacklogSendError::Refused(_))), "{status}: {verdict:?}"),
                _ => assert!(matches!(verdict, Err(BacklogSendError::Later(_))), "{status}: {verdict:?}"),
            }
            server.join().unwrap();
        }
        // No server at all: no status, and the pass would end.
        let closed = closed_port_base();
        let answer = Client::at(&closed, &closed, "tok", http_agent(Duration::from_secs(5))).send_with_status("LIVE", "pc", false, &[]);
        assert!(matches!(answer, Err((None, _))));
    }
}

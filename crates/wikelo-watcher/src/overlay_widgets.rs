//! The overlay's cards and toasts as plain data: which cards there are, where they sit (as
//! fractions of the monitor, so another resolution keeps the arrangement), how that is read
//! from and written to watcher.json, the queue of toasts (the alerts for new notices and a
//! change of location, the overlay's own warnings, the reward choices), the widgets menu
//! (which cards, whether the search and the alerts are on, where the search strip sits, what
//! Esc does), the search strip (what its one field lists, the filter chips and the groups,
//! the inventory it reads), the blueprints card (its stack of screens, the rows in view, the
//! details and the missions' blueprints it reads), and the source pins (which converting items the closed overlay shows
//! under a pinned one). No window here: everything is testable on its own.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::agent::OverlayWidget;
use crate::overlay_model::{self as model, OverlayData};
use crate::overlay_sync::{Limit, Outcome, next_delay};
use crate::{tr, trf};

/// Width of every card (logical px).
pub const CARD_WIDTH: f32 = 320.0;
/// Height of a card's header at the normal text size: the part of a card that must stay on
/// screen (see [`card_header_height`]).
pub const CARD_HEADER: f32 = 28.0;

/// Height of a card's header at text size `s`: it grows with the text, like the bodies (the
/// normal height for a size that is not a positive number).
pub fn card_header_height(s: f32) -> f32 {
    if s.is_finite() && s > 0.0 { CARD_HEADER * s } else { CARD_HEADER }
}
/// Room between the default places and the edges of the screen, and between two cards.
const EDGE: f32 = 24.0;
/// Top of the default places, below the bar and the search strip the open overlay shows.
const TOP: f32 = 150.0;
pub const DEFAULT_OPACITY: f32 = 0.9;
pub const MIN_OPACITY: f32 = 0.2;
/// Smallest and largest size a card can be dragged to (logical px; never past the screen).
pub const MIN_CARD: [f32; 2] = [220.0, 90.0];
pub const MAX_CARD: [f32; 2] = [720.0, 1000.0];
/// The text sizes of the cards, smallest to largest (1 = the normal size).
pub const TEXT_SCALES: [f32; 5] = [0.85, 1.0, 1.15, 1.3, 1.5];

/// How many cards there are.
pub const COUNT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widget {
    Fixados,
    Falta,
    Aqui,
    /// The list of records (notices); saved as `avisos`, its name before the alerts.
    Registros,
    Missoes,
    /// Every blueprint of the channel, searchable, with whether the account has it; a row
    /// opens to its craft or to where to get it. Drawn only while the overlay is open.
    Blueprints,
}

impl Widget {
    pub const ALL: [Widget; COUNT] = [Widget::Fixados, Widget::Falta, Widget::Aqui, Widget::Registros, Widget::Missoes, Widget::Blueprints];

    /// Its place in [`Widget::ALL`] and in a layout.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Its name in watcher.json.
    pub fn key(self) -> &'static str {
        match self {
            Widget::Fixados => "fixados",
            Widget::Falta => "falta",
            Widget::Aqui => "aqui",
            Widget::Registros => "avisos",
            Widget::Missoes => "missoes",
            Widget::Blueprints => "blueprints",
        }
    }

    /// Its name on screen (card header, widgets menu); never saved.
    pub fn title(self) -> &'static str {
        match self {
            Widget::Fixados => tr!("Pinned", "Fixados"),
            Widget::Falta => tr!("Missing", "Falta"),
            Widget::Aqui => tr!("Here", "Aqui"),
            Widget::Registros => tr!("Records", "Registros"),
            Widget::Missoes => tr!("Missions", "Missões"),
            Widget::Blueprints => tr!("Blueprints", "Blueprints"),
        }
    }
}

/// Where a card sits (its top-left corner as fractions of the screen), whether it stays up
/// while the overlay is closed, and the size the player dragged it to (fractions of the
/// screen; `None`: the normal width, as tall as its rows).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardPlace {
    pub frac: [f32; 2],
    pub pinned: bool,
    pub size: Option<[f32; 2]>,
}

pub type Layout = [CardPlace; COUNT];

fn to_frac(px: [f32; 2], screen: [f32; 2]) -> [f32; 2] {
    [px[0] / screen[0], px[1] / screen[1]]
}

/// A card size (logical px) kept between [`MIN_CARD`] and [`MAX_CARD`] and never bigger than
/// the screen; `None` when the size or the screen is not a finite positive number.
pub fn clamp_size(px: [f32; 2], screen: [f32; 2]) -> Option<[f32; 2]> {
    if !(px.iter().all(|v| v.is_finite()) && screen.iter().all(|s| s.is_finite() && *s > 0.0)) {
        return None;
    }
    let axis = |i: usize| {
        let lo = MIN_CARD[i].min(screen[i]);
        let hi = MAX_CARD[i].min(screen[i]);
        px[i].clamp(lo, hi)
    };
    Some([axis(0), axis(1)])
}

/// A saved size (fractions of the screen) in logical px, clamped (see [`clamp_size`]).
pub fn size_px(frac: [f32; 2], screen: [f32; 2]) -> Option<[f32; 2]> {
    clamp_size([frac[0] * screen[0], frac[1] * screen[1]], screen)
}

/// The size to store for a card dragged to `px` (logical px): clamped, as fractions.
pub fn size_frac(px: [f32; 2], screen: [f32; 2]) -> Option<[f32; 2]> {
    clamp_size(px, screen).map(|p| to_frac(p, screen))
}

/// The size (fractions) of a card whose corner was dragged by `delta` (logical px) from its
/// size now (the saved one, else `measured`): within the limits and, from where the card
/// sits, inside the screen. `None` when something is not a number.
pub fn resized(place: &CardPlace, measured: [f32; 2], delta: [f32; 2], screen: [f32; 2]) -> Option<[f32; 2]> {
    let now = place.size.and_then(|s| size_px(s, screen)).unwrap_or(measured);
    // `min` would turn a NaN into the room: checked first.
    if !now.iter().chain(&delta).all(|v| v.is_finite()) {
        return None;
    }
    let room = [screen[0] * (1.0 - place.frac[0]), screen[1] * (1.0 - place.frac[1])];
    size_frac([(now[0] + delta[0]).min(room[0]), (now[1] + delta[1]).min(room[1])], screen)
}

/// A saved size checked again for this screen: unchanged when it fits, clamped when it does
/// not, `None` when it is not a number.
fn reclamp_size(size: Option<[f32; 2]>, screen: [f32; 2]) -> Option<[f32; 2]> {
    let frac = size?;
    let raw = [frac[0] * screen[0], frac[1] * screen[1]];
    let clamped = clamp_size(raw, screen)?;
    Some(if clamped == raw { frac } else { to_frac(clamped, screen) })
}

/// The text size from watcher.json: the nearest of [`TEXT_SCALES`]; 1 without one (or a
/// value that is not a number).
pub fn text_scale_from(saved: Option<f32>) -> f32 {
    let Some(s) = saved.filter(|s| s.is_finite()) else { return 1.0 };
    TEXT_SCALES[nearest_scale(s)]
}

fn nearest_scale(s: f32) -> usize {
    let mut best = 0;
    for (i, step) in TEXT_SCALES.iter().enumerate() {
        if (step - s).abs() < (TEXT_SCALES[best] - s).abs() {
            best = i;
        }
    }
    best
}

/// One step larger (`up`) or smaller than `current`, stopping at the ends.
pub fn text_scale_step(current: f32, up: bool) -> f32 {
    let i = nearest_scale(if current.is_finite() { current } else { 1.0 });
    let j = if up { (i + 1).min(TEXT_SCALES.len() - 1) } else { i.saturating_sub(1) };
    TEXT_SCALES[j]
}

/// `115%`.
pub fn text_scale_label(s: f32) -> String {
    format!("{}%", (s * 100.0).round() as i32)
}

/// Keep a card of size `card` on a screen of size `screen` (both logical px): its corner
/// between the top-left of the screen and the point where the card touches the right / bottom
/// edge. Anything not finite is 0.
pub fn clamp_frac(frac: [f32; 2], card: [f32; 2], screen: [f32; 2]) -> [f32; 2] {
    let axis = |f: f32, card: f32, screen: f32| {
        let f = if f.is_finite() { f } else { 0.0 };
        // `max` drops a NaN, so `hi` is always a number >= 0.
        let hi = if screen > 0.0 { ((screen - card) / screen).max(0.0) } else { 0.0 };
        f.clamp(0.0, hi)
    };
    [axis(frac[0], card[0], screen[0]), axis(frac[1], card[1], screen[1])]
}

/// Clamp every card of `layout` into a screen of size `screen`, each with its own measured
/// size (`cards`, logical px, in [`Widget::ALL`] order), a dragged size first brought within
/// its limits for this screen. Returns whether any place or size changed.
pub fn clamp_layout(layout: &mut Layout, cards: &[[f32; 2]; COUNT], screen: [f32; 2]) -> bool {
    let mut moved = false;
    for (place, card) in layout.iter_mut().zip(cards) {
        let size = reclamp_size(place.size, screen);
        if size != place.size {
            place.size = size;
            moved = true;
        }
        // The measured size lags a frame behind a drag: the larger of the two keeps the card
        // on screen either way.
        let card = match size.and_then(|s| size_px(s, screen)) {
            Some(s) => [card[0].max(s[0]), card[1].max(s[1])],
            None => *card,
        };
        let clamped = clamp_frac(place.frac, card, screen);
        if clamped != place.frac {
            place.frac = clamped;
            moved = true;
        }
    }
    moved
}

/// The first-time layout: "Fixados" and, left of it, "Falta" pinned at the top right;
/// "Registros", "Aqui" and "Missões" unpinned (seen only while the overlay is open), and
/// "Blueprints" unpinned under "Missões", from the middle of the screen down (the top row has
/// no room left for a sixth card).
pub fn default_place(w: Widget, screen: [f32; 2]) -> CardPlace {
    let from_right = |n: f32| screen[0] - n * (CARD_WIDTH + EDGE);
    let (x, y, pinned) = match w {
        Widget::Fixados => (from_right(1.0), TOP, true),
        Widget::Falta => (from_right(2.0), TOP, true),
        Widget::Registros => (EDGE, TOP, false),
        Widget::Aqui => (2.0 * EDGE + CARD_WIDTH, TOP, false),
        Widget::Missoes => (from_right(3.0), TOP, false),
        Widget::Blueprints => (from_right(3.0), screen[1] / 2.0, false),
    };
    CardPlace { frac: clamp_frac(to_frac([x, y], screen), [CARD_WIDTH, CARD_HEADER], screen), pinned, size: None }
}

/// The layout from watcher.json: each saved card as it was, the others at their default
/// place. `legacy` is the corner of the old single-window overlay relative to this monitor
/// (logical px): it places "Fixados" when that card has no saved place yet. Everything is
/// clamped into the screen (the monitor may have changed); a saved size only when both of
/// its numbers are there, within its limits. Saved cards this overlay does not have (the
/// earlier free "Busca" card, a newer overlay's) are ignored.
pub fn resolve(saved: &BTreeMap<String, OverlayWidget>, legacy: Option<[f32; 2]>, screen: [f32; 2]) -> Layout {
    Widget::ALL.map(|w| {
        let place = match (saved.get(w.key()), w, legacy) {
            (Some(s), _, _) => CardPlace { frac: [s.x, s.y], pinned: s.pinned, size: s.w.zip(s.h).and_then(|(sw, sh)| size_px([sw, sh], screen)).map(|p| to_frac(p, screen)) },
            (None, Widget::Fixados, Some(p)) => CardPlace { frac: to_frac(p, screen), pinned: true, size: None },
            (None, _, _) => default_place(w, screen),
        };
        let card = place.size.and_then(|s| size_px(s, screen)).unwrap_or([CARD_WIDTH, CARD_HEADER]);
        CardPlace { frac: clamp_frac(place.frac, card, screen), ..place }
    })
}

/// What watcher.json keeps of a layout.
pub fn to_config(layout: &Layout) -> BTreeMap<String, OverlayWidget> {
    Widget::ALL
        .iter()
        .map(|w| {
            let p = layout[w.index()];
            (w.key().to_string(), OverlayWidget { x: p.frac[0], y: p.frac[1], pinned: p.pinned, w: p.size.map(|s| s[0]), h: p.size.map(|s| s[1]) })
        })
        .collect()
}

/// The order the cards are stacked in, back to front, by their index in [`Widget::ALL`].
pub type ZOrder = [usize; COUNT];

/// The first stacking: [`Widget::ALL`] order, the last one in front.
pub fn default_z() -> ZOrder {
    std::array::from_fn(|i| i)
}

/// Bring card `i` to the front, the others keeping their order behind it.
pub fn raise(z: &mut ZOrder, i: usize) {
    if let Some(p) = z.iter().position(|&c| c == i) {
        z[p..].rotate_left(1);
    }
}

/// The saved opacity, kept between [`MIN_OPACITY`] and 1; [`DEFAULT_OPACITY`] without one.
pub fn opacity_from(saved: Option<f32>) -> f32 {
    saved.filter(|o| o.is_finite()).map_or(DEFAULT_OPACITY, |o| o.clamp(MIN_OPACITY, 1.0))
}

// ----- widgets menu ---------------------------------------------------------------------

/// The name of the search strip in watcher.json's list of widgets switched off.
pub const SEARCH_KEY: &str = "pesquisa";
/// The name of the alerts (new notices popping up for a few seconds) in that list.
pub const ALERTS_KEY: &str = "alertas";
/// How many switches the widgets menu has: the cards, the search and the alerts.
pub const PARTS: usize = COUNT + 2;

/// One switch of the widgets menu: a card, the search strip, or the alerts (not a card:
/// nothing to drag, only on or off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Card(Widget),
    Search,
    Alerts,
}

impl Part {
    /// Every switch, in the menu's order: the cards, the search, the alerts.
    pub const ALL: [Part; PARTS] = [
        Part::Card(Widget::Fixados),
        Part::Card(Widget::Falta),
        Part::Card(Widget::Aqui),
        Part::Card(Widget::Registros),
        Part::Card(Widget::Missoes),
        Part::Card(Widget::Blueprints),
        Part::Search,
        Part::Alerts,
    ];

    fn index(self) -> usize {
        match self {
            Part::Card(w) => w.index(),
            Part::Search => COUNT,
            Part::Alerts => COUNT + 1,
        }
    }

    /// Its name in watcher.json.
    pub fn key(self) -> &'static str {
        match self {
            Part::Card(w) => w.key(),
            Part::Search => SEARCH_KEY,
            Part::Alerts => ALERTS_KEY,
        }
    }

    /// Its name in the widgets menu; never saved.
    pub fn title(self) -> &'static str {
        match self {
            Part::Card(w) => w.title(),
            Part::Search => tr!("Search", "Busca"),
            Part::Alerts => tr!("Alerts", "Alertas"),
        }
    }

    /// Listed in the widgets menu: every switch but the blueprints card's, which has a button
    /// of its own in the bar (shown or not, it is saved like the others).
    pub fn in_menu(self) -> bool {
        self != Part::Card(Widget::Blueprints)
    }
}

/// Which widgets are shown (the widgets menu's switches; a pinned card keeps its pin while
/// switched off). Everything is shown unless watcher.json says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown([bool; PARTS]);

impl Default for Shown {
    fn default() -> Self {
        Self([true; PARTS])
    }
}

impl Shown {
    /// From watcher.json's names switched off (names this overlay does not know are ignored).
    pub fn from_saved(hidden: &[String]) -> Self {
        Self(Part::ALL.map(|p| !hidden.iter().any(|k| k.trim() == p.key())))
    }

    pub fn get(&self, p: Part) -> bool {
        self.0[p.index()]
    }

    /// Switch `p`; `true` when that changed something.
    pub fn set(&mut self, p: Part, on: bool) -> bool {
        std::mem::replace(&mut self.0[p.index()], on) != on
    }

    /// "mostrar todos" / "ocultar todos"; `true` when that changed something.
    pub fn set_all(&mut self, on: bool) -> bool {
        let changed = self.0.iter().any(|&s| s != on);
        self.0 = [on; PARTS];
        changed
    }

    /// At least one widget is shown.
    pub fn any(&self) -> bool {
        self.0.iter().any(|&s| s)
    }

    /// What watcher.json keeps: every widget by name, and whether it is shown.
    pub fn to_config(self) -> BTreeMap<String, bool> {
        Part::ALL.iter().map(|p| (p.key().to_string(), self.get(*p))).collect()
    }
}

/// The saved place of the search strip (its top-left corner as fractions of the screen):
/// `None` (under the bar) without one, or with one that is not a number.
pub fn search_pos_from(saved: Option<[f32; 2]>) -> Option<[f32; 2]> {
    saved.filter(|p| p.iter().all(|v| v.is_finite()))
}

/// The place of the search strip whose grip was dragged by `delta` (logical px) from `now`
/// (its top-left corner on screen, relative to the screen's): as fractions, its row (`row`,
/// logical px) kept on the screen.
pub fn search_dragged(now: [f32; 2], delta: [f32; 2], row: [f32; 2], screen: [f32; 2]) -> [f32; 2] {
    let moved = [now[0] + delta[0], now[1] + delta[1]];
    let frac = if screen[0] > 0.0 && screen[1] > 0.0 { to_frac(moved, screen) } else { [0.0, 0.0] };
    clamp_frac(frac, row, screen)
}

/// The search strip back on the screen (another monitor, a narrower bar): unchanged when
/// its row (`row`, logical px) fits, else clamped (see [`clamp_frac`]); `None` stays under
/// the bar.
pub fn clamp_search(pos: Option<[f32; 2]>, row: [f32; 2], screen: [f32; 2]) -> Option<[f32; 2]> {
    pos.map(|p| clamp_frac(p, row, screen))
}

/// The results of the search strip unfold upwards when there is more room above its row
/// (`above`, logical px) than below it (`below`); downwards otherwise.
pub fn unfold_up(above: f32, below: f32) -> bool {
    above > below
}

/// What Esc does while the overlay is open, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscStep {
    /// Close the widgets menu.
    Menu,
    /// Empty the search field.
    Search,
    /// Close the overlay.
    Close,
}

/// What one Esc does: the widgets menu first, then the search text, then the overlay.
pub fn esc_step(menu_open: bool, search_text: &str) -> EscStep {
    if menu_open {
        EscStep::Menu
    } else if !search_text.is_empty() {
        EscStep::Search
    } else {
        EscStep::Close
    }
}

// ----- toasts ---------------------------------------------------------------------------

/// How long a toast stays.
pub const TOAST_FOR: Duration = Duration::from_secs(6);
/// Most toasts on screen; a new one pushes the oldest out.
pub const MAX_TOASTS: usize = 3;
/// A toast slides in for this long and fades out over the last [`TOAST_FADE`].
const TOAST_SLIDE: Duration = Duration::from_millis(250);
const TOAST_FADE: Duration = Duration::from_millis(400);
/// Repaint interval while a toast moves.
const ANIMATION_FRAME: Duration = Duration::from_millis(16);
/// Notices remembered as already shown.
const SEEN_CAP: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// Stable for the toast's life (its egui area id).
    pub id: u64,
    pub title: String,
    pub body: String,
    /// Draw it in the warning colour (a change waiting on the site).
    pub warn: bool,
    pub born: Instant,
}

/// The toasts on screen and what is needed to raise each one only once: the notices already
/// seen (across answers), the last known location, and the channel of the last answer.
#[derive(Debug, Default)]
pub struct Toasts {
    seen: HashSet<String>,
    order: VecDeque<String>,
    /// The channel of the last answer; `None` before the first one (or after a sign-out):
    /// that answer only sets what counts as already seen.
    channel: Option<String>,
    location: Option<String>,
    live: Vec<Toast>,
    next_id: u64,
    /// Reward choices already announced (or seen with the overlay open), oldest first.
    choices_seen: HashSet<i64>,
    choices_order: VecDeque<i64>,
}

/// Title of the toast for a reward waiting to be chosen.
pub fn choice_toast() -> &'static str {
    tr!("Reward to choose", "Recompensa a escolher")
}

impl Toasts {
    /// Take one answer: with `alerts` (the widgets menu's "Alertas" switch) a toast per notice
    /// never seen before, and one when the location changed. The first answer (and the first
    /// of another channel) raises nothing; neither does an answer while the alerts are off,
    /// but what it carries counts as seen all the same (switching them on replays nothing).
    pub fn feed(&mut self, d: &OverlayData, alerts: bool, now: Instant) {
        let primed = self.channel.as_deref() == Some(d.channel.as_str());
        let mut fresh: Vec<&model::Notice> = d.notices.iter().filter(|n| self.remember(model::notice_key(n))).collect();
        let here = d.at_location.as_deref().filter(|id| !id.trim().is_empty());
        if primed && alerts {
            // Oldest first, so the newest ends up nearest the corner.
            fresh.sort_by(|a, b| a.at.cmp(&b.at));
            for n in fresh {
                let changes = model::changes_line(n);
                let body = if changes.is_empty() { tr!("inventory updated", "inventário atualizado").to_string() } else { changes };
                let title = if n.is_pending() { tr!("Pending on the site", "Pendente no site") } else { tr!("Recorded", "Registrado") };
                self.push(title, body, n.is_pending(), now);
            }
            if let Some(id) = here
                && self.location.as_deref() != Some(id)
            {
                let name = model::location_name(id, &d.location_names);
                let body = match model::here_rows(d).len() {
                    0 => name,
                    1 => trf!("{name} · 1 useful item here", "{name} · 1 item útil aqui"),
                    n => trf!("{name} · {n} useful items here", "{name} · {n} itens úteis aqui"),
                };
                self.push(tr!("Location", "Local"), body, false, now);
            }
        }
        // An unknown location keeps the last one: coming back to it is no change.
        if let Some(id) = here {
            self.location = Some(id.to_string());
        }
        self.channel = Some(d.channel.clone());
    }

    /// The next answer only sets what counts as seen (signed out, or the server changed).
    pub fn unprime(&mut self) {
        self.channel = None;
    }

    /// Another server, account or channel: what was seen there says nothing here (the same
    /// numbers can name other notices and choices). The next answer only sets what counts as
    /// seen, except that its reward choices are announced, like at start. The toasts on
    /// screen stay.
    pub fn reset_identity(&mut self) {
        self.seen.clear();
        self.order.clear();
        self.channel = None;
        self.location = None;
        self.choices_seen.clear();
        self.choices_order.clear();
    }

    /// Take the reward choices of an answer: one toast per choice never seen before when
    /// `announce` (the overlay is closed); open, the card shows them and they only count as
    /// seen. A choice is never announced twice, whatever comes in between. Unlike notices,
    /// the choices waiting at start are announced: they still need the player.
    pub fn feed_choices(&mut self, choices: &[model::Choice], announce: bool, now: Instant) {
        for c in choices {
            let Some(id) = c.id else { continue };
            if self.choices_seen.contains(&id) {
                continue;
            }
            self.choices_seen.insert(id);
            self.choices_order.push_back(id);
            while self.choices_order.len() > SEEN_CAP {
                if let Some(old) = self.choices_order.pop_front() {
                    self.choices_seen.remove(&old);
                }
            }
            if announce {
                let body = match c.title.as_deref().filter(|t| !t.trim().is_empty()) {
                    Some(t) => format!("{t} · Shift+Tab"),
                    None => "Shift+Tab".to_string(),
                };
                self.push(choice_toast(), body, true, now);
            }
        }
    }

    /// A toast of the overlay's own (not from an answer), in the warning colour.
    pub fn warn(&mut self, title: &str, body: &str, now: Instant) {
        self.push(title, body.to_string(), true, now);
    }

    /// `true` the first time `key` is seen.
    fn remember(&mut self, key: String) -> bool {
        if self.seen.contains(&key) {
            return false;
        }
        self.seen.insert(key.clone());
        self.order.push_back(key);
        while self.order.len() > SEEN_CAP {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    fn push(&mut self, title: &str, body: String, warn: bool, now: Instant) {
        self.live.push(Toast { id: self.next_id, title: title.to_string(), body, warn, born: now });
        self.next_id += 1;
        if self.live.len() > MAX_TOASTS {
            let extra = self.live.len() - MAX_TOASTS;
            self.live.drain(..extra);
        }
    }

    /// Drop the toasts whose time is up.
    pub fn prune(&mut self, now: Instant) {
        self.live.retain(|t| now.saturating_duration_since(t.born) < TOAST_FOR);
    }

    /// The toasts on screen, oldest first.
    pub fn live(&self) -> &[Toast] {
        &self.live
    }

    /// When the next frame is needed for the toasts: soon while one slides or fades, at the
    /// start of the next fade otherwise; `None` without toasts.
    pub fn next_repaint(&self, now: Instant) -> Option<Duration> {
        self.live
            .iter()
            .filter_map(|t| {
                let age = now.saturating_duration_since(t.born);
                if age < TOAST_SLIDE || (age + TOAST_FADE >= TOAST_FOR && age < TOAST_FOR) {
                    Some(ANIMATION_FRAME)
                } else if age < TOAST_FOR {
                    Some(TOAST_FOR - TOAST_FADE - age)
                } else {
                    None
                }
            })
            .min()
    }
}

/// How far a toast of this age has slid in (0 = still off the edge, 1 = in place) and its
/// opacity (fading to 0 over the last [`TOAST_FADE`]).
pub fn toast_phase(age: Duration) -> (f32, f32) {
    let t = (age.as_secs_f32() / TOAST_SLIDE.as_secs_f32()).min(1.0);
    let slide = 1.0 - (1.0 - t).powi(3);
    let alpha = (TOAST_FOR.saturating_sub(age).as_secs_f32() / TOAST_FADE.as_secs_f32()).min(1.0);
    (slide, alpha)
}

// ----- search ---------------------------------------------------------------------------

/// How long the search text must stay put before it is sent (older servers, without the
/// index: the search is then the server's).
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// How long a search text is (in characters, after trimming) for it to be searched.
pub const SEARCH_MIN_CHARS: usize = 2;
pub const SEARCH_MAX_CHARS: usize = 64;
/// The inventory is read again this often while the overlay is open.
pub const INVENTORY_EVERY: Duration = Duration::from_secs(30);

/// The query for `text`: trimmed, when it is [`SEARCH_MIN_CHARS`] to [`SEARCH_MAX_CHARS`]
/// characters long.
pub fn search_query(text: &str) -> Option<String> {
    let q = text.trim();
    (SEARCH_MIN_CHARS..=SEARCH_MAX_CHARS).contains(&q.chars().count()).then(|| q.to_string())
}

/// What the search field lists for its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// An empty field: no chips and no results panel.
    Nothing,
    /// Too short (or too long): it says so.
    TooShort,
    /// What matches every word of this (trimmed) text.
    Query(String),
}

/// What the field lists for `text`: nothing without a text, else a query of
/// [`SEARCH_MIN_CHARS`] to [`SEARCH_MAX_CHARS`] characters.
pub fn listing(text: &str) -> Listing {
    let t = text.trim();
    if t.is_empty() {
        return Listing::Nothing;
    }
    search_query(t).map_or(Listing::TooShort, Listing::Query)
}

/// The filter chips under the search field: everything (the default; the choice is never
/// saved) or one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Tudo,
    Inventario,
    Missoes,
}

/// The groups the results come in, each under its heading, in this order. Blueprints are
/// searched only in their own card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Inventario,
    Missoes,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Inventario => tr!("Inventory", "Inventário"),
            Group::Missoes => tr!("Missions", "Missões"),
        }
    }
}

/// The chips for a text whose search found `items` items and `missions` missions: none when
/// it found nothing; else "Tudo" and one chip per group that has results, with its count.
pub fn chips(items: usize, missions: usize) -> Vec<(Filter, String)> {
    if items == 0 && missions == 0 {
        return Vec::new();
    }
    let mut out = vec![(Filter::Tudo, tr!("All", "Tudo").to_string())];
    if items > 0 {
        out.push((Filter::Inventario, trf!("Inventory {items}", "Inventário {items}")));
    }
    if missions > 0 {
        out.push((Filter::Missoes, trf!("Missions {missions}", "Missões {missions}")));
    }
    out
}

/// The filter in force: the one chosen while its group has results (its chip is there),
/// else "Tudo".
pub fn effective_filter(chosen: Filter, items: usize, missions: usize) -> Filter {
    match chosen {
        Filter::Inventario if items > 0 => Filter::Inventario,
        Filter::Missoes if missions > 0 => Filter::Missoes,
        _ => Filter::Tudo,
    }
}

/// The groups drawn under `filter`, in order (Inventário, then Missões), each only when it
/// has results.
pub fn groups(filter: Filter, items: usize, missions: usize) -> Vec<Group> {
    let mut out = Vec::new();
    if items > 0 && matches!(filter, Filter::Tudo | Filter::Inventario) {
        out.push(Group::Inventario);
    }
    if missions > 0 && matches!(filter, Filter::Tudo | Filter::Missoes) {
        out.push(Group::Missoes);
    }
    out
}

/// The line under a capped list: how many more items (or missions) there are.
pub fn more_line(more: usize, missions: bool) -> String {
    match (more, missions) {
        (1, false) => tr!("1 more item; type to filter", "mais 1 item; digite para filtrar").into(),
        (1, true) => tr!("1 more mission; type to filter", "mais 1 missão; digite para filtrar").into(),
        (n, false) => trf!("{n} more items; type to filter", "mais {n} itens; digite para filtrar"),
        (n, true) => trf!("{n} more missions; type to filter", "mais {n} missões; digite para filtrar"),
    }
}

/// What a search answered: the results, or why there are none.
pub type SearchAnswer = Result<model::SearchResults, String>;

/// The search text and, against an older server (no index), the debounce, the one request in
/// flight and the last answer kept. An answer for anything but the text as it is now is
/// dropped. Only the ids and names of an answer are used (the quantities come from the
/// inventory, the pins from the overlay's data), so a write never makes it stale.
#[derive(Debug, Default)]
pub struct Search {
    pub text: String,
    /// The text changed at this moment and has not been sent since.
    edited: Option<Instant>,
    seq: u64,
    /// The request on its way: its number and query.
    in_flight: Option<(u64, String)>,
    /// The last answer kept: its query and what it said.
    answer: Option<(String, SearchAnswer)>,
}

impl Search {
    /// The player changed the text.
    pub fn edited(&mut self, now: Instant) {
        self.edited = Some(now);
    }

    /// The request to send now, numbered: once the text has stayed put for
    /// [`SEARCH_DEBOUNCE`] and nothing is in flight (an answer then makes it due again), for a
    /// query of a searched length (see [`search_query`]) that is not already answered.
    pub fn due(&mut self, now: Instant) -> Option<(u64, String)> {
        let edited = self.edited?;
        if now.saturating_duration_since(edited) < SEARCH_DEBOUNCE || self.in_flight.is_some() {
            return None;
        }
        self.edited = None;
        let q = search_query(&self.text)?;
        if self.answer.as_ref().is_some_and(|(aq, a)| *aq == q && a.is_ok()) {
            return None;
        }
        self.seq += 1;
        self.in_flight = Some((self.seq, q.clone()));
        Some((self.seq, q))
    }

    /// How long until [`Search::due`] may have something (`None`: nothing waiting).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.edited.map(|e| SEARCH_DEBOUNCE.saturating_sub(now.saturating_duration_since(e)))
    }

    /// The answer to request `seq`; `true` when it is kept (it answers the text as it is now).
    pub fn answer(&mut self, seq: u64, answer: SearchAnswer) -> bool {
        if !self.in_flight.as_ref().is_some_and(|(s, _)| *s == seq) {
            return false;
        }
        let Some((_, q)) = self.in_flight.take() else { return false };
        if search_query(&self.text).as_deref() != Some(q.as_str()) {
            return false;
        }
        self.answer = Some((q, answer));
        true
    }

    /// There is something to search for: the results panel shows.
    pub fn active(&self) -> bool {
        search_query(&self.text).is_some()
    }

    /// The answer for the text as it is now; `None` while it is being searched.
    pub fn shown(&self) -> Option<&SearchAnswer> {
        let q = search_query(&self.text)?;
        self.answer.as_ref().filter(|(aq, _)| *aq == q).map(|(_, a)| a)
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        self.text.clear();
        self.edited = None;
        self.answer = None;
    }

    /// A text with no answer and nothing on its way (typed while the index was still coming,
    /// or the server turned out to be an older one): searched at once.
    pub fn wake(&mut self, now: Instant) {
        if self.active() && self.edited.is_none() && self.in_flight.is_none() && self.shown().is_none() {
            self.due_now(now);
        }
    }

    /// A moment that makes the text due at once (for [`Search::due`]), unless the player is
    /// still typing.
    fn due_now(&mut self, now: Instant) {
        let past = now.checked_sub(SEARCH_DEBOUNCE).unwrap_or(now);
        self.edited = Some(self.edited.map_or(past, |e| e.max(past)));
    }

    /// Request `seq` will never be answered (it belongs to an endpoint left behind): search
    /// again.
    pub fn cancel(&mut self, seq: u64, now: Instant) {
        if self.in_flight.as_ref().is_some_and(|(s, _)| *s == seq) {
            self.in_flight = None;
            if self.active() {
                self.due_now(now);
            }
        }
    }

    /// Another server, account or channel: the answer and the request on its way are
    /// forgotten, and the text (if any) is searched again there.
    pub fn reset(&mut self, now: Instant) {
        self.answer = None;
        self.in_flight = None;
        if self.active() {
            self.due_now(now);
        }
    }
}

/// The search text as kept: at most [`SEARCH_MAX_CHARS`] characters.
pub fn cut_search_text(mut text: String) -> String {
    if let Some((at, _)) = text.char_indices().nth(SEARCH_MAX_CHARS) {
        text.truncate(at);
    }
    text
}

/// The player's whole inventory (`GET /api/inventory`): the quantities and the places of the
/// search's item rows. Read the first time it is wanted, again [`INVENTORY_EVERY`] after each
/// answer while it is and after a write settles; a failure is tried again with the reads'
/// back-off. The caller asks [`InventoryFetch::due`] only while it is wanted (see
/// `Overlay::pump`). What was read stays until another answer (or another endpoint) replaces
/// it.
#[derive(Debug, Default)]
pub struct InventoryFetch {
    /// The last answer, and the number of the last write it includes (see
    /// `overlay_sync::Engine::on_inventory`).
    held: Option<(HashMap<String, model::Owned>, u64)>,
    /// When it is asked for again (`None`: at once, never asked yet).
    due_at: Option<Instant>,
    seq: u64,
    /// The request on its way.
    in_flight: Option<u64>,
    /// Failures in a row (for the back-off), and the last one.
    failures: u32,
    error: Option<String>,
    /// Changes whenever `held` does (for what is built from it).
    revision: u64,
}

impl InventoryFetch {
    /// The request to send now, numbered, if one is due and none is on its way.
    pub fn due(&mut self, now: Instant) -> Option<u64> {
        if self.in_flight.is_some() || self.due_at.is_some_and(|at| now < at) {
            return None;
        }
        self.seq += 1;
        self.in_flight = Some(self.seq);
        Some(self.seq)
    }

    /// How long until [`InventoryFetch::due`] may have something (`None`: one is on its way).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.in_flight.is_some() {
            return None;
        }
        Some(self.due_at.map_or(Duration::ZERO, |at| at.saturating_duration_since(now)))
    }

    /// The answer to request `seq`, including the writes up to `mark`; `false`: not the
    /// request on its way, dropped.
    pub fn answer(&mut self, seq: u64, owned: HashMap<String, model::Owned>, mark: u64, now: Instant) -> bool {
        if self.in_flight != Some(seq) {
            return false;
        }
        self.in_flight = None;
        self.held = Some((owned, mark));
        self.revision += 1;
        self.failures = 0;
        self.error = None;
        self.due_at = Some(now + INVENTORY_EVERY);
        true
    }

    /// Request `seq` failed (`limited`: a 429, whose wait the engine keeps): what was read
    /// stays, and it is asked again after the back-off.
    pub fn failed(&mut self, seq: u64, error: String, limited: bool, now: Instant) {
        if self.in_flight != Some(seq) {
            return;
        }
        self.in_flight = None;
        self.error = Some(error);
        if limited {
            self.due_at = Some(now);
        } else {
            self.failures = self.failures.saturating_add(1);
            self.due_at = Some(now + next_delay(Outcome::Unreachable, self.failures));
        }
    }

    /// Request `seq` will never be answered (an endpoint left behind): asked again.
    pub fn cancel(&mut self, seq: u64, now: Instant) {
        if self.in_flight == Some(seq) {
            self.in_flight = None;
            self.due_at = Some(now);
        }
    }

    /// A write settled: asked again as soon as it is wanted.
    pub fn refetch(&mut self, now: Instant) {
        self.due_at = Some(self.due_at.map_or(now, |at| at.min(now)));
    }

    /// A change a read settled that the answer does not include: added to it for good.
    pub fn bake(&mut self, change: &model::Local) {
        if let Some((owned, _)) = &mut self.held {
            model::bake_owned(owned, change);
            self.revision += 1;
        }
    }

    /// What was read, once an answer came.
    pub fn held(&self) -> Option<&HashMap<String, model::Owned>> {
        self.held.as_ref().map(|(owned, _)| owned)
    }

    /// The number of the last write the answer includes (0 without one).
    pub fn mark(&self) -> u64 {
        self.held.as_ref().map_or(0, |(_, mark)| *mark)
    }

    /// Why the last request failed (none since the last answer).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Changes whenever what was read does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Another endpoint: nothing known, asked at once (the numbers keep counting).
    pub fn reset(&mut self) {
        let (seq, revision) = (self.seq, self.revision + 1);
        *self = Self { seq, revision, ..Self::default() };
    }
}

/// The channel's locations for the move picker's filter (`GET /api/channels/{channel}/dataset`,
/// the list the site's location picker reads): asked for once something is typed in the
/// filter, and kept for the endpoint (they change with the game's version); a failure is tried
/// again with the reads' back-off while it is wanted. One request at a time (the engine sends
/// one at all).
#[derive(Debug, Default)]
pub struct LocationsFetch {
    held: Option<Vec<model::Location>>,
    in_flight: bool,
    /// When it may be asked for again after a failure.
    due_at: Option<Instant>,
    failures: u32,
    error: Option<String>,
}

impl LocationsFetch {
    /// Whether to ask now (and then it is on its way): not held yet, nothing on its way, and
    /// past the back-off of a failure.
    pub fn due(&mut self, now: Instant) -> bool {
        if self.held.is_some() || self.in_flight || self.due_at.is_some_and(|at| now < at) {
            return false;
        }
        self.in_flight = true;
        true
    }

    /// How long until [`LocationsFetch::due`] may say yes (`None`: held, or on its way).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.held.is_some() || self.in_flight {
            return None;
        }
        Some(self.due_at.map_or(Duration::ZERO, |at| at.saturating_duration_since(now)))
    }

    pub fn answer(&mut self, list: Vec<model::Location>) {
        self.in_flight = false;
        self.held = Some(list);
        self.failures = 0;
        self.error = None;
    }

    /// The request failed (`limited`: a 429, whose wait the engine keeps).
    pub fn failed(&mut self, error: String, limited: bool, now: Instant) {
        self.in_flight = false;
        self.error = Some(error);
        if limited {
            self.due_at = Some(now);
        } else {
            self.failures = self.failures.saturating_add(1);
            self.due_at = Some(now + next_delay(Outcome::Unreachable, self.failures));
        }
    }

    /// The request will never be answered (an endpoint left behind): asked again.
    pub fn cancel(&mut self, now: Instant) {
        self.in_flight = false;
        self.due_at = Some(now);
    }

    pub fn held(&self) -> Option<&[model::Location]> {
        self.held.as_deref()
    }

    /// Why the last request failed (none since the last answer).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Another endpoint: nothing known.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ----- the blueprints card --------------------------------------------------------------

/// What a screen of the blueprints card shows of one blueprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BpPanel {
    /// The blueprint and its craft (a click on its name in the list or in a mission's pool).
    Craft,
    /// The missions that give it ("Onde conseguir", from the list or from its craft screen).
    Where,
}

/// A screen of the blueprints card. The card shows one screen at a time: the list (nothing
/// on the [`ScreenStack`]) or the screen on top of the stack, with "Voltar" at the top.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BpScreen {
    /// One blueprint (its key), and which of its screens.
    Blueprint(String, BpPanel),
    /// The blueprints a mission gives ("Blueprints da missão", a click on a line of "Onde
    /// conseguir"): its contract id, and the title and giver of that line, drawn until the
    /// mission's answer comes.
    Mission { contract: String, title: String, giver: Option<String> },
}

impl BpScreen {
    /// The blueprint this screen is about, if it is one of a blueprint's screens.
    pub fn blueprint(&self) -> Option<&str> {
        match self {
            BpScreen::Blueprint(key, _) => Some(key.as_str()),
            BpScreen::Mission { .. } => None,
        }
    }

    /// The contract this screen is about, if it is a mission's screen.
    pub fn contract(&self) -> Option<&str> {
        match self {
            BpScreen::Mission { contract, .. } => Some(contract.as_str()),
            BpScreen::Blueprint(..) => None,
        }
    }
}

/// Most screens kept on the stack; past it the oldest are dropped (their "Voltar" then ends
/// at the list sooner).
pub const SCREENS_MAX: usize = 16;

/// The screens opened since the list, oldest first, the one shown last. "Voltar" goes back
/// one screen, however deep (list -> blueprint -> onde conseguir -> missão -> another
/// blueprint -> ...).
pub type ScreenStack = Vec<BpScreen>;

/// Open `screen` over the ones on `stack`; the screen already on top again changes nothing.
/// Past [`SCREENS_MAX`] the oldest screens are dropped.
pub fn screen_push(stack: &mut ScreenStack, screen: BpScreen) {
    if stack.last() == Some(&screen) {
        return;
    }
    stack.push(screen);
    if stack.len() > SCREENS_MAX {
        let extra = stack.len() - SCREENS_MAX;
        stack.drain(..extra);
    }
}

/// "Voltar": the screen on top goes, and the one under it shows again (none left: the list).
pub fn screen_back(stack: &mut ScreenStack) {
    let _ = stack.pop();
}

/// Where "Voltar" on the top screen of `stack` leads, for its hover text.
pub fn back_hint(stack: &[BpScreen]) -> &'static str {
    match stack.len().checked_sub(2).and_then(|i| stack.get(i)) {
        None => tr!("back to the list", "voltar para a lista"),
        Some(BpScreen::Blueprint(_, BpPanel::Craft)) => tr!("back to the blueprint", "voltar para a blueprint"),
        Some(BpScreen::Blueprint(_, BpPanel::Where)) => tr!("back to where to get it", "voltar para onde conseguir"),
        Some(BpScreen::Mission { .. }) => tr!("back to the mission", "voltar para a missão"),
    }
}

/// The blueprint whose detail the top screen of `stack` draws (its craft or its missions).
pub fn open_blueprint(stack: &[BpScreen]) -> Option<&str> {
    stack.last().and_then(|s| s.blueprint())
}

/// The contract whose blueprints the top screen of `stack` draws.
pub fn open_contract(stack: &[BpScreen]) -> Option<&str> {
    stack.last().and_then(|s| s.contract())
}

/// Which of `count` rows of `pitch` px each, laid out from `top`, fall inside the visible
/// band from `clip_top` to `clip_bottom` (one more on each side for a row cut in half): the
/// card lays out only those, and leaves the room of the others empty, so a list of thousands
/// costs a screenful of rows per frame.
pub fn visible_rows(top: f32, clip_top: f32, clip_bottom: f32, count: usize, pitch: f32) -> std::ops::Range<usize> {
    if count == 0 || !pitch.is_finite() || pitch <= 0.0 || !top.is_finite() || !clip_top.is_finite() || !clip_bottom.is_finite() {
        return 0..0;
    }
    // A float cast to usize saturates: far below the band is 0, far past it is `count`.
    let first = (((clip_top - top) / pitch).floor().max(0.0) as usize).min(count);
    let end = ((((clip_bottom - top) / pitch).ceil().max(0.0) as usize).saturating_add(1)).min(count);
    first.min(end)..end
}

/// Blueprint details (and missions' blueprints) kept for the session; past it, they are all
/// forgotten but the one arriving (a few hundred screens opened is far more than a session
/// sees).
pub const DETAILS_KEPT: usize = 200;

/// What a request for one keyed thing got: a blueprint's detail ([`DetailAnswer`]) or a
/// mission's blueprints ([`MissionAnswer`]).
#[derive(Debug, PartialEq)]
pub enum KeyedAnswer<T> {
    /// What was asked for and its ETag (none when the server sent none).
    Data(T, Option<String>),
    /// 304: what is held is still the server's.
    NotModified,
    /// No answer or an error, the server's sentence, and whether it was a 429.
    Failed(String, Option<Limit>),
}

/// What a request for a blueprint's detail got.
pub type DetailAnswer = KeyedAnswer<model::BlueprintDetail>;
/// What a request for a mission's blueprints got.
pub type MissionAnswer = KeyedAnswer<model::MissionBlueprints>;

/// The details of the blueprints opened in the card, by key.
pub type DetailFetch = KeyedFetch<model::BlueprintDetail>;
/// The blueprints of the missions opened in the card, by contract.
pub type MissionFetch = KeyedFetch<model::MissionBlueprints>;

/// What the screens of the blueprints card read, kept for the session, each with its ETag: a
/// screen opened asks for what it draws (again, with `If-None-Match` when it is held: a 304
/// keeps it), one request at a time. A failure is said on the screen (what was held stays)
/// and asked again only when the screen is opened again, or after the wait of a 429 while it
/// is still open. At most [`DETAILS_KEPT`] are kept.
#[derive(Debug)]
pub struct KeyedFetch<T> {
    held: HashMap<String, (T, Option<String>)>,
    /// The screen opened whose request has not gone out yet.
    wanted: Option<String>,
    in_flight: Option<String>,
    /// The last failure, and the key it was for.
    error: Option<(String, String)>,
}

impl<T> Default for KeyedFetch<T> {
    fn default() -> Self {
        Self { held: HashMap::new(), wanted: None, in_flight: None, error: None }
    }
}

impl<T> KeyedFetch<T> {
    /// The screen of `key` was opened: what it draws is asked for.
    pub fn open(&mut self, key: &str) {
        self.wanted = Some(key.to_string());
        if self.error.as_ref().is_some_and(|(k, _)| k == key) {
            self.error = None;
        }
    }

    /// The open screen was left: nothing more is asked for it.
    pub fn close(&mut self) {
        self.wanted = None;
    }

    /// The request to send now (the key), if one is wanted and none is on its way.
    pub fn due(&mut self) -> Option<String> {
        if self.in_flight.is_some() {
            return None;
        }
        let key = self.wanted.take()?;
        self.in_flight = Some(key.clone());
        Some(key)
    }

    /// A request waits for its turn.
    pub fn waiting(&self) -> bool {
        self.wanted.is_some() && self.in_flight.is_none()
    }

    /// The ETag of what is held for `key`, sent with its request.
    pub fn etag(&self, key: &str) -> Option<String> {
        self.held.get(key).and_then(|(_, etag)| etag.clone())
    }

    /// The answer for `key` (`still_open`: its screen is still the open one). An answer for
    /// anything but the request on its way is dropped.
    pub fn answer(&mut self, key: &str, answer: KeyedAnswer<T>, still_open: bool) {
        if self.in_flight.as_deref() != Some(key) {
            return;
        }
        self.in_flight = None;
        match answer {
            KeyedAnswer::Data(data, etag) => {
                if self.held.len() >= DETAILS_KEPT && !self.held.contains_key(key) {
                    self.held.clear();
                }
                self.held.insert(key.to_string(), (data, etag));
                self.clear_error(key);
            }
            KeyedAnswer::NotModified if self.held.contains_key(key) => self.clear_error(key),
            // Nothing held to keep (cannot happen: the ETag goes only with data held): asked
            // again, whole.
            KeyedAnswer::NotModified => self.again(key, still_open),
            KeyedAnswer::Failed(error, limit) => {
                self.error = Some((key.to_string(), error));
                // A 429: asked again once the engine's wait is over, if the screen is still open.
                if limit.is_some() {
                    self.again(key, still_open);
                }
            }
        }
    }

    /// Something is held for `key` (a screen gone back to draws it without asking again).
    pub fn has(&self, key: &str) -> bool {
        self.held.contains_key(key)
    }

    /// The request for `key` will never be answered (an endpoint left behind): asked again
    /// while its screen is open.
    pub fn cancel(&mut self, key: &str, still_open: bool) {
        if self.in_flight.as_deref() == Some(key) {
            self.in_flight = None;
            self.again(key, still_open);
        }
    }

    fn again(&mut self, key: &str, still_open: bool) {
        if still_open && self.wanted.is_none() {
            self.wanted = Some(key.to_string());
        }
    }

    fn clear_error(&mut self, key: &str) {
        if self.error.as_ref().is_some_and(|(k, _)| k == key) {
            self.error = None;
        }
    }

    /// What is held for `key`.
    pub fn get(&self, key: &str) -> Option<&T> {
        self.held.get(key).map(|(data, _)| data)
    }

    /// Why the last request for `key` failed (none since an answer for it).
    pub fn error(&self, key: &str) -> Option<&str> {
        self.error.as_ref().filter(|(k, _)| k == key).map(|(_, e)| e.as_str())
    }

    /// Another endpoint: nothing known.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ----- source pins ----------------------------------------------------------------------

/// Most source pins kept (an item pinned and an item that converts into it).
pub const SOURCE_PINS_MAX: usize = 400;

/// The source pins from watcher.json (ids that cannot be sent whole are left out).
pub fn source_pins_from(saved: &[[String; 2]]) -> HashSet<(String, String)> {
    saved.iter().filter(|[item, source]| model::id_usable(item) && model::id_usable(source)).take(SOURCE_PINS_MAX).map(|[item, source]| (item.clone(), source.clone())).collect()
}

/// What watcher.json keeps of the source pins: sorted (the file does not change while they
/// do not), at most [`SOURCE_PINS_MAX`].
pub fn source_pins_config(pins: &HashSet<(String, String)>) -> Vec<[String; 2]> {
    let mut out: Vec<[String; 2]> = pins.iter().map(|(item, source)| [item.clone(), source.clone()]).collect();
    out.sort();
    out.truncate(SOURCE_PINS_MAX);
    out
}

/// Pin or unpin `source` under `item`; `false` when nothing changed (full).
pub fn toggle_source_pin(pins: &mut HashSet<(String, String)>, item: &str, source: &str) -> bool {
    let key = (item.to_string(), source.to_string());
    if pins.remove(&key) {
        return true;
    }
    if pins.len() >= SOURCE_PINS_MAX {
        return false;
    }
    pins.insert(key)
}

/// Drop the source pins of items no longer pinned (`tracked`: the items pinned in the
/// server's answer); `true` when any went.
pub fn prune_source_pins<'a>(pins: &mut HashSet<(String, String)>, tracked: impl IntoIterator<Item = &'a str>) -> bool {
    let tracked: HashSet<&str> = tracked.into_iter().collect();
    let before = pins.len();
    pins.retain(|(item, _)| tracked.contains(item.as_str()));
    pins.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: [f32; 2] = [1920.0, 1080.0];

    fn px(p: CardPlace, screen: [f32; 2]) -> [f32; 2] {
        [(p.frac[0] * screen[0]).round(), (p.frac[1] * screen[1]).round()]
    }

    fn ow(x: f32, y: f32, pinned: bool) -> OverlayWidget {
        OverlayWidget { x, y, pinned, w: None, h: None }
    }

    fn at(frac: [f32; 2], pinned: bool) -> CardPlace {
        CardPlace { frac, pinned, size: None }
    }

    #[test]
    fn widgets_have_stable_names() {
        assert_eq!(Widget::ALL.map(Widget::key), ["fixados", "falta", "aqui", "avisos", "missoes", "blueprints"]);
        assert_eq!(Widget::ALL.map(Widget::title), ["Fixados", "Falta", "Aqui", "Registros", "Missões", "Blueprints"]);
        assert_eq!(Widget::Registros.key(), "avisos", "the records keep the saved name of the old card: no layout is lost");
        for (i, w) in Widget::ALL.iter().enumerate() {
            assert_eq!(w.index(), i);
        }
    }

    #[test]
    fn the_default_layout_pins_fixados_and_falta_top_right() {
        let l = resolve(&BTreeMap::new(), None, FHD);
        assert_eq!(px(l[Widget::Fixados.index()], FHD), [1576.0, 150.0], "under the bar and the search strip");
        assert_eq!(px(l[Widget::Falta.index()], FHD), [1232.0, 150.0], "left of Fixados, not under it");
        assert_eq!(px(l[Widget::Registros.index()], FHD), [24.0, 150.0]);
        assert_eq!(px(l[Widget::Aqui.index()], FHD), [368.0, 150.0]);
        assert_eq!(px(l[Widget::Missoes.index()], FHD), [888.0, 150.0], "left of Falta");
        assert_eq!(px(l[Widget::Blueprints.index()], FHD), [888.0, 540.0], "under Missões, from the middle down");
        assert_eq!(l.map(|p| p.pinned), [true, true, false, false, false, false], "Missões and Blueprints are off by default");
        assert!(l.iter().all(|p| p.size.is_none()), "every card at its normal size");
        // A small screen still keeps every card on it.
        let tiny = [500.0, 300.0];
        for p in resolve(&BTreeMap::new(), None, tiny) {
            let [x, y] = px(p, tiny);
            assert!((0.0..=500.0 - CARD_WIDTH).contains(&x) && (0.0..=300.0 - CARD_HEADER).contains(&y), "{p:?}");
        }
    }

    #[test]
    fn the_blueprints_card_has_a_place_of_its_own_and_loads_from_an_older_file() {
        // Its default place covers none of the others', even with every card 360 px tall.
        let overlaps = |a: CardPlace, b: CardPlace, screen: [f32; 2], h: f32| {
            let ([ax, ay], [bx, by]) = ([a.frac[0] * screen[0], a.frac[1] * screen[1]], [b.frac[0] * screen[0], b.frac[1] * screen[1]]);
            ax < bx + CARD_WIDTH && bx < ax + CARD_WIDTH && ay < by + h && by < ay + h
        };
        for (screen, h) in [(FHD, 360.0), ([1280.0, 720.0], 200.0), ([2560.0, 1440.0], 360.0)] {
            let l = resolve(&BTreeMap::new(), None, screen);
            let bp = l[Widget::Blueprints.index()];
            for w in Widget::ALL.into_iter().filter(|w| *w != Widget::Blueprints) {
                assert!(!overlaps(bp, l[w.index()], screen, h), "{w:?} on {screen:?}");
            }
        }
        // A watcher.json written before the card: it sits at its default place, and is saved
        // from then on.
        let old: crate::agent::AgentConfig = serde_json::from_str(r#"{"server":"https://x","token":"t","handle":"h","overlay_widgets":{"fixados":{"x":0.1,"y":0.1,"pinned":true}},"overlay_hidden":["missoes"]}"#).unwrap();
        let l = resolve(&old.overlay_widgets, None, FHD);
        assert_eq!(l[Widget::Blueprints.index()], default_place(Widget::Blueprints, FHD));
        assert_eq!(l[Widget::Fixados.index()], at([0.1, 0.1], true), "the saved cards as they were");
        assert!(to_config(&l).contains_key("blueprints"));
        let shown = Shown::from_saved(&old.overlay_hidden);
        assert!(shown.get(Part::Card(Widget::Blueprints)) && !shown.get(Part::Card(Widget::Missoes)), "shown unless switched off");
        // Switched off like any card, and back from watcher.json.
        let mut s = Shown::default();
        assert!(s.set(Part::Card(Widget::Blueprints), false));
        let hidden: Vec<String> = s.to_config().into_iter().filter(|(_, on)| !*on).map(|(k, _)| k).collect();
        assert_eq!(hidden, vec!["blueprints".to_string()]);
        assert_eq!(Shown::from_saved(&hidden), s);
        // Its place and size round-trip like the others'.
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        l[Widget::Blueprints.index()] = CardPlace { frac: [0.4, 0.3], pinned: true, size: Some([0.25, 0.5]) };
        assert_eq!(resolve(&to_config(&l), None, FHD), l);
    }

    #[test]
    fn positions_are_clamped_into_the_screen() {
        let card = [CARD_WIDTH, CARD_HEADER];
        assert_eq!(clamp_frac([0.5, 0.5], card, FHD), [0.5, 0.5]);
        assert_eq!(clamp_frac([-0.2, 2.0], card, FHD), [0.0, (1080.0 - CARD_HEADER) / 1080.0]);
        assert_eq!(clamp_frac([0.99, 0.0], card, FHD), [(1920.0 - CARD_WIDTH) / 1920.0, 0.0]);
        assert_eq!(clamp_frac([f32::NAN, f32::INFINITY], card, FHD), [0.0, 0.0]);
        assert_eq!(clamp_frac([0.5, 0.5], [3000.0, 2000.0], FHD), [0.0, 0.0], "a card bigger than the screen: the corner");
        assert_eq!(clamp_frac([0.5, 0.5], card, [0.0, f32::NAN]), [0.0, 0.0], "no screen size yet");
        assert_eq!(clamp_frac([0.5, 0.5], [f32::NAN, 10.0], FHD), [0.0, 0.5]);
    }

    #[test]
    fn the_whole_layout_is_clamped_with_each_cards_size() {
        let header = [CARD_WIDTH, CARD_HEADER];
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        let before = l;
        assert!(!clamp_layout(&mut l, &[header; COUNT], FHD), "already on screen: nothing moves");
        assert_eq!(l, before);

        // A short card placed near the bottom grows (a server answer): pushed back up, alone.
        let low = Widget::Aqui.index();
        l[low].frac = [0.3, (1080.0 - 40.0) / 1080.0];
        assert!(!clamp_layout(&mut l, &[header; COUNT], FHD), "a header-sized card 40 px above the bottom fits");
        let mut sizes = [header; COUNT];
        sizes[low] = [CARD_WIDTH, 300.0];
        assert!(clamp_layout(&mut l, &sizes, FHD));
        assert_eq!(l[low].frac, [0.3, (1080.0 - 300.0) / 1080.0]);
        assert_eq!(l[low].pinned, before[low].pinned, "only the place changes");
        for i in (0..COUNT).filter(|&i| i != low) {
            assert_eq!(l[i], before[i], "the other cards stay");
        }
        assert!(!clamp_layout(&mut l, &sizes, FHD), "clamping twice changes nothing more");

        // A smaller screen: the fractions stay where they fit, the rest come back on.
        let small = [1280.0, 720.0];
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        assert!(clamp_layout(&mut l, &[header; COUNT], small), "the right-hand cards no longer fit");
        for p in l {
            let [x, y] = [p.frac[0] * small[0], p.frac[1] * small[1]];
            assert!(x >= 0.0 && x + CARD_WIDTH <= small[0] + 0.01 && y >= 0.0 && y + CARD_HEADER <= small[1] + 0.01, "{p:?}");
        }
        assert_eq!(l[Widget::Registros.index()], resolve(&BTreeMap::new(), None, FHD)[Widget::Registros.index()], "a card that fits keeps its fraction");

        // Odd input never leaves an odd place.
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        l[0].frac = [f32::NAN, 0.5];
        assert!(clamp_layout(&mut l, &[header; COUNT], FHD));
        assert_eq!(l[0].frac, [0.0, 0.5]);

        // A dragged size within its limits stays; a hand-edited one past them is brought
        // back, and the card moves in with its new size.
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        let i = Widget::Registros.index();
        l[i].frac = [0.5, 0.1];
        l[i].size = Some([600.0 / 1920.0, 900.0 / 1080.0]);
        let mut sizes = [header; COUNT];
        sizes[i] = [600.0, 900.0];
        assert!(!clamp_layout(&mut l, &sizes, FHD), "fits as it is");
        l[i].size = Some([0.5, 1.4]);
        assert!(clamp_layout(&mut l, &sizes, FHD));
        let s = size_px(l[i].size.unwrap(), FHD).unwrap();
        assert_eq!(s.map(f32::round), [720.0, 1000.0], "within the limits");
        assert_eq!(l[i].frac[0], 0.5);
        assert!(l[i].frac[1] * 1080.0 + 1000.0 <= 1080.0 + 0.01, "pushed up to stay on screen: {:?}", l[i]);
        // A size that is not a number is dropped: the card is back to its normal size.
        l[i].size = Some([f32::NAN, 0.5]);
        assert!(clamp_layout(&mut l, &[header; COUNT], FHD));
        assert_eq!(l[i].size, None);
    }

    #[test]
    fn card_sizes_stay_within_limits_and_the_screen() {
        assert_eq!(clamp_size([400.0, 300.0], FHD), Some([400.0, 300.0]));
        assert_eq!(clamp_size([10.0, 10.0], FHD), Some(MIN_CARD), "the smallest card");
        assert_eq!(clamp_size([5000.0, 5000.0], FHD), Some(MAX_CARD), "the largest card");
        assert_eq!(clamp_size([5000.0, 5000.0], [600.0, 500.0]), Some([600.0, 500.0]), "never past the screen");
        assert_eq!(clamp_size([100.0, 50.0], [150.0, 60.0]), Some([150.0, 60.0]), "a screen smaller than the minimum: the screen");
        assert_eq!(clamp_size([f32::NAN, 300.0], FHD), None);
        assert_eq!(clamp_size([400.0, f32::INFINITY], FHD), None);
        assert_eq!(clamp_size([400.0, 300.0], [0.0, 1080.0]), None, "no screen size yet");
        let f = size_frac([480.0, 540.0], FHD).unwrap();
        assert_eq!(f, [0.25, 0.5]);
        assert_eq!(size_px(f, FHD), Some([480.0, 540.0]));
        assert_eq!(size_px(f, [3840.0, 2160.0]), Some([MAX_CARD[0], 1000.0]), "fractions follow the screen, within the limits");
    }

    #[test]
    fn dragging_the_corner_resizes_within_the_screen() {
        let plain = at([0.25, 0.5], false);
        let px_of = |p: Option<[f32; 2]>| size_px(p.unwrap(), FHD).map(|s| s.map(f32::round)).unwrap();
        // From the measured size the first time, then from the saved one.
        let first = resized(&plain, [320.0, 200.0], [40.0, 100.0], FHD);
        assert_eq!(px_of(first), [360.0, 300.0]);
        let sized = CardPlace { size: first, ..plain };
        assert_eq!(px_of(resized(&sized, [1.0, 1.0], [-20.0, 0.0], FHD)), [340.0, 300.0], "the measured size no longer counts");
        // Never smaller than the minimum, never past the limits or the screen's edge.
        assert_eq!(px_of(resized(&sized, [0.0, 0.0], [-1000.0, -1000.0], FHD)), MIN_CARD);
        assert_eq!(px_of(resized(&sized, [0.0, 0.0], [5000.0, 0.0], FHD)), [MAX_CARD[0], 300.0]);
        assert_eq!(px_of(resized(&sized, [0.0, 0.0], [0.0, 5000.0], FHD)), [360.0, 540.0], "down to the bottom edge from half the screen");
        assert_eq!(resized(&plain, [f32::NAN, 200.0], [1.0, 1.0], FHD), None);
    }

    #[test]
    fn saved_sizes_are_read_with_care() {
        let sized = |w: Option<f32>, h: Option<f32>| -> BTreeMap<String, OverlayWidget> { [("falta".to_string(), OverlayWidget { w, h, ..ow(0.1, 0.1, true) })].into() };
        let i = Widget::Falta.index();
        assert_eq!(resolve(&sized(Some(0.25), Some(0.5)), None, FHD)[i].size, Some([0.25, 0.5]));
        assert_eq!(resolve(&sized(Some(0.25), None), None, FHD)[i].size, None, "half a size is none");
        assert_eq!(resolve(&sized(Some(f32::NAN), Some(0.5)), None, FHD)[i].size, None);
        assert_eq!(resolve(&sized(Some(f32::INFINITY), Some(0.5)), None, FHD)[i].size, None);
        let huge = resolve(&sized(Some(9.0), Some(-3.0)), None, FHD)[i];
        assert_eq!(size_px(huge.size.unwrap(), FHD).map(|s| s.map(f32::round)), Some([MAX_CARD[0], MIN_CARD[1]]), "clamped on load");
        // A sized card near the right edge is placed with its own width.
        let wide = resolve(&[("falta".to_string(), OverlayWidget { w: Some(0.25), h: Some(0.5), ..ow(0.9, 0.9, true) })].into(), None, FHD)[i];
        assert_eq!(px(wide, FHD), [1440.0, 540.0]);
    }

    #[test]
    fn text_size_comes_in_steps() {
        assert_eq!(text_scale_from(None), 1.0);
        assert_eq!(text_scale_from(Some(1.15)), 1.15);
        assert_eq!(text_scale_from(Some(1.2)), 1.15, "the nearest step");
        assert_eq!(text_scale_from(Some(9.0)), 1.5);
        assert_eq!(text_scale_from(Some(0.1)), 0.85);
        assert_eq!(text_scale_from(Some(f32::NAN)), 1.0);
        assert_eq!(text_scale_step(1.0, true), 1.15);
        assert_eq!(text_scale_step(1.0, false), 0.85);
        assert_eq!(text_scale_step(0.85, false), 0.85, "stops at the smallest");
        assert_eq!(text_scale_step(1.5, true), 1.5, "stops at the largest");
        assert_eq!(text_scale_step(f32::NAN, true), 1.15);
        let mut s = 0.85;
        let mut seen = vec![s];
        while text_scale_step(s, true) != s {
            s = text_scale_step(s, true);
            seen.push(s);
        }
        assert_eq!(seen, TEXT_SCALES.to_vec(), "every step, in order");
        assert_eq!(TEXT_SCALES.map(text_scale_label), ["85%", "100%", "115%", "130%", "150%"]);
    }

    #[test]
    fn saved_cards_win_and_the_old_position_places_fixados() {
        let saved: BTreeMap<String, OverlayWidget> = [("falta".to_string(), ow(0.25, 0.5, false)), ("aqui".to_string(), ow(1.7, -3.0, true))].into();
        let l = resolve(&saved, Some([960.0, 540.0]), FHD);
        assert_eq!(l[Widget::Falta.index()], at([0.25, 0.5], false));
        assert_eq!(l[Widget::Aqui.index()], at([(1920.0 - CARD_WIDTH) / 1920.0, 0.0], true), "off the monitor: clamped back");
        assert_eq!(l[Widget::Fixados.index()], at([0.5, 0.5], true), "the old window's corner");
        assert_eq!(l[Widget::Registros.index()], default_place(Widget::Registros, FHD));
        // A layout saved before the split loads the records card where the old one was.
        let old: BTreeMap<String, OverlayWidget> = [("avisos".to_string(), ow(0.6, 0.3, true))].into();
        assert_eq!(resolve(&old, None, FHD)[Widget::Registros.index()], at([0.6, 0.3], true));
        // Once Fixados is saved, the old position is not read again.
        let saved: BTreeMap<String, OverlayWidget> = [("fixados".to_string(), ow(0.1, 0.1, false))].into();
        assert_eq!(resolve(&saved, Some([960.0, 540.0]), FHD)[0], at([0.1, 0.1], false));
        // An old position on a monitor that is gone lands on this one.
        let far = resolve(&BTreeMap::new(), Some([5000.0, 80.0]), FHD)[0];
        assert_eq!(px(far, FHD), [1920.0 - CARD_WIDTH, 80.0]);
    }

    #[test]
    fn a_layout_round_trips_through_the_config() {
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        l[Widget::Aqui.index()] = at([0.3, 0.6], true);
        l[Widget::Missoes.index()] = CardPlace { frac: [0.1, 0.2], pinned: true, size: Some([0.25, 0.5]) };
        let saved = to_config(&l);
        assert_eq!(saved.keys().map(String::as_str).collect::<Vec<_>>(), vec!["aqui", "avisos", "blueprints", "falta", "fixados", "missoes"]);
        assert_eq!(saved["aqui"], ow(0.3, 0.6, true));
        assert_eq!(saved["missoes"], OverlayWidget { x: 0.1, y: 0.2, pinned: true, w: Some(0.25), h: Some(0.5) });
        assert_eq!(resolve(&saved, Some([0.0, 0.0]), FHD), l, "the old position is ignored once cards are saved");
        // Through the JSON of watcher.json as well.
        let cfg = crate::agent::AgentConfig { token: "t".into(), overlay_widgets: saved, ..Default::default() };
        let back: crate::agent::AgentConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(resolve(&back.overlay_widgets, None, FHD), l);
    }

    #[test]
    fn the_old_busca_card_is_ignored_and_never_written_again() {
        let mut saved: BTreeMap<String, OverlayWidget> = [("busca".to_string(), ow(0.4, 0.4, true)), ("falta".to_string(), ow(0.25, 0.5, false))].into();
        let l = resolve(&saved, None, FHD);
        assert_eq!(l[Widget::Falta.index()], at([0.25, 0.5], false), "the other cards load as saved");
        saved.remove("busca");
        assert_eq!(resolve(&saved, None, FHD), l, "the old card changes nothing");
        assert!(!to_config(&l).contains_key("busca"));
    }

    #[test]
    fn opacity_is_kept_in_range() {
        assert_eq!(opacity_from(None), DEFAULT_OPACITY);
        assert_eq!(opacity_from(Some(0.5)), 0.5);
        assert_eq!(opacity_from(Some(0.0)), MIN_OPACITY);
        assert_eq!(opacity_from(Some(7.0)), 1.0);
        assert_eq!(opacity_from(Some(f32::NAN)), DEFAULT_OPACITY);
    }

    fn notice(id: &str, at: &str, delta: f64) -> model::Notice {
        model::Notice { id: id.into(), at: at.into(), status: "applied".into(), changes: vec![model::Change { name: "Wikelo Favor".into(), delta }] }
    }

    fn answer(notices: Vec<model::Notice>, at: Option<&str>) -> OverlayData {
        OverlayData {
            channel: "LIVE".into(),
            at_location: at.map(str::to_string),
            location_names: [("Stanton2_Orison".to_string(), "Orison".to_string())].into(),
            notices,
            ..Default::default()
        }
    }

    #[test]
    fn toasts_only_for_what_is_new_after_the_first_answer() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed(&answer(vec![notice("1", "2026-11-15T22:00:00Z", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert!(q.live().is_empty(), "nothing on the first answer");

        // The same answer again: nothing new.
        q.feed(&answer(vec![notice("1", "2026-11-15T22:00:00Z", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert!(q.live().is_empty());

        // A new notice (newest first, as the server sends them) and the old one.
        q.feed(&answer(vec![notice("2", "2026-11-15T22:05:00Z", 2.0), notice("1", "2026-11-15T22:00:00Z", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().iter().map(|t| (t.title.as_str(), t.body.as_str())).collect::<Vec<_>>(), vec![("Registrado", "+2 Wikelo Favor")]);

        // Pending -> applied is the same notice; a pending one is a warning.
        let mut pending = notice("3", "2026-11-15T22:06:00Z", -1.0);
        pending.status = "pending".into();
        q.feed(&answer(vec![pending.clone()], Some("Stanton2_Orison")), true, t0);
        q.feed(&answer(vec![model::Notice { status: "applied".into(), ..pending }], Some("Stanton2_Orison")), true, t0);
        let last = q.live().last().unwrap();
        assert_eq!((q.live().len(), last.title.as_str(), last.body.as_str(), last.warn), (2, "Pendente no site", "-1 Wikelo Favor", true));
    }

    #[test]
    fn a_new_location_raises_one_toast() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed(&answer(vec![], Some("RR_CRU_LEO")), true, t0);
        q.feed(&answer(vec![], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().iter().map(|t| (t.title.as_str(), t.body.as_str())).collect::<Vec<_>>(), vec![("Local", "Orison")]);
        q.feed(&answer(vec![], Some("Stanton2_Orison")), true, t0);
        q.feed(&answer(vec![], None), true, t0);
        q.feed(&answer(vec![], Some("")), true, t0);
        q.feed(&answer(vec![], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().len(), 1, "unknown in between and back: no change");
        // With something pinned stored there, the toast says so.
        let mut d = answer(vec![], Some("RR_CRU_LEO"));
        d.tracked = vec![model::TrackedItem { item_id: "favor".into(), name: "Wikelo Favor".into(), have: 3.0, places: vec![model::Place { location: "RR_CRU_LEO".into(), qty: 3.0 }], ..Default::default() }];
        q.feed(&d, true, t0);
        assert_eq!(q.live().last().unwrap().body, "RR CRU LEO · 1 item útil aqui");
    }

    #[test]
    fn another_channel_or_a_sign_out_starts_over_quietly() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed(&answer(vec![notice("1", "a", 1.0)], Some("RR_CRU_LEO")), true, t0);
        let mut ptu = answer(vec![notice("9", "b", 1.0)], Some("Stanton2_Orison"));
        ptu.channel = "PTU".into();
        q.feed(&ptu, true, t0);
        assert!(q.live().is_empty(), "first answer of another channel");
        q.unprime();
        q.feed(&answer(vec![notice("5", "c", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert!(q.live().is_empty(), "first answer after a sign-out");
        q.feed(&answer(vec![notice("6", "d", 1.0), notice("5", "c", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().len(), 1);
    }

    #[test]
    fn at_most_three_toasts_for_six_seconds() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed(&answer(vec![], None), true, t0);
        let many: Vec<model::Notice> = (1..=5).map(|i| notice(&i.to_string(), &format!("2026-11-15T22:0{i}:00Z"), i as f64)).rev().collect();
        q.feed(&answer(many, None), true, t0);
        assert_eq!(q.live().iter().map(|t| t.body.as_str()).collect::<Vec<_>>(), vec!["+3 Wikelo Favor", "+4 Wikelo Favor", "+5 Wikelo Favor"], "the newest three, oldest first");
        let ids: HashSet<u64> = q.live().iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 3, "distinct ids");

        q.prune(t0 + Duration::from_millis(5999));
        assert_eq!(q.live().len(), 3);
        q.prune(t0 + TOAST_FOR);
        assert!(q.live().is_empty());
        assert_eq!(q.next_repaint(t0 + TOAST_FOR), None);
    }

    #[test]
    fn alerts_switched_off_pop_nothing_and_are_never_replayed() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        // The notices there at start: never shown.
        q.feed(&answer(vec![notice("1", "a", 1.0)], Some("RR_CRU_LEO")), true, t0);
        assert!(q.live().is_empty());
        // Off: a new notice and a new place pop nothing, but count as seen.
        q.feed(&answer(vec![notice("2", "b", 2.0), notice("1", "a", 1.0)], Some("Stanton2_Orison")), false, t0);
        assert!(q.live().is_empty(), "the alerts are off");
        // On again: what came meanwhile is not replayed; the next new one pops once.
        q.feed(&answer(vec![notice("2", "b", 2.0), notice("1", "a", 1.0)], Some("Stanton2_Orison")), true, t0);
        assert!(q.live().is_empty(), "nothing replayed, the place included");
        q.feed(&answer(vec![notice("3", "c", 3.0), notice("2", "b", 2.0)], Some("Stanton2_Orison")), true, t0);
        q.feed(&answer(vec![notice("3", "c", 3.0), notice("2", "b", 2.0)], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().iter().map(|t| t.body.as_str()).collect::<Vec<_>>(), vec!["+3 Wikelo Favor"], "once");
        // Gone by itself after its time; the cap holds with the alerts on.
        q.prune(t0 + TOAST_FOR);
        assert!(q.live().is_empty());
        let many: Vec<model::Notice> = (4..=9).map(|i| notice(&i.to_string(), &format!("2026-11-15T22:0{i}:00Z"), i as f64)).collect();
        q.feed(&answer(many, Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().len(), MAX_TOASTS);
        // The overlay's own warnings and the reward choices do not depend on the switch.
        q.warn("Overlay", "x", t0);
        q.feed_choices(&[choice(77, None)], true, t0);
        assert_eq!(q.live().last().map(|t| t.title.as_str()), Some(choice_toast()));
    }

    #[test]
    fn the_overlay_can_raise_a_warning_of_its_own() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.warn("Overlay", "não foi possível abrir sobre o jogo", t0);
        let t = q.live().last().unwrap();
        assert_eq!((q.live().len(), t.title.as_str(), t.body.as_str(), t.warn), (1, "Overlay", "não foi possível abrir sobre o jogo", true), "even before the first answer");
    }

    fn choice(id: i64, title: Option<&str>) -> model::Choice {
        model::Choice { id: Some(id), title: title.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn one_toast_per_new_choice_never_repeated() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed_choices(&[choice(41, Some("Bring the goods")), choice(42, None)], true, t0);
        assert_eq!(
            q.live().iter().map(|t| (t.title.as_str(), t.body.as_str(), t.warn)).collect::<Vec<_>>(),
            vec![(choice_toast(), "Bring the goods · Shift+Tab", true), (choice_toast(), "Shift+Tab", true)],
            "the choices waiting at start are announced"
        );
        // The same choices in every following answer: nothing more.
        for _ in 0..3 {
            q.feed_choices(&[choice(41, Some("Bring the goods")), choice(42, None)], true, t0);
        }
        assert_eq!(q.live().len(), 2);
        // Seen with the overlay open: never announced afterwards.
        q.feed_choices(&[choice(43, None)], false, t0);
        q.feed_choices(&[choice(43, None)], true, t0);
        assert_eq!(q.live().len(), 2);
        // A 401 in between (the same server, account and channel) does not bring them back.
        q.unprime();
        q.feed_choices(&[choice(41, None), choice(43, None)], true, t0);
        assert_eq!(q.live().len(), 2);
        // A new one is.
        q.feed_choices(&[choice(41, None), choice(44, Some(" "))], true, t0);
        assert_eq!(q.live().last().map(|t| t.body.as_str()), Some("Shift+Tab"), "a blank title is no title");
        // Without an id there is nothing to announce.
        q.feed_choices(&[model::Choice::default()], true, t0);
        assert_eq!(q.live().len(), 3);
    }

    #[test]
    fn another_endpoint_forgets_what_was_seen() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        q.feed(&answer(vec![notice("41", "a", 1.0)], Some("RR_CRU_LEO")), true, t0);
        q.feed_choices(&[choice(41, None)], true, t0);
        assert_eq!(q.live().len(), 1);
        // Server B has its own choice 41 and notice 41.
        q.reset_identity();
        q.feed_choices(&[choice(41, Some("Outra"))], true, t0);
        assert_eq!(q.live().last().map(|t| t.body.as_str()), Some("Outra · Shift+Tab"), "the same number elsewhere is another choice");
        q.feed(&answer(vec![notice("41", "b", 2.0)], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().len(), 2, "its first answer is quiet: no notice, no location");
        q.feed(&answer(vec![notice("42", "c", 3.0), notice("41", "b", 2.0)], Some("Stanton2_Orison")), true, t0);
        assert_eq!(q.live().last().map(|t| t.body.as_str()), Some("+3 Wikelo Favor"));
        assert_eq!(q.live().len(), 3);
    }

    #[test]
    fn cards_come_to_the_front_when_touched() {
        let mut z = default_z();
        assert_eq!(z, [0, 1, 2, 3, 4, 5]);
        raise(&mut z, 1);
        assert_eq!(z, [0, 2, 3, 4, 5, 1]);
        raise(&mut z, 1);
        assert_eq!(z, [0, 2, 3, 4, 5, 1], "already in front");
        raise(&mut z, 0);
        assert_eq!(z, [2, 3, 4, 5, 1, 0]);
        raise(&mut z, 9);
        assert_eq!(z, [2, 3, 4, 5, 1, 0], "no such card");
    }

    #[test]
    fn a_search_left_behind_is_searched_again_there() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let found = model::SearchResults { items: vec![model::SearchItem { item_id: "favor".into(), name: "Wikelo Favor".into() }], ..Default::default() };
        let mut s = Search { text: "favor".into(), ..Default::default() };
        s.edited(t0);
        assert_eq!(s.due(t0 + ms(300)), Some((1, "favor".to_string())));
        assert!(s.answer(1, Ok(found.clone())));
        assert!(matches!(s.shown(), Some(Ok(r)) if r.items[0].item_id == "favor"));
        assert_eq!(s.due(t0 + ms(400)), None, "answered: a write never makes it stale");
        // Its endpoint left behind: searched again.
        s.edited(t0 + ms(400));
        s.text = "favo".into();
        assert_eq!(s.due(t0 + ms(700)), Some((2, "favo".to_string())));
        s.cancel(2, t0 + ms(800));
        assert_eq!(s.due(t0 + ms(800)), Some((3, "favo".to_string())));
        assert!(!s.answer(2, Ok(found.clone())), "the cancelled one is unknown");
        // Another endpoint: nothing of before is shown, and the text is searched there.
        s.reset(t0 + ms(900));
        assert!(s.shown().is_none());
        assert!(!s.answer(3, Ok(found)), "an answer from before");
        assert_eq!(s.due(t0 + ms(900)), Some((4, "favo".to_string())));
        // Nothing typed: nothing to search again.
        let mut idle = Search::default();
        idle.reset(t0);
        idle.cancel(1, t0);
        assert_eq!(idle.due(t0 + ms(1000)), None);
    }

    #[test]
    fn search_waits_for_the_typing_to_stop() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut s = Search::default();
        assert_eq!(s.due(t0), None, "nothing typed");
        s.text = "car".into();
        s.edited(t0);
        assert_eq!(s.due(t0 + ms(299)), None, "still typing");
        assert_eq!(s.wait(t0 + ms(100)), Some(ms(200)));
        s.text = "cari".into();
        s.edited(t0 + ms(200));
        assert_eq!(s.due(t0 + ms(400)), None, "the debounce starts over with each key");
        assert_eq!(s.due(t0 + ms(500)), Some((1, "cari".to_string())));
        assert_eq!(s.due(t0 + ms(900)), None, "sent once");
        assert_eq!(s.wait(t0 + ms(900)), None);
        assert!(s.active() && s.shown().is_none(), "searching");
        assert!(s.answer(1, Ok(model::SearchResults::default())));
        assert!(matches!(s.shown(), Some(Ok(_))));
        // The same text again (typed and erased): no second request.
        s.edited(t0 + ms(1000));
        assert_eq!(s.due(t0 + ms(1400)), None);
    }

    #[test]
    fn one_search_in_flight_and_stale_answers_dropped() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut s = Search { text: " ca ".into(), ..Default::default() };
        s.edited(t0);
        assert_eq!(s.due(t0 + ms(300)), Some((1, "ca".to_string())), "trimmed");
        // Typed on while it is on its way: nothing more goes out until it answers.
        s.text = "carinite".into();
        s.edited(t0 + ms(350));
        assert_eq!(s.due(t0 + ms(700)), None, "one request in flight");
        // Its answer is for the old text: dropped, and the new text goes out.
        assert!(!s.answer(1, Ok(model::SearchResults::default())));
        assert!(s.shown().is_none());
        assert_eq!(s.due(t0 + ms(700)), Some((2, "carinite".to_string())));
        assert!(!s.answer(1, Ok(model::SearchResults::default())), "an unknown request");
        // A failure is shown, and the same text is tried again on the next edit.
        assert!(s.answer(2, Err("HTTP 429".into())));
        assert!(matches!(s.shown(), Some(Err(_))));
        s.edited(t0 + ms(800));
        assert_eq!(s.due(t0 + ms(1100)), Some((3, "carinite".to_string())));
        // Emptied while in flight: the answer is dropped.
        s.clear();
        assert!(!s.answer(3, Ok(model::SearchResults::default())));
        assert!(!s.active() && s.shown().is_none());
        // Too short or too long: never sent.
        s.text = "c".into();
        s.edited(t0);
        assert_eq!(s.due(t0 + ms(400)), None);
        s.text = "x".repeat(SEARCH_MAX_CHARS + 1);
        s.edited(t0);
        assert_eq!(s.due(t0 + ms(400)), None);
        assert_eq!(search_query(&"é".repeat(SEARCH_MAX_CHARS)), Some("é".repeat(SEARCH_MAX_CHARS)), "counted in characters");
        // A text typed with no answer and nothing on its way (the index turned out to be
        // missing): searched at once.
        s.text = "favor".into();
        s.wake(t0);
        assert_eq!(s.due(t0), Some((4, "favor".to_string())));
    }

    #[test]
    fn toasts_repaint_fast_only_while_they_move() {
        let t0 = Instant::now();
        let mut q = Toasts::default();
        assert_eq!(q.next_repaint(t0), None);
        q.feed(&answer(vec![], None), true, t0);
        q.feed(&answer(vec![notice("1", "a", 1.0)], None), true, t0);
        assert_eq!(q.next_repaint(t0), Some(ANIMATION_FRAME), "sliding in");
        assert_eq!(q.next_repaint(t0 + Duration::from_secs(1)), Some(TOAST_FOR - TOAST_FADE - Duration::from_secs(1)), "still: wait for the fade");
        assert_eq!(q.next_repaint(t0 + TOAST_FOR - Duration::from_millis(100)), Some(ANIMATION_FRAME), "fading");
        assert_eq!(q.next_repaint(t0 + TOAST_FOR), None);

        assert_eq!(toast_phase(Duration::ZERO), (0.0, 1.0));
        let (slide, alpha) = toast_phase(Duration::from_secs(2));
        assert_eq!((slide, alpha), (1.0, 1.0));
        let (_, alpha) = toast_phase(TOAST_FOR - TOAST_FADE / 2);
        assert!((alpha - 0.5).abs() < 1e-3, "{alpha}");
        assert_eq!(toast_phase(TOAST_FOR + Duration::from_secs(1)).1, 0.0);
        let mid = toast_phase(TOAST_SLIDE / 2).0;
        assert!(mid > 0.5 && mid < 1.0, "eases out: {mid}");
    }

    #[test]
    fn one_field_lists_nothing_until_typed() {
        assert_eq!(listing(""), Listing::Nothing, "an empty field: no chips, no panel");
        assert_eq!(listing("   "), Listing::Nothing);
        assert_eq!(listing("c"), Listing::TooShort);
        assert_eq!(listing(" ca "), Listing::Query("ca".into()), "trimmed");
        assert_eq!(listing(&"é".repeat(SEARCH_MAX_CHARS)), Listing::Query("é".repeat(SEARCH_MAX_CHARS)), "counted in characters");
        assert_eq!(listing(&"x".repeat(SEARCH_MAX_CHARS + 1)), Listing::TooShort);
        assert_eq!(cut_search_text("x".repeat(70)).len(), SEARCH_MAX_CHARS);
        assert_eq!(more_line(1, false), "mais 1 item; digite para filtrar");
        assert_eq!(more_line(70, false), "mais 70 itens; digite para filtrar");
        assert_eq!(more_line(1, true), "mais 1 missão; digite para filtrar");
        assert_eq!(more_line(3, true), "mais 3 missões; digite para filtrar");
    }

    #[test]
    fn chips_show_only_the_groups_with_results_and_their_counts() {
        let labels = |items: usize, missions: usize| chips(items, missions).into_iter().map(|(_, l)| l).collect::<Vec<_>>();
        assert_eq!(labels(7, 3), vec!["Tudo", "Inventário 7", "Missões 3"]);
        assert_eq!(labels(7, 0), vec!["Tudo", "Inventário 7"], "no missions: no chip for them");
        assert_eq!(labels(0, 3), vec!["Tudo", "Missões 3"]);
        assert!(chips(0, 0).is_empty(), "nothing found: no chips");
        assert_eq!(chips(1, 1).into_iter().map(|(f, _)| f).collect::<Vec<_>>(), vec![Filter::Tudo, Filter::Inventario, Filter::Missoes]);
        assert_eq!(Filter::default(), Filter::Tudo, "the default");
    }

    #[test]
    fn results_come_grouped_inventory_first_and_a_chip_shows_one_group() {
        assert_eq!(groups(Filter::Tudo, 7, 3), vec![Group::Inventario, Group::Missoes], "in this order");
        assert_eq!(groups(Filter::Inventario, 7, 3), vec![Group::Inventario]);
        assert_eq!(groups(Filter::Missoes, 7, 3), vec![Group::Missoes]);
        assert_eq!(groups(Filter::Tudo, 0, 3), vec![Group::Missoes], "a group without results has no heading");
        assert!(groups(Filter::Tudo, 0, 0).is_empty());
        assert_eq!([Group::Inventario, Group::Missoes].map(Group::title), ["Inventário", "Missões"]);
        // A chip chosen whose group has no results for the new text: back to everything.
        assert_eq!(effective_filter(Filter::Missoes, 7, 3), Filter::Missoes);
        assert_eq!(effective_filter(Filter::Missoes, 7, 0), Filter::Tudo);
        assert_eq!(effective_filter(Filter::Inventario, 0, 3), Filter::Tudo);
        assert_eq!(effective_filter(Filter::Tudo, 7, 3), Filter::Tudo);
        assert_eq!(groups(effective_filter(Filter::Missoes, 7, 0), 7, 0), vec![Group::Inventario]);
    }

    fn craft(k: &str) -> BpScreen {
        BpScreen::Blueprint(k.to_string(), BpPanel::Craft)
    }

    fn place(k: &str) -> BpScreen {
        BpScreen::Blueprint(k.to_string(), BpPanel::Where)
    }

    fn mission(c: &str) -> BpScreen {
        BpScreen::Mission { contract: c.to_string(), title: "T".into(), giver: None }
    }

    #[test]
    fn back_on_a_blueprint_screen_goes_one_screen_up() {
        // The craft screen: back to the list.
        let mut s = ScreenStack::new();
        screen_push(&mut s, craft("BP_a"));
        screen_back(&mut s);
        assert!(s.is_empty());
        // "Onde conseguir" opened from the craft screen: back to it, then to the list.
        screen_push(&mut s, craft("BP_a"));
        screen_push(&mut s, place("BP_a"));
        screen_back(&mut s);
        assert_eq!(s.last(), Some(&craft("BP_a")));
        screen_back(&mut s);
        assert!(s.is_empty());
        // Opened from the list: back to the list.
        screen_push(&mut s, place("BP_a"));
        screen_back(&mut s);
        assert!(s.is_empty());
        // The list: nowhere to go back to.
        screen_back(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn back_returns_to_the_previous_screen_however_deep() {
        let mut s = ScreenStack::new();
        for screen in [craft("BP_a"), place("BP_a"), mission("c1"), craft("BP_b"), place("BP_b"), mission("c2")] {
            screen_push(&mut s, screen);
        }
        assert_eq!((open_contract(&s), open_blueprint(&s)), (Some("c2"), None));
        assert_eq!(back_hint(&s), "voltar para onde conseguir");
        let mut seen = Vec::new();
        while let Some(top) = s.last().cloned() {
            seen.push(top);
            screen_back(&mut s);
        }
        assert_eq!(seen, vec![mission("c2"), place("BP_b"), craft("BP_b"), mission("c1"), place("BP_a"), craft("BP_a")], "each screen in turn, back to the list");
        // The screen on top again changes nothing; the same blueprint further up is a screen of its own.
        screen_push(&mut s, craft("BP_a"));
        screen_push(&mut s, craft("BP_a"));
        assert_eq!(s.len(), 1);
        assert_eq!(back_hint(&s), "voltar para a lista");
        screen_push(&mut s, place("BP_a"));
        assert_eq!((back_hint(&s), open_blueprint(&s)), ("voltar para a blueprint", Some("BP_a")));
        screen_push(&mut s, mission("c1"));
        screen_push(&mut s, craft("BP_a"));
        assert_eq!((s.len(), back_hint(&s)), (4, "voltar para a missão"));
    }

    #[test]
    fn the_stack_of_screens_is_bounded_and_drops_the_oldest() {
        let mut s = ScreenStack::new();
        for n in 0..SCREENS_MAX + 5 {
            screen_push(&mut s, craft(&format!("BP_{n}")));
        }
        assert_eq!(s.len(), SCREENS_MAX);
        assert_eq!(s.first(), Some(&craft("BP_5")), "the oldest dropped");
        assert_eq!(s.last(), Some(&craft(&format!("BP_{}", SCREENS_MAX + 4))), "the newest on top");
        for _ in 0..SCREENS_MAX {
            screen_back(&mut s);
        }
        assert!(s.is_empty(), "back to the list after as many steps as are kept");
    }

    #[test]
    fn a_mission_screen_asks_for_its_blueprints_like_a_row_asks_for_its_detail() {
        let mut f = MissionFetch::default();
        f.open("c1");
        assert_eq!(f.due(), Some("c1".to_string()));
        assert_eq!(f.due(), None, "one request at a time");
        let m = model::MissionBlueprints { title: "T".into(), ..Default::default() };
        f.answer("c1", MissionAnswer::Data(m.clone(), Some("\"m1\"".into())), true);
        assert_eq!((f.get("c1"), f.has("c1"), f.etag("c1").as_deref()), (Some(&m), true, Some("\"m1\"")));
        // Opened again: asked again with its ETag; a 304 keeps it.
        f.open("c1");
        assert_eq!(f.due(), Some("c1".to_string()));
        f.answer("c1", MissionAnswer::NotModified, true);
        assert_eq!((f.get("c1"), f.error("c1")), (Some(&m), None));
        // A failure says the server's sentence; another endpoint forgets everything.
        f.open("c2");
        f.due();
        f.answer("c2", MissionAnswer::Failed("O site ainda não tem as blueprints por missão.".into(), None), true);
        assert_eq!((f.get("c2"), f.error("c2")), (None, Some("O site ainda não tem as blueprints por missão.")));
        f.reset();
        assert!(!f.has("c1"));
    }

    #[test]
    fn the_blueprints_card_has_its_button_in_the_bar_not_in_the_menu() {
        let listed: Vec<Part> = Part::ALL.into_iter().filter(|p| p.in_menu()).collect();
        assert_eq!(listed.iter().map(|p| p.key()).collect::<Vec<_>>(), vec!["fixados", "falta", "aqui", "avisos", "missoes", "pesquisa", "alertas"]);
        // Hidden by its close button or its bar button: the same switch, saved the same way.
        let mut s = Shown::default();
        assert!(s.set(Part::Card(Widget::Blueprints), false));
        assert_eq!(Shown::from_saved(&["blueprints".to_string()]), s);
    }

    #[test]
    fn the_header_grows_with_the_text() {
        assert_eq!(card_header_height(1.0), CARD_HEADER);
        assert_eq!(card_header_height(1.5), CARD_HEADER * 1.5);
        assert_eq!(card_header_height(0.85), CARD_HEADER * 0.85);
        for odd in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(card_header_height(odd), CARD_HEADER, "{odd}");
        }
        // A card as tall as its header near the bottom is kept on screen at the larger size.
        let mut l = resolve(&BTreeMap::new(), None, FHD);
        l[0].frac = [0.5, (1080.0 - CARD_HEADER) / 1080.0];
        let header = [CARD_WIDTH, card_header_height(1.5)];
        assert!(clamp_layout(&mut l, &[header; COUNT], FHD));
        assert!(l[0].frac[1] * 1080.0 + card_header_height(1.5) <= 1080.0 + 0.01);
    }

    #[test]
    fn only_the_rows_in_view_are_laid_out() {
        // 20 px a row from y = 100; the band shows y 300 to 500.
        assert_eq!(visible_rows(100.0, 300.0, 500.0, 5000, 20.0), 10..21, "rows 10 to 19, and the one cut at the bottom");
        assert_eq!(visible_rows(100.0, 0.0, 200.0, 5000, 20.0), 0..6, "the top of the list");
        assert_eq!(visible_rows(100.0, 0.0, 10_000_000.0, 5000, 20.0), 0..5000, "never past the list");
        assert_eq!(visible_rows(100.0, 200_000.0, 200_400.0, 5000, 20.0), 5000..5000, "scrolled past it: none");
        assert_eq!(visible_rows(100.0, 300.0, 500.0, 0, 20.0), 0..0);
        for pitch in [0.0, -3.0, f32::NAN, f32::INFINITY] {
            assert_eq!(visible_rows(100.0, 300.0, 500.0, 50, pitch), 0..0, "{pitch}");
        }
        assert_eq!(visible_rows(f32::NAN, 300.0, 500.0, 50, 20.0), 0..0);
        // A screenful, however long the list.
        let r = visible_rows(0.0, 1_000_000.0, 1_000_600.0, 1_000_000, 20.0);
        assert!(r.len() <= 32, "{r:?}");
    }

    fn some_detail() -> model::BlueprintDetail {
        model::BlueprintDetail { craft_time_sec: 90.0, ..Default::default() }
    }

    #[test]
    fn a_row_opened_asks_for_its_detail_once_and_again_with_its_etag_when_opened_again() {
        let mut f = DetailFetch::default();
        assert_eq!(f.due(), None, "nothing opened");
        f.open("BP_a");
        assert!(f.waiting());
        assert_eq!(f.due(), Some("BP_a".to_string()));
        assert_eq!((f.due(), f.waiting()), (None, false), "one request at a time");
        assert_eq!(f.etag("BP_a"), None, "nothing held: no ETag");
        f.answer("BP_a", DetailAnswer::Data(some_detail(), Some("\"e1\"".into())), true);
        assert_eq!(f.get("BP_a"), Some(&some_detail()));
        assert_eq!(f.due(), None, "kept: not asked again while open");
        // Opened again: asked again, with its ETag; a 304 keeps it.
        f.open("BP_a");
        assert_eq!(f.due(), Some("BP_a".to_string()));
        assert_eq!(f.etag("BP_a").as_deref(), Some("\"e1\""));
        f.answer("BP_a", DetailAnswer::NotModified, true);
        assert_eq!((f.get("BP_a"), f.error("BP_a")), (Some(&some_detail()), None));
        // A row closed before its request went out: not asked.
        f.open("BP_b");
        f.close();
        assert_eq!(f.due(), None);
        // An answer for anything but the request on its way is dropped.
        f.answer("BP_b", DetailAnswer::Data(some_detail(), None), true);
        assert_eq!(f.get("BP_b"), None);
    }

    #[test]
    fn a_failed_detail_says_why_keeps_what_was_held_and_a_429_asks_again_while_open() {
        let mut f = DetailFetch::default();
        f.open("BP_a");
        f.due();
        f.answer("BP_a", DetailAnswer::Data(some_detail(), None), true);
        f.open("BP_a");
        f.due();
        f.answer("BP_a", DetailAnswer::Failed("canal sem dados".into(), None), true);
        assert_eq!((f.error("BP_a"), f.get("BP_a").is_some()), (Some("canal sem dados"), true), "the server's sentence; the detail held stays");
        assert_eq!(f.error("BP_b"), None, "said under its own row only");
        assert_eq!(f.due(), None, "not asked again until opened again");
        f.open("BP_a");
        assert_eq!(f.error("BP_a"), None, "opened again: asked afresh");
        // A 429: asked again (the engine holds it back for the server's wait), if still open.
        f.due();
        f.answer("BP_a", DetailAnswer::Failed("consultas demais".into(), Some(Limit(None))), true);
        assert_eq!(f.due(), Some("BP_a".to_string()));
        f.answer("BP_a", DetailAnswer::Failed("consultas demais".into(), Some(Limit(None))), false);
        assert_eq!(f.due(), None, "closed meanwhile: not asked");
        // A 304 with nothing held is asked again whole.
        f.open("BP_c");
        f.due();
        f.answer("BP_c", DetailAnswer::NotModified, true);
        assert_eq!((f.due(), f.etag("BP_c")), (Some("BP_c".to_string()), None));
        // An endpoint left behind: asked again while open; another endpoint forgets everything.
        f.cancel("BP_c", true);
        assert_eq!(f.due(), Some("BP_c".to_string()));
        f.cancel("BP_c", false);
        assert_eq!(f.due(), None);
        f.reset();
        assert_eq!(f.get("BP_a"), None);
    }

    #[test]
    fn the_details_kept_are_bounded() {
        let mut f = DetailFetch::default();
        for n in 0..DETAILS_KEPT + 5 {
            let key = format!("BP_{n}");
            f.open(&key);
            assert_eq!(f.due(), Some(key.clone()));
            f.answer(&key, DetailAnswer::Data(some_detail(), None), true);
        }
        assert!(f.held.len() <= DETAILS_KEPT);
        assert!(f.get(&format!("BP_{}", DETAILS_KEPT + 4)).is_some(), "the one arriving is kept");
    }

    fn held(ids: &[(&str, f64)]) -> HashMap<String, model::Owned> {
        ids.iter().map(|&(id, qty)| (id.to_string(), model::Owned { qty, stacks: vec![] })).collect()
    }

    #[test]
    fn the_inventory_is_read_on_first_show_every_30_s_and_after_a_write() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut inv = InventoryFetch::default();
        assert!(inv.held().is_none() && inv.error().is_none(), "carregando…");
        assert_eq!(inv.wait(t0), Some(Duration::ZERO), "never read: at once");
        assert_eq!(inv.due(t0), Some(1));
        assert_eq!((inv.due(t0), inv.wait(t0)), (None, None), "one on its way");
        assert!(!inv.answer(9, held(&[("a", 1.0)]), 0, t0), "not the request on its way");
        let rev = inv.revision();
        assert!(inv.answer(1, held(&[("a", 3.0)]), 2, t0));
        assert_eq!((inv.mark(), inv.held().map(|h| h["a"].qty)), (2, Some(3.0)));
        assert_ne!(inv.revision(), rev, "what is built from it is built again");
        // Every 30 s while shown.
        assert_eq!(inv.due(t0 + s(29)), None);
        assert_eq!(inv.wait(t0 + s(10)), Some(s(20)));
        assert_eq!(inv.due(t0 + INVENTORY_EVERY), Some(2));
        // A failure keeps what was read, says so, and backs off like the reads.
        inv.failed(2, "HTTP 502".into(), false, t0 + s(30));
        assert_eq!((inv.held().map(|h| h["a"].qty), inv.error()), (Some(3.0), Some("HTTP 502")));
        assert_eq!(inv.wait(t0 + s(30)), Some(next_delay(Outcome::Unreachable, 1)));
        assert_eq!(inv.due(t0 + s(34)), None);
        assert_eq!(inv.due(t0 + s(35)), Some(3));
        inv.failed(3, "HTTP 502".into(), false, t0 + s(35));
        assert_eq!(inv.wait(t0 + s(35)), Some(next_delay(Outcome::Unreachable, 2)), "growing");
        // A 429: the engine holds it back; here it is due at once.
        inv.refetch(t0 + s(36));
        assert_eq!(inv.due(t0 + s(36)), Some(4));
        inv.failed(4, "HTTP 429".into(), true, t0 + s(36));
        assert_eq!(inv.wait(t0 + s(36)), Some(Duration::ZERO));
        // A write settled: asked again at once, never later than already due.
        let seq = inv.due(t0 + s(37)).unwrap();
        assert!(inv.answer(seq,held(&[("a", 3.0), ("b", 1.0)]), 5, t0 + s(37)));
        assert_eq!(inv.error(), None);
        inv.refetch(t0 + s(40));
        assert_eq!(inv.wait(t0 + s(40)), Some(Duration::ZERO));
        // A settled change the answer lacks: added for good.
        let rev = inv.revision();
        inv.bake(&model::Local::Adjust { item_id: "a".into(), delta: -1 });
        assert_eq!((inv.held().map(|h| h["a"].qty), inv.revision() != rev), (Some(2.0), true));
        // An endpoint left behind: asked again; another endpoint: nothing known.
        let seq = inv.due(t0 + s(40)).unwrap();
        inv.cancel(seq, t0 + s(41));
        assert_eq!(inv.wait(t0 + s(41)), Some(Duration::ZERO));
        let rev = inv.revision();
        inv.reset();
        assert!(inv.held().is_none() && inv.error().is_none() && inv.mark() == 0 && inv.revision() != rev);
        assert_eq!(inv.due(t0 + s(41)), Some(seq + 1), "numbers keep counting");
        assert!(!inv.answer(seq, held(&[]), 0, t0 + s(42)), "an answer from before");
    }

    #[test]
    fn the_locations_are_read_once_and_retried_after_a_failure() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut l = LocationsFetch::default();
        assert_eq!(l.wait(t0), Some(Duration::ZERO));
        assert!(l.due(t0));
        assert!(!l.due(t0) && l.wait(t0).is_none(), "one on its way");
        l.failed("HTTP 502".into(), false, t0);
        assert_eq!((l.held(), l.error()), (None, Some("HTTP 502")));
        assert_eq!(l.wait(t0), Some(next_delay(Outcome::Unreachable, 1)));
        assert!(!l.due(t0 + s(4)));
        assert!(l.due(t0 + s(5)));
        // A 429: the engine holds it back; here it is due at once.
        l.failed("HTTP 429".into(), true, t0 + s(5));
        assert!(l.due(t0 + s(5)));
        // An endpoint left behind: asked again.
        l.cancel(t0 + s(6));
        assert!(l.due(t0 + s(6)));
        let list = model::parse_locations(br#"{"locations":[{"id":"a","name":"A"}]}"#).unwrap();
        l.answer(list);
        assert_eq!((l.held().map(<[model::Location]>::len), l.error()), (Some(1), None));
        assert!(!l.due(t0 + s(100)) && l.wait(t0).is_none(), "kept: never asked again");
        l.reset();
        assert!(l.held().is_none() && l.due(t0), "another endpoint: asked anew");
    }

    #[test]
    fn source_pins_follow_the_pinned_items_and_stay_bounded() {
        let mut pins = source_pins_from(&[["favor".into(), "scrip".into()], ["favor".into(), "".into()], ["x".repeat(model::MAX_ID_CHARS + 1), "scrip".into()], ["gone".into(), "scrip".into()]]);
        assert_eq!(pins.len(), 2, "unusable ids are left out");
        assert!(toggle_source_pin(&mut pins, "favor", "ore"));
        assert!(pins.contains(&("favor".to_string(), "ore".to_string())));
        assert!(toggle_source_pin(&mut pins, "favor", "ore"));
        assert!(!pins.contains(&("favor".to_string(), "ore".to_string())), "a second click unpins");
        // An item no longer pinned takes its source pins with it, silently.
        assert!(prune_source_pins(&mut pins, ["favor", "other"]));
        assert_eq!(source_pins_config(&pins), vec![["favor".to_string(), "scrip".to_string()]]);
        assert!(!prune_source_pins(&mut pins, ["favor"]), "nothing more to drop");
        // Bounded and sorted for watcher.json.
        let mut many: HashSet<(String, String)> = (0..SOURCE_PINS_MAX).map(|n| (format!("i{n:03}"), "s".to_string())).collect();
        assert!(!toggle_source_pin(&mut many, "new", "s"), "full");
        let saved = source_pins_config(&many);
        assert_eq!(saved.len(), SOURCE_PINS_MAX);
        assert!(saved.windows(2).all(|w| w[0] < w[1]));
        // Round trip through watcher.json's JSON.
        let cfg = crate::agent::AgentConfig { token: "t".into(), overlay_source_pins: source_pins_config(&pins), ..Default::default() };
        let back: crate::agent::AgentConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(source_pins_from(&back.overlay_source_pins), pins);
    }

    #[test]
    fn the_widgets_menu_lists_every_card_and_the_search() {
        assert_eq!(Part::ALL.map(Part::key), ["fixados", "falta", "aqui", "avisos", "missoes", "blueprints", "pesquisa", "alertas"]);
        assert_eq!(Part::ALL.map(Part::title), ["Fixados", "Falta", "Aqui", "Registros", "Missões", "Blueprints", "Busca", "Alertas"]);
        assert!(!Widget::ALL.iter().any(|w| [SEARCH_KEY, ALERTS_KEY, "busca"].contains(&w.key())), "the search and the alerts have names of their own");
        assert!(Shown::default().get(Part::Alerts), "the alerts are on by default");
        for (i, p) in Part::ALL.iter().enumerate() {
            assert_eq!(p.index(), i);
        }
    }

    #[test]
    fn show_all_and_hide_all() {
        let mut s = Shown::default();
        assert!(Part::ALL.iter().all(|p| s.get(*p)), "everything shown at first");
        assert!(!s.set_all(true), "already all shown");
        assert!(s.set(Part::Card(Widget::Falta), false));
        assert!(!s.set(Part::Card(Widget::Falta), false), "no change");
        assert!(!s.get(Part::Card(Widget::Falta)) && s.get(Part::Card(Widget::Fixados)) && s.any());
        assert!(s.set_all(false));
        assert!(Part::ALL.iter().all(|p| !s.get(*p)) && !s.any(), "ocultar todos: the search too");
        assert!(!s.set_all(false));
        assert!(s.set(Part::Search, true));
        assert!(s.any() && !s.get(Part::Card(Widget::Fixados)));
        assert!(s.set_all(true));
        assert_eq!(s, Shown::default());
    }

    #[test]
    fn switches_round_trip_and_an_older_file_shows_everything() {
        let mut s = Shown::default();
        s.set(Part::Card(Widget::Aqui), false);
        s.set(Part::Search, false);
        let saved = s.to_config();
        assert_eq!(saved.len(), PARTS, "every widget by name");
        assert_eq!(saved.iter().filter(|(_, on)| !**on).map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["aqui", "pesquisa"]);
        // Through watcher.json: the overlay saves the names switched off.
        let hidden: Vec<String> = saved.iter().filter(|(_, on)| !**on).map(|(k, _)| k.clone()).collect();
        let cfg = crate::agent::AgentConfig { token: "t".into(), overlay_hidden: hidden, ..Default::default() };
        let back: crate::agent::AgentConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(Shown::from_saved(&back.overlay_hidden), s);
        // An older file has no such field: everything shown, the search under the bar.
        let old: crate::agent::AgentConfig = serde_json::from_str(r#"{"server":"https://x","token":"t","handle":"h","overlay_widgets":{"fixados":{"x":0.1,"y":0.1,"pinned":true}}}"#).unwrap();
        assert_eq!(Shown::from_saved(&old.overlay_hidden), Shown::default());
        assert_eq!(search_pos_from(old.overlay_search_pos), None);
        // Names this overlay does not know change nothing.
        assert_eq!(Shown::from_saved(&["novo".to_string(), " falta ".to_string()]), {
            let mut only = Shown::default();
            only.set(Part::Card(Widget::Falta), false);
            only
        });
    }

    #[test]
    fn the_search_strip_moves_stays_on_screen_and_round_trips() {
        let row = [600.0, 52.0];
        // Dragged from its first place under the bar.
        let p = search_dragged([660.0, 90.0], [100.0, 200.0], row, FHD);
        assert_eq!([p[0] * FHD[0], p[1] * FHD[1]].map(f32::round), [760.0, 290.0]);
        // Never off the screen, whatever the drag.
        let p = search_dragged([660.0, 90.0], [5000.0, -500.0], row, FHD);
        assert_eq!([p[0] * FHD[0], p[1] * FHD[1]].map(f32::round), [1320.0, 0.0]);
        let p = search_dragged([0.0, 1000.0], [-50.0, 500.0], row, FHD);
        assert_eq!([p[0] * FHD[0], p[1] * FHD[1]].map(f32::round), [0.0, 1028.0], "its row stays on screen");
        assert_eq!(search_dragged([f32::NAN, 10.0], [0.0, 0.0], row, FHD)[0], 0.0);
        assert_eq!(search_dragged([10.0, 10.0], [0.0, 0.0], row, [0.0, 0.0]), [0.0, 0.0], "no screen yet");
        // Another monitor: clamped back; one that fits keeps its fraction; none stays under the bar.
        let far = Some([0.9, 0.99]);
        assert_eq!(clamp_search(far, row, FHD), Some([(1920.0 - 600.0) / 1920.0, (1080.0 - 52.0) / 1080.0]));
        assert_eq!(clamp_search(Some([0.25, 0.5]), row, FHD), Some([0.25, 0.5]));
        assert_eq!(clamp_search(None, row, FHD), None);
        let small = [1280.0, 720.0];
        let back = clamp_search(Some([0.6, 0.5]), row, small).unwrap();
        assert!(back[0] * small[0] + row[0] <= small[0] + 0.01, "{back:?}");
        // Through watcher.json.
        let cfg = crate::agent::AgentConfig { token: "t".into(), overlay_search_pos: Some([0.25, 0.75]), ..Default::default() };
        let back: crate::agent::AgentConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(search_pos_from(back.overlay_search_pos), Some([0.25, 0.75]));
        assert_eq!(search_pos_from(Some([f32::NAN, 0.5])), None, "not a number: under the bar");
    }

    #[test]
    fn results_unfold_toward_the_larger_room() {
        assert!(!unfold_up(80.0, 900.0), "near the top: down");
        assert!(unfold_up(900.0, 80.0), "near the bottom: up");
        assert!(!unfold_up(500.0, 500.0), "a tie: down, as before");
        assert!(!unfold_up(f32::NAN, 100.0));
    }

    #[test]
    fn esc_closes_the_menu_then_empties_the_search_then_closes() {
        assert_eq!(esc_step(true, "carinite"), EscStep::Menu);
        assert_eq!(esc_step(true, ""), EscStep::Menu);
        assert_eq!(esc_step(false, "carinite"), EscStep::Search);
        assert_eq!(esc_step(false, " "), EscStep::Search, "any text, as before");
        assert_eq!(esc_step(false, ""), EscStep::Close);
    }
}

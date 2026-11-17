//! What `GET /api/channels/{channel}/overlay` answers, and the text the overlay draws from
//! it. Everything here is plain data and string formatting, testable without a window.
//!
//! Deserialization is lenient on purpose: every field has a default, unknown fields are
//! ignored, `null` reads as the default and a number may come as an integer, a float or a
//! numeric string. An older or newer server must never take the overlay down.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer};

use crate::{tr, trf};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OverlayData {
    #[serde(deserialize_with = "lenient")]
    pub channel: String,
    #[serde(deserialize_with = "lenient")]
    pub at_location: Option<String>,
    /// The account's home: offered first by the move picker, after where the player is.
    #[serde(deserialize_with = "lenient")]
    pub home: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub location_names: HashMap<String, String>,
    #[serde(deserialize_with = "lenient")]
    pub missing: Vec<MissingItem>,
    #[serde(deserialize_with = "lenient")]
    pub tracked: Vec<TrackedItem>,
    #[serde(deserialize_with = "lenient")]
    pub notices: Vec<Notice>,
    #[serde(deserialize_with = "number")]
    pub pending: f64,
    /// The pinned missions, oldest pin first (empty from an older server).
    #[serde(deserialize_with = "lenient")]
    pub missions: Vec<PinnedMission>,
    /// Reward choices waiting for the player, oldest first (empty from an older server).
    #[serde(deserialize_with = "lenient")]
    pub choices: Vec<Choice>,
}

/// A pinned mission and what it pays.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PinnedMission {
    #[serde(deserialize_with = "lenient")]
    pub mission_id: String,
    #[serde(deserialize_with = "lenient")]
    pub title: String,
    #[serde(deserialize_with = "lenient")]
    pub giver: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub rewards: Vec<Reward>,
}

/// One reward of a mission: `qty` of the item, `have` = what the player owns of it; `one_of`:
/// one of these rewards, not all of them.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Reward {
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub qty: f64,
    #[serde(deserialize_with = "number")]
    pub have: f64,
    #[serde(deserialize_with = "lenient")]
    pub one_of: bool,
}

/// A mission that paid one of several items, waiting for the player to say which
/// (`POST /api/pending/{id}/choose`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Choice {
    /// The pending change's id; a choice without one is dropped (see [`bound`]).
    #[serde(deserialize_with = "opt_int")]
    pub id: Option<i64>,
    #[serde(deserialize_with = "lenient")]
    pub at: String,
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(deserialize_with = "number")]
    pub qty: f64,
    #[serde(deserialize_with = "lenient")]
    pub options: Vec<ChoiceOption>,
    #[serde(deserialize_with = "lenient")]
    pub suggested: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChoiceOption {
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    /// What choosing it adds (`None` from an older server: the choice's `qty`).
    #[serde(deserialize_with = "opt_number")]
    pub qty: Option<f64>,
}

/// What `GET /api/channels/{channel}/overlay/search` answers (an older server, without the
/// index). What it says of the quantities and the pins is not read: those come from the
/// inventory and from the overlay's own data, like for the index (see [`item_rows`]).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    #[serde(deserialize_with = "lenient")]
    pub items: Vec<SearchItem>,
    #[serde(deserialize_with = "lenient")]
    pub missions: Vec<SearchMission>,
}

/// An item the search found, bare: its id and name.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchItem {
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
}

/// Everything the overlay can search in a channel (`GET .../overlay/index`). Names are
/// folded (and lowercased for the order) once, when the index arrives; a keystroke only
/// folds the query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayIndex {
    /// Part of the contract (the ETag carries it); kept for the logs and the tests.
    #[allow(dead_code)]
    pub version: String,
    pub items: Vec<IndexItem>,
    pub missions: Vec<IndexMission>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexItem {
    pub item_id: String,
    pub name: String,
    folded: String,
    lower: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexMission {
    pub mission_id: String,
    pub title: String,
    pub giver: Option<String>,
    folded: String,
    lower: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawIndex {
    #[serde(deserialize_with = "lenient")]
    version: String,
    #[serde(deserialize_with = "lenient")]
    items: Vec<RawIndexItem>,
    #[serde(deserialize_with = "lenient")]
    missions: Vec<RawIndexMission>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawIndexItem {
    #[serde(deserialize_with = "lenient")]
    item_id: String,
    #[serde(deserialize_with = "lenient")]
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawIndexMission {
    #[serde(deserialize_with = "lenient")]
    mission_id: String,
    #[serde(deserialize_with = "lenient")]
    title: String,
    #[serde(deserialize_with = "lenient")]
    giver: Option<String>,
}

/// A mission the search found, bare: its id, title and giver.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchMission {
    #[serde(deserialize_with = "lenient")]
    pub mission_id: String,
    #[serde(deserialize_with = "lenient")]
    pub title: String,
    #[serde(deserialize_with = "lenient")]
    pub giver: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Place {
    #[serde(deserialize_with = "lenient")]
    pub location: String,
    #[serde(deserialize_with = "number")]
    pub qty: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MissingItem {
    #[allow(dead_code)]
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub need: f64,
    #[serde(deserialize_with = "number")]
    pub have: f64,
    #[serde(deserialize_with = "number")]
    pub missing: f64,
    #[serde(deserialize_with = "lenient")]
    pub places: Vec<Place>,
    #[serde(deserialize_with = "lenient")]
    pub contracts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TrackedItem {
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub have: f64,
    #[serde(deserialize_with = "opt_number")]
    pub target: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub places: Vec<Place>,
    #[serde(deserialize_with = "lenient")]
    pub sources: Vec<Source>,
}

/// An item that converts into a tracked one: `give` of it buys `receive` of the tracked
/// item, so `have` of it makes `makes` more.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Source {
    #[allow(dead_code)]
    #[serde(deserialize_with = "lenient")]
    pub item_id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub have: f64,
    #[serde(deserialize_with = "number")]
    pub give: f64,
    #[serde(deserialize_with = "number")]
    pub receive: f64,
    #[serde(deserialize_with = "number")]
    pub makes: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Notice {
    /// The server's id (a number or a string); empty when it sends none.
    #[serde(deserialize_with = "id_text")]
    pub id: String,
    /// RFC 3339.
    #[serde(deserialize_with = "lenient")]
    pub at: String,
    /// `applied` or `pending`.
    #[serde(deserialize_with = "lenient")]
    pub status: String,
    #[serde(deserialize_with = "lenient")]
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Change {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "number")]
    pub delta: f64,
}

impl Notice {
    pub fn is_pending(&self) -> bool {
        self.status.eq_ignore_ascii_case("pending")
    }
}

/// The value, or its default when it is `null` or does not have the expected shape (a
/// string where a list was expected, say). Never an error.
fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// A number as JSON number or numeric string; anything else is 0.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(opt_number(d)?.unwrap_or(0.0))
}

/// An id as JSON number or string; anything else is empty.
fn id_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s,
        _ => String::new(),
    })
}

/// An integer as JSON number or numeric string; anything else is `None`.
fn opt_int<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn opt_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite()))
}

/// A body that is not what the route answers.
fn unexpected() -> String {
    tr!("unexpected answer from the server", "resposta inesperada do servidor").to_string()
}

/// The same, with why it did not read.
fn unexpected_because(e: serde_json::Error) -> String {
    trf!("unexpected answer from the server ({e})", "resposta inesperada do servidor ({e})")
}

/// Parse an overlay answer, bounded (see [`bound`]). `Err` only for a body that is not a
/// JSON object at all.
pub fn parse(body: &[u8]) -> Result<OverlayData, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(unexpected_because)?;
    if !v.is_object() {
        return Err(unexpected());
    }
    let mut d: OverlayData = serde_json::from_value(v).map_err(unexpected_because)?;
    bound(&mut d);
    Ok(d)
}

/// Parse a search answer, bounded like an overlay answer (at most [`MAX_RESULTS`] of each
/// kind, rows without an id dropped, ids never cut). `Err` only for a body that is not a
/// JSON object.
pub fn parse_search(body: &[u8]) -> Result<SearchResults, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(unexpected_because)?;
    if !v.is_object() {
        return Err(unexpected());
    }
    let mut r: SearchResults = serde_json::from_value(v).map_err(unexpected_because)?;
    r.items.retain(|i| !i.item_id.trim().is_empty());
    r.items.truncate(MAX_RESULTS);
    for i in &mut r.items {
        cap(&mut i.name);
    }
    r.missions.retain(|m| !m.mission_id.trim().is_empty());
    r.missions.truncate(MAX_RESULTS);
    for m in &mut r.missions {
        cap(&mut m.title);
        if let Some(g) = &mut m.giver {
            cap(g);
        }
    }
    Ok(r)
}

/// Most items and missions kept of an index (a dataset has a few thousand).
pub const MAX_INDEX_ROWS: usize = 50_000;

/// Parse an index and fold its names once. Rows without a usable id are dropped; ids are
/// never cut (they go back in URLs, see [`id_usable`]), names are (see [`MAX_CHARS`]).
pub fn parse_index(body: &[u8]) -> Result<OverlayIndex, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(unexpected_because)?;
    if !v.is_object() {
        return Err(unexpected());
    }
    let mut raw: RawIndex = serde_json::from_value(v).map_err(unexpected_because)?;
    cap(&mut raw.version);
    let items: Vec<IndexItem> = raw
        .items
        .into_iter()
        .filter(|i| id_usable(&i.item_id))
        .take(MAX_INDEX_ROWS)
        .map(|mut i| {
            cap(&mut i.name);
            let folded = fold_search(&i.name);
            let lower = i.name.to_lowercase();
            IndexItem { item_id: i.item_id, name: i.name, folded, lower }
        })
        .collect();
    let missions = raw
        .missions
        .into_iter()
        .filter(|m| id_usable(&m.mission_id))
        .take(MAX_INDEX_ROWS)
        .map(|mut m| {
            cap(&mut m.title);
            if let Some(g) = &mut m.giver {
                cap(g);
            }
            let folded = fold_search(&m.title);
            let lower = m.title.to_lowercase();
            IndexMission { mission_id: m.mission_id, title: m.title, giver: m.giver, folded, lower }
        })
        .collect();
    Ok(OverlayIndex { version: raw.version, items, missions })
}

// ----- inventory ------------------------------------------------------------------------

/// Most items kept of an inventory answer.
pub const MAX_OWNED: usize = 20_000;

/// Most stacks (place and quality) kept of one item.
pub const MAX_STACKS: usize = 5 * MAX_PER_ROW;

/// One stack of an item the player owns: where, with which quality (0: not informed), how
/// many. A move goes stack by stack (`POST /api/inventory/{item}/move` keeps the quality).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Stack {
    #[serde(deserialize_with = "lenient")]
    pub location: String,
    #[serde(deserialize_with = "quality")]
    pub quality: i64,
    #[serde(deserialize_with = "number")]
    pub qty: f64,
}

/// A quality as a whole number at least 0; anything else is 0.
fn quality<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(opt_int(d)?.unwrap_or(0).max(0))
}

/// One item the player owns (`GET /api/inventory`): its stacks summed, and its stacks (one per
/// place and quality, in the order the server gave).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Owned {
    pub qty: f64,
    pub stacks: Vec<Stack>,
}

impl Owned {
    /// Its places (see [`stack_places`]).
    pub fn places(&self) -> Vec<Place> {
        stack_places(&self.stacks)
    }
}

/// One place per location of `stacks`, the qualities of a location summed, in the order of
/// the stacks, at most [`MAX_PER_ROW`].
pub fn stack_places(stacks: &[Stack]) -> Vec<Place> {
    let mut out: Vec<Place> = Vec::new();
    for s in stacks.iter().filter(|s| s.qty > 0.0) {
        match out.iter_mut().find(|p| p.location == s.location) {
            Some(p) => p.qty += s.qty,
            None => out.push(Place { location: s.location.clone(), qty: s.qty }),
        }
    }
    out.truncate(MAX_PER_ROW);
    out
}

/// One entry of `GET /api/inventory` (`{itemId, qty, places: [{location, qty, quality}]}`);
/// anything else in it is ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawOwned {
    #[serde(deserialize_with = "lenient")]
    item_id: String,
    #[serde(deserialize_with = "number")]
    qty: f64,
    #[serde(deserialize_with = "lenient")]
    places: Vec<Stack>,
}

/// Parse `GET /api/inventory`: every item with something of it (its stacks summed; an entry
/// without stacks counts its own `qty`), an item listed twice summed, each stack (place and
/// quality) once. Entries that do not read, ids that cannot be sent back and nothing held are
/// left out; `Err` only for a body that is not a JSON list.
pub fn parse_inventory(body: &[u8]) -> Result<HashMap<String, Owned>, String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(unexpected_because)?;
    let serde_json::Value::Array(entries) = v else { return Err(unexpected()) };
    let mut out: HashMap<String, Owned> = HashMap::new();
    for entry in entries {
        let Ok(e) = serde_json::from_value::<RawOwned>(entry) else { continue };
        if !id_usable(&e.item_id) || (out.len() >= MAX_OWNED && !out.contains_key(&e.item_id)) {
            continue;
        }
        let stacks: Vec<&Stack> = e.places.iter().filter(|p| p.qty.is_finite() && p.qty > 0.0).collect();
        let qty = if e.places.is_empty() { e.qty.max(0.0) } else { stacks.iter().map(|p| p.qty).sum() };
        let owned = out.entry(e.item_id).or_default();
        owned.qty += qty;
        for p in stacks {
            let mut location = p.location.clone();
            cap(&mut location);
            match owned.stacks.iter_mut().find(|q| q.location == location && q.quality == p.quality) {
                Some(q) => q.qty += p.qty,
                None => owned.stacks.push(Stack { location, quality: p.quality, qty: p.qty }),
            }
        }
    }
    out.retain(|_, o| o.qty > 0.0);
    for o in out.values_mut() {
        o.stacks.truncate(MAX_STACKS);
    }
    Ok(out)
}

/// The inventory answer with one change applied for good (a read settled it, and the answer
/// is older than its write): a - / + on the total only (where it went is the next answer's),
/// never below zero, an item the answer did not have yet starting from 0; a move on the
/// stacks it went between.
pub fn bake_owned(owned: &mut HashMap<String, Owned>, change: &Local) {
    match change {
        Local::Adjust { item_id, delta } => {
            let o = owned.entry(item_id.clone()).or_default();
            o.qty = add_qty(o.qty, *delta);
        }
        Local::Move { item_id, from, to, parts } => {
            if let Some(o) = owned.get_mut(item_id) {
                move_stacks(&mut o.stacks, from, to, parts);
            }
        }
        _ => {}
    }
}

/// `qty` moved from place `from` to place `to` of a row's places (the overlay's data, or an
/// item row of the search): never more than `from` holds, `from` gone once empty, `to` added
/// at the end when it was not there. The total stays.
pub fn move_places(places: &mut Vec<Place>, from: &str, to: &str, qty: f64) {
    if from == to || !qty.is_finite() || qty <= 0.0 {
        return;
    }
    let held: f64 = places.iter().filter(|p| p.location == from).map(|p| p.qty).sum();
    let mut left = qty.min(held);
    if left <= 0.0 {
        return;
    }
    let moved = left;
    for p in places.iter_mut().filter(|p| p.location == from) {
        let take = left.min(p.qty);
        p.qty -= take;
        left -= take;
    }
    places.retain(|p| p.location != from || p.qty > 0.0);
    match places.iter_mut().find(|p| p.location == to) {
        Some(p) => p.qty += moved,
        None => places.push(Place { location: to.to_string(), qty: moved }),
    }
}

/// A move applied to an item's stacks, part by part (`(quality, qty)`, see [`split_move`]):
/// each part leaves the stack of its quality at `from` (never more than it holds) for the
/// stack of the same quality at `to`.
pub fn move_stacks(stacks: &mut Vec<Stack>, from: &str, to: &str, parts: &[(i64, i64)]) {
    if from == to {
        return;
    }
    for &(quality, qty) in parts {
        let Some(src) = stacks.iter_mut().find(|s| s.location == from && s.quality == quality) else { continue };
        let take = (qty.max(0) as f64).min(src.qty);
        if take <= 0.0 {
            continue;
        }
        src.qty -= take;
        match stacks.iter_mut().find(|s| s.location == to && s.quality == quality) {
            Some(dst) => dst.qty += take,
            None => stacks.push(Stack { location: to.to_string(), quality, qty: take }),
        }
    }
    stacks.retain(|s| s.qty > 0.0);
}

/// The parts (`(quality, qty)`) of moving `qty` from place `from`, given the item's stacks:
/// the lowest quality first (0, not informed, before the rest), each part at most what its
/// stack holds (whole units), so never more than the place holds. With no stack at `from`
/// known (an inventory answer without it), one part of quality 0: the server moves what it
/// has there.
pub fn split_move(stacks: &[Stack], from: &str, qty: i64) -> Vec<(i64, i64)> {
    if qty <= 0 {
        return Vec::new();
    }
    let mut at: Vec<&Stack> = stacks.iter().filter(|s| s.location == from && s.qty >= 1.0).collect();
    if at.is_empty() {
        return vec![(0, qty)];
    }
    at.sort_by_key(|s| s.quality);
    let mut left = qty;
    let mut parts = Vec::new();
    for s in at {
        if left <= 0 {
            break;
        }
        let take = left.min(s.qty.floor() as i64);
        if take > 0 {
            parts.push((s.quality, take));
            left -= take;
        }
    }
    parts
}

/// The merged quantity rule of the search's item rows: with an inventory answer (`held`),
/// what it says, and 0 for an item it does not list; without one yet, `None` (no number
/// rather than a wrong 0).
pub fn held_qty(held: Option<&HashMap<String, Owned>>, item_id: &str) -> Option<f64> {
    held.map(|h| h.get(item_id).map_or(0.0, |o| o.qty))
}

/// The player holds some of `item_id`, per the inventory answer.
fn owns(held: Option<&HashMap<String, Owned>>, item_id: &str) -> bool {
    held_qty(held, item_id).is_some_and(|q| q > 0.0)
}

/// Most item rows the search lists; a line under them says how many more matched.
pub const ITEM_ROWS_MAX: usize = 50;

/// What the search lists for one text, bare (no quantity, no pin: see [`item_rows`] and
/// [`mission_rows`]): the items, those the player owns first (per the inventory answer, when
/// there is one) and then by name, at most [`ITEM_ROWS_MAX`]; the missions by title, at most
/// [`MAX_RESULTS`]; and how many of each matched in all (the counts of the filter chips).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub items: Vec<SearchItem>,
    pub item_count: usize,
    pub missions: Vec<SearchMission>,
    pub mission_count: usize,
}

/// A search answer of an older server (no index) as [`Found`]: its items ordered like the
/// index's (owned first, then by name), its missions as they came.
pub fn found_from_answer(r: &SearchResults, held: Option<&HashMap<String, Owned>>) -> Found {
    let mut items = r.items.clone();
    items.sort_by_cached_key(|i| (!owns(held, &i.item_id), i.name.to_lowercase(), i.item_id.clone()));
    Found { item_count: items.len(), items, mission_count: r.missions.len(), missions: r.missions.clone() }
}

/// One item row of the search as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRow {
    pub item_id: String,
    pub name: String,
    /// What the player has (see [`held_qty`]), with the - / + the inventory answer does not
    /// include yet on top; `None` until the inventory has answered.
    pub qty: Option<f64>,
    pub pinned: bool,
    /// Where it is kept, from the inventory answer (none for an item not held).
    pub places: Vec<Place>,
}

/// The item rows of [`Found`] as drawn: the quantity and the places from the inventory answer
/// (`held`), `adjusts` (the changes that answer does not include) added to the quantities;
/// pinned when the overlay's data (with the changes on their way) has the pin, `pins` (every
/// change on its way) applied on top.
pub fn item_rows<'a, 'b>(
    items: &[SearchItem],
    held: Option<&HashMap<String, Owned>>,
    pinned_items: &HashSet<String>,
    pins: impl IntoIterator<Item = &'a Local>,
    adjusts: impl IntoIterator<Item = &'b Local>,
) -> Vec<ItemRow> {
    let mut rows: Vec<ItemRow> = items
        .iter()
        .map(|i| ItemRow {
            item_id: i.item_id.clone(),
            name: i.name.clone(),
            qty: held_qty(held, &i.item_id),
            pinned: pinned_items.contains(&i.item_id),
            places: held.and_then(|h| h.get(&i.item_id)).map(Owned::places).unwrap_or_default(),
        })
        .collect();
    for change in pins {
        if let Local::PinItem { item_id, .. } | Local::UnpinItem { item_id } = change {
            let on = matches!(change, Local::PinItem { .. });
            for r in rows.iter_mut().filter(|r| r.item_id == *item_id) {
                r.pinned = on;
            }
        }
    }
    for change in adjusts {
        match change {
            Local::Adjust { item_id, delta } => {
                for r in rows.iter_mut().filter(|r| r.item_id == *item_id) {
                    if let Some(q) = &mut r.qty {
                        *q = add_qty(*q, *delta);
                    }
                }
            }
            Local::Move { item_id, from, to, .. } => {
                for r in rows.iter_mut().filter(|r| r.item_id == *item_id) {
                    move_places(&mut r.places, from, to, change.moved());
                }
            }
            _ => {}
        }
    }
    rows
}

/// One mission row of the search as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionRow {
    pub mission_id: String,
    pub title: String,
    pub giver: Option<String>,
    pub pinned: bool,
}

/// The mission rows of [`Found`] as drawn: pinned per the overlay's data, `pins` (every
/// change on its way) on top.
pub fn mission_rows<'a>(missions: &[SearchMission], pinned_missions: &HashSet<String>, pins: impl IntoIterator<Item = &'a Local>) -> Vec<MissionRow> {
    let mut rows: Vec<MissionRow> =
        missions.iter().map(|m| MissionRow { mission_id: m.mission_id.clone(), title: m.title.clone(), giver: m.giver.clone(), pinned: pinned_missions.contains(&m.mission_id) }).collect();
    for change in pins {
        if let Local::PinMission { mission_id, .. } | Local::UnpinMission { mission_id } = change {
            let on = matches!(change, Local::PinMission { .. });
            for r in rows.iter_mut().filter(|r| r.mission_id == *mission_id) {
                r.pinned = on;
            }
        }
    }
    rows
}

/// Lowercase without accents, for the search.
pub fn fold_search(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
        'ł' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
        'ř' => 'r',
        'ś' | 'š' => 's',
        'ť' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        c => c,
    }
}

/// Search the index on this frame, like the site's search: every word of `query` must be
/// in the name / title, ignoring case and accents. Every item of the index that matches is
/// listed, owned or not: those the player holds (per `held`, the inventory answer) first,
/// then the rest, each by name; the missions by title (see [`Found`]). A query without a
/// word finds nothing.
pub fn search_index(index: &OverlayIndex, query: &str, held: Option<&HashMap<String, Owned>>) -> Found {
    let words: Vec<String> = fold_search(query).split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return Found::default();
    }
    let hit = |folded: &str| words.iter().all(|word| folded.contains(word.as_str()));
    let mut items: Vec<(bool, &IndexItem)> = index.items.iter().filter(|i| hit(&i.folded)).map(|i| (!owns(held, &i.item_id), i)).collect();
    items.sort_by(|(a_none, a), (b_none, b)| (*a_none, a.lower.as_str(), a.item_id.as_str()).cmp(&(*b_none, b.lower.as_str(), b.item_id.as_str())));
    let mut missions: Vec<&IndexMission> = index.missions.iter().filter(|m| hit(&m.folded)).collect();
    missions.sort_by(|a, b| (a.lower.as_str(), a.mission_id.as_str()).cmp(&(b.lower.as_str(), b.mission_id.as_str())));
    Found {
        item_count: items.len(),
        items: items.into_iter().take(ITEM_ROWS_MAX).map(|(_, i)| SearchItem { item_id: i.item_id.clone(), name: i.name.clone() }).collect(),
        mission_count: missions.len(),
        missions: missions.into_iter().take(MAX_RESULTS).map(|m| SearchMission { mission_id: m.mission_id.clone(), title: m.title.clone(), giver: m.giver.clone() }).collect(),
    }
}

/// Most rows of each list the overlay keeps from an answer, and the longest text.
pub const MAX_TRACKED: usize = 200;
pub const MAX_MISSING: usize = 200;
pub const MAX_NOTICES: usize = 20;
/// Pinned missions and reward choices kept of an answer.
pub const MAX_MISSIONS: usize = 50;
pub const MAX_CHOICES: usize = 5;
/// Missions the search lists, and the rows of each kind kept of a server's search answer.
pub const MAX_RESULTS: usize = 40;
/// Sources, places and contracts of one row; changes of one notice.
pub const MAX_PER_ROW: usize = 20;
pub const MAX_CHARS: usize = 200;
/// Longest item, mission or option id the overlay sends back to the server. Ids are never
/// cut (a cut id could name another object): a longer one makes its row read-only.
pub const MAX_ID_CHARS: usize = MAX_CHARS;

/// An id the overlay may use in a request: not blank, at most [`MAX_ID_CHARS`] characters.
pub fn id_usable(id: &str) -> bool {
    !id.trim().is_empty() && id.chars().count() <= MAX_ID_CHARS
}

/// Most a move part may carry.
pub const MAX_MOVE: i64 = 1_000_000_000;

/// Every id of `change` may be used in a request (see [`id_usable`]); a move also goes
/// between two different places (`""`: no place) whose ids are never cut, in parts of a
/// positive quantity and a quality at least 0.
pub fn change_usable(change: &Local) -> bool {
    match change {
        Local::Adjust { item_id, .. } | Local::PinItem { item_id, .. } | Local::UnpinItem { item_id } | Local::Choose { item_id, .. } => id_usable(item_id),
        Local::PinMission { mission_id, .. } | Local::UnpinMission { mission_id } => id_usable(mission_id),
        Local::Move { item_id, from, to, parts } => {
            let place_ok = |p: &str| p.chars().count() <= MAX_ID_CHARS;
            id_usable(item_id)
                && from != to
                && place_ok(from)
                && place_ok(to)
                && !parts.is_empty()
                && parts.iter().all(|&(quality, qty)| quality >= 0 && (1..=MAX_MOVE).contains(&qty))
        }
    }
}

/// Cut a string to [`MAX_CHARS`] characters (on a character boundary).
fn cap(s: &mut String) {
    if let Some((at, _)) = s.char_indices().nth(MAX_CHARS) {
        s.truncate(at);
    }
}

fn cap_places(places: &mut Vec<Place>) {
    places.truncate(MAX_PER_ROW);
    for p in places {
        cap(&mut p.location);
    }
}

/// Bound what a broken or hostile server answer can make the overlay draw: list lengths and
/// every string drawn. Location ids are cut the same way everywhere, so lookups still match.
/// Item, mission and option ids (sent back in requests) and the suggested option are never
/// cut: they are not drawn, and an overlong one only makes its row read-only (see
/// [`id_usable`]); the answer's own size limit bounds them.
pub fn bound(d: &mut OverlayData) {
    cap(&mut d.channel);
    for s in [&mut d.at_location, &mut d.home].into_iter().flatten() {
        cap(s);
    }
    d.location_names = std::mem::take(&mut d.location_names)
        .into_iter()
        .map(|(mut k, mut v)| {
            cap(&mut k);
            cap(&mut v);
            (k, v)
        })
        .collect();
    d.tracked.truncate(MAX_TRACKED);
    for t in &mut d.tracked {
        cap(&mut t.name);
        cap_places(&mut t.places);
        t.sources.truncate(MAX_PER_ROW);
        for s in &mut t.sources {
            cap(&mut s.name);
        }
    }
    d.missing.truncate(MAX_MISSING);
    for m in &mut d.missing {
        cap(&mut m.name);
        cap_places(&mut m.places);
        m.contracts.truncate(MAX_PER_ROW);
        for c in &mut m.contracts {
            cap(c);
        }
    }
    d.notices.truncate(MAX_NOTICES);
    for n in &mut d.notices {
        cap(&mut n.id);
        cap(&mut n.at);
        cap(&mut n.status);
        n.changes.truncate(MAX_PER_ROW);
        for c in &mut n.changes {
            cap(&mut c.name);
        }
    }
    d.missions.truncate(MAX_MISSIONS);
    for m in &mut d.missions {
        cap(&mut m.title);
        if let Some(g) = &mut m.giver {
            cap(g);
        }
        m.rewards.truncate(MAX_PER_ROW);
        for r in &mut m.rewards {
            cap(&mut r.name);
        }
    }
    // A choice without an id cannot be answered.
    d.choices.retain(|c| c.id.is_some());
    d.choices.truncate(MAX_CHOICES);
    for c in &mut d.choices {
        cap(&mut c.at);
        if let Some(t) = &mut c.title {
            cap(t);
        }
        c.options.retain(|o| !o.item_id.trim().is_empty());
        c.options.truncate(MAX_PER_ROW);
        for o in &mut c.options {
            cap(&mut o.name);
        }
    }
}

// ----- text -----------------------------------------------------------------------------

/// `12`, or `2.5` for the rare fractional quantity.
pub fn qty(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        let s = format!("{n:.1}");
        if s.ends_with(".0") { s[..s.len() - 2].to_string() } else { s }
    }
}

/// `+2` / `-1`.
pub fn signed(n: f64) -> String {
    if n > 0.0 { format!("+{}", qty(n)) } else { qty(n) }
}

/// Name of a location id: the dataset's name, else (no name, or a name the game never filled
/// in) the id made readable (see [`readable_location_id`]); `no location` (`sem local`) for
/// `''`. Display only: never a key.
pub fn location_name(id: &str, names: &HashMap<String, String>) -> String {
    if id.trim().is_empty() {
        return tr!("no location", "sem local").into();
    }
    names.get(id).filter(|n| !n.trim().is_empty() && !is_placeholder_name(n)).cloned().unwrap_or_else(|| readable_location_id(id))
}

/// `Stanton2_Orison` → `Orison`, `RR_CRU_LEO` → `RR CRU LEO`: something readable for an id
/// without a usable name. Like the site: a leading `Stanton…_` / `Pyro…_` / `Nyx…_` /
/// `Castra…_` / `Terra…_` dropped, `_` read as a space, a space between a lower case letter
/// and an upper case one; the id itself when nothing is left.
pub fn readable_location_id(id: &str) -> String {
    let s = split_camel(&strip_system(id).replace('_', " "));
    let s = s.trim();
    if s.is_empty() { id.to_string() } else { s.to_string() }
}

/// `id` without a leading system word (any case), its digits, one optional letter and the
/// `_` after them (`Stanton2_`, `Pyro5a_`); `id` itself when it has none.
fn strip_system(id: &str) -> &str {
    for system in ["stanton", "pyro", "nyx", "castra", "terra"] {
        let Some(head) = id.get(..system.len()) else { continue };
        if !head.eq_ignore_ascii_case(system) {
            continue;
        }
        let rest = id[system.len()..].trim_start_matches(|c: char| c.is_ascii_digit());
        let rest = rest.strip_prefix(|c: char| c.is_ascii_alphabetic()).filter(|r| r.starts_with('_')).unwrap_or(rest);
        if let Some(rest) = rest.strip_prefix('_') {
            return rest;
        }
    }
    id
}

/// A space between a lower case letter and the upper case letter after it: `GrimHEX` →
/// `Grim HEX`.
fn split_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut after_lower = false;
    for c in s.chars() {
        if after_lower && c.is_ascii_uppercase() {
            out.push(' ');
        }
        after_lower = c.is_ascii_lowercase();
        out.push(c);
    }
    out
}

/// A name the game never filled in (`<= PLACEHOLDER =>`, `<= UNINITIALIZED =>`): read as no
/// name.
pub fn is_placeholder_name(name: &str) -> bool {
    let t = name.trim();
    t.len() >= 4 && t.starts_with("<=") && t.ends_with("=>") && !t[2..t.len() - 2].contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// `have`, or `have / target`, and whether the target is reached.
pub fn tracked_qty(t: &TrackedItem) -> (String, bool) {
    match t.target {
        Some(target) if target > 0.0 => (format!("{} / {}", qty(t.have), qty(target)), t.have >= target),
        _ => (qty(t.have), false),
    }
}

/// `120 MG Scrip -> 2`: how much of the tracked item what is held of a source makes.
pub fn source_line(s: &Source) -> String {
    format!("{} {} -> {}", qty(s.have), s.name, qty(s.makes))
}

/// `50 MG Scrip = 1 Wikelo Favor`: the exchange rate (hover text in interactive mode).
pub fn rate_line(s: &Source, tracked_name: &str) -> String {
    format!("{} {} = {} {tracked_name}", qty(s.give), s.name, qty(s.receive))
}

/// `faltam 28` and the contracts that ask for the item, one per line (hover text).
pub fn missing_hover(m: &MissingItem) -> String {
    let mut s = missing_text(m.missing);
    for c in &m.contracts {
        s.push('\n');
        s.push_str(c);
    }
    s
}

/// `faltam 28` (`28 missing`).
fn missing_text(n: f64) -> String {
    let n = qty(n);
    trf!("{n} missing", "faltam {n}")
}

/// One line of a row's places, unfolded: the place's name, then its quantity, and whether
/// it is where the player is right now; with the place's id and quantity (what a move from
/// it starts from).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceText {
    pub name: String,
    pub qty: String,
    pub here: bool,
    pub location: String,
    pub amount: f64,
}

/// One line per place (`Orison` `10`, `sem local` `2`, …) in the order the server gave;
/// empty places are skipped.
pub fn places(places: &[Place], names: &HashMap<String, String>, at_location: Option<&str>) -> Vec<PlaceText> {
    places
        .iter()
        .filter(|p| p.qty != 0.0)
        .map(|p| PlaceText {
            name: location_name(&p.location, names),
            qty: qty(p.qty),
            here: !p.location.is_empty() && at_location == Some(p.location.as_str()),
            location: p.location.clone(),
            amount: p.qty,
        })
        .collect()
}

// ----- moving between places --------------------------------------------------------------

/// Most locations kept of the channel's list.
pub const MAX_LOCATIONS: usize = 5_000;
/// Most places a filtered move picker lists.
pub const MOVE_MATCHES_MAX: usize = 12;

/// One location of the channel (the list the site's location picker uses), named like the
/// site names it (see [`parse_locations`]), its name folded for the picker's filter.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub id: String,
    pub name: String,
    folded: String,
    /// Offered without being asked for: on the game's star map and with a real name (see
    /// [`offered`]).
    listed: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawLocation {
    #[serde(deserialize_with = "lenient")]
    id: String,
    #[serde(deserialize_with = "lenient")]
    name: String,
    #[serde(deserialize_with = "lenient")]
    system: Option<String>,
    /// The planet or moon it hangs from; absent in open space.
    #[serde(deserialize_with = "lenient")]
    body: Option<String>,
    /// Not on the game's star map (interiors, mission sites); absent: on it.
    #[serde(deserialize_with = "lenient")]
    hidden: bool,
}

/// On the game's star map and with a name the game filled in: offered to everyone.
fn offered_by_default(l: &RawLocation) -> bool {
    !l.hidden && !is_placeholder_name(&l.name)
}

/// Places that stay offered whatever the dataset says of them: where the player is, the home,
/// and every place holding something of the player's, so nothing owned becomes unreachable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Kept {
    pub here: Option<String>,
    pub home: Option<String>,
    pub in_use: HashSet<String>,
}

impl Kept {
    pub fn contains(&self, id: &str) -> bool {
        self.here.as_deref() == Some(id) || self.home.as_deref() == Some(id) || self.in_use.contains(id)
    }
}

/// The places [`Kept`] for the overlay's data (`d`: where the player is, the home, the places
/// of the pinned and missing items), the inventory answer (`held`: every stack) and the
/// changes on their way (`changes`: where a move goes). No place (`""`) is never one.
pub fn kept_places<'a>(d: Option<&OverlayData>, held: Option<&HashMap<String, Owned>>, changes: impl IntoIterator<Item = &'a Local>) -> Kept {
    let some = |id: Option<&String>| id.filter(|id| !id.trim().is_empty()).cloned();
    let mut in_use: HashSet<String> = HashSet::new();
    let mut add = |id: &str| {
        if !id.trim().is_empty() {
            in_use.insert(id.to_string());
        }
    };
    if let Some(d) = d {
        for p in d.tracked.iter().flat_map(|t| &t.places).chain(d.missing.iter().flat_map(|m| &m.places)).filter(|p| p.qty > 0.0) {
            add(&p.location);
        }
    }
    for s in held.into_iter().flat_map(|h| h.values()).flat_map(|o| &o.stacks).filter(|s| s.qty > 0.0) {
        add(&s.location);
    }
    for change in changes {
        if let Local::Move { to, .. } = change {
            add(to);
        }
    }
    Kept { here: some(d.and_then(|d| d.at_location.as_ref())), home: some(d.and_then(|d| d.home.as_ref())), in_use }
}

/// The site's rule for which places can be chosen: those on the game's star map with a real
/// name, plus the [`Kept`] ones even when hidden or unnamed.
pub fn offered(l: &Location, kept: &Kept) -> bool {
    l.listed || kept.contains(&l.id)
}

/// How each place of `list` is called, like the site calls it: no name or a placeholder becomes
/// the readable id; places of the same system and body sharing a name get the part of their
/// ids that tells them apart (`Derelict Outpost (003)`): always for a place not offered by
/// default, for the others only when two of them share it, so a visible name stays clean.
fn location_list_names(list: &[RawLocation]) -> Vec<String> {
    let bases: Vec<String> =
        list.iter().map(|l| if l.name.trim().is_empty() || is_placeholder_name(&l.name) { readable_location_id(&l.id) } else { l.name.clone() }).collect();
    let mut groups: HashMap<(&str, &str, &str), Vec<usize>> = HashMap::new();
    for (i, l) in list.iter().enumerate() {
        groups.entry((l.system.as_deref().unwrap_or(""), l.body.as_deref().unwrap_or(""), bases[i].as_str())).or_default().push(i);
    }
    let mut out = bases.clone();
    for members in groups.values().filter(|g| g.len() > 1) {
        let visible = members.iter().filter(|&&i| offered_by_default(&list[i])).count();
        let ids: Vec<&str> = members.iter().map(|&i| list[i].id.as_str()).collect();
        for (&i, part) in members.iter().zip(distinguishers(&ids)) {
            if !offered_by_default(&list[i]) || visible > 1 {
                out[i] = format!("{} ({part})", bases[i]);
            }
        }
    }
    out
}

/// Of ids that share a name, the part that tells them apart: their `_` words without the ones
/// all of them start and end with (`…_occ_003`, `…_occ_004` → `003`, `004`); the readable id
/// when nothing is left.
fn distinguishers(ids: &[&str]) -> Vec<String> {
    let words: Vec<Vec<&str>> = ids.iter().map(|id| id.split('_').collect()).collect();
    let Some(first) = words.first() else { return Vec::new() };
    let shortest = words.iter().map(Vec::len).min().unwrap_or(0);
    let mut head = 0;
    while head + 1 < shortest && words.iter().all(|w| w[head] == first[head]) {
        head += 1;
    }
    let mut tail = 0;
    while tail + 1 + head < shortest && words.iter().all(|w| w[w.len() - 1 - tail] == first[first.len() - 1 - tail]) {
        tail += 1;
    }
    ids.iter()
        .zip(&words)
        .map(|(id, w)| {
            let part = split_camel(&w[head..w.len() - tail].join(" "));
            let part = part.trim();
            if part.is_empty() { readable_location_id(id) } else { part.to_string() }
        })
        .collect()
}

/// What is read of the channel's dataset (`GET /api/channels/{channel}/dataset`): only its
/// locations; everything else in it is skipped.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawDataset {
    #[serde(deserialize_with = "lenient")]
    locations: Vec<RawLocation>,
}

/// The channel's locations from its dataset, by name: ids that cannot be sent back (blank, too
/// long) left out, each id once, named as `location_list_names` says, at most
/// [`MAX_LOCATIONS`]. Every one is kept, offered or not: [`offered`] decides when the picker
/// lists them. `Err` only for a body that is not a JSON object.
pub fn parse_locations(body: &[u8]) -> Result<Vec<Location>, String> {
    // Not read into a `Value` first (the dataset is large): a struct would also take a list.
    if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(unexpected());
    }
    let raw: RawDataset = serde_json::from_slice(body).map_err(unexpected_because)?;
    let mut seen = HashSet::new();
    let list: Vec<RawLocation> = raw.locations.into_iter().filter(|l| id_usable(&l.id) && seen.insert(l.id.clone())).take(MAX_LOCATIONS).collect();
    let names = location_list_names(&list);
    let mut out: Vec<Location> = list
        .into_iter()
        .zip(names)
        .map(|(l, mut name)| {
            cap(&mut name);
            let listed = offered_by_default(&l);
            Location { folded: fold_search(&name), id: l.id, name, listed }
        })
        .collect();
    out.sort_by(|a, b| (a.folded.as_str(), a.id.as_str()).cmp(&(b.folded.as_str(), b.id.as_str())));
    Ok(out)
}

/// Why a place is offered in the move picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// Where the player is now.
    Here,
    /// The account's home.
    Home,
    /// A place that holds some of the item already.
    Holding,
    /// A location of the channel matching the picker's filter.
    Found,
}

/// One place the move picker offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub kind: TargetKind,
}

/// The places a move from `from` can go to, in the picker's order: where the player is and
/// the home first (`kept`), then the places already holding the item (`holding`), then, for a
/// typed `filter`, the channel's locations whose name has every word of it (ignoring case and
/// accents) and that are [`offered`], at most [`MOVE_MATCHES_MAX`] of those. Never `from`,
/// each place once; names from the channel's list (named like the site names them), else the
/// overlay's data (`names`), else the readable id (see [`location_name`]).
pub fn move_targets(from: &str, kept: &Kept, holding: &[Place], names: &HashMap<String, String>, locations: &[Location], filter: &str) -> Vec<Target> {
    let name_of = |id: &str| locations.iter().find(|l| l.id == id).map_or_else(|| location_name(id, names), |l| l.name.clone());
    let mut out: Vec<Target> = Vec::new();
    let add = |id: &str, kind: TargetKind, out: &mut Vec<Target>| {
        if id != from && id_usable(id) && !out.iter().any(|t| t.id == id) {
            out.push(Target { id: id.to_string(), name: name_of(id), kind });
        }
    };
    for (id, kind) in [(&kept.here, TargetKind::Here), (&kept.home, TargetKind::Home)] {
        if let Some(id) = id {
            add(id, kind, &mut out);
        }
    }
    for p in holding.iter().filter(|p| p.qty > 0.0) {
        add(&p.location, TargetKind::Holding, &mut out);
    }
    let words: Vec<String> = fold_search(filter).split_whitespace().map(str::to_string).collect();
    if !words.is_empty() {
        let mut found = 0;
        for l in locations.iter().filter(|l| offered(l, kept) && words.iter().all(|w| l.folded.contains(w.as_str()))) {
            if found >= MOVE_MATCHES_MAX {
                break;
            }
            let before = out.len();
            add(&l.id, TargetKind::Found, &mut out);
            found += out.len() - before;
        }
    }
    out
}

/// The places as text, one line each (`Orison 10`), joined by ` · ` (for the tests).
#[cfg(test)]
pub fn places_line(list: &[Place], names: &HashMap<String, String>, at_location: Option<&str>) -> String {
    places(list, names, at_location).into_iter().map(|p| format!("{} {}", p.name, p.qty)).collect::<Vec<_>>().join(" · ")
}

/// `+2 Wikelo Favor, -1 MG Scrip`.
pub fn changes_line(n: &Notice) -> String {
    n.changes.iter().map(|c| format!("{} {}", signed(c.delta), c.name)).collect::<Vec<_>>().join(", ")
}

/// `22:10` in `offset` from an RFC 3339 timestamp; the raw text when it does not parse.
pub fn hhmm(rfc3339: &str, offset: time::UtcOffset) -> String {
    match time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339) {
        Ok(t) => {
            let t = t.to_offset(offset);
            format!("{:02}:{:02}", t.hour(), t.minute())
        }
        Err(_) => rfc3339.to_string(),
    }
}

/// `22:10  +2 Wikelo Favor, -1 MG Scrip`, with `· pendente` for one waiting on the site.
pub fn notice_line(n: &Notice, offset: time::UtcOffset) -> String {
    let mut s = format!("{}  {}", hhmm(&n.at, offset), changes_line(n));
    if n.is_pending() {
        s.push_str(tr!(" · pending", " · pendente"));
    }
    s
}

/// `1 pendente no site` / `3 pendentes no site`.
pub fn pending_text(n: f64) -> String {
    if n.round() as i64 == 1 {
        tr!("1 pending on the site", "1 pendente no site").into()
    } else {
        trf!("{} pending on the site", "{} pendentes no site", qty(n))
    }
}

/// What tells notices apart from one answer to the next: the server's id, else the time and
/// the changes (a status going from pending to applied is still the same notice).
pub fn notice_key(n: &Notice) -> String {
    if n.id.trim().is_empty() { format!("at:{}|{}", n.at, changes_line(n)) } else { format!("id:{}", n.id) }
}

/// Name of the place the player is at, when the server knows it.
pub fn current_place(d: &OverlayData) -> Option<String> {
    d.at_location.as_deref().filter(|id| !id.trim().is_empty()).map(|id| location_name(id, &d.location_names))
}

/// One row of the "Aqui" card: something stored where the player stands that a pinned item
/// or a pinned contract needs.
#[derive(Debug, Clone, PartialEq)]
pub struct HereRow {
    pub item_id: String,
    pub name: String,
    /// How much of it is stored right here.
    pub qty: f64,
    /// Pinned on its own: `have` or `have / target` over all places.
    pub tracked: Option<String>,
    /// Still missing for the pinned contracts.
    pub missing: Option<f64>,
    /// `(have, goal)` for the progress bar: the pinned item's target, else what the pinned
    /// contracts need.
    pub goal: Option<(f64, f64)>,
}

/// The "Aqui" card, built from the answer alone: the pinned items, then the items the pinned
/// contracts still lack, each only when some of it is stored at the current location. An
/// item in both lists is one row. Empty when the location is unknown.
pub fn here_rows(d: &OverlayData) -> Vec<HereRow> {
    let Some(at) = d.at_location.as_deref().filter(|id| !id.trim().is_empty()) else { return Vec::new() };
    let held = |places: &[Place]| places.iter().filter(|p| p.location == at).map(|p| p.qty).sum::<f64>();
    let key = |id: &str, name: &str| if id.is_empty() { format!("name:{name}") } else { format!("id:{id}") };
    let mut rows: Vec<HereRow> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for t in &d.tracked {
        let qty = held(t.places.as_slice());
        if qty <= 0.0 {
            continue;
        }
        let goal = t.target.filter(|g| *g > 0.0).map(|g| (t.have, g));
        rows.push(HereRow { item_id: t.item_id.clone(), name: t.name.clone(), qty, tracked: Some(tracked_qty(t).0), missing: None, goal });
        keys.push(key(&t.item_id, &t.name));
    }
    for m in &d.missing {
        let qty = held(m.places.as_slice());
        if qty <= 0.0 {
            continue;
        }
        let k = key(&m.item_id, &m.name);
        let goal = (m.need > 0.0).then_some((m.have, m.need));
        match keys.iter().position(|x| *x == k) {
            Some(i) => {
                rows[i].missing = Some(m.missing);
                rows[i].qty = rows[i].qty.max(qty);
                rows[i].goal = rows[i].goal.or(goal);
            }
            None => {
                rows.push(HereRow { item_id: m.item_id.clone(), name: m.name.clone(), qty, tracked: None, missing: Some(m.missing), goal });
                keys.push(k);
            }
        }
    }
    rows
}

/// `fixado: 3 / 10 · faltam 28`: why a row of "Aqui" is there.
pub fn here_detail(r: &HereRow) -> String {
    let mut parts = Vec::new();
    if let Some(q) = &r.tracked {
        parts.push(trf!("pinned: {q}", "fixado: {q}"));
    }
    if let Some(m) = r.missing {
        parts.push(if m > 0.0 { missing_text(m) } else { tr!("pinned contracts", "contratos fixados").into() });
    }
    parts.join(" · ")
}

/// How far `have` is toward `goal` (0 to 1) and whether it is reached; `None` without a
/// goal (none, zero or not a number).
pub fn progress(have: f64, goal: Option<f64>) -> Option<(f32, bool)> {
    let goal = goal.filter(|g| g.is_finite() && *g > 0.0)?;
    let have = if have.is_finite() { have.max(0.0) } else { 0.0 };
    Some(((have / goal).min(1.0) as f32, have >= goal))
}

/// `2× Carinite`: a reward's quantity and name.
pub fn reward_line(r: &Reward) -> String {
    format!("{}× {}", qty(r.qty), r.name)
}

/// A change the player made in the overlay, drawn at once while its request is on its way.
#[derive(Debug, Clone, PartialEq)]
pub enum Local {
    Adjust { item_id: String, delta: i64 },
    /// Pinned from a search result: its name and what the player has, for the row drawn
    /// until the next read brings the real one.
    PinItem { item_id: String, name: String, have: f64 },
    UnpinItem { item_id: String },
    PinMission { mission_id: String, title: String, giver: Option<String> },
    UnpinMission { mission_id: String },
    /// The item the player says a one-of mission paid.
    Choose { id: i64, item_id: String },
    /// Some of an item moved from place `from` to place `to` (`""`: no place), in parts of
    /// one quality each (`(quality, qty)`, see [`split_move`]): one
    /// `POST /api/inventory/{item}/move` per part, in order.
    Move { item_id: String, from: String, to: String, parts: Vec<(i64, i64)> },
}

impl Local {
    /// How many a move moves (0 for anything else).
    pub fn moved(&self) -> f64 {
        match self {
            Local::Move { parts, .. } => parts.iter().map(|&(_, q)| q.max(0) as f64).sum(),
            _ => 0.0,
        }
    }
}

/// `have` changed by `delta`, never below zero.
pub fn add_qty(have: f64, delta: i64) -> f64 {
    (have + delta as f64).max(0.0)
}

/// `server` with the changes still on their way applied, in the order they were made (see
/// `overlay_sync::Engine`). An unpin or a choice removes nothing: its row stays where it is,
/// marked, until a read settles it.
pub fn view<'a>(server: &OverlayData, changes: impl IntoIterator<Item = &'a Local>) -> OverlayData {
    let mut d = server.clone();
    for change in changes {
        apply_local(&mut d, change);
    }
    d
}

/// An unpin the server answered 404 that `d` (a read after it) still shows pinned.
pub fn still_pinned(d: &OverlayData, change: &Local) -> bool {
    match change {
        Local::UnpinItem { item_id } => d.tracked.iter().any(|t| t.item_id == *item_id),
        Local::UnpinMission { mission_id } => d.missions.iter().any(|m| m.mission_id == *mission_id),
        _ => false,
    }
}

/// A reward option's button: `2× Armor A` when the server says what it adds, else the name.
pub fn option_label(o: &ChoiceOption) -> String {
    match o.qty.filter(|q| *q > 0.0) {
        Some(q) => format!("{}× {}", qty(q), o.name),
        None => o.name.clone(),
    }
}

/// For each id, how many times it came before in the list: rows keyed by `(id, n)` stay
/// distinct when the server repeats an id, and keep their key when others come and go.
pub fn occurrences<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    ids.into_iter()
        .map(|id| {
            let n = seen.entry(id).or_insert(0);
            *n += 1;
            *n - 1
        })
        .collect()
}

fn apply_local(d: &mut OverlayData, change: &Local) {
    match change {
        Local::Adjust { item_id, delta } => {
            for t in &mut d.tracked {
                if t.item_id == *item_id {
                    t.have = add_qty(t.have, *delta);
                }
                for s in t.sources.iter_mut().filter(|s| s.item_id == *item_id) {
                    s.have = add_qty(s.have, *delta);
                    if s.give > 0.0 {
                        s.makes = (s.have / s.give).floor() * s.receive;
                    }
                }
            }
            for m in d.missing.iter_mut().filter(|m| m.item_id == *item_id) {
                m.have = add_qty(m.have, *delta);
                m.missing = (m.need - m.have).max(0.0);
            }
            for r in d.missions.iter_mut().flat_map(|m| m.rewards.iter_mut()).filter(|r| r.item_id == *item_id) {
                r.have = add_qty(r.have, *delta);
            }
        }
        Local::PinItem { item_id, name, have } => {
            if !d.tracked.iter().any(|t| t.item_id == *item_id) {
                d.tracked.push(TrackedItem { item_id: item_id.clone(), name: name.clone(), have: *have, ..Default::default() });
            }
        }
        Local::PinMission { mission_id, title, giver } => {
            if !d.missions.iter().any(|m| m.mission_id == *mission_id) {
                d.missions.push(PinnedMission { mission_id: mission_id.clone(), title: title.clone(), giver: giver.clone(), rewards: Vec::new() });
            }
        }
        // Where it is changes, the totals stay.
        Local::Move { item_id, from, to, .. } => {
            let qty = change.moved();
            for t in d.tracked.iter_mut().filter(|t| t.item_id == *item_id) {
                move_places(&mut t.places, from, to, qty);
            }
            for m in d.missing.iter_mut().filter(|m| m.item_id == *item_id) {
                move_places(&mut m.places, from, to, qty);
            }
        }
        // Kept in place, marked (see `view`): nothing under the mouse moves.
        Local::UnpinItem { .. } | Local::UnpinMission { .. } | Local::Choose { .. } => {}
    }
}

/// Percent-encode one path segment (item ids and channels go into URLs). A segment that is
/// exactly `.` or `..` is encoded whole, so no URL normalisation can turn it into a step up.
pub fn path_segment(s: &str) -> String {
    if s == "." || s == ".." {
        return "%2E".repeat(s.len());
    }
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "channel": "LIVE",
      "atLocation": "Stanton2_Orison",
      "home": "Stanton1_Lorville",
      "locationNames": { "Stanton2_Orison": "Orison", "RR_CRU_LEO": "Seraphim Station" },
      "missing": [ { "itemId": "x", "name": "Carinite", "need": 40, "have": 12, "missing": 28, "places": [ { "location": "Stanton2_Orison", "qty": 10 }, { "location": "", "qty": 2 } ], "contracts": ["Contract title"] } ],
      "tracked": [ { "itemId": "favor", "name": "Wikelo Favor", "have": 3, "target": 10, "places": [ { "location": "Stanton2_Orison", "qty": 3 } ], "sources": [ { "itemId": "scrip", "name": "MG Scrip", "have": 120, "give": 50, "receive": 1, "makes": 2 } ] } ],
      "notices": [ { "id": 12, "at": "2026-11-15T22:10:00Z", "status": "applied", "changes": [ { "name": "Wikelo Favor", "delta": 2 } ] } ],
      "pending": 1
    }"#;

    #[test]
    fn reads_the_full_sample() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(d.channel, "LIVE");
        assert_eq!(d.at_location.as_deref(), Some("Stanton2_Orison"));
        assert_eq!(d.home.as_deref(), Some("Stanton1_Lorville"));
        assert_eq!(d.location_names.get("RR_CRU_LEO").map(String::as_str), Some("Seraphim Station"));
        assert_eq!(d.pending, 1.0);
        let m = &d.missing[0];
        assert_eq!((m.item_id.as_str(), m.name.as_str(), m.need, m.have, m.missing), ("x", "Carinite", 40.0, 12.0, 28.0));
        assert_eq!(m.places, vec![Place { location: "Stanton2_Orison".into(), qty: 10.0 }, Place { location: "".into(), qty: 2.0 }]);
        assert_eq!(m.contracts, vec!["Contract title".to_string()]);
        let t = &d.tracked[0];
        assert_eq!((t.item_id.as_str(), t.have, t.target), ("favor", 3.0, Some(10.0)));
        assert_eq!(t.sources[0], Source { item_id: "scrip".into(), name: "MG Scrip".into(), have: 120.0, give: 50.0, receive: 1.0, makes: 2.0 });
        let n = &d.notices[0];
        assert_eq!((n.id.as_str(), n.at.as_str(), n.status.as_str(), n.is_pending()), ("12", "2026-11-15T22:10:00Z", "applied", false));
        assert_eq!(n.changes, vec![Change { name: "Wikelo Favor".into(), delta: 2.0 }]);
    }

    #[test]
    fn an_empty_object_is_an_empty_overlay() {
        assert_eq!(parse(b"{}").unwrap(), OverlayData::default());
    }

    #[test]
    fn unknown_fields_and_odd_values_are_tolerated() {
        let body = br#"{
          "channel": "PTU", "version": 7, "somethingNew": { "a": [1, 2] },
          "atLocation": null, "home": 5,
          "missing": [ { "itemId": "x", "name": "Carinite", "need": "40", "have": 12.0, "missing": null, "extra": true, "places": null } ],
          "tracked": [ { "itemId": "favor", "target": null, "sources": "not a list" } ],
          "notices": [ { "at": "2026-11-15T22:10:00Z", "status": "pending", "changes": [ { "name": "MG Scrip", "delta": -1, "itemId": "scrip" } ], "kind": "x" } ],
          "pending": 2.0,
          "locationNames": []
        }"#;
        let d = parse(body).unwrap();
        assert_eq!(d.channel, "PTU");
        assert_eq!((d.at_location.clone(), d.home.clone()), (None, None));
        assert_eq!((d.missing[0].need, d.missing[0].have, d.missing[0].missing), (40.0, 12.0, 0.0));
        assert!(d.missing[0].places.is_empty());
        assert_eq!((d.tracked[0].target, d.tracked[0].sources.len(), d.tracked[0].name.as_str()), (None, 0, ""));
        assert!(d.notices[0].is_pending());
        assert_eq!(d.notices[0].id, "", "no id: empty");
        assert!(d.location_names.is_empty());
        assert_eq!(d.pending, 2.0);
    }

    #[test]
    fn not_an_object_is_an_error() {
        assert!(parse(b"[]").is_err());
        assert!(parse(b"<html>").is_err());
    }

    #[test]
    fn source_line_reads_have_name_makes() {
        let s = Source { item_id: "scrip".into(), name: "MG Scrip".into(), have: 120.0, give: 50.0, receive: 1.0, makes: 2.0 };
        assert_eq!(source_line(&s), "120 MG Scrip -> 2");
        assert_eq!(source_line(&Source { have: 2.5, makes: 0.0, name: "X".into(), ..Default::default() }), "2.5 X -> 0");
    }

    #[test]
    fn places_line_names_and_marks_here() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        let m = &d.missing[0];
        assert_eq!(places_line(&m.places, &d.location_names, d.at_location.as_deref()), "Orison 10 · sem local 2");
        let parts = places(&m.places, &d.location_names, d.at_location.as_deref());
        assert_eq!(parts.iter().map(|p| p.here).collect::<Vec<_>>(), vec![true, false]);
        let unknown = [Place { location: "Pyro_Ruin".into(), qty: 4.0 }, Place { location: "RR_CRU_LEO".into(), qty: 0.0 }];
        assert_eq!(places_line(&unknown, &d.location_names, None), "Ruin 4", "unknown id made readable; empty places skipped");
        // Standing nowhere known: the unassigned stock is never "here".
        assert!(places(&m.places, &d.location_names, Some("")).iter().all(|p| !p.here));
    }

    #[test]
    fn notice_line_has_time_changes_and_pending() {
        let utc = time::UtcOffset::UTC;
        let n = Notice {
            id: String::new(),
            at: "2026-11-15T22:10:00Z".into(),
            status: "applied".into(),
            changes: vec![Change { name: "Wikelo Favor".into(), delta: 2.0 }, Change { name: "MG Scrip".into(), delta: -1.0 }],
        };
        assert_eq!(changes_line(&n), "+2 Wikelo Favor, -1 MG Scrip");
        assert_eq!(notice_line(&n, utc), "22:10  +2 Wikelo Favor, -1 MG Scrip");
        let p = Notice { status: "pending".into(), ..n.clone() };
        assert_eq!(notice_line(&p, utc), "22:10  +2 Wikelo Favor, -1 MG Scrip · pendente");
        let brt = time::UtcOffset::from_hms(-3, 0, 0).unwrap();
        assert_eq!(hhmm("2026-11-15T22:10:00Z", brt), "19:10");
        assert_eq!(hhmm("ontem", utc), "ontem");
    }

    #[test]
    fn quantities_and_counts() {
        assert_eq!(qty(12.0), "12");
        assert_eq!(qty(2.5), "2.5");
        assert_eq!(qty(0.0), "0");
        assert_eq!(qty(-3.0), "-3");
        assert_eq!(signed(2.0), "+2");
        assert_eq!(signed(-1.0), "-1");
        let t = TrackedItem { have: 3.0, target: Some(10.0), ..Default::default() };
        assert_eq!(tracked_qty(&t), ("3 / 10".to_string(), false));
        assert_eq!(tracked_qty(&TrackedItem { have: 10.0, ..t.clone() }), ("10 / 10".to_string(), true));
        assert_eq!(tracked_qty(&TrackedItem { target: None, ..t }), ("3".to_string(), false));
        assert_eq!(pending_text(1.0), "1 pendente no site");
        assert_eq!(pending_text(3.0), "3 pendentes no site");
    }

    #[test]
    fn rate_and_contracts_hovers() {
        let s = Source { item_id: "scrip".into(), name: "MG Scrip".into(), have: 120.0, give: 50.0, receive: 1.0, makes: 2.0 };
        assert_eq!(rate_line(&s, "Wikelo Favor"), "50 MG Scrip = 1 Wikelo Favor");
        let m = MissingItem { missing: 28.0, contracts: vec!["A".into(), "B".into()], ..Default::default() };
        assert_eq!(missing_hover(&m), "faltam 28\nA\nB");
    }

    #[test]
    fn notices_are_told_apart_by_id_else_time_and_changes() {
        let d = parse(br#"{"notices":[{"id":12,"at":"t1"},{"id":"a-7","at":"t1"},{"at":"t2","status":"pending","changes":[{"name":"X","delta":2}]},{"id":{"x":1},"at":"t3"}]}"#).unwrap();
        let keys: Vec<String> = d.notices.iter().map(notice_key).collect();
        assert_eq!(keys, vec!["id:12", "id:a-7", "at:t2|+2 X", "at:t3|"]);
        let applied = Notice { status: "applied".into(), ..d.notices[2].clone() };
        assert_eq!(notice_key(&applied), keys[2], "pending -> applied is the same notice");
    }

    #[test]
    fn current_place_names_the_location() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(current_place(&d).as_deref(), Some("Orison"));
        let unnamed = OverlayData { at_location: Some("Pyro_Ruin".into()), ..d.clone() };
        assert_eq!(current_place(&unnamed).as_deref(), Some("Ruin"));
        assert_eq!(current_place(&OverlayData { at_location: Some(" ".into()), ..d.clone() }), None);
        assert_eq!(current_place(&OverlayData { at_location: None, ..d }), None);
    }

    #[test]
    fn here_lists_what_pinned_things_need_at_this_place() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        let rows = here_rows(&d);
        assert_eq!(
            rows,
            vec![
                HereRow { item_id: "favor".into(), name: "Wikelo Favor".into(), qty: 3.0, tracked: Some("3 / 10".into()), missing: None, goal: Some((3.0, 10.0)) },
                HereRow { item_id: "x".into(), name: "Carinite".into(), qty: 10.0, tracked: None, missing: Some(28.0), goal: Some((12.0, 40.0)) },
            ]
        );
        assert_eq!(rows.iter().map(here_detail).collect::<Vec<_>>(), vec!["fixado: 3 / 10", "faltam 28"]);

        // Somewhere else: only what is stored there; nowhere known: nothing.
        let away = OverlayData { at_location: Some("RR_CRU_LEO".into()), ..d.clone() };
        assert!(here_rows(&away).is_empty());
        assert!(here_rows(&OverlayData { at_location: None, ..d.clone() }).is_empty());
        assert!(here_rows(&OverlayData { at_location: Some(String::new()), ..d.clone() }).is_empty(), "the unassigned stock is never here");

        // An item both pinned and missing for a contract is one row with both reasons.
        let mut both = d.clone();
        both.missing[0].item_id = "favor".into();
        both.missing[0].name = "Wikelo Favor".into();
        both.missing[0].places = vec![Place { location: "Stanton2_Orison".into(), qty: 3.0 }, Place { location: "Stanton2_Orison".into(), qty: 1.0 }];
        let rows = here_rows(&both);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].qty, rows[0].missing), (4.0, Some(28.0)), "places at the same location add up");
        assert_eq!(here_detail(&rows[0]), "fixado: 3 / 10 · faltam 28");
        assert_eq!(rows[0].goal, Some((3.0, 10.0)), "the pinned item's target wins");
        // Pinned without a target: the contracts' need is the goal.
        both.tracked[0].target = None;
        assert_eq!(here_rows(&both)[0].goal, Some((12.0, 40.0)));
    }

    #[test]
    fn reads_missions_and_choices() {
        let body = br#"{
          "missions": [ { "missionId": "m1", "title": "Bring the goods", "giver": "Wikelo", "rewards": [
              { "itemId": "favor", "name": "Wikelo Favor", "qty": 2, "have": 3, "oneOf": false },
              { "itemId": "a", "name": "Armor A", "qty": 1, "have": 0, "oneOf": true } ] },
            { "missionId": "m2", "title": "No giver", "giver": null, "rewards": null } ],
          "choices": [ { "id": 41, "at": "2026-11-16T10:00:00Z", "title": "Bring the goods", "qty": 1, "options": [ { "itemId": "a", "name": "Armor A" }, { "itemId": "b", "name": "Armor B" } ], "suggested": "b" },
            { "id": "42", "at": "t", "title": null, "qty": "2", "options": [ { "itemId": "", "name": "no id" } ], "suggested": null },
            { "at": "no id: cannot be answered" } ]
        }"#;
        let d = parse(body).unwrap();
        assert_eq!(d.missions.len(), 2);
        let m = &d.missions[0];
        assert_eq!((m.mission_id.as_str(), m.title.as_str(), m.giver.as_deref()), ("m1", "Bring the goods", Some("Wikelo")));
        assert_eq!(m.rewards[1], Reward { item_id: "a".into(), name: "Armor A".into(), qty: 1.0, have: 0.0, one_of: true });
        assert_eq!(reward_line(&m.rewards[0]), "2× Wikelo Favor");
        assert_eq!((d.missions[1].giver.clone(), d.missions[1].rewards.len()), (None, 0));
        assert_eq!(d.choices.iter().map(|c| c.id).collect::<Vec<_>>(), vec![Some(41), Some(42)], "a choice without an id is dropped");
        let c = &d.choices[0];
        assert_eq!((c.title.as_deref(), c.qty, c.suggested.as_deref(), c.options.len()), (Some("Bring the goods"), 1.0, Some("b"), 2));
        assert_eq!((d.choices[1].qty, d.choices[1].options.len()), (2.0, 0), "an option without an item cannot be chosen");
    }

    #[test]
    fn an_older_server_has_no_missions_nor_choices() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        assert!(d.missions.is_empty() && d.choices.is_empty());
        let odd = parse(br#"{"missions": "x", "choices": {"id": 1}}"#).unwrap();
        assert!(odd.missions.is_empty() && odd.choices.is_empty(), "odd shapes are empty lists");
    }

    #[test]
    fn reads_search_results() {
        let body = br#"{ "items": [ { "itemId": "favor", "name": "Wikelo Favor", "have": 3, "pinned": true, "target": 10 }, { "itemId": "", "name": "no id" }, { "itemId": "x", "name": "Carinite", "target": null } ],
                         "missions": [ { "missionId": "m1", "title": "Bring", "giver": "Wikelo", "pinned": false }, { "title": "no id" } ], "extra": 1 }"#;
        let r = parse_search(body).unwrap();
        assert_eq!(r.items.iter().map(|i| (i.item_id.as_str(), i.name.as_str())).collect::<Vec<_>>(), vec![("favor", "Wikelo Favor"), ("x", "Carinite")], "what it says of quantities and pins is not read");
        assert_eq!(r.missions.len(), 1);
        assert_eq!(r.missions[0].giver.as_deref(), Some("Wikelo"));
        assert_eq!(parse_search(b"{}").unwrap(), SearchResults::default());
        assert!(parse_search(b"[]").is_err());
        let many = serde_json::json!({ "items": vec![serde_json::json!({ "itemId": "i", "name": "x" }); 60], "missions": vec![serde_json::json!({ "missionId": "m" }); 60] });
        let r = parse_search(many.to_string().as_bytes()).unwrap();
        assert_eq!((r.items.len(), r.missions.len()), (MAX_RESULTS, MAX_RESULTS));
    }

    #[test]
    fn progress_is_a_fraction_of_the_goal() {
        assert_eq!(progress(3.0, Some(10.0)), Some((0.3, false)));
        assert_eq!(progress(10.0, Some(10.0)), Some((1.0, true)));
        assert_eq!(progress(25.0, Some(10.0)), Some((1.0, true)), "past the goal: full");
        assert_eq!(progress(-2.0, Some(10.0)), Some((0.0, false)));
        assert_eq!(progress(f64::NAN, Some(4.0)), Some((0.0, false)));
        assert_eq!(progress(3.0, None), None);
        assert_eq!(progress(3.0, Some(0.0)), None, "no goal: no bar");
        assert_eq!(progress(3.0, Some(f64::INFINITY)), None);
    }

    fn with_everything() -> OverlayData {
        let mut d = parse(SAMPLE.as_bytes()).unwrap();
        d.missions = vec![PinnedMission { mission_id: "m1".into(), title: "Bring".into(), giver: None, rewards: vec![Reward { item_id: "x".into(), name: "Carinite".into(), qty: 2.0, have: 12.0, one_of: false }] }];
        d.choices = vec![Choice { id: Some(41), ..Default::default() }, Choice { id: Some(42), ..Default::default() }];
        d
    }

    #[test]
    fn a_change_shows_at_once_and_settles_with_the_next_read() {
        let server = with_everything();
        // The changes on their way, in the order they were made (`overlay_sync::Engine`
        // keeps them and drops the ones a read settled).
        let changes = [Local::Adjust { item_id: "x".into(), delta: 10 }, Local::Adjust { item_id: "favor".into(), delta: -1 }, Local::Adjust { item_id: "scrip".into(), delta: 30 }];
        let v = view(&server, &changes);
        assert_eq!((v.missing[0].have, v.missing[0].missing), (22.0, 18.0), "the missing count follows");
        assert_eq!(v.missions[0].rewards[0].have, 22.0, "every row of the item");
        assert_eq!(v.tracked[0].have, 2.0);
        assert_eq!((v.tracked[0].sources[0].have, v.tracked[0].sources[0].makes), (150.0, 3.0), "what a source makes follows");
        assert_eq!(server.missing[0].have, 12.0, "the server's answer itself is untouched");

        // Never below zero.
        let down = [Local::Adjust { item_id: "favor".into(), delta: -10 }, Local::Adjust { item_id: "favor".into(), delta: 1 }];
        assert_eq!(view(&server, &down).tracked[0].have, 1.0);

        // A read that includes the first two: their changes are the server's now.
        let mut read = server.clone();
        read.missing[0].have = 22.0;
        read.tracked[0].have = 2.0;
        let v = view(&read, &changes[2..]);
        assert_eq!((v.missing[0].have, v.tracked[0].have), (22.0, 2.0), "not counted twice");
        assert_eq!(v.tracked[0].sources[0].have, 150.0, "the third is still on its way");
        assert_eq!(view(&read, std::iter::empty()), read);
    }

    #[test]
    fn a_failed_write_puts_the_number_back() {
        let server = with_everything();
        let both = [Local::Adjust { item_id: "favor".into(), delta: 1 }, Local::Adjust { item_id: "favor".into(), delta: 10 }];
        assert_eq!(view(&server, &both).tracked[0].have, 14.0);
        // The second was refused (`Engine::on_write` drops it): only it goes back.
        assert_eq!(view(&server, &both[..1]).tracked[0].have, 4.0, "only the failed one goes back");
        assert_eq!(view(&server, std::iter::empty()), server);
        assert_eq!(view(&server, &[Local::Choose { id: 41, item_id: "a".into() }]), server, "a choice moves nothing");
    }

    #[test]
    fn pins_show_at_once_and_unpins_and_choices_keep_their_rows() {
        let server = with_everything();
        let changes = [
            Local::UnpinItem { item_id: "favor".into() },
            Local::PinItem { item_id: "new".into(), name: "New".into(), have: 4.0 },
            Local::Adjust { item_id: "new".into(), delta: 1 },
            Local::UnpinMission { mission_id: "m1".into() },
            Local::PinMission { mission_id: "m2".into(), title: "Other".into(), giver: Some("Wikelo".into()) },
            Local::Choose { id: 41, item_id: "a".into() },
        ];
        let v = view(&server, &changes);
        assert_eq!(v.tracked.iter().map(|t| (t.item_id.as_str(), t.have)).collect::<Vec<_>>(), vec![("favor", 3.0), ("new", 5.0)], "the unpinned row stays in place until a read settles it");
        assert_eq!(v.missions.iter().map(|m| m.mission_id.as_str()).collect::<Vec<_>>(), vec!["m1", "m2"]);
        assert_eq!(v.choices.iter().map(|c| c.id).collect::<Vec<_>>(), vec![Some(41), Some(42)], "the answered choice too");
        assert_eq!(view(&server, &changes[..5]).choices.len(), 2);
        // Pinning what is already pinned adds no second row.
        let again = [Local::PinItem { item_id: "favor".into(), name: "Wikelo Favor".into(), have: 3.0 }];
        assert_eq!(view(&server, &again).tracked.len(), 1);
        // An unpin answered 404 is checked against the next read.
        assert!(still_pinned(&server, &changes[0]) && still_pinned(&server, &changes[3]));
        let mut read = server.clone();
        read.tracked.clear();
        read.missions.clear();
        assert!(!still_pinned(&read, &changes[0]) && !still_pinned(&read, &changes[3]));
        assert!(!still_pinned(&server, &changes[1]), "only unpins are checked");
    }

    #[test]
    fn ids_are_never_cut_and_overlong_ones_are_read_only() {
        let long = "i".repeat(MAX_ID_CHARS + 1);
        let fits = "é".repeat(MAX_ID_CHARS);
        assert!(id_usable("favor") && id_usable(&fits), "counted in characters");
        assert!(!id_usable(&long) && !id_usable("") && !id_usable("  "));
        assert!(change_usable(&Local::Adjust { item_id: "x".into(), delta: 1 }));
        assert!(!change_usable(&Local::UnpinItem { item_id: long.clone() }));
        assert!(!change_usable(&Local::UnpinMission { mission_id: long.clone() }));
        assert!(!change_usable(&Local::Choose { id: 41, item_id: String::new() }));
        let body = serde_json::json!({
            "tracked": [ { "itemId": long, "name": "x" } ],
            "missions": [ { "missionId": long, "rewards": [ { "itemId": long } ] } ],
            "choices": [ { "id": 41, "suggested": long, "options": [ { "itemId": long, "name": "a" } ] } ],
        });
        let d = parse(body.to_string().as_bytes()).unwrap();
        assert_eq!(d.tracked[0].item_id, long);
        assert_eq!(d.missions[0].mission_id, long);
        assert_eq!(d.missions[0].rewards[0].item_id, long);
        assert_eq!((d.choices[0].options[0].item_id.as_str(), d.choices[0].suggested.as_deref()), (long.as_str(), Some(long.as_str())));
        let found = parse_search(serde_json::json!({ "items": [ { "itemId": long } ], "missions": [ { "missionId": long } ] }).to_string().as_bytes()).unwrap();
        assert_eq!((found.items[0].item_id.as_str(), found.missions[0].mission_id.as_str()), (long.as_str(), long.as_str()));
    }

    #[test]
    fn options_show_their_own_quantity_when_the_server_sends_it() {
        let d = parse(br#"{"choices":[{"id":41,"qty":2,"options":[{"itemId":"a","name":"Armor A","qty":3},{"itemId":"b","name":"Armor B"},{"itemId":"c","name":"C","qty":"1"},{"itemId":"d","name":"D","qty":null}]}]}"#).unwrap();
        let o = &d.choices[0].options;
        assert_eq!(o.iter().map(|o| o.qty).collect::<Vec<_>>(), vec![Some(3.0), None, Some(1.0), None]);
        assert_eq!(o.iter().map(option_label).collect::<Vec<_>>(), vec!["3× Armor A", "Armor B", "1× C", "D"]);
        assert_eq!(d.choices[0].qty, 2.0, "the choice's own quantity stays for older overlays");
    }

    #[test]
    fn repeated_ids_get_distinct_row_keys() {
        assert_eq!(occurrences(["a", "b", "a", "c", "a"]), vec![0, 0, 1, 0, 2]);
        assert_eq!(occurrences(Vec::<&str>::new()), Vec::<usize>::new());
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(path_segment("favor_01-a.b~"), "favor_01-a.b~");
        assert_eq!(path_segment("a b/c?"), "a%20b%2Fc%3F");
        assert_eq!(path_segment("ç"), "%C3%A7");
        // Dot segments cannot be normalised away; dots inside a name stay as they are.
        assert_eq!(path_segment("."), "%2E");
        assert_eq!(path_segment(".."), "%2E%2E");
        assert_eq!(path_segment("..."), "...");
        assert_eq!(path_segment("a..b"), "a..b");
    }

    #[test]
    fn answers_are_bounded() {
        let long = "x".repeat(MAX_CHARS + 50);
        let accented = "é".repeat(MAX_CHARS + 1);
        let place = serde_json::json!({ "location": long, "qty": 1 });
        let tracked = serde_json::json!({ "itemId": long, "name": accented, "places": vec![place.clone(); 30], "sources": vec![serde_json::json!({ "name": long, "have": 1 }); 30] });
        let missing = serde_json::json!({ "itemId": "x", "name": long, "places": vec![place.clone(); 30], "contracts": vec![long.clone(); 30] });
        let notice = serde_json::json!({ "at": long, "status": long, "changes": vec![serde_json::json!({ "name": long, "delta": 1 }); 30] });
        // One heavy row first, then small ones past the cap.
        let rows = |heavy: serde_json::Value, n: usize| {
            let mut v = vec![serde_json::json!({ "itemId": "small", "name": "small" }); n];
            v[0] = heavy;
            v
        };
        let body = serde_json::json!({
            "channel": long, "atLocation": long, "home": long,
            "locationNames": { long.clone(): long },
            "tracked": rows(tracked, MAX_TRACKED + 10),
            "missing": rows(missing, MAX_MISSING + 10),
            "notices": rows(notice, MAX_NOTICES + 10),
        });
        let d = parse(body.to_string().as_bytes()).unwrap();
        assert_eq!((d.tracked.len(), d.missing.len(), d.notices.len()), (MAX_TRACKED, MAX_MISSING, MAX_NOTICES));
        let t = &d.tracked[0];
        assert_eq!((t.places.len(), t.sources.len()), (MAX_PER_ROW, MAX_PER_ROW));
        assert_eq!(t.name.chars().count(), MAX_CHARS, "cut by characters, not bytes");
        let m = &d.missing[0];
        assert_eq!((m.places.len(), m.contracts.len()), (MAX_PER_ROW, MAX_PER_ROW));
        assert_eq!(d.notices[0].changes.len(), MAX_PER_ROW);
        // An id is never cut (requests need it whole): its row is read-only instead.
        assert_eq!(t.item_id, long, "ids are never cut");
        assert!(!id_usable(&t.item_id));
        let strings = [
            &d.channel,
            d.at_location.as_ref().unwrap(),
            d.home.as_ref().unwrap(),
            &t.places[0].location,
            &t.sources[0].name,
            &m.name,
            &m.places[0].location,
            &m.contracts[0],
            &d.notices[0].at,
            &d.notices[0].status,
            &d.notices[0].changes[0].name,
        ];
        assert!(strings.iter().all(|s| s.chars().count() == MAX_CHARS), "{:?}", strings.iter().map(|s| s.len()).collect::<Vec<_>>());
        // Ids are cut the same way everywhere, so the place still finds its name.
        let (k, v) = d.location_names.iter().next().unwrap();
        assert_eq!((k.chars().count(), v.chars().count()), (MAX_CHARS, MAX_CHARS));
        assert_eq!(location_name(&t.places[0].location, &d.location_names).chars().count(), MAX_CHARS);
        assert!(d.location_names.contains_key(&t.places[0].location));
        // A normal answer is untouched.
        let sample = parse(SAMPLE.as_bytes()).unwrap();
        let mut again = sample.clone();
        bound(&mut again);
        assert_eq!(again, sample);
    }

    #[test]
    fn places_unfold_one_per_line_name_then_quantity() {
        let d = parse(SAMPLE.as_bytes()).unwrap();
        let lines = places(&d.missing[0].places, &d.location_names, d.at_location.as_deref());
        assert_eq!(
            lines,
            vec![
                PlaceText { name: "Orison".into(), qty: "10".into(), here: true, location: "Stanton2_Orison".into(), amount: 10.0 },
                PlaceText { name: "sem local".into(), qty: "2".into(), here: false, location: String::new(), amount: 2.0 },
            ]
        );
        let odd = [Place { location: "Pyro_Ruin".into(), qty: 2.5 }, Place { location: "RR_CRU_LEO".into(), qty: 0.0 }];
        assert_eq!(
            places(&odd, &d.location_names, None),
            vec![PlaceText { name: "Ruin".into(), qty: "2.5".into(), here: false, location: "Pyro_Ruin".into(), amount: 2.5 }],
            "an empty place is no line"
        );
    }

    #[test]
    fn folding_ignores_case_and_accents() {
        assert_eq!(fold_search("Ação Não-Pública ÇÉ"), "acao nao-publica ce");
        assert_eq!(fold_search("ŁÓDŹ Øre Ďáblo"), "lodz ore dablo");
        assert_eq!(fold_search("MG Scrip 2"), "mg scrip 2", "anything else as it is, lowercased");
    }

    fn index_body() -> serde_json::Value {
        serde_json::json!({
            "version": "v7",
            "extra": { "new": true },
            "items": [
                { "itemId": "b", "name": "Carinite (Pure)", "used": false },
                { "itemId": "a", "name": "carinite", "used": true },
                { "itemId": "c", "name": "Wikelo Favor", "used": true, "somethingNew": 1 },
                { "itemId": "", "name": "no id" },
                { "itemId": "d", "name": "Pedra Água", "used": "yes" },
                { "name": "missing id" }
            ],
            "missions": [
                { "missionId": "m2", "title": "Zeta Carinite", "giver": "Wikelo" },
                { "missionId": "m1", "title": "alpha carinite", "giver": null },
                { "missionId": "m3", "title": "Água", "giver": 5 }
            ]
        })
    }

    #[test]
    fn reads_the_index_leniently() {
        let index = parse_index(index_body().to_string().as_bytes()).unwrap();
        assert_eq!(index.version, "v7");
        assert_eq!(index.items.iter().map(|i| i.item_id.as_str()).collect::<Vec<_>>(), vec!["b", "a", "c", "d"], "rows without an id dropped; fields it does not use ignored");
        assert_eq!(index.missions.iter().map(|m| (m.mission_id.as_str(), m.giver.as_deref())).collect::<Vec<_>>(), vec![("m2", Some("Wikelo")), ("m1", None), ("m3", None)]);
        assert_eq!(parse_index(b"{}").unwrap(), OverlayIndex::default());
        assert!(parse_index(b"[]").is_err() && parse_index(b"<html>").is_err());
        let long = "i".repeat(MAX_ID_CHARS + 1);
        let odd = parse_index(serde_json::json!({ "items": [ { "itemId": long, "name": "x" } ], "missions": "x" }).to_string().as_bytes()).unwrap();
        assert!(odd.items.is_empty() && odd.missions.is_empty(), "an id that cannot be sent back is no row");
    }

    #[test]
    fn local_search_matches_every_word_ignoring_case_and_accents() {
        let index = parse_index(index_body().to_string().as_bytes()).unwrap();
        let ids = |q: &str| {
            let r = search_index(&index, q, None);
            (r.items.iter().map(|i| i.item_id.clone()).collect::<Vec<_>>(), r.missions.iter().map(|m| m.mission_id.clone()).collect::<Vec<_>>())
        };
        assert_eq!(ids("CARINITE"), (vec!["a".to_string(), "b".to_string()], vec!["m1".to_string(), "m2".to_string()]), "by name; missions by title");
        assert_eq!(ids("pure  carin"), (vec!["b".to_string()], vec![]), "every word, in any order");
        assert_eq!(ids("agua"), (vec!["d".to_string()], vec!["m3".to_string()]), "accents ignored in the names");
        assert_eq!(ids("ÁGUA"), (vec!["d".to_string()], vec!["m3".to_string()]), "and in the query");
        assert_eq!(ids("   "), (vec![], vec![]), "no word: nothing");
        assert_eq!(ids("zzz"), (vec![], vec![]));
        let r = search_index(&index, "carinite", None);
        assert_eq!((r.item_count, r.mission_count), (2, 2), "the counts of the chips");
    }

    fn held(list: &[(&str, f64)]) -> HashMap<String, Owned> {
        list.iter().map(|&(id, qty)| (id.to_string(), Owned { qty, stacks: vec![Stack { location: "Stanton2_Orison".into(), quality: 0, qty }] })).collect()
    }

    #[test]
    fn the_items_owned_come_first_then_the_rest_by_name_zeros_included() {
        let index = parse_index(index_body().to_string().as_bytes()).unwrap();
        let ids = |held: Option<&HashMap<String, Owned>>| search_index(&index, "carinite", held).items.into_iter().map(|i| i.item_id).collect::<Vec<_>>();
        assert_eq!(ids(None), vec!["a", "b"], "no inventory yet: by name");
        let inv = held(&[("b", 2.0)]);
        assert_eq!(ids(Some(&inv)), vec!["b", "a"], "the one owned first; the other stays, listed after it");
        assert_eq!(ids(Some(&held(&[("b", 0.0)]))), vec!["a", "b"], "nothing of it: not owned");
        assert_eq!(ids(Some(&held(&[("a", 1.0), ("b", 9.0)]))), vec!["a", "b"], "both owned: by name");
        // An older server's answer is ordered the same way.
        let answer = SearchResults {
            items: vec![SearchItem { item_id: "z".into(), name: "Zeta".into() }, SearchItem { item_id: "y".into(), name: "alpha".into() }, SearchItem { item_id: "x".into(), name: "Beta".into() }],
            missions: vec![SearchMission { mission_id: "m".into(), title: "T".into(), giver: None }],
        };
        let f = found_from_answer(&answer, Some(&held(&[("z", 1.0)])));
        assert_eq!(f.items.iter().map(|i| i.item_id.as_str()).collect::<Vec<_>>(), vec!["z", "y", "x"]);
        assert_eq!((f.item_count, f.mission_count), (3, 1));
    }

    #[test]
    fn the_search_keeps_fifty_items_and_forty_missions_and_counts_them_all() {
        const { assert!(MAX_RESULTS == 40 && ITEM_ROWS_MAX == 50) };
        let items: Vec<serde_json::Value> = (0..90).map(|n| serde_json::json!({ "itemId": format!("i{n:02}"), "name": format!("Scrap {n:02}") })).collect();
        let missions: Vec<serde_json::Value> = (0..90).map(|n| serde_json::json!({ "missionId": format!("m{n:02}"), "title": format!("Scrap run {:02}", 89 - n) })).collect();
        let index = parse_index(serde_json::json!({ "items": items, "missions": missions }).to_string().as_bytes()).unwrap();
        // Owned: every third one.
        let inv: HashMap<String, Owned> = (0..90).filter(|n| n % 3 == 0).map(|n| (format!("i{n:02}"), Owned { qty: 1.0, stacks: vec![] })).collect();
        let r = search_index(&index, "scrap", Some(&inv));
        assert_eq!((r.items.len(), r.item_count, r.missions.len(), r.mission_count), (ITEM_ROWS_MAX, 90, MAX_RESULTS, 90));
        let owned: Vec<String> = r.items.iter().take(30).map(|i| i.item_id.clone()).collect();
        assert_eq!(owned, (0..30).map(|n| format!("i{:02}", 3 * n)).collect::<Vec<_>>(), "the thirty owned first");
        assert_eq!((r.items[30].item_id.as_str(), r.items[31].item_id.as_str(), r.items[49].item_id.as_str()), ("i01", "i02", "i29"), "then the rest by name");
        assert_eq!(r.missions[0].title, "Scrap run 00");
        assert_eq!(r.missions[39].title, "Scrap run 39");
    }

    #[test]
    fn the_quantity_comes_from_the_inventory_and_is_zero_when_it_has_none() {
        let inv = held(&[("a", 3.0)]);
        assert_eq!(held_qty(Some(&inv), "a"), Some(3.0));
        assert_eq!(held_qty(Some(&inv), "b"), Some(0.0), "absent from the answer: 0, shown");
        assert_eq!(held_qty(None, "a"), None, "no answer yet: no number rather than a wrong 0");
    }

    #[test]
    fn item_and_mission_rows_take_pins_from_the_data_and_changes_on_top() {
        let index = parse_index(index_body().to_string().as_bytes()).unwrap();
        let inv = held(&[("a", 3.0)]);
        let found = search_index(&index, "carinite", Some(&inv));
        let pinned_items: HashSet<String> = ["a".to_string()].into();
        let pinned_missions: HashSet<String> = ["m2".to_string()].into();
        let none: [Local; 0] = [];
        let rows = item_rows(&found.items, Some(&inv), &pinned_items, &none, &none);
        assert_eq!(rows.iter().map(|r| (r.item_id.as_str(), r.pinned, r.qty)).collect::<Vec<_>>(), vec![("a", true, Some(3.0)), ("b", false, Some(0.0))], "owned first; the other with its 0");
        assert_eq!(rows[0].places, vec![Place { location: "Stanton2_Orison".into(), qty: 3.0 }], "where it is kept");
        assert!(rows[1].places.is_empty());
        let unknown = item_rows(&found.items, None, &pinned_items, &none, &none);
        assert!(unknown.iter().all(|r| r.qty.is_none() && r.places.is_empty()), "before the inventory answers: no numbers");
        let m = mission_rows(&found.missions, &pinned_missions, &none);
        assert_eq!(m.iter().map(|m| (m.mission_id.as_str(), m.pinned)).collect::<Vec<_>>(), vec![("m1", false), ("m2", true)]);
        // Every pin on its way counts; only the - / + the inventory answer does not have yet.
        let changes = [
            Local::UnpinItem { item_id: "a".into() },
            Local::PinItem { item_id: "b".into(), name: "Carinite (Pure)".into(), have: 0.0 },
            Local::PinMission { mission_id: "m1".into(), title: "alpha carinite".into(), giver: None },
            Local::Adjust { item_id: "a".into(), delta: 2 },
            Local::Adjust { item_id: "b".into(), delta: 5 },
        ];
        let rows = item_rows(&found.items, Some(&inv), &pinned_items, &changes, &changes[4..]);
        assert_eq!(rows.iter().map(|r| (r.item_id.as_str(), r.pinned, r.qty)).collect::<Vec<_>>(), vec![("a", false, Some(3.0)), ("b", true, Some(5.0))], "the answer has the +2 already; a 0 takes the +5");
        assert!(mission_rows(&found.missions, &pinned_missions, &changes).iter().all(|m| m.pinned));
        let rows = item_rows(&found.items, None, &pinned_items, &none, &changes[3..]);
        assert!(rows.iter().all(|r| r.qty.is_none()), "nothing to add to without an answer");
        let down = [Local::Adjust { item_id: "a".into(), delta: -9 }];
        assert_eq!(item_rows(&found.items, Some(&inv), &pinned_items, &none, &down)[0].qty, Some(0.0), "never below zero");
    }

    fn inventory_body() -> serde_json::Value {
        serde_json::json!([
            { "itemId": "a", "qty": 7, "updatedAt": null, "places": [
                { "location": "Stanton2_Orison", "qty": 3, "quality": 0 },
                { "location": "Stanton2_Orison", "qty": 2, "quality": 500 },
                { "location": "", "qty": 1, "quality": 0 },
                { "location": "RR_CRU_LEO", "qty": 0, "quality": 0 }
            ], "somethingNew": true },
            { "itemId": "b", "qty": "4" },
            { "itemId": "a", "qty": 1, "places": [ { "location": "RR_CRU_LEO", "qty": 1 } ] },
            { "itemId": "c", "qty": 0, "places": [] },
            { "itemId": "", "qty": 5 },
            { "qty": 5 },
            "not an entry",
            { "itemId": "d", "qty": 2, "places": "odd" }
        ])
    }

    #[test]
    fn the_inventory_sums_stacks_per_item_and_keeps_one_place_per_location() {
        let owned = parse_inventory(inventory_body().to_string().as_bytes()).unwrap();
        let mut ids: Vec<&str> = owned.keys().map(String::as_str).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "d"], "nothing held, no id or not an entry: left out");
        let a = &owned["a"];
        assert_eq!(a.qty, 7.0, "the stacks summed, the item listed twice added up");
        assert_eq!(
            a.places(),
            vec![Place { location: "Stanton2_Orison".into(), qty: 5.0 }, Place { location: "".into(), qty: 1.0 }, Place { location: "RR_CRU_LEO".into(), qty: 1.0 }],
            "one line per location, qualities summed, empty stacks dropped"
        );
        assert_eq!(
            a.stacks,
            vec![
                Stack { location: "Stanton2_Orison".into(), quality: 0, qty: 3.0 },
                Stack { location: "Stanton2_Orison".into(), quality: 500, qty: 2.0 },
                Stack { location: "".into(), quality: 0, qty: 1.0 },
                Stack { location: "RR_CRU_LEO".into(), quality: 0, qty: 1.0 },
            ],
            "each stack kept with its quality (a missing one is 0), for the moves"
        );
        assert_eq!(owned["b"], Owned { qty: 4.0, stacks: vec![] }, "no stacks: its own quantity");
        assert_eq!(owned["d"].qty, 2.0, "odd stacks read as none");
        let lines = places(&a.places(), &HashMap::from([("Stanton2_Orison".to_string(), "Orison".to_string())]), Some("RR_CRU_LEO"));
        assert_eq!(lines.iter().map(|l| (l.name.as_str(), l.qty.as_str(), l.here)).collect::<Vec<_>>(), vec![("Orison", "5", false), ("sem local", "1", false), ("RR CRU LEO", "1", true)]);
        assert!(parse_inventory(b"[]").unwrap().is_empty());
        assert!(parse_inventory(b"{}").is_err() && parse_inventory(b"<html>").is_err(), "not a list");
    }

    #[test]
    fn a_settled_change_is_baked_into_the_inventory_answer() {
        let owned = parse_inventory(inventory_body().to_string().as_bytes()).unwrap();
        let mut baked = owned.clone();
        bake_owned(&mut baked, &Local::Adjust { item_id: "b".into(), delta: -9 });
        bake_owned(&mut baked, &Local::Adjust { item_id: "a".into(), delta: 2 });
        bake_owned(&mut baked, &Local::Adjust { item_id: "new".into(), delta: 2 });
        bake_owned(&mut baked, &Local::UnpinItem { item_id: "a".into() });
        assert_eq!((baked["a"].qty, baked["b"].qty, baked["new"].qty), (9.0, 0.0, 2.0), "never below zero; an item at 0 starts from 0");
        assert_eq!(baked["a"].stacks, owned["a"].stacks, "where it went is the next answer's");
        assert_eq!(held_qty(Some(&baked), "b"), Some(0.0));
    }

    fn stack(location: &str, quality: i64, qty: f64) -> Stack {
        Stack { location: location.into(), quality, qty }
    }

    fn place(location: &str, qty: f64) -> Place {
        Place { location: location.into(), qty }
    }

    fn mv(item: &str, from: &str, to: &str, parts: &[(i64, i64)]) -> Local {
        Local::Move { item_id: item.into(), from: from.into(), to: to.into(), parts: parts.to_vec() }
    }

    #[test]
    fn a_move_takes_from_one_place_and_adds_to_the_other() {
        let mut p = vec![place("A", 5.0), place("", 2.0)];
        move_places(&mut p, "A", "B", 3.0);
        assert_eq!(p, vec![place("A", 2.0), place("", 2.0), place("B", 3.0)], "a new place goes at the end");
        move_places(&mut p, "A", "B", 9.0);
        assert_eq!(p, vec![place("", 2.0), place("B", 5.0)], "never more than the place holds; an emptied place goes");
        move_places(&mut p, "", "B", 2.0);
        assert_eq!(p, vec![place("B", 7.0)], "what had no place now has one");
        let before = p.clone();
        move_places(&mut p, "Z", "B", 1.0);
        move_places(&mut p, "B", "B", 1.0);
        move_places(&mut p, "B", "C", 0.0);
        move_places(&mut p, "B", "C", f64::NAN);
        assert_eq!(p, before, "from nowhere, to itself, nothing, not a number: no change");
    }

    #[test]
    fn a_move_goes_stack_by_stack_lowest_quality_first() {
        let stacks = vec![stack("A", 500, 2.0), stack("A", 0, 3.0), stack("B", 0, 1.0)];
        assert_eq!(split_move(&stacks, "A", 4), vec![(0, 3), (500, 1)], "quality 0 first, then the rest");
        assert_eq!(split_move(&stacks, "A", 5), vec![(0, 3), (500, 2)], "all of the line");
        assert_eq!(split_move(&stacks, "A", 9), vec![(0, 3), (500, 2)], "never more than the place holds");
        assert_eq!(split_move(&stacks, "B", 1), vec![(0, 1)]);
        assert_eq!(split_move(&stacks, "Z", 2), vec![(0, 2)], "no stack known there: one part, the server moves what it has");
        assert!(split_move(&stacks, "A", 0).is_empty());
        assert_eq!(split_move(&[stack("A", 0, 2.5)], "A", 3), vec![(0, 2)], "whole units only");
        // Applied to the stacks: each quality to the same quality elsewhere.
        let mut s = stacks.clone();
        move_stacks(&mut s, "A", "B", &[(0, 3), (500, 1)]);
        assert_eq!(s, vec![stack("A", 500, 1.0), stack("B", 0, 4.0), stack("B", 500, 1.0)]);
        move_stacks(&mut s, "A", "B", &[(500, 7), (42, 1)]);
        assert_eq!(s, vec![stack("B", 0, 4.0), stack("B", 500, 2.0)], "clamped to the stack; a quality not there moves nothing");
        let mut inv = HashMap::from([("x".to_string(), Owned { qty: 6.0, stacks: stacks.clone() })]);
        bake_owned(&mut inv, &mv("x", "A", "C", &[(0, 3)]));
        assert_eq!((inv["x"].qty, inv["x"].places()), (6.0, vec![place("A", 2.0), place("B", 1.0), place("C", 3.0)]), "baked: the total stays");
        bake_owned(&mut inv, &mv("nothing", "A", "C", &[(0, 3)]));
        assert!(!inv.contains_key("nothing"));
    }

    #[test]
    fn a_move_shows_at_once_in_the_cards_and_in_the_search() {
        let server = with_everything();
        let changes = [mv("x", "Stanton2_Orison", "RR_CRU_LEO", &[(0, 4)]), mv("favor", "Stanton2_Orison", "", &[(0, 1)])];
        let v = view(&server, &changes);
        assert_eq!(v.missing[0].places, vec![place("Stanton2_Orison", 6.0), place("", 2.0), place("RR_CRU_LEO", 4.0)]);
        assert_eq!((v.missing[0].have, v.missing[0].missing), (12.0, 28.0), "the totals stay");
        assert_eq!(v.tracked[0].places, vec![place("Stanton2_Orison", 2.0), place("", 1.0)]);
        assert_eq!(here_rows(&v).iter().map(|r| (r.item_id.as_str(), r.qty)).collect::<Vec<_>>(), vec![("favor", 2.0), ("x", 6.0)], "\"Aqui\" follows");
        assert_eq!(changes[0].moved(), 4.0);
        // The search's item rows: the moves the inventory answer does not have, on top.
        let inv = held(&[("x", 5.0)]);
        let items = [SearchItem { item_id: "x".into(), name: "Carinite".into() }];
        let none: [Local; 0] = [];
        let rows = item_rows(&items, Some(&inv), &HashSet::new(), &none, &changes[..1]);
        assert_eq!((rows[0].qty, rows[0].places.clone()), (Some(5.0), vec![place("Stanton2_Orison", 1.0), place("RR_CRU_LEO", 4.0)]));
        // A failed write drops the change: the server's places come back as they were.
        assert_eq!(view(&server, &changes[1..]).missing[0].places, server.missing[0].places);
    }

    #[test]
    fn a_move_is_sent_only_whole_and_between_two_places() {
        assert!(change_usable(&mv("x", "A", "B", &[(0, 3)])));
        assert!(change_usable(&mv("x", "", "B", &[(0, 1), (500, 2)])), "setting the place of what has none");
        assert!(change_usable(&mv("x", "A", "", &[(0, 1)])));
        assert!(!change_usable(&mv("x", "A", "A", &[(0, 1)])), "the same place");
        assert!(!change_usable(&mv("x", "A", "B", &[])), "nothing to move");
        assert!(!change_usable(&mv("x", "A", "B", &[(0, 0)])));
        assert!(!change_usable(&mv("x", "A", "B", &[(-1, 1)])));
        assert!(!change_usable(&mv("x", "A", "B", &[(0, MAX_MOVE + 1)])));
        assert!(!change_usable(&mv("", "A", "B", &[(0, 1)])));
        assert!(!change_usable(&mv("x", &"l".repeat(MAX_ID_CHARS + 1), "B", &[(0, 1)])), "a place id is never cut");
        assert_eq!(Local::Adjust { item_id: "x".into(), delta: 3 }.moved(), 0.0);
    }

    #[test]
    fn the_picker_offers_here_and_home_first_then_where_it_is_then_what_the_filter_finds() {
        let body = serde_json::json!({
            "contracts": [ { "id": "c" } ],
            "locations": [
                { "id": "Stanton2_Orison", "name": "Orison", "extra": 1 },
                { "id": "RR_CRU_LEO", "name": "Seraphim Station" },
                { "id": "Stanton1_Lorville", "name": "" },
                { "id": "", "name": "no id" },
                { "id": "RR_CRU_LEO", "name": "twice" },
                { "id": "Pyro_Ruin", "name": "Ruin Station" },
                { "name": "missing id" }
            ]
        });
        let locs = parse_locations(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            locs.iter().map(|l| (l.id.as_str(), l.name.as_str())).collect::<Vec<_>>(),
            vec![("Stanton1_Lorville", "Lorville"), ("Stanton2_Orison", "Orison"), ("Pyro_Ruin", "Ruin Station"), ("RR_CRU_LEO", "Seraphim Station")],
            "by name; each id once; no name: the readable id"
        );
        assert!(parse_locations(b"[]").is_err() && parse_locations(b"<html>").is_err());
        assert!(parse_locations(b"{}").unwrap().is_empty() && parse_locations(br#"{"locations":"x"}"#).unwrap().is_empty());
        let names = HashMap::from([("Stanton2_Orison".to_string(), "Orison".to_string())]);
        let holding = [place("RR_CRU_LEO", 2.0), place("Stanton2_Orison", 1.0), place("", 1.0)];
        let kept = |here: Option<&str>, home: Option<&str>| Kept { here: here.map(str::to_string), home: home.map(str::to_string), in_use: HashSet::new() };
        let ids = |from: &str, here: Option<&str>, home: Option<&str>, filter: &str| {
            move_targets(from, &kept(here, home), &holding, &names, &locs, filter).into_iter().map(|t| (t.id, t.kind)).collect::<Vec<_>>()
        };
        use TargetKind::{Found, Here, Holding, Home};
        assert_eq!(
            ids("", Some("Stanton2_Orison"), Some("Stanton1_Lorville"), ""),
            vec![("Stanton2_Orison".to_string(), Here), ("Stanton1_Lorville".to_string(), Home), ("RR_CRU_LEO".to_string(), Holding)],
            "here and home first, then where it is; each once; no place is not a target"
        );
        assert_eq!(ids("RR_CRU_LEO", Some("RR_CRU_LEO"), None, ""), vec![("Stanton2_Orison".to_string(), Holding)], "never where it comes from");
        assert_eq!(ids("RR_CRU_LEO", None, None, "ruin"), vec![("Stanton2_Orison".to_string(), Holding), ("Pyro_Ruin".to_string(), Found)], "the filter adds the channel's places");
        assert_eq!(ids("RR_CRU_LEO", None, None, "STATION"), vec![("Stanton2_Orison".to_string(), Holding), ("Pyro_Ruin".to_string(), Found)], "any case; the place it comes from is not offered");
        assert_eq!(ids("x", None, None, "zzz").len(), 2, "nothing found: only where it is");
        let named = move_targets("", &kept(None, Some("Pyro_Ruin")), &[], &names, &locs, "");
        assert_eq!(named[0].name, "Ruin Station", "named from the channel's list when the data has no name");
        assert_eq!(move_targets("", &kept(Some("Unknown_Place"), None), &[], &names, &locs, "")[0].name, "Unknown Place", "unknown: the readable id");
        // Many matches: at most a dozen.
        let many: Vec<serde_json::Value> = (0..40).map(|n| serde_json::json!({ "id": format!("L{n:02}"), "name": format!("Outpost {n:02}") })).collect();
        let locs = parse_locations(serde_json::json!({ "locations": many }).to_string().as_bytes()).unwrap();
        assert_eq!(move_targets("", &Kept::default(), &[], &names, &locs, "outpost").len(), MOVE_MATCHES_MAX);
    }

    /// A channel's list with one place of each kind the site's rule tells apart.
    fn rule_locations() -> Vec<Location> {
        let body = serde_json::json!({
            "locations": [
                { "id": "Stanton2_Orison", "name": "Orison", "system": "Stanton", "body": "Crusader" },
                { "id": "Stanton2_Orison_Interior", "name": "Orison Hab", "system": "Stanton", "body": "Crusader", "hidden": true },
                { "id": "Stanton1_Lorville_Gate", "name": "Lorville Gate", "system": "Stanton", "body": "Hurston", "hidden": true },
                { "id": "Stanton3_Area18_Hab", "name": "Area18 Hab", "system": "Stanton", "body": "ArcCorp", "hidden": true },
                { "id": "Pyro_Mission_Site", "name": "  <= PLACEHOLDER =>  ", "system": "Pyro" },
                { "id": "Pyro5a_Ruin_Camp", "name": "<= UNINITIALIZED =>", "system": "Pyro", "hidden": true },
                { "id": "Stanton4_Old", "name": "Old Place", "hidden": null }
            ]
        });
        parse_locations(body.to_string().as_bytes()).unwrap()
    }

    fn rule_location<'a>(list: &'a [Location], id: &str) -> &'a Location {
        list.iter().find(|l| l.id == id).unwrap()
    }

    #[test]
    fn the_site_rule_offers_what_the_star_map_shows_with_a_real_name() {
        let list = rule_locations();
        let nothing = Kept::default();
        assert!(offered(rule_location(&list, "Stanton2_Orison"), &nothing), "ordinary");
        assert!(offered(rule_location(&list, "Stanton4_Old"), &nothing), "hidden null reads as not hidden");
        assert!(!offered(rule_location(&list, "Stanton2_Orison_Interior"), &nothing), "hidden");
        assert!(!offered(rule_location(&list, "Pyro_Mission_Site"), &nothing), "placeholder name, even with spaces around it");
        assert!(!offered(rule_location(&list, "Pyro5a_Ruin_Camp"), &nothing), "hidden and unnamed");
        assert!(is_placeholder_name("<= PLACEHOLDER =>") && is_placeholder_name("<==>") && !is_placeholder_name("<=>") && !is_placeholder_name("Orison"));
        assert!(!is_placeholder_name("<= a\nb =>"), "one line only, like the site's pattern");
    }

    #[test]
    fn hidden_or_unnamed_places_stay_offered_where_the_player_is_keeps_home_or_has_something() {
        let list = rule_locations();
        let d = OverlayData {
            at_location: Some("Stanton2_Orison_Interior".into()),
            home: Some("Stanton1_Lorville_Gate".into()),
            missing: vec![MissingItem { item_id: "x".into(), places: vec![place("Pyro_Mission_Site", 2.0), place("", 1.0)], ..Default::default() }],
            ..Default::default()
        };
        let held = HashMap::from([("y".to_string(), Owned { qty: 1.0, stacks: vec![stack("Stanton3_Area18_Hab", 0, 1.0)] })]);
        let moving = [Local::Move { item_id: "y".into(), from: "Stanton3_Area18_Hab".into(), to: "Pyro5a_Ruin_Camp".into(), parts: vec![(0, 1)] }];
        let kept = kept_places(Some(&d), None, [] as [&Local; 0]);
        assert!(offered(rule_location(&list, "Stanton2_Orison_Interior"), &kept), "hidden but current");
        assert!(offered(rule_location(&list, "Stanton1_Lorville_Gate"), &kept), "hidden home");
        assert!(offered(rule_location(&list, "Pyro_Mission_Site"), &kept), "unnamed but owned (the overlay's data)");
        assert!(!offered(rule_location(&list, "Stanton3_Area18_Hab"), &kept), "hidden, nothing there yet");
        assert!(!kept.in_use.contains(""), "no place is never kept");
        let kept = kept_places(None, Some(&held), &moving);
        assert!(offered(rule_location(&list, "Stanton3_Area18_Hab"), &kept), "hidden but owned (the inventory)");
        assert!(offered(rule_location(&list, "Pyro5a_Ruin_Camp"), &kept), "where a move on its way goes");
        assert!(!offered(rule_location(&list, "Stanton2_Orison_Interior"), &kept));
        // In the picker: the filter finds a hidden place only when it is kept, named readably.
        let names = HashMap::new();
        let found = |kept: &Kept, filter: &str| move_targets("", kept, &[], &names, &list, filter).into_iter().filter(|t| t.kind == TargetKind::Found).map(|t| t.name).collect::<Vec<_>>();
        assert_eq!(found(&Kept::default(), "mission"), Vec::<String>::new(), "a placeholder is not found");
        assert_eq!(found(&Kept::default(), "hab"), Vec::<String>::new(), "hidden places are not found");
        assert_eq!(found(&Kept::default(), "orison"), vec!["Orison".to_string()]);
        let owned = Kept { in_use: HashSet::from(["Pyro_Mission_Site".to_string(), "Stanton3_Area18_Hab".to_string()]), ..Kept::default() };
        assert_eq!(found(&owned, "mission"), vec!["Mission Site".to_string()], "owned: found under its readable id");
        assert_eq!(found(&owned, "hab"), vec!["Area18 Hab".to_string()]);
    }

    #[test]
    fn places_are_named_like_the_site_names_them() {
        assert_eq!(readable_location_id("Stanton2_Orison"), "Orison");
        assert_eq!(readable_location_id("pyro5a_RuinCamp"), "Ruin Camp", "any case; digits and one letter");
        assert_eq!(readable_location_id("RR_CRU_LEO"), "RR CRU LEO");
        assert_eq!(readable_location_id("Stanton_"), "Stanton_", "nothing left: the id");
        assert_eq!(readable_location_id("Stantonab_X"), "Stantonab X", "two letters are not a system's suffix");
        let names = HashMap::from([("a".to_string(), "<= PLACEHOLDER =>".to_string()), ("Stanton2_Orison".to_string(), "Orison".to_string())]);
        assert_eq!(location_name("a", &names), "a");
        assert_eq!(location_name("Stanton1_GrimHEX", &names), "Grim HEX", "unknown: the readable id");
        assert_eq!(location_name("Stanton2_Orison", &names), "Orison");
        // Same name on the same body: told apart by their ids, a hidden one always.
        let body = serde_json::json!({
            "locations": [
                { "id": "Pyro1_derelict_occ_003", "name": "Derelict Outpost", "system": "Pyro", "body": "Pyro I" },
                { "id": "Pyro1_derelict_occ_004", "name": "Derelict Outpost", "system": "Pyro", "body": "Pyro I" },
                { "id": "Pyro2_derelict_occ_005", "name": "Derelict Outpost", "system": "Pyro", "body": "Monox" },
                { "id": "Stanton2_Orison", "name": "Orison", "system": "Stanton", "body": "Crusader" },
                { "id": "Stanton2_Orison_Hab", "name": "Orison", "system": "Stanton", "body": "Crusader", "hidden": true },
                { "id": "Pyro_Mission_A", "name": "<= PLACEHOLDER =>", "system": "Pyro" }
            ]
        });
        let list = parse_locations(body.to_string().as_bytes()).unwrap();
        let name = |id: &str| rule_location(&list, id).name.clone();
        assert_eq!((name("Pyro1_derelict_occ_003"), name("Pyro1_derelict_occ_004")), ("Derelict Outpost (003)".to_string(), "Derelict Outpost (004)".to_string()));
        assert_eq!(name("Pyro2_derelict_occ_005"), "Derelict Outpost", "alone on its body");
        assert_eq!((name("Stanton2_Orison"), name("Stanton2_Orison_Hab")), ("Orison".to_string(), "Orison (Orison Hab)".to_string()), "the visible one stays clean");
        assert_eq!(name("Pyro_Mission_A"), "Mission A");
    }
}

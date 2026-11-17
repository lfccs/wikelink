//! The overlay's requests as plain data: which one goes out next, and what an answer does to
//! the changes the player made. No window and no network here: everything is testable with
//! made-up times.
//!
//! One request at a time, chosen by [`Engine::next_job_all`]: a change first (the clicks on
//! one item's `-` / `+` summed into one write once they stop for [`COALESCE`]), then the read
//! that settles the changes made, the search index, the player's inventory (the quantities and
//! places of the search's item rows, the stacks a move between places goes by), the channel's
//! locations (the move picker's filter), a search (older servers only), and the regular read
//! every [`POLL_EVERY`] (none while changes wait to go out). A write may take several requests
//! (a move, one per quality): see [`classify_part`]. Every request counts against one
//! budget per minute ([`BUDGET_ALL`], reads and searches only up to [`BUDGET_READS`], so
//! writes always have room), and a 429 stops everything for the wait it was told to take;
//! the index and the inventory have budgets and 429 waits of their own ([`BUDGET_INDEX`],
//! [`BUDGET_INVENTORY`]).
//!
//! A change shows at once and stays drawn until a read sent after its write came back (then
//! the server's answer has it). A write the server refused (an HTTP error) is dropped, so the
//! number goes back; a write without an answer (time-out, lost connection) may or may not have
//! been applied: its row is marked as not confirmed until the next read, which is taken as it
//! is (never retried, never undone by arithmetic). A 429 keeps the change waiting.
//!
//! The answers belong to one generation of the endpoint (server, token and channel, see
//! [`Generations`]): a write made for one is never sent to another.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::agent;
use crate::overlay_model::Local;
use crate::trf;

/// Reads while the overlay is shown and the server answers.
pub const POLL_EVERY: Duration = Duration::from_secs(5);
/// Longest wait between two reads (signed out, an old server, a long outage), and the
/// longest wait a 429 can impose.
pub const MAX_WAIT: Duration = Duration::from_secs(300);
/// The read that settles the changes comes no sooner than this after the previous read.
pub const READ_AFTER_WRITE_GAP: Duration = Duration::from_secs(2);
/// Clicks on one item's `-` / `+` are summed until they stop for this long, then sent as one.
pub const COALESCE: Duration = Duration::from_millis(400);
/// Most changes on their way at once (a sum of clicks is one); the controls are disabled
/// while this many wait.
pub const MAX_OPS: usize = 8;
/// The budget: at most [`BUDGET_ALL`] requests within [`BUDGET_WINDOW`], reads and searches
/// only up to [`BUDGET_READS`] of them.
pub const BUDGET_WINDOW: Duration = Duration::from_secs(60);
pub const BUDGET_ALL: usize = 28;
pub const BUDGET_READS: usize = 22;
/// The search index has a budget of its own. Typing never spends it (the search is local).
pub const BUDGET_INDEX: usize = 11;
/// The whole inventory (`GET /api/inventory`, the route the site uses): read when the
/// overlay opens, every 30 s while it is open and after a write settles; this caps it.
pub const BUDGET_INVENTORY: usize = 8;
/// A 429 on the index or the inventory without a `Retry-After`: that kind waits this long.
const AUX_LIMIT_WAIT: Duration = Duration::from_secs(10);
/// Largest sum of clicks one write carries, either way.
pub const MAX_DELTA: i64 = 100_000;
/// Shortest wait asked of the window (never a busy loop).
const MIN_WAKE: Duration = Duration::from_millis(30);

/// How a read went, for the schedule of the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Good,
    SignedOut,
    NoRoute,
    Unreachable,
}

/// How long to wait before the next read after `outcome`; `failures` counts the reads in a
/// row that found the server unreachable, this one included. A good answer: [`POLL_EVERY`].
/// Signed out or an old server: [`MAX_WAIT`] (watcher.json changing ends the wait sooner).
/// Unreachable: [`POLL_EVERY`], doubling with each failure in a row, up to [`MAX_WAIT`].
pub fn next_delay(outcome: Outcome, failures: u32) -> Duration {
    match outcome {
        Outcome::Good => POLL_EVERY,
        Outcome::SignedOut | Outcome::NoRoute => MAX_WAIT,
        Outcome::Unreachable => {
            let doublings = failures.saturating_sub(1).min(16);
            POLL_EVERY.saturating_mul(1u32 << doublings).min(MAX_WAIT)
        }
    }
}

// ----- generations ----------------------------------------------------------------------

/// Where the requests go: the server, the token (empty for the local app) and the channel.
pub type Ident = (String, String, String);

/// Numbers the endpoints in the order they are seen: the same number while the server, the
/// token and the channel stay, one more whenever any of them changes (back to an earlier one
/// included). 0 = none seen yet.
#[derive(Debug, Default)]
pub struct Generations {
    ident: Option<Ident>,
    generation: u64,
}

impl Generations {
    /// The generation of `ident`, the endpoint of the request about to go out.
    pub fn observe(&mut self, ident: Ident) -> u64 {
        if self.ident.as_ref() != Some(&ident) {
            self.generation += 1;
            self.ident = Some(ident);
        }
        self.generation
    }
}

/// A write made while the overlay showed the data of generation `made_for` goes out only to
/// that same endpoint (`now`).
pub fn accepts(made_for: u64, now: u64) -> bool {
    made_for != 0 && made_for == now
}

// ----- answers --------------------------------------------------------------------------

/// A 429: try later, after the server's wait when it said one (`Retry-After`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit(pub Option<Duration>);

/// What became of one write.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteResult {
    Applied,
    /// 404 on an unpin: maybe gone already, maybe a server without the route; the next
    /// read decides.
    Gone,
    /// The server refused it (an HTTP error status), and why: undone.
    Failed(String),
    /// No answer (time-out, lost connection, an unreadable answer): it may or may not have
    /// been applied; the next read says.
    Unknown(String),
    /// 429: not applied; tried again later.
    Limited(Option<Duration>),
    /// Made for another server, account or channel: never sent.
    Stale,
}

/// Read a write's answer: the status, body and `Retry-After`, or the transport error.
/// `unpin`: a 404 is [`WriteResult::Gone`]; `what` names the change in the messages.
pub fn classify_write(answer: Result<(u16, &[u8], Option<Duration>), &str>, unpin: bool, what: &str) -> WriteResult {
    match answer {
        Err(e) => WriteResult::Unknown(trf!("no answer when trying to {what} ({e}); checking with the server", "sem resposta para {what} ({e}); conferindo com o servidor")),
        Ok((status, _, _)) if status < 400 => WriteResult::Applied,
        Ok((404, _, _)) if unpin => WriteResult::Gone,
        Ok((429, _, retry)) => WriteResult::Limited(retry),
        Ok((status, body, _)) => {
            let why = agent::server_error(status, body);
            WriteResult::Failed(trf!("could not {what}: {why}", "não deu para {what}: {why}"))
        }
    }
}

/// One part of a write made of several requests (a move goes one quality at a time), sent
/// after `done_before` parts were applied: `None` to go on with the next part, else what the
/// whole write came to. Before any part was applied a part's result is the write's (a refusal
/// undoes it, a 429 keeps it waiting, as for any write); once some were, the rest not going
/// through leaves it applied in part: unknown, the next read says what the server has (never
/// retried, never undone by arithmetic).
pub fn classify_part(done_before: bool, result: WriteResult, what: &str) -> Option<WriteResult> {
    match result {
        WriteResult::Applied | WriteResult::Gone => None,
        other if !done_before => Some(other),
        WriteResult::Unknown(msg) => Some(WriteResult::Unknown(msg)),
        _ => Some(WriteResult::Unknown(trf!("could not {what} completely; checking with the server", "não deu para {what} por inteiro; conferindo com o servidor"))),
    }
}

// ----- budget ---------------------------------------------------------------------------

/// The requests sent within the last [`BUDGET_WINDOW`].
#[derive(Debug, Default)]
pub struct Budget {
    sent: VecDeque<Instant>,
}

impl Budget {
    fn cap(write: bool) -> usize {
        if write { BUDGET_ALL } else { BUDGET_READS }
    }

    fn recent(&self, now: Instant) -> impl Iterator<Item = &Instant> {
        self.sent.iter().filter(move |t| now.saturating_duration_since(**t) < BUDGET_WINDOW)
    }

    /// A write (`write`) or a read / search fits now.
    pub fn allows(&self, now: Instant, write: bool) -> bool {
        self.recent(now).count() < Self::cap(write)
    }

    fn allows_cap(&self, now: Instant, cap: usize) -> bool {
        self.recent(now).count() < cap
    }

    pub fn record(&mut self, now: Instant) {
        while self.sent.front().is_some_and(|t| now.saturating_duration_since(*t) >= BUDGET_WINDOW) {
            self.sent.pop_front();
        }
        self.sent.push_back(now);
    }

    /// When a request of that kind fits again (`now` when it fits already).
    pub fn free_at(&self, now: Instant, write: bool) -> Instant {
        self.free_at_cap(now, Self::cap(write))
    }

    /// When one more request fits under `cap` (`now` when it fits already).
    fn free_at_cap(&self, now: Instant, cap: usize) -> Instant {
        let recent: Vec<Instant> = self.recent(now).copied().collect();
        if cap == 0 {
            return now + BUDGET_WINDOW;
        }
        if recent.len() < cap {
            return now;
        }
        recent[recent.len() - cap] + BUDGET_WINDOW
    }
}

// ----- the changes ----------------------------------------------------------------------

/// Where a change is: waiting to go out, on its way, or written and waiting for a read
/// (numbered in the order the writes finished; `Unknown`: no answer came).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    Queued,
    InFlight,
    Done(u64),
    Unknown(u64),
}

/// One change the player made, until a read settles it.
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    pub id: u64,
    pub change: Local,
    pub state: OpState,
    /// The last click that made or changed it.
    touched: Instant,
    /// An unpin the server answered 404: the read that settles it must not still show the pin.
    pub verify: bool,
}

impl Op {
    /// The number of its finished write, once it finished.
    pub fn finished(&self) -> Option<u64> {
        match self.state {
            OpState::Done(n) | OpState::Unknown(n) => Some(n),
            OpState::Queued | OpState::InFlight => None,
        }
    }

    fn ready(&self, now: Instant) -> bool {
        self.state == OpState::Queued
            && match self.change {
                Local::Adjust { .. } => now.saturating_duration_since(self.touched) >= COALESCE,
                _ => true,
            }
    }

    fn adjusts(&self, item: &str) -> bool {
        matches!(&self.change, Local::Adjust { item_id, .. } if item_id == item)
    }
}

/// A request for the data thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    Read,
    /// The change of [`Op`] `id`.
    Write(u64, Local),
    /// A search on an older server (no index): its number (see `Search::due`) and query.
    Search(u64, String),
    /// Fetch (or revalidate) the search index.
    Index,
    /// The player's whole inventory: its number (see `InventoryFetch::due`).
    Inventory(u64),
    /// The channel's locations, for the move picker's filter.
    Locations,
}

/// The request on its way, with the number of the last finished write when it went out
/// (for the reads and the inventory: what their answers include).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flight {
    Read(u64),
    Write(u64),
    Search,
    Index,
    Inventory(u64),
    Locations,
}

/// What a write's answer means for the player.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteEffect {
    Nothing,
    /// Undone; the toast says why.
    Failed(String),
    /// Marked as not confirmed until the next read; the toast says so.
    Unknown(String),
}

/// The changes on their way and the schedule of every request (see the module docs).
#[derive(Debug)]
pub struct Engine {
    ops: Vec<Op>,
    next_id: u64,
    /// Writes finished so far.
    finished: u64,
    flight: Option<Flight>,
    visible: bool,
    read_due: Instant,
    /// Reads in a row that found the server unreachable.
    failures: u32,
    last_read: Option<Instant>,
    /// After a 429, nothing goes out before this.
    blocked_until: Option<Instant>,
    /// 429s in a row.
    limited: u32,
    budget: Budget,
    index_budget: Budget,
    inventory_budget: Budget,
    /// After a 429 on the index or the inventory, that kind (only) waits until then.
    index_blocked_until: Option<Instant>,
    inventory_blocked_until: Option<Instant>,
}

/// The end of the wait a 429 on the index or the inventory asks for.
fn aux_block(limit: Option<Limit>, now: Instant) -> Option<Instant> {
    limit.map(|Limit(retry)| now + retry.unwrap_or(AUX_LIMIT_WAIT).min(MAX_WAIT))
}

/// The earlier of `a` (if any) and `t`.
fn earliest(a: Option<Instant>, t: Instant) -> Option<Instant> {
    Some(a.map_or(t, |a| a.min(t)))
}

impl Engine {
    /// Shown, with the first read due right away.
    pub fn new(now: Instant) -> Self {
        Self {
            ops: Vec::new(),
            next_id: 0,
            finished: 0,
            flight: None,
            visible: true,
            read_due: now,
            failures: 0,
            last_read: None,
            blocked_until: None,
            limited: 0,
            budget: Budget::default(),
            index_budget: Budget::default(),
            inventory_budget: Budget::default(),
            index_blocked_until: None,
            inventory_blocked_until: None,
        }
    }

    /// A click. A `-` / `+` joins the sum still waiting for its item; anything else is a
    /// change of its own. `false` when [`MAX_OPS`] changes wait already (nothing happens).
    pub fn click(&mut self, change: Local, now: Instant) -> bool {
        if let Local::Adjust { item_id, delta } = &change
            && let Some(op) = self.ops.iter_mut().rev().find(|o| o.state == OpState::Queued && o.adjusts(item_id))
        {
            if let Local::Adjust { delta: sum, .. } = &mut op.change {
                *sum = sum.saturating_add(*delta).clamp(-MAX_DELTA, MAX_DELTA);
            }
            op.touched = now;
            return true;
        }
        if self.full() {
            return false;
        }
        self.next_id += 1;
        self.ops.push(Op { id: self.next_id, change, state: OpState::Queued, touched: now, verify: false });
        true
    }

    /// The request to send now, if any (see the module docs); `search` is asked only when a
    /// search may go out, and returns it (numbered) when one is due.
    #[cfg(test)]
    pub fn next_job(&mut self, now: Instant, search: impl FnOnce() -> Option<(u64, String)>) -> Option<Job> {
        self.next_job_all(now, || false, || None, || false, search)
    }

    /// [`next_job`](Self::next_job), with the search index (`index`: one is wanted now) and
    /// the inventory (`inventory`: its number, once it is due), each within a budget of its
    /// own, then the channel's locations (`locations`: wanted now; within the reads' budget),
    /// in that order. They go before a search on an older server and the regular read, never
    /// before a write or the read that settles it. Each closure is asked only when its
    /// request may go out now.
    pub fn next_job_all(
        &mut self,
        now: Instant,
        index: impl FnOnce() -> bool,
        inventory: impl FnOnce() -> Option<u64>,
        locations: impl FnOnce() -> bool,
        search: impl FnOnce() -> Option<(u64, String)>,
    ) -> Option<Job> {
        if self.flight.is_some() || self.blocked_until.is_some_and(|t| now < t) {
            return None;
        }
        // Clicks that summed to nothing: nothing to send.
        self.ops.retain(|o| !(o.ready(now) && matches!(o.change, Local::Adjust { delta: 0, .. })));
        if let Some(i) = self.ops.iter().position(|o| o.ready(now)) {
            if !self.budget.allows(now, true) {
                return None;
            }
            self.budget.record(now);
            let op = &mut self.ops[i];
            op.state = OpState::InFlight;
            self.flight = Some(Flight::Write(op.id));
            return Some(Job::Write(op.id, op.change.clone()));
        }
        let queued = self.queued();
        if self.visible && !queued && self.unsettled() && self.last_read.is_none_or(|t| now >= t + READ_AFTER_WRITE_GAP) && self.budget.allows(now, false) {
            return Some(self.start_read(now));
        }
        if self.index_blocked_until.is_none_or(|t| now >= t) && self.index_budget.allows_cap(now, BUDGET_INDEX) && index() {
            self.index_budget.record(now);
            self.flight = Some(Flight::Index);
            return Some(Job::Index);
        }
        if self.inventory_blocked_until.is_none_or(|t| now >= t)
            && self.inventory_budget.allows_cap(now, BUDGET_INVENTORY)
            && let Some(seq) = inventory()
        {
            self.inventory_budget.record(now);
            self.flight = Some(Flight::Inventory(self.finished));
            return Some(Job::Inventory(seq));
        }
        if self.budget.allows(now, false) && locations() {
            self.budget.record(now);
            self.flight = Some(Flight::Locations);
            return Some(Job::Locations);
        }
        if self.budget.allows(now, false)
            && let Some((seq, q)) = search()
        {
            self.budget.record(now);
            self.flight = Some(Flight::Search);
            return Some(Job::Search(seq, q));
        }
        if self.visible && !queued && now >= self.read_due && self.budget.allows(now, false) {
            return Some(self.start_read(now));
        }
        None
    }

    fn start_read(&mut self, now: Instant) -> Job {
        self.budget.record(now);
        self.flight = Some(Flight::Read(self.finished));
        Job::Read
    }

    /// How long until [`Engine::next_job`] may have something (`None`: nothing scheduled, or
    /// a request is on its way and its answer wakes the window).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.flight.is_some() {
            return None;
        }
        let mut at = None;
        for o in self.ops.iter().filter(|o| o.state == OpState::Queued) {
            at = earliest(at, if matches!(o.change, Local::Adjust { .. }) { o.touched + COALESCE } else { now });
        }
        let queued = self.queued();
        if self.visible && !queued {
            if self.unsettled() {
                at = earliest(at, self.last_read.map_or(now, |t| t + READ_AFTER_WRITE_GAP));
            }
            at = earliest(at, self.read_due);
        }
        let mut at = at?;
        if let Some(b) = self.blocked_until {
            at = at.max(b);
        }
        at = at.max(self.budget.free_at(now, queued));
        Some(at.saturating_duration_since(now).max(MIN_WAKE))
    }

    /// Note a 429 (`Some`) or any other answer.
    fn note_limit(&mut self, limit: Option<Limit>, now: Instant) {
        match limit {
            Some(Limit(retry)) => {
                self.limited = self.limited.saturating_add(1);
                let wait = retry.unwrap_or_else(|| next_delay(Outcome::Unreachable, self.limited)).min(MAX_WAIT);
                self.blocked_until = Some(now + wait);
            }
            None => self.limited = 0,
        }
    }

    /// A read came back with `outcome` (`limit`: it was a 429). Returns the changes it
    /// settled (good answers only): their writes finished before it went out, so the answer
    /// has them.
    pub fn on_read(&mut self, outcome: Outcome, limit: Option<Limit>, now: Instant) -> Vec<Op> {
        let mark = match self.flight.take() {
            Some(Flight::Read(m)) => m,
            _ => 0,
        };
        self.last_read = Some(now);
        self.note_limit(limit, now);
        self.failures = if outcome == Outcome::Unreachable { self.failures.saturating_add(1) } else { 0 };
        let mut wait = next_delay(outcome, self.failures);
        if let Some(Limit(Some(retry))) = limit {
            wait = wait.max(retry.min(MAX_WAIT));
        }
        self.read_due = now + wait;
        if outcome != Outcome::Good {
            return Vec::new();
        }
        let (settled, kept): (Vec<Op>, Vec<Op>) = std::mem::take(&mut self.ops).into_iter().partition(|o| o.finished().is_some_and(|n| n <= mark));
        self.ops = kept;
        settled
    }

    /// The write of change `id` came back.
    pub fn on_write(&mut self, id: u64, result: WriteResult, now: Instant) -> WriteEffect {
        self.flight = None;
        let limit = match &result {
            WriteResult::Limited(retry) => Some(Limit(*retry)),
            _ => None,
        };
        self.note_limit(limit, now);
        let Some(i) = self.ops.iter().position(|o| o.id == id) else { return WriteEffect::Nothing };
        let gone = result == WriteResult::Gone;
        match result {
            WriteResult::Applied | WriteResult::Gone => {
                self.finished += 1;
                let op = &mut self.ops[i];
                op.state = OpState::Done(self.finished);
                op.verify = gone;
                WriteEffect::Nothing
            }
            WriteResult::Unknown(msg) => {
                self.finished += 1;
                self.ops[i].state = OpState::Unknown(self.finished);
                WriteEffect::Unknown(msg)
            }
            // Not applied: it waits (with any clicks added meanwhile) until the wait is over.
            WriteResult::Limited(_) => {
                self.ops[i].state = OpState::Queued;
                WriteEffect::Nothing
            }
            WriteResult::Failed(msg) => {
                self.ops.remove(i);
                WriteEffect::Failed(msg)
            }
            WriteResult::Stale => {
                self.ops.remove(i);
                WriteEffect::Nothing
            }
        }
    }

    /// A search came back (`limit`: it was a 429, which holds back everything).
    pub fn on_search(&mut self, limit: Option<Limit>, now: Instant) {
        if matches!(self.flight, Some(Flight::Search)) {
            self.flight = None;
        }
        self.note_limit(limit, now);
    }

    /// The locations came back (`limit`: it was a 429, which holds back everything).
    pub fn on_locations(&mut self, limit: Option<Limit>, now: Instant) {
        if matches!(self.flight, Some(Flight::Locations)) {
            self.flight = None;
        }
        self.note_limit(limit, now);
    }

    /// How long until the locations may go out (the reads' budget, a 429); zero: now.
    pub fn locations_ready_in(&self, now: Instant) -> Duration {
        self.search_ready_in(now)
    }

    /// The index request came back (`limit`: it was a 429, which holds back only the index).
    pub fn on_index(&mut self, limit: Option<Limit>, now: Instant) {
        if matches!(self.flight, Some(Flight::Index)) {
            self.flight = None;
        }
        if let Some(t) = aux_block(limit, now) {
            self.index_blocked_until = Some(t);
        }
    }

    /// The inventory came back (`limit`: it was a 429, which holds back only the inventory).
    /// Returns the number of the last write finished before it went out: it includes that one
    /// and every earlier one.
    pub fn on_inventory(&mut self, limit: Option<Limit>, now: Instant) -> u64 {
        let mark = match self.flight {
            Some(Flight::Inventory(m)) => {
                self.flight = None;
                m
            }
            _ => 0,
        };
        if let Some(t) = aux_block(limit, now) {
            self.inventory_blocked_until = Some(t);
        }
        mark
    }

    /// How long until a search (older servers) may go out (the reads' budget, a 429); zero:
    /// now.
    pub fn search_ready_in(&self, now: Instant) -> Duration {
        let at = self.budget.free_at(now, false).max(self.blocked_until.unwrap_or(now));
        at.saturating_duration_since(now)
    }

    /// How long until the index may go out (its budget, a 429); zero: now.
    pub fn index_ready_in(&self, now: Instant) -> Duration {
        let at = self.index_budget.free_at_cap(now, BUDGET_INDEX).max(self.index_blocked_until.unwrap_or(now));
        at.saturating_duration_since(now)
    }

    /// How long until the inventory may go out (its budget, a 429 on it or on everything);
    /// zero: now.
    pub fn inventory_ready_in(&self, now: Instant) -> Duration {
        let blocked = self.inventory_blocked_until.into_iter().chain(self.blocked_until).max().unwrap_or(now);
        let at = self.inventory_budget.free_at_cap(now, BUDGET_INVENTORY).max(blocked);
        at.saturating_duration_since(now)
    }

    /// An answer that is no longer wanted (another generation) came back: nothing is on its
    /// way any more.
    pub fn abandon(&mut self) {
        self.flight = None;
        self.ops.retain(|o| o.state != OpState::InFlight);
    }

    /// Another server, account or channel: nothing of before applies; read at once.
    pub fn reset(&mut self, now: Instant) {
        self.ops.clear();
        self.failures = 0;
        self.read_due = now;
        self.blocked_until = None;
        self.limited = 0;
        self.budget = Budget::default();
        self.index_budget = Budget::default();
        self.inventory_budget = Budget::default();
        self.index_blocked_until = None;
        self.inventory_blocked_until = None;
    }

    /// Signed out, or a server without the overlay: the changes are dropped.
    pub fn clear_ops(&mut self) {
        self.ops.clear();
    }

    /// Shown (`true`) or hidden: reads only happen while shown, and showing reads at once.
    pub fn set_visible(&mut self, on: bool, now: Instant) {
        if on && !self.visible {
            self.read_due = now;
        }
        self.visible = on;
    }

    /// Read as soon as possible (watcher.json now names another endpoint).
    pub fn read_now(&mut self, now: Instant) {
        self.read_due = now;
    }

    /// No request on its way.
    pub fn idle(&self) -> bool {
        self.flight.is_none()
    }

    /// [`MAX_OPS`] changes wait: no more clicks for now.
    pub fn full(&self) -> bool {
        self.ops.len() >= MAX_OPS
    }

    fn queued(&self) -> bool {
        self.ops.iter().any(|o| o.state == OpState::Queued)
    }

    /// Writes that finished and wait for a read.
    fn unsettled(&self) -> bool {
        self.ops.iter().any(|o| o.finished().is_some())
    }

    /// The read about to go out (or on its way) settles writes: its answer must have them.
    pub fn settling(&self) -> bool {
        self.unsettled()
    }

    #[cfg(test)]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Every change still on its way, in the order they were made (drawn over the server's
    /// answer).
    pub fn changes(&self) -> impl Iterator<Item = &Local> {
        self.ops.iter().map(|o| &o.change)
    }

    /// The changes an answer sent after write `mark` finished does not have yet.
    pub fn changes_after(&self, mark: u64) -> impl Iterator<Item = &Local> {
        self.ops.iter().filter(move |o| o.finished().is_none_or(|n| n > mark)).map(|o| &o.change)
    }

    /// A write of `item`'s quantity got no answer: its number is not confirmed, and it takes
    /// no more clicks until a read says what the server has.
    pub fn unconfirmed(&self, item: &str) -> bool {
        self.ops.iter().any(|o| matches!(o.state, OpState::Unknown(_)) && o.adjusts(item))
    }

    /// A pin or unpin of `item` is on its way.
    pub fn pin_busy(&self, item: &str) -> bool {
        self.ops.iter().any(|o| matches!(&o.change, Local::PinItem { item_id, .. } | Local::UnpinItem { item_id } if item_id == item))
    }

    /// An unpin of `item` is on its way: its row stays, marked, until a read settles it.
    pub fn unpinning(&self, item: &str) -> bool {
        self.ops.iter().any(|o| matches!(&o.change, Local::UnpinItem { item_id } if item_id == item))
    }

    /// A pin or unpin of mission `mission` is on its way.
    pub fn mission_busy(&self, mission: &str) -> bool {
        self.ops.iter().any(|o| matches!(&o.change, Local::PinMission { mission_id, .. } | Local::UnpinMission { mission_id } if mission_id == mission))
    }

    /// An unpin of mission `mission` is on its way.
    pub fn unpinning_mission(&self, mission: &str) -> bool {
        self.ops.iter().any(|o| matches!(&o.change, Local::UnpinMission { mission_id } if mission_id == mission))
    }

    /// A move of `item` between places is on its way (its places are being changed: no other
    /// move of it until a read settles this one).
    pub fn moving(&self, item: &str) -> bool {
        self.ops.iter().any(|o| matches!(&o.change, Local::Move { item_id, .. } if item_id == item))
    }

    /// The option chosen for reward choice `id`, while that answer is on its way.
    pub fn choosing(&self, id: i64) -> Option<&str> {
        self.ops.iter().find_map(|o| match &o.change {
            Local::Choose { id: c, item_id } if *c == id => Some(item_id.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay_model::{self as model, OverlayData};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn adjust(item: &str, delta: i64) -> Local {
        Local::Adjust { item_id: item.into(), delta }
    }

    fn no_search() -> Option<(u64, String)> {
        None
    }

    /// An engine whose first read went out and came back good at `t0`.
    fn settled_engine(t0: Instant) -> Engine {
        let mut e = Engine::new(t0);
        assert_eq!(e.next_job(t0, no_search), Some(Job::Read));
        assert!(e.on_read(Outcome::Good, None, t0).is_empty());
        e
    }

    #[test]
    fn polls_every_5_s_while_good() {
        assert_eq!(next_delay(Outcome::Good, 0), Duration::from_secs(5));
        assert_eq!(next_delay(Outcome::Good, 7), Duration::from_secs(5), "a good answer ends the back-off");
    }

    #[test]
    fn waits_long_when_signed_out_or_the_server_is_old() {
        assert_eq!(next_delay(Outcome::SignedOut, 0), Duration::from_secs(300));
        assert_eq!(next_delay(Outcome::NoRoute, 0), Duration::from_secs(300));
    }

    #[test]
    fn backs_off_exponentially_up_to_5_min() {
        let secs: Vec<u64> = (1..=9).map(|n| next_delay(Outcome::Unreachable, n).as_secs()).collect();
        assert_eq!(secs, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(next_delay(Outcome::Unreachable, u32::MAX), Duration::from_secs(300), "no overflow");
        assert_eq!(next_delay(Outcome::Unreachable, 0), Duration::from_secs(5));
    }

    #[test]
    fn clicks_on_one_item_go_out_as_one_write_after_a_pause() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert!(e.click(adjust("x", 1), t0));
        assert!(e.click(adjust("x", 1), t0 + ms(200)));
        assert!(e.click(adjust("x", 10), t0 + ms(500)));
        assert!(e.click(adjust("y", -1), t0 + ms(500)));
        assert_eq!(e.ops().len(), 2, "one change per item");
        assert_eq!(e.next_job(t0 + ms(800), no_search), None, "still clicking: the pause starts over with each click");
        assert_eq!(e.wait(t0 + ms(800)), Some(ms(100)));
        assert_eq!(e.next_job(t0 + ms(900), no_search), Some(Job::Write(1, adjust("x", 12))), "the sum, once");
        assert_eq!(e.next_job(t0 + ms(900), no_search), None, "one request at a time");
        assert_eq!(e.on_write(1, WriteResult::Applied, t0 + ms(950)), WriteEffect::Nothing);
        // A click while the sum is on its way starts a new one.
        assert!(e.click(adjust("x", 1), t0 + ms(920)));
        assert_eq!(e.next_job(t0 + ms(950), no_search), Some(Job::Write(2, adjust("y", -1))));
        e.on_write(2, WriteResult::Applied, t0 + ms(1000));
        assert_eq!(e.next_job(t0 + ms(1400), no_search), Some(Job::Write(3, adjust("x", 1))));
        e.on_write(3, WriteResult::Applied, t0 + ms(1450));
        // Clicks that cancel out send nothing.
        e.click(adjust("z", 1), t0 + ms(1500));
        e.click(adjust("z", -1), t0 + ms(1600));
        assert_eq!(e.next_job(t0 + ms(2100), no_search), Some(Job::Read), "only the settling read");
        assert!(!e.ops().iter().any(|o| o.adjusts("z")));
        // A sum never grows past the limit.
        let mut big = settled_engine(t0);
        big.click(adjust("x", MAX_DELTA), t0);
        big.click(adjust("x", MAX_DELTA), t0);
        assert_eq!(big.ops()[0].change, adjust("x", MAX_DELTA));
    }

    #[test]
    fn other_changes_go_out_at_once_in_order() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(adjust("x", 1), t0);
        e.click(Local::UnpinItem { item_id: "a".into() }, t0);
        e.click(Local::Choose { id: 41, item_id: "b".into() }, t0);
        assert_eq!(e.next_job(t0, no_search), Some(Job::Write(2, Local::UnpinItem { item_id: "a".into() })), "the sum still waits for its pause");
        e.on_write(2, WriteResult::Applied, t0);
        assert_eq!(e.next_job(t0, no_search), Some(Job::Write(3, Local::Choose { id: 41, item_id: "b".into() })));
        e.on_write(3, WriteResult::Applied, t0);
        assert_eq!(e.next_job(t0 + ms(100), no_search), None, "no read while a change waits to go out");
        assert_eq!(e.next_job(t0 + COALESCE, no_search), Some(Job::Write(1, adjust("x", 1))));
    }

    #[test]
    fn the_queue_is_bounded() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        for i in 0..MAX_OPS {
            assert!(e.click(Local::UnpinItem { item_id: format!("i{i}") }, t0));
        }
        assert!(e.full());
        assert!(!e.click(Local::UnpinItem { item_id: "more".into() }, t0), "full: refused");
        assert!(!e.click(adjust("new", 1), t0), "a new sum too");
        assert_eq!(e.ops().len(), MAX_OPS);
    }

    #[test]
    fn a_change_shows_until_a_read_sent_after_its_write_comes_back() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(adjust("x", 2), t0);
        assert_eq!(e.changes().cloned().collect::<Vec<_>>(), vec![adjust("x", 2)], "drawn at once");
        let t1 = t0 + COALESCE;
        assert_eq!(e.next_job(t1, no_search), Some(Job::Write(1, adjust("x", 2))));
        e.on_write(1, WriteResult::Applied, t1);
        // A read that went out before the write finished does not settle it (see the poll
        // below: none can go out meanwhile anyway, one request at a time).
        assert_eq!(e.next_job(t1, no_search), None, "the settling read waits for the gap after the last read");
        assert_eq!(e.wait(t1), Some(READ_AFTER_WRITE_GAP - COALESCE));
        let t2 = t0 + READ_AFTER_WRITE_GAP;
        assert_eq!(e.next_job(t2, no_search), Some(Job::Read));
        // A click during that read is not settled by it.
        e.click(Local::PinItem { item_id: "p".into(), name: "P".into(), have: 1.0 }, t2);
        let settled = e.on_read(Outcome::Good, None, t2);
        assert_eq!(settled.iter().map(|o| o.change.clone()).collect::<Vec<_>>(), vec![adjust("x", 2)]);
        assert_eq!(e.changes().count(), 1, "the pin is still on its way");
        // A failed read settles nothing.
        assert_eq!(e.next_job(t2, no_search).map(|j| matches!(j, Job::Write(..))), Some(true));
        e.on_write(2, WriteResult::Applied, t2);
        assert_eq!(e.next_job(t2 + READ_AFTER_WRITE_GAP, no_search), Some(Job::Read));
        assert!(e.on_read(Outcome::Unreachable, None, t2 + READ_AFTER_WRITE_GAP).is_empty());
        assert_eq!(e.changes().count(), 1);
    }

    #[test]
    fn a_refused_write_is_undone_and_an_unanswered_one_waits_for_a_read() {
        let t0 = Instant::now();
        let server = OverlayData { tracked: vec![model::TrackedItem { item_id: "favor".into(), have: 3.0, ..Default::default() }], ..Default::default() };
        let mut e = settled_engine(t0);
        e.click(adjust("favor", 1), t0);
        let t1 = t0 + COALESCE;
        e.next_job(t1, no_search);
        e.click(adjust("favor", 10), t1);
        assert_eq!(model::view(&server, e.changes()).tracked[0].have, 14.0);
        // The first one was refused: only it goes back.
        match e.on_write(1, WriteResult::Failed("não deu para ajustar a quantidade: HTTP 400".into()), t1) {
            WriteEffect::Failed(msg) => assert!(msg.contains("HTTP 400"), "{msg}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(e.changes().cloned().collect::<Vec<_>>(), vec![adjust("favor", 10)], "the refused one is dropped");
        assert_eq!(model::view(&server, e.changes()).tracked[0].have, 13.0, "only the refused one goes back");
        assert!(!e.unconfirmed("favor"));
        // The second gets no answer: still drawn, marked, and no more clicks on the item.
        let t2 = t1 + COALESCE;
        assert_eq!(e.next_job(t2, no_search), Some(Job::Write(2, adjust("favor", 10))));
        assert!(matches!(e.on_write(2, WriteResult::Unknown("timeout".into()), t2), WriteEffect::Unknown(_)));
        assert!(e.unconfirmed("favor") && !e.unconfirmed("other"));
        assert_eq!(model::view(&server, e.changes()).tracked[0].have, 13.0, "kept until the read, never undone by arithmetic");
        // Never sent again: the next request is the read, and what it says is taken as it is.
        assert_eq!(e.next_job(t2 + READ_AFTER_WRITE_GAP, no_search), Some(Job::Read));
        let settled = e.on_read(Outcome::Good, None, t2 + READ_AFTER_WRITE_GAP);
        assert_eq!(settled.len(), 1);
        assert!(matches!(settled[0].state, OpState::Unknown(_)));
        assert!(!e.unconfirmed("favor") && e.ops().is_empty());
        assert_eq!(model::view(&server, e.changes()), server);
    }

    #[test]
    fn writes_are_told_apart_by_how_they_ended() {
        let w = |a: Result<(u16, &[u8], Option<Duration>), &str>, unpin: bool| classify_write(a, unpin, "desafixar o item");
        let empty: &[u8] = &[];
        assert_eq!(w(Ok((200, &b"{}"[..], None)), false), WriteResult::Applied);
        assert_eq!(w(Ok((204, empty, None)), true), WriteResult::Applied);
        assert_eq!(w(Ok((404, empty, None)), true), WriteResult::Gone, "an unpin's 404 waits for the read");
        assert_eq!(w(Ok((404, empty, None)), false), WriteResult::Failed("não deu para desafixar o item: HTTP 404".into()));
        assert_eq!(w(Ok((429, empty, Some(Duration::from_secs(7)))), false), WriteResult::Limited(Some(Duration::from_secs(7))), "a 429 is never a failure");
        assert_eq!(w(Ok((400, r#"{"error":"quantidade inválida"}"#.as_bytes(), None)), false), WriteResult::Failed("não deu para desafixar o item: quantidade inválida".into()));
        assert_eq!(w(Ok((502, &b"<html>"[..], None)), false), WriteResult::Failed("não deu para desafixar o item: HTTP 502".into()), "an HTTP error status is definite");
        match w(Err("timeout"), false) {
            WriteResult::Unknown(msg) => assert!(msg.contains("timeout") && msg.contains("conferindo"), "{msg}"),
            other => panic!("no status is unknown, not failed: {other:?}"),
        }
    }

    #[test]
    fn a_429_keeps_the_sum_and_waits() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(adjust("x", 3), t0);
        let t1 = t0 + COALESCE;
        assert_eq!(e.next_job(t1, no_search), Some(Job::Write(1, adjust("x", 3))));
        assert_eq!(e.on_write(1, WriteResult::Limited(Some(Duration::from_secs(7))), t1), WriteEffect::Nothing);
        assert_eq!(e.changes().cloned().collect::<Vec<_>>(), vec![adjust("x", 3)], "never undone");
        // More clicks join the waiting sum.
        e.click(adjust("x", 1), t1 + ms(10));
        assert_eq!(e.ops().len(), 1);
        assert_eq!(e.next_job(t1 + Duration::from_secs(6), no_search), None, "the server's wait");
        assert_eq!(e.wait(t1 + Duration::from_secs(6)), Some(Duration::from_secs(1)));
        assert_eq!(e.next_job(t1 + Duration::from_secs(7), || Some((1, "ca".into()))), Some(Job::Write(1, adjust("x", 4))));
        // Without a wait from the server: the back-off, growing with each 429 in a row.
        let t2 = t1 + Duration::from_secs(7);
        e.on_write(1, WriteResult::Limited(None), t2);
        assert_eq!(e.next_job(t2 + Duration::from_secs(9), no_search), None);
        assert!(e.next_job(t2 + Duration::from_secs(10), no_search).is_some(), "second 429 in a row: 10 s");
        e.on_write(1, WriteResult::Applied, t2 + Duration::from_secs(10));
        // A 429 on a read or a search holds everything back too.
        let t3 = t2 + Duration::from_secs(20);
        assert_eq!(e.next_job(t3, no_search), Some(Job::Read));
        e.on_read(Outcome::Unreachable, Some(Limit(Some(Duration::from_secs(30)))), t3);
        e.click(Local::UnpinItem { item_id: "a".into() }, t3);
        assert_eq!(e.next_job(t3 + Duration::from_secs(29), no_search), None);
        assert!(matches!(e.next_job(t3 + Duration::from_secs(30), no_search), Some(Job::Write(..))));
    }

    #[test]
    fn polls_wait_while_changes_wait_and_one_read_settles_them() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert_eq!(e.next_job(t0 + POLL_EVERY - ms(1), no_search), None);
        e.click(adjust("x", 1), t0 + POLL_EVERY - ms(100));
        assert_eq!(e.next_job(t0 + POLL_EVERY, no_search), None, "the poll waits for the change");
        let t1 = t0 + POLL_EVERY + ms(300);
        assert!(matches!(e.next_job(t1, no_search), Some(Job::Write(..))));
        e.on_write(1, WriteResult::Applied, t1);
        assert_eq!(e.next_job(t1, no_search), Some(Job::Read), "one read settles it (the last was long ago)");
        e.on_read(Outcome::Good, None, t1);
        assert!(e.ops().is_empty());
        assert_eq!(e.next_job(t1 + ms(10), no_search), None, "and the poll starts over");
        assert_eq!(e.wait(t1 + ms(10)), Some(POLL_EVERY - ms(10)));
        // Hidden: no reads at all, writes still go; shown again: a read at once.
        e.set_visible(false, t1);
        assert_eq!(e.next_job(t1 + POLL_EVERY * 3, no_search), None);
        e.set_visible(true, t1 + POLL_EVERY * 3);
        assert_eq!(e.next_job(t1 + POLL_EVERY * 3, no_search), Some(Job::Read));
    }

    #[test]
    fn searches_share_the_one_request_and_know_what_they_include() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert_eq!(e.next_job(t0 + ms(10), || Some((1, "car".into()))), Some(Job::Search(1, "car".into())));
        assert_eq!(e.next_job(t0 + ms(10), || panic!("not asked while a request is on its way")), None);
        e.on_search(None, t0 + ms(20));
        assert!(e.idle(), "its answer ends the flight");
        e.click(Local::UnpinItem { item_id: "a".into() }, t0 + ms(20));
        e.next_job(t0 + ms(20), no_search);
        e.on_write(1, WriteResult::Applied, t0 + ms(30));
        e.click(Local::UnpinItem { item_id: "b".into() }, t0 + ms(40));
        // A search sent now has the first unpin, not the second.
        assert!(matches!(e.next_job(t0 + ms(40), no_search), Some(Job::Write(2, _))));
        e.on_write(2, WriteResult::Applied, t0 + ms(50));
        assert_eq!(e.changes_after(1).cloned().collect::<Vec<_>>(), vec![Local::UnpinItem { item_id: "b".into() }]);
        assert_eq!(e.changes_after(0).count(), 2);
        assert_eq!(e.changes_after(2).count(), 0);
    }

    #[test]
    fn submitted_unpins_and_choices_stay_marked_until_settled() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(Local::UnpinItem { item_id: "favor".into() }, t0);
        e.click(Local::UnpinMission { mission_id: "m1".into() }, t0);
        e.click(Local::Choose { id: 41, item_id: "a".into() }, t0);
        assert!(e.unpinning("favor") && e.pin_busy("favor") && !e.unpinning("other"));
        assert!(e.unpinning_mission("m1") && e.mission_busy("m1"));
        assert_eq!((e.choosing(41), e.choosing(42)), (Some("a"), None));
        for id in 1..=3 {
            assert!(matches!(e.next_job(t0, no_search), Some(Job::Write(..))));
            let result = if id == 3 { WriteResult::Failed("x".into()) } else { WriteResult::Applied };
            e.on_write(id, result, t0);
        }
        assert_eq!(e.choosing(41), None, "a refused choice can be made again");
        assert!(e.unpinning("favor"), "written, still marked until the read");
        e.next_job(t0 + READ_AFTER_WRITE_GAP, no_search);
        e.on_read(Outcome::Good, None, t0 + READ_AFTER_WRITE_GAP);
        assert!(!e.unpinning("favor") && !e.mission_busy("m1"));
        // A pin from the search is busy, not leaving.
        e.click(Local::PinItem { item_id: "p".into(), name: "P".into(), have: 0.0 }, t0);
        assert!(e.pin_busy("p") && !e.unpinning("p"));
    }

    #[test]
    fn an_unpin_answered_404_is_checked_by_the_read() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(Local::UnpinMission { mission_id: "m1".into() }, t0);
        e.next_job(t0, no_search);
        assert_eq!(e.on_write(1, WriteResult::Gone, t0), WriteEffect::Nothing, "no toast yet");
        assert!(e.unpinning_mission("m1"));
        e.next_job(t0 + READ_AFTER_WRITE_GAP, no_search);
        let settled = e.on_read(Outcome::Good, None, t0 + READ_AFTER_WRITE_GAP);
        assert_eq!(settled.len(), 1);
        assert!(settled[0].verify, "the overlay checks the answer still has the pin");
    }

    #[test]
    fn a_new_generation_starts_from_nothing() {
        let mut g = Generations::default();
        let a = || ("https://a".to_string(), "t1".to_string(), "LIVE".to_string());
        assert_eq!(g.observe(a()), 1);
        assert_eq!(g.observe(a()), 1, "the same endpoint");
        assert_eq!(g.observe(("https://a".into(), "t1".into(), "PTU".into())), 2, "another channel");
        assert_eq!(g.observe(("https://a".into(), "t2".into(), "PTU".into())), 3, "another token");
        assert_eq!(g.observe((String::new(), String::new(), "PTU".into())), 4, "signed out");
        assert_eq!(g.observe(a()), 5, "back to the first is another generation");
        assert!(accepts(5, 5));
        assert!(!accepts(4, 5), "a write made for the old one is never sent");
        assert!(!accepts(0, 0), "nothing shown yet: nothing to write");

        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(adjust("x", 1), t0);
        e.click(Local::UnpinItem { item_id: "a".into() }, t0);
        e.next_job(t0, no_search);
        e.reset(t0 + ms(5));
        assert!(e.ops().is_empty() && !e.full());
        assert_eq!(e.on_write(2, WriteResult::Applied, t0 + ms(10)), WriteEffect::Nothing, "an answer for a dropped change");
        assert_eq!(e.next_job(t0 + ms(10), no_search), Some(Job::Read), "read the new endpoint at once");
        // A stale write is simply dropped.
        e.on_read(Outcome::Good, None, t0 + ms(20));
        e.click(Local::UnpinItem { item_id: "b".into() }, t0 + ms(20));
        e.next_job(t0 + ms(20), no_search);
        assert_eq!(e.on_write(3, WriteResult::Stale, t0 + ms(30)), WriteEffect::Nothing);
        assert!(e.ops().is_empty());
    }

    #[test]
    fn one_budget_for_every_request_with_room_kept_for_writes() {
        let t0 = Instant::now();
        let mut b = Budget::default();
        for i in 0..BUDGET_READS {
            assert!(b.allows(t0 + ms(i as u64), false));
            b.record(t0 + ms(i as u64));
        }
        let t1 = t0 + ms(100);
        assert!(!b.allows(t1, false), "reads and searches stop first");
        assert!(b.allows(t1, true), "writes still fit");
        assert_eq!(b.free_at(t1, false), t0 + BUDGET_WINDOW);
        for _ in BUDGET_READS..BUDGET_ALL {
            b.record(t1);
        }
        assert!(!b.allows(t1, true));
        assert_eq!(b.free_at(t1, true), t0 + BUDGET_WINDOW, "when the oldest leaves the window");
        assert!(b.allows(t0 + BUDGET_WINDOW + ms(BUDGET_READS as u64), true), "a minute later it fits again");
        assert!(b.allows(t1 + BUDGET_WINDOW, false));

        // The engine holds back a poll the budget has no room for, and says when.
        let mut e = Engine::new(t0);
        for i in 0..BUDGET_READS {
            e.budget.record(t0 + ms(i as u64));
        }
        assert_eq!(e.next_job(t1, no_search), None);
        assert_eq!(e.wait(t1), Some(BUDGET_WINDOW - ms(100)));
        assert_eq!(e.search_ready_in(t1), BUDGET_WINDOW - ms(100), "a search waits for the same room");
        e.click(Local::UnpinItem { item_id: "a".into() }, t1);
        assert!(matches!(e.next_job(t1, no_search), Some(Job::Write(..))), "a write still goes");
    }

    #[test]
    fn the_index_waits_for_writes_and_goes_before_a_search() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        e.click(Local::UnpinItem { item_id: "a".into() }, t0);
        assert!(matches!(e.next_job_all(t0, || true, || None, || false, no_search), Some(Job::Write(1, _))), "a write first");
        assert!(!e.settling(), "not written yet");
        e.on_write(1, WriteResult::Applied, t0);
        assert!(e.settling(), "the next read must have the write: no ETag on it");
        let t1 = t0 + READ_AFTER_WRITE_GAP;
        assert_eq!(e.next_job_all(t1, || true, || None, || false, no_search), Some(Job::Read), "then the read that settles it");
        e.on_read(Outcome::Good, None, t1);
        assert!(!e.settling());
        assert_eq!(e.next_job_all(t1, || true, || None, || false, no_search), Some(Job::Index));
        assert_eq!(e.next_job_all(t1, || panic!("one request at a time"), || None, || false, no_search), None);
        e.on_index(None, t1);
        assert_eq!(e.next_job_all(t1, || false, || None, || false, || Some((7, "ca".into()))), Some(Job::Search(7, "ca".into())));
        e.on_search(None, t1);
        assert_eq!(e.next_job_all(t1, || false, || None, || false, no_search), None, "the regular read keeps its time");
    }

    #[test]
    fn a_429_on_the_index_holds_back_only_the_index() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert_eq!(e.next_job_all(t0, || true, || None, || false, no_search), Some(Job::Index));
        // Without a wait from the server: AUX_LIMIT_WAIT.
        e.on_index(Some(Limit(None)), t0);
        assert_eq!(e.index_ready_in(t0), AUX_LIMIT_WAIT);
        assert_eq!(e.next_job_all(t0 + ms(10), || panic!("held back"), || Some(1), || false, no_search), Some(Job::Inventory(1)), "the rest still goes");
        e.on_inventory(None, t0 + ms(10));
        e.click(Local::UnpinItem { item_id: "b".into() }, t0);
        assert!(matches!(e.next_job_all(t0 + ms(20), || panic!("held back"), || None, || false, no_search), Some(Job::Write(1, _))), "writes too");
        e.on_write(1, WriteResult::Applied, t0 + ms(20));
        assert_eq!(e.index_ready_in(t0 + AUX_LIMIT_WAIT), Duration::ZERO);
        // Another endpoint forgets the wait.
        e.on_index(Some(Limit(Some(Duration::from_secs(30)))), t0);
        e.reset(t0);
        assert_eq!(e.index_ready_in(t0), Duration::ZERO);
    }

    #[test]
    fn the_index_has_a_budget_of_its_own() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        for i in 0..BUDGET_INDEX {
            e.index_budget.record(t0 + ms(i as u64));
        }
        let t1 = t0 + POLL_EVERY;
        assert_eq!(e.next_job_all(t1, || panic!("no room"), || None, || false, no_search), Some(Job::Read), "the reads have room of their own");
        e.on_read(Outcome::Good, None, t1);
        assert_eq!(e.index_ready_in(t1), BUDGET_WINDOW - POLL_EVERY);
        let t2 = t0 + BUDGET_WINDOW;
        assert_eq!(e.next_job_all(t2, || true, || None, || false, no_search), Some(Job::Index), "a minute later it fits again");
        // A poll every 5 s stays within the reads' budget.
        const { assert!(BUDGET_INDEX >= 2 && (60 / POLL_EVERY.as_secs()) as usize + 4 <= BUDGET_READS) };
    }

    #[test]
    fn the_inventory_goes_after_the_index_with_a_budget_and_429_of_its_own() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert_eq!(e.next_job_all(t0, || true, || panic!("the index first"), || panic!("the index first"), no_search), Some(Job::Index));
        e.on_index(None, t0);
        assert_eq!(e.next_job_all(t0, || false, || Some(1), || panic!("the inventory first"), || panic!("the inventory first")), Some(Job::Inventory(1)));
        assert_eq!(e.next_job_all(t0, || false, || panic!("one request at a time"), || panic!("one request at a time"), no_search), None);
        assert_eq!(e.on_inventory(None, t0), 0, "no write finished before it went out");
        // A write first, and the inventory sent after it knows it has it.
        e.click(Local::UnpinItem { item_id: "a".into() }, t0);
        assert!(matches!(e.next_job_all(t0, || false, || panic!("a write first"), || false, no_search), Some(Job::Write(1, _))));
        e.on_write(1, WriteResult::Applied, t0);
        let t1 = t0 + READ_AFTER_WRITE_GAP;
        assert_eq!(e.next_job_all(t1, || false, || panic!("the settling read first"), || false, no_search), Some(Job::Read));
        e.on_read(Outcome::Good, None, t1);
        assert_eq!(e.next_job_all(t1, || false, || Some(2), || false, no_search), Some(Job::Inventory(2)));
        assert_eq!(e.on_inventory(Some(Limit(Some(Duration::from_secs(9)))), t1), 1, "it has the unpin");
        // Its 429 holds back only the inventory.
        assert_eq!(e.inventory_ready_in(t1), Duration::from_secs(9));
        assert_eq!(e.next_job_all(t1, || false, || panic!("held back"), || false, || Some((1, "ca".into()))), Some(Job::Search(1, "ca".into())));
        e.on_search(None, t1);
        // Its budget.
        let mut e = settled_engine(t0);
        for i in 0..BUDGET_INVENTORY {
            e.inventory_budget.record(t0 + ms(i as u64));
        }
        assert_eq!(e.next_job_all(t0 + ms(100), || false, || panic!("no room"), || false, no_search), None);
        assert_eq!(e.inventory_ready_in(t0 + ms(100)), BUDGET_WINDOW - ms(100));
        e.reset(t0 + ms(100));
        assert_eq!(e.inventory_ready_in(t0 + ms(100)), Duration::ZERO, "another endpoint forgets it");
        // A refresh every 30 s plus one per settled write stays within it.
        const { assert!(BUDGET_INVENTORY >= 4) };
    }

    fn mv(item: &str, parts: &[(i64, i64)]) -> Local {
        Local::Move { item_id: item.into(), from: "A".into(), to: "B".into(), parts: parts.to_vec() }
    }

    #[test]
    fn a_move_in_several_parts_is_applied_failed_or_unknown_as_a_whole() {
        let w = "mover o item";
        let applied = || WriteResult::Applied;
        let failed = || WriteResult::Failed("não deu para mover o item: HTTP 400".into());
        // Part by part: go on while they go through.
        assert_eq!(classify_part(false, applied(), w), None);
        assert_eq!(classify_part(true, applied(), w), None);
        // The first part refused or held back: nothing was applied, as any write.
        assert_eq!(classify_part(false, failed(), w), Some(failed()));
        assert_eq!(classify_part(false, WriteResult::Limited(None), w), Some(WriteResult::Limited(None)));
        assert_eq!(classify_part(false, WriteResult::Unknown("timeout".into()), w), Some(WriteResult::Unknown("timeout".into())));
        // A later one: applied in part, so unknown (never undone, never sent again).
        for later in [failed(), WriteResult::Limited(Some(Duration::from_secs(5)))] {
            match classify_part(true, later, w) {
                Some(WriteResult::Unknown(msg)) => assert!(msg.contains("mover o item") && msg.contains("conferindo"), "{msg}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(classify_part(true, WriteResult::Unknown("timeout".into()), w), Some(WriteResult::Unknown("timeout".into())));
    }

    #[test]
    fn a_move_goes_out_at_once_and_follows_the_rules_of_every_write() {
        let t0 = Instant::now();
        let server = OverlayData { tracked: vec![model::TrackedItem { item_id: "x".into(), have: 5.0, places: vec![model::Place { location: "A".into(), qty: 5.0 }], ..Default::default() }], ..Default::default() };
        let mut e = settled_engine(t0);
        assert!(e.click(mv("x", &[(0, 3)]), t0));
        assert!(e.moving("x") && !e.moving("y"));
        // Shown at once, the total unchanged.
        let v = model::view(&server, e.changes());
        assert_eq!(v.tracked[0].places, vec![model::Place { location: "A".into(), qty: 2.0 }, model::Place { location: "B".into(), qty: 3.0 }]);
        assert_eq!(v.tracked[0].have, 5.0);
        // Not summed like - / +: out at once, one write.
        assert_eq!(e.next_job(t0, no_search), Some(Job::Write(1, mv("x", &[(0, 3)]))));
        // Refused: the places come back.
        assert!(matches!(e.on_write(1, WriteResult::Failed("x".into()), t0), WriteEffect::Failed(_)));
        assert!(!e.moving("x"));
        assert_eq!(model::view(&server, e.changes()), server);
        // A 429 keeps it waiting, shown.
        e.click(mv("x", &[(0, 1)]), t0);
        assert!(matches!(e.next_job(t0, no_search), Some(Job::Write(2, _))));
        assert_eq!(e.on_write(2, WriteResult::Limited(Some(Duration::from_secs(4))), t0), WriteEffect::Nothing);
        assert_eq!(e.changes().count(), 1);
        assert_eq!(e.next_job(t0 + Duration::from_secs(3), no_search), None, "the server's wait");
        assert!(matches!(e.next_job(t0 + Duration::from_secs(4), no_search), Some(Job::Write(2, _))));
        // No answer (or applied in part): kept, until a read says what the server has.
        assert!(matches!(e.on_write(2, WriteResult::Unknown("timeout".into()), t0 + Duration::from_secs(4)), WriteEffect::Unknown(_)));
        assert!(e.moving("x"), "no other move of it until the read");
        let t1 = t0 + Duration::from_secs(4) + READ_AFTER_WRITE_GAP;
        assert_eq!(e.next_job(t1, no_search), Some(Job::Read));
        assert_eq!(e.on_read(Outcome::Good, None, t1).len(), 1);
        assert!(!e.moving("x") && e.changes().count() == 0);
        // Made for another endpoint: never sent, dropped.
        e.click(mv("x", &[(0, 1)]), t1);
        e.next_job(t1, no_search);
        assert_eq!(e.on_write(3, WriteResult::Stale, t1), WriteEffect::Nothing);
        assert!(!e.moving("x"));
    }

    #[test]
    fn the_locations_go_after_the_inventory_within_the_reads_budget() {
        let t0 = Instant::now();
        let mut e = settled_engine(t0);
        assert_eq!(e.next_job_all(t0, || false, || None, || true, || panic!("the locations first")), Some(Job::Locations));
        assert_eq!(e.next_job_all(t0, || false, || None, || panic!("one request at a time"), no_search), None);
        e.on_locations(None, t0);
        assert!(e.idle());
        // A 429 holds back everything, as a read's does.
        assert_eq!(e.next_job_all(t0, || false, || None, || true, no_search), Some(Job::Locations));
        e.on_locations(Some(Limit(Some(Duration::from_secs(6)))), t0);
        assert_eq!(e.locations_ready_in(t0), Duration::from_secs(6));
        assert_eq!(e.next_job_all(t0 + Duration::from_secs(5), || false, || None, || panic!("held back"), no_search), None);
        // No room in the reads' budget: it waits.
        let mut e = Engine::new(t0);
        for i in 0..BUDGET_READS {
            e.budget.record(t0 + ms(i as u64));
        }
        assert_eq!(e.next_job_all(t0 + ms(100), || false, || None, || panic!("no room"), no_search), None);
    }
}

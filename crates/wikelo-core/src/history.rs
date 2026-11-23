//! Version history of the dataset: which contracts / items first appeared in
//! which game build, and what the "novidades" of the latest build are.
//!
//! The store keeps one row per (kind, key, version) across every channel that
//! was ever extracted (LIVE, PTU, ...). The computation here is pure so it can
//! be unit tested without SQLite.
//!
//! Contracts and blueprints also keep a small JSON summary per build ([`ContractSummary`],
//! [`BlueprintSummary`]), so the changelog can say "before → after" ([`build_changelog`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::dataset::{Blueprint, BuildInfo, Contract, Unit, WikeloDataset};

/// `"4.10.197.2878"` → `[4, 10, 197, 2878]`. Non-numeric parts become 0.
pub fn version_key(version: &str) -> Vec<u64> {
    version.split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect()
}

/// The Perforce change number of a build, when `p4_change` is one: 1 to 18 ASCII digits
/// (18 digits always fit in a `u64`). Anything else (empty, too long, a sign, letters) is none.
pub fn p4_change_number(p4_change: &str) -> Option<u64> {
    if (1..=18).contains(&p4_change.len()) && p4_change.bytes().all(|b| b.is_ascii_digit()) { p4_change.parse().ok() } else { None }
}

/// Sort key for "which build is newer", the one ordering the server (which dataset a channel
/// uses) and the validator (what it warns about) share: the Perforce change number (0 when
/// there is none, see [`p4_change_number`]), then the version by its numeric parts.
pub fn build_order(build: &BuildInfo) -> (u64, Vec<u64>) {
    (p4_change_number(&build.p4_change).unwrap_or(0), version_key(&build.version))
}

/// Human label for a build: `sc-alpha-4.10.2` → `4.10.2`; falls back to the
/// first three components of the version string.
pub fn version_label(branch: &str, version: &str) -> String {
    let b = branch.trim();
    if let Some(rest) = b.rsplit_once("alpha-").map(|(_, r)| r).or_else(|| b.rsplit('-').next())
        && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return rest.to_string();
    }
    version.split('.').take(3).collect::<Vec<_>>().join(".")
}

/// Stable 64-bit FNV-1a hash of what matters for "did this contract change".
pub fn contract_fingerprint(c: &Contract) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(c.title.clone());
    for r in &c.requirements {
        parts.push(format!("R:{}:{}:{}:{:?}", r.name, r.qty, r.qty_max, r.unit));
        for o in &r.options {
            for x in o {
                parts.push(format!("RO:{}:{}", x.name, x.qty));
            }
        }
    }
    for r in &c.rewards {
        parts.push(format!("W:{}:{}:{}:{}:{:?}", r.result, r.option, r.name, r.qty, r.kind));
    }
    for g in rep_gained_sorted(c) {
        parts.push(format!("G:{}:{}", g.0, g.1));
    }
    if let Some(rr) = &c.rep_required {
        parts.push(format!("Q:{:?}", rr.min_rep));
    }
    parts.push(format!("O:{}", c.once_only));
    fnv_parts(parts)
}

/// FNV-1a over the parts, each one closed by a 0xff byte, as 16 hex digits. Every fingerprint
/// recorded so far was made this way, so it must not change.
fn fnv_parts(parts: Vec<String>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Fingerprint of a blueprint: its summary plus every alternative of every mandatory slot (the
/// summary keeps only the first), so it changes whenever the summary does.
pub fn blueprint_fingerprint(b: &Blueprint) -> String {
    blueprint_fingerprint_with(b, &serde_json::to_string(&blueprint_summary(b)).unwrap_or_default())
}

fn blueprint_fingerprint_with(b: &Blueprint, summary_json: &str) -> String {
    let mut parts = vec![summary_json.to_string(), format!("N:{:?}", b.slots_required)];
    for s in &b.slots {
        parts.push(format!("S:{}:{}", s.name, s.count));
        for o in &s.options {
            parts.push(format!("SO:{}:{}:{:?}:{}", o.name, o.qty, o.unit, o.min_quality));
        }
    }
    fnv_parts(parts)
}

fn rep_gained_sorted(c: &Contract) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = c.rep_gained.iter().map(|g| (g.scope_name.clone().unwrap_or_default(), g.amount)).collect();
    v.sort();
    v
}

/// One game build that was extracted at some point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildSeen {
    pub version: String,
    pub label: String,
    pub channel: String,
    pub p4_change: String,
    pub seen_at: String,
    pub contracts: usize,
    pub items: usize,
}

/// One (kind, key) observed in one build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SeenRow {
    /// `contract` | `item` | `blueprint` (keyed by the blueprint's record name)
    pub kind: String,
    pub key: String,
    pub version: String,
    pub name: String,
    pub fingerprint: String,
    /// JSON [`ContractSnapshot`] for contracts (empty for items / old rows).
    #[serde(default)]
    pub snapshot: String,
    /// JSON [`ContractSummary`] or [`BlueprintSummary`]; None for items and for rows recorded
    /// before summaries existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// What a contract looked like in one build; enough to explain a change later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContractSnapshot {
    pub title: String,
    pub requirements: Vec<SnapQty>,
    pub rewards: Vec<SnapQty>,
    pub rep_gained: i64,
    pub min_rep: Option<i64>,
    pub once_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnapQty {
    pub name: String,
    pub qty: f64,
    pub unit: String,
}

impl From<&Contract> for ContractSnapshot {
    fn from(c: &Contract) -> Self {
        let mut requirements: Vec<SnapQty> = Vec::new();
        for r in &c.requirements {
            if r.options.is_empty() {
                requirements.push(SnapQty { name: r.name.clone(), qty: r.qty as f64, unit: format!("{:?}", r.unit).to_lowercase() });
            } else {
                for (i, opt) in r.options.iter().enumerate() {
                    for x in opt {
                        requirements.push(SnapQty { name: format!("{} (opção {})", x.name, i + 1), qty: x.qty as f64, unit: format!("{:?}", x.unit).to_lowercase() });
                    }
                }
            }
        }
        let rewards = c
            .rewards
            .iter()
            .map(|r| SnapQty { name: r.name.clone(), qty: r.qty as f64, unit: if r.on_accept { "accept".into() } else { "count".into() } })
            .collect();
        ContractSnapshot {
            title: c.title.clone(),
            requirements,
            rewards,
            rep_gained: c.rep_gained.iter().map(|g| g.amount).sum(),
            min_rep: c.rep_required.as_ref().and_then(|r| r.min_rep),
            once_only: c.once_only,
        }
    }
}

/// One difference between two builds of a contract. `text` is the sentence in Portuguese (the
/// watcher and older clients); the structured fields let the site say it in its own language
/// (web/src/lib/changes.ts), with the item name left as it is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// `requirement` | `reward` | `rep` | `title` | `other`
    pub kind: String,
    pub text: String,
    /// `add` | `remove` | `qty` | `title` | `rep` | `min_rep` | `once`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub op: String,
    /// The item for `add`, `remove` and `qty`, as the snapshot names it (an alternative keeps
    /// its "(opção N)" suffix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What it was: the amount removed, the old quantity, title or reputation. `None` for a
    /// `min_rep` that did not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What it is now: the amount added, the new quantity, title or reputation; `once` or
    /// `repeatable` for `once`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

fn change(kind: &str, op: &str, text: String, name: Option<&str>, from: Option<String>, to: Option<String>) -> Change {
    Change { kind: kind.into(), text, op: op.into(), name: name.map(str::to_string), from, to }
}

fn fmt_qty(q: f64, unit: &str) -> String {
    let n = if q.fract() == 0.0 { format!("{}", q as i64) } else { format!("{q}") };
    if unit == "scu" { format!("{n} SCU") } else { format!("{n}×") }
}

/// Explain how `cur` differs from `prev`.
pub fn diff_snapshots(prev: &ContractSnapshot, cur: &ContractSnapshot) -> Vec<Change> {
    let mut out = Vec::new();
    if prev.title != cur.title {
        out.push(change("title", "title", format!("Título: “{}” → “{}”", prev.title, cur.title), None, Some(prev.title.clone()), Some(cur.title.clone())));
    }
    let diff_list = |kind: &str, a: &[SnapQty], b: &[SnapQty], out: &mut Vec<Change>| {
        let label_add = if kind == "requirement" { "Passou a pedir" } else { "Nova recompensa:" };
        let label_rm = if kind == "requirement" { "Não pede mais" } else { "Deixou de dar" };
        for x in b {
            match a.iter().find(|y| y.name == x.name) {
                None => {
                    let q = fmt_qty(x.qty, &x.unit);
                    out.push(change(kind, "add", format!("{label_add} {q} {}", x.name), Some(&x.name), None, Some(q)));
                }
                Some(y) if (y.qty - x.qty).abs() > 1e-9 => {
                    let (f, t) = (fmt_qty(y.qty, &y.unit), fmt_qty(x.qty, &x.unit));
                    out.push(change(kind, "qty", format!("{}: {f} → {t}", x.name), Some(&x.name), Some(f), Some(t)));
                }
                _ => {}
            }
        }
        for y in a {
            if !b.iter().any(|x| x.name == y.name) {
                let q = fmt_qty(y.qty, &y.unit);
                out.push(change(kind, "remove", format!("{label_rm} {q} {}", y.name), Some(&y.name), Some(q), None));
            }
        }
    };
    diff_list("requirement", &prev.requirements, &cur.requirements, &mut out);
    diff_list("reward", &prev.rewards, &cur.rewards, &mut out);
    if prev.rep_gained != cur.rep_gained {
        out.push(change("rep", "rep", format!("Reputação: +{} → +{}", prev.rep_gained, cur.rep_gained), None, Some(prev.rep_gained.to_string()), Some(cur.rep_gained.to_string())));
    }
    if prev.min_rep != cur.min_rep {
        let f = |v: Option<i64>| v.map(|x| x.to_string()).unwrap_or_else(|| "nenhuma".into());
        out.push(change("rep", "min_rep", format!("Rep mínima: {} → {}", f(prev.min_rep), f(cur.min_rep)), None, prev.min_rep.map(|x| x.to_string()), cur.min_rep.map(|x| x.to_string())));
    }
    if prev.once_only != cur.once_only {
        let text = if cur.once_only { "Passou a ser única" } else { "Deixou de ser única (repetível)" };
        out.push(change("other", "once", text.into(), None, None, Some(if cur.once_only { "once" } else { "repeatable" }.into())));
    }
    out
}

pub fn dataset_rows(ds: &WikeloDataset) -> (BuildSeen, Vec<SeenRow>) {
    let build = BuildSeen {
        version: ds.build.version.clone(),
        label: version_label(&ds.build.branch, &ds.build.version),
        channel: ds.build.channel.clone(),
        p4_change: ds.build.p4_change.clone(),
        seen_at: ds.generated_at.clone(),
        contracts: ds.contracts.len(),
        items: ds.items.len(),
    };
    let mut rows = Vec::with_capacity(ds.contracts.len() + ds.items.len() + ds.blueprints.len());
    for c in &ds.contracts {
        rows.push(SeenRow {
            kind: "contract".into(),
            key: c.id.clone(),
            version: ds.build.version.clone(),
            name: c.title.clone(),
            fingerprint: contract_fingerprint(c),
            snapshot: serde_json::to_string(&ContractSnapshot::from(c)).unwrap_or_default(),
            summary: serde_json::to_string(&contract_summary(c)).ok(),
        });
    }
    for it in &ds.items {
        rows.push(SeenRow {
            kind: "item".into(),
            key: it.id.clone(),
            version: ds.build.version.clone(),
            name: it.name.clone(),
            fingerprint: it.kind.clone(),
            snapshot: String::new(),
            summary: None,
        });
    }
    for b in &ds.blueprints {
        let summary = serde_json::to_string(&blueprint_summary(b)).unwrap_or_default();
        rows.push(SeenRow {
            kind: "blueprint".into(),
            key: b.name.clone(),
            version: ds.build.version.clone(),
            name: if b.output_name.is_empty() { b.name.clone() } else { b.output_name.clone() },
            fingerprint: blueprint_fingerprint_with(b, &summary),
            snapshot: String::new(),
            summary: Some(summary),
        });
    }
    (build, rows)
}

// ----- summaries and the changelog -----------------------------------------------------

/// Longest list a summary keeps (requirements, rewards, materials): a row stays near 2 KB.
pub const SUMMARY_LIST_MAX: usize = 40;
/// Longest item, slot or material name a summary keeps, in characters.
const SUMMARY_NAME_MAX: usize = 80;
/// Most entries of one kind the changelog lists for one build.
pub const CHANGELOG_KIND_MAX: usize = 300;

fn short(s: &str) -> String {
    s.chars().take(SUMMARY_NAME_MAX).collect()
}

fn unit_name(u: Unit) -> &'static str {
    match u {
        Unit::Count => "count",
        Unit::Scu => "scu",
    }
}

/// A quantity in a summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SummaryQty {
    pub name: String,
    pub qty: f64,
    /// Upper bound when the game asks for a range (only when above `qty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty_max: Option<f64>,
    /// `count` or `scu`; empty for rewards (always units).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit: String,
}

/// Reputation a contract pays in one scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRep {
    pub scope: String,
    pub amount: i64,
}

/// What a contract looked like in one build, for the changelog's "before → after".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContractSummary {
    pub title: String,
    /// Every requirement, then the items of each of its options, in the order the fingerprint
    /// reads them (at most [`SUMMARY_LIST_MAX`]).
    #[serde(default)]
    pub requirements: Vec<SummaryQty>,
    #[serde(default)]
    pub rewards: Vec<SummaryQty>,
    /// Minimum reputation asked, when the contract asks for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rep_required: Option<i64>,
    /// Reputation paid, per scope, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rep_gained: Vec<SummaryRep>,
    #[serde(default)]
    pub once_only: bool,
}

/// One ingredient of a blueprint summary: the first option of a mandatory slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SummaryMaterial {
    pub slot: String,
    pub name: String,
    pub qty: f64,
    pub unit: String,
    #[serde(default)]
    pub min_quality: i64,
    /// How many alternatives the slot offers, when more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<usize>,
}

/// What a blueprint looked like in one build, for the changelog's "before → after".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintSummary {
    pub output_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub craft_time_sec: f64,
    #[serde(default)]
    pub materials: Vec<SummaryMaterial>,
    /// How many ways to obtain it the data lists.
    #[serde(default)]
    pub sources: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chance_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chance_max: Option<f64>,
}

pub fn contract_summary(c: &Contract) -> ContractSummary {
    let mut requirements = Vec::new();
    for r in &c.requirements {
        requirements.push(SummaryQty {
            name: short(&r.name),
            qty: r.qty as f64,
            qty_max: (r.qty_max > r.qty).then_some(r.qty_max as f64),
            unit: unit_name(r.unit).into(),
        });
        for x in r.options.iter().flatten() {
            requirements.push(SummaryQty { name: short(&x.name), qty: x.qty as f64, qty_max: None, unit: unit_name(x.unit).into() });
        }
    }
    requirements.truncate(SUMMARY_LIST_MAX);
    let rewards = c
        .rewards
        .iter()
        .take(SUMMARY_LIST_MAX)
        .map(|r| SummaryQty { name: short(&r.name), qty: r.qty as f64, qty_max: None, unit: String::new() })
        .collect();
    ContractSummary {
        title: c.title.clone(),
        requirements,
        rewards,
        rep_required: c.rep_required.as_ref().and_then(|r| r.min_rep),
        rep_gained: rep_gained_sorted(c).into_iter().map(|(scope, amount)| SummaryRep { scope, amount }).collect(),
        once_only: c.once_only,
    }
}

pub fn blueprint_summary(b: &Blueprint) -> BlueprintSummary {
    let materials = b
        .slots
        .iter()
        .filter_map(|s| {
            let first = s.options.first()?;
            Some(SummaryMaterial {
                slot: short(&s.name),
                name: short(&first.name),
                qty: first.qty,
                unit: unit_name(first.unit).into(),
                min_quality: first.min_quality,
                options: (s.options.len() > 1).then_some(s.options.len()),
            })
        })
        .take(SUMMARY_LIST_MAX)
        .collect();
    let chance_min = b.sources.iter().map(|s| s.chance).reduce(f64::min);
    let chance_max = b.sources.iter().map(|s| s.chance).reduce(f64::max);
    BlueprintSummary {
        output_name: short(&b.output_name),
        category: b.category.clone(),
        craft_time_sec: b.craft_time_sec,
        materials,
        sources: b.sources.len(),
        chance_min,
        chance_max,
    }
}

/// A number as the changelog prints it: at most 4 decimals, trailing zeros cut; below 0.001,
/// 4 significant digits, so a small amount never reads 0.
pub fn fmt_amount(q: f64) -> String {
    if !q.is_finite() {
        return q.to_string();
    }
    let a = q.abs();
    let decimals = if a == 0.0 || a >= 0.001 { 4 } else { (3 - a.log10().floor() as i64).clamp(4, 12) as usize };
    let s = format!("{q:.decimals$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// `5×`, `2–4×`, `0.25 SCU`.
fn fmt_quantity(q: f64, max: Option<f64>, unit: &str) -> String {
    let n = match max {
        Some(m) => format!("{}–{}", fmt_amount(q), fmt_amount(m)),
        None => fmt_amount(q),
    };
    if unit == "scu" { format!("{n} SCU") } else { format!("{n}×") }
}

/// One field that differs between two builds: `before` is None when it did not exist then,
/// `after` when it does not exist now. Both are ready to print.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDetail {
    /// Stable machine name: `title`, `requirement:<name>`, `reward:<name>`, `reputation`,
    /// `minReputation`, `onceOnly` (`yes`/`no`), `outputName`, `category`, `craftTimeSec`,
    /// `material:<slot>`, `sources`.
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

fn diff_value(field: &str, before: Option<String>, after: Option<String>, out: &mut Vec<ChangeDetail>) {
    if before != after {
        out.push(ChangeDetail { field: field.into(), before, after });
    }
}

/// `(key, display)` pairs with entries sharing a key merged (" / " between them), in the order
/// the keys first appear.
fn keyed(entries: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in entries {
        if let Some(e) = out.iter_mut().find(|(ek, _)| *ek == k) {
            e.1.push_str(" / ");
            e.1.push_str(&v);
        } else {
            out.push((k, v));
        }
    }
    out
}

fn diff_keyed(prefix: &str, before: &[(String, String)], after: &[(String, String)], out: &mut Vec<ChangeDetail>) {
    for (k, v) in after {
        let old = before.iter().find(|(bk, _)| bk == k).map(|(_, bv)| bv.clone());
        diff_value(&format!("{prefix}:{k}"), old, Some(v.clone()), out);
    }
    for (k, v) in before {
        if !after.iter().any(|(ak, _)| ak == k) {
            out.push(ChangeDetail { field: format!("{prefix}:{k}"), before: Some(v.clone()), after: None });
        }
    }
}

fn requirement_list(s: &ContractSummary) -> Vec<(String, String)> {
    keyed(s.requirements.iter().map(|q| (q.name.clone(), fmt_quantity(q.qty, q.qty_max, &q.unit))))
}

fn reward_list(s: &ContractSummary) -> Vec<(String, String)> {
    keyed(s.rewards.iter().map(|q| (q.name.clone(), fmt_quantity(q.qty, None, "count"))))
}

/// `+100`, or `+100 BarterTrade · +50 Other` when the contract pays several scopes.
fn fmt_rep(g: &[SummaryRep]) -> Option<String> {
    match g {
        [] => None,
        [one] => Some(format!("{:+}", one.amount)),
        many => Some(many.iter().map(|r| format!("{:+} {}", r.amount, r.scope)).collect::<Vec<_>>().join(" · ")),
    }
}

fn yes_no(v: bool) -> String {
    if v { "yes".into() } else { "no".into() }
}

/// How `b` differs from `a`, field by field.
pub fn diff_contract_summaries(a: &ContractSummary, b: &ContractSummary) -> Vec<ChangeDetail> {
    let mut out = Vec::new();
    diff_value("title", Some(a.title.clone()), Some(b.title.clone()), &mut out);
    diff_keyed("requirement", &requirement_list(a), &requirement_list(b), &mut out);
    diff_keyed("reward", &reward_list(a), &reward_list(b), &mut out);
    diff_value("reputation", fmt_rep(&a.rep_gained), fmt_rep(&b.rep_gained), &mut out);
    diff_value("minReputation", a.rep_required.map(|v| v.to_string()), b.rep_required.map(|v| v.to_string()), &mut out);
    diff_value("onceOnly", Some(yes_no(a.once_only)), Some(yes_no(b.once_only)), &mut out);
    out
}

/// `Iron 0.02 SCU Q≥500 (+2 alt.)`.
fn fmt_material(m: &SummaryMaterial) -> String {
    let mut s = format!("{} {}", m.name, fmt_quantity(m.qty, None, &m.unit));
    if m.min_quality > 0 {
        s.push_str(&format!(" Q≥{}", m.min_quality));
    }
    if let Some(n) = m.options {
        s.push_str(&format!(" (+{} alt.)", n.saturating_sub(1)));
    }
    s
}

fn material_list(s: &BlueprintSummary) -> Vec<(String, String)> {
    keyed(s.materials.iter().map(|m| (m.slot.clone(), fmt_material(m))))
}

/// `3 (12.5%–50%)`, `1 (25%)`, `0`.
fn fmt_sources(s: &BlueprintSummary) -> String {
    let pct = |c: f64| format!("{}%", fmt_amount(c * 100.0));
    match (s.chance_min, s.chance_max) {
        (Some(lo), Some(hi)) if (hi - lo).abs() < 1e-9 => format!("{} ({})", s.sources, pct(lo)),
        (Some(lo), Some(hi)) => format!("{} ({}–{})", s.sources, pct(lo), pct(hi)),
        _ => s.sources.to_string(),
    }
}

/// How `b` differs from `a`, field by field.
pub fn diff_blueprint_summaries(a: &BlueprintSummary, b: &BlueprintSummary) -> Vec<ChangeDetail> {
    let mut out = Vec::new();
    diff_value("outputName", Some(a.output_name.clone()), Some(b.output_name.clone()), &mut out);
    diff_value("category", a.category.clone(), b.category.clone(), &mut out);
    diff_value("craftTimeSec", Some(format!("{} s", fmt_amount(a.craft_time_sec))), Some(format!("{} s", fmt_amount(b.craft_time_sec))), &mut out);
    diff_keyed("material", &material_list(a), &material_list(b), &mut out);
    diff_value("sources", Some(fmt_sources(a)), Some(fmt_sources(b)), &mut out);
    out
}

/// One contract or blueprint that came, changed or went in a build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogEntry {
    pub key: String,
    pub name: String,
    /// `added` | `changed` | `removed`
    pub change: String,
    /// Blueprints only: their category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// For `changed`, when both builds have a summary; empty otherwise.
    #[serde(default)]
    pub details: Vec<ChangeDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct KindCounts {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    /// More than [`CHANGELOG_KIND_MAX`] entries: the list has the first ones (changed, then
    /// added, then removed); the counts are the full ones.
    pub truncated: bool,
    /// Nothing compared: this build or the previous one has no row of this kind (the oldest
    /// build, or an extractor that did not write the kind yet).
    pub untracked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogCounts {
    pub contracts: KindCounts,
    pub blueprints: KindCounts,
    /// Items in this build the previous one did not have (items are not listed).
    pub items_added: usize,
}

/// The changelog of one build against the previous build of the same channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogBuild {
    pub version: String,
    pub label: String,
    /// When it was extracted (`BuildSeen::seen_at`).
    pub first_seen: String,
    /// The build the channel serves now.
    pub current: bool,
    /// The oldest build recorded for the channel: nothing before it to compare with.
    pub first: bool,
    pub counts: ChangelogCounts,
    pub contracts: Vec<ChangelogEntry>,
    pub blueprints: Vec<ChangelogEntry>,
}

fn has_summary(r: &SeenRow) -> Option<&str> {
    r.summary.as_deref().filter(|s| !s.is_empty())
}

fn entry_details(kind: &str, old: &SeenRow, new: &SeenRow) -> Vec<ChangeDetail> {
    let (Some(a), Some(b)) = (has_summary(old), has_summary(new)) else { return Vec::new() };
    match kind {
        "contract" => match (serde_json::from_str::<ContractSummary>(a), serde_json::from_str::<ContractSummary>(b)) {
            (Ok(a), Ok(b)) => diff_contract_summaries(&a, &b),
            _ => Vec::new(),
        },
        "blueprint" => match (serde_json::from_str::<BlueprintSummary>(a), serde_json::from_str::<BlueprintSummary>(b)) {
            (Ok(a), Ok(b)) => diff_blueprint_summaries(&a, &b),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn changelog_entry(kind: &str, row: &SeenRow, change: &str, details: Vec<ChangeDetail>) -> ChangelogEntry {
    let category = if kind == "blueprint" {
        has_summary(row).and_then(|s| serde_json::from_str::<BlueprintSummary>(s).ok()).and_then(|s| s.category)
    } else {
        None
    };
    ChangelogEntry { key: row.key.clone(), name: row.name.clone(), change: change.into(), category, details }
}

/// Entries of one kind: added (key absent before), changed (fingerprint differs), removed (key
/// absent now), each sorted by name, changed first, cut at [`CHANGELOG_KIND_MAX`].
fn diff_kind(kind: &str, prev: Option<&[SeenRow]>, cur: &[SeenRow]) -> (Vec<ChangelogEntry>, KindCounts) {
    let now: HashMap<&str, &SeenRow> = cur.iter().filter(|r| r.kind == kind).map(|r| (r.key.as_str(), r)).collect();
    let before: HashMap<&str, &SeenRow> = prev.unwrap_or_default().iter().filter(|r| r.kind == kind).map(|r| (r.key.as_str(), r)).collect();
    let mut counts = KindCounts::default();
    if prev.is_none() || now.is_empty() || before.is_empty() {
        counts.untracked = true;
        return (Vec::new(), counts);
    }
    let (mut changed, mut added, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    for (key, row) in &now {
        match before.get(key) {
            None => added.push(changelog_entry(kind, row, "added", Vec::new())),
            Some(old) if old.fingerprint != row.fingerprint => changed.push(changelog_entry(kind, row, "changed", entry_details(kind, old, row))),
            Some(_) => {}
        }
    }
    for (key, row) in &before {
        if !now.contains_key(key) {
            removed.push(changelog_entry(kind, row, "removed", Vec::new()));
        }
    }
    counts.added = added.len();
    counts.changed = changed.len();
    counts.removed = removed.len();
    for list in [&mut changed, &mut added, &mut removed] {
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.key.cmp(&b.key)));
    }
    let mut all: Vec<ChangelogEntry> = changed.into_iter().chain(added).chain(removed).collect();
    if all.len() > CHANGELOG_KIND_MAX {
        all.truncate(CHANGELOG_KIND_MAX);
        counts.truncated = true;
    }
    (all, counts)
}

/// The changelog of `build` from its rows and the rows of the previous build of the same
/// channel (`None`: it is the oldest one recorded).
pub fn build_changelog(build: &BuildSeen, current: bool, prev: Option<&[SeenRow]>, cur: &[SeenRow]) -> ChangelogBuild {
    let (contracts, contract_counts) = diff_kind("contract", prev, cur);
    let (blueprints, blueprint_counts) = diff_kind("blueprint", prev, cur);
    let items_added = match prev {
        Some(p) => {
            let before: HashSet<&str> = p.iter().filter(|r| r.kind == "item").map(|r| r.key.as_str()).collect();
            let now: HashSet<&str> = cur.iter().filter(|r| r.kind == "item").map(|r| r.key.as_str()).collect();
            if before.is_empty() { 0 } else { now.difference(&before).count() }
        }
        None => 0,
    };
    ChangelogBuild {
        version: build.version.clone(),
        label: build.label.clone(),
        first_seen: build.seen_at.clone(),
        current,
        first: prev.is_none(),
        counts: ChangelogCounts { contracts: contract_counts, blueprints: blueprint_counts, items_added },
        contracts,
        blueprints,
    }
}

/// The builds recorded for `channel` (case does not matter), newest first.
pub fn channel_builds(builds: &[BuildSeen], channel: &str) -> Vec<BuildSeen> {
    let mut out: Vec<BuildSeen> = builds.iter().filter(|b| b.channel.eq_ignore_ascii_case(channel)).cloned().collect();
    out.sort_by(|a, b| {
        version_key(&b.version)
            .cmp(&version_key(&a.version))
            .then_with(|| b.p4_change.parse::<u64>().unwrap_or(0).cmp(&a.p4_change.parse::<u64>().unwrap_or(0)))
    });
    out
}

/// The changelog of a channel: `builds` newest first (as [`channel_builds`] gives them),
/// `rows` by version (only the builds listed and the one before the last are read), at most
/// `limit` builds. The oldest build of the channel gets no entries.
pub fn channel_changelog(builds: &[BuildSeen], rows: &HashMap<String, Vec<SeenRow>>, current_version: &str, limit: usize) -> Vec<ChangelogBuild> {
    builds
        .iter()
        .enumerate()
        .take(limit)
        .map(|(i, b)| {
            let cur = rows.get(&b.version).map(Vec::as_slice).unwrap_or_default();
            // A previous build whose rows were not read compares as untracked, never as "all new".
            let prev = builds.get(i + 1).map(|p| rows.get(&p.version).map(Vec::as_slice).unwrap_or_default());
            build_changelog(b, b.version == current_version, prev, cur)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangedContract {
    pub id: String,
    pub title: String,
    /// Label of the last build where it looked different.
    pub since: String,
    /// What changed; empty when the previous build was recorded without a snapshot.
    #[serde(default)]
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemovedEntry {
    pub kind: String,
    pub key: String,
    pub name: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewEntry {
    pub kind: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WhatsNew {
    pub current_version: String,
    pub current_label: String,
    /// Earliest build ever recorded (what "new" is measured against).
    pub baseline_label: Option<String>,
    /// True once at least two distinct builds are known.
    pub has_history: bool,
    /// Every build recorded, oldest first.
    pub builds: Vec<BuildSeen>,
    /// Label of the most recent build (≤ current) that introduced something.
    pub latest_novelty_label: Option<String>,
    pub latest_novelty_version: Option<String>,
    /// Contract id → label of the build where it first appeared.
    pub first_seen: HashMap<String, String>,
    /// Contracts and items that appeared in `latest_novelty_version`.
    pub new_contracts: Vec<NewEntry>,
    pub new_items: Vec<NewEntry>,
    /// Contracts whose requirements/rewards differ from the previous recorded build.
    pub changed_contracts: Vec<ChangedContract>,
    /// Known before, absent from the current build.
    pub removed: Vec<RemovedEntry>,
}

/// One recorded observation of a key in a build.
#[derive(Clone)]
struct SeenVersion {
    version: String,
    name: String,
    fingerprint: String,
    snapshot: String,
}

/// (kind, key) -> version key -> observation
type SeenMap = HashMap<(String, String), BTreeMap<Vec<u64>, SeenVersion>>;

/// Compute the novelties for `current` (a dataset) from the recorded history.
pub fn compute_whats_new(builds: &[BuildSeen], rows: &[SeenRow], current: &WikeloDataset) -> WhatsNew {
    let cur_key = version_key(&current.build.version);
    let mut builds: Vec<BuildSeen> = builds.to_vec();
    builds.sort_by(|a, b| version_key(&a.version).cmp(&version_key(&b.version)).then_with(|| a.p4_change.cmp(&b.p4_change)));
    let label_of: HashMap<&str, &str> = builds.iter().map(|b| (b.version.as_str(), b.label.as_str())).collect();
    let label = |v: &str| label_of.get(v).map(|s| s.to_string()).unwrap_or_else(|| v.to_string());

    // Only builds up to the one being viewed count (PTU ahead of LIVE must not leak).
    let considered: Vec<&BuildSeen> = builds.iter().filter(|b| version_key(&b.version) <= cur_key).collect();
    let baseline = considered.first().map(|b| b.version.clone());
    let previous = considered.iter().rev().find(|b| version_key(&b.version) < cur_key).map(|b| b.version.clone());

    // (kind,key) -> version key -> (version, name, fingerprint)
    let mut seen: SeenMap = HashMap::new();
    for r in rows {
        // The novelties speak of contracts and items only; blueprint rows (the changelog's)
        // would otherwise all read as removed items.
        if r.kind != "contract" && r.kind != "item" {
            continue;
        }
        let vk = version_key(&r.version);
        if vk > cur_key {
            continue;
        }
        seen.entry((r.kind.clone(), r.key.clone())).or_default().insert(
            vk,
            SeenVersion { version: r.version.clone(), name: r.name.clone(), fingerprint: r.fingerprint.clone(), snapshot: r.snapshot.clone() },
        );
    }

    let mut out = WhatsNew {
        current_version: current.build.version.clone(),
        current_label: version_label(&current.build.branch, &current.build.version),
        baseline_label: baseline.as_deref().map(label),
        has_history: considered.len() >= 2,
        builds: builds.clone(),
        ..Default::default()
    };
    if !out.has_history {
        return out;
    }
    let baseline_key = version_key(baseline.as_deref().unwrap_or_default());

    // First-seen per key and the latest build that introduced a contract.
    let mut first_seen_version: HashMap<(String, String), Vec<u64>> = HashMap::new();
    let mut latest_novelty: Option<Vec<u64>> = None;
    for (k, versions) in &seen {
        let first = versions.keys().next().cloned().unwrap_or_default();
        if first > baseline_key && k.0 == "contract" {
            // Only contracts that still exist in the current build count as novelties.
            if current.contracts.iter().any(|c| c.id == k.1) && latest_novelty.as_ref().is_none_or(|l| first > *l) {
                latest_novelty = Some(first.clone());
            }
        }
        first_seen_version.insert(k.clone(), first);
    }
    for c in &current.contracts {
        if let Some(v) = seen.get(&("contract".to_string(), c.id.clone())).and_then(|m| m.values().next()) {
            out.first_seen.insert(c.id.clone(), label(&v.version));
        }
    }

    if let Some(nv) = latest_novelty {
        let nv_str = seen.values().find_map(|m| m.get(&nv).map(|v| v.version.clone())).unwrap_or_default();
        out.latest_novelty_label = Some(label(&nv_str));
        out.latest_novelty_version = Some(nv_str);
        for c in &current.contracts {
            if first_seen_version.get(&("contract".to_string(), c.id.clone())) == Some(&nv) {
                out.new_contracts.push(NewEntry { kind: "contract".into(), key: c.id.clone(), name: c.title.clone() });
            }
        }
        for it in &current.items {
            if first_seen_version.get(&("item".to_string(), it.id.clone())) == Some(&nv) {
                out.new_items.push(NewEntry { kind: "item".into(), key: it.id.clone(), name: it.name.clone() });
            }
        }
        out.new_contracts.sort_by(|a, b| a.name.cmp(&b.name));
        out.new_items.sort_by(|a, b| a.name.cmp(&b.name));
    }

    // Changed: fingerprint differs from the previous recorded build where it existed.
    for c in &current.contracts {
        let Some(versions) = seen.get(&("contract".to_string(), c.id.clone())) else { continue };
        let now_fp = contract_fingerprint(c);
        if let Some((_, prev)) = versions.iter().rev().find(|(k, _)| **k < cur_key)
            && prev.fingerprint != now_fp
        {
            let changes = serde_json::from_str::<ContractSnapshot>(&prev.snapshot)
                .map(|p| diff_snapshots(&p, &ContractSnapshot::from(c)))
                .unwrap_or_default();
            out.changed_contracts.push(ChangedContract { id: c.id.clone(), title: c.title.clone(), since: label(&prev.version), changes });
        }
    }
    out.changed_contracts.sort_by(|a, b| a.title.cmp(&b.title));

    // Removed: seen before, not in the current dataset.
    let _ = previous;
    let cur_contracts: HashMap<&str, ()> = current.contracts.iter().map(|c| (c.id.as_str(), ())).collect();
    let cur_items: HashMap<&str, ()> = current.items.iter().map(|i| (i.id.as_str(), ())).collect();
    for ((kind, key), versions) in &seen {
        let present = match kind.as_str() {
            "contract" => cur_contracts.contains_key(key.as_str()),
            _ => cur_items.contains_key(key.as_str()),
        };
        if present {
            continue;
        }
        if let Some((_, last)) = versions.iter().next_back() {
            out.removed.push(RemovedEntry { kind: kind.clone(), key: key.clone(), name: last.name.clone(), last_seen: label(&last.version) });
        }
    }
    out.removed.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::dataset::*;

    fn ds(version: &str, branch: &str, contracts: &[(&str, &str, i64)], items: &[&str]) -> WikeloDataset {
        WikeloDataset {
            schema_version: 1,
            source: "test".into(),
            build: BuildInfo { channel: "LIVE".into(), branch: branch.into(), version: version.into(), ..Default::default() },
            generated_at: "2026-01-01T00:00:00Z".into(),
            faction: Faction::default(),
            emporiums: vec![],
            contracts: contracts
                .iter()
                .map(|(id, title, qty)| Contract {
                    id: id.to_string(),
                    title: title.to_string(),
                    requirements: vec![Requirement { name: "Favor".into(), qty: *qty, ..Default::default() }],
                    ..Default::default()
                })
                .collect(),
            items: items.iter().map(|i| Item { id: i.to_string(), name: i.to_string(), ..Default::default() }).collect(),
            favor_conversions: vec![],
            crafting: vec![],
            warnings: vec![],
            mission_rewards: vec![],
            mission_drops: vec![],
            missions: vec![],
            locations: vec![],
            quality_tiers: vec![],
            blueprints: vec![],
            extractor: None,
        }
    }

    #[test]
    fn version_helpers() {
        assert_eq!(version_key("4.10.197.2878"), vec![4, 10, 197, 2878]);
        assert!(version_key("4.10.197.2878") > version_key("4.9.185.24066"));
        assert_eq!(version_label("sc-alpha-4.10.2", "4.10.197.2878"), "4.10.2");
        assert_eq!(version_label("", "4.10.197.2878"), "4.10.197");
    }

    #[test]
    fn build_order_takes_only_real_change_numbers() {
        assert_eq!(p4_change_number("12913470"), Some(12_913_470));
        assert_eq!(p4_change_number(&"9".repeat(18)), Some(999_999_999_999_999_999));
        let long = "1".repeat(19);
        for bad in ["", long.as_str(), "12a4", "+12", "-1", " 12", "１２"] {
            assert_eq!(p4_change_number(bad), None, "{bad:?}");
        }
        let build = |version: &str, p4: &str| BuildInfo { version: version.into(), p4_change: p4.into(), ..Default::default() };
        // The change number decides first, then the version by its numbers (not as text).
        assert!(build_order(&build("4.9.0.1", "200")) > build_order(&build("5.0.0.1", "100")));
        assert!(build_order(&build("4.10.0.1", "100")) > build_order(&build("4.9.0.1", "100")));
        // A change number that is not one counts as none, so it never outranks a real one.
        assert!(build_order(&build("5.0.0.1", "9".repeat(20).as_str())) < build_order(&build("4.9.0.1", "1")));
        assert!(build_order(&build("5.0.0.1", "12x")) < build_order(&build("4.9.0.1", "1")));
    }

    #[test]
    fn novelties_between_two_builds() {
        let old = ds("4.9.185.24066", "sc-alpha-4.9.0", &[("a", "A", 1), ("b", "B", 1), ("gone", "Gone", 1)], &["i1"]);
        let new = ds("4.10.197.2878", "sc-alpha-4.10.2", &[("a", "A", 1), ("b", "B", 5), ("c", "C", 1)], &["i1", "i2"]);
        let (b1, r1) = dataset_rows(&old);
        let (b2, r2) = dataset_rows(&new);
        let rows: Vec<SeenRow> = r1.into_iter().chain(r2).collect();
        let wn = compute_whats_new(&[b1.clone(), b2.clone()], &rows, &new);
        assert!(wn.has_history);
        assert_eq!(wn.baseline_label.as_deref(), Some("4.9.0"));
        assert_eq!(wn.latest_novelty_label.as_deref(), Some("4.10.2"));
        assert_eq!(wn.new_contracts.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), vec!["c"]);
        assert_eq!(wn.new_items.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), vec!["i2"]);
        assert_eq!(wn.changed_contracts.len(), 1);
        assert_eq!(wn.changed_contracts[0].id, "b");
        assert_eq!(wn.changed_contracts[0].changes.len(), 1);
        assert_eq!(wn.changed_contracts[0].changes[0].text, "Favor: 1× → 5×");
        let ch = &wn.changed_contracts[0].changes[0];
        assert_eq!((ch.kind.as_str(), ch.op.as_str(), ch.name.as_deref(), ch.from.as_deref(), ch.to.as_deref()), ("requirement", "qty", Some("Favor"), Some("1×"), Some("5×")));
        assert_eq!(wn.removed.len(), 1);
        assert_eq!(wn.removed[0].key, "gone");
        assert_eq!(wn.first_seen.get("c").map(String::as_str), Some("4.10.2"));
        assert_eq!(wn.first_seen.get("a").map(String::as_str), Some("4.9.0"));

        // A later patch with nothing new keeps pointing at 4.10.2.
        let patch = ds("4.10.250.1", "sc-alpha-4.10.3", &[("a", "A", 1), ("b", "B", 5), ("c", "C", 1)], &["i1", "i2"]);
        let (b3, r3) = dataset_rows(&patch);
        let rows: Vec<SeenRow> = rows.into_iter().chain(r3).collect();
        let wn = compute_whats_new(&[b1.clone(), b2.clone(), b3], &rows, &patch);
        assert_eq!(wn.latest_novelty_label.as_deref(), Some("4.10.2"));
        assert_eq!(wn.new_contracts.len(), 1);
        assert!(wn.changed_contracts.is_empty());

        // Viewing the old build must not see the future.
        let wn_old = compute_whats_new(&[b1, b2], &rows, &old);
        assert!(!wn_old.has_history);
    }

    /// A rifle blueprint: one SCU slot with two alternatives, one source at 25 %.
    fn bp(name: &str, craft: f64) -> Blueprint {
        let input = |name: &str, qty: f64, min_quality: i64| CraftInput { kind: "resource".into(), name: name.into(), qty, unit: Unit::Scu, min_quality, ..Default::default() };
        Blueprint {
            id: format!("g-{name}"),
            name: name.into(),
            category: Some("FPSWeapons".into()),
            output_class_name: name.into(),
            output_name: format!("Out {name}"),
            craft_time_sec: craft,
            slots: vec![BlueprintSlot { name: "Frame".into(), count: 1, options: vec![input("Iron", 0.02, 500), input("Tin", 0.03, 0)], ..Default::default() }],
            sources: vec![BlueprintSource { kind: "contract".into(), contract_id: "c1".into(), title: "T".into(), when: "complete".into(), pool: "P".into(), chance: 0.25, ..Default::default() }],
            ..Default::default()
        }
    }

    fn row(kind: &str, key: &str, fingerprint: &str, summary: Option<String>) -> SeenRow {
        SeenRow { kind: kind.into(), key: key.into(), version: "1".into(), name: key.into(), fingerprint: fingerprint.into(), snapshot: String::new(), summary }
    }

    #[test]
    fn amounts_are_printed_short() {
        assert_eq!(fmt_amount(5.0), "5");
        assert_eq!(fmt_amount(0.33333), "0.3333");
        assert_eq!(fmt_amount(1234.5), "1234.5");
        assert_eq!(fmt_amount(0.00002), "0.00002", "a small amount keeps its digits");
        assert_eq!(fmt_amount(0.0), "0");
        assert_eq!(fmt_amount(-0.0), "0");
        assert_eq!(fmt_quantity(2.0, Some(4.0), "count"), "2–4×");
        assert_eq!(fmt_quantity(0.25, None, "scu"), "0.25 SCU");
    }

    #[test]
    fn contract_summary_flattens_options_and_moves_with_the_fingerprint() {
        let c = Contract {
            id: "c".into(),
            title: "Ship".into(),
            requirements: vec![
                Requirement { name: "Favor".into(), qty: 2, qty_max: 4, ..Default::default() },
                Requirement {
                    kind: RequirementKind::Or,
                    name: "One of".into(),
                    qty: 1,
                    qty_max: 1,
                    options: vec![vec![Requirement { name: "Ore".into(), qty: 3, unit: Unit::Scu, ..Default::default() }]],
                    ..Default::default()
                },
            ],
            rewards: vec![Reward { result: 0, option: 0, weight: 1.0, owner_only: true, item_id: None, class_name: None, name: "Wolf".into(), qty: 1, kind: RewardKind::Vehicle, on_accept: false }],
            rep_gained: vec![RepGain { scope_id: None, scope_name: Some("BarterTrade".into()), amount: 100 }],
            rep_required: Some(RepRequirement { min_rep: Some(340), ..Default::default() }),
            ..Default::default()
        };
        let s = contract_summary(&c);
        let names: Vec<&str> = s.requirements.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(names, ["Favor", "One of", "Ore"]);
        assert_eq!((s.requirements[0].qty_max, s.requirements[1].qty_max), (Some(4.0), None));
        assert_eq!(s.requirements[2].unit, "scu");
        assert_eq!((s.rewards.len(), s.rep_required, s.rep_gained.len()), (1, Some(340), 1));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""rewards":[{"name":"Wolf","qty":1.0}]"#), "rewards carry no unit: {json}");

        let mut more = c.clone();
        more.requirements[1].options[0][0].qty = 5;
        assert_ne!(contract_summary(&more), s);
        assert_ne!(contract_fingerprint(&more), contract_fingerprint(&c), "the fingerprint moves with the summary");
        let d = diff_contract_summaries(&s, &contract_summary(&more));
        assert_eq!(d, vec![ChangeDetail { field: "requirement:Ore".into(), before: Some("3 SCU".into()), after: Some("5 SCU".into()) }]);

        let mut other = c.clone();
        other.title = "Ship II".into();
        other.once_only = true;
        other.rep_required = None;
        other.rep_gained[0].amount = 50;
        other.rewards.clear();
        let fields: Vec<(String, Option<String>, Option<String>)> = diff_contract_summaries(&s, &contract_summary(&other)).into_iter().map(|d| (d.field, d.before, d.after)).collect();
        let f = |a: &str, b: Option<&str>, c: Option<&str>| (a.to_string(), b.map(str::to_string), c.map(str::to_string));
        assert_eq!(
            fields,
            vec![
                f("title", Some("Ship"), Some("Ship II")),
                f("reward:Wolf", Some("1×"), None),
                f("reputation", Some("+100"), Some("+50")),
                f("minReputation", Some("340"), None),
                f("onceOnly", Some("no"), Some("yes")),
            ]
        );
    }

    #[test]
    fn blueprint_summary_keeps_the_first_option_and_the_chance_range() {
        let mut b = bp("BP_rifle", 90.0);
        let likelier = BlueprintSource { chance: 0.5, ..b.sources[0].clone() };
        b.sources.push(likelier);
        let s = blueprint_summary(&b);
        assert_eq!(s.output_name, "Out BP_rifle");
        assert_eq!(s.materials.len(), 1);
        assert_eq!((s.materials[0].name.as_str(), s.materials[0].min_quality, s.materials[0].options), ("Iron", 500, Some(2)));
        assert_eq!((s.sources, s.chance_min, s.chance_max), (2, Some(0.25), Some(0.5)));
        assert_eq!(fmt_material(&s.materials[0]), "Iron 0.02 SCU Q≥500 (+1 alt.)");
        assert_eq!(fmt_sources(&s), "2 (25%–50%)");

        let fp = blueprint_fingerprint(&b);
        let mut slower = b.clone();
        slower.craft_time_sec = 120.0;
        assert_ne!(blueprint_fingerprint(&slower), fp);
        assert_eq!(
            diff_blueprint_summaries(&s, &blueprint_summary(&slower)),
            vec![ChangeDetail { field: "craftTimeSec".into(), before: Some("90 s".into()), after: Some("120 s".into()) }]
        );
        // An alternative the summary does not show still moves the fingerprint.
        let mut alt = b.clone();
        alt.slots[0].options[1].qty = 0.04;
        assert_eq!(blueprint_summary(&alt), s);
        assert_ne!(blueprint_fingerprint(&alt), fp);
        // Fewer sources, another material.
        let mut poorer = b.clone();
        poorer.sources.truncate(1);
        poorer.slots[0].options.truncate(1);
        poorer.slots[0].options[0].name = "Gold".into();
        let d = diff_blueprint_summaries(&s, &blueprint_summary(&poorer));
        assert_eq!(d.iter().map(|d| d.field.as_str()).collect::<Vec<_>>(), ["material:Frame", "sources"]);
        assert_eq!(d[0].after.as_deref(), Some("Gold 0.02 SCU Q≥500"));
        assert_eq!(d[1].after.as_deref(), Some("1 (25%)"));
    }

    #[test]
    fn summaries_stay_small() {
        let c = Contract {
            title: "Big".into(),
            requirements: (0..100).map(|i| Requirement { name: format!("Item {i:03}"), qty: i, ..Default::default() }).collect(),
            ..Default::default()
        };
        let s = contract_summary(&c);
        assert_eq!(s.requirements.len(), SUMMARY_LIST_MAX);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.len() < 2048, "{} bytes", json.len());
        let long = Requirement { name: "x".repeat(500), qty: 1, ..Default::default() };
        let s = contract_summary(&Contract { requirements: vec![long], ..Default::default() });
        assert_eq!(s.requirements[0].name.chars().count(), SUMMARY_NAME_MAX);
    }

    #[test]
    fn changelog_lists_what_came_changed_and_went() {
        let mut old = ds("4.9.185.24066", "sc-alpha-4.9.0", &[("a", "A", 1), ("b", "B", 1), ("gone", "Gone", 1)], &["i1"]);
        old.blueprints = vec![bp("BP_a", 90.0), bp("BP_gone", 10.0)];
        let mut new = ds("4.10.197.2878", "sc-alpha-4.10.2", &[("a", "A", 1), ("b", "B", 5), ("c", "C", 1)], &["i1", "i2"]);
        new.blueprints = vec![bp("BP_a", 120.0), bp("BP_new", 10.0)];
        let (_, r1) = dataset_rows(&old);
        let (b2, r2) = dataset_rows(&new);
        assert!(r2.iter().any(|r| r.kind == "blueprint" && r.key == "BP_a" && r.name == "Out BP_a" && r.summary.is_some()));
        assert!(r2.iter().filter(|r| r.kind == "item").all(|r| r.summary.is_none()));

        let cl = build_changelog(&b2, true, Some(&r1), &r2);
        assert_eq!((cl.label.as_str(), cl.current, cl.first, cl.counts.items_added), ("4.10.2", true, false, 1));
        assert_eq!(cl.counts.contracts, KindCounts { added: 1, changed: 1, removed: 1, ..Default::default() });
        assert_eq!(cl.counts.blueprints, KindCounts { added: 1, changed: 1, removed: 1, ..Default::default() });
        fn summary(e: &[ChangelogEntry]) -> Vec<(String, String)> {
            e.iter().map(|e| (e.key.clone(), e.change.clone())).collect()
        }
        fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
            v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        }
        assert_eq!(summary(&cl.contracts), pairs(&[("b", "changed"), ("c", "added"), ("gone", "removed")]));
        assert_eq!(cl.contracts[0].details, vec![ChangeDetail { field: "requirement:Favor".into(), before: Some("1×".into()), after: Some("5×".into()) }]);
        assert!(cl.contracts.iter().all(|e| e.category.is_none()));
        assert_eq!(summary(&cl.blueprints), pairs(&[("BP_a", "changed"), ("BP_new", "added"), ("BP_gone", "removed")]));
        assert_eq!(cl.blueprints[0].details[0].field, "craftTimeSec");
        assert_eq!(cl.blueprints[2].category.as_deref(), Some("FPSWeapons"), "a removed blueprint keeps its category");
        assert!(cl.blueprints[1].details.is_empty() && cl.blueprints[2].details.is_empty());

        let json = serde_json::to_value(&cl).unwrap();
        assert_eq!(json["counts"]["itemsAdded"], 1);
        assert_eq!(json["contracts"][0]["details"][0], serde_json::json!({ "field": "requirement:Favor", "before": "1×", "after": "5×" }));
        assert!(json["contracts"][0].get("category").is_none());
    }

    #[test]
    fn a_change_without_the_previous_summary_has_no_details() {
        let old = ds("4.9.185.24066", "sc-alpha-4.9.0", &[("b", "B", 1)], &[]);
        let new = ds("4.10.197.2878", "sc-alpha-4.10.2", &[("b", "B", 5)], &[]);
        let (_, mut r1) = dataset_rows(&old);
        for r in &mut r1 {
            r.summary = None; // recorded before summaries existed
        }
        let (b2, r2) = dataset_rows(&new);
        let cl = build_changelog(&b2, false, Some(&r1), &r2);
        assert_eq!(cl.contracts.len(), 1);
        assert_eq!(cl.contracts[0].change, "changed");
        assert!(cl.contracts[0].details.is_empty());
        assert!(cl.counts.blueprints.untracked, "no blueprint rows on either side");
    }

    #[test]
    fn the_oldest_build_and_kinds_without_rows_are_not_compared() {
        let mut new = ds("4.10.197.2878", "sc-alpha-4.10.2", &[("a", "A", 1)], &["i1"]);
        new.blueprints = vec![bp("BP_a", 90.0)];
        let (b2, r2) = dataset_rows(&new);
        let first = build_changelog(&b2, true, None, &r2);
        assert!(first.first && first.counts.contracts.untracked && first.counts.blueprints.untracked);
        assert!(first.contracts.is_empty() && first.blueprints.is_empty() && first.counts.items_added == 0);

        // The build before was recorded without blueprints: they are not all "new".
        let old = ds("4.9.185.24066", "sc-alpha-4.9.0", &[("a", "A", 1)], &["i1"]);
        let (_, r1) = dataset_rows(&old);
        let cl = build_changelog(&b2, true, Some(&r1), &r2);
        assert!(!cl.first && !cl.counts.contracts.untracked && cl.counts.blueprints.untracked);
        assert!(cl.blueprints.is_empty());
    }

    #[test]
    fn a_long_list_is_cut_changed_first() {
        let prev: Vec<SeenRow> = (0..10).map(|i| row("contract", &format!("old{i:03}"), "x", None)).chain([row("contract", "z-same", "1", None)]).collect();
        let cur: Vec<SeenRow> = (0..CHANGELOG_KIND_MAX + 50).map(|i| row("contract", &format!("new{i:03}"), "x", None)).chain([row("contract", "z-same", "2", None)]).collect();
        let build = BuildSeen { version: "1".into(), label: "1".into(), channel: "LIVE".into(), p4_change: String::new(), seen_at: String::new(), contracts: 0, items: 0 };
        let cl = build_changelog(&build, false, Some(&prev), &cur);
        assert_eq!(cl.counts.contracts, KindCounts { added: CHANGELOG_KIND_MAX + 50, changed: 1, removed: 10, truncated: true, untracked: false });
        assert_eq!(cl.contracts.len(), CHANGELOG_KIND_MAX);
        assert_eq!((cl.contracts[0].key.as_str(), cl.contracts[0].change.as_str()), ("z-same", "changed"));
        assert!(cl.contracts[1..].iter().all(|e| e.change == "added"), "the removed ones are past the cut");
    }

    #[test]
    fn a_channel_changelog_is_newest_first_and_limited() {
        let builds: Vec<BuildSeen> = [("4.9.0.1", "LIVE"), ("4.10.0.1", "LIVE"), ("4.10.1.1", "PTU"), ("4.10.2.1", "live")]
            .iter()
            .map(|(v, ch)| BuildSeen { version: v.to_string(), label: v.to_string(), channel: ch.to_string(), p4_change: String::new(), seen_at: String::new(), contracts: 0, items: 0 })
            .collect();
        let live = channel_builds(&builds, "LIVE");
        assert_eq!(live.iter().map(|b| b.version.as_str()).collect::<Vec<_>>(), ["4.10.2.1", "4.10.0.1", "4.9.0.1"]);
        let mut rows = HashMap::new();
        for b in &live {
            rows.insert(b.version.clone(), vec![SeenRow { version: b.version.clone(), ..row("contract", "a", &b.version, None) }]);
        }
        let all = channel_changelog(&live, &rows, "4.10.2.1", 50);
        assert_eq!(all.len(), 3);
        assert!(all[0].current && !all[1].current);
        assert_eq!((all[0].counts.contracts.changed, all[1].counts.contracts.changed), (1, 1));
        assert!(all[2].first && !all[1].first);
        let two = channel_changelog(&live, &rows, "4.10.2.1", 2);
        assert_eq!(two.len(), 2);
        assert!(!two[1].first, "the build past the limit still serves as its previous one");
    }

    #[test]
    fn whats_new_ignores_blueprint_rows() {
        let mut old = ds("4.9.185.24066", "sc-alpha-4.9.0", &[("a", "A", 1)], &["i1"]);
        old.blueprints = vec![bp("BP_a", 90.0)];
        let mut new = ds("4.10.197.2878", "sc-alpha-4.10.2", &[("a", "A", 1)], &["i1"]);
        new.blueprints = vec![bp("BP_a", 90.0)];
        let (b1, r1) = dataset_rows(&old);
        let (b2, r2) = dataset_rows(&new);
        let rows: Vec<SeenRow> = r1.into_iter().chain(r2).collect();
        let wn = compute_whats_new(&[b1, b2], &rows, &new);
        assert!(wn.has_history);
        assert!(wn.removed.is_empty(), "{:?}", wn.removed);
    }
}

//! The craft simulator's arithmetic: which quality tiers belong to a material, the value its
//! tiers give a raw quality, the quality a slot is computed at, the value one quality effect
//! gives at one quality, and the combination over a blueprint's mandatory slots into what the
//! crafted item gets. Pure: no I/O.
//!
//! The game data does not say how slots combine: multipliers of the same property multiply
//! together and integers add up, which is an estimate, not the game's own rule.

use std::collections::HashMap;

use crate::dataset::{Blueprint, QualityBand, QualityEffect, QualityRange, QualityRangeKind, QualityTiers};
use crate::validate::MAX_QUALITY;

/// The quality a material is assumed to have when nothing says otherwise (the tier holding it,
/// or this number when the material has no tiers).
pub const QUALITY_DEFAULT: i64 = 500;

/// Rounded to the nearest integer with halves going up (2.5 → 3, -2.5 → -2), as JavaScript's
/// `Math.round` does, so a browser computing the same thing gets the same numbers; never -0.
fn js_round(v: f64) -> f64 {
    (v + 0.5).floor() + 0.0
}

/// A multiplier rounded to 4 decimals.
pub fn round_multiplier(m: f64) -> f64 {
    js_round(m * 10_000.0) / 10_000.0
}

/// A blueprint someone can get in this build: a default one, or one with a source that is not
/// disabled.
pub fn blueprint_obtainable(bp: &Blueprint) -> bool {
    bp.default || bp.sources.iter().any(|s| !s.disabled)
}

// ----- quality tiers ---------------------------------------------------------------------

/// A name as the tier lookup compares it: letters and digits only, case ignored
/// ("Raw Agricium" → "rawagricium").
pub fn material_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The quality tiers of every material, looked up by a material's name: first against the
/// tiers' `name`, then against their `key`, both compared by [`material_key`]; the first
/// entry wins when two compare equal.
pub struct QualityTierIndex<'a> {
    by_key: HashMap<String, &'a QualityTiers>,
}

impl<'a> QualityTierIndex<'a> {
    pub fn new(all: &'a [QualityTiers]) -> Self {
        let mut by_key: HashMap<String, &'a QualityTiers> = HashMap::with_capacity(all.len() * 2);
        // Names first, so a name always wins over another material's key.
        for t in all {
            let k = material_key(&t.name);
            if !k.is_empty() {
                by_key.entry(k).or_insert(t);
            }
        }
        for t in all {
            let k = material_key(&t.key);
            if !k.is_empty() {
                by_key.entry(k).or_insert(t);
            }
        }
        Self { by_key }
    }

    /// The tiers of `material`; None when no tiers match it.
    pub fn find(&self, material: &str) -> Option<&'a QualityTiers> {
        let k = material_key(material);
        if k.is_empty() {
            return None;
        }
        self.by_key.get(&k).copied()
    }
}

/// The band holding the raw quality `quality`, else the nearest one (the lower one on a tie);
/// None when the material has no bands.
pub fn tier_band(tiers: &QualityTiers, quality: i64) -> Option<&QualityBand> {
    let mut best: Option<(&QualityBand, i64)> = None;
    for b in &tiers.bands {
        let d = if quality < b.start {
            b.start.saturating_sub(quality)
        } else if quality > b.end {
            quality.saturating_sub(b.end)
        } else {
            0
        };
        let better = match best {
            None => true,
            Some((cur, cur_d)) => d < cur_d || (d == cur_d && b.start < cur.start),
        };
        if better {
            best = Some((b, d));
        }
    }
    best.map(|(b, _)| b)
}

/// What the game rounds a raw quality to: the value of [`tier_band`]; the raw quality itself
/// when the material has no bands.
pub fn tier_value(tiers: &QualityTiers, quality: i64) -> i64 {
    tier_band(tiers, quality).map_or(quality, |b| b.value)
}

/// The quality assumed for a material nobody says anything about: among the tiers whose value
/// reaches `min_quality`, the one whose band holds [`QUALITY_DEFAULT`], else the lowest of
/// them; without tiers (or none reaching the minimum), `QUALITY_DEFAULT` or the minimum when it
/// is higher, at most [`MAX_QUALITY`].
pub fn assumed_quality(tiers: Option<&QualityTiers>, min_quality: i64) -> i64 {
    let min = min_quality.max(0);
    if let Some(t) = tiers {
        let allowed: Vec<&QualityBand> = t.bands.iter().filter(|b| b.value >= min).collect();
        let holding = allowed.iter().copied().filter(|b| (b.start..=b.end).contains(&QUALITY_DEFAULT)).min_by_key(|b| b.start);
        if let Some(b) = holding.or_else(|| allowed.iter().copied().min_by_key(|b| b.start)) {
            return b.value;
        }
    }
    min.clamp(QUALITY_DEFAULT, MAX_QUALITY)
}

/// The quality one slot is computed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialQuality {
    /// After the material's tiers.
    pub quality: i64,
    /// No stack with a known quality of at least the minimum: the value is [`assumed_quality`].
    pub assumed: bool,
}

/// The quality of a material from the qualities of the stacks held (0 is "not informed" and
/// does not count): the highest one that reaches `min_quality`, through the material's tiers;
/// [`assumed_quality`] when there is none.
pub fn material_quality(tiers: Option<&QualityTiers>, held: impl IntoIterator<Item = i64>, min_quality: i64) -> MaterialQuality {
    match held.into_iter().filter(|q| *q > 0 && *q >= min_quality).max() {
        Some(q) => MaterialQuality { quality: tiers.map_or(q, |t| tier_value(t, q)), assumed: false },
        None => MaterialQuality { quality: assumed_quality(tiers, min_quality), assumed: true },
    }
}

// ----- quality effects -------------------------------------------------------------------

/// What one quality effect gives at one quality: a multiplier (`Linear`) or an integer added
/// (`IntegerAdditive`, already rounded).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectValue {
    pub kind: QualityRangeKind,
    pub value: f64,
}

/// The value of one effect at `quality`: the first range (in the record's order) holding it,
/// interpolated linearly from `at_start` to `at_end`; outside every range (a gap, below the
/// first, above the last), the nearest range's nearest end. A range whose start equals its end
/// gives `at_start`; an integer added is rounded to the nearest integer. None when the effect
/// has no range.
pub fn effect_value_at(effect: &QualityEffect, quality: i64) -> Option<EffectValue> {
    let q = quality as f64;
    let mut best: Option<&QualityRange> = None;
    let mut best_d = f64::INFINITY;
    for r in &effect.ranges {
        let (s, e) = (r.start_quality as f64, r.end_quality as f64);
        let (lo, hi) = (s.min(e), s.max(e));
        let d = if q < lo {
            lo - q
        } else if q > hi {
            q - hi
        } else {
            0.0
        };
        if d < best_d {
            best = Some(r);
            best_d = d;
            if d <= 0.0 {
                break;
            }
        }
    }
    let r = best?;
    let start = r.start_quality as f64;
    let span = r.end_quality as f64 - start;
    // Clamped to the range: a quality outside it takes the nearer end.
    let at = if span == 0.0 { 0.0 } else { ((q - start) / span).clamp(0.0, 1.0) };
    let v = r.at_start + (r.at_end - r.at_start) * at;
    let value = match r.kind {
        QualityRangeKind::IntegerAdditive => js_round(v),
        QualityRangeKind::Linear => v,
    };
    Some(EffectValue { kind: r.kind, value })
}

/// One slot as the combination reads it: what its quality changes and the quality it is at.
#[derive(Debug, Clone, Copy)]
pub struct CraftPart<'a> {
    pub effects: &'a [QualityEffect],
    pub quality: i64,
}

/// One gameplay property of the crafted item, combined over every slot.
#[derive(Debug, Clone, PartialEq)]
pub struct CraftTotal {
    pub key: String,
    /// The property's name as the game writes it, or its key when no record names it.
    pub name: String,
    /// Every slot's multiplier multiplied together; None when no slot gives one.
    pub multiplier: Option<f64>,
    /// Every slot's integer added up; None when no slot gives one.
    pub added: Option<i64>,
}

/// What the crafted item gets, one entry per property, in the order the slots first name them:
/// multipliers of a property multiply together and its integers add up (a property can have
/// both). Effects without any range are left out.
pub fn combine_craft(parts: &[CraftPart<'_>]) -> Vec<CraftTotal> {
    let mut out: Vec<CraftTotal> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for p in parts {
        for e in p.effects {
            let Some(v) = effect_value_at(e, p.quality) else { continue };
            let named = e.name.as_deref().filter(|n| !n.is_empty());
            let i = match at.get(&e.key) {
                Some(&i) => {
                    // A later record that names the property names the line.
                    if out[i].name == e.key
                        && let Some(n) = named
                    {
                        out[i].name = n.to_string();
                    }
                    i
                }
                None => {
                    out.push(CraftTotal { key: e.key.clone(), name: named.unwrap_or(e.key.as_str()).to_string(), multiplier: None, added: None });
                    at.insert(e.key.clone(), out.len() - 1);
                    out.len() - 1
                }
            };
            let line = &mut out[i];
            match v.kind {
                QualityRangeKind::IntegerAdditive => line.added = Some(line.added.unwrap_or(0).saturating_add(v.value as i64)),
                QualityRangeKind::Linear => line.multiplier = Some(line.multiplier.unwrap_or(1.0) * v.value),
            }
        }
    }
    out
}

/// [`combine_craft`] over the blueprint's mandatory slots that have options and quality
/// effects, each at `quality(slot index)`; a slot for which it answers None is left out.
pub fn blueprint_craft(bp: &Blueprint, quality: impl Fn(usize) -> Option<i64>) -> Vec<CraftTotal> {
    let parts: Vec<CraftPart<'_>> = bp
        .slots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.options.is_empty() && !s.quality_effects.is_empty())
        .filter_map(|(i, s)| Some(CraftPart { effects: &s.quality_effects, quality: quality(i)? }))
        .collect();
    combine_craft(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{BlueprintSlot, BlueprintSource, CraftInput};

    fn range(kind: QualityRangeKind, start: i64, end: i64, at_start: f64, at_end: f64) -> QualityRange {
        QualityRange { kind, start_quality: start, end_quality: end, at_start, at_end }
    }

    fn lin(start: i64, end: i64, at_start: f64, at_end: f64) -> QualityRange {
        range(QualityRangeKind::Linear, start, end, at_start, at_end)
    }

    fn add(start: i64, end: i64, at_start: f64, at_end: f64) -> QualityRange {
        range(QualityRangeKind::IntegerAdditive, start, end, at_start, at_end)
    }

    fn effect(key: &str, name: Option<&str>, ranges: Vec<QualityRange>) -> QualityEffect {
        QualityEffect { key: key.into(), name: name.map(str::to_string), ranges }
    }

    fn value(e: &QualityEffect, q: i64) -> f64 {
        effect_value_at(e, q).expect("a range").value
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Agricium's eight bands as the game has them.
    fn agricium() -> QualityTiers {
        let bands = [(0, 399, 346), (400, 599, 588), (600, 699, 667), (700, 799, 796), (800, 899, 852), (900, 949, 943), (950, 998, 971), (999, 1000, 1000)]
            .into_iter()
            .map(|(start, end, value)| QualityBand { start, end, value })
            .collect();
        QualityTiers { key: "Agricium".into(), name: "Agricium".into(), bands }
    }

    #[test]
    fn inside_a_range_the_value_is_interpolated() {
        let e = effect("k", None, vec![lin(0, 1000, 0.9, 1.1)]);
        assert!(close(value(&e, 500), 1.0));
        assert!(close(value(&e, 0), 0.9));
        assert!(close(value(&e, 1000), 1.1));
        assert!(close(value(&e, 250), 0.95));
    }

    #[test]
    fn a_boundary_shared_by_two_ranges_takes_the_first() {
        let e = effect("k", None, vec![lin(0, 500, 1.0, 2.0), lin(500, 1000, 3.0, 4.0)]);
        assert!(close(value(&e, 500), 2.0), "the first range in the record's order");
        assert!(close(value(&e, 501), 3.002));
    }

    #[test]
    fn in_a_gap_the_nearest_range_gives_its_nearest_end() {
        let e = effect("k", None, vec![lin(0, 400, 1.0, 2.0), lin(600, 1000, 3.0, 4.0)]);
        assert!(close(value(&e, 450), 2.0), "nearer the first range's end");
        assert!(close(value(&e, 550), 3.0), "nearer the second range's start");
        assert!(close(value(&e, 500), 2.0), "as near both: the first one");
    }

    #[test]
    fn below_the_first_and_above_the_last_range_the_ends_hold() {
        let e = effect("k", None, vec![lin(200, 400, 1.0, 2.0), lin(400, 800, 2.0, 5.0)]);
        assert!(close(value(&e, 100), 1.0));
        assert!(close(value(&e, 0), 1.0));
        assert!(close(value(&e, 900), 5.0));
        assert!(close(value(&e, 1000), 5.0));
    }

    #[test]
    fn a_zero_width_range_gives_its_start() {
        let e = effect("k", None, vec![lin(500, 500, 1.5, 2.0)]);
        assert!(close(value(&e, 500), 1.5));
        assert!(close(value(&e, 700), 1.5), "the nearest range, even outside it");
        assert!(close(value(&e, 0), 1.5));
    }

    #[test]
    fn a_reversed_range_is_read_as_written() {
        let e = effect("k", None, vec![lin(1000, 0, 2.0, 1.0)]);
        assert!(close(value(&e, 1000), 2.0));
        assert!(close(value(&e, 250), 1.25));
    }

    #[test]
    fn an_effect_without_ranges_gives_nothing() {
        assert_eq!(effect_value_at(&effect("k", None, vec![]), 500), None);
    }

    #[test]
    fn integers_are_rounded_with_halves_going_up() {
        // 1.5 → 2 and -0.5 → 0 (halves go up), never -0.
        assert_eq!(value(&effect("k", None, vec![add(0, 1000, 0.0, 3.0)]), 500), 2.0);
        let half_down = value(&effect("k", None, vec![add(0, 1000, -1.0, 0.0)]), 500);
        assert_eq!(half_down, 0.0);
        assert!(half_down.is_sign_positive());
        assert_eq!(value(&effect("k", None, vec![add(0, 1000, -3.0, 0.0)]), 500), -1.0, "-1.5 → -1");
        assert_eq!(round_multiplier(1.234_56), 1.2346);
        assert_eq!(round_multiplier(0.999_94), 0.9999);
    }

    #[test]
    fn an_integer_additive_property_adds_up_across_two_slots() {
        let a = [effect("gpp_ammo", Some("Ammo"), vec![add(0, 1000, 0.0, 4.0)])];
        let b = [effect("gpp_ammo", None, vec![add(0, 1000, -1.0, -1.0)])];
        let out = combine_craft(&[CraftPart { effects: &a, quality: 500 }, CraftPart { effects: &b, quality: 900 }]);
        assert_eq!(out, vec![CraftTotal { key: "gpp_ammo".into(), name: "Ammo".into(), multiplier: None, added: Some(1) }]);
    }

    #[test]
    fn multipliers_multiply_across_slots_in_first_seen_order() {
        let a = [effect("recoil", None, vec![lin(0, 1000, 1.1, 1.1)]), effect("spread", Some("Spread"), vec![lin(0, 1000, 0.5, 0.5)])];
        let b = [effect("recoil", Some("Recoil"), vec![lin(0, 1000, 1.2, 1.2)])];
        let out = combine_craft(&[CraftPart { effects: &a, quality: 500 }, CraftPart { effects: &b, quality: 500 }]);
        assert_eq!(out.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(), vec!["recoil", "spread"]);
        assert_eq!(out[0].name, "Recoil", "the key until a record names it");
        assert!(close(out[0].multiplier.unwrap(), 1.32));
        assert_eq!(out[0].added, None);
        assert_eq!(out[1].name, "Spread");
    }

    #[test]
    fn a_property_can_have_both_kinds() {
        let a = [effect("p", Some("Power"), vec![lin(0, 1000, 1.0, 2.0)])];
        let b = [effect("p", None, vec![add(0, 1000, 0.0, 10.0)])];
        let out = combine_craft(&[CraftPart { effects: &a, quality: 250 }, CraftPart { effects: &b, quality: 250 }]);
        assert_eq!(out.len(), 1);
        assert!(close(out[0].multiplier.unwrap(), 1.25));
        assert_eq!(out[0].added, Some(3), "2.5 → 3");
    }

    #[test]
    fn effects_without_ranges_and_blueprints_without_effects_give_no_entries() {
        let none = [effect("k", Some("K"), vec![])];
        assert!(combine_craft(&[CraftPart { effects: &none, quality: 500 }]).is_empty());
        assert!(combine_craft(&[]).is_empty());
        let bp = Blueprint {
            slots: vec![BlueprintSlot { name: "Frame".into(), count: 1, options: vec![CraftInput { name: "Iron".into(), ..Default::default() }], ..Default::default() }],
            ..Default::default()
        };
        assert!(blueprint_craft(&bp, |_| Some(500)).is_empty());
    }

    #[test]
    fn a_blueprint_combines_its_mandatory_slots_with_options_and_effects() {
        let slot = |effects: Vec<QualityEffect>, options: usize| BlueprintSlot {
            name: "S".into(),
            count: 1,
            options: (0..options).map(|_| CraftInput { name: "Iron".into(), ..Default::default() }).collect(),
            quality_effects: effects,
            ..Default::default()
        };
        let bp = Blueprint {
            slots: vec![
                slot(vec![effect("k", None, vec![lin(0, 1000, 1.0, 2.0)])], 1),
                // No options: no material, no quality.
                slot(vec![effect("k", None, vec![lin(0, 1000, 10.0, 10.0)])], 0),
                slot(vec![effect("k", None, vec![lin(0, 1000, 1.0, 3.0)])], 2),
                // The caller has no quality for it.
                slot(vec![effect("k", None, vec![lin(0, 1000, 7.0, 7.0)])], 1),
            ],
            optional_slots: vec![slot(vec![effect("k", None, vec![lin(0, 1000, 5.0, 5.0)])], 1)],
            ..Default::default()
        };
        let out = blueprint_craft(&bp, |i| match i {
            0 => Some(500),
            2 => Some(1000),
            _ => None,
        });
        assert_eq!(out.len(), 1);
        assert!(close(out[0].multiplier.unwrap(), 1.5 * 3.0), "optional slots stay out");
    }

    #[test]
    fn a_tier_band_holds_the_quality_or_is_the_nearest() {
        let t = agricium();
        assert_eq!(tier_value(&t, 650), 667);
        assert_eq!(tier_value(&t, 399), 346);
        assert_eq!(tier_value(&t, 400), 588);
        assert_eq!(tier_value(&t, 1000), 1000);
        let gapped = QualityTiers { key: "X".into(), name: "X".into(), bands: vec![QualityBand { start: 100, end: 200, value: 150 }, QualityBand { start: 300, end: 400, value: 350 }] };
        assert_eq!(tier_value(&gapped, 50), 150, "below every band: the lowest");
        assert_eq!(tier_value(&gapped, 2000), 350, "above every band: the highest");
        assert_eq!(tier_value(&gapped, 250), 150, "in a gap, as near both: the lower");
        assert_eq!(tier_value(&gapped, 260), 350);
        let empty = QualityTiers { key: "E".into(), name: "E".into(), bands: vec![] };
        assert_eq!(tier_band(&empty, 500), None);
        assert_eq!(tier_value(&empty, 640), 640, "no bands: the raw number");
    }

    #[test]
    fn the_tier_lookup_reads_names_first_then_keys() {
        let all = vec![
            QualityTiers { key: "JumpingLimes".into(), name: "Jumping Limes".into(), bands: vec![] },
            QualityTiers { key: "Agricium".into(), name: "Raw Agricium".into(), bands: vec![] },
            QualityTiers { key: "RawAgricium".into(), name: "Other".into(), bands: vec![] },
        ];
        let index = QualityTierIndex::new(&all);
        assert_eq!(index.find("jumping-limes").map(|t| t.key.as_str()), Some("JumpingLimes"));
        assert_eq!(index.find("Raw Agricium").map(|t| t.key.as_str()), Some("Agricium"), "a name beats another one's key");
        assert_eq!(index.find("agricium").map(|t| t.key.as_str()), Some("Agricium"), "by key");
        assert_eq!(index.find("Other").map(|t| t.key.as_str()), Some("RawAgricium"));
        assert!(index.find("Iron").is_none());
        assert!(index.find(" - ").is_none());
        assert_eq!(material_key("Raw Agricium (Ore)"), "rawagriciumore");
    }

    #[test]
    fn a_held_stack_gives_the_quality_through_the_tiers() {
        let t = agricium();
        assert_eq!(material_quality(Some(&t), [0, 300, 874], 500), MaterialQuality { quality: 852, assumed: false }, "the highest stack reaching the minimum");
        assert_eq!(material_quality(None, [0, 300, 874], 500), MaterialQuality { quality: 874, assumed: false }, "no tiers: the raw number");
        assert_eq!(material_quality(Some(&t), [120], 0), MaterialQuality { quality: 346, assumed: false });
    }

    #[test]
    fn without_a_known_sufficient_stack_the_quality_is_assumed() {
        let t = agricium();
        // The band holding 500 (400-599 → 588).
        assert_eq!(material_quality(Some(&t), [0, 300], 500), MaterialQuality { quality: 588, assumed: true });
        assert_eq!(material_quality(Some(&t), std::iter::empty::<i64>(), 0), MaterialQuality { quality: 588, assumed: true });
        // That band is below the minimum: the lowest one that reaches it.
        assert_eq!(material_quality(Some(&t), Vec::<i64>::new(), 600), MaterialQuality { quality: 667, assumed: true });
        // No tiers: 500, or the minimum when higher.
        assert_eq!(material_quality(None, [0], 0), MaterialQuality { quality: 500, assumed: true });
        assert_eq!(material_quality(None, [650], 700), MaterialQuality { quality: 700, assumed: true });
        // No tier reaches the minimum: the raw number, at most 1000.
        assert_eq!(assumed_quality(Some(&t), 2000), 1000);
        assert_eq!(assumed_quality(Some(&QualityTiers { key: "E".into(), name: "E".into(), bands: vec![] }), 0), 500);
        // No band holds 500: the lowest allowed one.
        let high = QualityTiers { key: "H".into(), name: "H".into(), bands: vec![QualityBand { start: 800, end: 1000, value: 900 }, QualityBand { start: 600, end: 799, value: 700 }] };
        assert_eq!(assumed_quality(Some(&high), 0), 700);
    }

    #[test]
    fn obtainable_is_default_or_a_source_that_is_not_disabled() {
        let source = |disabled: bool| BlueprintSource { title: "T".into(), chance: 0.5, disabled, ..Default::default() };
        assert!(!blueprint_obtainable(&Blueprint::default()));
        assert!(blueprint_obtainable(&Blueprint { default: true, ..Default::default() }));
        assert!(!blueprint_obtainable(&Blueprint { sources: vec![source(true)], ..Default::default() }));
        assert!(blueprint_obtainable(&Blueprint { sources: vec![source(true), source(false)], ..Default::default() }));
    }
}

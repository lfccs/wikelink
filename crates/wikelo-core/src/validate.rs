//! Reproducible identity of a dataset and semantic checks on it.
//!
//! The extractor is deterministic, so two players on the same game build produce the same
//! dataset. [`canonical_hash`] names that content independently of machine-specific fields
//! and of vector order, so anyone with the game can reproduce it; [`validate_dataset`]
//! reports what is inconsistent in a dataset (errors) or looks odd (warnings).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dataset::*;
use crate::history::{build_order, contract_fingerprint, p4_change_number, version_key};

/// Bumped whenever the canonical form changes (older hashes stop being comparable).
pub const HASH_VERSION: &str = "v1";

pub const MAX_CONTRACTS: usize = 2000;
pub const MAX_ITEMS: usize = 20_000;
pub const MAX_CRAFTING: usize = 5000;
pub const MAX_MISSION_REWARDS: usize = 20_000;
pub const MAX_MISSION_DROPS: usize = 20_000;
pub const MAX_MISSIONS: usize = 20_000;
pub const MAX_LOCATIONS: usize = 5000;
pub const MAX_WARNINGS: usize = 2000;
/// The game has about 1600; room for growth, not for a hostile bundle.
pub const MAX_BLUEPRINTS: usize = 10_000;
/// Sources over every blueprint (a pool lists up to ~20 blueprints, ~1100 results draw from pools).
pub const MAX_BLUEPRINT_SOURCES: usize = 200_000;
/// Per blueprint: slots, options per slot, effects per slot, multipliers per slot.
pub const MAX_BLUEPRINT_PARTS: usize = 64;
/// Quality bands of one effect (the game's have up to five).
pub const MAX_QUALITY_RANGES: usize = 32;
/// Sources of one blueprint.
pub const MAX_SOURCES_PER_BLUEPRINT: usize = 5000;
/// Every blueprint together, serialized as JSON (the game's come to about 4.6 MB).
pub const MAX_BLUEPRINTS_BYTES: usize = 16 * 1024 * 1024;
/// One year; the game's craft times are minutes.
pub const MAX_CRAFT_TIME_SEC: f64 = 31_536_000.0;
/// Largest ingredient quantity, modifier or multiplier taken as real (the game's are below 100).
pub const MAX_BLUEPRINT_VALUE: f64 = 1_000_000.0;
/// The game's quality scale.
pub const MAX_QUALITY: i64 = 1000;
/// Materials with quality tiers (the game has 63).
pub const MAX_QUALITY_TIERS: usize = 400;
/// Bands of one material's quality tiers (the game's have eight).
pub const MAX_QUALITY_BANDS: usize = 16;
/// Longest key or name of a material's quality tiers.
pub const MAX_QUALITY_TIER_TEXT: usize = 64;
/// Location ids are game class names (`Pyro1_ASD_Monorail_LazarusTransportHub_Phoenix_1A`).
const MAX_LOCATION_ID: usize = 128;
const MAX_SHORT: usize = 200;
const MAX_TEXT: usize = 8000;
const MAX_ID: usize = 64;
/// Largest quantity a favor conversion may name (the game's are small integers: 50 scrip, 1 favor).
pub const MAX_CONVERSION_QTY: i64 = 1_000_000_000;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ----- canonical hash ---------------------------------------------------------------

/// `v1:<sha256>` of the dataset's content: same game files + same extractor build give
/// the same hash on any machine. Leaves out what is machine- or run-specific
/// (`generatedAt`, p4k size, channel folder, warnings, provenance, chosen icon files)
/// and sorts every list whose order carries no meaning.
pub fn canonical_hash(ds: &WikeloDataset) -> String {
    let c = canonicalize(ds);
    let bytes = serde_json::to_vec(&c).unwrap_or_default();
    format!("{HASH_VERSION}:{}", sha256_hex(&bytes))
}

fn canonicalize(ds: &WikeloDataset) -> WikeloDataset {
    let mut c = ds.clone();
    c.source.clear();
    c.generated_at.clear();
    c.build.p4k_size = None;
    c.build.channel.clear();
    c.warnings.clear();
    c.extractor = None;
    c.faction.scopes.sort_by(|a, b| a.id.cmp(&b.id));
    c.emporiums.sort_by(|a, b| a.key.cmp(&b.key));
    for e in &mut c.emporiums {
        e.class_names.sort();
    }
    c.contracts.sort_by(|a, b| a.id.cmp(&b.id));
    for ct in &mut c.contracts {
        ct.aliases.sort();
        ct.emporium_keys.sort();
        ct.completion_tags.sort_by(|a, b| a.id.cmp(&b.id));
        ct.prerequisites.required_tags.sort_by(|a, b| a.id.cmp(&b.id));
        ct.prerequisites.excluded_tags.sort_by(|a, b| a.id.cmp(&b.id));
        ct.prerequisites.required_contract_ids.sort();
        ct.offered_in_build = None;
    }
    c.items.sort_by(|a, b| a.id.cmp(&b.id));
    for it in &mut c.items {
        it.image = None;
        it.image_source = None;
    }
    c.favor_conversions.sort_by(|a, b| a.contract_id.cmp(&b.contract_id));
    c.crafting.sort_by(|a, b| a.blueprint_id.cmp(&b.blueprint_id));
    for r in &mut c.crafting {
        r.sources.sort_by(|a, b| (&a.contract_id, &a.when).cmp(&(&b.contract_id, &b.when)));
    }
    c.mission_rewards.sort_by(|a, b| (&a.contract_id, &a.item_id).cmp(&(&b.contract_id, &b.item_id)));
    c.mission_drops.sort_by(|a, b| (&a.contract_id, &a.item_id).cmp(&(&b.contract_id, &b.item_id)));
    c.missions.sort_by(|a, b| a.id.cmp(&b.id));
    c.locations.sort_by(|a, b| a.id.cmp(&b.id));
    // Slots, options and ranges keep the record's order, which means something; sources do not.
    c.blueprints.sort_by(|a, b| a.id.cmp(&b.id));
    for bp in &mut c.blueprints {
        bp.sources.sort_by(|a, b| (&a.contract_id, &a.when, &a.pool).cmp(&(&b.contract_id, &b.when, &b.pool)).then(a.chance.total_cmp(&b.chance)));
    }
    // The materials' order carries no meaning; their bands' does.
    c.quality_tiers.sort_by(|a, b| a.key.cmp(&b.key));
    c
}

// ----- validation -------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub contracts: usize,
    pub items: usize,
    pub with_image: usize,
    pub crafting: usize,
    pub mission_rewards: usize,
    /// Absent in reports written before the field existed.
    #[serde(default)]
    pub mission_drops: usize,
    #[serde(default)]
    pub missions: usize,
    #[serde(default)]
    pub locations: usize,
    #[serde(default)]
    pub blueprints: usize,
    pub warnings: usize,
}

/// Outcome of [`validate_dataset`]: any error means "do not install".
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: Stats,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// URL-segment safe and free of path tricks: letters, digits, `_ - . :` (resources are
/// `ResourceType:Gold`), no `..`, at most 200 chars. Files named after a class go through
/// the stricter `media::safe_name`, which also refuses `:`.
pub fn safe_class_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && !s.contains("..") && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// Is `name` a blueprint record name (`BP_CRAFT_behr_rifle_ballistic_01`): 1 to 120 of
/// `A-Z a-z 0-9 _ . -`? The one rule for blueprint names: the validator applies it to every
/// dataset, the server to the names its routes take and to a visitor's hand-over.
pub fn valid_blueprint_name(name: &str) -> bool {
    (1..=120).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Longest `itemType`, `itemSubType` or `itemClass` of a blueprint.
pub const MAX_BLUEPRINT_LABEL: usize = 64;
/// Most `tags` of one blueprint.
pub const MAX_BLUEPRINT_TAGS: usize = 6;
/// Longest blueprint tag.
pub const MAX_BLUEPRINT_TAG: usize = 40;
/// Longest manufacturer code (`ACOM`).
pub const MAX_MANUFACTURER_CODE: usize = 16;
/// Longest manufacturer name (`Amon & Reese Co.`).
pub const MAX_MANUFACTURER_NAME: usize = 80;
/// Largest item size (the game's go to 12).
pub const MAX_ITEM_SIZE: u32 = 12;
/// Largest item grade (the game's are 1 to 4, A to D).
pub const MAX_ITEM_GRADE: u32 = 9;

/// 1 to `max` characters, none of them a control character: what a blueprint's item type,
/// sub type, class and manufacturer fields may hold. The extractor leaves out what fails it.
pub fn valid_blueprint_label(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.chars().count()) && !s.chars().any(char::is_control)
}

/// What the key and the name of a material's quality tiers may hold: 1 to
/// [`MAX_QUALITY_TIER_TEXT`] characters, none of them a control character. The extractor leaves
/// out what fails it.
pub fn valid_quality_tier_text(s: &str) -> bool {
    valid_blueprint_label(s, MAX_QUALITY_TIER_TEXT)
}

/// The bands of a material's quality tiers: 1 to [`MAX_QUALITY_BANDS`] of them, every `start`,
/// `end` and `value` inside 0..=[`MAX_QUALITY`] with `start <= end`, in ascending order without
/// overlapping. The extractor leaves out a material whose bands fail it.
pub fn valid_quality_bands(bands: &[QualityBand]) -> bool {
    (1..=MAX_QUALITY_BANDS).contains(&bands.len())
        && bands.iter().all(|b| [b.start, b.end, b.value].iter().all(|v| (0..=MAX_QUALITY).contains(v)) && b.start <= b.end)
        && bands.windows(2).all(|w| w[0].end < w[1].start)
}

/// A blueprint tag (a folder of the record's path): 1 to [`MAX_BLUEPRINT_TAG`] of
/// `a-z 0-9 _ -`. The extractor leaves out what fails it.
pub fn valid_blueprint_tag(tag: &str) -> bool {
    (1..=MAX_BLUEPRINT_TAG).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// `io::Write` that only counts the bytes and fails once they pass `max`, so measuring a
/// hostile list stops there instead of holding all of it.
struct SizeLimit {
    written: usize,
    max: usize,
}

impl std::io::Write for SizeLimit {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written = self.written.saturating_add(buf.len());
        if self.written > self.max { Err(std::io::Error::other("too big")) } else { Ok(buf.len()) }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Does `value` serialize to JSON within `max` bytes?
fn json_fits(value: &impl Serialize, max: usize) -> bool {
    serde_json::to_writer(SizeLimit { written: 0, max }, value).is_ok()
}

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn version_ok(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    (2..=4).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_digit()))
}

fn channel_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// `img/<stem>.png|svg` with a file-name-safe stem.
pub fn image_ref_ok(s: &str) -> bool {
    let Some(file) = s.strip_prefix("img/") else { return false };
    let Some((stem, ext)) = file.rsplit_once('.') else { return false };
    matches!(ext, "png" | "svg") && !stem.is_empty() && stem.len() <= 200 && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn text_problem(s: &str, max: usize) -> Option<String> {
    if s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Some("contém caracteres de controle".into());
    }
    let n = s.chars().count();
    if n > max {
        return Some(format!("texto com {n} caracteres (máximo {max})"));
    }
    None
}

/// At most [`MAX_SHORT`] characters of `s`, so a report never repeats a huge hostile string.
fn short(s: &str) -> String {
    s.chars().take(MAX_SHORT).collect()
}

struct Checker<'a> {
    report: Report,
    items: HashSet<&'a str>,
    contracts: HashSet<&'a str>,
    scopes: HashSet<&'a str>,
    emporiums: HashSet<&'a str>,
}

impl Checker<'_> {
    fn err(&mut self, msg: impl Into<String>) {
        if self.report.errors.len() < 50 {
            self.report.errors.push(msg.into());
        }
    }

    fn warn(&mut self, msg: impl Into<String>) {
        if self.report.warnings.len() < 200 {
            self.report.warnings.push(msg.into());
        }
    }

    fn text(&mut self, what: &str, s: &str, max: usize) {
        if let Some(p) = text_problem(s, max) {
            self.err(format!("{what}: {p}"));
        }
    }

    fn item_ref(&mut self, what: &str, id: &Option<String>, hard: bool) {
        if let Some(id) = id
            && !self.items.contains(id.as_str())
        {
            let msg = format!("{what} referencia o item {id}, que não está na lista de itens");
            if hard { self.err(msg) } else { self.warn(msg) }
        }
    }

    fn requirement(&mut self, where_: &str, r: &Requirement) {
        self.text(&format!("{where_} nome"), &r.name, MAX_SHORT);
        if r.qty < 0 || r.qty_max < r.qty {
            self.err(format!("{where_}: quantidade inválida ({}..{})", r.qty, r.qty_max));
        }
        self.item_ref(where_, &r.item_id, true);
        for c in &r.candidates {
            self.item_ref(&format!("{where_} candidato"), &c.item_id, true);
        }
        for (oi, option) in r.options.iter().enumerate() {
            for (ri, x) in option.iter().enumerate() {
                self.requirement(&format!("{where_} opção {oi} #{ri}"), x);
            }
        }
    }

    /// Finite and within ±[`MAX_BLUEPRINT_VALUE`].
    fn value_ok(v: f64) -> bool {
        v.is_finite() && v.abs() <= MAX_BLUEPRINT_VALUE
    }

    fn blueprint(&mut self, bp: &Blueprint) {
        let where_ = format!("blueprint {}", short(&bp.name));
        // The record name is what the pin and acquired routes take in their URLs.
        if !valid_blueprint_name(&bp.name) {
            self.err(format!("{where_}: nome de registro inválido"));
        }
        for s in [&bp.display_name, &bp.category].into_iter().flatten() {
            self.text(&where_, s, MAX_SHORT);
        }
        if !safe_class_name(&bp.output_class_name) {
            self.err(format!("{where_}: classe de saída inválida"));
        }
        self.text(&format!("{where_} saída"), &bp.output_name, MAX_SHORT);
        self.item_ref(&where_, &bp.output_item_id, true);
        for (what, s) in [("tipo de item", &bp.item_type), ("subtipo de item", &bp.item_sub_type), ("classe de item", &bp.item_class)] {
            if s.as_deref().is_some_and(|s| !valid_blueprint_label(s, MAX_BLUEPRINT_LABEL)) {
                self.err(format!("{where_}: {what} inválido"));
            }
        }
        if bp.size.is_some_and(|n| n > MAX_ITEM_SIZE) {
            self.err(format!("{where_}: tamanho fora de 0..{MAX_ITEM_SIZE}"));
        }
        if bp.grade.is_some_and(|n| !(1..=MAX_ITEM_GRADE).contains(&n)) {
            self.err(format!("{where_}: grau fora de 1..{MAX_ITEM_GRADE}"));
        }
        if let Some(m) = &bp.manufacturer
            && (!valid_blueprint_label(&m.code, MAX_MANUFACTURER_CODE) || !valid_blueprint_label(&m.name, MAX_MANUFACTURER_NAME))
        {
            self.err(format!("{where_}: fabricante inválido"));
        }
        if bp.tags.len() > MAX_BLUEPRINT_TAGS || bp.tags.iter().any(|t| !valid_blueprint_tag(t)) {
            self.err(format!("{where_}: tags inválidas"));
        }
        if !bp.craft_time_sec.is_finite() || !(0.0..=MAX_CRAFT_TIME_SEC).contains(&bp.craft_time_sec) {
            self.err(format!("{where_}: tempo inválido"));
        }
        if bp.slots.len() > MAX_BLUEPRINT_PARTS || bp.optional_slots.len() > MAX_BLUEPRINT_PARTS {
            self.err(format!("{where_}: slots demais"));
            return;
        }
        if bp.slots_required.is_some_and(|n| n == 0 || n as usize > bp.slots.len()) {
            self.warn(format!("{where_} pede {:?} de {} slots", bp.slots_required, bp.slots.len()));
        }
        for (i, slot) in bp.slots.iter().enumerate() {
            self.blueprint_slot(&format!("{where_} slot #{i}"), slot);
        }
        for (i, slot) in bp.optional_slots.iter().enumerate() {
            self.blueprint_slot(&format!("{where_} slot opcional #{i}"), slot);
        }
        if bp.sources.len() > MAX_SOURCES_PER_BLUEPRINT {
            self.err(format!("{where_}: origens demais ({})", bp.sources.len()));
            return;
        }
        for (i, src) in bp.sources.iter().enumerate() {
            let at = format!("{where_} origem #{i}");
            if src.kind != "contract" {
                self.warn(format!("{at} com kind desconhecido: {:?}", short(&src.kind)));
            }
            self.text(&at, &src.kind, MAX_SHORT);
            if !id_ok(&src.contract_id) {
                self.err(format!("{at}: contrato com id inválido"));
            }
            self.text(&at, &src.title, MAX_SHORT);
            if let Some(g) = &src.giver {
                self.text(&at, g, MAX_SHORT);
            }
            if !matches!(src.when.as_str(), "accept" | "complete") {
                self.err(format!("{at}: momento inválido"));
            }
            self.text(&at, &src.pool, MAX_SHORT);
            if !src.chance.is_finite() || !(0.0..=1.0).contains(&src.chance) {
                self.err(format!("{at}: chance fora de 0..1"));
            }
            if src.pool_chance.is_some_and(|c| !c.is_finite() || !(0.0..=1.0).contains(&c)) {
                self.err(format!("{at}: chance do pool fora de 0..1"));
            }
        }
    }

    fn blueprint_slot(&mut self, where_: &str, slot: &BlueprintSlot) {
        self.text(&format!("{where_} nome"), &slot.name, MAX_SHORT);
        if slot.options.len() > MAX_BLUEPRINT_PARTS || slot.quality_effects.len() > MAX_BLUEPRINT_PARTS || slot.quantity_multipliers.len() > MAX_BLUEPRINT_PARTS {
            self.err(format!("{where_}: partes demais"));
            return;
        }
        if slot.count == 0 || slot.count as usize > slot.options.len() {
            self.warn(format!("{where_} pede {} de {} opções", slot.count, slot.options.len()));
        }
        for inp in &slot.options {
            self.text(&format!("{where_} ingrediente"), &inp.name, MAX_SHORT);
            if let Some(c) = &inp.class_name {
                self.text(&format!("{where_} ingrediente"), c, MAX_SHORT);
            }
            // The page and the overlay only know these two.
            if !matches!(inp.kind.as_str(), "resource" | "item") {
                self.err(format!("{where_}: ingrediente com kind desconhecido: {:?}", short(&inp.kind)));
            }
            if !Self::value_ok(inp.qty) || inp.qty < 0.0 || !(0..=MAX_QUALITY).contains(&inp.min_quality) {
                self.err(format!("{where_}: ingrediente {} com quantidade ou qualidade inválida", short(&inp.name)));
            }
            self.item_ref(&format!("{where_} ingrediente"), &inp.item_id, true);
        }
        for m in &slot.quantity_multipliers {
            if m.option.is_some_and(|o| o as usize >= slot.options.len()) || !Self::value_ok(m.multiplier) || m.multiplier < 0.0 {
                self.err(format!("{where_}: multiplicador inválido"));
            }
        }
        for e in &slot.quality_effects {
            let key = short(&e.key);
            if !safe_class_name(&e.key) {
                self.err(format!("{where_}: efeito de qualidade com chave inválida: {key:?}"));
            }
            if let Some(n) = &e.name {
                self.text(&format!("{where_} efeito"), n, MAX_SHORT);
            }
            if e.ranges.len() > MAX_QUALITY_RANGES {
                self.err(format!("{where_}: efeito {key} com faixas demais"));
                continue;
            }
            if e.ranges.is_empty() {
                self.warn(format!("{where_}: efeito {key} sem faixas"));
            }
            for r in &e.ranges {
                if !(0..=MAX_QUALITY).contains(&r.start_quality) || !(0..=MAX_QUALITY).contains(&r.end_quality) || !Self::value_ok(r.at_start) || !Self::value_ok(r.at_end) {
                    self.err(format!("{where_}: efeito {key} com faixa inválida"));
                } else if r.start_quality > r.end_quality {
                    self.warn(format!("{where_}: efeito {key} com faixa invertida"));
                }
            }
        }
    }
}

/// Check a dataset before installing it; `previous` is the dataset currently active for
/// the channel (for "this looks like a step backwards" warnings).
pub fn validate_dataset(ds: &WikeloDataset, previous: Option<&WikeloDataset>) -> Report {
    let mut ck = Checker {
        report: Report::default(),
        items: ds.items.iter().map(|i| i.id.as_str()).collect(),
        contracts: ds.contracts.iter().map(|c| c.id.as_str()).collect(),
        scopes: ds.faction.scopes.iter().map(|s| s.id.as_str()).collect(),
        emporiums: ds.emporiums.iter().map(|e| e.key.as_str()).collect(),
    };

    // Header.
    if ds.schema_version != SCHEMA_VERSION {
        ck.err(format!("schemaVersion {} (este servidor entende {})", ds.schema_version, SCHEMA_VERSION));
    }
    if !channel_ok(&ds.build.channel) {
        ck.err(format!("canal inválido: {:?}", ds.build.channel));
    }
    if !version_ok(&ds.build.version) {
        ck.err(format!("versão inválida: {:?}", ds.build.version));
    }
    // Empty or a change number `build_order` takes (1 to 18 digits): anything else would sort
    // as no number at all and could not say which build is newer.
    if !ds.build.p4_change.is_empty() && p4_change_number(&ds.build.p4_change).is_none() {
        ck.err(format!("p4Change inválido: {:?}", short(&ds.build.p4_change)));
    }
    for (what, s) in [("branch", &ds.build.branch), ("buildId", &ds.build.build_id), ("tag", &ds.build.tag), ("generatedAt", &ds.generated_at), ("source", &ds.source)] {
        if s.len() > 80 || text_problem(s, 80).is_some() {
            ck.err(format!("{what} inválido"));
        }
    }
    if let Some(v) = ds.build.dcb_version
        && !matches!(v, 6 | 8)
    {
        ck.warn(format!("DataCore v{v} desconhecido"));
    }
    if let Some(x) = &ds.extractor {
        for (what, s) in [("extractor.name", &x.name), ("extractor.version", &x.version), ("extractor.commit", &x.commit), ("extractor.dcbSha256", &x.dcb_sha256), ("extractor.iniSha256", &x.ini_sha256)] {
            if s.len() > 80 || text_problem(s, 80).is_some() {
                ck.err(format!("{what} inválido"));
            }
        }
    }

    // Sizes.
    if ds.contracts.is_empty() {
        ck.err("nenhum contrato");
    }
    for (what, n, max) in [
        ("contratos", ds.contracts.len(), MAX_CONTRACTS),
        ("itens", ds.items.len(), MAX_ITEMS),
        ("receitas", ds.crafting.len(), MAX_CRAFTING),
        ("recompensas de missão", ds.mission_rewards.len(), MAX_MISSION_REWARDS),
        ("loot de missão", ds.mission_drops.len(), MAX_MISSION_DROPS),
        ("missões", ds.missions.len(), MAX_MISSIONS),
        ("locais", ds.locations.len(), MAX_LOCATIONS),
        ("blueprints", ds.blueprints.len(), MAX_BLUEPRINTS),
        ("origens de blueprint", ds.blueprints.iter().map(|b| b.sources.len()).sum::<usize>(), MAX_BLUEPRINT_SOURCES),
        ("materiais com faixas de qualidade", ds.quality_tiers.len(), MAX_QUALITY_TIERS),
        ("avisos", ds.warnings.len(), MAX_WARNINGS),
    ] {
        if n > max {
            ck.err(format!("{what}: {n} (máximo {max})"));
        }
    }
    for w in ds.warnings.iter().take(MAX_WARNINGS) {
        if text_problem(w, 500).is_some() {
            ck.err("aviso com texto inválido");
            break;
        }
    }

    // Faction / reputation scopes.
    if ds.faction.id != WIKELO_FACTION_ID {
        ck.warn(format!("facção {} em vez da do Wikelo", ds.faction.id));
    }
    ck.text("nome da facção", &ds.faction.name, MAX_SHORT);
    let mut seen = HashSet::new();
    for sc in &ds.faction.scopes {
        if !id_ok(&sc.id) || !seen.insert(sc.id.as_str()) {
            ck.err(format!("escopo de reputação com id inválido ou repetido: {:?}", sc.id));
        }
        ck.text("escopo", &sc.name, MAX_SHORT);
        ck.text("escopo", &sc.display_name, MAX_SHORT);
        if sc.ceiling.is_some_and(|c| c < 0) {
            ck.warn(format!("escopo {} com teto negativo", sc.name));
        }
        if sc.standings.windows(2).any(|w| w[0].min_rep > w[1].min_rep) {
            ck.warn(format!("escopo {}: standings fora de ordem", sc.name));
        }
        for st in &sc.standings {
            ck.text("standing", &st.name, MAX_SHORT);
            ck.text("standing", &st.display_name, MAX_SHORT);
        }
    }

    // Emporiums.
    let mut seen = HashSet::new();
    for e in &ds.emporiums {
        if !safe_class_name(&e.key) || !seen.insert(e.key.as_str()) {
            ck.err(format!("emporium com chave inválida ou repetida: {:?}", e.key));
        }
        ck.text("emporium", &e.name, MAX_SHORT);
        for c in &e.class_names {
            ck.text("emporium classe", c, MAX_SHORT);
        }
    }

    // Items first: contracts reference them.
    let mut seen = HashSet::new();
    let mut seen_class = HashSet::new();
    for it in &ds.items {
        if !id_ok(&it.id) || !seen.insert(it.id.as_str()) {
            ck.err(format!("item com id inválido ou repetido: {:?}", it.id));
        }
        if !safe_class_name(&it.class_name) {
            ck.err(format!("item {} com nome de classe inválido: {:?}", it.id, it.class_name));
        } else if !seen_class.insert(it.class_name.as_str()) {
            ck.warn(format!("classe de item repetida: {}", it.class_name));
        }
        ck.text(&format!("item {}", it.id), &it.name, MAX_SHORT);
        for s in [&it.item_type, &it.sub_type].into_iter().flatten() {
            ck.text("tipo de item", s, MAX_SHORT);
        }
        if !matches!(it.kind.as_str(), "currency" | "favour" | "commodity" | "harvestable" | "part" | "armor" | "weapon" | "vehicle" | "component" | "blueprint" | "other") {
            ck.warn(format!("item {} com kind desconhecido: {:?}", it.name, it.kind));
        }
        if let Some(img) = &it.image
            && !image_ref_ok(img)
        {
            ck.err(format!("item {} com referência de imagem inválida: {:?}", it.name, img));
        }
        if let Some(src) = &it.image_source
            && !matches!(src.as_str(), "vehicle_icon" | "frontend_ship" | "loadout_icon" | "inv_icon" | "generic")
        {
            ck.warn(format!("item {} com imageSource desconhecido: {:?}", it.name, src));
        }
        // Ascending, without repeats, inside the game's 1..=1000 scale.
        if it.qualities.len() > 32 || it.qualities.iter().any(|q| !(1..=1000).contains(q)) || it.qualities.windows(2).any(|w| w[0] >= w[1]) {
            ck.err(format!("item {} com qualidades inválidas: {:?}", it.name, it.qualities));
        }
    }

    // Contracts.
    let mut seen = HashSet::new();
    let mut seen_debug = HashSet::new();
    for c in &ds.contracts {
        if !id_ok(&c.id) || !seen.insert(c.id.as_str()) {
            ck.err(format!("contrato com id inválido ou repetido: {:?}", c.id));
        }
        let label = if c.title.is_empty() { c.debug_name.clone() } else { c.title.clone() };
        ck.text(&format!("contrato {label} debugName"), &c.debug_name, MAX_SHORT);
        if !c.debug_name.is_empty() && !seen_debug.insert(c.debug_name.as_str()) {
            ck.warn(format!("debugName repetido: {}", c.debug_name));
        }
        ck.text(&format!("contrato {label} título"), &c.title, MAX_SHORT);
        ck.text(&format!("contrato {label} descrição"), &c.description, MAX_TEXT);
        ck.text(&format!("contrato {label} descrição"), &c.description_plain, MAX_TEXT);
        for s in [&c.mission_type, &c.title_key, &c.description_key].into_iter().flatten() {
            ck.text(&format!("contrato {label}"), s, MAX_SHORT);
        }
        for a in &c.aliases {
            ck.text(&format!("contrato {label} alias"), a, MAX_SHORT);
        }
        if c.cooldown.abandoned_sec < 0 || c.cooldown.personal_sec < 0 {
            ck.warn(format!("contrato {label} com cooldown negativo"));
        }
        for v in [c.lifetime.instance_lifetime, c.lifetime.respawn_time].into_iter().flatten() {
            if !v.is_finite() || v < 0.0 {
                ck.warn(format!("contrato {label} com tempo de vida inválido"));
            }
        }
        if let Some(rep) = &c.rep_required {
            if let Some(sid) = &rep.scope_id
                && !ck.scopes.contains(sid.as_str())
            {
                ck.warn(format!("contrato {label} exige reputação num escopo desconhecido"));
            }
            for s in [&rep.min_standing_name, &rep.max_standing_name].into_iter().flatten() {
                ck.text(&format!("contrato {label} standing"), s, MAX_SHORT);
            }
        }
        for g in &c.rep_gained {
            if let Some(sid) = &g.scope_id
                && !ck.scopes.contains(sid.as_str())
            {
                ck.warn(format!("contrato {label} dá reputação num escopo desconhecido"));
            }
        }
        for t in c.completion_tags.iter().chain(&c.prerequisites.required_tags).chain(&c.prerequisites.excluded_tags) {
            if !id_ok(&t.id) {
                ck.err(format!("contrato {label} com tag inválida: {:?}", t.id));
            }
            if let Some(n) = &t.name {
                ck.text("tag", n, MAX_SHORT);
            }
        }
        for req in &c.prerequisites.required_contract_ids {
            if !ck.contracts.contains(req.as_str()) {
                ck.warn(format!("contrato {label} exige o contrato {req}, que não existe"));
            }
        }
        for k in &c.emporium_keys {
            if !ck.emporiums.contains(k.as_str()) {
                ck.warn(format!("contrato {label} aponta para o emporium {k}, que não existe"));
            }
        }
        for (i, r) in c.requirements.iter().enumerate() {
            ck.requirement(&format!("contrato {label} requisito #{i}"), r);
        }
        for (i, r) in c.rewards.iter().enumerate() {
            let where_ = format!("contrato {label} recompensa #{i}");
            ck.text(&where_, &r.name, MAX_SHORT);
            if r.qty < 0 || !r.weight.is_finite() || r.weight < 0.0 {
                ck.err(format!("{where_}: quantidade ou peso inválido"));
            }
            ck.item_ref(&where_, &r.item_id, true);
        }
    }

    // Favors, crafting, mission rewards.
    for f in &ds.favor_conversions {
        if !ck.contracts.contains(f.contract_id.as_str()) {
            ck.warn(format!("conversão de favor aponta para o contrato {}, que não existe", f.contract_id));
        }
        for q in f.give.iter().chain(&f.receive) {
            ck.text("conversão", &q.name, MAX_SHORT);
            if q.qty < 0 {
                ck.err("conversão de favor com quantidade negativa");
            } else if q.qty > MAX_CONVERSION_QTY {
                ck.err(format!("conversão de favor com quantidade grande demais ({})", q.qty));
            }
            ck.item_ref("conversão de favor", &q.item_id, false);
        }
    }
    let mut seen = HashSet::new();
    for r in &ds.crafting {
        if !id_ok(&r.blueprint_id) || !seen.insert(r.blueprint_id.as_str()) {
            ck.err(format!("receita com id inválido ou repetido: {:?}", r.blueprint_id));
        }
        for s in [&r.blueprint_name, &r.output_name, &r.output_class_name] {
            ck.text(&format!("receita {}", r.blueprint_name), s, MAX_SHORT);
        }
        if !r.output_class_name.is_empty() && !safe_class_name(&r.output_class_name) {
            ck.err(format!("receita {} com classe de saída inválida", r.blueprint_name));
        }
        if let Some(cat) = &r.category {
            ck.text("categoria de receita", cat, MAX_SHORT);
        }
        if !r.craft_time_sec.is_finite() || r.craft_time_sec < 0.0 {
            ck.err(format!("receita {} com tempo inválido", r.blueprint_name));
        }
        if !ck.items.contains(r.output_item_id.as_str()) {
            ck.warn(format!("receita {} produz um item desconhecido", r.blueprint_name));
        }
        for slot in &r.slots {
            ck.text("slot de receita", &slot.name, MAX_SHORT);
            if slot.count == 0 {
                ck.warn(format!("receita {}: slot {} pede 0 opções", r.blueprint_name, slot.name));
            }
            for inp in &slot.options {
                ck.text("ingrediente", &inp.name, MAX_SHORT);
                if !inp.qty.is_finite() || inp.qty < 0.0 || inp.min_quality < 0 {
                    ck.err(format!("receita {}: ingrediente {} com quantidade inválida", r.blueprint_name, inp.name));
                }
                ck.item_ref("ingrediente", &inp.item_id, false);
            }
        }
        for s in &r.sources {
            ck.text("origem de receita", &s.contract_title, MAX_SHORT);
            if !s.chance.is_finite() || !(0.0..=1.0).contains(&s.chance) {
                ck.warn(format!("receita {}: chance fora de 0..1", r.blueprint_name));
            }
        }
    }
    // `GET …/blueprints` sends them all in one answer. Checked before each blueprint, so the
    // report (which keeps a limited number of errors) always says it.
    if !ds.blueprints.is_empty() && !json_fits(&ds.blueprints, MAX_BLUEPRINTS_BYTES) {
        ck.err(format!("blueprints: mais de {} MB no total", MAX_BLUEPRINTS_BYTES / (1024 * 1024)));
    }
    let mut seen = HashSet::new();
    let mut seen_name = HashSet::new();
    for bp in ds.blueprints.iter().take(MAX_BLUEPRINTS) {
        if !id_ok(&bp.id) || !seen.insert(bp.id.as_str()) {
            ck.err(format!("blueprint com id inválido ou repetido: {:?}", short(&bp.id)));
        }
        // Pins and acquired marks name a blueprint by its record name, so it must be unique.
        if !seen_name.insert(bp.name.as_str()) {
            ck.err(format!("blueprint com nome repetido: {:?}", short(&bp.name)));
        }
        ck.blueprint(bp);
    }
    // Quality tiers: the Blueprints page's craft simulator looks a slot's material up by name.
    let mut seen = HashSet::new();
    for t in ds.quality_tiers.iter().take(MAX_QUALITY_TIERS) {
        let key = short(&t.key);
        if !valid_quality_tier_text(&t.key) || !seen.insert(t.key.as_str()) {
            ck.err(format!("faixas de qualidade com chave inválida ou repetida: {key:?}"));
        }
        if !valid_quality_tier_text(&t.name) {
            ck.err(format!("faixas de qualidade {key:?}: nome inválido"));
        }
        if !valid_quality_bands(&t.bands) {
            ck.err(format!("faixas de qualidade {key:?}: faixas inválidas"));
        }
    }
    let mut seen = HashSet::new();
    for m in &ds.mission_rewards {
        if !id_ok(&m.contract_id) || !id_ok(&m.item_id) {
            ck.err(format!("recompensa de missão com id inválido: {:?}", m.debug_name));
        }
        if !seen.insert((m.contract_id.as_str(), m.item_id.as_str())) {
            ck.warn(format!("recompensa de missão repetida: {} / {}", m.debug_name, m.name));
        }
        for s in [&m.debug_name, &m.generator, &m.name, &m.class_name] {
            ck.text("recompensa de missão", s, MAX_SHORT);
        }
        if m.qty <= 0 {
            ck.err(format!("recompensa de missão {} com quantidade {}", m.debug_name, m.qty));
        }
        if !ck.items.contains(m.item_id.as_str()) {
            ck.err(format!("recompensa de missão {} referencia o item {}, que não está na lista", m.debug_name, m.item_id));
        }
    }
    let mut seen = HashSet::new();
    for d in &ds.mission_drops {
        if !id_ok(&d.contract_id) || !id_ok(&d.item_id) {
            ck.err(format!("loot de missão com id inválido: {:?}", d.debug_name));
        }
        if !seen.insert((d.contract_id.as_str(), d.item_id.as_str())) {
            ck.warn(format!("loot de missão repetido: {} / {}", d.debug_name, d.name));
        }
        for s in [&d.debug_name, &d.generator, &d.name, &d.class_name] {
            ck.text("loot de missão", s, MAX_SHORT);
        }
        if !d.chance.is_finite() || !(0.0..=1.0).contains(&d.chance) {
            ck.err(format!("loot de missão {} com chance fora de 0..1", d.debug_name));
        }
        if !ck.items.contains(d.item_id.as_str()) {
            ck.err(format!("loot de missão {} referencia o item {}, que não está na lista", d.debug_name, d.item_id));
        }
    }
    let mut seen = HashSet::new();
    for m in &ds.missions {
        if !id_ok(&m.id) {
            ck.err(format!("missão com id inválido: {:?}", m.debug_name));
        }
        if !seen.insert(m.id.as_str()) {
            ck.warn(format!("missão repetida: {}", m.debug_name));
        }
        for s in [&m.debug_name, &m.generator, &m.title] {
            ck.text("missão", s, MAX_SHORT);
        }
        for s in [&m.giver, &m.system, &m.family].into_iter().flatten() {
            ck.text("missão", s, MAX_SHORT);
        }
        if m.requires.iter().any(|r| !id_ok(r)) {
            ck.err(format!("missão {} exige um id inválido", m.debug_name));
        }
        ck.text("descrição de missão", &m.description_plain, MAX_TEXT);
        if let Some(rep) = &m.rep_required {
            for s in [&rep.min_standing_name, &rep.max_standing_name].into_iter().flatten() {
                ck.text("reputação de missão", s, MAX_SHORT);
            }
        }
    }

    // Locations: the id is matched against Game.log class names and may end up in URLs.
    let mut seen = HashSet::new();
    for l in ds.locations.iter().take(MAX_LOCATIONS) {
        if l.id.len() > MAX_LOCATION_ID || !safe_class_name(&l.id) || !seen.insert(l.id.as_str()) {
            ck.err(format!("local com id inválido ou repetido: {:?}", l.id.chars().take(MAX_LOCATION_ID).collect::<String>()));
        }
        if l.name.trim().is_empty() {
            ck.err(format!("local {} sem nome", l.id));
        }
        ck.text(&format!("local {}", l.id), &l.name, MAX_SHORT);
        if let Some(sys) = &l.system {
            ck.text(&format!("local {} sistema", l.id), sys, MAX_SHORT);
        }
        if let Some(body) = &l.body {
            ck.text(&format!("local {} corpo", l.id), body, MAX_SHORT);
        }
        if let Some(orbits) = &l.orbits {
            ck.text(&format!("local {} planeta", l.id), orbits, MAX_SHORT);
        }
        if !matches!(l.kind.as_str(), "city" | "station" | "outpost" | "other") {
            ck.warn(format!("local {} com kind desconhecido: {:?}", l.id, l.kind));
        }
    }

    // Against what is active now.
    if let Some(prev) = previous {
        let (vn, vp) = (version_key(&ds.build.version), version_key(&prev.build.version));
        // The server's own rule (`build_order`): an older build is kept but never used.
        if build_order(&ds.build) < build_order(&prev.build) {
            ck.warn(format!(
                "versão {} (p4Change {}) é mais antiga que a ativa ({}, p4Change {}): fica guardada sem trocar a ativa",
                ds.build.version,
                short(&ds.build.p4_change),
                prev.build.version,
                short(&prev.build.p4_change)
            ));
        } else if vn == vp && canonical_hash(ds) != canonical_hash(prev) {
            ck.warn(format!("mesma versão da ativa ({}) com conteúdo diferente", prev.build.version));
        }
        // The change number decides; when it and the version disagree, one of them is wrong.
        if let (Some(pn), Some(pp)) = (p4_change_number(&ds.build.p4_change), p4_change_number(&prev.build.p4_change))
            && ((pn > pp && vn < vp) || (pn < pp && vn > vp))
        {
            let (p4_says, version_says) = if pn > pp { ("mais novo", "mais antiga") } else { ("mais antigo", "mais nova") };
            ck.warn(format!(
                "p4Change {pn} é {p4_says} que o da ativa ({pp}), mas a versão {} é {version_says} que a ativa ({}); vale o p4Change",
                ds.build.version, prev.build.version
            ));
        }
        if !prev.blueprints.is_empty() && ds.blueprints.is_empty() {
            ck.warn(format!("nenhuma blueprint, e a ativa tem {}", prev.blueprints.len()));
        }
        let new_ids: HashSet<&str> = ds.contracts.iter().map(|c| c.id.as_str()).collect();
        let removed: Vec<&Contract> = prev.contracts.iter().filter(|c| !new_ids.contains(c.id.as_str())).collect();
        if !prev.contracts.is_empty() && removed.len() * 10 > prev.contracts.len() * 3 {
            let names: Vec<&str> = removed.iter().take(10).map(|c| if c.title.is_empty() { c.debug_name.as_str() } else { c.title.as_str() }).collect();
            ck.warn(format!("mudança grande: {} dos {} contratos ativos não existem mais ({})", removed.len(), prev.contracts.len(), names.join(", ")));
        }
        let new_items: HashSet<&str> = ds.items.iter().map(|i| i.id.as_str()).collect();
        let removed_items = prev.items.iter().filter(|i| !new_items.contains(i.id.as_str())).count();
        if !prev.items.is_empty() && removed_items * 2 > prev.items.len() {
            ck.warn(format!("mudança grande: {} dos {} itens ativos não existem mais", removed_items, prev.items.len()));
        }
        let prev_fp: HashMap<&str, String> = prev.contracts.iter().map(|c| (c.id.as_str(), contract_fingerprint(c))).collect();
        let changed = ds.contracts.iter().filter(|c| prev_fp.get(c.id.as_str()).is_some_and(|fp| *fp != contract_fingerprint(c))).count();
        if changed * 2 > ds.contracts.len().max(1) {
            ck.warn(format!("mudança grande: {changed} de {} contratos mudaram requisitos ou recompensas", ds.contracts.len()));
        }
    }

    ck.report.stats = Stats {
        contracts: ds.contracts.len(),
        items: ds.items.len(),
        with_image: ds.items.iter().filter(|i| i.image.is_some()).count(),
        crafting: ds.crafting.len(),
        mission_rewards: ds.mission_rewards.len(),
        mission_drops: ds.mission_drops.len(),
        missions: ds.missions.len(),
        locations: ds.locations.len(),
        blueprints: ds.blueprints.len(),
        warnings: ds.warnings.len(),
    };
    ck.report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, class: &str) -> Item {
        Item { id: id.into(), class_name: class.into(), name: class.replace('_', " "), kind: "part".into(), image: Some(format!("img/{class}.png")), image_source: Some("inv_icon".into()), ..Default::default() }
    }

    fn location(id: &str, name: &str) -> Location {
        Location { id: id.into(), name: name.into(), system: Some("Stanton".into()), kind: "other".into(), home: false, ..Default::default() }
    }

    fn sample() -> WikeloDataset {
        let reward = |id: &str| Reward {
            result: 0,
            option: 0,
            weight: 1.0,
            owner_only: true,
            item_id: Some(id.into()),
            class_name: None,
            name: id.into(),
            qty: 1,
            kind: RewardKind::Currency,
            on_accept: false,
        };
        let contract = |id: &str, title: &str, qty: i64| Contract {
            id: id.into(),
            debug_name: format!("TheCollector_{title}"),
            title: title.into(),
            requirements: vec![Requirement { kind: RequirementKind::Entity, item_id: Some("i1".into()), name: "Favor".into(), qty, qty_max: qty, ..Default::default() }],
            rewards: vec![reward("i2")],
            emporium_keys: vec!["dasi".into()],
            ..Default::default()
        };
        WikeloDataset {
            schema_version: SCHEMA_VERSION,
            source: "local-extract".into(),
            build: BuildInfo { channel: "LIVE".into(), branch: "sc-alpha-4.10.2".into(), version: "4.10.197.2878".into(), p4_change: "12913470".into(), dcb_version: Some(8), p4k_size: Some(1), ..Default::default() },
            generated_at: "2026-10-15T00:00:00Z".into(),
            faction: Faction {
                id: WIKELO_FACTION_ID.into(),
                name: "Wikelo Emporium".into(),
                scopes: vec![RepScope { id: "s2".into(), name: "b".into(), ..Default::default() }, RepScope { id: "s1".into(), name: "a".into(), ..Default::default() }],
            },
            emporiums: vec![Emporium { key: "dasi".into(), name: "Dasi".into(), location_hash: None, class_names: vec!["X".into()] }],
            contracts: vec![contract("c2", "Scorpius", 15), contract("c1", "Intro", 1)],
            items: vec![item("i2", "MG_Scrip"), item("i1", "Wikelo_Favor")],
            favor_conversions: vec![],
            crafting: vec![],
            warnings: vec!["w".into()],
            mission_rewards: vec![MissionReward { contract_id: "m1".into(), debug_name: "Patrol".into(), generator: "HH".into(), item_id: "i2".into(), class_name: "MG_Scrip".into(), name: "MG Scrip".into(), qty: 10, owner_only: true, one_of: false }],
            mission_drops: vec![],
            missions: vec![],
            locations: vec![location("Stanton2_Orison", "Orison"), location("RR_CRU_LEO", "Seraphim Station")],
            blueprints: vec![],
            quality_tiers: vec![],
            extractor: Some(ExtractorInfo { name: "wikelo-watcher".into(), version: "0.1.0".into(), commit: "dev".into(), dcb_sha256: "a".into(), ini_sha256: "b".into(), revision: 0 }),
        }
    }

    #[test]
    fn hash_ignores_order_and_machine_fields() {
        let a = sample();
        let mut b = sample();
        b.contracts.reverse();
        b.items.reverse();
        b.faction.scopes.reverse();
        b.locations.reverse();
        b.generated_at = "2030-01-01T00:00:00Z".into();
        b.build.channel = "PTU".into();
        b.build.p4k_size = Some(999);
        b.warnings.clear();
        b.extractor = None;
        for it in &mut b.items {
            it.image = Some("img/other.svg".into());
        }
        assert_eq!(canonical_hash(&a), canonical_hash(&b));
        assert!(canonical_hash(&a).starts_with("v1:"));
        let mut c = sample();
        c.contracts[0].requirements[0].qty += 1;
        assert_ne!(canonical_hash(&a), canonical_hash(&c));
        let mut d = sample();
        d.build.version = "4.11.0.1".into();
        assert_ne!(canonical_hash(&a), canonical_hash(&d));
        let mut e = sample();
        e.locations[0].name = "Orison City".into();
        assert_ne!(canonical_hash(&a), canonical_hash(&e));

        // Quality tiers are in the hash; the materials' order is not.
        let t = with_tiers();
        assert_ne!(canonical_hash(&a), canonical_hash(&t), "the field is in the hash");
        let mut u = with_tiers();
        u.quality_tiers.reverse();
        assert_eq!(canonical_hash(&t), canonical_hash(&u), "the materials' order carries no meaning");
        let mut v = with_tiers();
        v.quality_tiers[0].bands[0].value += 1;
        assert_ne!(canonical_hash(&t), canonical_hash(&v), "a changed band value");
        let mut w = with_tiers();
        w.quality_tiers[1].name = "Jumping Lime".into();
        assert_ne!(canonical_hash(&t), canonical_hash(&w), "a changed name");
    }

    #[test]
    fn hash_without_locations_is_unchanged() {
        // The canonical form of a dataset with no locations must stay byte for byte what it
        // was before the field existed, or every stored hash would stop matching.
        let mut ds = sample();
        ds.locations.clear();
        let json = serde_json::to_string(&canonicalize(&ds)).unwrap();
        assert!(!json.contains("locations"), "{json}");
        // Same for the quality tiers.
        assert!(!json.contains("qualityTiers"), "{json}");
    }

    /// The bands every material has today, with Agricium's values.
    fn bands() -> Vec<QualityBand> {
        [(0, 399, 346), (400, 599, 588), (600, 699, 667), (700, 799, 796), (800, 899, 852), (900, 949, 943), (950, 998, 971), (999, 1000, 1000)]
            .into_iter()
            .map(|(start, end, value)| QualityBand { start, end, value })
            .collect()
    }

    fn with_tiers() -> WikeloDataset {
        let mut ds = sample();
        ds.quality_tiers = vec![
            QualityTiers { key: "Agricium".into(), name: "Agricium".into(), bands: bands() },
            QualityTiers { key: "JumpingLimes".into(), name: "Jumping Limes".into(), bands: bands() },
        ];
        ds
    }

    #[test]
    fn valid_quality_tiers_pass() {
        let r = validate_dataset(&with_tiers(), None);
        assert!(r.ok(), "{:?}", r.errors);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        // The bounds themselves pass.
        let mut ds = with_tiers();
        ds.quality_tiers[0].key = "K".repeat(MAX_QUALITY_TIER_TEXT);
        ds.quality_tiers[0].name = "é".repeat(MAX_QUALITY_TIER_TEXT);
        ds.quality_tiers[1].bands = (0..MAX_QUALITY_BANDS as i64).map(|n| QualityBand { start: n * 10, end: n * 10 + 9, value: n * 10 }).collect();
        let r = validate_dataset(&ds, None);
        assert!(r.ok(), "{:?}", r.errors);
        let mut ds = with_tiers();
        ds.quality_tiers[0].bands = vec![QualityBand { start: 0, end: 0, value: 0 }, QualityBand { start: 1, end: MAX_QUALITY, value: MAX_QUALITY }];
        assert!(validate_dataset(&ds, None).ok(), "a one-point band, and the scale's edges");
        let mut ds = sample();
        ds.quality_tiers = (0..MAX_QUALITY_TIERS).map(|n| QualityTiers { key: format!("M{n}"), name: format!("M {n}"), bands: bands() }).collect();
        assert!(validate_dataset(&ds, None).ok(), "{MAX_QUALITY_TIERS} materials");
    }

    #[test]
    fn bad_quality_tiers_are_errors() {
        let rejects = |what: &str, change: &dyn Fn(&mut QualityTiers)| {
            let mut ds = with_tiers();
            change(&mut ds.quality_tiers[0]);
            let r = validate_dataset(&ds, None);
            assert!(r.errors.iter().any(|e| e.starts_with("faixas de qualidade")), "{what} should be refused: {:?}", r.errors);
            assert!(r.errors.iter().all(|e| e.chars().count() < 600), "{what}: errors stay short: {:?}", r.errors);
        };
        rejects("duplicate key", &|t| t.key = "JumpingLimes".into());
        rejects("empty key", &|t| t.key = String::new());
        rejects("long key", &|t| t.key = "K".repeat(MAX_QUALITY_TIER_TEXT + 1));
        rejects("huge key", &|t| t.key = "K".repeat(100_000));
        rejects("key with a control character", &|t| t.key = "Agri\u{0}cium".into());
        rejects("empty name", &|t| t.name = String::new());
        rejects("long name", &|t| t.name = "é".repeat(MAX_QUALITY_TIER_TEXT + 1));
        rejects("name with a newline", &|t| t.name = "Agri\ncium".into());
        rejects("no bands", &|t| t.bands.clear());
        rejects("too many bands", &|t| t.bands = (0..=MAX_QUALITY_BANDS as i64).map(|n| QualityBand { start: n * 10, end: n * 10 + 9, value: n * 10 }).collect());
        rejects("negative start", &|t| t.bands[0].start = -1);
        rejects("end above the scale", &|t| t.bands[7].end = MAX_QUALITY + 1);
        rejects("start above the scale", &|t| t.bands = vec![QualityBand { start: MAX_QUALITY + 1, end: MAX_QUALITY + 2, value: 1 }]);
        rejects("negative value", &|t| t.bands[0].value = -1);
        rejects("value above the scale", &|t| t.bands[7].value = MAX_QUALITY + 1);
        rejects("start after end", &|t| t.bands[0] = QualityBand { start: 399, end: 0, value: 346 });
        rejects("overlapping bands", &|t| t.bands[1].start = 399);
        rejects("bands out of order", &|t| t.bands.swap(0, 1));

        let mut ds = sample();
        ds.quality_tiers = (0..=MAX_QUALITY_TIERS).map(|n| QualityTiers { key: format!("M{n}"), name: format!("M {n}"), bands: bands() }).collect();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.starts_with("materiais com faixas de qualidade")), "too many materials");
    }

    fn source(contract_id: &str, chance: f64) -> BlueprintSource {
        BlueprintSource {
            kind: "contract".into(),
            contract_id: contract_id.into(),
            title: "Intro".into(),
            giver: Some("Wikelo Emporium".into()),
            when: "complete".into(),
            pool: "BP_MISSIONREWARD_Intro".into(),
            chance,
            pool_chance: None,
            disabled: false,
        }
    }

    fn blueprint(id: &str) -> Blueprint {
        Blueprint {
            id: id.into(),
            name: format!("BP_CRAFT_{id}"),
            display_name: None,
            category: Some("FPSWeapons".into()),
            output_item_id: Some("i2".into()),
            output_class_name: "MG_Scrip".into(),
            output_name: "MG Scrip".into(),
            item_type: Some("WeaponPersonal".into()),
            item_sub_type: Some("Medium".into()),
            size: Some(2),
            grade: Some(1),
            manufacturer: Some(BlueprintManufacturer { code: "BEHR".into(), name: "Behring".into() }),
            item_class: Some("Military".into()),
            tags: vec!["fpsgear".into(), "weapons".into(), "rifle".into()],
            default: true,
            craft_time_sec: 150.0,
            slots_required: Some(1),
            slots: vec![BlueprintSlot {
                name: "Frame".into(),
                count: 1,
                options: vec![CraftInput { kind: "resource".into(), item_id: Some("i1".into()), class_name: None, name: "Iron".into(), qty: 0.04, unit: Unit::Scu, min_quality: 0 }],
                quantity_multipliers: vec![QuantityMultiplier { option: Some(0), multiplier: 1.15 }],
                quality_effects: vec![QualityEffect {
                    key: "gpp_weapon_damage".into(),
                    name: Some("Damage".into()),
                    ranges: vec![QualityRange { kind: QualityRangeKind::Linear, start_quality: 0, end_quality: 1000, at_start: 0.925, at_end: 1.075 }],
                }],
            }],
            optional_slots: vec![],
            sources: vec![source("c1", 0.25), source("c2", 0.5)],
        }
    }

    fn with_blueprints() -> WikeloDataset {
        let mut ds = sample();
        ds.blueprints = vec![blueprint("b1"), blueprint("b2")];
        ds
    }

    #[test]
    fn hash_covers_blueprints_but_not_their_order() {
        let a = with_blueprints();
        assert_ne!(canonical_hash(&a), canonical_hash(&sample()), "the field is in the hash");
        let mut b = with_blueprints();
        b.blueprints.reverse();
        b.blueprints[0].sources.reverse();
        assert_eq!(canonical_hash(&a), canonical_hash(&b), "list order carries no meaning");
        let mut c = with_blueprints();
        c.blueprints[1].slots[0].quality_effects[0].ranges[0].at_end = 1.1;
        assert_ne!(canonical_hash(&a), canonical_hash(&c), "a changed quality range");
        let mut d = with_blueprints();
        d.blueprints[0].sources[0].chance = 0.3;
        assert_ne!(canonical_hash(&a), canonical_hash(&d), "a changed chance");
        let mut p = with_blueprints();
        p.blueprints[0].sources[0].pool_chance = Some(0.25);
        assert_ne!(canonical_hash(&a), canonical_hash(&p), "the pool chance is in the hash");
        let mut e = with_blueprints();
        e.blueprints[0].craft_time_sec = 151.0;
        assert_ne!(canonical_hash(&a), canonical_hash(&e), "a changed craft time");
        // What the blueprint's item is: every field is in the hash.
        type Change<'a> = (&'a str, &'a dyn Fn(&mut Blueprint));
        let changes: [Change; 9] = [
            ("item type", &|b: &mut Blueprint| b.item_type = Some("WeaponGun".into())),
            ("sub type", &|b: &mut Blueprint| b.item_sub_type = None),
            ("size", &|b: &mut Blueprint| b.size = Some(3)),
            ("grade", &|b: &mut Blueprint| b.grade = Some(2)),
            ("manufacturer code", &|b: &mut Blueprint| b.manufacturer.as_mut().unwrap().code = "KSAR".into()),
            ("manufacturer name", &|b: &mut Blueprint| b.manufacturer.as_mut().unwrap().name = "Kastak Arms".into()),
            ("class", &|b: &mut Blueprint| b.item_class = Some("Civilian".into())),
            ("tags", &|b: &mut Blueprint| b.tags.push("ballistic".into())),
            ("default", &|b: &mut Blueprint| b.default = false),
        ];
        for (what, change) in changes {
            let mut f = with_blueprints();
            change(&mut f.blueprints[0]);
            assert_ne!(canonical_hash(&a), canonical_hash(&f), "a changed {what}");
        }
        // A blueprint without them serializes as one did before they existed.
        let mut bare = with_blueprints();
        for bp in &mut bare.blueprints {
            *bp = Blueprint { item_type: None, item_sub_type: None, size: None, grade: None, manufacturer: None, item_class: None, tags: vec![], default: false, ..bp.clone() };
        }
        let json = serde_json::to_string(&canonicalize(&bare).blueprints).unwrap();
        for key in ["itemType", "itemSubType", "\"size\"", "\"grade\"", "manufacturer", "itemClass", "\"tags\"", "\"default\""] {
            assert!(!json.contains(key), "{key} stays out when unknown: {json}");
        }
        // Same for a source without a pool chance (the sample's sources have none).
        assert!(!json.contains("poolChance"), "poolChance stays out when absent: {json}");
        // Without blueprints the canonical form is what it was before the field existed.
        let json = serde_json::to_string(&canonicalize(&sample())).unwrap();
        assert!(!json.contains("blueprints"), "{json}");
    }

    #[test]
    fn valid_blueprints_pass() {
        let r = validate_dataset(&with_blueprints(), None);
        assert!(r.ok(), "{:?}", r.errors);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.stats.blueprints, 2);
        // A pool chance anywhere from 0 to 1 (both included) is accepted.
        for c in [0.0, 0.25, 1.0] {
            let mut ds = with_blueprints();
            ds.blueprints[0].sources[0].pool_chance = Some(c);
            let r = validate_dataset(&ds, None);
            assert!(r.ok(), "pool chance {c}: {:?}", r.errors);
        }
    }

    #[test]
    fn bad_blueprints_are_errors() {
        let rejects = |what: &str, change: &dyn Fn(&mut Blueprint)| {
            let mut ds = with_blueprints();
            change(&mut ds.blueprints[0]);
            let r = validate_dataset(&ds, None);
            assert!(!r.ok(), "{what} should be refused");
            assert!(r.errors.iter().all(|e| e.chars().count() < 600), "{what}: errors stay short: {:?}", r.errors);
        };
        rejects("duplicate id", &|b| b.id = "b2".into());
        rejects("unsafe id", &|b| b.id = "../x".into());
        rejects("unsafe record name", &|b| b.name = "a b".into());
        rejects("unsafe output class", &|b| b.output_class_name = "../etc".into());
        rejects("dangling output item", &|b| b.output_item_id = Some("nope".into()));
        rejects("control characters", &|b| b.output_name = "a\u{0}b".into());
        rejects("long category", &|b| b.category = Some("x".repeat(MAX_SHORT + 1)));
        rejects("infinite time", &|b| b.craft_time_sec = f64::INFINITY);
        rejects("negative time", &|b| b.craft_time_sec = -1.0);
        rejects("absurd time", &|b| b.craft_time_sec = MAX_CRAFT_TIME_SEC * 2.0);
        rejects("too many slots", &|b| b.slots = vec![b.slots[0].clone(); MAX_BLUEPRINT_PARTS + 1]);
        rejects("too many optional slots", &|b| b.optional_slots = vec![b.slots[0].clone(); MAX_BLUEPRINT_PARTS + 1]);
        rejects("too many options", &|b| b.slots[0].options = vec![b.slots[0].options[0].clone(); MAX_BLUEPRINT_PARTS + 1]);
        rejects("too many effects", &|b| b.slots[0].quality_effects = vec![b.slots[0].quality_effects[0].clone(); MAX_BLUEPRINT_PARTS + 1]);
        rejects("negative quantity", &|b| b.slots[0].options[0].qty = -1.0);
        rejects("NaN quantity", &|b| b.slots[0].options[0].qty = f64::NAN);
        rejects("absurd quantity", &|b| b.slots[0].options[0].qty = MAX_BLUEPRINT_VALUE * 2.0);
        rejects("quality above the scale", &|b| b.slots[0].options[0].min_quality = MAX_QUALITY + 1);
        rejects("negative quality", &|b| b.slots[0].options[0].min_quality = -1);
        rejects("dangling ingredient", &|b| b.slots[0].options[0].item_id = Some("nope".into()));
        rejects("multiplier of a missing option", &|b| b.slots[0].quantity_multipliers[0].option = Some(1));
        rejects("NaN multiplier", &|b| b.slots[0].quantity_multipliers[0].multiplier = f64::NAN);
        rejects("unsafe effect key", &|b| b.slots[0].quality_effects[0].key = "x".repeat(10_000));
        rejects("too many ranges", &|b| {
            let range = b.slots[0].quality_effects[0].ranges[0].clone();
            b.slots[0].quality_effects[0].ranges = vec![range; MAX_QUALITY_RANGES + 1];
        });
        rejects("range above the scale", &|b| b.slots[0].quality_effects[0].ranges[0].end_quality = MAX_QUALITY + 1);
        rejects("negative range start", &|b| b.slots[0].quality_effects[0].ranges[0].start_quality = -1);
        rejects("infinite modifier", &|b| b.slots[0].quality_effects[0].ranges[0].at_start = f64::INFINITY);
        rejects("absurd modifier", &|b| b.slots[0].quality_effects[0].ranges[0].at_end = -MAX_BLUEPRINT_VALUE * 2.0);
        rejects("chance above one", &|b| b.sources[0].chance = 1.5);
        rejects("negative chance", &|b| b.sources[0].chance = -0.1);
        rejects("NaN chance", &|b| b.sources[0].chance = f64::NAN);
        rejects("pool chance above one", &|b| b.sources[0].pool_chance = Some(1.01));
        rejects("negative pool chance", &|b| b.sources[0].pool_chance = Some(-0.1));
        rejects("NaN pool chance", &|b| b.sources[0].pool_chance = Some(f64::NAN));
        rejects("infinite pool chance", &|b| b.sources[0].pool_chance = Some(f64::INFINITY));
        rejects("unknown moment", &|b| b.sources[0].when = "later".into());
        rejects("bad contract id", &|b| b.sources[0].contract_id = String::new());
        rejects("long title", &|b| b.sources[0].title = "x".repeat(MAX_SHORT + 1));
        rejects("too many sources of one blueprint", &|b| b.sources = vec![source("c1", 0.1); MAX_SOURCES_PER_BLUEPRINT + 1]);
        rejects("empty item type", &|b| b.item_type = Some(String::new()));
        rejects("long item type", &|b| b.item_type = Some("x".repeat(MAX_BLUEPRINT_LABEL + 1)));
        rejects("item type with a newline", &|b| b.item_type = Some("Weapon\nGun".into()));
        rejects("long sub type", &|b| b.item_sub_type = Some("x".repeat(MAX_BLUEPRINT_LABEL + 1)));
        rejects("sub type with a control character", &|b| b.item_sub_type = Some("a\u{7}".into()));
        rejects("empty class", &|b| b.item_class = Some(String::new()));
        rejects("long class", &|b| b.item_class = Some("é".repeat(MAX_BLUEPRINT_LABEL + 1)));
        rejects("size above 12", &|b| b.size = Some(MAX_ITEM_SIZE + 1));
        rejects("grade 0", &|b| b.grade = Some(0));
        rejects("grade above 9", &|b| b.grade = Some(MAX_ITEM_GRADE + 1));
        rejects("empty manufacturer code", &|b| b.manufacturer = Some(BlueprintManufacturer { code: String::new(), name: "Behring".into() }));
        rejects("long manufacturer code", &|b| b.manufacturer = Some(BlueprintManufacturer { code: "X".repeat(MAX_MANUFACTURER_CODE + 1), name: "Behring".into() }));
        rejects("empty manufacturer name", &|b| b.manufacturer = Some(BlueprintManufacturer { code: "BEHR".into(), name: String::new() }));
        rejects("long manufacturer name", &|b| b.manufacturer = Some(BlueprintManufacturer { code: "BEHR".into(), name: "x".repeat(MAX_MANUFACTURER_NAME + 1) }));
        rejects("manufacturer name with a tab", &|b| b.manufacturer = Some(BlueprintManufacturer { code: "BEHR".into(), name: "Beh\tring".into() }));
        rejects("too many tags", &|b| b.tags = vec!["a".into(); MAX_BLUEPRINT_TAGS + 1]);
        rejects("empty tag", &|b| b.tags = vec![String::new()]);
        rejects("long tag", &|b| b.tags = vec!["a".repeat(MAX_BLUEPRINT_TAG + 1)]);
        rejects("upper-case tag", &|b| b.tags = vec!["FPSGear".into()]);
        rejects("tag with a separator", &|b| b.tags = vec!["fpsgear/weapons".into()]);
        rejects("tag with a space", &|b| b.tags = vec!["fps gear".into()]);

        let mut ds = sample();
        ds.blueprints = (0..=MAX_BLUEPRINTS).map(|n| blueprint(&format!("b{n}"))).collect();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.starts_with("blueprints:")), "too many blueprints");
        let mut ds = with_blueprints();
        let many = vec![source("c1", 0.1); MAX_SOURCES_PER_BLUEPRINT];
        ds.blueprints = (0..MAX_BLUEPRINT_SOURCES / MAX_SOURCES_PER_BLUEPRINT + 1).map(|n| Blueprint { sources: many.clone(), ..blueprint(&format!("b{n}")) }).collect();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.starts_with("origens de blueprint")), "too many sources overall");
    }

    #[test]
    fn odd_blueprints_are_warnings() {
        let warns = |what: &str, change: &dyn Fn(&mut Blueprint)| {
            let mut ds = with_blueprints();
            change(&mut ds.blueprints[0]);
            let r = validate_dataset(&ds, None);
            assert!(r.ok(), "{what}: {:?}", r.errors);
            assert!(!r.warnings.is_empty(), "{what} should warn");
        };
        warns("a slot asking for nothing", &|b| b.slots[0].count = 0);
        warns("more slots required than listed", &|b| b.slots_required = Some(2));
        warns("an inverted range", &|b| {
            let r = &mut b.slots[0].quality_effects[0].ranges[0];
            r.start_quality = 900;
            r.end_quality = 100;
        });
        warns("an unknown source kind", &|b| b.sources[0].kind = "shop".into());
    }

    #[test]
    fn bad_locations_are_errors() {
        let r = validate_dataset(&sample(), None);
        assert_eq!(r.stats.locations, 2);

        let mut ds = sample();
        ds.locations[1].id = "Stanton2_Orison".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("repetido")), "duplicate id");
        let mut ds = sample();
        ds.locations[0].id = "../x".into();
        assert!(!validate_dataset(&ds, None).ok(), "unsafe id");
        let mut ds = sample();
        ds.locations[0].id = "A".repeat(129);
        assert!(!validate_dataset(&ds, None).ok(), "id too long");
        let mut ds = sample();
        ds.locations[0].name = "Ori\u{7}son".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("controle")));
        let mut ds = sample();
        ds.locations[0].name = "x".repeat(201);
        assert!(!validate_dataset(&ds, None).ok(), "name too long");
        let mut ds = sample();
        ds.locations[0].name = " ".into();
        assert!(!validate_dataset(&ds, None).ok(), "blank name");
        let mut ds = sample();
        ds.locations = (0..=MAX_LOCATIONS).map(|n| location(&format!("L{n}"), "x")).collect();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("locais")));
        let mut ds = sample();
        ds.locations[0].kind = "moon".into();
        let r = validate_dataset(&ds, None);
        assert!(r.ok() && r.warnings.iter().any(|w| w.contains("kind")), "{:?}", r.warnings);
    }

    #[test]
    fn valid_sample_passes() {
        let r = validate_dataset(&sample(), None);
        assert!(r.ok(), "{:?}", r.errors);
        assert_eq!(r.stats.contracts, 2);
        assert_eq!(r.stats.with_image, 2);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn dangling_and_unsafe_values_are_errors() {
        let mut ds = sample();
        ds.contracts[0].rewards[0].item_id = Some("nope".into());
        let r = validate_dataset(&ds, None);
        assert!(r.errors.iter().any(|e| e.contains("item nope")), "{:?}", r.errors);

        let mut ds = sample();
        ds.items[0].class_name = "../etc/passwd".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("classe inválido")));
        let mut ds = sample();
        ds.items[0].class_name = "ResourceType:Gold".into();
        assert!(validate_dataset(&ds, None).ok(), "resources carry a colon");

        let mut ds = sample();
        ds.items[0].image = Some("../../x.png".into());
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("imagem inválida")));

        let mut ds = sample();
        ds.contracts[0].title = "a\u{0}b".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("controle")));

        let mut ds = sample();
        ds.build.version = "4.10; DROP".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("versão inválida")));

        let mut ds = sample();
        ds.contracts.clear();
        assert!(!validate_dataset(&ds, None).ok());
    }

    #[test]
    fn favor_conversion_quantities_are_bounded() {
        let conv = |give: i64| FavorConversion {
            contract_id: "c1".into(),
            give: vec![QtyRef { item_id: Some("i2".into()), name: "MG Scrip".into(), qty: give, unit: Unit::Count }],
            receive: vec![QtyRef { item_id: Some("i1".into()), name: "Favor".into(), qty: 1, unit: Unit::Count }],
        };
        let mut ds = sample();
        ds.favor_conversions = vec![conv(50), conv(MAX_CONVERSION_QTY)];
        let r = validate_dataset(&ds, None);
        assert!(r.ok(), "{:?}", r.errors);
        ds.favor_conversions = vec![conv(MAX_CONVERSION_QTY + 1)];
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("grande demais")));
        ds.favor_conversions = vec![conv(-1)];
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("negativa")));
    }

    #[test]
    fn step_backwards_is_a_warning() {
        let active = sample();
        let mut older = sample();
        older.build.version = "4.9.0.1".into();
        older.contracts.truncate(1);
        let r = validate_dataset(&older, Some(&active));
        assert!(r.ok());
        assert!(r.warnings.iter().any(|w| w.contains("mais antiga")), "{:?}", r.warnings);
        assert!(r.warnings.iter().any(|w| w.contains("mudança grande")), "{:?}", r.warnings);
        assert!(validate_dataset(&sample(), Some(&active)).warnings.is_empty());
    }

    #[test]
    fn p4_change_must_be_a_change_number() {
        for bad in ["9".repeat(20), "12a4".to_string(), "-1".to_string(), " 1".to_string()] {
            let mut ds = sample();
            ds.build.p4_change = bad.clone();
            assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.starts_with("p4Change inválido")), "{bad:?}");
        }
        let longest = "9".repeat(18);
        for good in ["", "1", "12913470", longest.as_str()] {
            let mut ds = sample();
            ds.build.p4_change = good.to_string();
            assert!(validate_dataset(&ds, None).ok(), "{good:?}");
        }
    }

    #[test]
    fn p4_change_and_version_disagreeing_is_a_warning() {
        let active = sample(); // 4.10.197.2878, p4Change 12913470
        let disagree = |r: &Report| r.warnings.iter().any(|w| w.contains("vale o p4Change"));
        // A newer change number with an older version: the change number decides (newer).
        let mut ds = sample();
        ds.build.p4_change = "12913471".into();
        ds.build.version = "4.9.0.1".into();
        let r = validate_dataset(&ds, Some(&active));
        assert!(r.ok() && disagree(&r), "{:?}", r.warnings);
        assert!(!r.warnings.iter().any(|w| w.contains("fica guardada")), "not older by the server's rule: {:?}", r.warnings);
        // The reverse: an older change number with a newer version is older.
        let mut ds = sample();
        ds.build.p4_change = "12913469".into();
        ds.build.version = "4.11.0.1".into();
        let r = validate_dataset(&ds, Some(&active));
        assert!(disagree(&r) && r.warnings.iter().any(|w| w.contains("mais antiga")), "{:?}", r.warnings);
        // Both newer: nothing to say.
        let mut ds = sample();
        ds.build.p4_change = "12913471".into();
        ds.build.version = "4.11.0.1".into();
        assert!(!disagree(&validate_dataset(&ds, Some(&active))));
    }

    #[test]
    fn blueprint_names_are_unique_and_url_safe() {
        assert!(valid_blueprint_name("BP_CRAFT_behr_rifle_ballistic_01") && valid_blueprint_name(&"a".repeat(120)));
        for bad in ["", "a b", "BP:x", "../x", "ação"] {
            assert!(!valid_blueprint_name(bad), "{bad:?}");
        }
        assert!(!valid_blueprint_name(&"a".repeat(121)));

        let mut ds = with_blueprints();
        ds.blueprints[1].name = ds.blueprints[0].name.clone();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("nome repetido")), "duplicate name");
        let long = "B".repeat(121);
        for bad in ["BP:CRAFT", long.as_str()] {
            let mut ds = with_blueprints();
            ds.blueprints[0].name = bad.to_string();
            assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("nome de registro inválido")), "{bad:?}");
        }
    }

    #[test]
    fn blueprint_ingredient_kind_is_bounded() {
        let mut ds = with_blueprints();
        ds.blueprints[0].slots[0].options[0].kind = "item".into();
        assert!(validate_dataset(&ds, None).ok());
        ds.blueprints[0].slots[0].options[0].kind = "ship".into();
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("kind desconhecido")));
        let mut ds = with_blueprints();
        ds.blueprints[0].optional_slots = vec![ds.blueprints[0].slots[0].clone()];
        ds.blueprints[0].optional_slots[0].options[0].kind = "x".repeat(10_000);
        let r = validate_dataset(&ds, None);
        assert!(!r.ok() && r.errors.iter().all(|e| e.chars().count() < 600), "{:?}", r.errors.len());
    }

    #[test]
    fn blueprints_are_bounded_in_total_size() {
        let mut ds = with_blueprints();
        // About 18 MB of sources on one blueprint.
        let big = BlueprintSource { pool: "p".repeat(4000), ..source("c1", 0.1) };
        ds.blueprints[0].sources = vec![big; 4500];
        assert!(validate_dataset(&ds, None).errors.iter().any(|e| e.contains("MB no total")));
        assert!(!validate_dataset(&with_blueprints(), None).errors.iter().any(|e| e.contains("MB no total")));
    }

    #[test]
    fn losing_every_blueprint_is_a_warning() {
        let active = with_blueprints();
        let r = validate_dataset(&sample(), Some(&active));
        assert!(r.ok() && r.warnings.iter().any(|w| w.contains("nenhuma blueprint")), "{:?}", r.warnings);
        assert!(!validate_dataset(&with_blueprints(), Some(&active)).warnings.iter().any(|w| w.contains("nenhuma blueprint")));
        assert!(!validate_dataset(&sample(), Some(&sample())).warnings.iter().any(|w| w.contains("nenhuma blueprint")));
    }
}

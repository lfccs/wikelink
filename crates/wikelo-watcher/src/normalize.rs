//! Walks the materialized `ContractGenerator.TheCollector` tree (plus the
//! records it references) and produces the normalized [`WikeloDataset`].
//!
//! The walker is deliberately tolerant: it matches nodes by `_Type_` and
//! property name, records anything unexpected as a warning, and never panics
//! on a missing field.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use serde_json::Value;
use wikelo_core::*;

use crate::loc::{Localization, plain_text};
use crate::resolver::{RefResolver, record_value, ref_id};

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("record {0} not found in DataCore")]
    GeneratorMissing(String),
    #[error("export error: {0}")]
    Export(#[from] starbreaker_datacore::ExportError),
}

/// Known Wikelo Emporium locations: (class name suffix, key, location hash seen in Game.log).
const KNOWN_EMPORIUMS: &[(&str, &str, u64)] = &[
    ("Stanton1", "dasi", 1_231_535_936),
    ("Stanton2", "selo", 1_615_454_559),
    ("Stanton4", "kinga", 3_168_785_171),
];

#[derive(Debug, Clone, Default)]
struct Availability {
    once_only: bool,
    hide_in_mobiglas: bool,
    max_players: Option<i64>,
    abandoned_cooldown: f64,
    personal_cooldown: f64,
    can_reaccept_after_abandoning: bool,
    can_reaccept_after_failing: bool,
}

impl Availability {
    fn from_default(v: &Value) -> Self {
        Self {
            once_only: b(v, "onceOnly").unwrap_or(false),
            hide_in_mobiglas: b(v, "hideInMobiGlas").unwrap_or(false),
            max_players: i(v, "maxPlayersPerInstance"),
            abandoned_cooldown: f(v, "abandonedCooldownTime").unwrap_or(0.0),
            personal_cooldown: f(v, "personalCooldownTime").unwrap_or(0.0),
            can_reaccept_after_abandoning: b(v, "canReacceptAfterAbandoning").unwrap_or(false),
            can_reaccept_after_failing: b(v, "canReacceptAfterFailing").unwrap_or(false),
        }
    }

    /// Apply `boolParamOverrides` / `intParamOverrides` of a `ContractParamOverrides` node.
    fn apply_overrides(&mut self, params: Option<&Value>) {
        let Some(params) = params else { return };
        for o in arr(params, "boolParamOverrides") {
            let (Some(p), Some(v)) = (s(o, "param"), b(o, "value")) else { continue };
            match p.to_ascii_lowercase().as_str() {
                "onceonly" => self.once_only = v,
                "hideinmobiglas" => self.hide_in_mobiglas = v,
                "canreacceptafterabandoning" => self.can_reaccept_after_abandoning = v,
                "canreacceptafterfailing" => self.can_reaccept_after_failing = v,
                _ => {}
            }
        }
        for o in arr(params, "intParamOverrides") {
            let (Some(p), Some(v)) = (s(o, "param"), i(o, "value")) else { continue };
            match p.to_ascii_lowercase().as_str() {
                "maxplayersperinstance" => self.max_players = Some(v),
                "abandonedcooldowntime" => self.abandoned_cooldown = v as f64,
                "personalcooldowntime" => self.personal_cooldown = v as f64,
                _ => {}
            }
        }
    }
}

struct EmporiumInfo {
    emporium: Emporium,
    tag_ids: HashSet<String>,
}

pub struct Normalizer<'r, 'db, 'a> {
    res: &'r mut RefResolver<'db, 'a>,
    loc: &'r Localization,
    tag_names: HashMap<String, String>,
    resource_names: HashMap<String, String>,
    /// Resource record id -> the quality values it can have (see [`Item::qualities`]).
    resource_qualities: HashMap<String, Vec<i64>>,
    /// Quality table (`CraftingQualityQuantizationRecord`) id -> every resource whose
    /// `qualityQuantization` property points at it, with the refined resource it turns into.
    quantization_users: HashMap<String, Vec<(String, Option<String>)>>,
    /// Resource record name without `ResourceType.`, in lower case -> the resource's id.
    resource_ids_by_record: HashMap<String, String>,
    items: BTreeMap<String, Item>,
    emporiums: Vec<EmporiumInfo>,
    scope_ids: HashSet<String>,
    warnings: Vec<String>,
    /// Blueprint record id -> (contract id, granted on accept, chance).
    blueprint_sources: HashMap<String, Vec<(String, bool, f64)>>,
    /// Gameplay property record id -> its English name (see [`localized_name`]).
    property_names: HashMap<String, Option<String>>,
}

impl<'r, 'db, 'a> Normalizer<'r, 'db, 'a> {
    pub fn new(res: &'r mut RefResolver<'db, 'a>, loc: &'r Localization) -> Self {
        Self {
            res,
            loc,
            tag_names: HashMap::new(),
            resource_names: HashMap::new(),
            resource_qualities: HashMap::new(),
            quantization_users: HashMap::new(),
            resource_ids_by_record: HashMap::new(),
            items: BTreeMap::new(),
            emporiums: Vec::new(),
            scope_ids: HashSet::new(),
            warnings: Vec::new(),
            blueprint_sources: HashMap::new(),
            property_names: HashMap::new(),
        }
    }

    pub fn run(mut self, build: BuildInfo, generated_at: String) -> Result<WikeloDataset, NormalizeError> {
        let generator = self
            .res
            .find_by_name("ContractGenerator", GENERATOR_RECORD_NAME)
            .ok_or_else(|| NormalizeError::GeneratorMissing(GENERATOR_RECORD_NAME.to_string()))?;
        let root = record_value(&self.res.materialize(&generator)?);

        self.load_tag_names();
        self.load_resource_names();
        self.load_emporiums();

        let mut contracts: Vec<Contract> = Vec::new();
        for g in arr(&root, "generators") {
            let gen_debug = s(g, "debugName").unwrap_or("?").to_string();
            let mut defaults = g.get("defaultAvailability").map(Availability::from_default).unwrap_or_default();
            defaults.apply_overrides(g.get("contractParams"));
            let gen_prereqs: Vec<Value> = g
                .get("defaultAvailability")
                .map(|d| arr(d, "prerequisites").to_vec())
                .unwrap_or_default();
            let gen_mission_type = g.get("contractParams").and_then(|p| p.get("missionTypeOverride")).cloned();
            let gen_dropoff = g
                .get("contractParams")
                .map(|p| find_property(arr(p, "propertyOverrides"), "DropoffLocation_BP"))
                .unwrap_or(None)
                .cloned();
            for key in ["contracts", "introContracts", "legacyContracts"] {
                for c in arr(g, key) {
                    match self.contract(c, &defaults, &gen_prereqs, gen_mission_type.as_ref(), gen_dropoff.as_ref()) {
                        Some(contract) => contracts.push(contract),
                        None => self.warnings.push(format!("skipped a contract in generator {gen_debug} ({key})")),
                    }
                }
            }
        }

        // Required contracts from completion tags.
        let mut tag_to_contracts: HashMap<String, Vec<String>> = HashMap::new();
        for c in &contracts {
            for t in &c.completion_tags {
                tag_to_contracts.entry(t.id.clone()).or_default().push(c.id.clone());
            }
        }
        for c in &mut contracts {
            let mut ids: Vec<String> = Vec::new();
            for t in &c.prerequisites.required_tags {
                if let Some(v) = tag_to_contracts.get(&t.id) {
                    ids.extend(v.iter().cloned());
                }
            }
            ids.sort();
            ids.dedup();
            c.prerequisites.required_contract_ids = ids;
        }
        contracts.sort_by(|a, b| a.category.order().cmp(&b.category.order()).then_with(|| a.title.cmp(&b.title)));

        let crafting = self.load_crafting(&contracts);
        let mission_rewards = self.load_mission_rewards(&contracts);

        let favor_conversions = contracts
            .iter()
            .filter(|c| c.category == Category::Favour)
            .map(|c| FavorConversion {
                contract_id: c.id.clone(),
                give: c
                    .requirements
                    .iter()
                    .map(|r| QtyRef { item_id: r.item_id.clone(), name: r.name.clone(), qty: r.qty, unit: r.unit })
                    .collect(),
                receive: c
                    .rewards
                    .iter()
                    .map(|r| QtyRef { item_id: r.item_id.clone(), name: r.name.clone(), qty: r.qty, unit: Unit::Count })
                    .collect(),
            })
            .collect();

        let faction = self.faction();
        // Last of the item readers: it looks the catalog up without adding to it.
        let blueprints = self.load_blueprints(&contracts, &faction.name);
        // After the blueprints: a material is named the way their slots name it.
        let quality_tiers = self.load_quality_tiers(&blueprints);
        let emporiums: Vec<Emporium> = self.emporiums.iter().map(|e| e.emporium.clone()).collect();
        let mut warnings = std::mem::take(&mut self.warnings);
        warnings.extend(std::mem::take(&mut self.res.warnings));

        let locations = self.load_locations(&emporiums);
        Ok(WikeloDataset {
            schema_version: SCHEMA_VERSION,
            source: "local-extract".into(),
            build,
            generated_at,
            faction,
            emporiums,
            contracts,
            items: self.items.into_values().collect(),
            favor_conversions,
            crafting,
            warnings,
            mission_rewards,
            mission_drops: Vec::new(), // filled by `drops` once the contracts and items are known
            missions: Vec::new(),      // filled by `missions` from the rewards and drops above
            locations,
            blueprints,
            quality_tiers,
            extractor: None,
        })
    }

    /// Rewards paid by *other* contract givers in items: the ones the Wikelo tracker already
    /// knows (MG Scrip, Council Scrip, secure drives...) and the few one-off item rewards of
    /// event generators (Battaglia's armor, weapons and ships), which join the item catalog
    /// here. A completion seen in Game.log can then credit the inventory with the exact
    /// quantity from the game data. aUEC, reputation, badges and scenario points are not items.
    fn load_mission_rewards(&mut self, wikelo: &[Contract]) -> Vec<MissionReward> {
        let known: HashMap<String, Item> = self.items.values().map(|i| (i.id.clone(), i.clone())).collect();
        let wikelo_ids: HashSet<&str> = wikelo.iter().map(|c| c.id.as_str()).collect();
        let mut out = Vec::new();
        for (name, record) in self.res.records_of_type("ContractGenerator") {
            if name == GENERATOR_RECORD_NAME {
                continue;
            }
            let root = match self.res.materialize(&record) {
                Ok(v) => record_value(&v),
                Err(e) => {
                    self.warnings.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let generator = name.trim_start_matches("ContractGenerator.").to_string();
            for g in arr(&root, "generators") {
                for key in ["contracts", "introContracts", "legacyContracts"] {
                    for c in arr(g, key) {
                        let Some(id) = s(c, "id") else { continue };
                        if wikelo_ids.contains(id) {
                            continue;
                        }
                        let debug_name = s(c, "debugName").unwrap_or("?").trim().to_string();
                        let Some(results) = c.get("contractResults") else { continue };
                        for r in arr(results, "contractResults") {
                            if ty(r) != Some("ContractResult_Item") || !applies_on_success(r) {
                                continue;
                            }
                            let Some(eref) = r.get("entityClass") else { continue };
                            let Some(item) = self.item(eref) else { continue };
                            out.push(MissionReward {
                                contract_id: id.to_string(),
                                debug_name: debug_name.clone(),
                                generator: generator.clone(),
                                item_id: item.id.clone(),
                                class_name: item.class_name.clone(),
                                name: item.name.clone(),
                                qty: i(r, "amount").unwrap_or(1),
                                owner_only: b(r, "awardOnlyToMissionOwner").unwrap_or(true),
                                one_of: false,
                            });
                        }
                    }
                }
            }
        }
        out.extend(self.sample_rewards(&known));
        out.sort_by(|x, y| x.generator.cmp(&y.generator).then(x.debug_name.cmp(&y.debug_name)).then(x.name.cmp(&y.name)));
        out
    }

    /// Project Hyperion (Hockrow's phase-3 facility delve) pays its recombinant with a physical
    /// sample box spawned in the lab, not with a contract result, so the generator record never
    /// names the item. The player picks one of the nine `Carryable_TBO_ASDReward_*` samples and
    /// Game.log does not say which (every alternative completes at once), hence `one_of`: the
    /// site asks the player when the mission is seen to complete. Checked on 4.10 logs.
    fn sample_rewards(&mut self, known: &HashMap<String, Item>) -> Vec<MissionReward> {
        const GENERATOR_RECORD: &str = "ContractGenerator.HockrowAgency_FacilityDelve";
        const MISSION_PREFIX: &str = "Hockrow_FacilityDelve_P3MM";
        const SAMPLE_PREFIX: &str = "carryable_tbo_asdreward_";
        let generator = GENERATOR_RECORD.trim_start_matches("ContractGenerator.");
        let Some(record) = self.res.find_by_name("ContractGenerator", GENERATOR_RECORD) else { return Vec::new() };
        let Ok(v) = self.res.materialize(&record) else { return Vec::new() };
        let root = record_value(&v);
        let mut samples: Vec<&Item> = known.values().filter(|i| i.class_name.to_ascii_lowercase().starts_with(SAMPLE_PREFIX)).collect();
        samples.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = Vec::new();
        for g in arr(&root, "generators") {
            for c in arr(g, "contracts") {
                let (Some(id), Some(debug_name)) = (s(c, "id"), s(c, "debugName")) else { continue };
                if !debug_name.trim().starts_with(MISSION_PREFIX) {
                    continue;
                }
                out.extend(samples.iter().map(|item| MissionReward {
                    contract_id: id.to_string(),
                    debug_name: debug_name.trim().to_string(),
                    generator: generator.to_string(),
                    item_id: item.id.clone(),
                    class_name: item.class_name.clone(),
                    name: item.name.clone(),
                    qty: 1,
                    owner_only: false,
                    one_of: true,
                }));
            }
        }
        out
    }

    // ----- crafting ---------------------------------------------------------------

    /// Recipes (`CraftingBlueprintRecord`) whose output is an item some contract
    /// asks for or hands out, or whose blueprint a contract grants.
    fn load_crafting(&mut self, contracts: &[Contract]) -> Vec<CraftRecipe> {
        let titles: HashMap<String, String> = contracts.iter().map(|c| (c.id.clone(), c.title.clone())).collect();
        let mut out: Vec<CraftRecipe> = Vec::new();
        for (name, rec) in self.res.records_of_type("CraftingBlueprintRecord") {
            let Ok(v) = self.res.materialize(&rec) else { continue };
            let root = record_value(&v);
            let Some(bp) = root.get("blueprint") else { continue };
            let Some(entity) = bp.get("processSpecificData").and_then(|p| p.get("entityClass")).cloned() else { continue };
            let Some(out_id) = ref_id(&entity).map(str::to_string) else { continue };
            let bp_id = rec.id.to_string();
            if !self.items.contains_key(&out_id) && !self.blueprint_sources.contains_key(&bp_id) {
                continue;
            }
            let Some(item) = self.item(&entity) else { continue };
            let tiers = arr(bp, "tiers");
            let Some(tier) = tiers.first() else { continue };
            let costs = tier.get("recipe").and_then(|r| r.get("costs"));
            let mut slots: Vec<CraftSlot> = Vec::new();
            if let Some(costs) = costs {
                if let Some(m) = costs.get("mandatoryCost") {
                    self.craft_slots(m, &mut slots);
                }
                for o in arr(costs, "optionalCosts") {
                    self.craft_slots(o, &mut slots);
                }
            }
            let craft_time_sec = craft_time_sec(costs);
            let category = bp.get("category").and_then(|c| self.res.ref_name(c)).map(|n| n.trim_start_matches("BlueprintCategoryRecord.").to_string());
            let sources = self
                .blueprint_sources
                .get(&bp_id)
                .map(|v| {
                    v.iter()
                        .map(|(cid, on_accept, chance)| CraftSource {
                            contract_id: cid.clone(),
                            contract_title: titles.get(cid).cloned().unwrap_or_default(),
                            when: if *on_accept { "accept".into() } else { "complete".into() },
                            chance: *chance,
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(CraftRecipe {
                blueprint_id: bp_id,
                blueprint_name: name.trim_start_matches("CraftingBlueprintRecord.").to_string(),
                output_item_id: item.id.clone(),
                output_class_name: item.class_name.clone(),
                output_name: item.name.clone(),
                category,
                craft_time_sec,
                slots,
                sources,
            });
        }
        out.sort_by(|a, b| a.output_name.cmp(&b.output_name));
        out
    }

    /// Flatten a `CraftingCost_Select` tree into slots (a slot = pick N of its options).
    fn craft_slots(&mut self, cost: &Value, slots: &mut Vec<CraftSlot>) {
        match ty(cost) {
            Some("CraftingCost_Select") => {
                let opts = arr(cost, "options");
                let leaf = opts.iter().any(|o| matches!(ty(o), Some("CraftingCost_Resource") | Some("CraftingCost_Item")));
                if leaf {
                    let name = self.craft_slot_name(cost);
                    let count = i(cost, "count").unwrap_or(1).max(1) as u32;
                    let options: Vec<CraftInput> = opts.iter().filter_map(|o| self.craft_input(o)).collect();
                    if !options.is_empty() {
                        slots.push(CraftSlot { name, count, options });
                    }
                } else {
                    for o in opts {
                        self.craft_slots(o, slots);
                    }
                }
            }
            Some("CraftingCost_Resource") | Some("CraftingCost_Item") => {
                if let Some(inp) = self.craft_input(cost) {
                    slots.push(CraftSlot { name: inp.name.clone(), count: 1, options: vec![inp] });
                }
            }
            _ => {}
        }
    }

    /// English name of a slot (`CraftingCost_Select`): its display name, else its debug name in
    /// title case.
    fn craft_slot_name(&self, cost: &Value) -> String {
        let info = cost.get("nameInfo");
        info.and_then(|n| s(n, "displayName"))
            .and_then(|k| self.loc.text_opt(k))
            .or_else(|| info.and_then(|n| s(n, "debugName")).map(title_case))
            .unwrap_or_else(|| "Material".into())
    }

    /// Name of the resource a `CraftingCost_Resource` (`o`, resource id `rid`) asks for.
    fn resource_name(&self, o: &Value, rid: &str) -> String {
        self.resource_names
            .get(rid)
            .cloned()
            .or_else(|| o.get("resource").and_then(|r| self.res.ref_name(r)).map(|n| n.trim_start_matches("ResourceType.").to_string()))
            .unwrap_or_else(|| "?".into())
    }

    fn craft_input(&mut self, o: &Value) -> Option<CraftInput> {
        match ty(o)? {
            "CraftingCost_Resource" => {
                let rid = o.get("resource").and_then(ref_id)?.to_string();
                let name = self.resource_name(o, &rid);
                self.ensure_resource_item(&rid, &name);
                let qty = o.get("quantity").and_then(|q| f(q, "standardCargoUnits")).unwrap_or(0.0);
                Some(CraftInput {
                    kind: "resource".into(),
                    item_id: Some(rid),
                    class_name: None,
                    name,
                    qty,
                    unit: Unit::Scu,
                    min_quality: i(o, "minQuality").unwrap_or(0),
                })
            }
            "CraftingCost_Item" => {
                let item = o.get("entityClass").and_then(|e| self.item(e))?;
                Some(CraftInput {
                    kind: "item".into(),
                    item_id: Some(item.id.clone()),
                    class_name: Some(item.class_name.clone()),
                    name: item.name.clone(),
                    qty: i(o, "quantity").unwrap_or(1) as f64,
                    unit: Unit::Count,
                    min_quality: i(o, "minQuality").unwrap_or(0),
                })
            }
            _ => None,
        }
    }

    /// Resources are tracked like items (inventory in SCU); make sure one exists.
    fn ensure_resource_item(&mut self, rid: &str, name: &str) {
        self.items.entry(rid.to_string()).or_insert_with(|| Item {
            id: rid.to_string(),
            class_name: format!("ResourceType:{name}"),
            name: name.to_string(),
            item_type: Some("Resource".into()),
            sub_type: None,
            kind: "commodity".into(),
            image: None,
            image_source: None,
            qualities: self.resource_qualities.get(rid).cloned().unwrap_or_default(),
        });
    }

    // ----- blueprints ----------------------------------------------------------------

    /// Every creation blueprint of the build with where to get it. Unlike [`Self::load_crafting`]
    /// it adds nothing to the item catalog (ids are set only for what the catalog holds), so it
    /// runs once the catalog is complete.
    fn load_blueprints(&mut self, contracts: &[Contract], faction_name: &str) -> Vec<Blueprint> {
        let mut notes = BlueprintNotes::default();
        let mut sources = self.blueprint_sources(contracts, faction_name, &mut notes);
        let defaults = self.default_blueprints();
        if defaults.is_none() {
            notes.no_default_list = 1;
        }
        let mut out: Vec<Blueprint> = Vec::new();
        for (name, rec) in self.res.records_of_type("CraftingBlueprintRecord") {
            let root = match self.res.materialize(&rec) {
                Ok(v) => record_value(&v),
                Err(e) => {
                    self.warnings.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let id = rec.id.to_string();
            let Some(mut bp) = blueprint_from_record(&id, short_record_name(&name), &root, self, &mut notes) else { continue };
            // The record's own file in the game data (`libs/foundry/records/crafting/blueprints/...`).
            let file_offset = rec.file_name_offset;
            let path = self.res.db().resolve_string(file_offset).to_string();
            describe_blueprint_item(&mut bp, &root, &path, defaults.as_ref(), self, &mut notes);
            bp.sources = sources.remove(&id).unwrap_or_default();
            out.push(bp);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        let with_type = out.iter().filter(|b| b.item_type.is_some()).count();
        let with_manufacturer = out.iter().filter(|b| b.manufacturer.is_some()).count();
        let defaults_n = out.iter().filter(|b| b.default).count();
        tracing::info!("blueprint items: {with_type} with an item type, {with_manufacturer} with a manufacturer, {defaults_n} default");
        let with_sources = out.iter().filter(|b| !b.sources.is_empty()).count();
        let with_offered = out.iter().filter(|b| b.sources.iter().any(|src| !src.disabled)).count();
        let with_effects = out.iter().filter(|b| b.slots.iter().chain(&b.optional_slots).any(|slot| !slot.quality_effects.is_empty())).count();
        let mut per_category: BTreeMap<&str, usize> = BTreeMap::new();
        for b in &out {
            *per_category.entry(b.category.as_deref().unwrap_or("-")).or_default() += 1;
        }
        let categories = per_category.iter().map(|(c, n)| format!("{c} {n}")).collect::<Vec<_>>().join(", ");
        tracing::info!(
            "blueprints: {} ({with_sources} with a source, {with_offered} with a source offered in this build, {with_effects} with quality effects; {} records left out as not a creation blueprint); by category: {categories}",
            out.len(),
            notes.not_creation
        );
        self.warnings.extend(notes.warnings());
        out
    }

    /// Where each blueprint is granted, by blueprint record id: every contract result
    /// (`BlueprintRewards`) of every generator, Wikelo's included, that draws from a pool
    /// listing it.
    fn blueprint_sources(&mut self, contracts: &[Contract], faction_name: &str, notes: &mut BlueprintNotes) -> HashMap<String, Vec<BlueprintSource>> {
        struct Draw {
            contract_id: String,
            debug_name: String,
            when: &'static str,
            pool: String,
            pool_id: String,
            chance: f64,
        }
        let mut draws: Vec<Draw> = Vec::new();
        // Pool record id -> each blueprint's share of the pool.
        let mut pools: HashMap<String, Vec<(String, f64)>> = HashMap::new();
        for (_, record) in self.res.records_of_type("ContractGenerator") {
            // A generator that does not materialize was already reported by the mission rewards.
            let Ok(v) = self.res.materialize(&record) else { continue };
            let root = record_value(&v);
            for g in arr(&root, "generators") {
                for key in ["contracts", "introContracts", "legacyContracts"] {
                    for c in arr(g, key) {
                        let Some(contract_id) = s(c, "id") else { continue };
                        let Some(results) = c.get("contractResults") else { continue };
                        for r in arr(results, "contractResults") {
                            if ty(r) != Some("BlueprintRewards") {
                                continue;
                            }
                            let Some(when) = result_when(r) else {
                                notes.other_moment += 1;
                                continue;
                            };
                            let Some((pool_ref, pool_id)) = r.get("blueprintPool").and_then(|p| ref_id(p).map(|id| (p, id.to_string()))) else {
                                notes.unresolved_pools += 1;
                                continue;
                            };
                            if !pools.contains_key(&pool_id) {
                                let Some(pool) = self.res.resolve_value(pool_ref) else {
                                    notes.unresolved_pools += 1;
                                    continue;
                                };
                                pools.insert(pool_id.clone(), pool_shares(&pool));
                            }
                            let (chance, clamped) = result_chance(f(r, "chance"));
                            if clamped {
                                notes.clamped_chances += 1;
                            }
                            draws.push(Draw {
                                contract_id: contract_id.to_string(),
                                debug_name: s(c, "debugName").unwrap_or("?").trim().to_string(),
                                when,
                                pool: fit(self.res.ref_name(pool_ref).map(|n| short_record_name(&n).to_string()).unwrap_or_else(|| pool_id.clone())),
                                pool_id,
                                chance,
                            });
                        }
                    }
                }
            }
        }

        // Titles and givers as the dataset names them: Wikelo's contracts by their entry in
        // `contracts`, every contract (Wikelo's too) the way `missions` describes it.
        let wanted: HashSet<&str> = draws.iter().map(|d| d.contract_id.as_str()).collect();
        let infos: HashMap<String, MissionInfo> =
            crate::missions::describe_missions(&mut *self.res, self.loc, &wanted, false, &mut self.warnings).into_iter().map(|m| (m.id.clone(), m)).collect();
        let wikelo: HashMap<&str, &str> = contracts.iter().map(|c| (c.id.as_str(), c.title.as_str())).collect();

        let mut out: HashMap<String, Vec<BlueprintSource>> = HashMap::new();
        for d in &draws {
            let info = infos.get(&d.contract_id);
            let wikelo_title = wikelo.get(d.contract_id.as_str()).copied();
            let title = wikelo_title.map(str::to_string).or_else(|| info.map(|m| m.title.clone())).unwrap_or_else(|| d.debug_name.clone());
            let giver = info.and_then(|m| m.giver.clone()).or_else(|| wikelo_title.map(|_| faction_name.to_string()));
            for (bp_id, share) in pools.get(&d.pool_id).map(Vec::as_slice).unwrap_or(&[]) {
                out.entry(bp_id.clone()).or_default().push(BlueprintSource {
                    kind: "contract".into(),
                    contract_id: d.contract_id.clone(),
                    title: fit(title.clone()),
                    giver: giver.clone().map(fit),
                    when: d.when.into(),
                    pool: d.pool.clone(),
                    chance: (d.chance * share).min(1.0),
                    pool_chance: pool_chance(d.chance),
                    disabled: info.is_some_and(|m| m.disabled),
                });
            }
        }
        for list in out.values_mut() {
            list.sort_by(|a, b| (&a.title, &a.contract_id, &a.when, &a.pool).cmp(&(&b.title, &b.contract_id, &b.when, &b.pool)).then(a.chance.total_cmp(&b.chance)));
        }
        out
    }

    /// The game's default blueprint list (`CraftingGlobalParams.defaultBlueprintSelection`),
    /// read once per extraction. None when no such record holds a readable list.
    fn default_blueprints(&mut self) -> Option<DefaultBlueprints> {
        let mut out: Option<DefaultBlueprints> = None;
        for (name, rec) in self.res.records_of_type("CraftingGlobalParams") {
            let root = match self.res.materialize(&rec) {
                Ok(v) => record_value(&v),
                Err(e) => {
                    self.warnings.push(format!("{name}: {e}"));
                    continue;
                }
            };
            if let Some(list) = default_blueprint_list(&root) {
                let all = out.get_or_insert_with(DefaultBlueprints::default);
                all.ids.extend(list.ids);
                all.paths.extend(list.paths);
            }
        }
        out
    }

    /// Like [`Self::craft_input`], without adding anything to the item catalog: the id is set
    /// only when the catalog already holds the resource or the item.
    fn blueprint_input(&mut self, o: &Value) -> Option<CraftInput> {
        match ty(o)? {
            "CraftingCost_Resource" => {
                let rid = o.get("resource").and_then(ref_id)?.to_string();
                let name = fit(self.resource_name(o, &rid));
                Some(CraftInput {
                    kind: "resource".into(),
                    item_id: self.items.contains_key(&rid).then_some(rid),
                    class_name: None,
                    name,
                    qty: o.get("quantity").and_then(|q| f(q, "standardCargoUnits")).unwrap_or(0.0),
                    unit: Unit::Scu,
                    min_quality: i(o, "minQuality").unwrap_or(0),
                })
            }
            "CraftingCost_Item" => {
                let (item_id, class_name, name) = self.item_face(o.get("entityClass")?)?;
                Some(CraftInput {
                    kind: "item".into(),
                    item_id,
                    class_name: Some(fit(class_name)),
                    name: fit(name),
                    qty: i(o, "quantity").unwrap_or(1) as f64,
                    unit: Unit::Count,
                    min_quality: i(o, "minQuality").unwrap_or(0),
                })
            }
            _ => None,
        }
    }

    // ----- supporting databases ------------------------------------------------

    fn load_tag_names(&mut self) {
        for (_, rec) in self.res.records_of_type("TagDatabase") {
            if let Ok(v) = self.res.materialize(&rec) {
                let root = record_value(&v);
                let mut stack: Vec<&Value> = arr(&root, "tags").iter().collect();
                while let Some(t) = stack.pop() {
                    if let (Some(id), Some(name)) = (s(t, "_RecordId_"), s(t, "tagName")) {
                        self.tag_names.insert(id.to_string(), name.to_string());
                    }
                    stack.extend(arr(t, "children").iter());
                }
            }
        }
    }

    fn load_resource_names(&mut self) {
        // Refined resource -> (ore id, the ore's quality table) of every ore that turns into it.
        let mut from_ores: BTreeMap<String, Vec<(String, Vec<i64>)>> = BTreeMap::new();
        for (_, rec) in self.res.records_of_type("ResourceTypeDatabase") {
            if let Ok(v) = self.res.materialize(&rec) {
                let root = record_value(&v);
                let mut found: Vec<(String, String)> = Vec::new();
                // (resource id, its quality table reference, the refined resource it turns into)
                let mut tables: Vec<(String, Value, Option<String>)> = Vec::new();
                // (record name without `ResourceType.` in lower case, resource id)
                let mut by_record: Vec<(String, String)> = Vec::new();
                walk(&root, &mut |node| {
                    if ty(node) == Some("ResourceType")
                        && let Some(id) = s(node, "_RecordId_")
                    {
                        if let Some(record) = s(node, "_RecordName_") {
                            by_record.push((record.trim_start_matches("ResourceType.").to_ascii_lowercase(), id.to_string()));
                        }
                        let table = arr(node, "properties")
                            .iter()
                            .find_map(|p| p.get("qualityQuantization").and_then(|q| q.get("qualityQuantizationRecord")).filter(|r| ref_id(r).is_some()));
                        if let Some(table) = table {
                            let refined = node.get("refinedVersion").and_then(ref_id).map(str::to_string);
                            tables.push((id.to_string(), table.clone(), refined));
                        }
                        let name = s(node, "displayName")
                            .map(|k| self.loc.text(k))
                            .or_else(|| s(node, "_RecordName_").map(|n| n.trim_start_matches("ResourceType.").to_string()))
                            .unwrap_or_else(|| id.to_string());
                        found.push((id.to_string(), name));
                    }
                });
                self.resource_names.extend(found);
                for (record, id) in by_record {
                    self.resource_ids_by_record.entry(record).or_insert(id);
                }
                for (id, table, refined) in tables {
                    // The link the quality tiers are named by (`load_quality_tiers`).
                    if let Some(table_id) = ref_id(&table) {
                        self.quantization_users.entry(table_id.to_string()).or_default().push((id.clone(), refined.clone()));
                    }
                    let Some(rec) = self.res.resolve_value(&table) else { continue };
                    let mut values: Vec<i64> = rec
                        .get("qualityQuantization")
                        .map(|q| arr(q, "bands").iter().filter_map(|b| i(b, "mappedValue")).filter(|v| (1..=1000).contains(v)).collect())
                        .unwrap_or_default();
                    values.sort_unstable();
                    values.dedup();
                    if values.is_empty() {
                        continue;
                    }
                    // The ore carries the table; what the player stores is usually the refined resource.
                    if let Some(refined) = refined {
                        from_ores.entry(refined).or_default().push((id.clone(), values.clone()));
                    }
                    self.resource_qualities.insert(id, values);
                }
            }
        }
        // After every record, so a refined resource's own table wins whatever the order.
        let (inherited, warnings) = refined_qualities(&self.resource_qualities, from_ores);
        self.resource_qualities.extend(inherited);
        self.warnings.extend(warnings);
    }

    /// The quality tiers of every crafting material: one per `CraftingQualityQuantizationRecord`
    /// (`Quantization_<Material>`), sorted by key. The record names no material; the resources
    /// do, through their `qualityQuantization` property, so a material is named after the
    /// resources that point at its record (see [`pick_tier_name`]). A record nobody points at
    /// takes the name of the resource whose record name is its key, and failing that its key
    /// split into words. Unreadable records are left out and counted in one warning.
    fn load_quality_tiers(&mut self, blueprints: &[Blueprint]) -> Vec<QualityTiers> {
        const RECORD_PREFIX: &str = "CraftingQualityQuantizationRecord.Quantization_";
        // What blueprint slots call their resource ingredients.
        let slot_names: HashSet<&str> = blueprints
            .iter()
            .flat_map(|b| b.slots.iter().chain(&b.optional_slots))
            .flat_map(|slot| &slot.options)
            .filter(|o| o.kind == "resource")
            .map(|o| o.name.as_str())
            .collect();
        // Resources with a table of their own, and the tables of the ores behind each refined one.
        let owners: HashSet<&str> = self.quantization_users.values().flatten().map(|(id, _)| id.as_str()).collect();
        let mut refined_tables: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (table, users) in &self.quantization_users {
            for refined in users.iter().filter_map(|(_, refined)| refined.as_deref()) {
                refined_tables.entry(refined).or_default().insert(table.as_str());
            }
        }
        let resource_names = &self.resource_names;
        let name_of = move |rid: &str| resource_names.get(rid).map(|n| n.trim()).filter(|n| valid_quality_tier_text(n));

        let mut out: Vec<QualityTiers> = Vec::new();
        let (mut unreadable, mut by_reference, mut by_record, mut by_key) = (0usize, 0usize, 0usize, 0usize);
        for (record, rec) in self.res.records_of_type("CraftingQualityQuantizationRecord") {
            let Some(key) = record.strip_prefix(RECORD_PREFIX).filter(|k| valid_quality_tier_text(k)) else {
                unreadable += 1;
                continue;
            };
            // The designers' starting point for new materials, not a material.
            if key.eq_ignore_ascii_case("TEMPLATE") {
                continue;
            }
            let Ok(v) = self.res.materialize(&rec) else {
                unreadable += 1;
                continue;
            };
            let Some(bands) = quality_bands(&record_value(&v)) else {
                unreadable += 1;
                continue;
            };
            let users: &[(String, Option<String>)] = self.quantization_users.get(&rec.id.to_string()).map(Vec::as_slice).unwrap_or(&[]);
            // A refined resource counts only when it has no table of its own and every ore
            // that turns into it uses this one.
            let mut refined: Vec<&str> = users
                .iter()
                .filter_map(|(_, refined)| refined.as_deref())
                .filter(|r| !owners.contains(r) && refined_tables.get(r).is_some_and(|tables| tables.len() == 1))
                .filter_map(name_of)
                .collect();
            let mut direct: Vec<&str> = users.iter().filter_map(|(rid, _)| name_of(rid.as_str())).collect();
            for names in [&mut refined, &mut direct] {
                names.sort_unstable();
                names.dedup();
            }
            let from_record = self.resource_ids_by_record.get(&key.to_ascii_lowercase()).and_then(|rid| name_of(rid.as_str()));
            let (name, link) = pick_tier_name(key, &refined, &direct, from_record, &slot_names);
            match link {
                TierLink::Reference => by_reference += 1,
                TierLink::RecordName => by_record += 1,
                TierLink::Key => by_key += 1,
            }
            out.push(QualityTiers { key: key.to_string(), name, bands });
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out.dedup_by(|a, b| a.key == b.key);
        tracing::info!(
            "quality tiers: {} materials ({by_reference} named through the resources that use them, {by_record} by record name, {by_key} from the key alone)",
            out.len()
        );
        if out.len() > MAX_QUALITY_TIERS {
            self.warnings.push(format!("quality tiers: {} materials, only the first {MAX_QUALITY_TIERS} by key are kept", out.len()));
            out.truncate(MAX_QUALITY_TIERS);
        }
        if unreadable > 0 {
            self.warnings.push(format!("quality tiers: {unreadable} quantization records without a usable Quantization_ name or readable bands, left out"));
        }
        if by_key > 0 {
            self.warnings.push(format!("quality tiers: {by_key} materials matched to no resource, named from their record key"));
        }
        out
    }

    /// Every named place of the star map. The record name is the class Game.log writes in
    /// `<RequestLocationInventory>`. Places the map draws as a landing zone, station or
    /// outpost are the ones offered to the player; the rest (interiors, mission sites, jump
    /// points, what the map hides) is kept `hidden`, only so the current place has a name.
    /// Templates and records whose name is still a placeholder are left out.
    fn load_locations(&mut self, emporiums: &[Emporium]) -> Vec<Location> {
        // First pass: what every record is and what it hangs from, to climb to the planet and the star.
        let mut nodes: HashMap<String, MapNode> = HashMap::new();
        let mut places: Vec<(String, String, Rc<Value>)> = Vec::new();
        for (record, rec) in self.res.records_of_type("StarMapObject") {
            let Some(id) = record.strip_prefix("StarMapObject.") else { continue };
            let Ok(v) = self.res.materialize(&rec) else { continue };
            let root = record_value(&v);
            let name = s(&root, "name").and_then(|k| self.loc.text_opt(k)).map(|t| t.trim().to_string()).filter(|t| real_place_name(t));
            let icon = s(&root, "navIcon").unwrap_or("").to_string();
            let parent = root.get("parent").and_then(|p| self.res.ref_name(p)).and_then(|n| n.strip_prefix("StarMapObject.").map(str::to_string));
            if let Some(name) = &name
                && matches!(icon.as_str(), "LandingZone" | "Station" | "Outpost" | "Default")
                && !id.ends_with("_TEMPLATE")
                && id.len() <= 128
                && wikelo_core::validate::safe_class_name(id)
            {
                places.push((id.to_string(), name.clone(), Rc::clone(&root)));
            }
            nodes.insert(id.to_string(), MapNode { name, icon, parent });
        }

        let mut out: BTreeMap<String, Location> = BTreeMap::new();
        for (id, display, root) in places {
            let icon = s(&root, "navIcon").unwrap_or("");
            let kind = match icon {
                "LandingZone" => "city",
                "Station" => "station",
                "Outpost" => "outpost",
                _ => "other",
            };
            let climbed = climb_star_map(&id, &nodes);
            if let Some(problem) = climbed.problem {
                self.warnings.push(problem);
            }
            let body = climbed.body;
            let orbits = climbed.orbits;
            let system = climbed.system.or_else(|| root.get("parent").and_then(|p| s(p, "$path")).and_then(crate::missions::system_of_path));
            let jump_point = id.starts_with("JumpPoint_");
            let hidden = icon == "Default" || jump_point || root.get("hideInStarmap").and_then(Value::as_bool) == Some(true);
            // The residences offered on the spawn screen carry that screen's backdrop picture.
            let backdrop = backdrop_file(s(&root, "locationImagePath").unwrap_or(""));
            // The gateway stations at the jump points and Grim HEX carry one too, but nobody
            // can live there.
            let home = !hidden && backdrop.starts_with("pubackdrop_") && !backdrop.contains("prison") && !id.starts_with("RR_JP_") && id != "GrimHEX";
            out.insert(id.clone(), Location { id, name: display, system, kind: kind.into(), home, body, orbits, hidden });
        }
        for e in emporiums {
            for class in &e.class_names {
                // The same rules as the star-map places: validation refuses the whole dataset
                // over one id that is too long or has odd characters.
                if class.starts_with("TheCollectorAsteroid") && class.len() <= 128 && wikelo_core::validate::safe_class_name(class) {
                    // The star map knows them only as unmarked points: offered all the same.
                    let known = out.remove(class);
                    let (system, body, orbits) = known.map(|l| (l.system, l.body, l.orbits)).unwrap_or((Some("Stanton".into()), None, None));
                    out.insert(class.clone(), Location { id: class.clone(), name: e.name.clone(), system: system.or(Some("Stanton".into())), kind: "station".into(), home: false, body, orbits, hidden: false });
                }
            }
        }
        out.into_values().collect()
    }

    fn load_emporiums(&mut self) {
        let prefix = "MissionLocationTemplate.TheCollectorsAsteriod_Stanton";
        for (name, rec) in self.res.records_of_type("MissionLocationTemplate") {
            let Some(suffix) = name.strip_prefix(prefix) else { continue };
            if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Ok(v) = self.res.materialize(&rec) else { continue };
            let root = record_value(&v);
            let class_name = name.trim_start_matches("MissionLocationTemplate.").to_string();
            let known = KNOWN_EMPORIUMS.iter().find(|(sfx, _, _)| class_name.ends_with(sfx));
            let display = root
                .get("locationData")
                .and_then(|d| d.get("stringVariants"))
                .map(|sv| arr(sv, "variants"))
                .and_then(|vars| vars.first())
                .and_then(|var| s(var, "string"))
                .map(|k| self.loc.text(k))
                .unwrap_or_else(|| class_name.clone());
            let tag_ids: HashSet<String> = root
                .get("locationData")
                .and_then(|d| d.get("generalTags"))
                .map(|gt| arr(gt, "tags").iter().filter_map(ref_id).map(str::to_string).collect())
                .unwrap_or_default();
            let mut class_names = vec![class_name.clone()];
            // Game.log spells the location differently (`TheCollectorAsteroid_StantonN`).
            class_names.push(class_name.replace("TheCollectorsAsteriod", "TheCollectorAsteroid"));
            self.emporiums.push(EmporiumInfo {
                emporium: Emporium {
                    key: known.map(|(_, k, _)| k.to_string()).unwrap_or_else(|| class_name.to_ascii_lowercase()),
                    name: display,
                    location_hash: known.map(|(_, _, h)| *h),
                    class_names,
                },
                tag_ids,
            });
        }
        self.emporiums.sort_by(|a, b| a.emporium.key.cmp(&b.emporium.key));
    }

    fn faction(&mut self) -> Faction {
        let mut name = "Wikelo Emporium".to_string();
        if let Some(v) = self.res.get(WIKELO_FACTION_ID) {
            let rv = record_value(&v);
            if let Some(k) = s(&rv, "displayName") {
                name = self.loc.text(k);
            }
        }
        let mut scopes: Vec<RepScope> = Vec::new();
        let ids: Vec<String> = self.scope_ids.iter().cloned().collect();
        for id in ids {
            if let Some(scope) = self.scope(&id) {
                scopes.push(scope);
            }
        }
        // By id, not name: `scope_ids` is a HashSet and names may collide, and the dataset
        // must come out identical on every machine.
        scopes.sort_by(|a, b| a.id.cmp(&b.id));
        Faction { id: WIKELO_FACTION_ID.to_string(), name, scopes }
    }

    fn scope(&mut self, id: &str) -> Option<RepScope> {
        let v = self.res.get(id)?;
        let rv = record_value(&v);
        let name = s(&rv, "scopeName").unwrap_or(id).to_string();
        let display_name = s(&rv, "displayName").map(|k| self.loc.text(k)).unwrap_or_else(|| name.clone());
        let map = rv.get("standingMap");
        let ceiling = map.and_then(|m| i(m, "reputationCeiling"));
        let mut standings: Vec<Standing> = Vec::new();
        if let Some(m) = map {
            for sref in arr(m, "standings") {
                if let Some(st) = self.standing(sref) {
                    standings.push(st);
                }
            }
        }
        standings.sort_by_key(|st| st.min_rep);
        Some(RepScope { id: id.to_string(), name, display_name, ceiling, standings })
    }

    fn standing(&mut self, sref: &Value) -> Option<Standing> {
        let id = ref_id(sref).map(str::to_string);
        let v = self.res.resolve_value(sref)?;
        Some(Standing {
            id,
            name: s(&v, "name").unwrap_or("?").to_string(),
            display_name: s(&v, "displayName").map(|k| self.loc.text(k)).unwrap_or_else(|| "?".into()),
            min_rep: i(&v, "minReputation").unwrap_or(0),
        })
    }

    // ----- contracts -------------------------------------------------------------

    fn contract(
        &mut self,
        c: &Value,
        defaults: &Availability,
        gen_prereqs: &[Value],
        gen_mission_type: Option<&Value>,
        gen_dropoff: Option<&Value>,
    ) -> Option<Contract> {
        let id = s(c, "id")?.to_string();
        let debug_name = s(c, "debugName").unwrap_or("?").trim().to_string();
        let params = c.get("paramOverrides");

        let mut avail = defaults.clone();
        avail.apply_overrides(params);

        let mut title_key = None;
        let mut description_key = None;
        for o in params.map(|p| arr(p, "stringParamOverrides")).unwrap_or(&[]) {
            match (s(o, "param"), s(o, "value")) {
                (Some("Title"), Some(v)) => title_key = Some(v.to_string()),
                (Some("Description"), Some(v)) => description_key = Some(v.to_string()),
                _ => {}
            }
        }
        let title = title_key.as_deref().and_then(|k| self.loc.text_opt(k)).unwrap_or_else(|| debug_name.clone());
        let description = description_key.as_deref().and_then(|k| self.loc.text_opt(k)).unwrap_or_default();
        let description_plain = plain_text(&description);

        // Template (hauling orders live in its objective handler).
        let template = c.get("template").and_then(|t| self.res.resolve_value(t));
        let template_props: Vec<Value> = template.as_ref().map(|t| arr(t, "contractProperties").to_vec()).unwrap_or_default();
        let contract_props: Vec<Value> = params.map(|p| arr(p, "propertyOverrides").to_vec()).unwrap_or_default();

        let hauling_override: Option<Vec<Value>> = find_property(&contract_props, "HaulingOverride")
            .or_else(|| find_property(&template_props, "HaulingOverride"))
            .and_then(|p| p.get("value"))
            .map(|v| arr(v, "haulingOrderContent").to_vec());

        let mut requirements: Vec<Requirement> = Vec::new();
        if let Some(t) = template.as_ref() {
            for tok in arr(t, "objectiveTokens") {
                let Some(handler) = tok.get("objectiveHandler") else { continue };
                if ty(handler) != Some("ObjectiveHandler_Hauling") {
                    continue;
                }
                let orders = arr(handler, "haulingOrders").to_vec();
                requirements.extend(self.hauling_orders(&orders, hauling_override.as_deref(), &debug_name));
            }
        } else {
            self.warnings.push(format!("{debug_name}: template not resolved"));
        }
        if requirements.is_empty()
            && let Some(content) = hauling_override.as_deref()
        {
            // No template handler; fall back to the override content directly.
            requirements.extend(self.hauling_content(content, &debug_name));
        }

        // Rewards, reputation, completion tags.
        let mut rewards: Vec<Reward> = Vec::new();
        let mut rep_gained: Vec<RepGain> = Vec::new();
        let mut completion_tags: Vec<TagRef> = Vec::new();
        if let Some(results) = c.get("contractResults") {
            for (idx, r) in arr(results, "contractResults").iter().enumerate() {
                // Blueprints handed out on accept (so the player can craft what the
                // contract asks for) are worth showing; other non-success results are not.
                let on_accept = !applies_on_success(r) && ty(r) == Some("BlueprintRewards") && applies_on_accept(r);
                if !applies_on_success(r) && !on_accept {
                    continue;
                }
                self.contract_result(r, idx as u32, &id, &debug_name, on_accept, &mut rewards, &mut rep_gained, &mut completion_tags);
            }
        }

        // Prerequisites: generator defaults + contract additions.
        let mut prerequisites = Prerequisites::default();
        let mut rep_required: Option<RepRequirement> = None;
        for p in gen_prereqs.iter().chain(arr(c, "additionalPrerequisites").iter()) {
            self.prerequisite(p, &mut prerequisites, &mut rep_required);
        }

        // Mission type.
        let mission_type_ref = params
            .and_then(|p| p.get("missionTypeOverride"))
            .filter(|v| !v.is_null())
            .or(gen_mission_type.filter(|v| !v.is_null()))
            .cloned()
            .or_else(|| template.as_ref().and_then(|t| t.get("contractDisplayInfo")).and_then(|d| d.get("type")).cloned());
        let mission_type = mission_type_ref.and_then(|r| self.res.resolve_value(&r)).and_then(|mt| {
            s(&mt, "LocalisedTypeName").map(|k| self.loc.text(k))
        });

        // Emporiums (drop-off locations) from the tag search.
        let dropoff = find_property(&contract_props, "DropoffLocation_BP")
            .cloned()
            .or_else(|| gen_dropoff.cloned())
            .or_else(|| find_property(&template_props, "DropoffLocation_BP").cloned());
        let emporium_keys = self.emporium_keys(dropoff.as_ref());

        let lifetime = Lifetime {
            instance_lifetime: c.get("contractLifeTime").and_then(|l| f(l, "instanceLifeTime")),
            respawn_time: c.get("generationParams").and_then(|g| f(g, "respawnTime")),
            max_instances: c.get("generationParams").and_then(|g| i(g, "maxInstances")),
            max_per_player: c.get("generationParams").and_then(|g| i(g, "maxInstancesPerPlayer")).or(avail.max_players),
        };

        let category = Category::derive(title_key.as_deref(), mission_type.as_deref(), &debug_name, &rewards);

        Some(Contract {
            id,
            debug_name,
            aliases: vec![],
            category,
            mission_type,
            title_key: title_key.map(|k| k.trim_start_matches('@').to_string()),
            title,
            description_key: description_key.map(|k| k.trim_start_matches('@').to_string()),
            description,
            description_plain,
            rep_required,
            rep_gained,
            once_only: avail.once_only,
            not_for_release: b(c, "notForRelease").unwrap_or(false),
            work_in_progress: b(c, "workInProgress").unwrap_or(false),
            hidden_in_mobiglas: avail.hide_in_mobiglas,
            cooldown: Cooldown {
                abandoned_sec: avail.abandoned_cooldown.round() as i64,
                personal_sec: avail.personal_cooldown.round() as i64,
            },
            lifetime,
            prerequisites,
            completion_tags,
            emporium_keys,
            offered_in_build: None,
            requirements,
            rewards,
        })
    }

    fn emporium_keys(&mut self, dropoff: Option<&Value>) -> Vec<String> {
        let Some(prop) = dropoff else { return self.emporiums.iter().map(|e| e.emporium.key.clone()).collect() };
        let mut positive: Vec<String> = Vec::new();
        walk(prop, &mut |node| {
            if ty(node) == Some("TagSearchTerm") {
                positive.extend(arr(node, "positiveTags").iter().filter_map(ref_id).map(str::to_string));
            }
        });
        if positive.is_empty() {
            return self.emporiums.iter().map(|e| e.emporium.key.clone()).collect();
        }
        self.emporiums
            .iter()
            .filter(|e| positive.iter().all(|t| e.tag_ids.contains(t)))
            .map(|e| e.emporium.key.clone())
            .collect()
    }

    fn prerequisite(&mut self, p: &Value, out: &mut Prerequisites, rep: &mut Option<RepRequirement>) {
        match ty(p) {
            Some("ContractPrerequisite_CompletedContractTags") => {
                for t in p.get("requiredCompletedContractTags").map(|l| arr(l, "tags")).unwrap_or(&[]) {
                    if let Some(tag) = self.tag_ref(t) {
                        out.required_tags.push(tag);
                    }
                }
                for t in p.get("excludedCompletedContractTags").map(|l| arr(l, "tags")).unwrap_or(&[]) {
                    if let Some(tag) = self.tag_ref(t) {
                        out.excluded_tags.push(tag);
                    }
                }
                if let Some(n) = i(p, "requiredCountValue") {
                    out.required_count = Some(n);
                }
            }
            Some("ContractPrerequisite_Reputation") => {
                let scope_id = p.get("scope").and_then(ref_id).map(str::to_string);
                if let Some(sid) = &scope_id {
                    self.scope_ids.insert(sid.clone());
                }
                let min = p.get("minStanding").and_then(|v| self.standing(v));
                let max = p.get("maxStanding").and_then(|v| self.standing(v));
                *rep = Some(RepRequirement {
                    scope_id,
                    min_rep: min.as_ref().map(|st| st.min_rep),
                    min_standing_name: min.map(|st| st.display_name),
                    max_standing_name: max.map(|st| st.display_name),
                });
            }
            Some("ContractPrerequisite_CrimeStat") => {
                out.crime_stat = Some(CrimeStatRange {
                    min: f(p, "minCrimeStat").unwrap_or(0.0) as i64,
                    max: f(p, "maxCrimeStat").unwrap_or(0.0) as i64,
                });
            }
            Some(other) => self.warnings.push(format!("unknown prerequisite type {other}")),
            None => {}
        }
    }

    fn tag_ref(&self, v: &Value) -> Option<TagRef> {
        let id = ref_id(v)?.to_string();
        let name = self.tag_names.get(&id).cloned().or_else(|| s(v, "tagName").map(str::to_string));
        Some(TagRef { id, name })
    }

    #[allow(clippy::too_many_arguments)]
    fn contract_result(
        &mut self,
        r: &Value,
        result_index: u32,
        contract_id: &str,
        debug_name: &str,
        on_accept: bool,
        rewards: &mut Vec<Reward>,
        rep_gained: &mut Vec<RepGain>,
        completion_tags: &mut Vec<TagRef>,
    ) {
        let owner_only = b(r, "awardOnlyToMissionOwner").unwrap_or(true);
        match ty(r) {
            Some("ContractResult_Item") => {
                if let Some(item) = r.get("entityClass").and_then(|e| self.item(e)) {
                    rewards.push(Reward {
                        result: result_index,
                        option: 0,
                        weight: 1.0,
                        owner_only,
                        item_id: Some(item.id.clone()),
                        class_name: Some(item.class_name.clone()),
                        name: item.name.clone(),
                        qty: i(r, "amount").unwrap_or(1),
                        kind: reward_kind(&item.kind),
                        on_accept,
                    });
                }
            }
            Some("ContractResult_ItemsWeighting") => {
                let mut groups: Vec<Value> = Vec::new();
                for g in arr(r, "itemAwardStructure") {
                    match ty(g) {
                        Some("ItemAwardWeightings") => groups.push(g.clone()),
                        _ => {
                            // e.g. ItemAwardWeightingsParams { awardsRecord: $ref }
                            if let Some(rec) = g.get("awardsRecord").and_then(|a| self.res.resolve_value(a)) {
                                walk(&rec, &mut |n| {
                                    if ty(n) == Some("ItemAwardWeightings") {
                                        groups.push(n.clone());
                                    }
                                });
                            } else {
                                self.warnings.push(format!("{debug_name}: unknown item award structure {:?}", ty(g)));
                            }
                        }
                    }
                }
                for (opt, g) in groups.iter().enumerate() {
                    let weight = f(g, "weighting").unwrap_or(1.0);
                    for a in arr(g, "awards") {
                        if let Some(item) = a.get("entityClass").and_then(|e| self.item(e)) {
                            rewards.push(Reward {
                                result: result_index,
                                option: opt as u32,
                                weight,
                                owner_only,
                                item_id: Some(item.id.clone()),
                                class_name: Some(item.class_name.clone()),
                                name: item.name.clone(),
                                qty: i(a, "amountToAward").unwrap_or(1),
                                kind: reward_kind(&item.kind),
                                on_accept,
                            });
                        }
                    }
                }
            }
            Some("BlueprintRewards") => {
                let chance = f(r, "chance").unwrap_or(1.0);
                let Some(pool) = r.get("blueprintPool").and_then(|p| self.res.resolve_value(p)) else {
                    self.warnings.push(format!("{debug_name}: blueprint pool not resolved"));
                    return;
                };
                for (opt, bp) in arr(&pool, "blueprintRewards").iter().enumerate() {
                    let weight = f(bp, "weight").unwrap_or(1.0) * chance;
                    let Some(bp_rec) = bp.get("blueprintRecord").and_then(|b| self.res.resolve_value(b)) else { continue };
                    let bp_name = bp.get("blueprintRecord").and_then(|b| self.res.ref_name(b));
                    if let Some(bp_id) = bp.get("blueprintRecord").and_then(ref_id) {
                        self.blueprint_sources.entry(bp_id.to_string()).or_default().push((contract_id.to_string(), on_accept, weight));
                    }
                    let entity = bp_rec
                        .get("blueprint")
                        .and_then(|b| b.get("processSpecificData"))
                        .and_then(|p| p.get("entityClass"))
                        .cloned();
                    let item = entity.as_ref().and_then(|e| self.item(e));
                    let name = match &item {
                        Some(it) => format!("Blueprint: {}", it.name),
                        None => format!("Blueprint: {}", bp_name.unwrap_or_else(|| "?".into())),
                    };
                    rewards.push(Reward {
                        result: result_index,
                        option: opt as u32,
                        weight,
                        owner_only,
                        item_id: item.as_ref().map(|it| it.id.clone()),
                        class_name: item.as_ref().map(|it| it.class_name.clone()),
                        name,
                        qty: 1,
                        kind: RewardKind::Blueprint,
                        on_accept,
                    });
                }
            }
            Some("ContractResult_LegacyReputation") => {
                let Some(amounts) = r.get("contractResultReputationAmounts") else { return };
                let scope_id = amounts.get("reputationScope").and_then(ref_id).map(str::to_string);
                if let Some(sid) = &scope_id {
                    self.scope_ids.insert(sid.clone());
                }
                let scope_name = scope_id.as_deref().and_then(|sid| self.res.get(sid)).and_then(|v| {
                    let rv = record_value(&v);
                    s(&rv, "scopeName").map(str::to_string)
                });
                let amount = amounts
                    .get("reward")
                    .and_then(|rw| self.res.resolve_value(rw))
                    .and_then(|rw| i(&rw, "reputationAmount"))
                    .unwrap_or(0);
                rep_gained.push(RepGain { scope_id, scope_name, amount });
            }
            Some("ContractResult_CompletionTags") => {
                for ct in arr(r, "completionTags") {
                    if let Some(tag) = ct.get("tag").and_then(|t| self.tag_ref(t)) {
                        completion_tags.push(tag);
                    }
                }
            }
            Some("ContractResult_CalculatedReputation") => {
                let scope_id = r.get("reputationScope").and_then(ref_id).map(str::to_string);
                if let Some(sid) = &scope_id {
                    self.scope_ids.insert(sid.clone());
                }
                rep_gained.push(RepGain { scope_id, scope_name: None, amount: 0 });
            }
            Some("ContractResult_Reward") | Some("ContractResult_CalculatedReward") => {
                // UEC payouts are not used by Wikelo contracts; ignore silently.
            }
            Some(other) => self.warnings.push(format!("{debug_name}: unknown contract result {other}")),
            None => {}
        }
    }

    // ----- hauling orders --------------------------------------------------------

    /// `HaulingOrder_*` entries from a template's `ObjectiveHandler_Hauling`.
    fn hauling_orders(&mut self, orders: &[Value], override_content: Option<&[Value]>, debug_name: &str) -> Vec<Requirement> {
        let mut out = Vec::new();
        for o in orders {
            match ty(o) {
                Some("HaulingOrder_Property") => {
                    if let Some(content) = override_content {
                        out.extend(self.hauling_content(content, debug_name));
                    }
                }
                Some("HaulingOrder_EntityClass") | Some("HaulingOrder_Resource") | Some("HaulingOrder_EntityClasses")
                | Some("HaulingOrder_MissionItem") | Some("HaulingOrder_Or") => {
                    if let Some(req) = self.hauling_entry(o, debug_name) {
                        out.push(req);
                    }
                }
                Some(other) => self.warnings.push(format!("{debug_name}: unknown hauling order {other}")),
                None => {}
            }
        }
        out
    }

    /// `HaulingOrderContent_*` entries from a `MissionPropertyValue_HaulingOrders`.
    fn hauling_content(&mut self, content: &[Value], debug_name: &str) -> Vec<Requirement> {
        content.iter().filter_map(|c| self.hauling_entry(c, debug_name)).collect()
    }

    /// One hauling entry of either family (`HaulingOrder_X` / `HaulingOrderContent_X`).
    fn hauling_entry(&mut self, o: &Value, debug_name: &str) -> Option<Requirement> {
        let t = ty(o)?;
        let kind = t.rsplit('_').next().unwrap_or(t);
        match kind {
            "EntityClass" => {
                let item = o.get("entityClass").and_then(|e| self.item(e));
                let (qty, qty_max) = amounts(i(o, "minAmount"), i(o, "maxAmount"));
                Some(Requirement {
                    kind: RequirementKind::Entity,
                    item_id: item.as_ref().map(|it| it.id.clone()),
                    class_name: item.as_ref().map(|it| it.class_name.clone()),
                    name: item.as_ref().map(|it| it.name.clone()).unwrap_or_else(|| "?".into()),
                    qty,
                    qty_max,
                    unit: Unit::Count,
                    candidates: vec![],
                    options: vec![],
                })
            }
            "Resource" => {
                let id = o.get("resource").and_then(ref_id).map(str::to_string);
                let name = id
                    .as_deref()
                    .and_then(|rid| self.resource_names.get(rid).cloned())
                    .or_else(|| o.get("resource").and_then(|r| self.res.ref_name(r)).map(|n| n.trim_start_matches("ResourceType.").to_string()))
                    .unwrap_or_else(|| "?".into());
                if let Some(rid) = &id {
                    self.items.entry(rid.clone()).or_insert_with(|| Item {
                        id: rid.clone(),
                        class_name: format!("ResourceType:{name}"),
                        name: name.clone(),
                        item_type: Some("Resource".into()),
                        sub_type: None,
                        kind: "commodity".into(),
                        image: None,
                        image_source: None,
                        qualities: self.resource_qualities.get(rid).cloned().unwrap_or_default(),
                    });
                }
                let min = f(o, "minSCU").map(|x| x.round() as i64);
                let max = f(o, "maxSCU").map(|x| x.round() as i64);
                let (qty, qty_max) = amounts(min, max);
                Some(Requirement {
                    kind: RequirementKind::Resource,
                    item_id: id,
                    class_name: None,
                    name,
                    qty,
                    qty_max,
                    unit: Unit::Scu,
                    candidates: vec![],
                    options: vec![],
                })
            }
            "EntityClasses" => {
                let list = o.get("haulingEntityClasses").and_then(|h| self.res.resolve_value(h));
                let mut candidates: Vec<ItemRef> = Vec::new();
                let mut display: Option<String> = None;
                if let Some(l) = list {
                    display = s(&l, "orderDisplayName").map(|k| self.loc.text(k));
                    for e in arr(&l, "entityClasses") {
                        if let Some(it) = self.item(e) {
                            candidates.push(ItemRef {
                                item_id: Some(it.id.clone()),
                                class_name: Some(it.class_name.clone()),
                                name: it.name.clone(),
                            });
                        }
                    }
                }
                let (qty, qty_max) = amounts(i(o, "minAmount"), i(o, "maxAmount"));
                let name = display
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" / "));
                Some(Requirement {
                    kind: RequirementKind::Entities,
                    item_id: None,
                    class_name: None,
                    name,
                    qty,
                    qty_max,
                    unit: Unit::Count,
                    candidates,
                    options: vec![],
                })
            }
            "Or" => {
                let mut options: Vec<Vec<Requirement>> = Vec::new();
                for opt in arr(o, "options") {
                    let orders: Vec<Value> = arr(opt, "orders").to_vec();
                    options.push(self.hauling_content(&orders, debug_name));
                }
                Some(Requirement {
                    kind: RequirementKind::Or,
                    item_id: None,
                    class_name: None,
                    name: "Uma das opções".into(),
                    qty: 0,
                    qty_max: 0,
                    unit: Unit::Count,
                    candidates: vec![],
                    options,
                })
            }
            "MissionItem" => {
                let (qty, qty_max) = amounts(i(o, "minAmount"), i(o, "maxAmount"));
                self.warnings.push(format!("{debug_name}: mission-item hauling order not fully resolved"));
                Some(Requirement {
                    kind: RequirementKind::MissionItem,
                    item_id: None,
                    class_name: None,
                    name: "Item de missão".into(),
                    qty,
                    qty_max,
                    unit: Unit::Count,
                    candidates: vec![],
                    options: vec![],
                })
            }
            other => {
                self.warnings.push(format!("{debug_name}: unknown hauling entry {other}"));
                None
            }
        }
    }

    // ----- items -------------------------------------------------------------------

    /// Resolve an `EntityClassDefinition` reference into the item catalog.
    fn item(&mut self, eref: &Value) -> Option<Item> {
        let id = ref_id(eref)?;
        if let Some(it) = self.items.get(id) {
            return Some(it.clone());
        }
        let item = self.describe_item(eref)?;
        self.items.insert(item.id.clone(), item.clone());
        Some(item)
    }

    /// The crafted or asked-for item as (id when the catalog holds it, class name, English
    /// name), without adding it to the catalog.
    fn item_face(&mut self, eref: &Value) -> Option<(Option<String>, String, String)> {
        let id = ref_id(eref)?;
        if let Some(it) = self.items.get(id) {
            return Some((Some(it.id.clone()), it.class_name.clone(), it.name.clone()));
        }
        let it = self.describe_item(eref)?;
        Some((None, it.class_name, it.name))
    }

    /// What the catalog would hold for an `EntityClassDefinition` reference, read from the
    /// game data (the catalog is not looked at nor changed).
    fn describe_item(&mut self, eref: &Value) -> Option<Item> {
        let id = ref_id(eref)?.to_string();
        let class_name = self
            .res
            .ref_name(eref)
            .map(|n| n.trim_start_matches("EntityClassDefinition.").to_string())
            .unwrap_or_else(|| id.clone());
        let rv: Option<Rc<Value>> = self.res.resolve_value(eref);
        let mut item_type = None;
        let mut sub_type = None;
        let mut name_key: Option<String> = None;
        let mut qualities: Vec<i64> = Vec::new();
        if let Some(rv) = rv.as_ref() {
            // A hand-mined or gathered item is made of one resource and rolls that resource's quality.
            for comp in arr(rv, "Components") {
                if comp.get("generateRandomQuality").and_then(Value::as_bool) != Some(true) {
                    continue;
                }
                match composition_resource(comp) {
                    Composition::One(rid) => {
                        if let Some(q) = self.resource_qualities.get(rid) {
                            qualities = q.clone();
                            break;
                        }
                    }
                    // Made of several resources: no one table fits, so no quality steps.
                    Composition::Several => break,
                    Composition::Empty => {}
                }
            }
            for comp in arr(rv, "Components") {
                if ty(comp) == Some("SAttachableComponentParams")
                    && let Some(ad) = comp.get("AttachDef")
                {
                    item_type = s(ad, "Type").map(str::to_string);
                    sub_type = s(ad, "SubType").map(str::to_string);
                    name_key = ad.get("Localization").and_then(|l| s(l, "Name")).map(str::to_string);
                    break;
                }
            }
            if name_key.is_none() {
                for comp in arr(rv, "Components") {
                    if ty(comp) == Some("VehicleComponentParams") {
                        name_key = s(comp, "vehicleName").map(str::to_string);
                        item_type.get_or_insert_with(|| "NOITEM_Vehicle".into());
                        break;
                    }
                }
            }
        } else {
            self.warnings.push(format!("entity class {class_name} ({id}) not resolved"));
        }
        let name = name_key
            .as_deref()
            .and_then(|k| self.loc.text_opt(k))
            .unwrap_or_else(|| class_name.clone());
        let kind = item_kind(&class_name, item_type.as_deref(), sub_type.as_deref(), &name);
        Some(Item { id, class_name, name, item_type, sub_type, kind: kind.to_string(), image: None, image_source: None, qualities })
    }
}

// ----- blueprints --------------------------------------------------------------------------

/// What reading a blueprint record needs besides the record itself. The [`Normalizer`]
/// answers from DataCore and the localization; the tests answer from small tables.
trait BlueprintReader {
    /// One ingredient option (`CraftingCost_Resource` or `CraftingCost_Item`); None when unreadable.
    fn input(&mut self, cost: &Value) -> Option<CraftInput>;
    /// English name of a slot (`CraftingCost_Select`).
    fn slot_name(&self, select: &Value) -> String;
    /// English name of a gameplay property, from the record the reference points at.
    fn property_name(&mut self, property: &Value) -> Option<String>;
    /// The crafted item: (id when the item catalog holds it, class name, English name).
    fn output(&mut self, entity: &Value) -> Option<(Option<String>, String, String)>;
    /// Localized text of a key; None when missing or a placeholder.
    fn text(&self, key: &str) -> Option<String>;
    /// The record a reference points at (its `_RecordValue_`); None when it does not resolve.
    fn record(&mut self, reference: &Value) -> Option<Rc<Value>>;
}

impl BlueprintReader for Normalizer<'_, '_, '_> {
    fn input(&mut self, cost: &Value) -> Option<CraftInput> {
        self.blueprint_input(cost)
    }

    fn slot_name(&self, select: &Value) -> String {
        self.craft_slot_name(select)
    }

    fn property_name(&mut self, property: &Value) -> Option<String> {
        let id = ref_id(property)?.to_string();
        if let Some(name) = self.property_names.get(&id) {
            return name.clone();
        }
        let loc = self.loc;
        let name = self.res.resolve_value(property).and_then(|rec| localized_name(&rec, loc));
        self.property_names.insert(id, name.clone());
        name
    }

    fn output(&mut self, entity: &Value) -> Option<(Option<String>, String, String)> {
        self.item_face(entity)
    }

    fn text(&self, key: &str) -> Option<String> {
        self.loc.text_opt(key)
    }

    fn record(&mut self, reference: &Value) -> Option<Rc<Value>> {
        self.res.resolve_value(reference)
    }
}

/// What reading the blueprints met that the dataset does not carry, counted, so the
/// extraction reports it in a few lines instead of one per record.
#[derive(Debug, Default, PartialEq)]
struct BlueprintNotes {
    /// Records that are not creation blueprints (the game's generic dismantle one).
    not_creation: usize,
    /// Creation blueprints whose output item could not be read.
    no_output: usize,
    /// Record or output class names validation would refuse.
    unsafe_names: usize,
    /// Blueprints with more than one tier (only the first is read, as for `crafting`).
    extra_tiers: usize,
    /// Quality effects or quantity multipliers no slot owns (on a group, or on an ingredient).
    unplaced: usize,
    /// Groups of slots inside groups: their `count` is not kept.
    nested_groups: usize,
    /// Quality ranges of an unknown type, with missing values or outside the 0..=1000 scale.
    skipped_ranges: usize,
    /// Quality effects without a usable property key or without a usable range.
    skipped_effects: usize,
    /// Quantity multipliers that are not finite or outside 0..=`MAX_BLUEPRINT_VALUE`.
    skipped_multipliers: usize,
    /// Results whose chance was outside 0..=1, clamped.
    clamped_chances: usize,
    /// `BlueprintRewards` results whose pool did not resolve.
    unresolved_pools: usize,
    /// `BlueprintRewards` results that fire neither on accept nor on completion.
    other_moment: usize,
    /// Blueprints whose crafted item's record did not resolve: nothing about the item is kept.
    no_item_record: usize,
    /// Crafted items without an `SItemDefinition`: no type, size, grade, manufacturer or class.
    no_item_definition: usize,
    /// Item definitions whose `Type` is missing, empty or `UNDEFINED`.
    no_item_type: usize,
    /// Item definitions without a `Size` in 0..=`MAX_ITEM_SIZE`.
    no_size: usize,
    /// Item definitions without a `Grade` in 1..=`MAX_ITEM_GRADE`.
    no_grade: usize,
    /// Manufacturer references that did not resolve.
    unresolved_manufacturers: usize,
    /// Manufacturers without a usable `Code` or without a real English name.
    unnamed_manufacturers: usize,
    /// Type, sub type or class values validation would refuse (too long, control characters).
    unusable_labels: usize,
    /// Blueprint records whose path has no `blueprints/crafting/` folder: no tags.
    no_tag_folder: usize,
    /// Folders of a blueprint's path a tag cannot carry, or past the sixth.
    dropped_tags: usize,
    /// 1 when the game's default blueprint list was not found: no blueprint is marked default.
    no_default_list: usize,
}

impl BlueprintNotes {
    fn warnings(&self) -> Vec<String> {
        [
            (self.no_output, "creation blueprints without a readable output item, left out"),
            (self.unsafe_names, "blueprints with a record or output class name the dataset cannot carry, left out"),
            (self.extra_tiers, "blueprints with more than one tier; only the first is read"),
            (self.unplaced, "quality effects or quantity multipliers outside a slot, not kept"),
            (self.nested_groups, "groups of slots inside groups; their count is not kept"),
            (self.skipped_ranges, "quality ranges of an unknown type or with unusable values, skipped"),
            (self.skipped_effects, "quality effects without a property key or a usable range, skipped"),
            (self.skipped_multipliers, "quantity multipliers with unusable values, skipped"),
            (self.clamped_chances, "blueprint reward chances outside 0..1, clamped"),
            (self.unresolved_pools, "blueprint reward results whose pool did not resolve"),
            (self.other_moment, "blueprint reward results granted neither on accept nor on completion, skipped"),
            (self.no_item_record, "blueprints whose crafted item record did not resolve; nothing about the item is kept"),
            (self.no_item_definition, "blueprints whose crafted item has no SItemDefinition; no type, size, grade, manufacturer or class"),
            (self.no_item_type, "blueprints whose crafted item has no type (missing or UNDEFINED)"),
            (self.no_size, "blueprints whose crafted item has no size in 0..12"),
            (self.no_grade, "blueprints whose crafted item has no grade in 1..9"),
            (self.unresolved_manufacturers, "blueprints whose crafted item's manufacturer did not resolve"),
            (self.unnamed_manufacturers, "blueprints whose crafted item's manufacturer has no code or no real name, left out"),
            (self.unusable_labels, "item types, sub types or classes the dataset cannot carry, left out"),
            (self.no_tag_folder, "blueprint records outside a blueprints/crafting/ folder, without tags"),
            (self.dropped_tags, "blueprint path folders that cannot be tags (odd characters, or past the sixth), left out"),
            (self.no_default_list, "default blueprint list (CraftingGlobalParams) not found; no blueprint is marked default"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("blueprints: {n} {what}"))
        .collect()
    }
}

/// One `CraftingBlueprintRecord` (its `_RecordValue_`, record id `id`, record name `name`
/// without the type prefix) as a [`Blueprint`] with no sources yet. None, counted in `notes`,
/// unless it is a creation blueprint with a readable output item.
fn blueprint_from_record(id: &str, name: &str, root: &Value, reader: &mut dyn BlueprintReader, notes: &mut BlueprintNotes) -> Option<Blueprint> {
    let Some(bp) = root.get("blueprint") else {
        notes.not_creation += 1;
        return None;
    };
    let process = bp.get("processSpecificData");
    if process.and_then(ty) != Some("CraftingProcess_Creation") {
        notes.not_creation += 1;
        return None;
    }
    let Some((output_item_id, output_class_name, output_name)) = process.and_then(|p| p.get("entityClass")).and_then(|e| reader.output(e)) else {
        notes.no_output += 1;
        return None;
    };
    if !safe_class_name(name) || !safe_class_name(&output_class_name) {
        notes.unsafe_names += 1;
        return None;
    }
    let tiers = arr(bp, "tiers");
    if tiers.len() > 1 {
        notes.extra_tiers += 1;
    }
    let costs = tiers.first().and_then(|t| t.get("recipe")).and_then(|r| r.get("costs"));
    let mut slots: Vec<BlueprintSlot> = Vec::new();
    let mut optional_slots: Vec<BlueprintSlot> = Vec::new();
    let mut slots_required = None;
    if let Some(costs) = costs {
        if let Some(m) = costs.get("mandatoryCost") {
            if ty(m) == Some("CraftingCost_Select") && !is_slot(m) {
                slots_required = i(m, "count").and_then(|n| u32::try_from(n).ok());
            }
            read_cost(m, 0, reader, &mut slots, notes);
        }
        for o in arr(costs, "optionalCosts") {
            read_cost(o, 0, reader, &mut optional_slots, notes);
        }
    }
    Some(Blueprint {
        id: id.to_string(),
        name: name.to_string(),
        display_name: s(bp, "blueprintName").and_then(|k| reader.text(k)).filter(|t| real_place_name(t)).map(fit),
        category: bp.get("category").and_then(short_ref_name).map(fit),
        output_item_id,
        // An item whose name the game left as a placeholder is shown by its class name.
        output_name: fit(if real_place_name(&output_name) { output_name } else { output_class_name.clone() }),
        output_class_name,
        craft_time_sec: craft_time_sec(costs),
        slots_required,
        slots,
        optional_slots,
        sources: Vec::new(),
        ..Default::default()
    })
}

/// The blueprints the game hands every player (`CraftingGlobalParams.defaultBlueprintSelection`),
/// by record id and by [`blueprint_path_key`], since a reference may come as either.
#[derive(Debug, Default, PartialEq)]
struct DefaultBlueprints {
    ids: HashSet<String>,
    paths: HashSet<String>,
}

impl DefaultBlueprints {
    /// Is the blueprint with record id `id`, whose record file is `path`, on the list?
    fn contains(&self, id: &str, path: &str) -> bool {
        self.ids.contains(id) || blueprint_path_key(path).is_some_and(|k| self.paths.contains(&k))
    }
}

/// The `blueprintRecords` of a `CraftingGlobalParams` record value; None when it has no such
/// list (another kind of selection, or no selection).
fn default_blueprint_list(params: &Value) -> Option<DefaultBlueprints> {
    let list = params.get("defaultBlueprintSelection")?.get("blueprintRecords")?.as_array()?;
    let mut out = DefaultBlueprints::default();
    for entry in list {
        if let Some(id) = ref_id(entry) {
            out.ids.insert(id.to_string());
        }
        let path = entry.get("$path").or_else(|| entry.get("_RecordPath_")).and_then(Value::as_str).or_else(|| entry.as_str());
        if let Some(key) = path.and_then(blueprint_path_key) {
            out.paths.insert(key);
        }
    }
    Some(out)
}

/// What follows `blueprints/` in a record path, in lower case with `/` separators, so a
/// record's own path (`libs/foundry/records/crafting/blueprints/crafting/x.json`) and a
/// reference to it (`file://./../../libs/.../blueprints/crafting/x.json`) give the same key.
fn blueprint_path_key(path: &str) -> Option<String> {
    let path = format!("/{}", path.replace('\\', "/").to_ascii_lowercase());
    const ANCHOR: &str = "/blueprints/";
    let at = path.find(ANCHOR)? + ANCHOR.len();
    let key = &path[at..];
    (!key.is_empty()).then(|| key.to_string())
}

/// The folders of a blueprint record's path after `blueprints/crafting/`, in lower case and
/// without the file name (`.../blueprints/crafting/fpsgear/weapons/rifle/bp_x.json` gives
/// `fpsgear`, `weapons`, `rifle`), with how many folders were left out because a tag cannot
/// carry them or because [`MAX_BLUEPRINT_TAGS`] were already taken. None when the path has
/// no `blueprints/crafting/` folder.
fn blueprint_tags(path: &str) -> Option<(Vec<String>, usize)> {
    let path = format!("/{}", path.replace('\\', "/").to_ascii_lowercase());
    const ANCHOR: &str = "/blueprints/crafting/";
    let at = path.find(ANCHOR)? + ANCHOR.len();
    let mut folders: Vec<&str> = path[at..].split('/').collect();
    folders.pop(); // the file name
    let mut tags: Vec<String> = Vec::new();
    let mut dropped = 0;
    for folder in folders.into_iter().filter(|f| !f.is_empty()) {
        if tags.len() < MAX_BLUEPRINT_TAGS && valid_blueprint_tag(folder) {
            tags.push(folder.to_string());
        } else {
            dropped += 1;
        }
    }
    Some((tags, dropped))
}

/// Fills in what a blueprint says about its crafted item (`item_type` to `item_class`, read
/// from the item's record), its `tags` (from `path`, the blueprint record's own file) and
/// `default` (from `defaults`, None when the game's list was not found). `root` is the
/// blueprint record's value. What cannot be read is left out and counted in `notes`.
fn describe_blueprint_item(bp: &mut Blueprint, root: &Value, path: &str, defaults: Option<&DefaultBlueprints>, reader: &mut dyn BlueprintReader, notes: &mut BlueprintNotes) {
    let entity = root.get("blueprint").and_then(|b| b.get("processSpecificData")).and_then(|p| p.get("entityClass"));
    match entity.and_then(|e| reader.record(e)) {
        Some(item) => item_facts(bp, &item, reader, notes),
        None => notes.no_item_record += 1,
    }
    match blueprint_tags(path) {
        Some((tags, dropped)) => {
            bp.tags = tags;
            notes.dropped_tags += dropped;
        }
        None => notes.no_tag_folder += 1,
    }
    bp.default = defaults.is_some_and(|d| d.contains(&bp.id, path));
}

/// Type, sub type, size, grade, manufacturer and class of a crafted item, from its record
/// value `item`, onto `bp`.
fn item_facts(bp: &mut Blueprint, item: &Value, reader: &mut dyn BlueprintReader, notes: &mut BlueprintNotes) {
    let Some(def) = item_definition(item) else {
        notes.no_item_definition += 1;
        return;
    };
    let item_type = s(def, "Type").map(str::trim).unwrap_or("");
    if item_type.is_empty() || item_type.eq_ignore_ascii_case("UNDEFINED") {
        notes.no_item_type += 1;
    }
    bp.item_type = item_label(Some(item_type), notes);
    // `UNDEFINED` is what most items without a sub type say: not a miss.
    bp.item_sub_type = item_label(s(def, "SubType"), notes);
    bp.size = i(def, "Size").and_then(|n| u32::try_from(n).ok()).filter(|n| *n <= MAX_ITEM_SIZE);
    if bp.size.is_none() {
        notes.no_size += 1;
    }
    bp.grade = i(def, "Grade").and_then(|n| u32::try_from(n).ok()).filter(|n| (1..=MAX_ITEM_GRADE).contains(n));
    if bp.grade.is_none() {
        notes.no_grade += 1;
    }
    bp.manufacturer = item_manufacturer(def.get("Manufacturer"), reader, notes);
    let description = def.get("Localization").and_then(|l| s(l, "Description")).and_then(|k| reader.text(k));
    bp.item_class = item_label(description.as_deref().and_then(description_class), notes);
}

/// The crafted item's `SItemDefinition`: the `AttachDef` of its components that says so, else
/// the one of its `SAttachableComponentParams`.
fn item_definition(item: &Value) -> Option<&Value> {
    let components = arr(item, "Components");
    components.iter().filter_map(|c| c.get("AttachDef")).find(|d| ty(d) == Some("SItemDefinition")).or_else(|| {
        components
            .iter()
            .find(|c| ty(c) == Some("SAttachableComponentParams"))
            .and_then(|c| c.get("AttachDef"))
            .filter(|d| d.is_object())
    })
}

/// A game value for `item_type`, `item_sub_type` or `item_class`, trimmed: None when absent,
/// empty or `UNDEFINED`; None, and counted, when validation would refuse it.
fn item_label(value: Option<&str>, notes: &mut BlueprintNotes) -> Option<String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("UNDEFINED"))?;
    if valid_blueprint_label(value, MAX_BLUEPRINT_LABEL) {
        Some(value.to_string())
    } else {
        notes.unusable_labels += 1;
        None
    }
}

/// The manufacturer an item definition's `Manufacturer` reference points at: its `Code` and
/// English name. None when there is no reference (null, empty, the all-zero id); None, and
/// counted, when it does not resolve or has no usable code or no real name.
fn item_manufacturer(reference: Option<&Value>, reader: &mut dyn BlueprintReader, notes: &mut BlueprintNotes) -> Option<BlueprintManufacturer> {
    const NO_RECORD: &str = "00000000-0000-0000-0000-000000000000";
    let reference = reference.filter(|r| !r.is_null() && !r.as_str().is_some_and(|p| p.trim().is_empty()) && ref_id(r) != Some(NO_RECORD))?;
    let Some(rec) = reader.record(reference) else {
        notes.unresolved_manufacturers += 1;
        return None;
    };
    let code = s(&rec, "Code").map(str::trim).filter(|c| valid_blueprint_label(c, MAX_MANUFACTURER_CODE)).map(str::to_string);
    let name = rec
        .get("Localization")
        .and_then(|l| s(l, "Name"))
        .and_then(|k| reader.text(k))
        .map(|t| t.trim().to_string())
        .filter(|t| real_place_name(t) && valid_blueprint_label(t, MAX_MANUFACTURER_NAME));
    match (code, name) {
        (Some(code), Some(name)) => Some(BlueprintManufacturer { code, name }),
        _ => {
            notes.unnamed_manufacturers += 1;
            None
        }
    }
}

/// The value of the `Class:` line among the `Key: value` lines that open an item description
/// (`Item Type: Cooler`, `Size: 1`, `Class: Competition`, before the first blank line), in
/// whatever position it comes; None when the opening lines have none.
fn description_class(text: &str) -> Option<&str> {
    text.lines().map(str::trim).skip_while(|l| l.is_empty()).take_while(|l| !l.is_empty()).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        (key.trim().eq_ignore_ascii_case("class") && !value.is_empty()).then_some(value)
    })
}

/// A `CraftingCost_Select` whose options include ingredients: a slot (the others group slots).
fn is_slot(select: &Value) -> bool {
    arr(select, "options").iter().any(|o| matches!(ty(o), Some("CraftingCost_Resource") | Some("CraftingCost_Item")))
}

/// Reads a cost tree into slots the way [`Normalizer::craft_slots`] does (a select with
/// ingredients is a slot, a select of selects a group read through, a bare ingredient a slot
/// of its own), and keeps what that one drops: the quality effects and quantity multipliers.
fn read_cost(cost: &Value, depth: usize, reader: &mut dyn BlueprintReader, slots: &mut Vec<BlueprintSlot>, notes: &mut BlueprintNotes) {
    match ty(cost) {
        Some("CraftingCost_Select") if is_slot(cost) => {
            let mut options: Vec<CraftInput> = Vec::new();
            let mut quantity_multipliers: Vec<QuantityMultiplier> =
                quantity_multipliers(cost, notes).into_iter().map(|multiplier| QuantityMultiplier { option: None, multiplier }).collect();
            for o in arr(cost, "options") {
                let Some(input) = reader.input(o) else { continue };
                let option = Some(options.len() as u32);
                quantity_multipliers.extend(quantity_multipliers_of(o, option, notes));
                notes.unplaced += property_modifiers(o).count();
                options.push(input);
            }
            let quality_effects = quality_effects(cost, reader, notes);
            if options.is_empty() {
                notes.unplaced += quality_effects.len() + quantity_multipliers.len();
                return;
            }
            slots.push(BlueprintSlot {
                name: fit(reader.slot_name(cost)),
                count: i(cost, "count").and_then(|n| u32::try_from(n).ok()).unwrap_or(1),
                options,
                quantity_multipliers,
                quality_effects,
            });
        }
        Some("CraftingCost_Select") => {
            if depth > 0 {
                notes.nested_groups += 1;
            }
            let unplaced = property_modifiers(cost).count() + quantity_multipliers(cost, notes).len();
            notes.unplaced += unplaced;
            for o in arr(cost, "options") {
                read_cost(o, depth + 1, reader, slots, notes);
            }
        }
        Some("CraftingCost_Resource") | Some("CraftingCost_Item") => {
            let Some(input) = reader.input(cost) else { return };
            let quantity_multipliers = quantity_multipliers_of(cost, Some(0), notes);
            let quality_effects = quality_effects(cost, reader, notes);
            slots.push(BlueprintSlot { name: fit(input.name.clone()), count: 1, options: vec![input], quantity_multipliers, quality_effects });
        }
        _ => {}
    }
}

/// The `CraftingCostContext_QuantityMultiplier` values on a cost node; unusable ones are
/// counted in `notes` and left out.
fn quantity_multipliers(cost: &Value, notes: &mut BlueprintNotes) -> Vec<f64> {
    let mut out = Vec::new();
    for c in arr(cost, "context").iter().filter(|c| ty(c) == Some("CraftingCostContext_QuantityMultiplier")) {
        match f(c, "multiplier") {
            Some(m) if finite_value(m) && m >= 0.0 => out.push(m),
            _ => notes.skipped_multipliers += 1,
        }
    }
    out
}

/// [`quantity_multipliers`] of a cost node, tagged with the option index they belong to.
fn quantity_multipliers_of(cost: &Value, option: Option<u32>, notes: &mut BlueprintNotes) -> Vec<QuantityMultiplier> {
    quantity_multipliers(cost, notes).into_iter().map(|multiplier| QuantityMultiplier { option, multiplier }).collect()
}

/// The `CraftingGameplayPropertyModifierCommon` entries in a cost node's context.
fn property_modifiers(cost: &Value) -> impl Iterator<Item = &Value> {
    arr(cost, "context")
        .iter()
        .filter(|c| ty(c) == Some("CraftingCostContext_ResultGameplayPropertyModifiers"))
        .flat_map(|c| match c.get("gameplayPropertyModifiers") {
            // `CraftingGameplayPropertyModifiers_List { gameplayPropertyModifiers: [...] }`, or a bare list.
            Some(Value::Array(list)) => list.as_slice(),
            Some(list) => arr(list, "gameplayPropertyModifiers"),
            None => &[],
        })
}

/// What a slot's quality changes, one entry per gameplay property, as the record lists them.
fn quality_effects(cost: &Value, reader: &mut dyn BlueprintReader, notes: &mut BlueprintNotes) -> Vec<QualityEffect> {
    let mut out = Vec::new();
    for m in property_modifiers(cost) {
        let property = m.get("gameplayPropertyRecord");
        let Some(key) = property.and_then(property_key).filter(|k| safe_class_name(k)) else {
            notes.skipped_effects += 1;
            continue;
        };
        let mut ranges = Vec::new();
        for r in arr(m, "valueRanges") {
            match quality_range(r) {
                Some(range) => ranges.push(range),
                None => notes.skipped_ranges += 1,
            }
        }
        if ranges.is_empty() {
            notes.skipped_effects += 1;
            continue;
        }
        let name = property.and_then(|p| reader.property_name(p)).map(fit);
        out.push(QualityEffect { key, name, ranges });
    }
    out
}

/// One `CraftingGameplayPropertyModifierValueRange_*`, as written; None for an unknown type,
/// a missing value, or a quality outside the game's 0..=1000 scale.
fn quality_range(r: &Value) -> Option<QualityRange> {
    let (kind, at_start_key, at_end_key) = match ty(r)? {
        "CraftingGameplayPropertyModifierValueRange_Linear" => (QualityRangeKind::Linear, "modifierAtStart", "modifierAtEnd"),
        "CraftingGameplayPropertyModifierValueRange_LinearIntegerAdditive" => (QualityRangeKind::IntegerAdditive, "additiveModifierAtStart", "additiveModifierAtEnd"),
        _ => return None,
    };
    let range = QualityRange { kind, start_quality: i(r, "startQuality")?, end_quality: i(r, "endQuality")?, at_start: f(r, at_start_key)?, at_end: f(r, at_end_key)? };
    let scale = 0..=MAX_QUALITY;
    (scale.contains(&range.start_quality) && scale.contains(&range.end_quality) && finite_value(range.at_start) && finite_value(range.at_end)).then_some(range)
}

/// Finite and within ±`MAX_BLUEPRINT_VALUE`, as validation demands.
fn finite_value(v: f64) -> bool {
    v.is_finite() && v.abs() <= MAX_BLUEPRINT_VALUE
}

/// Name of the record a reference points at, without its type prefix (`gpp_weapon_recoil_kick`):
/// from the reference's record name, else from the file name of its path.
fn property_key(v: &Value) -> Option<String> {
    let named = v.get("$name").or_else(|| v.get("_RecordName_")).and_then(Value::as_str).map(short_record_name);
    let key = named.or_else(|| {
        let path = v.get("$path").and_then(Value::as_str).or_else(|| v.as_str())?;
        let file = path.rsplit(['/', '\\']).next()?;
        Some(file.strip_suffix(".json").unwrap_or(file))
    })?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// `BlueprintPoolRecord.BP_REWARDS_X` -> `BP_REWARDS_X`: a record name without its type prefix.
fn short_record_name(name: &str) -> &str {
    name.split_once('.').map_or(name, |(_, rest)| rest)
}

/// The record name of a reference (`$ref` object or sub-record), without its type prefix.
fn short_ref_name(v: &Value) -> Option<String> {
    v.get("$name").or_else(|| v.get("_RecordName_")).and_then(Value::as_str).map(|n| short_record_name(n).to_string())
}

/// The English name a record carries: its `displayName` key, else the first other `@` key in
/// a field whose name says "name". None when none of them resolves to real text.
fn localized_name(rec: &Value, loc: &Localization) -> Option<String> {
    let map = rec.as_object()?;
    let others = map.iter().filter(|(k, _)| k.as_str() != "displayName" && k.to_ascii_lowercase().contains("name")).map(|(_, v)| v);
    map.get("displayName")
        .into_iter()
        .chain(others)
        .filter_map(Value::as_str)
        .filter(|k| k.starts_with('@'))
        .find_map(|k| loc.text_opt(k))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Each blueprint's share of a `BlueprintPoolRecord`: its weight over the sum of every
/// entry's weight. Entries without a blueprint count in the sum, as the record lists them; a
/// blueprint listed twice adds its weights; a missing weight is 1 (as the Wikelo rewards read
/// it), a negative or non-finite one 0. Empty when the total is not positive.
fn pool_shares(pool: &Value) -> Vec<(String, f64)> {
    let weight = |e: &Value| match f(e, "weight").unwrap_or(1.0) {
        w if w.is_finite() && w > 0.0 => w,
        _ => 0.0,
    };
    let entries = arr(pool, "blueprintRewards");
    let total: f64 = entries.iter().map(weight).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut weights: Vec<(String, f64)> = Vec::new();
    for e in entries {
        let Some(id) = e.get("blueprintRecord").and_then(ref_id) else { continue };
        match weights.iter_mut().find(|(known, _)| known == id) {
            Some((_, w)) => *w += weight(e),
            None => weights.push((id.to_string(), weight(e))),
        }
    }
    weights.into_iter().map(|(id, w)| (id, (w / total).min(1.0))).collect()
}

/// When a contract result grants its reward: `complete` when it fires on success, else
/// `accept` when it fires on accept (the order the Wikelo rewards are read in); None for a
/// result that does neither.
fn result_when(r: &Value) -> Option<&'static str> {
    if applies_on_success(r) {
        Some("complete")
    } else if applies_on_accept(r) {
        Some("accept")
    } else {
        None
    }
}

/// A result's `chance` as a probability: missing is 1 (as the Wikelo rewards read it), a value
/// outside 0..=1 is clamped; the flag says it was.
fn result_chance(chance: Option<f64>) -> (f64, bool) {
    let c = chance.unwrap_or(1.0);
    if !c.is_finite() {
        return (0.0, true);
    }
    (c.clamp(0.0, 1.0), !(0.0..=1.0).contains(&c))
}

/// [`BlueprintSource::pool_chance`] from a result's chance (as [`result_chance`] gives it):
/// kept only when the mission does not always give a blueprint of the pool.
fn pool_chance(chance: f64) -> Option<f64> {
    (chance < 1.0).then_some(chance)
}

/// `TimeValue_Partitioned` of a recipe's costs, in seconds (0 when absent).
fn craft_time_sec(costs: Option<&Value>) -> f64 {
    costs
        .and_then(|c| c.get("craftTime"))
        .map(|t| f(t, "days").unwrap_or(0.0) * 86_400.0 + f(t, "hours").unwrap_or(0.0) * 3_600.0 + f(t, "minutes").unwrap_or(0.0) * 60.0 + f(t, "seconds").unwrap_or(0.0))
        .unwrap_or(0.0)
}

/// At most 200 characters (the longest short text validation accepts), so one odd string in
/// the game data cannot get the whole dataset refused.
fn fit(s: String) -> String {
    if s.chars().count() <= 200 { s } else { s.chars().take(200).collect() }
}

// ----- helpers -------------------------------------------------------------------------

/// A display name the game really filled in (`<= PLACEHOLDER =>` and `<= UNINITIALIZED =>` are not).
fn real_place_name(text: &str) -> bool {
    !text.is_empty() && !text.starts_with("<=") && !text.starts_with('@')
}

fn amounts(min: Option<i64>, max: Option<i64>) -> (i64, i64) {
    let min = min.unwrap_or(0);
    let max = max.unwrap_or(0);
    let qty = if max > 0 { max } else { min };
    (qty, min.max(max))
}

fn applies_on_success(r: &Value) -> bool {
    match r.get("missionResults").and_then(Value::as_array) {
        Some(flags) => flags.first().and_then(Value::as_bool).unwrap_or(true),
        None => true,
    }
}

/// `missionResults[1]`: the result fires when the mission is accepted/started
/// (seen on Wikelo contracts that hand out the blueprint for the item they ask for).
fn applies_on_accept(r: &Value) -> bool {
    r.get("missionResults")
        .and_then(Value::as_array)
        .and_then(|flags| flags.get(1))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `SUPPORT STRUCTURE` -> `Support Structure`.
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + &c.as_str().to_ascii_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_property<'v>(props: &'v [Value], variable: &str) -> Option<&'v Value> {
    props.iter().find(|p| s(p, "missionVariableName").is_some_and(|n| n.eq_ignore_ascii_case(variable)))
}

pub fn item_kind(class_name: &str, item_type: Option<&str>, sub_type: Option<&str>, name: &str) -> &'static str {
    let cls = class_name.to_ascii_lowercase();
    let lname = name.to_ascii_lowercase();
    if cls.contains("favour") || cls.contains("favor") || lname.contains("wikelo favor") || lname.contains("polaris bit") {
        return "favour";
    }
    match item_type.unwrap_or("") {
        "Currency" => "currency",
        t if t.starts_with("Char_Armor") || t.starts_with("Char_Clothing") => "armor",
        "WeaponPersonal" | "WeaponAttachment" | "WeaponMagazine" => "weapon",
        t if t.starts_with("NOITEM_Vehicle") || t == "Vehicle" => "vehicle",
        "Misc" if sub_type == Some("Harvestable") => "harvestable",
        "Resource" => "commodity",
        _ if cls.contains("blueprint") => "blueprint",
        _ => "other",
    }
}

pub fn reward_kind(kind: &str) -> RewardKind {
    match kind {
        "vehicle" => RewardKind::Vehicle,
        "armor" => RewardKind::Armor,
        "weapon" => RewardKind::Weapon,
        "currency" => RewardKind::Currency,
        "favour" => RewardKind::Favour,
        "blueprint" => RewardKind::Blueprint,
        _ => RewardKind::Other,
    }
}

pub fn ty(v: &Value) -> Option<&str> {
    v.get("_Type_").and_then(Value::as_str)
}

pub fn s<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str)
}

pub fn i(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
}

pub fn f(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

pub fn b(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

pub fn arr<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// What an entity component's `defaultComposition` is made of.
#[derive(Debug, PartialEq, Eq)]
enum Composition<'v> {
    /// One resource (its record id).
    One(&'v str),
    /// More than one resource.
    Several,
    /// Nothing, or an entry that names no resource.
    Empty,
}

fn composition_resource(comp: &Value) -> Composition<'_> {
    match arr(comp, "defaultComposition") {
        [only] => only.get("entry").and_then(ref_id).map_or(Composition::Empty, Composition::One),
        [] => Composition::Empty,
        // The game lists the same resource twice on some items (the Quasi Grazer egg): still one.
        [first, rest @ ..] => match first.get("entry").and_then(ref_id) {
            Some(id) if rest.iter().all(|e| e.get("entry").and_then(ref_id) == Some(id)) => Composition::One(id),
            _ => Composition::Several,
        },
    }
}

/// The quality tables refined resources take from their ores (`from_ores`: refined id ->
/// (ore id, ore table)), for the ones without a table of their own in `own`: the sorted,
/// de-duplicated union of their ores' tables. Also returns a warning per refined resource
/// whose ores disagree.
fn refined_qualities(own: &HashMap<String, Vec<i64>>, from_ores: BTreeMap<String, Vec<(String, Vec<i64>)>>) -> (Vec<(String, Vec<i64>)>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for (refined, ores) in from_ores {
        if own.get(&refined).is_some_and(|v| !v.is_empty()) {
            continue; // its own table wins
        }
        let mut values: Vec<i64> = ores.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            continue;
        }
        if ores.iter().any(|(_, v)| *v != values) {
            let mut ids: Vec<&str> = ores.iter().map(|(id, _)| id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            warnings.push(format!("refined resource {refined}: its ores ({}) have different quality tables; using their union", ids.join(", ")));
        }
        out.push((refined, values));
    }
    (out, warnings)
}

/// The bands of a `CraftingQualityQuantizationRecord` value (`qualityQuantization.bands`),
/// ascending. None when a band lacks `start`, `end` or `mappedValue`, or when the bands break
/// [`valid_quality_bands`] (none or too many, outside 0..=1000, inverted, overlapping).
fn quality_bands(root: &Value) -> Option<Vec<QualityBand>> {
    let list = arr(root.get("qualityQuantization")?, "bands");
    let mut bands: Vec<QualityBand> =
        list.iter().map(|b| Some(QualityBand { start: i(b, "start")?, end: i(b, "end")?, value: i(b, "mappedValue")? })).collect::<Option<Vec<_>>>()?;
    bands.sort_by_key(|b| (b.start, b.end));
    valid_quality_bands(&bands).then_some(bands)
}

/// How a material's quality tiers got their name (see [`pick_tier_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TierLink {
    /// From a resource whose `qualityQuantization` property points at the record.
    Reference,
    /// From the resource whose record name is the material's key.
    RecordName,
    /// No resource: the key split into words.
    Key,
}

/// The name of a material's quality tiers. `refined`: names of the refined resources the
/// record's ores turn into; `direct`: names of the resources that point at the record
/// themselves; `by_record`: name of the resource whose record name is `key`. A name blueprint
/// slots use (`slot_names`) wins, then a refined resource (what the player stores and crafts
/// with), then a direct one, then the record name match, then `key` split into words.
fn pick_tier_name(key: &str, refined: &[&str], direct: &[&str], by_record: Option<&str>, slot_names: &HashSet<&str>) -> (String, TierLink) {
    if let Some(name) = refined.iter().chain(direct).find(|n| slot_names.contains(**n)) {
        return ((*name).to_string(), TierLink::Reference);
    }
    if let Some(name) = refined.first().or(direct.first()) {
        return ((*name).to_string(), TierLink::Reference);
    }
    if let Some(name) = by_record {
        return (name.to_string(), TierLink::RecordName);
    }
    (words_of_key(key), TierLink::Key)
}

/// A record key split into words: a space before a capital that follows a lower-case letter or
/// a digit, or that starts a word after an acronym (`JumpingLimes` -> `Jumping Limes`,
/// `VLKFang` -> `VLK Fang`, `TEMPLATE` unchanged).
fn words_of_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::with_capacity(key.len() + 4);
    for (n, &c) in chars.iter().enumerate() {
        if n > 0 && c.is_uppercase() {
            let prev = chars[n - 1];
            let starts_word_after_acronym = prev.is_uppercase() && chars.get(n + 1).is_some_and(|next| next.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || starts_word_after_acronym {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

/// A `StarMapObject` as far as the location climb needs it: its display name (when it has a
/// real one), its map icon, and the record it hangs from.
struct MapNode {
    name: Option<String>,
    icon: String,
    parent: Option<String>,
}

/// Most ancestors climbed from one place. The real map is a handful of levels deep; this is
/// only a guard against a broken chain.
const MAX_MAP_DEPTH: usize = 32;

/// What climbing from a place found.
#[derive(Debug, Default, PartialEq)]
struct Climb {
    body: Option<String>,
    /// The planet above, when the body is a moon.
    orbits: Option<String>,
    system: Option<String>,
    /// A warning when the climb stopped early (a parent cycle, or more than [`MAX_MAP_DEPTH`] ancestors).
    problem: Option<String>,
}

/// Climb from `id` through its parents: the nearest planet or moon is the body (failing
/// that, the nearest named ancestor, e.g. a Lagrange point); when the body is a moon, the
/// planet above it is what it orbits; the star names the system. Stops
/// at a missing parent, at the star, at a parent already visited (a cycle) or after
/// [`MAX_MAP_DEPTH`] ancestors; the last two leave a warning.
fn climb_star_map(id: &str, nodes: &HashMap<String, MapNode>) -> Climb {
    let mut out = Climb::default();
    let mut on_body = false;
    let mut on_moon = false;
    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(id);
    let mut up: Option<&str> = nodes.get(id).and_then(|n| n.parent.as_deref());
    let mut steps = 0usize;
    while let Some(pid) = up {
        if !visited.insert(pid) {
            out.problem = Some(format!("star map: {id}: its parents loop back to {pid}; stopped climbing there"));
            break;
        }
        if steps == MAX_MAP_DEPTH {
            out.problem = Some(format!("star map: {id}: more than {MAX_MAP_DEPTH} ancestors; stopped climbing at {pid}"));
            break;
        }
        steps += 1;
        let Some(node) = nodes.get(pid) else { break };
        if node.icon == "Star" {
            out.system = node.name.clone();
            break;
        }
        let is_body = matches!(node.icon.as_str(), "Planet" | "Moon");
        if !on_body && (is_body || out.body.is_none()) && node.name.is_some() {
            out.body = node.name.clone();
            on_body = is_body;
            on_moon = node.icon == "Moon";
        } else if on_moon && out.orbits.is_none() && node.icon == "Planet" && node.name.is_some() {
            out.orbits = node.name.clone();
        }
        up = node.parent.as_deref();
    }
    out
}

/// The file name of a `locationImagePath`, in lower case, whichever separator the path uses.
fn backdrop_file(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase()
}

/// Depth-first visit of every object node.
pub fn walk<'v>(v: &'v Value, f: &mut dyn FnMut(&'v Value)) {
    match v {
        Value::Object(map) => {
            f(v);
            for child in map.values() {
                walk(child, f);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk(child, f);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod blueprint_tests {
    use super::*;
    use serde_json::json;

    /// Answers from the record alone: names are the references' record names.
    struct TableReader;

    impl BlueprintReader for TableReader {
        fn input(&mut self, cost: &Value) -> Option<CraftInput> {
            let (kind, reference, qty, unit) = match ty(cost)? {
                "CraftingCost_Resource" => ("resource", cost.get("resource")?, cost.get("quantity").and_then(|q| f(q, "standardCargoUnits")).unwrap_or(0.0), Unit::Scu),
                "CraftingCost_Item" => ("item", cost.get("entityClass")?, i(cost, "quantity").unwrap_or(1) as f64, Unit::Count),
                _ => return None,
            };
            Some(CraftInput {
                kind: kind.into(),
                item_id: ref_id(reference).map(str::to_string),
                class_name: None,
                name: short_ref_name(reference)?,
                qty,
                unit,
                min_quality: i(cost, "minQuality").unwrap_or(0),
            })
        }

        fn slot_name(&self, select: &Value) -> String {
            select.get("nameInfo").and_then(|n| s(n, "debugName")).map(title_case).unwrap_or_default()
        }

        fn property_name(&mut self, property: &Value) -> Option<String> {
            (property_key(property)?.as_str() == "gpp_weapon_damage").then(|| "Damage".to_string())
        }

        fn output(&mut self, entity: &Value) -> Option<(Option<String>, String, String)> {
            let class = short_ref_name(entity)?;
            Some((ref_id(entity).map(str::to_string), class.clone(), class.replace('_', " ")))
        }

        fn text(&self, key: &str) -> Option<String> {
            (key == "@mission_item_0075").then(|| "Microsatellite".to_string())
        }

        fn record(&mut self, _reference: &Value) -> Option<Rc<Value>> {
            None
        }
    }

    /// Answers records (by reference id) and texts (by key, `@` included) from tables, the
    /// rest as [`TableReader`] does.
    #[derive(Default)]
    struct RecordReader {
        records: HashMap<String, Value>,
        texts: HashMap<String, String>,
    }

    impl BlueprintReader for RecordReader {
        fn input(&mut self, cost: &Value) -> Option<CraftInput> {
            TableReader.input(cost)
        }

        fn slot_name(&self, select: &Value) -> String {
            TableReader.slot_name(select)
        }

        fn property_name(&mut self, property: &Value) -> Option<String> {
            TableReader.property_name(property)
        }

        fn output(&mut self, entity: &Value) -> Option<(Option<String>, String, String)> {
            TableReader.output(entity)
        }

        fn text(&self, key: &str) -> Option<String> {
            self.texts.get(key).cloned()
        }

        fn record(&mut self, reference: &Value) -> Option<Rc<Value>> {
            self.records.get(ref_id(reference)?).cloned().map(Rc::new)
        }
    }

    fn reference(name: &str, id: &str) -> Value {
        json!({ "$ref": id, "$name": name, "$path": format!("file://./records/{}.json", short_record_name(name).to_ascii_lowercase()) })
    }

    fn resource(name: &str, scu: f64, min_quality: i64, context: Value) -> Value {
        json!({
            "_Type_": "CraftingCost_Resource", "context": context,
            "resource": reference(&format!("ResourceType.{name}"), &format!("res-{name}")),
            "quantity": { "_Type_": "SStandardCargoUnit", "standardCargoUnits": scu },
            "minQuality": min_quality
        })
    }

    fn linear(start: i64, end: i64, at_start: f64, at_end: f64) -> Value {
        json!({ "_Type_": "CraftingGameplayPropertyModifierValueRange_Linear", "startQuality": start, "endQuality": end, "modifierAtStart": at_start, "modifierAtEnd": at_end })
    }

    fn additive(start: i64, end: i64, at_start: i64, at_end: i64) -> Value {
        json!({ "_Type_": "CraftingGameplayPropertyModifierValueRange_LinearIntegerAdditive", "startQuality": start, "endQuality": end, "additiveModifierAtStart": at_start, "additiveModifierAtEnd": at_end })
    }

    /// A `ResultGameplayPropertyModifiers` context: (property reference, its value ranges) each.
    fn modifiers(list: Vec<(Value, Vec<Value>)>) -> Value {
        let list: Vec<Value> = list.into_iter().map(|(p, ranges)| json!({ "_Type_": "CraftingGameplayPropertyModifierCommon", "gameplayPropertyRecord": p, "valueRanges": ranges })).collect();
        json!({ "_Type_": "CraftingCostContext_ResultGameplayPropertyModifiers", "gameplayPropertyModifiers": { "_Type_": "CraftingGameplayPropertyModifiers_List", "gameplayPropertyModifiers": list } })
    }

    fn slot(debug_name: &str, context: Vec<Value>, options: Vec<Value>) -> Value {
        json!({
            "_Type_": "CraftingCost_Select", "context": context,
            "nameInfo": { "_Type_": "CraftingNameInfo", "debugName": debug_name, "displayName": "@LOC_PLACEHOLDER" },
            "count": 1, "options": options
        })
    }

    /// A `CraftingBlueprintRecord` value laid out like the game's (`behr_rifle_ballistic_01`).
    fn record(mandatory: Value, optional: Vec<Value>) -> Value {
        json!({
            "_Type_": "CraftingBlueprintRecord",
            "blueprint": {
                "_Type_": "CraftingBlueprint",
                "category": { "_RecordName_": "BlueprintCategoryRecord.FPSWeapons", "_RecordId_": "cat" },
                "blueprintName": "@LOC_PLACEHOLDER",
                "processSpecificData": { "_Type_": "CraftingProcess_Creation", "entityClass": reference("EntityClassDefinition.behr_rifle_ballistic_01", "out") },
                "tiers": [{
                    "_Type_": "CraftingBlueprintTier",
                    "recipe": { "_Type_": "CraftingRecipe", "processSpecificData": null, "costs": {
                        "_Type_": "CraftingRecipeCosts",
                        "craftTime": { "_Type_": "TimeValue_Partitioned", "days": 0, "hours": 1, "minutes": 2, "seconds": 30.0 },
                        "mandatoryCost": mandatory,
                        "optionalCosts": optional
                    }, "results": null },
                    "research": null
                }]
            }
        })
    }

    #[test]
    fn a_blueprint_keeps_its_slots_quality_effects_and_multipliers() {
        let kick = reference("CraftingGameplayPropertyRecord.gpp_weapon_recoil_kick", "p-kick");
        // The game's dumps write some references as a bare path.
        let damage = json!("file://./../../crafting/craftedproperties/gpp_weapon_damage.json");
        let unknown = json!({ "_Type_": "CraftingGameplayPropertyModifierValueRange_Stepped", "startQuality": 0, "endQuality": 1 });
        let frame = slot("FRAME", vec![modifiers(vec![(kick, vec![linear(0, 1000, 1.2, 0.8)])])], vec![resource("Aluminum", 0.04, 0, json!([]))]);
        let barrel = slot(
            "BARREL:",
            vec![modifiers(vec![(damage, vec![additive(0, 249, -2, -2), additive(250, 1000, 0, 1), unknown])])],
            vec![
                resource("Ouratite", 0.04, 1, json!([{ "_Type_": "CraftingCostContext_QuantityMultiplier", "multiplier": 1.15 }])),
                json!({ "_Type_": "CraftingCost_Item", "context": [], "entityClass": reference("EntityClassDefinition.harvestable_mineral_1h_hadanite", "hadanite"), "quantity": 7, "minQuality": 500 }),
            ],
        );
        let mandatory = json!({
            "_Type_": "CraftingCost_Select",
            "context": [{ "_Type_": "CraftingCostContext_ResultCompositionInclusion", "option": "Include" }],
            "nameInfo": { "_Type_": "CraftingNameInfo", "debugName": "ASPECTS", "displayName": "@LOC_PLACEHOLDER" },
            "count": 2,
            "options": [frame, barrel]
        });
        let mut notes = BlueprintNotes::default();
        let bp = blueprint_from_record("bp-id", "BP_CRAFT_behr_rifle_ballistic_01", &record(mandatory, vec![]), &mut TableReader, &mut notes).expect("a creation blueprint");

        assert_eq!((bp.id.as_str(), bp.name.as_str(), bp.category.as_deref()), ("bp-id", "BP_CRAFT_behr_rifle_ballistic_01", Some("FPSWeapons")));
        assert_eq!((bp.output_item_id.as_deref(), bp.output_class_name.as_str(), bp.output_name.as_str()), (Some("out"), "behr_rifle_ballistic_01", "behr rifle ballistic 01"));
        assert_eq!(bp.display_name, None, "a placeholder name is no name");
        assert_eq!(bp.craft_time_sec, 3750.0);
        assert_eq!(bp.slots_required, Some(2));
        assert!(bp.optional_slots.is_empty() && bp.sources.is_empty());
        assert_eq!(
            bp.slots[0],
            BlueprintSlot {
                name: "Frame".into(),
                count: 1,
                options: vec![CraftInput { kind: "resource".into(), item_id: Some("res-Aluminum".into()), class_name: None, name: "Aluminum".into(), qty: 0.04, unit: Unit::Scu, min_quality: 0 }],
                quantity_multipliers: vec![],
                quality_effects: vec![QualityEffect {
                    key: "gpp_weapon_recoil_kick".into(),
                    name: None,
                    ranges: vec![QualityRange { kind: QualityRangeKind::Linear, start_quality: 0, end_quality: 1000, at_start: 1.2, at_end: 0.8 }],
                }],
            }
        );
        let barrel = &bp.slots[1];
        assert_eq!((barrel.name.as_str(), barrel.count), ("Barrel:", 1));
        assert_eq!(barrel.options.iter().map(|o| (o.name.as_str(), o.qty, o.unit, o.min_quality)).collect::<Vec<_>>(), [("Ouratite", 0.04, Unit::Scu, 1), ("harvestable_mineral_1h_hadanite", 7.0, Unit::Count, 500)]);
        assert_eq!(barrel.quantity_multipliers, [QuantityMultiplier { option: Some(0), multiplier: 1.15 }], "the multiplier stays with its option");
        assert_eq!(
            barrel.quality_effects,
            [QualityEffect {
                key: "gpp_weapon_damage".into(),
                name: Some("Damage".into()),
                ranges: vec![
                    QualityRange { kind: QualityRangeKind::IntegerAdditive, start_quality: 0, end_quality: 249, at_start: -2.0, at_end: -2.0 },
                    QualityRange { kind: QualityRangeKind::IntegerAdditive, start_quality: 250, end_quality: 1000, at_start: 0.0, at_end: 1.0 },
                ],
            }]
        );
        assert_eq!(notes, BlueprintNotes { skipped_ranges: 1, ..Default::default() }, "the unknown range type is counted, not guessed");
    }

    #[test]
    fn optional_costs_stay_apart_and_odd_shapes_are_counted() {
        let frame = slot("FRAME", vec![], vec![resource("Aluminum", 0.2, 0, json!([]))]);
        let paint = slot("PAINT", vec![], vec![resource("Iron", 0.01, 0, json!([]))]);
        let mut rec = record(frame, vec![paint]);
        rec["blueprint"]["blueprintName"] = json!("@mission_item_0075");
        let mut notes = BlueprintNotes::default();
        let bp = blueprint_from_record("b", "BP_CRAFT_x", &rec, &mut TableReader, &mut notes).expect("a creation blueprint");
        assert_eq!(bp.slots_required, None, "a single slot is not a group");
        assert_eq!(bp.slots.iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>(), ["Frame"]);
        assert_eq!(bp.optional_slots.iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>(), ["Paint"]);
        assert_eq!(bp.display_name.as_deref(), Some("Microsatellite"));
        assert_eq!(notes, BlueprintNotes::default());

        // A group inside the root group, carrying a quality effect no slot owns.
        let inner = json!({
            "_Type_": "CraftingCost_Select",
            "context": [modifiers(vec![(reference("CraftingGameplayPropertyRecord.gpp_a", "a"), vec![linear(0, 1000, 1.0, 2.0)])])],
            "count": 1,
            "options": [slot("A", vec![], vec![resource("Iron", 1.0, 0, json!([]))])]
        });
        let root = json!({ "_Type_": "CraftingCost_Select", "count": 1, "options": [inner] });
        let mut notes = BlueprintNotes::default();
        let bp = blueprint_from_record("b", "BP_CRAFT_x", &record(root, vec![]), &mut TableReader, &mut notes).expect("a creation blueprint");
        assert_eq!((bp.slots.len(), bp.slots_required), (1, Some(1)));
        assert_eq!(notes, BlueprintNotes { nested_groups: 1, unplaced: 1, ..Default::default() });

        // The game's one dismantle record is not a creation blueprint.
        let dismantle = json!({ "_Type_": "CraftingBlueprintRecord", "blueprint": { "_Type_": "CraftingBlueprint", "processSpecificData": { "_Type_": "GenericCraftingProcess_Dismantle" }, "tiers": [] } });
        let mut notes = BlueprintNotes::default();
        assert!(blueprint_from_record("d", "GlobalGenericDismantle", &dismantle, &mut TableReader, &mut notes).is_none());
        assert_eq!(notes, BlueprintNotes { not_creation: 1, ..Default::default() });

        // An output class name validation would refuse leaves the blueprint out.
        let mut rec = record(slot("FRAME", vec![], vec![resource("Iron", 1.0, 0, json!([]))]), vec![]);
        rec["blueprint"]["processSpecificData"]["entityClass"] = reference("EntityClassDefinition.bad name", "x");
        let mut notes = BlueprintNotes::default();
        assert!(blueprint_from_record("b", "BP_CRAFT_x", &rec, &mut TableReader, &mut notes).is_none());
        assert_eq!(notes, BlueprintNotes { unsafe_names: 1, ..Default::default() });
        assert_eq!(notes.warnings().len(), 1);
    }

    #[test]
    fn pool_shares_are_weights_over_the_pool_total() {
        let entry = |id: Option<&str>, weight: Option<f64>| {
            let mut e = json!({ "_Type_": "BlueprintReward", "blueprintRecord": id.map(|id| json!({ "$ref": id, "$name": "CraftingBlueprintRecord.x" })) });
            if let Some(w) = weight {
                e["weight"] = json!(w);
            }
            e
        };
        // Total 7: a 1 + b 3 + an empty entry 1 + a again 1 + c with no weight (1) + d negative (0).
        let pool = json!({ "blueprintRewards": [
            entry(Some("a"), Some(1.0)), entry(Some("b"), Some(3.0)), entry(None, Some(1.0)),
            entry(Some("a"), Some(1.0)), entry(Some("c"), None), entry(Some("d"), Some(-2.0))
        ] });
        assert_eq!(pool_shares(&pool), vec![("a".to_string(), 2.0 / 7.0), ("b".to_string(), 3.0 / 7.0), ("c".to_string(), 1.0 / 7.0), ("d".to_string(), 0.0)]);
        assert!(pool_shares(&json!({ "blueprintRewards": [entry(Some("a"), Some(0.0))] })).is_empty(), "no weight at all");
        assert!(pool_shares(&json!({})).is_empty());
    }

    #[test]
    fn a_source_chance_is_the_result_chance_times_the_share() {
        assert_eq!(result_chance(None), (1.0, false));
        assert_eq!(result_chance(Some(0.5)), (0.5, false));
        assert_eq!(result_chance(Some(1.5)), (1.0, true));
        assert_eq!(result_chance(Some(-0.1)), (0.0, true));
        let shares = pool_shares(&json!({ "blueprintRewards": [{ "weight": 1.0, "blueprintRecord": { "$ref": "a" } }, { "weight": 3.0, "blueprintRecord": { "$ref": "b" } }] }));
        let (chance, _) = result_chance(Some(0.5));
        assert_eq!(shares.iter().map(|(id, share)| (id.as_str(), chance * share)).collect::<Vec<_>>(), [("a", 0.125), ("b", 0.375)]);
        // The pool chance is the result's own chance, kept only below 1.
        assert_eq!(pool_chance(chance), Some(0.5));
        assert_eq!(pool_chance(0.25), Some(0.25));
        assert_eq!(pool_chance(0.0), Some(0.0));
        assert_eq!(pool_chance(result_chance(None).0), None, "a result without chance always gives one");
        assert_eq!(pool_chance(result_chance(Some(1.5)).0), None, "clamped to 1");

        assert_eq!(result_when(&json!({ "missionResults": [true, false] })), Some("complete"));
        assert_eq!(result_when(&json!({ "missionResults": [false, true] })), Some("accept"));
        assert_eq!(result_when(&json!({})), Some("complete"), "no flags: on success, as the Wikelo rewards read it");
        assert_eq!(result_when(&json!({ "missionResults": [false, false] })), None);
    }

    #[test]
    fn property_keys_come_from_the_record_name_or_the_file_name() {
        let key = |v: Value| property_key(&v);
        assert_eq!(key(json!({ "$ref": "x", "$name": "CraftingGameplayPropertyRecord.gpp_weapon_recoil_kick", "$path": "a/b.json" })).as_deref(), Some("gpp_weapon_recoil_kick"));
        assert_eq!(key(json!({ "_RecordName_": "T.gpp_armor_damagemitigation", "_RecordId_": "x" })).as_deref(), Some("gpp_armor_damagemitigation"));
        assert_eq!(key(json!({ "$ref": "x", "$path": "libs/foundry/records/crafting/craftedproperties/gpp_health_maxhealth.json" })).as_deref(), Some("gpp_health_maxhealth"));
        assert_eq!(key(json!("file://./../../crafting/craftedproperties/gpp_weapon_firerate.json")).as_deref(), Some("gpp_weapon_firerate"));
        assert_eq!(key(json!(null)), None);
        assert_eq!(key(json!({ "$name": " " })), None);
    }

    #[test]
    fn a_property_name_is_its_display_name_or_another_name_key() {
        let loc = Localization::parse(b"gpp_kick=Recoil Kick\r\nui_damage=Damage\r\n");
        assert_eq!(localized_name(&json!({ "displayName": "@gpp_kick", "propertyName": "@ui_damage" }), &loc).as_deref(), Some("Recoil Kick"));
        assert_eq!(localized_name(&json!({ "displayName": "@LOC_PLACEHOLDER", "uiName": "@ui_damage" }), &loc).as_deref(), Some("Damage"));
        assert_eq!(localized_name(&json!({ "displayName": "@missing", "description": "@ui_damage" }), &loc), None, "only fields that say name");
        assert_eq!(localized_name(&json!({ "name": "ui_damage" }), &loc), None, "a key needs its @");
    }

    #[test]
    fn long_text_is_cut_to_what_validation_accepts() {
        assert_eq!(fit("Frame".into()), "Frame");
        assert_eq!(fit("é".repeat(201)).chars().count(), 200);
    }

    // ----- what a blueprint's item is ------------------------------------------------------

    /// A crafted item record laid out like the game's (`COOL_ACOM_S01_IcePlunge_SCItem`): the
    /// item definition sits in the `SAttachableComponentParams`, after other components.
    fn item_record(item_type: &str, sub_type: &str, size: i64, grade: i64, manufacturer: Value, description: &str) -> Value {
        json!({
            "_Type_": "EntityClassDefinition",
            "Components": [
                { "_Type_": "SGeometryResourceParams", "Geometry": {} },
                { "_Type_": "SAttachableComponentParams", "AttachDef": {
                    "_Type_": "SItemDefinition", "Type": item_type, "SubType": sub_type, "Size": size, "Grade": grade,
                    "Manufacturer": manufacturer, "inheritParentManufacturer": false, "Tags": "",
                    "Localization": { "_Type_": "SCItemLocalization", "Name": "@item_Name", "ShortName": "@LOC_EMPTY", "Description": description }
                } }
            ]
        })
    }

    fn manufacturer_record(code: &str, name_key: &str) -> Value {
        json!({ "_Type_": "SCItemManufacturer", "Localization": { "_Type_": "SCItemLocalization", "Name": name_key, "ShortName": "@LOC_EMPTY" }, "Code": code })
    }

    const COOLER_PATH: &str = "libs/foundry/records/crafting/blueprints/crafting/vehiclecomponents/cooler/bp_craft_cool_acom_s01_iceplunge.json";

    /// A reader that knows the ACOM IcePlunge cooler (the blueprint record's output is `out`).
    fn cooler_reader() -> RecordReader {
        let mut reader = RecordReader::default();
        let acom = reference("SCItemManufacturer.ACOM", "m-acom");
        reader.records.insert("out".into(), item_record("Cooler", "UNDEFINED", 1, 3, acom, "@item_DescCOOL_ACOM_S01_IcePlunge"));
        reader.records.insert("m-acom".into(), manufacturer_record("ACOM", "@manufacturer_NameACOM"));
        reader.texts.insert("@manufacturer_NameACOM".into(), "ACOM".into());
        reader.texts.insert(
            "@item_DescCOOL_ACOM_S01_IcePlunge".into(),
            "Item Type: Cooler\nManufacturer: ACOM\nSize: 1\nGrade: C\nClass: Competition\n\nWhen you're pushing your ship to the limit, you'll be glad you have ACOM's IcePlunge.".into(),
        );
        reader
    }

    /// The test blueprint record read and described with `reader`.
    fn described(reader: &mut RecordReader, path: &str, defaults: Option<&DefaultBlueprints>) -> (Blueprint, BlueprintNotes) {
        let rec = record(slot("FRAME", vec![], vec![resource("Iron", 1.0, 0, json!([]))]), vec![]);
        let mut notes = BlueprintNotes::default();
        let mut bp = blueprint_from_record("bp-id", "BP_CRAFT_x", &rec, reader, &mut notes).expect("a creation blueprint");
        describe_blueprint_item(&mut bp, &rec, path, defaults, reader, &mut notes);
        (bp, notes)
    }

    #[test]
    fn a_blueprint_says_what_its_item_is() {
        let (bp, notes) = described(&mut cooler_reader(), COOLER_PATH, None);
        assert_eq!(bp.item_type.as_deref(), Some("Cooler"));
        assert_eq!(bp.item_sub_type, None, "UNDEFINED is no sub type");
        assert_eq!((bp.size, bp.grade), (Some(1), Some(3)));
        assert_eq!(bp.manufacturer, Some(BlueprintManufacturer { code: "ACOM".into(), name: "ACOM".into() }));
        assert_eq!(bp.item_class.as_deref(), Some("Competition"));
        assert_eq!(bp.tags, ["vehiclecomponents", "cooler"]);
        assert!(!bp.default, "no default list");
        assert_eq!(notes, BlueprintNotes::default(), "UNDEFINED sub type and no default list are not misses here");

        // A sub type the game fills in is kept; the rest of the item reads the same.
        let mut reader = cooler_reader();
        reader.records.insert("out".into(), item_record("WeaponAttachment", "Magazine", 1, 1, json!(null), "@item_Descbehr_lmg_ballistic_01_mag"));
        reader.texts.insert("@item_Descbehr_lmg_ballistic_01_mag".into(), "Item Type: Magazine\nCapacity: 75\n\nThis magazine for the Behring FS-9 LMG holds seventy five 5mm rounds.".into());
        let (bp, notes) = described(&mut reader, "libs/foundry/records/crafting/blueprints/crafting/fpsgear/ammo/ballistic/bp_craft_behr_lmg_ballistic_01_mag.json", None);
        assert_eq!((bp.item_type.as_deref(), bp.item_sub_type.as_deref()), (Some("WeaponAttachment"), Some("Magazine")));
        assert_eq!((bp.manufacturer, bp.item_class), (None, None), "no manufacturer and no Class: line");
        assert_eq!(bp.tags, ["fpsgear", "ammo", "ballistic"]);
        assert_eq!(notes, BlueprintNotes::default(), "an item without a manufacturer or a class is not a miss");
    }

    #[test]
    fn what_cannot_be_read_about_the_item_is_left_out_and_counted() {
        // The item record does not resolve: nothing about the item, the path still gives tags.
        let (bp, notes) = described(&mut RecordReader::default(), COOLER_PATH, None);
        assert_eq!((bp.item_type, bp.size, bp.grade, bp.manufacturer), (None, None, None, None));
        assert_eq!(bp.tags, ["vehiclecomponents", "cooler"]);
        assert_eq!(notes, BlueprintNotes { no_item_record: 1, ..Default::default() });

        // An item without an item definition.
        let mut reader = RecordReader::default();
        reader.records.insert("out".into(), json!({ "_Type_": "EntityClassDefinition", "Components": [{ "_Type_": "SGeometryResourceParams" }] }));
        let (bp, notes) = described(&mut reader, COOLER_PATH, None);
        assert_eq!(bp.item_type, None);
        assert_eq!(notes, BlueprintNotes { no_item_definition: 1, ..Default::default() });

        // UNDEFINED type, size past 12, grade 0, a manufacturer that does not resolve.
        let mut reader = cooler_reader();
        reader.records.insert("out".into(), item_record("UNDEFINED", "Gun", 13, 0, reference("SCItemManufacturer.GONE", "m-gone"), "@item_DescCOOL_ACOM_S01_IcePlunge"));
        let (bp, notes) = described(&mut reader, COOLER_PATH, None);
        assert_eq!((bp.item_type, bp.item_sub_type.as_deref(), bp.size, bp.grade, bp.manufacturer), (None, Some("Gun"), None, None, None));
        assert_eq!(bp.item_class.as_deref(), Some("Competition"), "the rest is still read");
        assert_eq!(notes, BlueprintNotes { no_item_type: 1, no_size: 1, no_grade: 1, unresolved_manufacturers: 1, ..Default::default() });
        assert_eq!(notes.warnings().len(), 4, "one line per kind of miss: {:?}", notes.warnings());

        // A type validation would refuse, and a manufacturer whose name is a placeholder.
        let mut reader = cooler_reader();
        reader.records.insert("m-acom".into(), manufacturer_record("ACOM", "@LOC_PLACEHOLDER"));
        reader.records.insert("out".into(), item_record(&"X".repeat(MAX_BLUEPRINT_LABEL + 1), "UNDEFINED", 1, 3, reference("SCItemManufacturer.ACOM", "m-acom"), "@none"));
        let (bp, notes) = described(&mut reader, COOLER_PATH, None);
        assert_eq!((bp.item_type, bp.manufacturer, bp.item_class), (None, None, None));
        assert_eq!(notes, BlueprintNotes { unusable_labels: 1, unnamed_manufacturers: 1, ..Default::default() });

        // The path is not under blueprints/crafting/.
        let (bp, notes) = described(&mut cooler_reader(), "libs/foundry/records/crafting/blueprints/dismantle/x.json", None);
        assert!(bp.tags.is_empty());
        assert_eq!(notes, BlueprintNotes { no_tag_folder: 1, ..Default::default() });
    }

    #[test]
    fn a_missing_manufacturer_is_none_without_a_miss() {
        let mut notes = BlueprintNotes::default();
        let mut reader = cooler_reader();
        for none in [json!(null), json!(""), json!({ "$ref": "00000000-0000-0000-0000-000000000000", "$name": "", "$path": "" })] {
            assert_eq!(item_manufacturer(Some(&none), &mut reader, &mut notes), None, "{none}");
        }
        assert_eq!(item_manufacturer(None, &mut reader, &mut notes), None);
        assert_eq!(notes, BlueprintNotes::default());

        let acom = reference("SCItemManufacturer.ACOM", "m-acom");
        assert_eq!(item_manufacturer(Some(&acom), &mut reader, &mut notes), Some(BlueprintManufacturer { code: "ACOM".into(), name: "ACOM".into() }));
        // A code with nothing in it, or past the limit, is no code.
        let long = "A".repeat(MAX_MANUFACTURER_CODE + 1);
        for code in [" ", long.as_str()] {
            reader.records.insert("m-acom".into(), manufacturer_record(code, "@manufacturer_NameACOM"));
            assert_eq!(item_manufacturer(Some(&acom), &mut reader, &mut notes), None, "{code:?}");
        }
        assert_eq!(notes, BlueprintNotes { unnamed_manufacturers: 2, ..Default::default() });
    }

    #[test]
    fn the_class_is_the_class_line_of_the_description_header() {
        // Where the cooler has it: after the other header lines.
        assert_eq!(description_class("Item Type: Cooler\nManufacturer: ACOM\nSize: 1\nGrade: C\nClass: Competition\n\nWhen you're pushing..."), Some("Competition"));
        // First, or with odd spacing and case.
        assert_eq!(description_class("Class: Military\nItem Type: Quantum Drive\n\nText."), Some("Military"));
        assert_eq!(description_class("Manufacturer: Wei-Tek\r\n  class :  Stealth  \r\n\r\nText."), Some("Stealth"));
        // A leading blank line does not hide the header.
        assert_eq!(description_class("\nItem Type: Shield Generator\nClass: Industrial"), Some("Industrial"));
        // Absent: a header without it (the laser cannon starts with its manufacturer), a
        // `Class:` in the prose, an empty value, another key.
        assert_eq!(description_class("Manufacturer: Amon & Reese Co.\nItem Type: Laser Cannon\nSize: 1\n\nThe Omnisky III is the base model."), None);
        assert_eq!(description_class("Item Type: Magazine\nCapacity: 75\n\nClass: in the prose is not a header line"), None);
        assert_eq!(description_class("Class:\nSize: 1"), None);
        assert_eq!(description_class("Classification: Civilian"), None);
        assert_eq!(description_class(""), None);
    }

    #[test]
    fn undefined_or_empty_labels_are_none() {
        let mut notes = BlueprintNotes::default();
        assert_eq!(item_label(Some("UNDEFINED"), &mut notes), None);
        assert_eq!(item_label(Some("undefined"), &mut notes), None);
        assert_eq!(item_label(Some("  "), &mut notes), None);
        assert_eq!(item_label(None, &mut notes), None);
        assert_eq!(item_label(Some(" Medium "), &mut notes).as_deref(), Some("Medium"));
        assert_eq!(notes, BlueprintNotes::default());
        assert_eq!(item_label(Some("a\u{1}b"), &mut notes), None);
        assert_eq!(notes, BlueprintNotes { unusable_labels: 1, ..Default::default() });
    }

    #[test]
    fn tags_are_the_folders_after_blueprints_crafting() {
        let tags = |path: &str| blueprint_tags(path).map(|(t, dropped)| (t.join(","), dropped));
        let owned = |t: &str, dropped: usize| Some((t.to_string(), dropped));
        assert_eq!(tags("libs/foundry/records/crafting/blueprints/crafting/fpsgear/ammo/ballistic/bp_craft_behr_rifle_ballistic_01_mag.json"), owned("fpsgear,ammo,ballistic", 0));
        assert_eq!(tags(r"Libs\Foundry\Records\Crafting\Blueprints\Crafting\FPSGear\Armour\Combat\Light\BP_CRAFT_cds_armor_light_helmet_01_01_01.json"), owned("fpsgear,armour,combat,light", 0), "Windows separators and upper case");
        assert_eq!(tags("file://./../../../../../libs/foundry/records/crafting/blueprints/crafting/fpsgear/weapons/rifle/bp_craft_behr_rifle_ballistic_01.json"), owned("fpsgear,weapons,rifle", 0), "a reference's path");
        assert_eq!(tags(r"libs/foundry\records/crafting\blueprints/crafting\fpsgear/weapons\bp.json"), owned("fpsgear,weapons", 0), "mixed separators");
        assert_eq!(tags("blueprints/crafting/fpsgear/bp.json"), owned("fpsgear", 0), "the path starts there");
        assert_eq!(tags("libs/foundry/records/crafting/blueprints/crafting/bp.json"), owned("", 0), "no folder: no tag, no miss");
        assert_eq!(tags("libs/foundry/records/crafting/blueprints/crafting//fpsgear/bp.json"), owned("fpsgear", 0), "an empty segment is no folder");
        assert_eq!(tags("blueprints/crafting/a/b/c/d/e/f/g/bp.json"), owned("a,b,c,d,e,f", 1), "six at most");
        assert_eq!(tags("blueprints/crafting/fps gear/weapons/../bp.json"), owned("weapons", 2), "folders a tag cannot carry");
        assert_eq!(tags("libs/foundry/records/crafting/blueprints/dismantle/globalgenericdismantle.json"), None);
        assert_eq!(tags("libs/foundry/records/crafting/myblueprints/crafting/x/bp.json"), None, "a whole folder name");
        assert_eq!(tags(""), None);
    }

    #[test]
    fn the_default_list_is_read_by_id_or_by_path() {
        let params = json!({
            "_Type_": "CraftingGlobalParams",
            "defaultBlueprintSelection": { "_Type_": "DefaultBlueprintSelection_Whitelist", "blueprintRecords": [
                "file://./../../../../../libs/foundry/records/crafting/blueprints/dismantle/globalgenericdismantle.json",
                "file://./../../../../../libs/foundry/records/crafting/blueprints/crafting/fpsgear/weapons/rifle/bp_craft_behr_rifle_ballistic_01.json",
                { "$ref": "bp-pistol", "$name": "CraftingBlueprintRecord.BP_CRAFT_behr_pistol_ballistic_01", "$path": "libs/foundry/records/crafting/blueprints/crafting/fpsgear/weapons/pistol/bp_craft_behr_pistol_ballistic_01.json" },
                null
            ] }
        });
        let list = default_blueprint_list(&params).expect("a list");
        assert_eq!(list.ids, HashSet::from(["bp-pistol".to_string()]));
        assert_eq!(list.paths.len(), 3);
        assert!(list.contains("some-id", r"Libs\Foundry\Records\Crafting\Blueprints\Crafting\FPSGear\Weapons\Rifle\BP_CRAFT_behr_rifle_ballistic_01.json"), "by path");
        assert!(list.contains("bp-pistol", "somewhere/else.json"), "by id");
        assert!(!list.contains("other", "libs/foundry/records/crafting/blueprints/crafting/fpsgear/weapons/rifle/bp_craft_other.json"));
        assert!(!list.contains("other", ""));
        assert_eq!(default_blueprint_list(&json!({ "defaultBlueprintSelection": { "_Type_": "DefaultBlueprintSelection_All" } })), None, "another kind of selection");
        assert_eq!(default_blueprint_list(&json!({ "defaultBlueprintSelection": null })), None);
        assert_eq!(default_blueprint_list(&json!({})), None);

        // On a blueprint: by its record id, by its path, and not without a list.
        let by_id = DefaultBlueprints { ids: HashSet::from(["bp-id".to_string()]), paths: HashSet::new() };
        assert!(described(&mut cooler_reader(), COOLER_PATH, Some(&by_id)).0.default);
        let by_path = DefaultBlueprints { ids: HashSet::new(), paths: blueprint_path_key(COOLER_PATH).into_iter().collect() };
        assert!(described(&mut cooler_reader(), COOLER_PATH, Some(&by_path)).0.default);
        assert!(!described(&mut cooler_reader(), COOLER_PATH, Some(&DefaultBlueprints::default())).0.default);
        assert!(!described(&mut cooler_reader(), COOLER_PATH, None).0.default);
    }

    #[test]
    fn the_item_fields_reach_the_json_only_when_known() {
        let (bp, _) = described(&mut cooler_reader(), COOLER_PATH, None);
        let json = serde_json::to_value(&bp).unwrap();
        assert_eq!(json["itemType"], "Cooler");
        assert_eq!(json["manufacturer"], json!({ "code": "ACOM", "name": "ACOM" }));
        assert_eq!(json["itemClass"], "Competition");
        assert_eq!(json["tags"], json!(["vehiclecomponents", "cooler"]));
        assert_eq!((json["size"].clone(), json["grade"].clone()), (json!(1), json!(3)));
        assert!(json.get("itemSubType").is_none() && json.get("default").is_none(), "{json}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_prefers_max_then_min() {
        assert_eq!(amounts(Some(15), Some(15)), (15, 15));
        assert_eq!(amounts(Some(1), Some(0)), (1, 1));
        assert_eq!(amounts(None, Some(4)), (4, 4));
        assert_eq!(amounts(None, None), (0, 0));
    }

    #[test]
    fn item_kinds() {
        assert_eq!(item_kind("Carryable_1H_CY_banu_favour_Wikelo", Some("Misc"), Some("Harvestable"), "Wikelo Favor"), "favour");
        assert_eq!(item_kind("Carryable_1H_CY_Physical_Currency_Scrip_Merc_1", Some("Currency"), None, "MG Scrip"), "currency");
        assert_eq!(item_kind("qrt_combat_heavy_arms_02_02_01", Some("Char_Armor_Arms"), Some("Heavy"), "Ana Arms Endro"), "armor");
        assert_eq!(item_kind("RSI_Scorpius_Stealth", Some("NOITEM_Vehicle"), Some("Vehicle_Spaceship"), "x"), "vehicle");
        assert_eq!(item_kind("Harvestable_Mineral_1H_Carinite", Some("Misc"), Some("Harvestable"), "Carinite"), "harvestable");
    }

    #[test]
    fn refined_resources_take_their_ores_tables() {
        let own: HashMap<String, Vec<i64>> = [("ore_a".to_string(), vec![330, 517]), ("refined_own".to_string(), vec![900])].into_iter().collect();
        let mut from_ores: BTreeMap<String, Vec<(String, Vec<i64>)>> = BTreeMap::new();
        // Today's data: one ore per refined resource.
        from_ores.insert("refined_one".into(), vec![("ore_a".into(), vec![330, 517])]);
        // Its own table wins.
        from_ores.insert("refined_own".into(), vec![("ore_a".into(), vec![330, 517])]);
        // Two ores with the same table: no warning.
        from_ores.insert("refined_same".into(), vec![("ore_b".into(), vec![100, 200]), ("ore_c".into(), vec![100, 200])]);
        // Two ores that disagree: the union, and a warning.
        from_ores.insert("refined_mixed".into(), vec![("ore_e".into(), vec![517, 874]), ("ore_d".into(), vec![330, 517])]);
        let (got, warnings) = refined_qualities(&own, from_ores);
        assert_eq!(
            got,
            vec![("refined_mixed".to_string(), vec![330, 517, 874]), ("refined_one".to_string(), vec![330, 517]), ("refined_same".to_string(), vec![100, 200])]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("refined_mixed") && warnings[0].contains("ore_d, ore_e"), "{warnings:?}");
    }

    #[test]
    fn only_a_single_resource_composition_names_a_resource() {
        let id = "11111111-2222-3333-4444-555555555555";
        let one = serde_json::json!({ "defaultComposition": [{ "entry": { "$ref": id } }] });
        assert_eq!(composition_resource(&one), Composition::One(id));
        let two = serde_json::json!({ "defaultComposition": [{ "entry": { "$ref": id } }, { "entry": { "$ref": "other" } }] });
        assert_eq!(composition_resource(&two), Composition::Several);
        let twice = serde_json::json!({ "defaultComposition": [{ "entry": { "$ref": id } }, { "entry": { "$ref": id } }] });
        assert_eq!(composition_resource(&twice), Composition::One(id));
    }

    #[test]
    fn placeholder_names_are_not_places() {
        assert!(real_place_name("Orison"));
        for bad in ["", "<= PLACEHOLDER =>", "<= UNINITIALIZED =>", "@LOC_UNINITIALIZED"] {
            assert!(!real_place_name(bad), "{bad}");
        }
        assert_eq!(composition_resource(&serde_json::json!({ "defaultComposition": [] })), Composition::Empty);
        assert_eq!(composition_resource(&serde_json::json!({})), Composition::Empty);
        assert_eq!(composition_resource(&serde_json::json!({ "defaultComposition": [{ "entry": 3 }] })), Composition::Empty);
    }

    #[test]
    fn the_backdrop_is_the_file_name_with_either_separator() {
        assert_eq!(backdrop_file("UI/Textures/Backdrops/PUBackdrop_Orison.dds"), "pubackdrop_orison.dds");
        assert_eq!(backdrop_file(r"UI\Textures\Backdrops\PUBackdrop_Orison.dds"), "pubackdrop_orison.dds");
        assert_eq!(backdrop_file(r"UI\Textures/Backdrops\pubackdrop_area18.dds"), "pubackdrop_area18.dds", "mixed");
        assert_eq!(backdrop_file("pubackdrop_lorville.dds"), "pubackdrop_lorville.dds", "no directory");
        assert_eq!(backdrop_file(""), "");
        assert_eq!(backdrop_file("UI/"), "");
    }

    fn map(entries: &[(&str, Option<&str>, &str, Option<&str>)]) -> HashMap<String, MapNode> {
        entries
            .iter()
            .map(|&(id, name, icon, parent)| (id.to_string(), MapNode { name: name.map(str::to_string), icon: icon.into(), parent: parent.map(str::to_string) }))
            .collect()
    }

    #[test]
    fn the_climb_finds_the_body_and_the_system() {
        let nodes = map(&[
            ("Stanton", Some("Stanton"), "Star", None),
            ("Stanton2", Some("Crusader"), "Planet", Some("Stanton")),
            ("Stanton2_L1", Some("CRU-L1"), "Default", Some("Stanton2")),
            ("Orison", Some("Orison"), "LandingZone", Some("Stanton2")),
            ("Stanton2b", Some("Daymar"), "Moon", Some("Stanton2")),
            ("Paf", Some("PAF-I"), "Outpost", Some("Stanton2b")),
            ("Station", Some("Estação"), "Station", Some("Stanton2_L1")),
            ("Lost", Some("Perdido"), "Outpost", Some("Gone")),
        ]);
        assert_eq!(climb_star_map("Orison", &nodes), Climb { body: Some("Crusader".into()), system: Some("Stanton".into()), ..Default::default() });
        assert_eq!(climb_star_map("Station", &nodes), Climb { body: Some("Crusader".into()), system: Some("Stanton".into()), ..Default::default() }, "a planet above wins over the nearer named point");
        assert_eq!(
            climb_star_map("Paf", &nodes),
            Climb { body: Some("Daymar".into()), orbits: Some("Crusader".into()), system: Some("Stanton".into()), problem: None },
            "a moon's planet is what it orbits"
        );
        assert_eq!(climb_star_map("Lost", &nodes), Climb::default(), "a missing parent ends the climb quietly");
    }

    #[test]
    fn the_climb_goes_past_eight_levels_and_reports_cycles_and_overflow() {
        // Nine intermediate nodes between the place and the star.
        let mut entries: Vec<(String, Option<String>, String, Option<String>)> = vec![("Sun".into(), Some("Pyro".into()), "Star".into(), None)];
        let mut parent = "Sun".to_string();
        for i in 0..9 {
            let id = format!("n{i}");
            entries.push((id.clone(), None, "Default".into(), Some(parent)));
            parent = id;
        }
        entries.push(("Deep".into(), Some("Fundo".into()), "Outpost".into(), Some(parent)));
        let nodes: HashMap<String, MapNode> = entries.into_iter().map(|(id, name, icon, parent)| (id, MapNode { name, icon, parent })).collect();
        assert_eq!(climb_star_map("Deep", &nodes), Climb { system: Some("Pyro".into()), ..Default::default() });

        // A loop: stops at the repeated parent, with a warning.
        let looped = map(&[("A", Some("A"), "Outpost", Some("B")), ("B", Some("Bee"), "Default", Some("C")), ("C", None, "Default", Some("B"))]);
        let got = climb_star_map("A", &looped);
        assert_eq!((got.body.as_deref(), got.system.as_deref()), (Some("Bee"), None));
        assert!(got.problem.as_deref().is_some_and(|p| p.contains("A") && p.contains("loop")), "{got:?}");
        let self_loop = map(&[("S", Some("S"), "Outpost", Some("S"))]);
        assert!(climb_star_map("S", &self_loop).problem.is_some(), "a record that is its own parent");

        // Deeper than the limit: stops there, with a warning.
        let mut deep: Vec<(String, Option<String>, String, Option<String>)> = vec![("Sun".into(), Some("Nyx".into()), "Star".into(), None)];
        let mut parent = "Sun".to_string();
        for i in 0..MAX_MAP_DEPTH {
            let id = format!("d{i}");
            deep.push((id.clone(), None, "Default".into(), Some(parent)));
            parent = id;
        }
        deep.push(("Bottom".into(), Some("Fundo".into()), "Outpost".into(), Some(parent)));
        let nodes: HashMap<String, MapNode> = deep.into_iter().map(|(id, name, icon, parent)| (id, MapNode { name, icon, parent })).collect();
        let got = climb_star_map("Bottom", &nodes);
        assert_eq!(got.system, None);
        assert!(got.problem.as_deref().is_some_and(|p| p.contains("Bottom") && p.contains("32")), "{got:?}");
        // Exactly at the limit (the star is the 32nd ancestor) still reaches the system.
        let at_limit = climb_star_map(&format!("d{}", MAX_MAP_DEPTH - 1), &nodes);
        assert_eq!((at_limit.system.as_deref(), at_limit.problem), (Some("Nyx"), None));
    }
}

#[cfg(test)]
mod quality_tier_tests {
    use super::*;
    use serde_json::json;

    /// `_RecordValue_` of `CraftingQualityQuantizationRecord.Quantization_Agricium` as the game has it.
    fn agricium() -> Value {
        let band = |start: i64, end: i64, mapped: i64| json!({ "_Type_": "CraftingQualityQuantizationBand", "start": start, "end": end, "mappedValue": mapped });
        json!({
            "_Type_": "CraftingQualityQuantizationRecord",
            "qualityQuantization": {
                "_Type_": "CraftingQualityQuantization",
                "bands": [
                    band(0, 399, 346), band(400, 599, 588), band(600, 699, 667), band(700, 799, 796),
                    band(800, 899, 852), band(900, 949, 943), band(950, 998, 971), band(999, 1000, 1000)
                ]
            }
        })
    }

    #[test]
    fn bands_are_read_from_the_record() {
        let bands = quality_bands(&agricium()).expect("the game's record reads");
        assert_eq!(bands.len(), 8);
        assert_eq!(bands[0], QualityBand { start: 0, end: 399, value: 346 });
        assert_eq!(bands[5], QualityBand { start: 900, end: 949, value: 943 });
        assert_eq!(bands[7], QualityBand { start: 999, end: 1000, value: 1000 });

        // Listed out of order: sorted.
        let mut shuffled = agricium();
        shuffled["qualityQuantization"]["bands"].as_array_mut().unwrap().reverse();
        assert_eq!(quality_bands(&shuffled), Some(bands));
    }

    #[test]
    fn unusable_bands_leave_the_record_out() {
        let with = |change: &dyn Fn(&mut Value)| {
            let mut v = agricium();
            change(&mut v);
            quality_bands(&v)
        };
        assert_eq!(
            with(&|v| {
                v["qualityQuantization"]["bands"][2].as_object_mut().unwrap().remove("mappedValue");
            }),
            None,
            "a band without a value"
        );
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"][0]["start"] = json!("zero")), None, "a start that is no number");
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"][7]["end"] = json!(1001)), None, "above the scale");
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"][0]["mappedValue"] = json!(-1)), None, "a negative value");
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"][1]["start"] = json!(399)), None, "overlapping bands");
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"][3]["end"] = json!(650)), None, "an inverted band");
        assert_eq!(with(&|v| v["qualityQuantization"]["bands"] = json!([])), None, "no band");
        assert_eq!(with(&|v| *v = json!({ "_Type_": "CraftingQualityQuantizationRecord" })), None, "no quantization");
    }

    #[test]
    fn a_material_is_named_after_the_resources_that_use_its_record() {
        let slots: HashSet<&str> = ["Agricium", "Iron"].into_iter().collect();
        // A name the blueprint slots use wins, whether refined or not.
        assert_eq!(pick_tier_name("Agricium", &["Agricium"], &["Agricium (Ore)"], None, &slots), ("Agricium".to_string(), TierLink::Reference));
        assert_eq!(pick_tier_name("Iron", &["Refined Iron"], &["Iron"], None, &slots), ("Iron".to_string(), TierLink::Reference));
        // Otherwise the refined resource, then the resource itself.
        assert_eq!(pick_tier_name("Gold", &["Gold"], &["Gold (Ore)"], Some("Golden"), &slots), ("Gold".to_string(), TierLink::Reference));
        assert_eq!(pick_tier_name("JumpingLimes", &[], &["Jumping Limes"], None, &slots), ("Jumping Limes".to_string(), TierLink::Reference));
        // Nobody points at the record: the resource with the same record name, then the key.
        assert_eq!(pick_tier_name("Tin", &[], &[], Some("Tin"), &slots), ("Tin".to_string(), TierLink::RecordName));
        assert_eq!(pick_tier_name("HeartOfTheWoods", &[], &[], None, &slots), ("Heart Of The Woods".to_string(), TierLink::Key));
    }

    #[test]
    fn keys_split_into_words() {
        assert_eq!(words_of_key("JumpingLimes"), "Jumping Limes");
        assert_eq!(words_of_key("Agricium"), "Agricium");
        assert_eq!(words_of_key("VLKFang"), "VLK Fang");
        assert_eq!(words_of_key("TEMPLATE"), "TEMPLATE");
        assert_eq!(words_of_key("Quasi2Egg"), "Quasi2 Egg");
        assert_eq!(words_of_key(""), "");
    }
}

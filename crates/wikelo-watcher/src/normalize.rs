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
    items: BTreeMap<String, Item>,
    emporiums: Vec<EmporiumInfo>,
    scope_ids: HashSet<String>,
    warnings: Vec<String>,
    /// Blueprint record id -> (contract id, granted on accept, chance).
    blueprint_sources: HashMap<String, Vec<(String, bool, f64)>>,
}

impl<'r, 'db, 'a> Normalizer<'r, 'db, 'a> {
    pub fn new(res: &'r mut RefResolver<'db, 'a>, loc: &'r Localization) -> Self {
        Self {
            res,
            loc,
            tag_names: HashMap::new(),
            resource_names: HashMap::new(),
            resource_qualities: HashMap::new(),
            items: BTreeMap::new(),
            emporiums: Vec::new(),
            scope_ids: HashSet::new(),
            warnings: Vec::new(),
            blueprint_sources: HashMap::new(),
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
            let craft_time_sec = costs
                .and_then(|c| c.get("craftTime"))
                .map(|t| {
                    f(t, "days").unwrap_or(0.0) * 86_400.0
                        + f(t, "hours").unwrap_or(0.0) * 3_600.0
                        + f(t, "minutes").unwrap_or(0.0) * 60.0
                        + f(t, "seconds").unwrap_or(0.0)
                })
                .unwrap_or(0.0);
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
                    let info = cost.get("nameInfo");
                    let name = info
                        .and_then(|n| s(n, "displayName"))
                        .and_then(|k| self.loc.text_opt(k))
                        .or_else(|| info.and_then(|n| s(n, "debugName")).map(title_case))
                        .unwrap_or_else(|| "Material".into());
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

    fn craft_input(&mut self, o: &Value) -> Option<CraftInput> {
        match ty(o)? {
            "CraftingCost_Resource" => {
                let rid = o.get("resource").and_then(ref_id)?.to_string();
                let name = self
                    .resource_names
                    .get(&rid)
                    .cloned()
                    .or_else(|| o.get("resource").and_then(|r| self.res.ref_name(r)).map(|n| n.trim_start_matches("ResourceType.").to_string()))
                    .unwrap_or_else(|| "?".into());
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
                walk(&root, &mut |node| {
                    if ty(node) == Some("ResourceType")
                        && let Some(id) = s(node, "_RecordId_")
                    {
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
                for (id, table, refined) in tables {
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
        let id = ref_id(eref)?.to_string();
        if let Some(it) = self.items.get(&id) {
            return Some(it.clone());
        }
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
        let item = Item { id: id.clone(), class_name, name, item_type, sub_type, kind: kind.to_string(), image: None, image_source: None, qualities };
        self.items.insert(id, item.clone());
        Some(item)
    }
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

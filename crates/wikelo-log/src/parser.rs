//! Regex-based classification of Game.log lines into [`LogEvent`]s.
//!
//! Every pattern here was checked against real 4.10.x logs. Lines are
//! pre-filtered with cheap substring checks before any regex runs.

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionType {
    Complete,
    Abandon,
    Fail,
    Disconnect,
    Deactivate,
    Other(String),
}

impl CompletionType {
    pub fn parse(s: &str) -> Self {
        match s {
            "Complete" => Self::Complete,
            "Abandon" => Self::Abandon,
            "Fail" => Self::Fail,
            "Disconnect" => Self::Disconnect,
            "Deactivate" => Self::Deactivate,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationRef {
    pub name: String,
    pub hash: Option<u64>,
    pub class_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LogEvent {
    /// `nickname="TestPilot" playerGEID=123456789012`
    Identity { at: String, nickname: String, geid: String },
    /// First line of a session: `BackupNameAttachment=" Build(12913470) ..."`.
    SessionStart { at: String, changelist: Option<String> },
    /// `<GenerateLocationProperty> ... contract: TheCollector_X` — contract offered in this build.
    ContractOffered { at: String, debug_name: String, locations: Vec<LocationRef> },
    /// `<CreateHaulingObjectiveHandler> ... sourcename: X, ... itemGuid: <uuid>` — requirement seen at offer time.
    HaulingOrderSeen { at: String, debug_name: String, item_id: String, location_hash: Option<u64> },
    /// `Added notification "Contract Accepted: <title>: " ... MissionId: [<uuid>]`
    MissionAccepted { at: String, mission_id: String, title: String },
    /// `<CLocalMissionPhaseMarker::CreateMarker> ... missionId [..], generator name [..], contract [..], contractDefinitionId[..], objectiveId [..]`
    MissionMarker {
        at: String,
        mission_id: String,
        generator: String,
        debug_name: String,
        contract_id: Option<String>,
        objective_id: Option<String>,
        item_id: Option<String>,
        location_hash: Option<u64>,
    },
    /// `<ObjectiveUpserted> ... mission_id X - objective_id Y - state MISSION_OBJECTIVE_STATE_...`
    ObjectiveState { at: String, mission_id: String, objective_id: String, state: String, item_id: Option<String>, location_hash: Option<u64> },
    /// `<EndMission> Ending mission for player. MissionId[..] Player[..] PlayerId[..] CompletionType[..] Reason[..]`
    EndMission { at: String, mission_id: String, nickname: String, player_id: String, completion: CompletionType, reason: String },
    /// `Added notification "Received Blueprint: <name>:`
    BlueprintReceived { at: String, name: String },
    /// `<RequestLocationInventory> Player[..] requested inventory for Location[TheCollector...]`
    AtEmporium { at: String, nickname: String, location_class: String },
    /// `<RequestLocationInventory> Player[..] requested inventory for Location[<class>]`, any
    /// location (`Stanton2_Orison`, `RR_CRU_LEO`): where the player's local inventory is.
    /// An emporium line gives this one and then [`LogEvent::AtEmporium`] (see `parse_all`).
    AtLocation { at: String, location_class: String },
    /// `<Add Inventory Management Move> ... Type[Move|Store|Drop|Interaction] ... ItemClass[<class>]`
    ItemMoved { at: String, nickname: String, move_type: String, source: String, target: String, item_class: String },
    /// `<MissionShared> Received share push message: ownerId[<geid>] - missionId[<uuid>]` — another
    /// player shared their mission with us; `owner_id` is their GEID. Our own missions never
    /// produce this line, so it is what tells a shared mission from one we accepted ourselves
    /// (both show the same "Contract Accepted" notification).
    MissionShared { at: String, mission_id: String, owner_id: String },
}

impl LogEvent {
    pub fn at(&self) -> &str {
        match self {
            LogEvent::Identity { at, .. }
            | LogEvent::SessionStart { at, .. }
            | LogEvent::ContractOffered { at, .. }
            | LogEvent::HaulingOrderSeen { at, .. }
            | LogEvent::MissionAccepted { at, .. }
            | LogEvent::MissionMarker { at, .. }
            | LogEvent::ObjectiveState { at, .. }
            | LogEvent::EndMission { at, .. }
            | LogEvent::BlueprintReceived { at, .. }
            | LogEvent::AtEmporium { at, .. }
            | LogEvent::AtLocation { at, .. }
            | LogEvent::ItemMoved { at, .. }
            | LogEvent::MissionShared { at, .. } => at,
        }
    }
}

/// Longest blueprint name kept from a log line, in characters; a longer one is ignored.
pub const BLUEPRINT_NAME_MAX: usize = 200;

/// Whether `c` is a Unicode format character (general category Cf: soft hyphen, zero-width
/// spaces and joiners, bidirectional marks and overrides, the byte order mark, tag
/// characters…). The list follows Unicode 15.1; no crate is used for it.
pub fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Whether `name` (already trimmed) is kept as a blueprint name: not empty, at most
/// [`BLUEPRINT_NAME_MAX`] characters, and without control or format characters.
pub fn is_blueprint_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= BLUEPRINT_NAME_MAX && !name.chars().any(|c| c.is_control() || is_format_char(c))
}

/// Leading `<2026-10-12T18:44:32.784Z>` timestamp of a log line.
pub fn timestamp_of(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('<')?;
    let end = rest.find('>')?;
    let ts = &rest[..end];
    if ts.len() >= 20 && ts.as_bytes()[4] == b'-' && ts.ends_with('Z') { Some(ts) } else { None }
}

pub struct LineParser {
    identity: Regex,
    session_start: Regex,
    offered: Regex,
    location: Regex,
    hauling: Regex,
    accepted: Regex,
    marker: Regex,
    objective_id: Regex,
    objective_state: Regex,
    end_mission: Regex,
    blueprint: Regex,
    at_emporium: Regex,
    at_location: Regex,
    item_moved: Regex,
    shared: Regex,
}

impl Default for LineParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LineParser {
    pub fn new() -> Self {
        let rx = |s: &str| Regex::new(s).expect("valid regex");
        Self {
            identity: rx(r#"nickname="(?P<nick>[^"]+)"\s+playerGEID=(?P<geid>\d+)"#),
            session_start: rx(r#"BackupNameAttachment="\s*Build\((?P<cl>\d+)\)"#),
            offered: rx(r"<GenerateLocationProperty> Generated Locations - variablename: \S+, locations: (?P<locs>.*?)\s+contract: (?P<contract>\S+)"),
            location: rx(r"\((?P<name>[^()\[\]]+?)\s+(?:\[(?P<hash>\d+)\]\s+)?\[(?P<class>[^\]]+)\]\)"),
            hauling: rx(r"<CreateHaulingObjectiveHandler> .*?sourcename: (?P<contract>[^,]+),.*?(?:locationHash: (?P<hash>\d+))?.*?itemGuid: (?P<item>[0-9a-fA-F-]{36})"),
            accepted: rx(r#"Added notification "Contract Accepted:\s*(?P<title>.*?):?\s*"\s*\[\d+\].*?MissionId: \[(?P<mid>[0-9a-fA-F-]{36})\]"#),
            marker: rx(r"<CLocalMissionPhaseMarker::CreateMarker> Creating objective marker: missionId \[(?P<mid>[0-9a-fA-F-]{36})\], generator name \[(?P<gen>[^\]]*)\], contract \[(?P<contract>[^\]]*)\](?:, contractDefinitionId\[(?P<cdef>[0-9a-fA-F-]{36})\])?(?:, objectiveId \[(?P<obj>[^\]]*)\])?"),
            objective_id: rx(r"^d_(?P<hash>\d+)_(?P<item>[0-9a-fA-F-]{36})_\d+_\d+$"),
            objective_state: rx(r"<ObjectiveUpserted> .*?mission_id (?P<mid>[0-9a-fA-F-]{36}) - objective_id (?P<obj>\S+) - state (?P<state>MISSION_OBJECTIVE_STATE_\w+)"),
            end_mission: rx(r"<EndMission> Ending mission for player\. MissionId\[(?P<mid>[0-9a-fA-F-]{36})\] Player\[(?P<nick>[^\]]*)\] PlayerId\[(?P<pid>\d*)\] CompletionType\[(?P<ct>\w+)\] Reason\[(?P<reason>[^\]]*)\]"),
            // Anchored: the line starts with its timestamp and this notification, so the same
            // words elsewhere in a line (inside another message) never count.
            blueprint: rx(r#"^<[^<>]+> \[Notice\] <SHUDEvent_OnNotification> Added notification "Received Blueprint: (?P<name>.*?):?\s*"\s*\[\d+\]"#),
            at_emporium: rx(r"<RequestLocationInventory> Player\[(?P<nick>[^\]]+)\] requested inventory for Location\[(?P<loc>TheCollector[A-Za-z]*_Stanton\d+)\]"),
            at_location: rx(r"<RequestLocationInventory> Player\[(?P<nick>[^\]]+)\] requested inventory for Location\[(?P<loc>[A-Za-z0-9_.-]{1,128})\]"),
            item_moved: rx(r"<Add Inventory Management Move> New Request\[\d+\] Player\[(?P<nick>[^\]]+)\] Type\[(?P<ty>\w+)\] SourceInventory\[(?P<src>[^\]]*)\] TargetInventory\[(?P<dst>[^\]]*)\] ItemClass\[(?P<class>[^\]]*)\]"),
            shared: rx(r"<MissionShared> Received share push message: ownerId\[(?P<owner>\d+)\] - missionId\[(?P<mid>[0-9a-fA-F-]{36})\]"),
        }
    }

    /// Every event of a line, in order. Only an emporium's `<RequestLocationInventory>` line
    /// gives two: [`LogEvent::AtLocation`] and then [`LogEvent::AtEmporium`]; every other line
    /// gives what [`parse`](Self::parse) gives.
    pub fn parse_all(&self, line: &str) -> Vec<LogEvent> {
        match self.parse(line) {
            Some(LogEvent::AtEmporium { at, nickname, location_class }) => vec![
                LogEvent::AtLocation { at: at.clone(), location_class: location_class.clone() },
                LogEvent::AtEmporium { at, nickname, location_class },
            ],
            Some(ev) => vec![ev],
            None => Vec::new(),
        }
    }

    /// The main event of a line. An emporium's `<RequestLocationInventory>` line gives
    /// [`LogEvent::AtEmporium`] here, as it always did; [`parse_all`](Self::parse_all) also
    /// gives its [`LogEvent::AtLocation`].
    pub fn parse(&self, line: &str) -> Option<LogEvent> {
        let line = line.trim_end_matches(['\r', '\n']);
        let at = timestamp_of(line).unwrap_or("").to_string();

        if line.contains("BackupNameAttachment=") {
            let cl = self.session_start.captures(line).and_then(|c| c.name("cl")).map(|m| m.as_str().to_string());
            return Some(LogEvent::SessionStart { at, changelist: cl });
        }
        if line.contains("playerGEID=")
            && let Some(c) = self.identity.captures(line)
        {
            return Some(LogEvent::Identity { at, nickname: c["nick"].to_string(), geid: c["geid"].to_string() });
        }
        if line.contains("<CLocalMissionPhaseMarker::CreateMarker>")
            && let Some(c) = self.marker.captures(line)
        {
            let objective_id = c.name("obj").map(|m| m.as_str().to_string());
            let (item_id, location_hash) = objective_id
                .as_deref()
                .and_then(|o| self.objective_id.captures(o))
                .map(|oc| (Some(oc["item"].to_string()), oc["hash"].parse::<u64>().ok()))
                .unwrap_or((None, None));
            return Some(LogEvent::MissionMarker {
                at,
                mission_id: c["mid"].to_string(),
                generator: c["gen"].to_string(),
                debug_name: c["contract"].trim().to_string(),
                contract_id: c.name("cdef").map(|m| m.as_str().to_string()),
                objective_id,
                item_id,
                location_hash,
            });
        }
        if line.contains("<EndMission>")
            && let Some(c) = self.end_mission.captures(line)
        {
            return Some(LogEvent::EndMission {
                at,
                mission_id: c["mid"].to_string(),
                nickname: c["nick"].to_string(),
                player_id: c["pid"].to_string(),
                completion: CompletionType::parse(&c["ct"]),
                reason: c["reason"].to_string(),
            });
        }
        if line.contains("Contract Accepted:") && line.contains("<SHUDEvent_OnNotification>")
            && let Some(c) = self.accepted.captures(line)
        {
            return Some(LogEvent::MissionAccepted { at, mission_id: c["mid"].to_string(), title: c["title"].trim().to_string() });
        }
        if line.contains("Received Blueprint:") && line.contains("<SHUDEvent_OnNotification>") && !at.is_empty()
            && let Some(c) = self.blueprint.captures(line)
        {
            // A name that is empty, too long or carries control or format characters is not
            // one the game shows: the line is ignored.
            let name = c["name"].trim();
            if !is_blueprint_name(name) {
                return None;
            }
            return Some(LogEvent::BlueprintReceived { at, name: name.to_string() });
        }
        if line.contains("<ObjectiveUpserted>")
            && let Some(c) = self.objective_state.captures(line)
        {
            let obj = c["obj"].to_string();
            let (item_id, location_hash) = self
                .objective_id
                .captures(&obj)
                .map(|oc| (Some(oc["item"].to_string()), oc["hash"].parse::<u64>().ok()))
                .unwrap_or((None, None));
            return Some(LogEvent::ObjectiveState {
                at,
                mission_id: c["mid"].to_string(),
                objective_id: obj,
                state: c["state"].to_string(),
                item_id,
                location_hash,
            });
        }
        if line.contains("<GenerateLocationProperty>") && line.contains("contract: TheColl")
            && let Some(c) = self.offered.captures(line)
        {
            let locations = self
                .location
                .captures_iter(&c["locs"])
                .map(|l| LocationRef {
                    name: l["name"].trim().to_string(),
                    hash: l.name("hash").and_then(|h| h.as_str().parse().ok()),
                    class_name: l["class"].to_string(),
                })
                .collect();
            return Some(LogEvent::ContractOffered { at, debug_name: c["contract"].trim().to_string(), locations });
        }
        if line.contains("<CreateHaulingObjectiveHandler>") && line.contains("TheColl")
            && let Some(c) = self.hauling.captures(line)
        {
            return Some(LogEvent::HaulingOrderSeen {
                at,
                debug_name: c["contract"].trim().to_string(),
                item_id: c["item"].to_string(),
                location_hash: c.name("hash").and_then(|h| h.as_str().parse().ok()),
            });
        }
        if line.contains("<RequestLocationInventory>") {
            if let Some(c) = self.at_emporium.captures(line) {
                return Some(LogEvent::AtEmporium { at, nickname: c["nick"].to_string(), location_class: c["loc"].to_string() });
            }
            // The game logs this placeholder when it has no location to name (e.g. in space).
            if let Some(c) = self.at_location.captures(line).filter(|c| &c["loc"] != "INVALID_LOCATION_ID") {
                return Some(LogEvent::AtLocation { at, location_class: c["loc"].to_string() });
            }
        }
        if line.contains("<MissionShared>")
            && let Some(c) = self.shared.captures(line)
        {
            return Some(LogEvent::MissionShared { at, mission_id: c["mid"].to_string(), owner_id: c["owner"].to_string() });
        }
        if line.contains("<Add Inventory Management Move>")
            && let Some(c) = self.item_moved.captures(line)
        {
            let class = c["class"].trim();
            if class.is_empty() {
                return None;
            }
            return Some(LogEvent::ItemMoved {
                at,
                nickname: c["nick"].to_string(),
                move_type: c["ty"].to_string(),
                source: c["src"].to_string(),
                target: c["dst"].to_string(),
                item_class: class.to_string(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "<2026-10-12T18:44:32.784Z> [Notice] <CLocalMissionPhaseMarker::CreateMarker> Creating objective marker: missionId [a0000000-0000-4000-8000-000000000001], generator name [TheCollector], contract [TheCollector_Favours_PolarisParts], contractDefinitionId[82a0d506-def3-4ab5-b0e0-8833c7102f34], objectiveId [dropoff_f94ac2dc-0e72-411a-8985-63d7ed0577f1_0], markerEntityId [39014], zoneHostId [874535716059], position [x: 14333387186.508734, y: 2001161588.504044, z: 0.000000] [Team_MissionFeatures][Missions]";
    const ACCEPTED: &str = "<2026-10-12T17:50:15.036Z> [Notice] <SHUDEvent_OnNotification> Added notification \"Contract Accepted:  RSI Disc. Month: Important Supply Haul: \" [25] to queue. New queue size: 2, MissionId: [a0000000-0000-4000-8000-000000000002], ObjectiveId: [] [Team_CoreGameplayFeatures][Missions][Comms]";
    const END: &str = "<2026-10-12T21:46:30.460Z> [Notice] <EndMission> Ending mission for player. MissionId[a0000000-0000-4000-8000-000000000003] Player[TestPilot] PlayerId[123456789012] CompletionType[Complete] Reason[Mission Ended] [Team_MissionFeatures][Missions]";
    const OFFERED: &str = "<2026-10-13T21:50:41.490Z> [Notice] <GenerateLocationProperty> Generated Locations - variablename: DropoffLocation_BP[Destination], locations: (Wikelo Emporium Dasi Station [1231535936] [TheCollectorsAsteriod_Stanton1])(Wikelo Emporium Kinga Station [3168785171] [TheCollectorsAsteriod_Stanton4])(Wikelo Emporium Selo Station [1615454559] [TheCollectorsAsteriod_Stanton2]) contract: TheCollector_RedHunterArmour [Team_MissionFeatures][Missions]";
    const OFFERED_OLD: &str = "<2025-08-02T14:41:12.000Z> [Notice] <GenerateLocationProperty> Generated Locations - variablename: DropoffLocation_BP[Destination], locations: (Wikelo Emporium Dasi Station [TheCollectorsAsteriod_Stanton1]) contract: TheCollector_Vehicles_Peregrine  [Team_MissionFeatures][Missions]";
    const AT_EMP: &str = "<2026-10-12T18:29:34.437Z> [Notice] <RequestLocationInventory> Player[TestPilot] requested inventory for Location[TheCollectorAsteroid_Stanton2] [Team_CoreGameplayFeatures][Inventory]";
    const MOVED: &str = "<2026-10-13T21:37:42.896Z> [Notice] <Add Inventory Management Move> New Request[8] Player[TestPilot] Type[Move] SourceInventory[900000000001:Container:0] TargetInventory[123456789012:Location:1752411604] ItemClass[behr_optics_rdot_x1_s1] StoredEntity[NULL] LocallyDetached[No] LocalAttached[NULL, ] PendingMoves[1, ] Caller[CSCLocalPlayerPersonalThoughtComponent::OnInventoryMoveItemOntoUnoccupiedPosition] [Team_CoreGameplayFeatures][Inventory]";
    const IDENT: &str = "<2026-10-13T21:36:19.000Z> [Notice] <AccountLoginCharacterStatus_Character> Character: createdAt 1 - updatedAt 2 - geid 123456789012 - accountId 1 - name TestPilot - state STATE_CURRENT nickname=\"TestPilot\" playerGEID=123456789012 [Team_GameServices][Login]";
    const HEADER: &str = "<2026-10-13T21:34:02.356Z> BackupNameAttachment=\" Build(12913470) 13 Oct 26 (18 33 55)\"  -- used by backup system";
    const BLUEPRINT: &str = "<2026-06-01T12:00:00.000Z> [Notice] <SHUDEvent_OnNotification> Added notification \"Received Blueprint: Monde Arms Hemlock Camo: \" [3] to queue. New queue size: 1, MissionId: [00000000-0000-0000-0000-000000000000], ObjectiveId: [] [Team_CoreGameplayFeatures][Missions][Comms]";
    const SHARED: &str = "<2026-10-12T17:50:09.311Z> [Notice] <MissionShared> Received share push message: ownerId[210987654321] - missionId[a0000000-0000-4000-8000-000000000002] [Team_GameServices][Missions]";

    #[test]
    fn parses_marker() {
        let p = LineParser::new();
        match p.parse(MARKER).unwrap() {
            LogEvent::MissionMarker { at, mission_id, generator, debug_name, contract_id, objective_id, .. } => {
                assert_eq!(at, "2026-10-12T18:44:32.784Z");
                assert_eq!(mission_id, "a0000000-0000-4000-8000-000000000001");
                assert_eq!(generator, "TheCollector");
                assert_eq!(debug_name, "TheCollector_Favours_PolarisParts");
                assert_eq!(contract_id.as_deref(), Some("82a0d506-def3-4ab5-b0e0-8833c7102f34"));
                assert!(objective_id.unwrap().starts_with("dropoff_"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_accepted_end_blueprint() {
        let p = LineParser::new();
        assert_eq!(
            p.parse(ACCEPTED).unwrap(),
            LogEvent::MissionAccepted {
                at: "2026-10-12T17:50:15.036Z".into(),
                mission_id: "a0000000-0000-4000-8000-000000000002".into(),
                title: "RSI Disc. Month: Important Supply Haul".into(),
            }
        );
        match p.parse(END).unwrap() {
            LogEvent::EndMission { mission_id, nickname, completion, reason, .. } => {
                assert_eq!(mission_id, "a0000000-0000-4000-8000-000000000003");
                assert_eq!(nickname, "TestPilot");
                assert_eq!(completion, CompletionType::Complete);
                assert_eq!(reason, "Mission Ended");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            p.parse(BLUEPRINT).unwrap(),
            LogEvent::BlueprintReceived { at: "2026-06-01T12:00:00.000Z".into(), name: "Monde Arms Hemlock Camo".into() }
        );
    }

    #[test]
    fn a_blueprint_line_must_start_with_its_timestamp_and_the_notification() {
        let p = LineParser::new();
        let rest = BLUEPRINT.split_once("> ").unwrap().1;
        // The same words later in another line, or without the leading timestamp: no event.
        assert!(p.parse(rest).is_none(), "no timestamp");
        assert!(p.parse(&format!("  {BLUEPRINT}")).is_none(), "not at the start");
        assert!(p.parse(&format!("<2026-06-01T12:00:00.000Z> [Notice] <Other> said: {rest}")).is_none(), "inside another message");
        assert!(p.parse(&BLUEPRINT.replace("[Notice]", "[Warning]")).is_none());
        assert!(p.parse(&BLUEPRINT.replace("<2026-06-01T12:00:00.000Z>", "<not a time>")).is_none(), "a timestamp that is not one");
        // A trailing CR (a Windows line end) is still the same line.
        assert!(matches!(p.parse(&format!("{BLUEPRINT}\r\n")), Some(LogEvent::BlueprintReceived { .. })));
    }

    #[test]
    fn blueprint_names_are_bounded_and_plain() {
        let p = LineParser::new();
        let with = |name: &str| BLUEPRINT.replace("Monde Arms Hemlock Camo", name);
        let name_of = |line: &str| match p.parse(line) {
            Some(LogEvent::BlueprintReceived { name, .. }) => Some(name),
            Some(other) => panic!("unexpected {other:?}"),
            None => None,
        };
        let longest = "a".repeat(BLUEPRINT_NAME_MAX);
        assert_eq!(name_of(&with(&longest)), Some(longest.clone()), "200 characters is fine");
        // Counted in characters, not bytes.
        let accented = "é".repeat(BLUEPRINT_NAME_MAX);
        assert_eq!(name_of(&with(&accented)), Some(accented.clone()));
        assert_eq!(name_of(&with(&"a".repeat(BLUEPRINT_NAME_MAX + 1))), None, "too long");
        assert_eq!(name_of(&with("   ")), None, "empty after trimming");
        assert_eq!(name_of(&with("  Padded  ")), Some("Padded".into()));
        for bad in ["Evil\u{202E}gpj", "Zero\u{200B}Width", "Bell\u{7}", "Tab\tName", "Esc\u{1b}[31m", "Bom\u{FEFF}", "Soft\u{AD}Hyphen", "Tag\u{E0041}"] {
            assert_eq!(name_of(&with(bad)), None, "{bad:?}");
        }
        assert!(is_format_char('\u{202E}') && is_format_char('\u{200B}') && !is_format_char('a') && !is_format_char('\u{FFFD}'));
    }

    #[test]
    fn parses_mission_shared() {
        let p = LineParser::new();
        assert_eq!(
            p.parse(SHARED).unwrap(),
            LogEvent::MissionShared {
                at: "2026-10-12T17:50:09.311Z".into(),
                mission_id: "a0000000-0000-4000-8000-000000000002".into(),
                owner_id: "210987654321".into(),
            }
        );
        // The companion line carries nothing.
        assert!(p.parse("<2026-10-12T17:50:09.310Z> [Notice] <MissionSharedOnMessaged> onMessage [Team_GameServices][Missions]").is_none());
    }

    #[test]
    fn parses_offered_with_and_without_hashes() {
        let p = LineParser::new();
        match p.parse(OFFERED).unwrap() {
            LogEvent::ContractOffered { debug_name, locations, .. } => {
                assert_eq!(debug_name, "TheCollector_RedHunterArmour");
                assert_eq!(locations.len(), 3);
                assert_eq!(locations[0].name, "Wikelo Emporium Dasi Station");
                assert_eq!(locations[0].hash, Some(1_231_535_936));
                assert_eq!(locations[0].class_name, "TheCollectorsAsteriod_Stanton1");
            }
            other => panic!("unexpected {other:?}"),
        }
        match p.parse(OFFERED_OLD).unwrap() {
            LogEvent::ContractOffered { debug_name, locations, .. } => {
                assert_eq!(debug_name, "TheCollector_Vehicles_Peregrine");
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].hash, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    const AT_LOC: &str = "<2026-11-15T14:19:28.590Z> [Notice] <RequestLocationInventory> Player[TestPilot] requested inventory for Location[RR_JP_NyxCastra] [Team_CoreGameplayFeatures][Inventory]";

    #[test]
    fn parses_any_location() {
        let p = LineParser::new();
        let ev = LogEvent::AtLocation { at: "2026-11-15T14:19:28.590Z".into(), location_class: "RR_JP_NyxCastra".into() };
        assert_eq!(p.parse(AT_LOC), Some(ev.clone()));
        assert_eq!(p.parse_all(AT_LOC), vec![ev]);
        let city = AT_LOC.replace("RR_JP_NyxCastra", "Pyro1_ASD_Monorail_LazarusTransportHub_Phoenix_1A");
        assert!(matches!(p.parse(&city), Some(LogEvent::AtLocation { location_class, .. }) if location_class == "Pyro1_ASD_Monorail_LazarusTransportHub_Phoenix_1A"));
        // Nothing usable inside the brackets: no event.
        assert!(p.parse(&AT_LOC.replace("RR_JP_NyxCastra", "")).is_none());
        assert!(p.parse(&AT_LOC.replace("RR_JP_NyxCastra", "a b")).is_none());
    }

    #[test]
    fn emporium_line_gives_location_then_emporium() {
        let p = LineParser::new();
        let at = "2026-10-12T18:29:34.437Z".to_string();
        let class = "TheCollectorAsteroid_Stanton2".to_string();
        assert_eq!(
            p.parse_all(AT_EMP),
            vec![
                LogEvent::AtLocation { at: at.clone(), location_class: class.clone() },
                LogEvent::AtEmporium { at: at.clone(), nickname: "TestPilot".into(), location_class: class.clone() },
            ]
        );
        assert_eq!(p.parse(AT_EMP), Some(LogEvent::AtEmporium { at, nickname: "TestPilot".into(), location_class: class }), "parse is unchanged");
        assert!(p.parse_all("<2026-10-13T21:34:02.358Z> [Notice] <StatObjLoad 0x800 Format> whatever").is_empty());
        assert_eq!(p.parse_all(MOVED).len(), 1);
    }

    #[test]
    fn location_events_serialize_with_their_kind() {
        let ev = LogEvent::AtLocation { at: "t".into(), location_class: "Stanton2_Orison".into() };
        assert_eq!(serde_json::to_string(&ev).unwrap(), r#"{"kind":"atLocation","at":"t","location_class":"Stanton2_Orison"}"#);
    }

    #[test]
    fn parses_misc() {
        let p = LineParser::new();
        assert_eq!(
            p.parse(AT_EMP).unwrap(),
            LogEvent::AtEmporium {
                at: "2026-10-12T18:29:34.437Z".into(),
                nickname: "TestPilot".into(),
                location_class: "TheCollectorAsteroid_Stanton2".into()
            }
        );
        match p.parse(MOVED).unwrap() {
            LogEvent::ItemMoved { move_type, item_class, .. } => {
                assert_eq!(move_type, "Move");
                assert_eq!(item_class, "behr_optics_rdot_x1_s1");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            p.parse(IDENT).unwrap(),
            LogEvent::Identity { at: "2026-10-13T21:36:19.000Z".into(), nickname: "TestPilot".into(), geid: "123456789012".into() }
        );
        assert_eq!(p.parse(HEADER).unwrap(), LogEvent::SessionStart { at: "2026-10-13T21:34:02.356Z".into(), changelist: Some("12913470".into()) });
        assert!(p.parse("<2026-10-13T21:34:02.358Z> [Notice] <StatObjLoad 0x800 Format> whatever").is_none());
    }
}

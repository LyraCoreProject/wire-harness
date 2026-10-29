use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::compare::EquivalenceRules;
use crate::decode::{decode, Packet};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u32,
    pub id: String,
    pub behavior: String,
    pub rules: EquivalenceRules,
    pub opening: Opening,
    #[serde(default)]
    pub fixtures: Vec<FixtureVerb>,
    #[serde(default)]
    pub characters: Vec<ScenarioCharacter>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioCharacter {
    pub id: String,
    #[serde(default = "initially_connected")]
    pub initially_connected: bool,
    pub fixtures: Vec<FixtureVerb>,
}

fn initially_connected() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Opening {
    pub window_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureVerb {
    Character {
        name: String,
        race: String,
        class: String,
        level: u8,
        at: Position,
    },
    Money {
        copper: u32,
    },
    KnownFlightNode {
        node_id: u32,
    },
    Creature {
        id: String,
        entry: u32,
        offset: f32,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub map: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
}

impl Position {
    pub fn offset(&self, distance: f32) -> Self {
        let (sin, cos) = self.orientation.sin_cos();
        Self {
            x: self.x + distance * cos,
            y: self.y + distance * sin,
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub send: Option<ClientPacket>,
    #[serde(default)]
    pub action: Option<SessionAction>,
    pub checkpoint: Checkpoint,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionAction {
    Connect,
    Disconnect,
    Reconnect,
    Walk { to: Position, speed: f32 },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "opcode", deny_unknown_fields)]
pub enum ClientPacket {
    #[serde(rename = "CMSG_PING")]
    Ping { sequence: u32, latency_ms: u32 },
    #[serde(rename = "CMSG_NAME_QUERY")]
    NameQuery { target: Target },
    #[serde(rename = "CMSG_TAXINODE_STATUS_QUERY")]
    TaxiNodeStatusQuery { target: Target },
    #[serde(rename = "CMSG_TAXIQUERYAVAILABLENODES")]
    TaxiQueryAvailableNodes { target: Target },
    #[serde(rename = "CMSG_ACTIVATETAXI")]
    ActivateTaxi {
        target: Target,
        source: u32,
        destination: u32,
    },
    #[serde(rename = "CMSG_BANKER_ACTIVATE")]
    BankerActivate { target: Target },
    #[serde(rename = "CMSG_BUY_BANK_SLOT")]
    BuyBankSlot { target: Target },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Character,
    NamedCharacter(String),
    Fixture(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    #[serde(default)]
    pub expect: Vec<Expectation>,
    pub window_ms: u64,
    #[serde(default)]
    pub replies: Vec<u16>,
    #[serde(default)]
    pub broadcasts: Vec<Broadcast>,
    #[serde(default)]
    pub characters: Vec<CharacterCheckpoint>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterCheckpoint {
    pub character: String,
    #[serde(default)]
    pub replies: Vec<u16>,
    #[serde(default)]
    pub broadcasts: Vec<Broadcast>,
    #[serde(default)]
    pub expect: Vec<Expectation>,
    #[serde(default)]
    pub creates: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub opcode: u16,
    pub path: Vec<String>,
    pub value: crate::trace::RecordedField,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Broadcast {
    pub opcode: u16,
    pub stream: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Stream {
    Opening,
    Reply(String),
    Broadcast(String),
    Unexpected,
    Malformed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub character: Option<String>,
    pub received_after_ms: Option<u64>,
    pub checkpoint: String,
    pub stream: Stream,
    pub packet: Packet,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Self> {
        let scenario: Self = toml::from_str(text).context("scenario TOML")?;
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.schema_version, 3 | 4),
            "unsupported scenario schema"
        );
        if self.schema_version == 4 {
            return self.validate_multi();
        }
        ensure!(
            self.characters.is_empty(),
            "schema 3 has no named Characters"
        );
        ensure!(
            !self
                .rules
                .character_scope
                .as_ref()
                .is_some_and(|scope| scope.all_characters || scope.creates_only),
            "schema 3 scope cannot name multiple Characters"
        );
        ensure!(
            self.rule_characters().next().is_none(),
            "schema 3 rules cannot name a Character"
        );
        ensure!(
            identifier(&self.id) && identifier(&self.behavior),
            "invalid scenario or Behavior id"
        );
        ensure!(
            !self.fixtures.is_empty()
                && self.fixtures.len() <= 256
                && self
                    .fixtures
                    .iter()
                    .filter(|f| matches!(f, FixtureVerb::Character { .. }))
                    .count()
                    == 1,
            "exactly one Character fixture is required"
        );
        self.rules.validate()?;
        ensure!(
            (1..=30_000).contains(&self.opening.window_ms),
            "opening window must be 1 through 30000 ms"
        );
        let (fixture_ids, nodes) = validate_fixtures(&self.fixtures)?;
        ensure!(
            !self.steps.is_empty() && self.steps.len() <= 256,
            "scenario needs 1 through 256 steps"
        );
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            ensure!(
                step.character.is_none() && step.action.is_none(),
                "schema 3 step must send a packet"
            );
            let send = step.send.as_ref().context("step needs a packet")?;
            ensure!(
                step.checkpoint.characters.is_empty(),
                "schema 3 checkpoint has no named Characters"
            );
            ensure!(
                identifier(&step.id) && step.id != "opening" && ids.insert(&step.id),
                "invalid or duplicate step id"
            );
            let checkpoint = &step.checkpoint;
            ensure!(
                (1..=180_000).contains(&checkpoint.window_ms),
                "checkpoint window must be 1 through 180000 ms"
            );
            ensure!(
                checkpoint.replies.len() <= 256,
                "checkpoint needs bounded replies"
            );
            if let Some(Target::Fixture(id)) = send.target() {
                ensure!(
                    fixture_ids.contains(id),
                    "request names an unknown creature fixture"
                );
            }
            ensure!(
                !matches!(send.target(), Some(Target::NamedCharacter(_))),
                "schema 3 cannot target a named Character"
            );
            if let ClientPacket::ActivateTaxi {
                source,
                destination,
                ..
            } = send
            {
                ensure!(
                    source != destination && nodes.contains(source) && nodes.contains(destination),
                    "taxi endpoints must be distinct known-node fixtures"
                );
            }
            let expected = send.reply_opcode();
            if let Some(expected) = expected {
                ensure!(
                    checkpoint.replies.contains(&expected),
                    "checkpoint omits its request's reply opcode"
                );
            } else {
                ensure!(
                    !checkpoint.expect.is_empty(),
                    "request without a success reply needs a checkpoint assertion"
                );
            }
            ensure!(
                checkpoint.expect.len() <= 256,
                "too many checkpoint assertions"
            );
            for assertion in &checkpoint.expect {
                assertion.value.validate(0)?;
                ensure!(
                    assertion.count > 0
                        && assertion.count <= 4096
                        && !assertion.path.is_empty()
                        && assertion.path.iter().all(|p| identifier(p)),
                    "invalid checkpoint assertion"
                );
                ensure!(
                    checkpoint.stream(&step.id, assertion.opcode) != Stream::Unexpected,
                    "assertion opcode must be declared"
                );
            }
            let mut opcodes = BTreeSet::new();
            for broadcast in &checkpoint.broadcasts {
                ensure!(identifier(&broadcast.stream), "invalid broadcast stream id");
                ensure!(
                    !checkpoint.replies.contains(&broadcast.opcode)
                        && opcodes.insert(broadcast.opcode),
                    "ambiguous broadcast opcode"
                );
            }
        }
        for rule in &self.rules.unexpected_packets {
            let step = self
                .steps
                .iter()
                .find(|step| step.id == rule.checkpoint)
                .context("unexpected packet rule names an unknown checkpoint")?;
            ensure!(
                step.checkpoint.stream(&step.id, rule.opcode) == Stream::Unexpected,
                "unexpected packet rule cannot excuse a declared reply or broadcast"
            );
        }
        Ok(())
    }

    fn validate_multi(&self) -> Result<()> {
        ensure!(
            identifier(&self.id) && identifier(&self.behavior),
            "invalid scenario or Behavior id"
        );
        ensure!(
            self.fixtures.is_empty(),
            "schema 4 uses named Character fixtures"
        );
        ensure!(
            (2..=16).contains(&self.characters.len()),
            "schema 4 needs 2 through 16 Characters"
        );
        ensure!(
            (1..=30_000).contains(&self.opening.window_ms),
            "invalid opening window"
        );
        self.rules.validate()?;
        if let Some(scope) = &self.rules.character_scope {
            ensure!(
                !scope.creates_only || scope.all_characters,
                "create-only scope needs all Characters"
            );
        }
        let mut ids = BTreeSet::new();
        for character in &self.characters {
            ensure!(
                identifier(&character.id) && ids.insert(character.id.clone()),
                "invalid or duplicate Character id"
            );
            let FixtureVerb::Character { name, .. } = character
                .fixtures
                .first()
                .context("Character fixture is required")?
            else {
                anyhow::bail!("named Character must start with a Character fixture")
            };
            ensure!(self.characters.iter().filter(|c| matches!(c.fixtures.first(), Some(FixtureVerb::Character { name: other, .. }) if other == name)).count() == 1, "duplicate Character name");
            validate_fixtures(&character.fixtures)?;
        }
        ensure!(
            self.rule_characters().all(|id| ids.contains(id)),
            "rule names an unknown Character"
        );
        ensure!(
            !self.steps.is_empty() && self.steps.len() <= 256,
            "scenario needs 1 through 256 steps"
        );
        let mut step_ids = BTreeSet::new();
        let mut connected: BTreeMap<_, _> = self
            .characters
            .iter()
            .map(|character| (character.id.as_str(), character.initially_connected))
            .collect();
        let mut positions: BTreeMap<_, _> = self
            .characters
            .iter()
            .map(|character| {
                let FixtureVerb::Character { at, .. } = &character.fixtures[0] else {
                    unreachable!()
                };
                (character.id.as_str(), at.clone())
            })
            .collect();
        ensure!(
            connected.values().any(|state| *state),
            "opening needs a connected Character"
        );
        for step in &self.steps {
            ensure!(
                identifier(&step.id) && step.id != "opening" && step_ids.insert(&step.id),
                "invalid or duplicate step id"
            );
            let actor = step
                .character
                .as_deref()
                .context("step needs a Character")?;
            ensure!(ids.contains(actor), "step names an unknown Character");
            ensure!(
                step.send.is_some() != step.action.is_some(),
                "step needs one packet or action"
            );
            let active = *connected.get(actor).unwrap();
            match &step.action {
                Some(SessionAction::Connect) => {
                    ensure!(!active, "connect requires a disconnected Character")
                }
                Some(SessionAction::Disconnect | SessionAction::Reconnect)
                | Some(SessionAction::Walk { .. })
                | None => ensure!(active, "step actor is disconnected"),
            }
            if matches!(step.action.as_ref(), Some(SessionAction::Connect)) {
                connected.insert(actor, true);
            }
            if matches!(step.action.as_ref(), Some(SessionAction::Disconnect)) {
                connected.insert(actor, false);
            }
            ensure!(
                connected.values().any(|state| *state),
                "checkpoint needs a connected Character"
            );
            ensure!(
                (1..=180_000).contains(&step.checkpoint.window_ms),
                "invalid checkpoint window"
            );
            ensure!(
                step.checkpoint.replies.is_empty()
                    && step.checkpoint.broadcasts.is_empty()
                    && step.checkpoint.expect.is_empty(),
                "schema 4 uses per-Character checkpoints"
            );
            ensure!(
                step.checkpoint.characters.len() == self.characters.len(),
                "checkpoint must declare every Character"
            );
            let mut observed = BTreeSet::new();
            for capture in &step.checkpoint.characters {
                ensure!(
                    ids.contains(capture.character.as_str()) && observed.insert(&capture.character),
                    "invalid or duplicate checkpoint Character"
                );
                ensure!(
                    capture.replies.len() <= 256
                        && capture.broadcasts.len() <= 256
                        && capture.expect.len() <= 256
                        && capture.creates.len() <= 16,
                    "checkpoint declaration exceeds bounds"
                );
                let mut creates = BTreeSet::new();
                ensure!(
                    capture
                        .creates
                        .iter()
                        .all(|id| ids.contains(id.as_str()) && creates.insert(id)),
                    "invalid or duplicate create target"
                );
                ensure!(
                    capture.creates.is_empty()
                        || capture
                            .broadcasts
                            .iter()
                            .any(|broadcast| broadcast.opcode == 0x00a9),
                    "create assertion needs an object update stream"
                );
                let mut opcodes = BTreeSet::new();
                for broadcast in &capture.broadcasts {
                    ensure!(
                        identifier(&broadcast.stream)
                            && !capture.replies.contains(&broadcast.opcode)
                            && opcodes.insert(broadcast.opcode),
                        "ambiguous broadcast opcode"
                    );
                }
                for assertion in &capture.expect {
                    assertion.value.validate(0)?;
                    ensure!(
                        assertion.count > 0
                            && assertion.count <= 4096
                            && !assertion.path.is_empty()
                            && assertion.path.iter().all(|p| identifier(p))
                            && capture.stream(&step.id, assertion.opcode) != Stream::Unexpected,
                        "invalid checkpoint assertion"
                    );
                }
            }
            if let Some(send) = &step.send {
                if let Some(Target::NamedCharacter(id)) = send.target() {
                    ensure!(ids.contains(id), "request names an unknown Character");
                }
                if let Some(Target::Fixture(id)) = send.target() {
                    ensure!(
                        self.characters
                            .iter()
                            .find(|c| c.id == actor)
                            .unwrap()
                            .fixtures
                            .iter()
                            .any(
                                |f| matches!(f, FixtureVerb::Creature { id: own, .. } if own == id)
                            ),
                        "request names an unknown creature fixture"
                    );
                }
                if let Some(reply) = send.reply_opcode() {
                    ensure!(
                        step.checkpoint
                            .for_character(actor)
                            .unwrap()
                            .replies
                            .contains(&reply),
                        "actor checkpoint omits request reply"
                    );
                } else {
                    ensure!(
                        !step
                            .checkpoint
                            .for_character(actor)
                            .unwrap()
                            .expect
                            .is_empty(),
                        "request without a success reply needs a checkpoint assertion"
                    );
                }
                if let ClientPacket::ActivateTaxi {
                    source,
                    destination,
                    ..
                } = send
                {
                    let fixtures = &self
                        .characters
                        .iter()
                        .find(|character| character.id == actor)
                        .unwrap()
                        .fixtures;
                    let (_, nodes) = validate_fixtures(fixtures)?;
                    ensure!(
                        source != destination
                            && nodes.contains(source)
                            && nodes.contains(destination),
                        "taxi endpoints must be distinct known-node fixtures"
                    );
                }
            }
            if let Some(SessionAction::Walk { to, speed }) = &step.action {
                ensure!(
                    [to.x, to.y, to.z, to.orientation, *speed]
                        .iter()
                        .all(|v| v.is_finite())
                        && (0.1..=7.0).contains(speed),
                    "invalid walk"
                );
                let from = positions.get(actor).unwrap();
                ensure!(from.map == to.map, "walk cannot change maps");
                let distance =
                    ((to.x - from.x).powi(2) + (to.y - from.y).powi(2) + (to.z - from.z).powi(2))
                        .sqrt();
                ensure!(
                    distance > 0.0
                        && (distance / speed * 1000.0).ceil() < step.checkpoint.window_ms as f32,
                    "walk exceeds checkpoint window"
                );
                positions.insert(actor, to.clone());
            }
        }
        for rule in &self.rules.unexpected_packets {
            let step = self
                .steps
                .iter()
                .find(|s| s.id == rule.checkpoint)
                .context("unexpected packet rule names an unknown checkpoint")?;
            ensure!(
                rule.character
                    .as_ref()
                    .is_none_or(|id| ids.contains(id.as_str())),
                "unexpected packet rule names an unknown Character"
            );
            ensure!(
                step.checkpoint.characters.iter().any(|capture| rule
                    .character
                    .as_ref()
                    .is_none_or(|id| *id == capture.character)
                    && capture.stream(&step.id, rule.opcode) == Stream::Unexpected),
                "unexpected packet rule cannot excuse only declared packets"
            );
        }
        Ok(())
    }

    fn rule_characters(&self) -> impl Iterator<Item = &String> {
        self.rules
            .random
            .iter()
            .filter_map(|rule| rule.character.as_ref())
            .chain(
                self.rules
                    .identities
                    .iter()
                    .filter_map(|rule| rule.character.as_ref()),
            )
            .chain(
                self.rules
                    .clocks
                    .iter()
                    .filter_map(|rule| rule.character.as_ref()),
            )
            .chain(
                self.rules
                    .data_fields
                    .iter()
                    .filter_map(|rule| rule.character.as_ref()),
            )
            .chain(
                self.rules
                    .packet_timing
                    .iter()
                    .filter_map(|rule| rule.character.as_ref()),
            )
            .chain(
                self.rules
                    .unexpected_packets
                    .iter()
                    .filter_map(|rule| rule.character.as_ref()),
            )
    }
}

fn validate_fixtures(fixtures: &[FixtureVerb]) -> Result<(BTreeSet<String>, BTreeSet<u32>)> {
    ensure!(
        !fixtures.is_empty()
            && fixtures.len() <= 256
            && fixtures
                .iter()
                .filter(|f| matches!(f, FixtureVerb::Character { .. }))
                .count()
                == 1,
        "exactly one Character fixture is required"
    );
    let FixtureVerb::Character {
        name,
        race,
        class,
        level,
        at,
    } = &fixtures[0]
    else {
        anyhow::bail!("Character must be the first fixture")
    };
    ensure!(
        (2..=12).contains(&name.len())
            && name.bytes().all(|b| b.is_ascii_alphabetic())
            && name.as_bytes()[0].is_ascii_uppercase()
            && name.as_bytes()[1..].iter().all(u8::is_ascii_lowercase),
        "fixture name must have 2 through 12 letters, with only the first capitalized"
    );
    ensure!(
        matches!(
            race.as_str(),
            "human" | "orc" | "dwarf" | "nightelf" | "undead" | "tauren" | "gnome" | "troll"
        ),
        "invalid Character race"
    );
    ensure!(
        matches!(
            class.as_str(),
            "warrior"
                | "paladin"
                | "hunter"
                | "rogue"
                | "priest"
                | "shaman"
                | "mage"
                | "warlock"
                | "druid"
        ),
        "invalid Character class"
    );
    ensure!(
        (1..=60).contains(level),
        "Character level must be 1 through 60"
    );
    ensure!(
        [at.x, at.y, at.z, at.orientation]
            .iter()
            .all(|v| v.is_finite()),
        "fixture coordinates must be finite"
    );
    let mut fixture_ids = BTreeSet::new();
    let mut nodes = BTreeSet::new();
    let mut money = false;
    for fixture in &fixtures[1..] {
        match fixture {
            FixtureVerb::Character { .. } => anyhow::bail!("duplicate Character fixture"),
            FixtureVerb::Money { copper } => {
                ensure!(
                    !money && *copper <= 0x7fff_ffff,
                    "duplicate or overflowing money fixture"
                );
                money = true;
            }
            FixtureVerb::KnownFlightNode { node_id } => {
                ensure!(
                    (1..=256).contains(node_id) && nodes.insert(*node_id),
                    "invalid or duplicate flight node"
                );
            }
            FixtureVerb::Creature { id, entry, offset } => {
                ensure!(
                    identifier(id) && fixture_ids.insert(id.clone()),
                    "invalid or duplicate creature fixture id"
                );
                ensure!(
                    *entry != 0 && offset.is_finite() && offset.abs() <= 100.0,
                    "invalid creature fixture"
                );
            }
        }
    }
    Ok((fixture_ids, nodes))
}

impl ClientPacket {
    fn reply_opcode(&self) -> Option<u16> {
        match self {
            Self::Ping { .. } => Some(0x01dd),
            Self::NameQuery { .. } => Some(0x0051),
            Self::TaxiNodeStatusQuery { .. } => Some(0x01ab),
            Self::TaxiQueryAvailableNodes { .. } => Some(0x01a9),
            Self::ActivateTaxi { .. } => Some(0x01ae),
            Self::BankerActivate { .. } => Some(0x01b8),
            Self::BuyBankSlot { .. } => None,
        }
    }
    fn target(&self) -> Option<&Target> {
        match self {
            Self::Ping { .. } => None,
            Self::NameQuery { target }
            | Self::TaxiNodeStatusQuery { target }
            | Self::TaxiQueryAvailableNodes { target }
            | Self::BankerActivate { target }
            | Self::BuyBankSlot { target }
            | Self::ActivateTaxi { target, .. } => Some(target),
        }
    }

    pub fn encode(
        &self,
        character_guid: u64,
        fixtures: &BTreeMap<String, u64>,
    ) -> Result<(u32, Vec<u8>)> {
        self.encode_with(character_guid, fixtures, &BTreeMap::new())
    }

    pub fn encode_with(
        &self,
        character_guid: u64,
        fixtures: &BTreeMap<String, u64>,
        characters: &BTreeMap<String, u64>,
    ) -> Result<(u32, Vec<u8>)> {
        let guid = match self.target() {
            None => 0,
            Some(Target::Character) => character_guid,
            Some(Target::NamedCharacter(id)) => {
                *characters.get(id).context("unbound Character target")?
            }
            Some(Target::Fixture(id)) => *fixtures.get(id).context("unbound creature fixture")?,
        };
        if self.target().is_some() {
            ensure!(guid != 0, "target GUID must be nonzero");
        }
        Ok(match self {
            Self::Ping {
                sequence,
                latency_ms,
            } => {
                let mut body = sequence.to_le_bytes().to_vec();
                body.extend(latency_ms.to_le_bytes());
                (0x01dc, body)
            }
            Self::NameQuery { .. } => (0x0050, guid.to_le_bytes().to_vec()),
            Self::TaxiNodeStatusQuery { .. } => (0x01aa, guid.to_le_bytes().to_vec()),
            Self::TaxiQueryAvailableNodes { .. } => (0x01ac, guid.to_le_bytes().to_vec()),
            Self::BankerActivate { .. } => (0x01b7, guid.to_le_bytes().to_vec()),
            Self::BuyBankSlot { .. } => (0x01b9, guid.to_le_bytes().to_vec()),
            Self::ActivateTaxi {
                source,
                destination,
                ..
            } => {
                let mut body = guid.to_le_bytes().to_vec();
                body.extend(source.to_le_bytes());
                body.extend(destination.to_le_bytes());
                (0x01ad, body)
            }
        })
    }
}

impl Observation {
    /// Opening packets retain decoded fields where supported; unsupported bodies never reach disk.
    pub fn opening(opcode: u16, body: &[u8]) -> Self {
        Self {
            character: None,
            received_after_ms: None,
            checkpoint: "opening".into(),
            stream: Stream::Opening,
            packet: decode(opcode, body).unwrap_or(Packet {
                opcode,
                fields: Default::default(),
            }),
        }
    }

    /// Retains every opcode and discards undeclared bodies.
    pub fn from_frame(step: &Step, opcode: u16, body: &[u8]) -> Self {
        Self::from_frame_with_rules(step, opcode, body, &[])
    }

    /// Decodes unexpected packets only when a rule filters their subject.
    pub fn from_frame_with_rules(
        step: &Step,
        opcode: u16,
        body: &[u8],
        unexpected_rules: &[crate::compare::UnexpectedPacketRule],
    ) -> Self {
        Self::from_frame_for_rules(step, None, opcode, body, unexpected_rules)
    }

    pub fn from_frame_for(step: &Step, character: Option<&str>, opcode: u16, body: &[u8]) -> Self {
        Self::from_frame_for_rules(step, character, opcode, body, &[])
    }

    pub fn from_frame_for_rules(
        step: &Step,
        character: Option<&str>,
        opcode: u16,
        body: &[u8],
        unexpected_rules: &[crate::compare::UnexpectedPacketRule],
    ) -> Self {
        let wire_opcode = opcode;
        let opcode = if opcode == 0x01f6 { 0x00a9 } else { opcode };
        let stream = character
            .and_then(|id| step.checkpoint.for_character(id))
            .map_or_else(
                || step.checkpoint.stream(&step.id, opcode),
                |capture| capture.stream(&step.id, opcode),
            );
        let stream = if stream == Stream::Unexpected
            && character == step.character.as_deref()
            && matches!(
                step.action.as_ref(),
                Some(SessionAction::Connect | SessionAction::Reconnect)
            ) {
            Stream::Opening
        } else {
            stream
        };
        let decode_unexpected = stream == Stream::Unexpected
            && unexpected_rules.iter().any(|rule| {
                rule.checkpoint == step.id
                    && rule.opcode == opcode
                    && rule.subject_guid_high.is_some()
                    && rule
                        .character
                        .as_deref()
                        .is_none_or(|id| Some(id) == character)
            });
        let (stream, packet) = if stream == Stream::Unexpected && !decode_unexpected {
            (
                stream,
                Packet {
                    opcode,
                    fields: Default::default(),
                },
            )
        } else if stream == Stream::Opening {
            (
                stream,
                decode(wire_opcode, body).unwrap_or(Packet {
                    opcode,
                    fields: Default::default(),
                }),
            )
        } else {
            match decode(wire_opcode, body) {
                Ok(packet) => (stream, packet),
                Err(_) => (
                    Stream::Malformed,
                    Packet {
                        opcode,
                        fields: Default::default(),
                    },
                ),
            }
        };
        Self {
            character: character.map(str::to_owned),
            received_after_ms: None,
            checkpoint: step.id.clone(),
            stream,
            packet,
        }
    }
}

impl Checkpoint {
    pub(crate) fn stream(&self, id: &str, opcode: u16) -> Stream {
        if self.replies.contains(&opcode) {
            Stream::Reply(id.into())
        } else if let Some(broadcast) = self.broadcasts.iter().find(|b| b.opcode == opcode) {
            Stream::Broadcast(broadcast.stream.clone())
        } else {
            Stream::Unexpected
        }
    }

    pub fn for_character(&self, id: &str) -> Option<&CharacterCheckpoint> {
        self.characters
            .iter()
            .find(|capture| capture.character == id)
    }
}

impl CharacterCheckpoint {
    pub(crate) fn stream(&self, id: &str, opcode: u16) -> Stream {
        if self.replies.contains(&opcode) {
            Stream::Reply(id.into())
        } else if let Some(b) = self.broadcasts.iter().find(|b| b.opcode == opcode) {
            Stream::Broadcast(b.stream.clone())
        } else {
            Stream::Unexpected
        }
    }
}

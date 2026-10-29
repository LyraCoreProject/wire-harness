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
    pub fixtures: Vec<FixtureVerb>,
    pub steps: Vec<Step>,
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
    pub send: ClientPacket,
    pub checkpoint: Checkpoint,
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
    Fixture(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    #[serde(default)]
    pub expect: Vec<Expectation>,
    pub window_ms: u64,
    pub replies: Vec<u16>,
    #[serde(default)]
    pub broadcasts: Vec<Broadcast>,
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
        ensure!(self.schema_version == 3, "unsupported scenario schema");
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
        let FixtureVerb::Character {
            name,
            race,
            class,
            level,
            at,
        } = &self.fixtures[0]
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
        for fixture in &self.fixtures[1..] {
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
                        identifier(id) && fixture_ids.insert(id),
                        "invalid or duplicate creature fixture id"
                    );
                    ensure!(
                        *entry != 0 && offset.is_finite() && offset.abs() <= 100.0,
                        "invalid creature fixture"
                    );
                }
            }
        }
        ensure!(
            !self.steps.is_empty() && self.steps.len() <= 256,
            "scenario needs 1 through 256 steps"
        );
        let mut ids = BTreeSet::new();
        for step in &self.steps {
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
            if let Some(Target::Fixture(id)) = step.send.target() {
                ensure!(
                    fixture_ids.contains(id),
                    "request names an unknown creature fixture"
                );
            }
            let expected = match step.send {
                ClientPacket::Ping { .. } => Some(0x01dd),
                ClientPacket::NameQuery { .. } => Some(0x0051),
                ClientPacket::TaxiNodeStatusQuery { .. } => Some(0x01ab),
                ClientPacket::TaxiQueryAvailableNodes { .. } => Some(0x01a9),
                ClientPacket::BankerActivate { .. } => Some(0x01b8),
                ClientPacket::BuyBankSlot { .. } => None,
                ClientPacket::ActivateTaxi {
                    source,
                    destination,
                    ..
                } => {
                    ensure!(
                        source != destination
                            && nodes.contains(&source)
                            && nodes.contains(&destination),
                        "taxi endpoints must be distinct known-node fixtures"
                    );
                    Some(0x01ae)
                }
            };
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
}

impl ClientPacket {
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
        let guid = match self.target() {
            None => 0,
            Some(Target::Character) => character_guid,
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
        let wire_opcode = opcode;
        let opcode = if opcode == 0x01f6 { 0x00a9 } else { opcode };
        let stream = step.checkpoint.stream(&step.id, opcode);
        let decode_unexpected = stream == Stream::Unexpected
            && unexpected_rules.iter().any(|rule| {
                rule.checkpoint == step.id
                    && rule.opcode == opcode
                    && rule.subject_guid_high.is_some()
            });
        let (stream, packet) = if stream == Stream::Unexpected && !decode_unexpected {
            (
                stream,
                Packet {
                    opcode,
                    fields: Default::default(),
                },
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
}

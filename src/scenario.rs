use std::collections::BTreeSet;

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
    pub fixtures: Vec<FixtureVerb>,
    pub steps: Vec<Step>,
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Character,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub window_ms: u64,
    pub replies: Vec<u16>,
    #[serde(default)]
    pub broadcasts: Vec<Broadcast>,
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
    Reply(String),
    Broadcast(String),
    Unexpected,
    Malformed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
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
        ensure!(self.schema_version == 2, "unsupported scenario schema");
        ensure!(
            identifier(&self.id) && identifier(&self.behavior),
            "invalid scenario or Behavior id"
        );
        ensure!(
            self.fixtures.len() == 1,
            "exactly one Character fixture is required"
        );
        self.rules.validate()?;
        let FixtureVerb::Character {
            name,
            race,
            class,
            level,
            at,
        } = &self.fixtures[0];
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
        ensure!(
            !self.steps.is_empty() && self.steps.len() <= 256,
            "scenario needs 1 through 256 steps"
        );
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            ensure!(
                identifier(&step.id) && ids.insert(&step.id),
                "invalid or duplicate step id"
            );
            let checkpoint = &step.checkpoint;
            ensure!(
                (1..=30_000).contains(&checkpoint.window_ms),
                "checkpoint window must be 1 through 30000 ms"
            );
            ensure!(
                !checkpoint.replies.is_empty() && checkpoint.replies.len() <= 256,
                "checkpoint needs bounded replies"
            );
            let expected = match step.send {
                ClientPacket::Ping { .. } => 0x01dd,
                ClientPacket::NameQuery { .. } => 0x0051,
            };
            ensure!(
                checkpoint.replies.contains(&expected),
                "checkpoint omits its request's reply opcode"
            );
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
    pub fn encode(&self, character_guid: u64) -> (u32, Vec<u8>) {
        match self {
            Self::Ping {
                sequence,
                latency_ms,
            } => {
                let mut body = sequence.to_le_bytes().to_vec();
                body.extend(latency_ms.to_le_bytes());
                (0x01dc, body)
            }
            Self::NameQuery {
                target: Target::Character,
            } => (0x0050, character_guid.to_le_bytes().to_vec()),
        }
    }
}

impl Observation {
    /// Retains every opcode; undeclared bodies are discarded without decoding or storing packet text.
    pub fn from_frame(step: &Step, opcode: u16, body: &[u8]) -> Self {
        let stream = step.checkpoint.stream(&step.id, opcode);
        let (stream, packet) = if stream == Stream::Unexpected {
            (
                stream,
                Packet {
                    opcode,
                    fields: Default::default(),
                },
            )
        } else {
            match decode(opcode, body) {
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

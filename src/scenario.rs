use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::decode::{decode, Packet};
use crate::{is_read_timeout, WireClient};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u32,
    pub id: String,
    pub behavior: String,
    pub fixtures: Vec<FixtureVerb>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureVerb {
    Character {
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub checkpoint: String,
    pub stream: Stream,
    pub packet: Packet,
}

pub trait FixtureAdapter {
    /// Returns a connected client after every requested fixture has been applied and checked.
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<WireClient>;
}

pub trait Transport {
    fn character_guid(&self) -> u64;
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()>;
    /// Returns None when the receive deadline expires; other failures must propagate.
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>>;
}

impl Transport for WireClient {
    fn character_guid(&self) -> u64 {
        self.self_guid
    }
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()> {
        self.send_raw(opcode, body)
    }
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        self.set_recv_timeout(timeout)?;
        match self.recv_raw() {
            Ok(packet) => Ok(Some(packet)),
            Err(error) if is_read_timeout(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
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
        ensure!(self.schema_version == 1, "unsupported scenario schema");
        ensure!(
            identifier(&self.id) && identifier(&self.behavior),
            "invalid scenario or Behavior id"
        );
        ensure!(
            self.fixtures.len() == 1,
            "exactly one Character fixture is required"
        );
        let FixtureVerb::Character {
            race,
            class,
            level,
            at,
        } = &self.fixtures[0];
        crate::cli::parse_race(race)?;
        crate::cli::parse_class(class)?;
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

pub fn run(scenario: &Scenario, adapter: &mut dyn FixtureAdapter) -> Result<Vec<Observation>> {
    scenario.validate()?;
    let mut client = adapter.prepare(&scenario.fixtures)?;
    run_on(scenario, &mut client)
}

pub fn run_on(scenario: &Scenario, transport: &mut impl Transport) -> Result<Vec<Observation>> {
    scenario.validate()?;
    ensure!(
        transport.character_guid() != 0,
        "scenario requires a Character"
    );
    let mut observations = Vec::new();
    for step in &scenario.steps {
        let (opcode, body) = step.send.encode(transport.character_guid());
        transport.send(opcode, &body)?;
        let deadline = Instant::now() + Duration::from_millis(step.checkpoint.window_ms);
        let mut replies = Vec::new();
        let mut frames = 0;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            let Some((opcode, body)) = transport.receive(remaining)? else {
                break;
            };
            frames += 1;
            ensure!(frames <= 4096, "checkpoint exceeded 4096 received frames");
            let stream = if step.checkpoint.replies.contains(&opcode) {
                replies.push(opcode);
                Stream::Reply(step.id.clone())
            } else if let Some(broadcast) = step
                .checkpoint
                .broadcasts
                .iter()
                .find(|b| b.opcode == opcode)
            {
                Stream::Broadcast(broadcast.stream.clone())
            } else {
                continue;
            };
            observations.push(Observation {
                checkpoint: step.id.clone(),
                stream,
                packet: decode(opcode, &body)?,
            });
        }
        if replies != step.checkpoint.replies {
            bail!(
                "checkpoint {}: reply opcodes {replies:x?}, expected {:x?}",
                step.id,
                step.checkpoint.replies
            );
        }
    }
    Ok(observations)
}

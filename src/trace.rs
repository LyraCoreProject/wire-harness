use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decode::Field;
use crate::scenario::{identifier, Observation, Scenario, Stream};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if hex(&value, 64) {
            Ok(Self(value))
        } else {
            Err("invalid SHA-256 digest")
        }
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RecordedField {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Guid(u64),
    TimeMs(u64),
    WallClockMs(u64),
    TextSha256(Sha256Digest),
    List(Vec<RecordedField>),
    Object(BTreeMap<String, RecordedField>),
}

impl RecordedField {
    fn scrub(field: &Field) -> Self {
        match field {
            Field::Unsigned(v) => Self::Unsigned(*v),
            Field::Signed(v) => Self::Signed(*v),
            Field::Float(v) => Self::Float(*v),
            Field::Guid(v) => Self::Guid(*v),
            Field::TimeMs(v) => Self::TimeMs(*v),
            Field::WallClockMs(v) => Self::WallClockMs(*v),
            Field::Text(v) => Self::TextSha256(Sha256Digest::of(v.as_bytes())),
            Field::List(v) => Self::List(v.iter().map(Self::scrub).collect()),
            Field::Object(v) => Self::Object(scrub_fields(v)),
        }
    }

    pub(crate) fn validate(&self, depth: usize) -> Result<()> {
        ensure!(depth <= 32, "trace fields exceed 32 levels");
        match self {
            Self::Float(value) => ensure!(value.is_finite(), "non-finite trace field"),
            Self::List(values) => {
                for value in values {
                    value.validate(depth + 1)?;
                }
            }
            Self::Object(fields) => validate_fields(fields, depth + 1)?,
            _ => (),
        }
        Ok(())
    }
}

fn scrub_fields(fields: &BTreeMap<String, Field>) -> BTreeMap<String, RecordedField> {
    fields
        .iter()
        .map(|(name, field)| (name.clone(), RecordedField::scrub(field)))
        .collect()
}

fn validate_fields(fields: &BTreeMap<String, RecordedField>, depth: usize) -> Result<()> {
    for (name, field) in fields {
        ensure!(identifier(name), "invalid trace field identifier");
        field.validate(depth)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub server: String,
    pub binary_release: String,
    pub binary_sha256: Sha256Digest,
    pub source_commit: String,
    pub database_commit: String,
}

impl Provenance {
    pub fn validate(&self) -> Result<()> {
        ensure!(identifier(&self.server), "invalid server identifier");
        ensure!(
            !self.binary_release.is_empty()
                && self.binary_release.len() <= 160
                && self
                    .binary_release
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)),
            "invalid binary release identifier"
        );
        ensure!(
            hex(&self.source_commit, 40),
            "source commit must be a full SHA"
        );
        ensure!(
            hex(&self.database_commit, 40),
            "database commit must be a full SHA"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    Golden { provenance: Provenance },
    Candidate { adapter: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub schema_version: u32,
    pub character_guid: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub characters: BTreeMap<String, u64>,
    pub scenario: String,
    pub behavior: String,
    pub scenario_sha256: Sha256Digest,
    pub recorded_at_unix_ms: u64,
    pub origin: Origin,
}

impl Header {
    pub fn golden(
        scenario_text: &str,
        provenance: Provenance,
        recorded_at_unix_ms: u64,
    ) -> Result<Self> {
        provenance.validate()?;
        Self::new(
            scenario_text,
            Origin::Golden { provenance },
            recorded_at_unix_ms,
        )
    }

    pub fn candidate(
        scenario_text: &str,
        adapter: String,
        recorded_at_unix_ms: u64,
    ) -> Result<Self> {
        ensure!(identifier(&adapter), "invalid candidate adapter identifier");
        Self::new(
            scenario_text,
            Origin::Candidate { adapter },
            recorded_at_unix_ms,
        )
    }

    fn new(scenario_text: &str, origin: Origin, recorded_at_unix_ms: u64) -> Result<Self> {
        let scenario = Scenario::parse(scenario_text)?;
        Ok(Self {
            schema_version: scenario.schema_version,
            character_guid: 0,
            characters: BTreeMap::new(),
            scenario: scenario.id,
            behavior: scenario.behavior,
            scenario_sha256: Sha256Digest::of(scenario_text.as_bytes()),
            recorded_at_unix_ms,
            origin,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedPacket {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    pub received_after_ms: Option<u64>,
    pub checkpoint: String,
    pub stream: Stream,
    pub opcode: u16,
    pub fields: BTreeMap<String, RecordedField>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trace {
    pub header: Header,
    pub packets: Vec<RecordedPacket>,
}

impl Trace {
    /// Scrubs all packet text, including nested fields, before constructing serializable packets.
    pub fn record(header: Header, observations: &[Observation]) -> Result<Self> {
        let trace = Self {
            header,
            packets: observations
                .iter()
                .map(|o| RecordedPacket {
                    character: o.character.clone(),
                    received_after_ms: o.received_after_ms,
                    checkpoint: o.checkpoint.clone(),
                    stream: o.stream.clone(),
                    opcode: o.packet.opcode,
                    fields: scrub_fields(&o.packet.fields),
                })
                .collect(),
        };
        trace.validate()?;
        Ok(trace)
    }

    pub fn parse(json: &[u8]) -> Result<Self> {
        ensure!(json.len() <= 16 * 1024 * 1024, "trace exceeds 16 MiB");
        let trace: Self = serde_json::from_slice(json).context("trace JSON")?;
        trace.validate()?;
        Ok(trace)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.header.schema_version, 3 | 4),
            "unsupported trace schema"
        );
        if self.header.schema_version == 3 {
            ensure!(
                self.header.characters.is_empty(),
                "schema 3 trace has named Characters"
            );
        } else {
            ensure!(
                self.header.character_guid == 0 && (1..=16).contains(&self.header.characters.len()),
                "schema 4 needs named Character GUIDs"
            );
            ensure!(
                self.header
                    .characters
                    .iter()
                    .all(|(id, guid)| identifier(id) && *guid != 0),
                "invalid named Character GUID"
            );
            ensure!(
                self.header
                    .characters
                    .values()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.header.characters.len(),
                "named Characters must have distinct GUIDs"
            );
        }
        ensure!(
            identifier(&self.header.scenario) && identifier(&self.header.behavior),
            "invalid trace identity"
        );
        match &self.header.origin {
            Origin::Golden { provenance } => {
                provenance.validate()?;
            }
            Origin::Candidate { adapter } => {
                ensure!(identifier(adapter), "invalid candidate adapter identifier")
            }
        }
        ensure!(self.packets.len() <= 65_536, "trace exceeds 65536 packets");
        let mut started = false;
        for packet in &self.packets {
            if self.header.schema_version == 4 {
                ensure!(
                    packet
                        .character
                        .as_ref()
                        .is_some_and(|id| self.header.characters.contains_key(id)),
                    "packet names an unknown Character"
                );
            } else {
                ensure!(
                    packet.character.is_none(),
                    "schema 3 packet names a Character"
                );
            }
            ensure!(
                identifier(&packet.checkpoint),
                "invalid checkpoint identifier"
            );
            if packet.checkpoint == "opening" {
                ensure!(
                    !started && packet.stream == Stream::Opening,
                    "opening packets must precede every step"
                );
            } else {
                started = true;
                ensure!(
                    packet.stream != Stream::Opening || self.header.schema_version == 4,
                    "opening packets must precede every step"
                );
            }
            match &packet.stream {
                Stream::Opening => (),
                Stream::Reply(id) => {
                    ensure!(
                        *id == packet.checkpoint,
                        "reply stream must name its checkpoint request"
                    );
                }
                Stream::Broadcast(id) => ensure!(identifier(id), "invalid stream identifier"),
                Stream::Malformed => {
                    ensure!(
                        packet.fields.is_empty(),
                        "undecoded packets must not retain fields"
                    );
                }
                Stream::Unexpected => (),
            }
            validate_fields(&packet.fields, 0)?;
        }
        Ok(())
    }

    /// Writes a new scrubbed trace without replacing an existing file.
    pub fn write_new(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        ensure!(bytes.len() < 16 * 1024 * 1024, "trace exceeds 16 MiB");
        bytes.push(b'\n');
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct GoldenTrace {
    scenario: Scenario,
    trace: Trace,
}

impl GoldenTrace {
    /// Binds rules to recorded scenario bytes and checks disclosures; see docs/traces.md.
    pub fn new(scenario_text: &str, trace: Trace) -> Result<Self> {
        trace.validate()?;
        ensure!(
            matches!(trace.header.origin, Origin::Golden { .. }),
            "expected a Golden Trace, got a candidate"
        );
        let scenario = Scenario::parse(scenario_text)?;
        ensure!(
            trace.header.scenario_sha256 == Sha256Digest::of(scenario_text.as_bytes())
                && trace.header.scenario == scenario.id
                && trace.header.behavior == scenario.behavior
                && trace.header.schema_version == scenario.schema_version,
            "Golden Trace does not match this scenario revision"
        );
        if scenario.schema_version == 4 {
            ensure!(
                scenario
                    .characters
                    .iter()
                    .map(|character| &character.id)
                    .collect::<std::collections::BTreeSet<_>>()
                    == trace.header.characters.keys().collect(),
                "Golden Trace Character set differs from scenario"
            );
            for packet in &trace.packets {
                if packet.stream == Stream::Opening && packet.checkpoint != "opening" {
                    ensure!(
                        scenario
                            .steps
                            .iter()
                            .any(|step| step.id == packet.checkpoint
                                && ((step.character == packet.character
                                    && matches!(
                                        step.action.as_ref(),
                                        Some(
                                            crate::scenario::SessionAction::Connect
                                                | crate::scenario::SessionAction::Reconnect
                                        )
                                    ))
                                    || step.events.iter().any(|event| {
                                        packet.character.as_deref()
                                            == Some(event.character.as_str())
                                            && matches!(
                                                event.action,
                                                crate::scenario::SessionAction::Connect
                                                    | crate::scenario::SessionAction::Reconnect
                                            )
                                    }))),
                        "opening packet must belong to a Character connection step"
                    );
                }
            }
        }
        crate::compare::validate_rule_samples(&scenario, &trace)?;
        Ok(Self { scenario, trace })
    }

    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }
}

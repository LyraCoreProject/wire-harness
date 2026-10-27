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
            Field::Text(v) => Self::TextSha256(Sha256Digest::of(v.as_bytes())),
            Field::List(v) => Self::List(v.iter().map(Self::scrub).collect()),
            Field::Object(v) => Self::Object(scrub_fields(v)),
        }
    }

    fn validate(&self, depth: usize) -> Result<()> {
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
#[serde(deny_unknown_fields)]
pub struct Header {
    pub schema_version: u32,
    pub scenario: String,
    pub behavior: String,
    pub scenario_sha256: Sha256Digest,
    pub recorded_at_unix_ms: u64,
    pub provenance: Provenance,
}

impl Header {
    pub fn new(
        scenario_text: &str,
        provenance: Provenance,
        recorded_at_unix_ms: u64,
    ) -> Result<Self> {
        let scenario = Scenario::parse(scenario_text)?;
        provenance.validate()?;
        Ok(Self {
            schema_version: 1,
            scenario: scenario.id,
            behavior: scenario.behavior,
            scenario_sha256: Sha256Digest::of(scenario_text.as_bytes()),
            recorded_at_unix_ms,
            provenance,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedPacket {
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
        ensure!(self.header.schema_version == 1, "unsupported trace schema");
        ensure!(
            identifier(&self.header.scenario) && identifier(&self.header.behavior),
            "invalid trace identity"
        );
        self.header.provenance.validate()?;
        ensure!(
            !self.packets.is_empty() && self.packets.len() <= 65_536,
            "trace needs 1 through 65536 packets"
        );
        for packet in &self.packets {
            ensure!(
                identifier(&packet.checkpoint),
                "invalid checkpoint identifier"
            );
            let (Stream::Reply(id) | Stream::Broadcast(id)) = &packet.stream;
            ensure!(identifier(id), "invalid stream identifier");
            if matches!(packet.stream, Stream::Reply(_)) {
                ensure!(
                    *id == packet.checkpoint,
                    "reply stream must name its checkpoint request"
                );
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

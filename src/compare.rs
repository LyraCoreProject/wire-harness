use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::scenario::{identifier, Stream};
use crate::trace::{RecordedField as Field, RecordedPacket, Trace};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RandomRange {
    Unsigned { minimum: u64, maximum: u64 },
    Signed { minimum: i64, maximum: i64 },
    Float { minimum: f64, maximum: f64 },
}

impl RandomRange {
    fn valid(&self) -> bool {
        match self {
            Self::Unsigned { minimum, maximum } => minimum <= maximum,
            Self::Signed { minimum, maximum } => minimum <= maximum,
            Self::Float { minimum, maximum } => {
                minimum.is_finite() && maximum.is_finite() && minimum <= maximum
            }
        }
    }

    fn contains(&self, value: &Field) -> bool {
        match (self, value) {
            (Self::Unsigned { minimum, maximum }, Field::Unsigned(value)) => {
                (minimum..=maximum).contains(&value)
            }
            (Self::Signed { minimum, maximum }, Field::Signed(value)) => {
                (minimum..=maximum).contains(&value)
            }
            (Self::Float { minimum, maximum }, Field::Float(value)) => {
                (minimum..=maximum).contains(&value)
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RandomRule {
    pub name: String,
    pub reason: String,
    pub opcode: u16,
    pub path: Vec<String>,
    pub range: RandomRange,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceRules {
    pub time_tolerance_ms: u64,
    pub random: Vec<RandomRule>,
}

impl Default for EquivalenceRules {
    fn default() -> Self {
        Self {
            time_tolerance_ms: 100,
            random: Vec::new(),
        }
    }
}

impl EquivalenceRules {
    pub fn validate(&self) -> Result<()> {
        let mut paths = BTreeSet::new();
        let mut names = BTreeSet::new();
        for rule in &self.random {
            ensure!(
                identifier(&rule.name) && names.insert(&rule.name),
                "invalid or duplicate rule name"
            );
            ensure!(
                !rule.reason.trim().is_empty() && rule.reason.len() <= 512,
                "rule needs a reason"
            );
            ensure!(
                !rule.path.is_empty() && rule.path.iter().all(|p| identifier(p)),
                "invalid rule path"
            );
            ensure!(
                paths.insert((rule.opcode, &rule.path)),
                "duplicate random field rule"
            );
            ensure!(rule.range.valid(), "invalid random range");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Difference {
    pub checkpoint: String,
    pub stream: Option<Stream>,
    pub packet_index: Option<usize>,
    pub field_path: Vec<String>,
    pub rule: String,
    pub reason: String,
    pub expected: Option<Field>,
    pub actual: Option<Field>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub differences: Vec<Difference>,
}

impl Comparison {
    pub fn is_match(&self) -> bool {
        self.differences.is_empty()
    }
}

struct Comparator<'a> {
    rules: &'a EquivalenceRules,
    expected_guids: BTreeMap<u64, u64>,
    actual_guids: BTreeMap<u64, u64>,
    used_random: BTreeSet<usize>,
    differences: Vec<Difference>,
}

#[derive(Clone)]
struct Location {
    checkpoint: String,
    stream: Option<Stream>,
    packet_index: Option<usize>,
    path: Vec<String>,
}

impl Location {
    fn child(&self, field: String) -> Self {
        let mut location = self.clone();
        location.path.push(field);
        location
    }
}

impl Comparator<'_> {
    fn difference(
        &mut self,
        location: &Location,
        rule: &str,
        reason: &str,
        expected: Option<&Field>,
        actual: Option<&Field>,
    ) {
        self.differences.push(Difference {
            checkpoint: location.checkpoint.clone(),
            stream: location.stream.clone(),
            packet_index: location.packet_index,
            field_path: location.path.clone(),
            rule: rule.into(),
            reason: reason.into(),
            expected: expected.cloned(),
            actual: actual.cloned(),
        });
    }

    fn fields(
        &mut self,
        opcode: u16,
        location: &Location,
        expected: &BTreeMap<String, Field>,
        actual: &BTreeMap<String, Field>,
    ) {
        for name in expected
            .keys()
            .chain(actual.keys())
            .collect::<BTreeSet<_>>()
        {
            let location = location.child(name.clone());
            match (expected.get(name), actual.get(name)) {
                (Some(e), Some(a)) => self.field(opcode, &location, e, a),
                (e, a) => {
                    self.difference(&location, "field_shape", "field is missing or extra", e, a)
                }
            }
        }
    }

    fn field(&mut self, opcode: u16, location: &Location, expected: &Field, actual: &Field) {
        if let Some((index, rule)) = self
            .rules
            .random
            .iter()
            .enumerate()
            .find(|(_, r)| r.opcode == opcode && r.path == location.path)
        {
            self.used_random.insert(index);
            if !rule.range.contains(expected) || !rule.range.contains(actual) {
                self.difference(
                    location,
                    &rule.name,
                    &rule.reason,
                    Some(expected),
                    Some(actual),
                );
            }
            return;
        }
        match (expected, actual) {
            (Field::Guid(e), Field::Guid(a)) => {
                let mismatch = if *e == 0 || *a == 0 {
                    e != a
                } else {
                    self.expected_guids.get(e).is_some_and(|mapped| mapped != a)
                        || self.actual_guids.get(a).is_some_and(|mapped| mapped != e)
                };
                if mismatch {
                    self.difference(
                        location,
                        "guid_bijection",
                        "GUID mapping must be consistent and one-to-one; zero maps only to zero",
                        Some(expected),
                        Some(actual),
                    );
                } else {
                    self.expected_guids.insert(*e, *a);
                    self.actual_guids.insert(*a, *e);
                }
            }
            (Field::TimeMs(e), Field::TimeMs(a)) => {
                if e.abs_diff(*a) > self.rules.time_tolerance_ms {
                    self.difference(
                        location,
                        "time_tolerance",
                        "time exceeds the configured millisecond tolerance",
                        Some(expected),
                        Some(actual),
                    );
                }
            }
            (Field::List(e), Field::List(a)) => {
                if e.len() != a.len() {
                    self.difference(
                        location,
                        "list_shape",
                        "list lengths differ",
                        Some(&Field::Unsigned(e.len() as u64)),
                        Some(&Field::Unsigned(a.len() as u64)),
                    );
                }
                for (index, (e, a)) in e.iter().zip(a).enumerate() {
                    self.field(opcode, &location.child(index.to_string()), e, a);
                }
            }
            (Field::Object(e), Field::Object(a)) => self.fields(opcode, location, e, a),
            _ if expected == actual => (),
            _ => self.difference(
                location,
                "exact_field",
                "field type and value must match",
                Some(expected),
                Some(actual),
            ),
        }
    }
}

type Streams<'a> = BTreeMap<(&'a str, &'a Stream), Vec<&'a RecordedPacket>>;

fn streams(trace: &Trace) -> Streams<'_> {
    let mut streams = BTreeMap::new();
    for packet in &trace.packets {
        streams
            .entry((packet.checkpoint.as_str(), &packet.stream))
            .or_insert_with(Vec::new)
            .push(packet);
    }
    streams
}

/// Compares within each checkpoint and stream; independent broadcast streams may interleave.
pub fn compare(expected: &Trace, actual: &Trace, rules: &EquivalenceRules) -> Result<Comparison> {
    expected.validate()?;
    actual.validate()?;
    rules.validate()?;
    let mut comparator = Comparator {
        rules,
        expected_guids: BTreeMap::new(),
        actual_guids: BTreeMap::new(),
        used_random: BTreeSet::new(),
        differences: Vec::new(),
    };
    if expected.header.scenario != actual.header.scenario
        || expected.header.behavior != actual.header.behavior
        || expected.header.scenario_sha256 != actual.header.scenario_sha256
    {
        comparator.difference(
            &Location {
                checkpoint: String::new(),
                stream: None,
                packet_index: None,
                path: vec!["header".into()],
            },
            "scenario_identity",
            "traces must record the same scenario revision and Behavior",
            None,
            None,
        );
        return Ok(Comparison {
            differences: comparator.differences,
        });
    }
    let expected_streams = streams(expected);
    let actual_streams = streams(actual);
    for key in expected_streams
        .keys()
        .chain(actual_streams.keys())
        .collect::<BTreeSet<_>>()
    {
        let e = expected_streams
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let a = actual_streams
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut location = Location {
            checkpoint: key.0.into(),
            stream: Some(key.1.clone()),
            packet_index: None,
            path: Vec::new(),
        };
        if e.len() != a.len() {
            comparator.difference(
                &location,
                "packet_count",
                "each stream must contain the same number of packets",
                Some(&Field::Unsigned(e.len() as u64)),
                Some(&Field::Unsigned(a.len() as u64)),
            );
        }
        for (index, (e, a)) in e.iter().zip(a).enumerate() {
            location.packet_index = Some(index);
            if e.opcode != a.opcode {
                comparator.difference(
                    &location,
                    "packet_order",
                    "reply packets and packets within a broadcast stream keep their order",
                    Some(&Field::Unsigned(e.opcode.into())),
                    Some(&Field::Unsigned(a.opcode.into())),
                );
                continue;
            }
            comparator.fields(e.opcode, &location, &e.fields, &a.fields);
        }
    }
    for (index, rule) in rules.random.iter().enumerate() {
        if !comparator.used_random.contains(&index) {
            comparator.difference(
                &Location {
                    checkpoint: String::new(),
                    stream: None,
                    packet_index: None,
                    path: rule.path.clone(),
                },
                &rule.name,
                "random rule did not match a compared field",
                None,
                None,
            );
        }
    }
    Ok(Comparison {
        differences: comparator.differences,
    })
}

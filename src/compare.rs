use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::scenario::{identifier, Observation, Scenario, Stream};
use crate::trace::{GoldenTrace, Header, RecordedField as Field, RecordedPacket, Trace};

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
pub struct UnexpectedPacketRule {
    pub name: String,
    pub reason: String,
    pub checkpoint: String,
    pub opcode: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceRules {
    pub time_tolerance_ms: u64,
    pub wall_clock_tolerance_ms: u64,
    #[serde(default)]
    pub random: Vec<RandomRule>,
    #[serde(default)]
    pub unexpected_packets: Vec<UnexpectedPacketRule>,
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
        let mut packets = BTreeSet::new();
        for rule in &self.unexpected_packets {
            ensure!(
                identifier(&rule.name) && names.insert(&rule.name),
                "invalid or duplicate rule name"
            );
            ensure!(
                !rule.reason.trim().is_empty() && rule.reason.len() <= 512,
                "rule needs a reason"
            );
            ensure!(identifier(&rule.checkpoint), "invalid rule checkpoint");
            ensure!(
                packets.insert((&rule.checkpoint, rule.opcode)),
                "duplicate unexpected packet rule"
            );
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
    pub opcode: Option<u16>,
    pub deviations: Vec<AppliedDeviation>,
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
    pub fn deviation_count(&self) -> usize {
        self.differences
            .iter()
            .filter(|d| !d.deviations.is_empty())
            .count()
    }

    pub fn is_match(&self) -> bool {
        self.differences.is_empty()
    }
}

struct Comparator<'a> {
    rules: &'a EquivalenceRules,
    recorded_at: (u64, u64),
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
    opcode: Option<u16>,
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
            opcode: location.opcode,
            deviations: Vec::new(),
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
            (Field::WallClockMs(e), Field::WallClockMs(a)) => {
                if e.abs_diff(self.recorded_at.0) > self.rules.wall_clock_tolerance_ms
                    || a.abs_diff(self.recorded_at.1) > self.rules.wall_clock_tolerance_ms
                {
                    self.difference(
                        location,
                        "wall_clock",
                        "wall clock must be near its own recording time",
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliberateDeviation {
    pub name: String,
    pub reason: String,
    pub source: String,
    pub difference: DeviationKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviationKind {
    FieldValue {
        checkpoint: String,
        opcode: u16,
        path: Vec<String>,
        expected: Field,
        actual: Field,
    },
    UnexpectedPacket {
        checkpoint: String,
        opcode: u16,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppliedDeviation {
    pub name: String,
    pub reason: String,
    pub source: String,
}

impl DeliberateDeviation {
    fn applies(&self, difference: &Difference) -> bool {
        match &self.difference {
            DeviationKind::FieldValue {
                checkpoint,
                opcode,
                path,
                expected,
                actual,
            } => {
                difference.checkpoint == *checkpoint
                    && difference.opcode == Some(*opcode)
                    && difference.field_path == *path
                    && !path.is_empty()
                    && difference.expected.as_ref() == Some(expected)
                    && difference.actual.as_ref() == Some(actual)
            }
            DeviationKind::UnexpectedPacket { checkpoint, opcode } => {
                difference.rule == "unexpected_packet"
                    && difference.checkpoint == *checkpoint
                    && difference.opcode == Some(*opcode)
                    && difference.actual.is_some()
            }
        }
    }
}

type Streams<'a> = BTreeMap<(String, Stream), Vec<&'a RecordedPacket>>;

impl Comparator<'_> {
    fn packets<'a>(
        &mut self,
        scenario: &Scenario,
        trace: &'a Trace,
        expected: bool,
        check_replies: bool,
    ) -> Streams<'a> {
        let mut streams = BTreeMap::new();
        for (index, packet) in trace.packets.iter().enumerate() {
            let location = Location {
                checkpoint: packet.checkpoint.clone(),
                stream: Some(packet.stream.clone()),
                packet_index: Some(index),
                path: Vec::new(),
                opcode: Some(packet.opcode),
            };
            let value = Field::Unsigned(packet.opcode.into());
            let (e, a) = if expected {
                (Some(&value), None)
            } else {
                (None, Some(&value))
            };
            let Some(step) = scenario.steps.iter().find(|s| s.id == packet.checkpoint) else {
                self.difference(
                    &location,
                    "unexpected_checkpoint",
                    "checkpoint is absent from the scenario",
                    e,
                    a,
                );
                continue;
            };
            let stream = step.checkpoint.stream(&step.id, packet.opcode);
            if stream == Stream::Unexpected {
                if !self
                    .rules
                    .unexpected_packets
                    .iter()
                    .any(|r| r.checkpoint == packet.checkpoint && r.opcode == packet.opcode)
                {
                    self.difference(
                        &location,
                        "unexpected_packet",
                        "opcode is undeclared in this checkpoint",
                        e,
                        a,
                    );
                }
                continue;
            }
            if packet.stream == Stream::Malformed {
                self.difference(
                    &location,
                    "packet_decode",
                    "declared packet could not be decoded",
                    e,
                    a,
                );
            } else if packet.stream != stream {
                self.difference(
                    &location,
                    "packet_stream",
                    "packet stream does not match its scenario declaration",
                    e,
                    a,
                );
            }
            streams
                .entry((packet.checkpoint.clone(), stream))
                .or_insert_with(Vec::new)
                .push(packet);
        }
        if check_replies {
            for step in &scenario.steps {
                let received: Vec<_> = streams
                    .get(&(step.id.clone(), Stream::Reply(step.id.clone())))
                    .into_iter()
                    .flatten()
                    .map(|p| p.opcode)
                    .collect();
                if received != step.checkpoint.replies {
                    let location = Location {
                        checkpoint: step.id.clone(),
                        stream: Some(Stream::Reply(step.id.clone())),
                        packet_index: None,
                        path: Vec::new(),
                        opcode: None,
                    };
                    let opcodes = |opcodes: &[u16]| {
                        Field::List(
                            opcodes
                                .iter()
                                .map(|v| Field::Unsigned((*v).into()))
                                .collect(),
                        )
                    };
                    self.difference(
                        &location,
                        "checkpoint_replies",
                        "reply opcodes must match the scenario in order and multiplicity",
                        Some(&opcodes(&step.checkpoint.replies)),
                        Some(&opcodes(&received)),
                    );
                }
            }
        }
        streams
    }

    fn finish(mut self, deviations: &[DeliberateDeviation]) -> Result<Comparison> {
        let mut names = BTreeSet::new();
        for deviation in deviations {
            ensure!(
                identifier(&deviation.name) && names.insert(&deviation.name),
                "invalid or duplicate deviation name"
            );
            ensure!(
                !deviation.reason.trim().is_empty() && !deviation.source.trim().is_empty(),
                "deviation needs a reason and source"
            );
            let mut used = false;
            for difference in &mut self.differences {
                if deviation.applies(difference) {
                    used = true;
                    difference.deviations.push(AppliedDeviation {
                        name: deviation.name.clone(),
                        reason: deviation.reason.clone(),
                        source: deviation.source.clone(),
                    });
                }
            }
            if !used {
                self.difference(
                    &Location {
                        checkpoint: String::new(),
                        stream: None,
                        packet_index: None,
                        path: Vec::new(),
                        opcode: None,
                    },
                    "unused_deviation",
                    &format!("{}: {}", deviation.name, deviation.reason),
                    None,
                    None,
                );
            }
        }
        Ok(Comparison {
            differences: self.differences,
        })
    }
}

fn comparator(rules: &EquivalenceRules, recorded_at: (u64, u64)) -> Comparator<'_> {
    Comparator {
        rules,
        recorded_at,
        expected_guids: BTreeMap::new(),
        actual_guids: BTreeMap::new(),
        used_random: BTreeSet::new(),
        differences: Vec::new(),
    }
}

/// Checks checkpoint replies and every received opcode, without requiring a Golden Trace.
pub fn check_observations(scenario: &Scenario, observations: &[Observation]) -> Result<Comparison> {
    scenario.validate()?;
    let header = Header {
        schema_version: 2,
        scenario: scenario.id.clone(),
        behavior: scenario.behavior.clone(),
        scenario_sha256: crate::trace::Sha256Digest::of(b""),
        recorded_at_unix_ms: 0,
        origin: crate::trace::Origin::Candidate {
            adapter: "observations".into(),
        },
    };
    let trace = Trace::record(header, observations)?;
    let mut comparator = comparator(&scenario.rules, (0, 0));
    comparator.packets(scenario, &trace, false, true);
    comparator.finish(&[])
}

/// Deviations annotate counted differences; they never make is_match return true.
pub fn compare(
    expected: &GoldenTrace,
    actual: &Trace,
    deviations: &[DeliberateDeviation],
) -> Result<Comparison> {
    actual.validate()?;
    let scenario = expected.scenario();
    let expected = expected.trace();
    let rules = &scenario.rules;
    let mut comparator = comparator(
        rules,
        (
            expected.header.recorded_at_unix_ms,
            actual.header.recorded_at_unix_ms,
        ),
    );
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
                opcode: None,
            },
            "scenario_identity",
            "traces must record the same scenario revision and Behavior",
            None,
            None,
        );
        return comparator.finish(deviations);
    }
    let expected_streams = comparator.packets(scenario, expected, true, true);
    let actual_streams = comparator.packets(scenario, actual, false, false);
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
            checkpoint: key.0.clone(),
            stream: Some(key.1.clone()),
            packet_index: None,
            path: Vec::new(),
            opcode: None,
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
            location.opcode = Some(a.opcode);
            if e.opcode != a.opcode {
                comparator.difference(
                    &location,
                    "packet_order",
                    "reply packets and packets within a broadcast stream keep their order",
                    Some(&Field::Unsigned(e.opcode.into())),
                    Some(&Field::Unsigned(a.opcode.into())),
                );
            } else if e.stream != Stream::Malformed && a.stream != Stream::Malformed {
                comparator.fields(e.opcode, &location, &e.fields, &a.fields);
            }
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
                    opcode: Some(rule.opcode),
                },
                &rule.name,
                "random rule did not match a compared field",
                None,
                None,
            );
        }
    }
    comparator.finish(deviations)
}

/// In-process callers supply decoded observations and their capture time, without server provenance.
pub fn compare_observations(
    expected: &GoldenTrace,
    observations: &[Observation],
    recorded_at_unix_ms: u64,
    deviations: &[DeliberateDeviation],
) -> Result<Comparison> {
    let mut header = expected.trace().header.clone();
    header.origin = crate::trace::Origin::Candidate {
        adapter: "observations".into(),
    };
    header.recorded_at_unix_ms = recorded_at_unix_ms;
    compare(expected, &Trace::record(header, observations)?, deviations)
}

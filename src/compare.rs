use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::scenario::{identifier, Observation, Scenario, SessionAction, Stream};
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
    #[serde(default)]
    pub character: Option<String>,
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
    #[serde(default)]
    pub character: Option<String>,
    pub opcode: u16,
    #[serde(default)]
    pub subject_guid_high: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldRule {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub character: Option<String>,
    pub opcode: u16,
    pub path: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockRule {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub character: Option<String>,
    pub opcode: u16,
    pub path: Vec<String>,
    pub tolerance_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterScope {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub all_characters: bool,
    #[serde(default)]
    pub creates_only: bool,
    pub packets: Vec<PacketSubjects>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketSubjects {
    pub opcode: u16,
    pub paths: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketTimingRule {
    #[serde(default)]
    pub values_only: bool,
    #[serde(default)]
    pub path: Vec<String>,
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub character: Option<String>,
    pub opcode: u16,
    pub tolerance_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceRules {
    #[serde(default)]
    pub identities: Vec<FieldRule>,
    #[serde(default)]
    pub clocks: Vec<ClockRule>,
    #[serde(default)]
    pub data_fields: Vec<FieldRule>,
    #[serde(default)]
    pub packet_timing: Vec<PacketTimingRule>,
    #[serde(default)]
    pub character_scope: Option<CharacterScope>,
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
                rule.character.as_deref().is_none_or(identifier),
                "invalid rule Character"
            );
            ensure!(
                paths.insert((rule.opcode, rule.path.clone(), rule.character.clone())),
                "duplicate random field rule"
            );
            ensure!(rule.range.valid(), "invalid random range");
        }
        for (name, reason, opcode, path, character) in self
            .identities
            .iter()
            .chain(&self.data_fields)
            .map(|r| (&r.name, &r.reason, r.opcode, &r.path, &r.character))
            .chain(
                self.clocks
                    .iter()
                    .map(|r| (&r.name, &r.reason, r.opcode, &r.path, &r.character)),
            )
        {
            ensure!(
                identifier(name) && names.insert(name),
                "invalid or duplicate rule name"
            );
            ensure!(
                !reason.trim().is_empty() && reason.len() <= 512,
                "rule needs a reason"
            );
            ensure!(
                !path.is_empty() && path.iter().all(|p| identifier(p)),
                "invalid field rule path"
            );
            ensure!(
                character.as_deref().is_none_or(identifier),
                "invalid rule Character"
            );
            ensure!(
                paths.insert((opcode, path.clone(), character.clone())),
                "overlapping field rules"
            );
        }
        ensure!(
            paths
                .iter()
                .all(|(opcode, path, character)| character.is_none()
                    || !paths.contains(&(*opcode, path.clone(), None))),
            "general and Character field rules overlap"
        );
        let mut timed = BTreeSet::new();
        for rule in &self.packet_timing {
            ensure!(
                identifier(&rule.name) && names.insert(&rule.name),
                "invalid or duplicate rule name"
            );
            ensure!(
                !rule.reason.trim().is_empty() && rule.reason.len() <= 512,
                "rule needs a reason"
            );
            ensure!(
                rule.path.iter().all(|part| identifier(part)),
                "invalid timing field path"
            );
            ensure!(
                rule.character.as_deref().is_none_or(identifier),
                "invalid rule Character"
            );
            ensure!(
                !rule.values_only
                    || (rule.opcode == 0x00a9
                        && rule.path.len() == 4
                        && rule.path[0] == "objects"
                        && rule.path[1].parse::<usize>().is_ok()
                        && rule.path[2] == "values"),
                "values_only timing requires an object VALUES field path"
            );
            ensure!(
                timed.insert((rule.opcode, rule.path.clone(), rule.character.clone())),
                "duplicate packet timing rule"
            );
        }
        ensure!(
            timed
                .iter()
                .all(|(opcode, path, character)| character.is_none()
                    || !timed.contains(&(*opcode, path.clone(), None))),
            "general and Character timing rules overlap"
        );
        if let Some(rule) = &self.character_scope {
            ensure!(
                identifier(&rule.name) && names.insert(&rule.name),
                "invalid or duplicate rule name"
            );
            ensure!(
                !rule.reason.trim().is_empty() && rule.reason.len() <= 512,
                "rule needs a reason"
            );
            let mut opcodes = BTreeSet::new();
            for packet in &rule.packets {
                ensure!(
                    packet.opcode != 0x00a9
                        && opcodes.insert(packet.opcode)
                        && !packet.paths.is_empty()
                        && packet.paths.iter().all(
                            |path| !path.is_empty() && path.iter().all(|part| identifier(part))
                        ),
                    "invalid packet subject paths"
                );
            }
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
                rule.character.as_deref().is_none_or(identifier),
                "invalid rule Character"
            );
            ensure!(
                packets.insert((&rule.checkpoint, &rule.character, rule.opcode)),
                "duplicate unexpected packet rule"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Difference {
    pub classification: DifferenceClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferenceClass {
    Rule,
    Data,
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

    /// Data records remain listed; only rule differences make a comparison fail.
    pub fn is_match(&self) -> bool {
        self.rule_difference_count() == 0
    }

    pub fn rule_difference_count(&self) -> usize {
        self.differences
            .iter()
            .filter(|difference| difference.classification == DifferenceClass::Rule)
            .count()
    }

    pub fn data_difference_count(&self) -> usize {
        self.differences.len() - self.rule_difference_count()
    }
}

struct Comparator<'a> {
    rules: &'a EquivalenceRules,
    recorded_at: (u64, u64),
    expected_guids: BTreeMap<u64, u64>,
    actual_guids: BTreeMap<u64, u64>,
    used_random: BTreeSet<usize>,
    identities: BTreeMap<(usize, Option<String>, u64), u64>,
    reverse_identities: BTreeMap<(usize, Option<String>, u64), u64>,
    used_identities: BTreeSet<usize>,
    clock_origins: BTreeMap<(usize, Option<String>), (u32, u32)>,
    used_clocks: BTreeSet<usize>,
    used_data_fields: BTreeSet<usize>,
    used_timing: BTreeSet<usize>,
    unmatched_opcodes: BTreeSet<(Option<String>, u16)>,
    unmatched_fields: BTreeSet<(Option<String>, u16, Vec<String>)>,
    differences: Vec<Difference>,
}

#[derive(Clone)]
struct Location {
    character: Option<String>,
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

fn data_shape_matches(expected: &Field, actual: &Field) -> bool {
    match (expected, actual) {
        (Field::Unsigned(_), Field::Unsigned(_))
        | (Field::Signed(_), Field::Signed(_))
        | (Field::Float(_), Field::Float(_))
        | (Field::TextSha256(_), Field::TextSha256(_)) => true,
        (Field::Object(e), Field::Object(a)) => {
            e.keys().eq(a.keys())
                && e.iter()
                    .all(|(key, value)| data_shape_matches(value, &a[key]))
        }
        (Field::List(e), Field::List(a)) => {
            e.iter()
                .chain(a)
                .all(|value| data_shape_matches(value, value))
                && match (e.first(), a.first()) {
                    (Some(ef), Some(af)) => {
                        e.iter().all(|value| data_shape_matches(value, af))
                            && a.iter().all(|value| data_shape_matches(ef, value))
                    }
                    _ => true,
                }
        }
        _ => false,
    }
}

// See docs/traces.md for alignment ties and the bounded score cache.
fn align(
    expected: usize,
    actual: usize,
    score: impl Fn(usize, usize) -> Option<usize>,
) -> Vec<(Option<usize>, Option<usize>)> {
    let scores = expected
        .checked_mul(actual)
        .filter(|&count| count <= 1_048_576)
        .map(|_| {
            (0..expected)
                .flat_map(|i| (0..actual).map(move |j| (i, j)))
                .map(|(i, j)| score(i, j))
                .collect::<Vec<_>>()
        });
    let score = |i, j| match &scores {
        Some(scores) => scores[i * actual + j],
        None => score(i, j),
    };
    fn lengths(
        e: &[usize],
        a: &[usize],
        score: &impl Fn(usize, usize) -> Option<usize>,
    ) -> Vec<(usize, usize)> {
        let mut row = vec![(0, 0); a.len() + 1];
        for &ei in e {
            let mut diagonal = (0, 0);
            for (j, &ai) in a.iter().enumerate() {
                let previous = row[j + 1];
                let skipped = row[j].max(previous);
                row[j + 1] = match score(ei, ai) {
                    Some(quality) => skipped.max((diagonal.0 + 1, diagonal.1 + quality)),
                    None => skipped,
                };
                diagonal = previous;
            }
        }
        row
    }
    fn lcs(
        e: &[usize],
        a: &[usize],
        score: &impl Fn(usize, usize) -> Option<usize>,
        pairs: &mut Vec<(usize, usize)>,
    ) {
        if e.is_empty() || a.is_empty() {
            return;
        }
        if e.len() == 1 {
            if let Some((ai, _)) = a
                .iter()
                .rev()
                .filter_map(|&ai| score(e[0], ai).map(|quality| (ai, quality)))
                .max_by_key(|(_, quality)| *quality)
            {
                pairs.push((e[0], ai));
            }
            return;
        }
        let middle = e.len() / 2;
        let left = lengths(&e[..middle], a, score);
        let right = lengths(
            &e[middle..].iter().copied().rev().collect::<Vec<_>>(),
            &a.iter().copied().rev().collect::<Vec<_>>(),
            score,
        );
        let split = (0..=a.len())
            .max_by_key(|&i| {
                (
                    left[i].0 + right[a.len() - i].0,
                    left[i].1 + right[a.len() - i].1,
                    i,
                )
            })
            .unwrap();
        drop(left);
        drop(right);
        lcs(&e[..middle], &a[..split], score, pairs);
        lcs(&e[middle..], &a[split..], score, pairs);
    }
    let mut pairs = Vec::new();
    lcs(
        &(0..expected).collect::<Vec<_>>(),
        &(0..actual).collect::<Vec<_>>(),
        &score,
        &mut pairs,
    );
    let mut result = Vec::new();
    let (mut ei, mut ai) = (0, 0);
    for (e, a) in pairs.into_iter().chain([(expected, actual)]) {
        result.extend((ei..e).map(|i| (Some(i), None)));
        result.extend((ai..a).map(|i| (None, Some(i))));
        if e < expected {
            result.push((Some(e), Some(a)));
        }
        ei = e + 1;
        ai = a + 1;
    }
    result
}

fn equal_field(expected: &Field, actual: &Field) -> usize {
    match (expected, actual) {
        (Field::Guid(_), Field::Guid(_)) => 0,
        (Field::Object(e), Field::Object(a)) => equal_fields(e, a),
        (Field::List(e), Field::List(a)) => e.iter().zip(a).map(|(e, a)| equal_field(e, a)).sum(),
        (e, a) => usize::from(e == a),
    }
}

fn equal_fields(expected: &BTreeMap<String, Field>, actual: &BTreeMap<String, Field>) -> usize {
    expected
        .iter()
        .filter_map(|(key, e)| actual.get(key).map(|a| equal_field(e, a)))
        .sum()
}

fn subject(fields: &BTreeMap<String, Field>) -> Option<u64> {
    for name in ["guid", "caster", "attacker"] {
        if let Some(Field::Guid(guid)) = fields.get(name) {
            return Some(*guid);
        }
    }
    if let Some(Field::List(objects)) = fields.get("objects") {
        if let [Field::Object(object)] = objects.as_slice() {
            return subject(object);
        }
    }
    None
}

impl Comparator<'_> {
    fn unmatched_opcode(&self, character: &Option<String>, opcode: u16) -> bool {
        self.unmatched_opcodes.iter().any(|(recipient, candidate)| {
            *candidate == opcode
                && character
                    .as_ref()
                    .is_none_or(|id| recipient.as_ref() == Some(id))
        })
    }

    fn unmatched_field(&self, character: &Option<String>, opcode: u16, path: &[String]) -> bool {
        self.unmatched_fields
            .iter()
            .any(|(recipient, candidate, candidate_path)| {
                *candidate == opcode
                    && candidate_path == path
                    && character
                        .as_ref()
                        .is_none_or(|id| recipient.as_ref() == Some(id))
            })
    }

    fn guid_matches(&self, expected: u64, actual: u64) -> bool {
        if expected == 0 || actual == 0 {
            expected == actual
        } else {
            self.expected_guids
                .get(&expected)
                .map_or(true, |mapped| *mapped == actual)
                && self
                    .actual_guids
                    .get(&actual)
                    .map_or(true, |mapped| *mapped == expected)
        }
    }

    fn packet_matches(&self, expected: &RecordedPacket, actual: &RecordedPacket) -> bool {
        expected.opcode == actual.opcode
            && match (subject(&expected.fields), subject(&actual.fields)) {
                (Some(e), Some(a)) => self.guid_matches(e, a),
                _ => true,
            }
    }

    fn difference(
        &mut self,
        location: &Location,
        rule: &str,
        reason: &str,
        expected: Option<&Field>,
        actual: Option<&Field>,
    ) {
        self.differences.push(Difference {
            classification: DifferenceClass::Rule,
            character: location.character.clone(),
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
        if let Some((index, rule)) = self.rules.data_fields.iter().enumerate().find(|(_, rule)| {
            rule.opcode == opcode
                && rule.path == location.path
                && rule
                    .character
                    .as_ref()
                    .is_none_or(|id| location.character.as_ref() == Some(id))
        }) {
            self.used_data_fields.insert(index);
            let valid_shape = data_shape_matches(expected, actual);
            if !valid_shape || expected != actual {
                self.difference(
                    location,
                    &rule.name,
                    &rule.reason,
                    Some(expected),
                    Some(actual),
                );
                if valid_shape {
                    self.differences.last_mut().unwrap().classification = DifferenceClass::Data;
                }
            }
            return;
        }
        if let Some((index, rule)) = self.rules.random.iter().enumerate().find(|(_, r)| {
            r.opcode == opcode
                && r.path == location.path
                && r.character
                    .as_ref()
                    .is_none_or(|id| location.character.as_ref() == Some(id))
        }) {
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
        if let Some((index, rule)) = self.rules.identities.iter().enumerate().find(|(_, rule)| {
            rule.opcode == opcode
                && rule.path == location.path
                && rule
                    .character
                    .as_ref()
                    .is_none_or(|id| location.character.as_ref() == Some(id))
        }) {
            self.used_identities.insert(index);
            let matches = match (expected, actual) {
                (Field::Unsigned(e), Field::Unsigned(a))
                    if u32::try_from(*e).is_ok() && u32::try_from(*a).is_ok() =>
                {
                    !self
                        .identities
                        .get(&(index, location.character.clone(), *e))
                        .is_some_and(|mapped| mapped != a)
                        && !self
                            .reverse_identities
                            .get(&(index, location.character.clone(), *a))
                            .is_some_and(|mapped| mapped != e)
                }
                _ => false,
            };
            if matches {
                if let (Field::Unsigned(e), Field::Unsigned(a)) = (expected, actual) {
                    self.identities
                        .insert((index, location.character.clone(), *e), *a);
                    self.reverse_identities
                        .insert((index, location.character.clone(), *a), *e);
                }
            } else {
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
        if let Some((index, rule)) = self.rules.clocks.iter().enumerate().find(|(_, rule)| {
            rule.opcode == opcode
                && rule.path == location.path
                && rule
                    .character
                    .as_ref()
                    .is_none_or(|id| location.character.as_ref() == Some(id))
        }) {
            self.used_clocks.insert(index);
            let matches = if let (Field::Unsigned(e), Field::Unsigned(a)) = (expected, actual) {
                match (u32::try_from(*e), u32::try_from(*a)) {
                    (Ok(e), Ok(a)) => {
                        let (eo, ao) = *self
                            .clock_origins
                            .entry((index, location.character.clone()))
                            .or_insert((e, a));
                        u64::from(e.wrapping_sub(eo).abs_diff(a.wrapping_sub(ao)))
                            <= rule.tolerance_ms
                    }
                    _ => false,
                }
            } else {
                false
            };
            if !matches {
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
                let keyed = e.iter().chain(a).any(
                    |field| matches!(field, Field::Object(fields) if subject(fields).is_some()),
                );
                let pairs = if e.len() == a.len() && !keyed {
                    (0..e.len()).map(|i| (Some(i), Some(i))).collect()
                } else {
                    align(e.len(), a.len(), |i, j| {
                        let matches = match (&e[i], &a[j]) {
                            (Field::Object(e), Field::Object(a)) => {
                                match (subject(e), subject(a)) {
                                    (Some(e), Some(a)) => self.guid_matches(e, a),
                                    _ => e == a,
                                }
                            }
                            (e, a) => e == a,
                        };
                        matches.then_some(usize::from(e[i] == a[j]))
                    })
                };
                for (ei, ai) in pairs {
                    let location = location.child(ei.or(ai).unwrap().to_string());
                    match (ei, ai) {
                        (Some(i), Some(j)) => self.field(opcode, &location, &e[i], &a[j]),
                        _ => self.difference(
                            &location,
                            "list_shape",
                            "list item is missing or extra",
                            ei.map(|i| &e[i]),
                            ai.map(|i| &a[i]),
                        ),
                    }
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
        if difference.classification != DifferenceClass::Rule {
            return false;
        }
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

fn contains_guid(field: &Field, guid: u64) -> bool {
    match field {
        Field::Guid(value) => *value == guid,
        Field::List(values) => values.iter().any(|field| contains_guid(field, guid)),
        Field::Object(values) => values.values().any(|field| contains_guid(field, guid)),
        _ => false,
    }
}

fn scoped_trace(scenario: &Scenario, trace: &Trace) -> Result<Trace> {
    let Some(scope) = &scenario.rules.character_scope else {
        return Ok(trace.clone());
    };
    ensure!(
        trace.header.character_guid != 0 || !trace.header.characters.is_empty(),
        "Character-scoped rules require the recorded Character GUID"
    );
    let mut result = trace.clone();
    result.packets.clear();
    for packet in &trace.packets {
        let character_guid = packet
            .character
            .as_ref()
            .and_then(|id| trace.header.characters.get(id))
            .copied()
            .unwrap_or(trace.header.character_guid);
        let in_scope = |guid: u64| {
            guid == character_guid
                || (scope.all_characters
                    && trace.header.characters.values().any(|value| *value == guid))
        };
        let declared_stream = scenario
            .steps
            .iter()
            .find(|step| step.id == packet.checkpoint)
            .map(|step| {
                step.checkpoint
                    .for_character(packet.character.as_deref().unwrap_or(""))
                    .map_or_else(
                        || step.checkpoint.stream(&step.id, packet.opcode),
                        |capture| capture.stream(&step.id, packet.opcode),
                    )
            });
        if packet.stream == Stream::Opening
            || packet.stream == Stream::Malformed
            || declared_stream.as_ref() != Some(&packet.stream)
        {
            result.packets.push(packet.clone());
            continue;
        }
        if let Some(subjects) = scope
            .packets
            .iter()
            .find(|subjects| subjects.opcode == packet.opcode)
        {
            let fields: Vec<_> = subjects
                .paths
                .iter()
                .filter_map(|path| field_at(&packet.fields, path))
                .collect();
            if !fields.is_empty()
                && !fields.iter().any(|field| {
                    trace
                        .header
                        .characters
                        .values()
                        .any(|guid| scope.all_characters && contains_guid(field, *guid))
                        || contains_guid(field, character_guid)
                })
            {
                continue;
            }
        }
        if packet.opcode == 0x00a9 {
            if let Some(Field::List(objects)) = packet.fields.get("objects") {
                for object in objects {
                    let mut object = object.clone();
                    if let Field::Object(fields) = &mut object {
                        if let Some(Field::Guid(guid)) = fields.get("guid") {
                            if !in_scope(*guid) {
                                continue;
                            }
                        } else if let Some(Field::List(guids)) = fields.get_mut("guids") {
                            guids.retain(
                                |guid| !matches!(guid, Field::Guid(value) if !in_scope(*value)),
                            );
                            if guids.is_empty() {
                                continue;
                            }
                        }
                        if scope.creates_only {
                            if !matches!(fields.get("kind"), Some(Field::Unsigned(2 | 3))) {
                                continue;
                            }
                            fields.retain(|name, _| name == "guid" || name == "kind");
                            result.packets.push(object_packet(packet, object));
                            continue;
                        }
                        if fields.get("kind") == Some(&Field::Unsigned(0)) {
                            if let Some(Field::Object(values)) = fields.get("values") {
                                if !values.is_empty() {
                                    for (name, value) in values {
                                        let mut single = fields.clone();
                                        single.insert(
                                            "values".into(),
                                            Field::Object(BTreeMap::from([(
                                                name.clone(),
                                                value.clone(),
                                            )])),
                                        );
                                        result
                                            .packets
                                            .push(object_packet(packet, Field::Object(single)));
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                    result.packets.push(object_packet(packet, object));
                }
                continue;
            }
        }
        result.packets.push(packet.clone());
    }
    Ok(result)
}

fn object_packet(packet: &RecordedPacket, object: Field) -> RecordedPacket {
    let mut packet = packet.clone();
    packet
        .fields
        .insert("objects".into(), Field::List(vec![object]));
    packet
}

fn update_field(packet: &RecordedPacket) -> Option<String> {
    if packet.opcode != 0x00a9 {
        return None;
    }
    let Field::List(objects) = packet.fields.get("objects")? else {
        return None;
    };
    let [Field::Object(object)] = objects.as_slice() else {
        return None;
    };
    if object.get("kind") != Some(&Field::Unsigned(0)) {
        return None;
    }
    let Field::Object(values) = object.get("values")? else {
        return None;
    };
    (values.len() == 1).then(|| values.keys().next().unwrap().clone())
}

fn field_at<'a>(fields: &'a BTreeMap<String, Field>, path: &[String]) -> Option<&'a Field> {
    let (first, rest) = path.split_first()?;
    let mut value = fields.get(first)?;
    for part in rest {
        value = match value {
            Field::Object(fields) => fields.get(part)?,
            Field::List(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(value)
}

impl PacketTimingRule {
    fn applies(&self, packet: &RecordedPacket) -> bool {
        self.opcode == packet.opcode
            && self
                .character
                .as_ref()
                .is_none_or(|id| packet.character.as_ref() == Some(id))
            && (self.path.is_empty() || field_at(&packet.fields, &self.path).is_some())
            && (!self.values_only
                || field_at(
                    &packet.fields,
                    &[self.path[0].clone(), self.path[1].clone(), "kind".into()],
                ) == Some(&Field::Unsigned(0)))
    }
}

pub(crate) fn validate_rule_samples(scenario: &Scenario, trace: &Trace) -> Result<()> {
    let trace = scoped_trace(scenario, trace)?;
    for (name, reason, character, opcode, path, unchecked) in scenario
        .rules
        .identities
        .iter()
        .map(|rule| {
            (
                &rule.name,
                &rule.reason,
                &rule.character,
                rule.opcode,
                &rule.path,
                "identity consistency is unchecked: single sample",
            )
        })
        .chain(scenario.rules.clocks.iter().map(|rule| {
            (
                &rule.name,
                &rule.reason,
                &rule.character,
                rule.opcode,
                &rule.path,
                "clock consistency is unchecked: single sample",
            )
        }))
    {
        let samples = trace
            .packets
            .iter()
            .filter(|packet| {
                packet.opcode == opcode
                    && character
                        .as_ref()
                        .is_none_or(|id| packet.character.as_ref() == Some(id))
                    && scenario.steps.iter().any(|step| {
                        step.id == packet.checkpoint
                            && step
                                .checkpoint
                                .for_character(packet.character.as_deref().unwrap_or(""))
                                .map_or_else(
                                    || step.checkpoint.stream(&step.id, opcode),
                                    |capture| capture.stream(&step.id, opcode),
                                )
                                == packet.stream
                            && !matches!(
                                packet.stream,
                                Stream::Opening | Stream::Unexpected | Stream::Malformed
                            )
                    })
                    && field_at(&packet.fields, path).is_some()
            })
            .take(2)
            .count();
        ensure!(
            samples != 1 || reason.to_ascii_lowercase().contains(unchecked),
            "rule {name} has one sample; its reason must state '{unchecked}'"
        );
    }
    Ok(())
}

type Streams<'a> =
    BTreeMap<(String, Option<String>, Stream, Option<String>), Vec<&'a RecordedPacket>>;

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
            if packet.stream == Stream::Opening {
                if packet.checkpoint != "opening" {
                    let allowed = scenario.steps.iter().any(|step| {
                        step.id == packet.checkpoint
                            && step.character == packet.character
                            && matches!(
                                step.action.as_ref(),
                                Some(SessionAction::Connect | SessionAction::Reconnect)
                            )
                    });
                    if !allowed {
                        self.difference(
                            &Location {
                                character: packet.character.clone(),
                                checkpoint: packet.checkpoint.clone(),
                                stream: Some(Stream::Opening),
                                packet_index: Some(index),
                                path: Vec::new(),
                                opcode: Some(packet.opcode),
                            },
                            "unexpected_opening",
                            "only a Character connection can have opening packets during a step",
                            None,
                            Some(&Field::Unsigned(packet.opcode.into())),
                        );
                    }
                }
                continue;
            }
            let location = Location {
                character: packet.character.clone(),
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
            let stream = step
                .checkpoint
                .for_character(packet.character.as_deref().unwrap_or(""))
                .map_or_else(
                    || step.checkpoint.stream(&step.id, packet.opcode),
                    |capture| capture.stream(&step.id, packet.opcode),
                );
            if stream == Stream::Unexpected {
                let rule = self
                    .rules
                    .unexpected_packets
                    .iter()
                    .filter(|r| {
                        r.checkpoint == packet.checkpoint
                            && r.opcode == packet.opcode
                            && r.character
                                .as_ref()
                                .is_none_or(|id| packet.character.as_ref() == Some(id))
                    })
                    .max_by_key(|rule| rule.character.is_some());
                if packet.stream == Stream::Malformed {
                    self.difference(
                        &location,
                        "packet_decode",
                        "packet could not be decoded",
                        e,
                        a,
                    );
                } else if !rule.is_some_and(|rule| {
                    rule.subject_guid_high.map_or(true, |high| {
                        subject(&packet.fields).is_some_and(|guid| guid >> 48 == u64::from(high))
                    })
                }) {
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
            if packet.opcode == 0x02f4
                && packet.fields.get("weather_type") == Some(&Field::Unsigned(0))
            {
                for (name, required) in
                    [("grade", Field::Float(0.0)), ("sound", Field::Unsigned(0))]
                {
                    if packet.fields.get(name) != Some(&required) {
                        self.difference(
                            &location.child(name.into()),
                            "clear_weather",
                            "weather type 0 requires grade and sound 0",
                            Some(&required),
                            packet.fields.get(name),
                        );
                    }
                }
            }
            streams
                .entry((
                    packet.checkpoint.clone(),
                    packet.character.clone(),
                    stream,
                    scenario
                        .rules
                        .character_scope
                        .as_ref()
                        .and_then(|_| update_field(packet)),
                ))
                .or_insert_with(Vec::new)
                .push(packet);
        }
        if check_replies {
            for step in &scenario.steps {
                let captures: Vec<_> = if scenario.schema_version == 4 {
                    step.checkpoint
                        .characters
                        .iter()
                        .map(|capture| {
                            (
                                Some(capture.character.as_str()),
                                capture.replies.as_slice(),
                                capture.expect.as_slice(),
                            )
                        })
                        .collect()
                } else {
                    vec![(
                        None,
                        step.checkpoint.replies.as_slice(),
                        step.checkpoint.expect.as_slice(),
                    )]
                };
                for (character, replies, assertions) in captures {
                    if let Some(capture) =
                        character.and_then(|id| step.checkpoint.for_character(id))
                    {
                        for target in &capture.creates {
                            let target_guid = trace.header.characters[target];
                            let found = trace.packets.iter().any(|packet| {
                            packet.character.as_deref() == character
                                && packet.checkpoint == step.id
                                && packet.opcode == 0x00a9
                                && matches!(packet.fields.get("objects"), Some(Field::List(objects)) if objects.iter().any(|object| matches!(object, Field::Object(fields) if matches!(fields.get("kind"), Some(Field::Unsigned(2 | 3))) && fields.get("guid") == Some(&Field::Guid(target_guid)))))
                        });
                            if !found {
                                self.difference(
                                    &Location {
                                        character: character.map(str::to_owned),
                                        checkpoint: step.id.clone(),
                                        stream: Some(Stream::Broadcast("objects".into())),
                                        packet_index: None,
                                        path: vec!["objects".into()],
                                        opcode: Some(0x00a9),
                                    },
                                    "checkpoint_create",
                                    "Character must receive the named Character's CREATE update",
                                    Some(&Field::Guid(target_guid)),
                                    None,
                                );
                            }
                        }
                    }
                    let received: Vec<_> = trace
                        .packets
                        .iter()
                        .filter(|packet| {
                            packet.checkpoint == step.id
                                && packet.character.as_deref() == character
                                && replies.contains(&packet.opcode)
                        })
                        .map(|packet| packet.opcode)
                        .collect();
                    for assertion in assertions {
                        let count = trace
                            .packets
                            .iter()
                            .filter(|packet| {
                                packet.checkpoint == step.id
                                    && packet.character.as_deref() == character
                                    && packet.opcode == assertion.opcode
                                    && field_at(&packet.fields, &assertion.path)
                                        == Some(&assertion.value)
                            })
                            .count();
                        if count != assertion.count {
                            self.difference(
                                &Location {
                                    character: character.map(str::to_owned),
                                    checkpoint: step.id.clone(),
                                    stream: None,
                                    packet_index: None,
                                    path: assertion.path.clone(),
                                    opcode: Some(assertion.opcode),
                                },
                                "checkpoint_assertion",
                                "matching field occurrences must meet the scenario assertion",
                                Some(&Field::Unsigned(assertion.count as u64)),
                                Some(&Field::Unsigned(count as u64)),
                            );
                        }
                    }
                    if received != replies {
                        let location = Location {
                            character: character.map(str::to_owned),
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
                            Some(&opcodes(replies)),
                            Some(&opcodes(&received)),
                        );
                    }
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
                        character: None,
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
        identities: BTreeMap::new(),
        reverse_identities: BTreeMap::new(),
        used_identities: BTreeSet::new(),
        clock_origins: BTreeMap::new(),
        used_clocks: BTreeSet::new(),
        used_data_fields: BTreeSet::new(),
        unmatched_opcodes: BTreeSet::new(),
        unmatched_fields: BTreeSet::new(),
        used_timing: BTreeSet::new(),
        differences: Vec::new(),
    }
}

/// Checks schema 3 replies and received opcodes without a Golden Trace.
pub fn check_observations(scenario: &Scenario, observations: &[Observation]) -> Result<Comparison> {
    check_character_observations(scenario, observations, 0)
}

pub fn check_character_observations(
    scenario: &Scenario,
    observations: &[Observation],
    character_guid: u64,
) -> Result<Comparison> {
    scenario.validate()?;
    ensure!(
        scenario.schema_version == 3,
        "single-Character check requires schema 3"
    );
    let header = Header {
        schema_version: scenario.schema_version,
        character_guid,
        characters: BTreeMap::new(),
        scenario: scenario.id.clone(),
        behavior: scenario.behavior.clone(),
        scenario_sha256: crate::trace::Sha256Digest::of(b""),
        recorded_at_unix_ms: 0,
        origin: crate::trace::Origin::Candidate {
            adapter: "observations".into(),
        },
    };
    let trace = Trace::record(header, observations)?;
    let trace = scoped_trace(scenario, &trace)?;
    let mut comparator = comparator(&scenario.rules, (0, 0));
    comparator.packets(scenario, &trace, false, true);
    comparator.finish(&[])
}

pub fn check_multi_observations(
    scenario: &Scenario,
    observations: &[Observation],
    characters: &BTreeMap<String, u64>,
) -> Result<Comparison> {
    scenario.validate()?;
    ensure!(
        scenario.schema_version == 4,
        "named Character check requires schema 4"
    );
    let header = Header {
        schema_version: 4,
        character_guid: 0,
        characters: characters.clone(),
        scenario: scenario.id.clone(),
        behavior: scenario.behavior.clone(),
        scenario_sha256: crate::trace::Sha256Digest::of(b""),
        recorded_at_unix_ms: 0,
        origin: crate::trace::Origin::Candidate {
            adapter: "observations".into(),
        },
    };
    let trace = scoped_trace(scenario, &Trace::record(header, observations)?)?;
    let mut comparator = comparator(&scenario.rules, (0, 0));
    comparator.packets(scenario, &trace, false, true);
    comparator.finish(&[])
}

/// Deviations annotate rule differences; they never make is_match return true.
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
        || expected.header.schema_version != actual.header.schema_version
    {
        comparator.difference(
            &Location {
                character: None,
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
    let expected = scoped_trace(scenario, expected)?;
    let actual = scoped_trace(scenario, actual)?;
    let expected_streams = comparator.packets(scenario, &expected, true, true);
    let actual_streams = comparator.packets(scenario, &actual, false, false);
    if expected.header.character_guid != 0 && actual.header.character_guid != 0 {
        comparator
            .expected_guids
            .insert(expected.header.character_guid, actual.header.character_guid);
        comparator
            .actual_guids
            .insert(actual.header.character_guid, expected.header.character_guid);
    }
    if scenario.schema_version == 4 {
        ensure!(
            expected
                .header
                .characters
                .keys()
                .eq(actual.header.characters.keys()),
            "candidate Character set differs"
        );
        for (id, expected_guid) in &expected.header.characters {
            let actual_guid = actual.header.characters[id];
            ensure!(
                comparator
                    .expected_guids
                    .insert(*expected_guid, actual_guid)
                    .is_none_or(|guid| guid == actual_guid),
                "conflicting Character GUID mapping"
            );
            ensure!(
                comparator
                    .actual_guids
                    .insert(actual_guid, *expected_guid)
                    .is_none_or(|guid| guid == *expected_guid),
                "conflicting Character GUID mapping"
            );
        }
    }
    let mut keys: Vec<_> = expected_streams
        .keys()
        .chain(actual_streams.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    keys.sort_by_key(|key| scenario.steps.iter().position(|step| step.id == key.0));
    for key in keys {
        let e = expected_streams
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let a = actual_streams
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (ei, ai) in align(e.len(), a.len(), |i, j| {
            comparator
                .packet_matches(e[i], a[j])
                .then(|| equal_fields(&e[i].fields, &a[j].fields))
        }) {
            let ep = ei.map(|i| e[i]);
            let ap = ai.map(|i| a[i]);
            let packet = ep.or(ap).unwrap();
            let location = Location {
                character: key.1.clone(),
                checkpoint: key.0.clone(),
                stream: Some(key.2.clone()),
                packet_index: ei.or(ai),
                path: Vec::new(),
                opcode: Some(packet.opcode),
            };
            let mut update_location = location.clone();
            if let Some(field) = &key.3 {
                update_location.path =
                    vec!["objects".into(), "0".into(), "values".into(), field.clone()];
            }
            match (ep, ap) {
                (Some(e), Some(a))
                    if e.stream != Stream::Malformed && a.stream != Stream::Malformed =>
                {
                    if let Some((index, rule)) = rules
                        .packet_timing
                        .iter()
                        .enumerate()
                        .filter(|(_, rule)| {
                            if rule.values_only {
                                rule.applies(e) && rule.applies(a)
                            } else {
                                rule.applies(e) || rule.applies(a)
                            }
                        })
                        .max_by_key(|(_, rule)| rule.path.len())
                    {
                        comparator.used_timing.insert(index);
                        if !matches!((e.received_after_ms, a.received_after_ms), (Some(e), Some(a)) if e.abs_diff(a) <= rule.tolerance_ms)
                        {
                            comparator.difference(
                                &update_location,
                                &rule.name,
                                &rule.reason,
                                e.received_after_ms.map(Field::TimeMs).as_ref(),
                                a.received_after_ms.map(Field::TimeMs).as_ref(),
                            );
                        }
                    }
                    comparator.fields(e.opcode, &location, &e.fields, &a.fields);
                }
                (Some(_), Some(_)) => (),
                _ => {
                    comparator
                        .unmatched_opcodes
                        .insert((packet.character.clone(), packet.opcode));
                    for (opcode, path) in rules
                        .identities
                        .iter()
                        .chain(&rules.data_fields)
                        .map(|rule| (rule.opcode, &rule.path))
                        .chain(rules.clocks.iter().map(|rule| (rule.opcode, &rule.path)))
                        .chain(rules.random.iter().map(|rule| (rule.opcode, &rule.path)))
                    {
                        if opcode == packet.opcode && field_at(&packet.fields, path).is_some() {
                            comparator.unmatched_fields.insert((
                                packet.character.clone(),
                                opcode,
                                path.clone(),
                            ));
                        }
                    }
                    for rule in &rules.packet_timing {
                        if rule.applies(packet) && !rule.path.is_empty() {
                            comparator.unmatched_fields.insert((
                                packet.character.clone(),
                                rule.opcode,
                                rule.path.clone(),
                            ));
                        }
                    }
                    comparator.difference(
                        &update_location,
                        if ep.is_some() {
                            "missing_packet"
                        } else {
                            "extra_packet"
                        },
                        "packets must align by opcode and mapped subject GUID in stream order",
                        ep.map(|p| Field::Unsigned(p.opcode.into())).as_ref(),
                        ap.map(|p| Field::Unsigned(p.opcode.into())).as_ref(),
                    );
                }
            }
        }
    }
    for (index, rule) in rules.random.iter().enumerate() {
        if !comparator.used_random.contains(&index)
            && !comparator.unmatched_field(&rule.character, rule.opcode, &rule.path)
        {
            comparator.difference(
                &Location {
                    character: rule.character.clone(),
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
    for (index, rule) in rules.identities.iter().enumerate() {
        if !comparator.used_identities.contains(&index)
            && !comparator.unmatched_field(&rule.character, rule.opcode, &rule.path)
        {
            comparator.difference(
                &Location {
                    character: rule.character.clone(),
                    checkpoint: String::new(),
                    stream: None,
                    packet_index: None,
                    path: rule.path.clone(),
                    opcode: Some(rule.opcode),
                },
                &rule.name,
                "identity rule did not match a compared field",
                None,
                None,
            );
        }
    }
    for (index, rule) in rules.clocks.iter().enumerate() {
        if !comparator.used_clocks.contains(&index)
            && !comparator.unmatched_field(&rule.character, rule.opcode, &rule.path)
        {
            comparator.difference(
                &Location {
                    character: rule.character.clone(),
                    checkpoint: String::new(),
                    stream: None,
                    packet_index: None,
                    path: rule.path.clone(),
                    opcode: Some(rule.opcode),
                },
                &rule.name,
                "clock rule did not match a compared field",
                None,
                None,
            );
        }
    }
    for (index, rule) in rules.packet_timing.iter().enumerate() {
        if !comparator.used_timing.contains(&index)
            && !(if rule.path.is_empty() {
                comparator.unmatched_opcode(&rule.character, rule.opcode)
            } else {
                comparator.unmatched_field(&rule.character, rule.opcode, &rule.path)
            })
        {
            comparator.difference(
                &Location {
                    character: rule.character.clone(),
                    checkpoint: String::new(),
                    stream: None,
                    packet_index: None,
                    path: rule.path.clone(),
                    opcode: Some(rule.opcode),
                },
                &rule.name,
                "packet timing rule did not match a compared packet",
                None,
                None,
            );
        }
    }
    for (index, rule) in rules.data_fields.iter().enumerate() {
        if !comparator.used_data_fields.contains(&index)
            && !comparator.unmatched_field(&rule.character, rule.opcode, &rule.path)
        {
            comparator.difference(
                &Location {
                    character: rule.character.clone(),
                    checkpoint: String::new(),
                    stream: None,
                    packet_index: None,
                    path: rule.path.clone(),
                    opcode: Some(rule.opcode),
                },
                &rule.name,
                "data field rule did not match a compared field",
                None,
                None,
            );
        }
    }
    comparator.finish(deviations)
}

/// Schema 3 callers supply decoded observations and capture time without server provenance.
pub fn compare_observations(
    expected: &GoldenTrace,
    observations: &[Observation],
    recorded_at_unix_ms: u64,
    deviations: &[DeliberateDeviation],
) -> Result<Comparison> {
    ensure!(
        expected.scenario().schema_version == 3,
        "single-Character comparison requires schema 3"
    );
    let mut header = expected.trace().header.clone();
    header.origin = crate::trace::Origin::Candidate {
        adapter: "observations".into(),
    };
    header.recorded_at_unix_ms = recorded_at_unix_ms;
    header.character_guid = 0;
    compare(expected, &Trace::record(header, observations)?, deviations)
}

/// Supplies the schema 3 actor identity used by Character scope rules.
pub fn compare_character_observations(
    expected: &GoldenTrace,
    observations: &[Observation],
    character_guid: u64,
    recorded_at_unix_ms: u64,
    deviations: &[DeliberateDeviation],
) -> Result<Comparison> {
    ensure!(
        expected.scenario().schema_version == 3,
        "single-Character comparison requires schema 3"
    );
    let mut header = expected.trace().header.clone();
    header.character_guid = character_guid;
    header.origin = crate::trace::Origin::Candidate {
        adapter: "observations".into(),
    };
    header.recorded_at_unix_ms = recorded_at_unix_ms;
    compare(expected, &Trace::record(header, observations)?, deviations)
}

pub fn compare_multi_observations(
    expected: &GoldenTrace,
    observations: &[Observation],
    characters: &BTreeMap<String, u64>,
    recorded_at_unix_ms: u64,
    deviations: &[DeliberateDeviation],
) -> Result<Comparison> {
    ensure!(
        expected.scenario().schema_version == 4,
        "named Character comparison requires schema 4"
    );
    let mut header = expected.trace().header.clone();
    header.character_guid = 0;
    header.characters = characters.clone();
    header.origin = crate::trace::Origin::Candidate {
        adapter: "observations".into(),
    };
    header.recorded_at_unix_ms = recorded_at_unix_ms;
    compare(expected, &Trace::record(header, observations)?, deviations)
}

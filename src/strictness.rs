use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::compare::{
    compare, compare_with_rules, scoped_trace, EquivalenceRules, PacketTimingRule,
};
use crate::scenario::{Scenario, SessionAction, Stream};
use crate::trace::{GoldenTrace, RecordedField as Field, RecordedPacket, Sha256Digest, Trace};

#[derive(Debug, Serialize)]
pub struct StrictnessReport {
    pub schema_version: u32,
    pub scenario: String,
    pub behavior: String,
    pub mutations: usize,
    pub detected: usize,
    pub detected_mutations: Vec<String>,
    pub excused_by_rule: BTreeMap<String, usize>,
    pub excused_mutations: BTreeMap<String, Vec<String>>,
    pub rule_intents: BTreeMap<String, String>,
    pub unexplained_survivors: Vec<Survivor>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Survivor {
    pub id: String,
    pub description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Allowlist {
    schema_version: u32,
    entries: Vec<AllowlistEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowlistEntry {
    scenario: String,
    id: String,
    reason: String,
}

pub fn check_allowlist(reports: &[StrictnessReport], json: &str) -> Result<()> {
    let allowlist: Allowlist = serde_json::from_str(json).context("strictness allowlist")?;
    ensure!(
        allowlist.schema_version == 1,
        "unsupported strictness allowlist schema"
    );
    let scenarios: BTreeSet<_> = reports
        .iter()
        .map(|report| report.scenario.as_str())
        .collect();
    let mut listed = BTreeSet::new();
    for entry in allowlist.entries {
        ensure!(
            scenarios.contains(entry.scenario.as_str()),
            "allowlist names an unknown scenario: {}",
            entry.scenario
        );
        ensure!(
            !entry.reason.trim().is_empty(),
            "allowlist entry needs a reason: {}",
            entry.id
        );
        ensure!(
            listed.insert((entry.scenario, entry.id)),
            "duplicate strictness allowlist entry"
        );
    }
    let actual: BTreeSet<_> = reports
        .iter()
        .flat_map(|report| {
            report
                .unexplained_survivors
                .iter()
                .map(move |survivor| (report.scenario.clone(), survivor.id.clone()))
        })
        .collect();
    let added: Vec<_> = actual.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&actual).collect();
    ensure!(
        added.is_empty() && stale.is_empty(),
        "strictness allowlist mismatch; new survivors: {added:?}; stale entries: {stale:?}"
    );
    Ok(())
}

enum Change {
    Field(Vec<String>),
    Packet,
    Timing,
}

struct Analyzer<'a> {
    golden: &'a GoldenTrace,
    report: StrictnessReport,
    without_rule: BTreeMap<String, (EquivalenceRules, usize)>,
    seen: BTreeSet<[u8; 32]>,
    scoped: Trace,
}

impl Analyzer<'_> {
    fn excuse(&mut self, name: String, id: String) {
        *self.report.excused_by_rule.entry(name.clone()).or_default() += 1;
        self.report
            .excused_mutations
            .entry(name)
            .or_default()
            .push(id);
    }

    fn run_field(
        &mut self,
        id: String,
        description: String,
        index: usize,
        packet: &RecordedPacket,
        changed: RecordedPacket,
        path: Vec<String>,
    ) -> Result<()> {
        if changed == *packet {
            return Ok(());
        }
        let change = Change::Field(path);
        if scope_intends(self.golden, packet, &change)
            && scoped_packet(self.golden, changed.clone())?
                == scoped_packet(self.golden, packet.clone())?
        {
            let mut hash = Sha256::new();
            hash.update(b"packet\0");
            hash.update(index.to_le_bytes());
            hash.update(serde_json::to_vec(&changed)?);
            if self.seen.insert(hash.finalize().into()) {
                self.report.mutations += 1;
                self.excuse(
                    self.golden
                        .scenario()
                        .rules
                        .character_scope
                        .as_ref()
                        .unwrap()
                        .name
                        .clone(),
                    id,
                );
            }
            return Ok(());
        }
        let mut candidate = self.golden.trace().clone();
        candidate.packets[index] = changed;
        self.run(id, description, candidate, packet, change)
    }

    fn run(
        &mut self,
        id: String,
        description: String,
        candidate: Trace,
        packet: &RecordedPacket,
        change: Change,
    ) -> Result<()> {
        let source_index = match change {
            Change::Field(_) | Change::Timing => self
                .golden
                .trace()
                .packets
                .iter()
                .position(|source| std::ptr::eq(source, packet)),
            Change::Packet => None,
        };
        let digest: [u8; 32] = if let Some(index) = source_index {
            if candidate.packets[index] == *packet {
                return Ok(());
            }
            let mut hash = Sha256::new();
            hash.update(b"packet\0");
            hash.update(index.to_le_bytes());
            hash.update(serde_json::to_vec(&candidate.packets[index])?);
            hash.finalize().into()
        } else {
            if candidate == *self.golden.trace() {
                return Ok(());
            }
            let mut hash = Sha256::new();
            hash.update(b"trace\0");
            hash.update(serde_json::to_vec(&candidate)?);
            hash.finalize().into()
        };
        if !self.seen.insert(digest) {
            return Ok(());
        }
        self.report.mutations += 1;
        if matches!(change, Change::Timing) && scope_intends(self.golden, packet, &change) {
            let index = source_index.expect("timing mutation uses a recorded packet");
            if scoped_packet(self.golden, candidate.packets[index].clone())?
                == scoped_packet(self.golden, packet.clone())?
            {
                self.excuse(
                    self.golden
                        .scenario()
                        .rules
                        .character_scope
                        .as_ref()
                        .unwrap()
                        .name
                        .clone(),
                    id,
                );
                return Ok(());
            }
        }
        if matches!(change, Change::Packet)
            && (scope_intends(self.golden, packet, &change)
                || self
                    .golden
                    .trace()
                    .packets
                    .iter()
                    .zip(&candidate.packets)
                    .any(|(source, changed)| {
                        source != changed && scope_intends(self.golden, source, &change)
                    }))
            && scoped_trace(
                self.golden.scenario(),
                &candidate,
                &self.golden.scenario().rules,
            )? == self.scoped
        {
            self.excuse(
                self.golden
                    .scenario()
                    .rules
                    .character_scope
                    .as_ref()
                    .unwrap()
                    .name
                    .clone(),
                id,
            );
            return Ok(());
        }
        let comparison = compare(self.golden, &candidate, &[])?;
        if !comparison.is_match() {
            self.report.detected += 1;
            self.report.detected_mutations.push(id);
            return Ok(());
        }
        for name in candidate_rules(self.golden, packet, &change) {
            if matches!(change, Change::Timing) {
                self.excuse(name, id);
                return Ok(());
            }
            if !self.without_rule.contains_key(&name) {
                let mut rules = self.golden.scenario().rules.clone();
                remove_rule(&mut rules, &name);
                let baseline = compare_with_rules(self.golden, self.golden.trace(), &rules, &[])?
                    .rule_difference_count();
                self.without_rule.insert(name.clone(), (rules, baseline));
            }
            let (rules, baseline) = &self.without_rule[&name];
            if compare_with_rules(self.golden, &candidate, rules, &[])?.rule_difference_count()
                > *baseline
            {
                self.excuse(name, id);
                return Ok(());
            }
        }
        self.report
            .unexplained_survivors
            .push(Survivor { id, description });
        Ok(())
    }
}

fn scoped_packet(golden: &GoldenTrace, packet: RecordedPacket) -> Result<Trace> {
    scoped_trace(
        golden.scenario(),
        &Trace {
            header: golden.trace().header.clone(),
            packets: vec![packet],
        },
        &golden.scenario().rules,
    )
}

fn remove_rule(rules: &mut EquivalenceRules, name: &str) {
    rules.random.retain(|rule| rule.name != name);
    rules.identities.retain(|rule| rule.name != name);
    rules.clocks.retain(|rule| rule.name != name);
    rules.data_fields.retain(|rule| rule.name != name);
    rules.packet_timing.retain(|rule| rule.name != name);
    rules.unexpected_packets.retain(|rule| rule.name != name);
    if rules
        .character_scope
        .as_ref()
        .is_some_and(|rule| rule.name == name)
    {
        rules.character_scope = None;
    }
}

fn rule_character_applies(character: &Option<String>, packet: &RecordedPacket) -> bool {
    character
        .as_ref()
        .map_or(true, |id| packet.character.as_ref() == Some(id))
}

fn timing_applies(rule: &PacketTimingRule, packet: &RecordedPacket) -> bool {
    if rule.opcode != packet.opcode || !rule_character_applies(&rule.character, packet) {
        return false;
    }
    if rule.path.is_empty() {
        return true;
    }
    if at(&packet.fields, &rule.path).is_none() {
        return false;
    }
    if !rule.values_only {
        return true;
    }
    let Some(Field::List(objects)) = packet.fields.get("objects") else {
        return false;
    };
    matches!(rule.path.get(1).and_then(|index| index.parse::<usize>().ok()).and_then(|index| objects.get(index)), Some(Field::Object(fields)) if fields.get("kind") == Some(&Field::Unsigned(0)))
}

fn subject_path(fields: &BTreeMap<String, Field>) -> Option<Vec<String>> {
    for name in ["guid", "caster", "attacker"] {
        if matches!(fields.get(name), Some(Field::Guid(_))) {
            return Some(vec![name.into()]);
        }
    }
    if let Some(Field::List(objects)) = fields.get("objects") {
        if let [Field::Object(object)] = objects.as_slice() {
            let mut path = vec!["objects".into(), "0".into()];
            path.extend(subject_path(object)?);
            return Some(path);
        }
    }
    None
}

fn subject(packet: &RecordedPacket) -> Option<u64> {
    match at(&packet.fields, &subject_path(&packet.fields)?) {
        Some(Field::Guid(guid)) => Some(*guid),
        _ => None,
    }
}

fn observed_rule_subjects(
    golden: &GoldenTrace,
    checkpoint: &str,
    character: &Option<String>,
    opcode: u16,
) -> (BTreeSet<u16>, bool) {
    let mut highs = BTreeSet::new();
    let mut subjectless = false;
    for packet in golden.trace().packets.iter().filter(|packet| {
        packet.checkpoint == checkpoint
            && character
                .as_ref()
                .map_or(true, |id| packet.character.as_ref() == Some(id))
            && packet.opcode == opcode
            && packet.stream == Stream::Unexpected
    }) {
        if let Some(guid) = subject(packet) {
            highs.insert((guid >> 48) as u16);
        } else {
            subjectless = true;
        }
    }
    (highs, subjectless)
}

fn scope_intends(golden: &GoldenTrace, packet: &RecordedPacket, change: &Change) -> bool {
    let Some(scope) = &golden.scenario().rules.character_scope else {
        return false;
    };
    let character_guid = packet
        .character
        .as_ref()
        .and_then(|id| golden.trace().header.characters.get(id))
        .copied()
        .unwrap_or(golden.trace().header.character_guid);
    let in_scope = |guid| {
        guid == character_guid
            || (scope.all_characters
                && golden
                    .trace()
                    .header
                    .characters
                    .values()
                    .any(|value| *value == guid))
    };
    let ignored_object = |object: &Field| {
        let Field::Object(fields) = object else {
            return false;
        };
        if scope.creates_only && !matches!(fields.get("kind"), Some(Field::Unsigned(2 | 3))) {
            return true;
        }
        match fields.get("guid") {
            Some(Field::Guid(guid)) => {
                !in_scope(*guid) || (scope.creates_only && *guid == character_guid)
            }
            _ => {
                matches!(fields.get("guids"), Some(Field::List(guids)) if guids.iter().all(|guid| matches!(guid, Field::Guid(value) if !in_scope(*value))))
            }
        }
    };
    if packet.opcode == 0x00a9 {
        let Some(Field::List(objects)) = packet.fields.get("objects") else {
            return false;
        };
        if let Change::Field(path) = change {
            if path.first().map(String::as_str) == Some("objects") {
                let Some(index) = path.get(1).and_then(|index| index.parse::<usize>().ok()) else {
                    return false;
                };
                return objects.get(index).is_some_and(ignored_object);
            }
        }
        return objects.iter().all(ignored_object);
    }
    scope
        .packets
        .iter()
        .find(|entry| entry.opcode == packet.opcode)
        .is_some_and(|entry| {
            let subjects: Vec<_> = entry
                .paths
                .iter()
                .filter_map(|path| at(&packet.fields, path))
                .collect();
            !subjects.is_empty()
                && subjects.iter().all(|field| {
                    !golden
                        .trace()
                        .header
                        .characters
                        .values()
                        .any(|guid| scope.all_characters && field_contains_guid(field, *guid))
                        && !field_contains_guid(field, character_guid)
                })
        })
}

fn field_contains_guid(field: &Field, guid: u64) -> bool {
    match field {
        Field::Guid(value) => *value == guid,
        Field::Object(fields) => fields
            .values()
            .any(|field| field_contains_guid(field, guid)),
        Field::List(fields) => fields.iter().any(|field| field_contains_guid(field, guid)),
        _ => false,
    }
}

fn candidate_rules(golden: &GoldenTrace, packet: &RecordedPacket, change: &Change) -> Vec<String> {
    let rules = &golden.scenario().rules;
    let mut names = Vec::new();
    match change {
        Change::Field(path) => {
            let mut path = path.clone();
            if rules.character_scope.is_some()
                && packet.opcode == 0x00a9
                && path.first().map(String::as_str) == Some("objects")
                && path
                    .get(1)
                    .is_some_and(|index| index.parse::<usize>().is_ok())
            {
                path[1] = "0".into();
            }
            for rule in rules.identities.iter().chain(&rules.data_fields) {
                if rule.opcode == packet.opcode
                    && rule_character_applies(&rule.character, packet)
                    && path.starts_with(&rule.path)
                {
                    names.push(rule.name.clone());
                }
            }
            for rule in &rules.clocks {
                if rule.opcode == packet.opcode
                    && rule_character_applies(&rule.character, packet)
                    && path.starts_with(&rule.path)
                {
                    names.push(rule.name.clone());
                }
            }
            for rule in &rules.random {
                if rule.opcode == packet.opcode
                    && rule_character_applies(&rule.character, packet)
                    && path.starts_with(&rule.path)
                {
                    names.push(rule.name.clone());
                }
            }
        }
        Change::Packet => {
            if packet.stream == Stream::Unexpected {
                for rule in &rules.unexpected_packets {
                    if rule.opcode != packet.opcode
                        || rule.checkpoint != packet.checkpoint
                        || !rule_character_applies(&rule.character, packet)
                    {
                        continue;
                    }
                    let high = subject(packet).map(|guid| (guid >> 48) as u16);
                    let (observed, subjectless) = observed_rule_subjects(
                        golden,
                        &rule.checkpoint,
                        &rule.character,
                        rule.opcode,
                    );
                    let allowed = rule
                        .subject_guid_high
                        .map_or(observed, |high| BTreeSet::from([high]));
                    if high.map_or(rule.subject_guid_high.is_none() && subjectless, |high| {
                        allowed.contains(&high)
                    }) {
                        names.push(rule.name.clone());
                    }
                }
            }
        }
        Change::Timing => {
            if let Some(rule) = rules
                .packet_timing
                .iter()
                .filter(|rule| timing_applies(rule, packet))
                .max_by_key(|rule| rule.path.len())
            {
                names.push(rule.name.clone());
            }
        }
    }
    if scope_intends(golden, packet, change) {
        if let Some(scope) = &rules.character_scope {
            names.push(scope.name.clone());
        }
    }
    names
}

fn rule_intents(golden: &GoldenTrace) -> BTreeMap<String, String> {
    let rules = &golden.scenario().rules;
    let mut intents = BTreeMap::new();
    for rule in rules.identities.iter().chain(&rules.data_fields) {
        intents.insert(
            rule.name.clone(),
            format!(
                "opcode {}, field {}, Character {}",
                rule.opcode,
                rule.path.join("."),
                rule.character.as_deref().unwrap_or("any")
            ),
        );
    }
    for rule in &rules.clocks {
        intents.insert(
            rule.name.clone(),
            format!(
                "opcode {}, field {}, Character {}",
                rule.opcode,
                rule.path.join("."),
                rule.character.as_deref().unwrap_or("any")
            ),
        );
    }
    for rule in &rules.random {
        intents.insert(
            rule.name.clone(),
            format!(
                "opcode {}, field {}, Character {}",
                rule.opcode,
                rule.path.join("."),
                rule.character.as_deref().unwrap_or("any")
            ),
        );
    }
    for rule in &rules.packet_timing {
        intents.insert(
            rule.name.clone(),
            format!(
                "opcode {}, arrival within {} ms, field {}, Character {}, VALUES only {}",
                rule.opcode,
                rule.tolerance_ms,
                if rule.path.is_empty() {
                    "any".into()
                } else {
                    rule.path.join(".")
                },
                rule.character.as_deref().unwrap_or("any"),
                rule.values_only
            ),
        );
    }
    for rule in &rules.unexpected_packets {
        let (observed, subjectless) =
            observed_rule_subjects(golden, &rule.checkpoint, &rule.character, rule.opcode);
        let highs = rule
            .subject_guid_high
            .map_or(observed, |high| BTreeSet::from([high]));
        intents.insert(
            rule.name.clone(),
            format!(
                "unexpected opcode {} at {}, Character {}, subject GUID high bits {:?}, unsubjected packets {}",
                rule.opcode,
                rule.checkpoint,
                rule.character.as_deref().unwrap_or("any"),
                highs,
                rule.subject_guid_high.is_none() && subjectless
            ),
        );
    }
    if let Some(scope) = &rules.character_scope {
        let subjects = scope
            .packets
            .iter()
            .map(|packet| {
                format!(
                    "{}:[{}]",
                    packet.opcode,
                    packet
                        .paths
                        .iter()
                        .map(|path| path.join("."))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        intents.insert(
            scope.name.clone(),
            format!(
                "opcode 169 object GUIDs and subject paths {{{subjects}}}; outside recorded Character GUIDs; all Characters {}, CREATE only {}",
                scope.all_characters, scope.creates_only
            ),
        );
    }
    intents
}

fn paths(fields: &BTreeMap<String, Field>) -> Vec<Vec<String>> {
    fn descend(value: &Field, path: Vec<String>, result: &mut Vec<Vec<String>>) {
        result.push(path.clone());
        match value {
            Field::Object(fields) => {
                for (name, value) in fields {
                    let mut next = path.clone();
                    next.push(name.clone());
                    descend(value, next, result);
                }
            }
            Field::List(values) => {
                for (index, value) in values.iter().enumerate() {
                    let mut next = path.clone();
                    next.push(index.to_string());
                    descend(value, next, result);
                }
            }
            _ => (),
        }
    }
    let mut result = Vec::new();
    for (name, value) in fields {
        descend(value, vec![name.clone()], &mut result);
    }
    result
}

fn at_mut<'a>(fields: &'a mut BTreeMap<String, Field>, path: &[String]) -> Option<&'a mut Field> {
    let (first, rest) = path.split_first()?;
    let value = fields.get_mut(first)?;
    descend_mut(value, rest)
}

fn at<'a>(fields: &'a BTreeMap<String, Field>, path: &[String]) -> Option<&'a Field> {
    let (first, rest) = path.split_first()?;
    let mut value = fields.get(first)?;
    for part in rest {
        value = match value {
            Field::Object(fields) => fields.get(part)?,
            Field::List(values) => values.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(value)
}

fn descend_mut<'a>(value: &'a mut Field, path: &[String]) -> Option<&'a mut Field> {
    let Some((first, rest)) = path.split_first() else {
        return Some(value);
    };
    match value {
        Field::Object(fields) => descend_mut(fields.get_mut(first)?, rest),
        Field::List(values) => descend_mut(values.get_mut(first.parse::<usize>().ok()?)?, rest),
        _ => None,
    }
}

fn remove(fields: &mut BTreeMap<String, Field>, path: &[String]) -> bool {
    let Some((last, parent)) = path.split_last() else {
        return false;
    };
    if parent.is_empty() {
        return fields.remove(last).is_some();
    }
    match at_mut(fields, parent) {
        Some(Field::Object(fields)) => fields.remove(last).is_some(),
        Some(Field::List(values)) => last
            .parse::<usize>()
            .ok()
            .filter(|index| *index < values.len())
            .is_some_and(|index| {
                values.remove(index);
                true
            }),
        _ => false,
    }
}

fn guids(trace: &Trace) -> Vec<u64> {
    fn collect(value: &Field, values: &mut BTreeSet<u64>) {
        match value {
            Field::Guid(guid) if *guid != 0 => {
                values.insert(*guid);
            }
            Field::Object(fields) => fields.values().for_each(|value| collect(value, values)),
            Field::List(fields) => fields.iter().for_each(|value| collect(value, values)),
            _ => (),
        }
    }
    let mut values = BTreeSet::new();
    values.extend(trace.header.characters.values().copied());
    if trace.header.character_guid != 0 {
        values.insert(trace.header.character_guid);
    }
    for packet in &trace.packets {
        packet
            .fields
            .values()
            .for_each(|value| collect(value, &mut values));
    }
    values.into_iter().collect()
}

fn replacements(value: &Field, known: &[u64]) -> Vec<(&'static str, Field)> {
    let unsigned = |value: u64| {
        let mut result = vec![("zero", Field::Unsigned(0))];
        if let Some(next) = value.checked_add(1) {
            result.push(("plus_one", Field::Unsigned(next)));
        }
        if let Some(next) = value.checked_sub(1) {
            result.push(("minus_one", Field::Unsigned(next)));
        }
        result
    };
    let mut result = match value {
        Field::Unsigned(value) => unsigned(*value),
        Field::Signed(value) => {
            let mut result = vec![("zero", Field::Signed(0))];
            if let Some(next) = value.checked_add(1) {
                result.push(("plus_one", Field::Signed(next)));
            }
            if let Some(next) = value.checked_sub(1) {
                result.push(("minus_one", Field::Signed(next)));
            }
            result
        }
        Field::Float(value) => vec![
            ("zero", Field::Float(0.0)),
            ("plus_one", Field::Float(value + 1.0)),
            ("minus_one", Field::Float(value - 1.0)),
        ],
        Field::TimeMs(value) => unsigned(*value)
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    if let Field::Unsigned(value) = value {
                        Field::TimeMs(value)
                    } else {
                        unreachable!()
                    },
                )
            })
            .collect(),
        Field::WallClockMs(value) => unsigned(*value)
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    if let Field::Unsigned(value) = value {
                        Field::WallClockMs(value)
                    } else {
                        unreachable!()
                    },
                )
            })
            .collect(),
        Field::Guid(value) => {
            let mut result = vec![("unknown_subject", Field::Guid(u64::MAX))];
            if let Some(other) = known.iter().find(|known| **known != *value) {
                result.push(("known_subject", Field::Guid(*other)));
            }
            result
        }
        Field::TextSha256(_) => vec![(
            "other_text",
            Field::TextSha256(Sha256Digest::of(b"strictness mutation")),
        )],
        Field::Object(_) | Field::List(_) => Vec::new(),
    };
    result.retain(|(_, replacement)| replacement != value);
    result
}

fn stream_for(
    scenario: &Scenario,
    checkpoint: &str,
    character: Option<&str>,
    opcode: u16,
) -> Stream {
    let step = scenario
        .steps
        .iter()
        .find(|step| step.id == checkpoint)
        .expect("scenario step");
    step.checkpoint
        .for_character(character.unwrap_or(""))
        .map_or_else(
            || step.checkpoint.stream(checkpoint, opcode),
            |capture| capture.stream(checkpoint, opcode),
        )
}

fn with_subject(packet: &RecordedPacket, guid: u64) -> Option<RecordedPacket> {
    let mut packet = packet.clone();
    let path = subject_path(&packet.fields)?;
    if let Some(Field::Guid(value)) = at_mut(&mut packet.fields, &path) {
        *value = guid;
        return Some(packet);
    }
    None
}

pub fn analyze(golden: &GoldenTrace) -> Result<StrictnessReport> {
    ensure!(
        compare(golden, golden.trace(), &[])?.is_match(),
        "Golden Trace does not match itself"
    );
    let trace = golden.trace();
    let scenario = golden.scenario();
    let known = guids(trace);
    let mut analyzer = Analyzer {
        golden,
        report: StrictnessReport {
            schema_version: 2,
            scenario: scenario.id.clone(),
            behavior: scenario.behavior.clone(),
            mutations: 0,
            detected: 0,
            detected_mutations: Vec::new(),
            excused_by_rule: BTreeMap::new(),
            excused_mutations: BTreeMap::new(),
            rule_intents: rule_intents(golden),
            unexplained_survivors: Vec::new(),
        },
        without_rule: BTreeMap::new(),
        seen: BTreeSet::new(),
        scoped: scoped_trace(scenario, trace, &scenario.rules)?,
    };
    for (index, packet) in trace.packets.iter().enumerate() {
        if matches!(
            packet.stream,
            Stream::Opening | Stream::Unexpected | Stream::Malformed
        ) {
            continue;
        }
        let base = format!("packet_{index}");
        let describe = |action: &str| {
            format!(
                "{} opcode {} at {}: {action}",
                packet.character.as_deref().unwrap_or("single Character"),
                packet.opcode,
                packet.checkpoint
            )
        };
        let mut candidate = trace.clone();
        candidate.packets.remove(index);
        analyzer.run(
            format!("{base}/drop"),
            describe("drop packet"),
            candidate,
            packet,
            Change::Packet,
        )?;
        let mut candidate = trace.clone();
        candidate.packets.insert(index, packet.clone());
        analyzer.run(
            format!("{base}/duplicate"),
            describe("duplicate packet"),
            candidate,
            packet,
            Change::Packet,
        )?;
        let neighbour = if index + 1 < trace.packets.len()
            && trace.packets[index + 1].stream != Stream::Opening
        {
            Some(index + 1)
        } else {
            index
                .checked_sub(1)
                .filter(|previous| trace.packets[*previous].stream != Stream::Opening)
        };
        if let Some(neighbour) = neighbour {
            let mut candidate = trace.clone();
            candidate.packets.swap(index, neighbour);
            analyzer.run(
                format!("{base}/swap_neighbour"),
                describe(&format!(
                    "swap with opcode {} at {} for {}",
                    trace.packets[neighbour].opcode,
                    trace.packets[neighbour].checkpoint,
                    trace.packets[neighbour]
                        .character
                        .as_deref()
                        .unwrap_or("single Character")
                )),
                candidate,
                packet,
                Change::Packet,
            )?;
        }
        if let Some(next) = scenario
            .steps
            .iter()
            .position(|step| step.id == packet.checkpoint)
            .and_then(|at| scenario.steps.get(at + 1))
        {
            let mut candidate = trace.clone();
            candidate.packets[index].checkpoint = next.id.clone();
            candidate.packets[index].stream = stream_for(
                scenario,
                &next.id,
                packet.character.as_deref(),
                packet.opcode,
            );
            analyzer.run(
                format!("{base}/next_checkpoint"),
                describe("move to next checkpoint"),
                candidate,
                packet,
                Change::Packet,
            )?;
        }
        for other in scenario
            .characters
            .iter()
            .filter(|character| Some(character.id.as_str()) != packet.character.as_deref())
        {
            let mut candidate = trace.clone();
            candidate.packets[index].character = Some(other.id.clone());
            candidate.packets[index].stream =
                stream_for(scenario, &packet.checkpoint, Some(&other.id), packet.opcode);
            analyzer.run(
                format!("{base}/recipient_{}", other.id),
                describe(&format!("deliver to {}", other.id)),
                candidate,
                packet,
                Change::Packet,
            )?;
        }
        let mut candidate = trace.clone();
        let mut opening = candidate.packets.remove(index);
        opening.checkpoint = "opening".into();
        opening.stream = Stream::Opening;
        let first = candidate
            .packets
            .iter()
            .position(|packet| packet.checkpoint != "opening")
            .unwrap_or(candidate.packets.len());
        candidate.packets.insert(first, opening);
        analyzer.run(
            format!("{base}/opening"),
            describe("label as opening"),
            candidate,
            packet,
            Change::Packet,
        )?;
        if let (Some(recorded), Some(rule)) = (
            packet.received_after_ms,
            scenario
                .rules
                .packet_timing
                .iter()
                .filter(|rule| timing_applies(rule, packet))
                .max_by_key(|rule| rule.path.len()),
        ) {
            for (name, offset) in [
                ("at_bound", rule.tolerance_ms),
                ("past_bound", rule.tolerance_ms.saturating_add(1)),
            ] {
                let arrival = recorded
                    .checked_add(offset)
                    .or_else(|| recorded.checked_sub(offset));
                if let Some(arrival) = arrival {
                    let mut candidate = trace.clone();
                    candidate.packets[index].received_after_ms = Some(arrival);
                    analyzer.run(
                        format!("{base}/received_after_ms/{name}"),
                        describe(&format!("change arrival by {offset} ms")),
                        candidate,
                        packet,
                        Change::Timing,
                    )?;
                }
            }
        }
        for path in paths(&packet.fields) {
            let label = path.join(".");
            let mut changed = packet.clone();
            remove(&mut changed.fields, &path);
            analyzer.run_field(
                format!("{base}/field_{label}/remove"),
                describe(&format!("remove {label}")),
                index,
                packet,
                changed,
                path.clone(),
            )?;
            let value = at(&packet.fields, &path).expect("enumerated field");
            for (name, replacement) in replacements(value, &known) {
                let mut changed = packet.clone();
                *at_mut(&mut changed.fields, &path).expect("enumerated field") = replacement;
                analyzer.run_field(
                    format!("{base}/field_{label}/{name}"),
                    describe(&format!("change {label}: {name}")),
                    index,
                    packet,
                    changed,
                    path.clone(),
                )?;
            }
        }
    }
    let opcodes: BTreeSet<_> = trace.packets.iter().map(|packet| packet.opcode).collect();
    for step in &scenario.steps {
        for opcode in &opcodes {
            let Some(template) = trace
                .packets
                .iter()
                .filter(|packet| packet.opcode == *opcode)
                .max_by_key(|packet| paths(&packet.fields).len())
            else {
                continue;
            };
            let recipients: Vec<Option<&str>> = if scenario.schema_version == 4 {
                step.checkpoint
                    .characters
                    .iter()
                    .map(|capture| Some(capture.character.as_str()))
                    .collect()
            } else {
                vec![None]
            };
            for recipient in recipients {
                let mut packet = template.clone();
                packet.checkpoint = step.id.clone();
                packet.character = recipient.map(str::to_owned);
                packet.stream = stream_for(scenario, &step.id, recipient, *opcode);
                if matches!(
                    step.action.as_ref(),
                    Some(SessionAction::Connect | SessionAction::Reconnect)
                ) && step.character.as_deref() == recipient
                    && packet.stream != Stream::Unexpected
                {
                    let mut opening = packet.clone();
                    opening.stream = Stream::Opening;
                    let mut candidate = trace.clone();
                    candidate.packets.push(opening.clone());
                    analyzer.run(
                        format!(
                            "inject_{}/{}/{}_opening",
                            step.id,
                            opcode,
                            recipient.unwrap_or("single")
                        ),
                        format!("inject declared opcode {opcode} as opening at {}", step.id),
                        candidate,
                        &opening,
                        Change::Packet,
                    )?;
                }
                let mut candidates = vec![("original", packet.clone())];
                let actor = recipient
                    .and_then(|id| trace.header.characters.get(id))
                    .copied()
                    .unwrap_or(trace.header.character_guid);
                if actor != 0 {
                    if let Some(other) = with_subject(&packet, actor) {
                        candidates.push(("character_subject", other));
                    }
                }
                if let Some(other) = with_subject(&packet, (61744_u64 << 48) | 1) {
                    candidates.push(("creature_subject", other));
                }
                for (subject, packet) in candidates {
                    let mut candidate = trace.clone();
                    candidate.packets.push(packet.clone());
                    analyzer.run(
                        format!(
                            "inject_{}/{}/{}_{subject}",
                            step.id,
                            opcode,
                            recipient.unwrap_or("single")
                        ),
                        format!(
                            "inject opcode {opcode} for {} at {} ({subject})",
                            recipient.unwrap_or("single Character"),
                            step.id
                        ),
                        candidate,
                        &packet,
                        Change::Packet,
                    )?;
                }
            }
        }
    }
    Ok(analyzer.report)
}

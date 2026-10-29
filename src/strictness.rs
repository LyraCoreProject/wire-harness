use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::compare::{compare, compare_with_rules, scoped_trace, EquivalenceRules};
use crate::scenario::{Scenario, Stream};
use crate::trace::{GoldenTrace, RecordedField as Field, RecordedPacket, Sha256Digest, Trace};

#[derive(Debug, Serialize)]
pub struct StrictnessReport {
    pub schema_version: u32,
    pub scenario: String,
    pub behavior: String,
    pub mutations: usize,
    pub detected: usize,
    pub excused_by_rule: BTreeMap<String, usize>,
    pub unexplained_survivors: Vec<Survivor>,
    pub broad_rules: Vec<BroadRule>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Survivor {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct BroadRule {
    pub name: String,
    pub excused: usize,
    pub share_percent: usize,
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

#[derive(Clone)]
enum Change {
    Field { opcode: u16, path: Vec<String> },
    Packet { opcode: u16 },
}

struct Analyzer<'a> {
    golden: &'a GoldenTrace,
    report: StrictnessReport,
    without_rule: BTreeMap<String, (EquivalenceRules, usize)>,
    seen: BTreeSet<[u8; 32]>,
}

impl Analyzer<'_> {
    fn run(
        &mut self,
        id: String,
        description: String,
        candidate: Trace,
        change: Change,
    ) -> Result<()> {
        if candidate == *self.golden.trace() {
            return Ok(());
        }
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&candidate)?).into();
        if !self.seen.insert(digest) {
            return Ok(());
        }
        self.report.mutations += 1;
        let comparison = compare(self.golden, &candidate, &[])?;
        if !comparison.is_match() {
            self.report.detected += 1;
            return Ok(());
        }
        for name in candidate_rules(self.golden.scenario(), &change) {
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
                *self.report.excused_by_rule.entry(name).or_default() += 1;
                return Ok(());
            }
        }
        self.report
            .unexplained_survivors
            .push(Survivor { id, description });
        Ok(())
    }
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

fn candidate_rules(scenario: &Scenario, change: &Change) -> Vec<String> {
    let rules = &scenario.rules;
    let mut names = Vec::new();
    let opcode = match change {
        Change::Field { opcode, path } => {
            for rule in rules.identities.iter().chain(&rules.data_fields) {
                if rule.opcode == *opcode
                    && (path.starts_with(&rule.path) || rule.path.starts_with(path))
                {
                    names.push(rule.name.clone());
                }
            }
            for rule in &rules.clocks {
                if rule.opcode == *opcode
                    && (path.starts_with(&rule.path) || rule.path.starts_with(path))
                {
                    names.push(rule.name.clone());
                }
            }
            for rule in &rules.random {
                if rule.opcode == *opcode
                    && (path.starts_with(&rule.path) || rule.path.starts_with(path))
                {
                    names.push(rule.name.clone());
                }
            }
            *opcode
        }
        Change::Packet { opcode } => *opcode,
    };
    if let Some(scope) = &rules.character_scope {
        names.push(scope.name.clone());
    }
    if matches!(change, Change::Packet { .. }) {
        names.extend(
            rules
                .unexpected_packets
                .iter()
                .filter(|rule| rule.opcode == opcode)
                .map(|rule| rule.name.clone()),
        );
    }
    names
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
    for path in paths(&packet.fields) {
        if let Some(Field::Guid(value)) = at_mut(&mut packet.fields, &path) {
            *value = guid;
            return Some(packet);
        }
    }
    None
}

pub fn analyze(golden: &GoldenTrace) -> Result<StrictnessReport> {
    ensure!(
        compare(golden, golden.trace(), &[])?.is_match(),
        "Golden Trace does not match itself"
    );
    let source = golden.trace();
    let scoped = scoped_trace(golden.scenario(), source, &golden.scenario().rules)?;
    let scoped_golden = golden.with_trace(scoped);
    let golden = &scoped_golden;
    let trace = golden.trace();
    let scenario = golden.scenario();
    let known = guids(source);
    let mut analyzer = Analyzer {
        golden,
        report: StrictnessReport {
            schema_version: 1,
            scenario: scenario.id.clone(),
            behavior: scenario.behavior.clone(),
            mutations: 0,
            detected: 0,
            excused_by_rule: BTreeMap::new(),
            unexplained_survivors: Vec::new(),
            broad_rules: Vec::new(),
        },
        without_rule: BTreeMap::new(),
        seen: BTreeSet::new(),
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
            Change::Packet {
                opcode: packet.opcode,
            },
        )?;
        let mut candidate = trace.clone();
        candidate.packets.insert(index, packet.clone());
        analyzer.run(
            format!("{base}/duplicate"),
            describe("duplicate packet"),
            candidate,
            Change::Packet {
                opcode: packet.opcode,
            },
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
                Change::Packet {
                    opcode: packet.opcode,
                },
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
                Change::Packet {
                    opcode: packet.opcode,
                },
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
                Change::Packet {
                    opcode: packet.opcode,
                },
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
            Change::Packet {
                opcode: packet.opcode,
            },
        )?;
        for path in paths(&packet.fields) {
            let label = path.join(".");
            let mut candidate = trace.clone();
            remove(&mut candidate.packets[index].fields, &path);
            analyzer.run(
                format!("{base}/field_{label}/remove"),
                describe(&format!("remove {label}")),
                candidate,
                Change::Field {
                    opcode: packet.opcode,
                    path: path.clone(),
                },
            )?;
            let value = at(&packet.fields, &path).expect("enumerated field");
            for (name, replacement) in replacements(value, &known) {
                let mut candidate = trace.clone();
                *at_mut(&mut candidate.packets[index].fields, &path).expect("enumerated field") =
                    replacement;
                analyzer.run(
                    format!("{base}/field_{label}/{name}"),
                    describe(&format!("change {label}: {name}")),
                    candidate,
                    Change::Field {
                        opcode: packet.opcode,
                        path: path.clone(),
                    },
                )?;
            }
        }
    }
    let opcodes: BTreeSet<_> = source.packets.iter().map(|packet| packet.opcode).collect();
    for step in &scenario.steps {
        for opcode in &opcodes {
            let Some(template) = source
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
                    candidate.packets.push(packet);
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
                        Change::Packet { opcode: *opcode },
                    )?;
                }
            }
        }
    }
    for (name, excused) in &analyzer.report.excused_by_rule {
        if *excused >= 10 && *excused * 4 >= analyzer.report.mutations {
            analyzer.report.broad_rules.push(BroadRule {
                name: name.clone(),
                excused: *excused,
                share_percent: *excused * 100 / analyzer.report.mutations,
            });
        }
    }
    Ok(analyzer.report)
}

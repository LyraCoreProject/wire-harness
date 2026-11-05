use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::compare::{Comparison, Difference, DifferenceClass};
use crate::decode::{Field, Packet};
use crate::rolls::{binomial_p_ratio_upper, FAMILY_ALPHA, TESTS_PER_SUBMISSION};
use crate::scenario::Stream;
use crate::trace::RecordedField;

pub const CATEGORIES: [&str; 5] = ["miss", "resist", "crit", "hit", "partial_resist"];

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probability {
    pub numerator: u64,
    pub denominator: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specification {
    pub samples_per_level: u32,
    pub reference_samples_per_level: u32,
    pub reference_characters: u8,
    pub spell_id: u32,
    pub intellect: u32,
    pub target_entry: u32,
    pub target_faction: u32,
    pub target_levels: [u32; 2],
    pub expected: [[Probability; 5]; 2],
}

impl Specification {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.samples_per_level == 50_000
                && self.reference_samples_per_level == 2_000
                && self.reference_characters == 16,
            "spell roll sample allocation changed"
        );
        ensure!(
            (1..=65535).contains(&self.spell_id)
                && self.intellect <= 10_000
                && self.target_entry > 0
                && self.target_faction > 0
                && self
                    .target_levels
                    .iter()
                    .all(|level| (1..=255).contains(level))
                && self.target_levels[0] < self.target_levels[1],
            "invalid spell roll context"
        );
        for probabilities in &self.expected {
            for probability in probabilities {
                ensure!(
                    probability.denominator > 0
                        && probability.denominator <= (1u64 << 53)
                        && probability.numerator <= probability.denominator,
                    "invalid exact spell probability"
                );
            }
            let sum: f64 = probabilities[..4]
                .iter()
                .map(|p| p.numerator as f64 / p.denominator as f64)
                .sum();
            ensure!(
                (sum - 1.0).abs() <= 4.0 * f64::EPSILON,
                "spell outcome probabilities must sum to one"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts(pub [u32; 5]);

impl Counts {
    // Partial resistance overlaps landed hits and crits; it is not another cast.
    pub fn total(&self) -> u64 {
        self.0[..4].iter().map(|count| u64::from(*count)).sum()
    }

    fn add(&mut self, category: usize) -> Result<()> {
        self.0[category] = self.0[category]
            .checked_add(1)
            .context("spell count overflow")?;
        Ok(())
    }

    fn validate(&self, samples: u32) -> Result<()> {
        ensure!(
            self.total() == u64::from(samples),
            "incomplete spell sample"
        );
        ensure!(
            u64::from(self.0[4]) <= u64::from(self.0[2]) + u64::from(self.0[3]),
            "partial resists exceed landed casts"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    None,
    Start,
    Go,
    Damage,
}

pub struct Collector {
    caster: u64,
    target: u64,
    spell_id: u32,
    school: u8,
    cast_time_ms: u32,
    pending: Pending,
    pub counts: Counts,
}

impl Collector {
    pub fn new(
        caster: u64,
        target: u64,
        spell_id: u32,
        school: u8,
        cast_time_ms: u32,
    ) -> Result<Self> {
        ensure!(
            caster != 0
                && target != 0
                && caster != target
                && (1..=65535).contains(&spell_id)
                && (1..=6).contains(&school)
                && cast_time_ms > 0
                && cast_time_ms <= 60_000,
            "invalid spell participants"
        );
        Ok(Self {
            caster,
            target,
            spell_id,
            school,
            cast_time_ms,
            pending: Pending::None,
            counts: Counts::default(),
        })
    }

    pub fn begin_cast(&mut self) -> Result<()> {
        ensure!(self.pending == Pending::None, "previous cast is incomplete");
        self.pending = Pending::Start;
        Ok(())
    }

    pub fn idle(&self) -> bool {
        self.pending == Pending::None
    }

    pub fn observe(&mut self, packet: &Packet) -> Result<bool> {
        if packet.opcode == 0x0130
            && packet.fields.get("spell") == Some(&Field::Unsigned(u64::from(self.spell_id)))
            && packet.fields.get("result") == Some(&Field::Unsigned(2))
        {
            anyhow::bail!("spell cast was refused: {:?}", packet.fields);
        }
        if !matches!(packet.opcode, 0x0131 | 0x0132 | 0x0250)
            || packet.fields.get("caster") != Some(&Field::Guid(self.caster))
        {
            return Ok(false);
        }
        ensure!(
            number(packet, "spell")? == u64::from(self.spell_id),
            "unexpected own-caster spell"
        );
        if packet.opcode == 0x0131 {
            ensure!(
                self.pending == Pending::Start,
                "unsolicited or duplicate spell start"
            );
            ensure!(
                packet.fields.get("duration") == Some(&Field::TimeMs(u64::from(self.cast_time_ms))),
                "spell cast time changed"
            );
            self.pending = Pending::Go;
            return Ok(false);
        }
        if packet.opcode == 0x0132 {
            ensure!(
                self.pending == Pending::Go,
                "unsolicited or duplicate spell go"
            );
            let Some(Field::List(hits)) = packet.fields.get("hits") else {
                anyhow::bail!("missing spell hits")
            };
            let Some(Field::List(misses)) = packet.fields.get("misses") else {
                anyhow::bail!("missing spell misses")
            };
            if hits == &[Field::Guid(self.target)] && misses.is_empty() {
                self.pending = Pending::Damage;
                return Ok(false);
            }
            ensure!(
                hits.is_empty() && misses.len() == 1,
                "spell changed target count"
            );
            let Field::Object(miss) = &misses[0] else {
                anyhow::bail!("invalid spell miss")
            };
            ensure!(
                miss.get("guid") == Some(&Field::Guid(self.target)),
                "spell changed target"
            );
            let category = match miss.get("reason") {
                Some(Field::Unsigned(1)) => 0,
                Some(Field::Unsigned(2)) => 1,
                _ => anyhow::bail!("ineligible spell miss reason"),
            };
            self.counts.add(category)?;
        } else {
            ensure!(
                self.pending == Pending::Damage,
                "unsolicited or duplicate spell damage"
            );
            ensure!(
                packet.fields.get("target") == Some(&Field::Guid(self.target)),
                "spell damage changed target"
            );
            ensure!(
                number(packet, "school")? == u64::from(self.school)
                    && number(packet, "absorbed")? == 0
                    && number(packet, "blocked")? == 0
                    && number(packet, "periodic")? == 0
                    && number(packet, "unused")? == 0
                    && number(packet, "extended")? == 0,
                "ineligible spell damage context"
            );
            let flags = number(packet, "hit_info")?;
            let resisted = match packet.fields.get("resisted") {
                Some(Field::Signed(value)) if *value >= 0 => *value,
                _ => anyhow::bail!("ineligible negative or missing spell resistance"),
            };
            ensure!(
                flags & !2 == 0 && number(packet, "damage")? > 0,
                "ineligible spell damage flags or damage"
            );
            self.counts.add(if flags & 2 != 0 { 2 } else { 3 })?;
            if resisted > 0 {
                self.counts.add(4)?;
            }
        }
        self.pending = Pending::None;
        Ok(true)
    }
}

fn number(packet: &Packet, name: &str) -> Result<u64> {
    match packet.fields.get(name) {
        Some(Field::Unsigned(value)) => Ok(*value),
        _ => anyhow::bail!("spell packet has no {name}"),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRecording {
    pub classifier_version: u32,
    pub setup_ms: u64,
    pub elapsed_ms: u64,
    pub characters: Vec<RecordedCharacter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCharacter {
    pub level: u32,
    pub counts: Counts,
    pub blocks: Vec<Counts>,
}

impl CheckRecording {
    pub fn validate(&self, spec: &Specification) -> Result<()> {
        spec.validate()?;
        ensure!(
            self.classifier_version == 1
                && self.setup_ms > 0
                && self.elapsed_ms > 0
                && self.elapsed_ms < 30 * 60 * 1000
                && self.characters.len() == 16,
            "invalid spell check provenance or duration"
        );
        for (level_index, level) in spec.target_levels.into_iter().enumerate() {
            let characters: Vec<_> = self
                .characters
                .iter()
                .filter(|c| c.level == level)
                .collect();
            ensure!(
                characters.len() == 8,
                "spell check needs eight Characters per level"
            );
            let mut total = Counts::default();
            for character in characters {
                character.counts.validate(250)?;
                ensure!(character.blocks.len() == 5, "spell check needs five blocks");
                let mut sum = Counts::default();
                for block in &character.blocks {
                    block.validate(50)?;
                    for (sum, count) in sum.0.iter_mut().zip(block.0) {
                        *sum += count;
                    }
                }
                ensure!(sum == character.counts, "spell blocks disagree");
                for (sum, count) in total.0.iter_mut().zip(character.counts.0) {
                    *sum += count;
                }
            }
            ensure!(
                compare(spec, &total, level_index, "reference_check", 2000)?.is_match(),
                "small spell check disagrees with derived rates at level {level}: {:?}",
                total.0
            );
        }
        Ok(())
    }
}

pub fn compare(
    spec: &Specification,
    counts: &Counts,
    level_index: usize,
    seed_slot: &str,
    samples: u32,
) -> Result<Comparison> {
    spec.validate()?;
    ensure!(level_index < 2, "invalid spell target level");
    counts.validate(samples)?;
    let mut differences = Vec::new();
    for (category, name) in CATEGORIES.iter().enumerate() {
        let expected = spec.expected[level_index][category];
        let actual = counts.0[category];
        let p = binomial_p_ratio_upper(samples, actual, expected.numerator, expected.denominator)?;
        let alpha = FAMILY_ALPHA / f64::from(TESTS_PER_SUBMISSION);
        if p <= alpha {
            differences.push(Difference {
                classification: DifferenceClass::Rule, character: None,
                checkpoint: format!("level_{}_{}", spec.target_levels[level_index], seed_slot),
                stream: Some(Stream::Broadcast("casts".into())), packet_index: None,
                field_path: vec!["outcomes".into(), (*name).into()], opcode: Some(0x0250),
                deviations: vec![], rule: "spell_distribution".into(),
                reason: format!("{actual}/{samples}, expected {}/{}; equal-tail binomial p upper={p:.12e}; Bonferroni alpha={alpha:.12e}", expected.numerator, expected.denominator),
                expected: Some(RecordedField::Unsigned((u128::from(expected.numerator) * u128::from(samples) / u128::from(expected.denominator)) as u64)),
                actual: Some(RecordedField::Unsigned(u64::from(actual))),
            });
        }
    }
    Ok(Comparison { differences })
}

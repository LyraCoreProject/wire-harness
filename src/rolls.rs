use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::compare::{Comparison, Difference, DifferenceClass};
use crate::decode::{Field, Packet};
use crate::scenario::Stream;

pub const CATEGORIES: [&str; 7] = ["miss", "dodge", "parry", "block", "glancing", "crit", "hit"];
pub const PUBLIC_SEEDS: [u64; 2] = [0x4c79726100000001, 0x4c79726100000002];
pub const FAMILY_ALPHA: f64 = 1e-6;
pub const TESTS_PER_SUBMISSION: u32 = 7 * 2 * 3;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specification {
    pub samples_per_level: u32,
    pub reference_samples_per_level: u32,
    pub reference_characters: u8,
    pub agility: u32,
    pub weapon_skill: u32,
    pub defense_skill: u32,
    pub target_faction: u32,
    pub target_entry: u32,
    pub target_levels: [u32; 2],
    pub expected_basis_points: [[u32; 7]; 2],
}

impl Specification {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.samples_per_level == 50_000,
            "roll sample size must be 50000"
        );
        ensure!(
            self.reference_samples_per_level == 2_000,
            "reference sample size must be 2000"
        );
        ensure!(
            self.reference_characters == 16,
            "reference check needs 16 Characters"
        );
        ensure!(
            self.agility <= 10_000
                && self.weapon_skill <= 1275
                && self.defense_skill <= 1275
                && self.target_entry > 0
                && self.target_faction > 0,
            "invalid roll context"
        );
        ensure!(
            self.target_levels
                .iter()
                .all(|level| (1..=255).contains(level))
                && self.target_levels[0] < self.target_levels[1],
            "invalid roll target levels"
        );
        for probabilities in self.expected_basis_points {
            ensure!(
                probabilities.iter().all(|p| *p <= 10_000),
                "invalid roll probability"
            );
            ensure!(
                probabilities.iter().sum::<u32>() == 10_000,
                "roll probabilities must sum to one"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts(pub [u32; 7]);

impl Counts {
    pub fn total(&self) -> u32 {
        self.0
            .iter()
            .map(|count| u64::from(*count))
            .sum::<u64>()
            .min(u64::from(u32::MAX)) as u32
    }

    pub fn observe(&mut self, packet: &Packet, attacker: u64, target: u64) -> Result<bool> {
        if packet.opcode != 0x014a {
            return Ok(false);
        }
        if packet.fields.get("attacker") != Some(&Field::Guid(attacker)) {
            return Ok(false);
        }
        ensure!(
            packet.fields.get("victim") == Some(&Field::Guid(target)),
            "swing changed target"
        );
        let number = |name: &str| -> Result<u64> {
            match packet.fields.get(name) {
                Some(Field::Unsigned(value)) => Ok(*value),
                _ => anyhow::bail!("swing has no {name}"),
            }
        };
        ensure!(
            number("school")? == 0
                && number("spell")? == 0
                && number("absorbed")? == 0
                && number("resisted")? == 0,
            "ineligible school, spell, absorb or resist"
        );
        let flags = number("hit_info")?;
        let state = number("victim_state")?;
        ensure!(
            flags & (0x4 | 0x8000 | 0x20 | 0x40) == 0,
            "ineligible offhand, crushing or absorbed swing"
        );
        let outcomes = [
            flags & 0x10 != 0,
            state == 2,
            state == 3,
            flags & 0x800 != 0 || state == 5,
            flags & 0x4000 != 0,
            flags & 0x80 != 0,
        ];
        ensure!(
            outcomes.iter().filter(|v| **v).count() <= 1,
            "conflicting swing categories"
        );
        ensure!(matches!(state, 0..=3 | 5), "ineligible swing victim state");
        let category = outcomes.iter().position(|v| *v).unwrap_or(6);
        ensure!(
            (state == 0) == (category == 0),
            "inconsistent miss victim state"
        );
        ensure!(
            number("damage")? == number("subdamage")?,
            "swing subdamage disagrees"
        );
        if category < 3 {
            ensure!(
                number("damage")? == 0 && number("blocked")? == 0,
                "avoided swing dealt damage"
            );
        }
        self.0[category] = self.0[category]
            .checked_add(1)
            .context("swing count overflow")?;
        Ok(true)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRecording {
    pub classifier_version: u32,
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
        ensure!(
            self.classifier_version == 1 && self.elapsed_ms > 0 && self.elapsed_ms < 30 * 60 * 1000,
            "invalid check recording duration or classifier"
        );
        ensure!(
            self.characters.len() == usize::from(spec.reference_characters),
            "check Character count"
        );
        for level in spec.target_levels {
            let characters: Vec<_> = self
                .characters
                .iter()
                .filter(|c| c.level == level)
                .collect();
            ensure!(
                characters.len() == 8,
                "check needs eight Characters per level"
            );
            ensure!(
                characters
                    .iter()
                    .map(|c| u64::from(c.counts.total()))
                    .sum::<u64>()
                    == u64::from(spec.reference_samples_per_level),
                "check sample count"
            );
            for character in characters {
                ensure!(
                    character.counts.total() == 250 && character.blocks.len() == 5,
                    "check Character samples"
                );
                let mut sum = Counts::default();
                for block in &character.blocks {
                    ensure!(block.total() == 50, "check block samples");
                    for (total, count) in sum.0.iter_mut().zip(block.0) {
                        *total += count;
                    }
                }
                ensure!(sum == character.counts, "check block counts disagree");
            }
        }
        for (index, level) in spec.target_levels.into_iter().enumerate() {
            let mut counts = Counts::default();
            for character in self.characters.iter().filter(|c| c.level == level) {
                for category in 0..7 {
                    counts.0[category] += character.counts.0[category];
                }
            }
            ensure!(
                compare(
                    spec,
                    &counts,
                    index,
                    "reference_check",
                    spec.reference_samples_per_level
                )?
                .is_match(),
                "small check disagrees with derived rates at level {level}: {:?}",
                counts.0
            );
        }
        Ok(())
    }
}

// Equal-tail exact binomial; the numerical allowance makes rejection conservative.
pub fn binomial_p_upper(n: u32, k: u32, basis_points: u32) -> Result<f64> {
    ensure!(
        n > 0 && n <= 50_000 && k <= n && basis_points <= 10_000,
        "invalid binomial input"
    );
    if basis_points == 0 {
        return Ok(if k == 0 { 1.0 } else { 0.0 });
    }
    if basis_points == 10_000 {
        return Ok(if k == n { 1.0 } else { 0.0 });
    }
    let p = f64::from(basis_points) / 10_000.0;
    let mode = ((f64::from(n) + 1.0) * p).floor() as u32;
    let mut total = 1.0;
    let mut lower = if mode <= k { 1.0 } else { 0.0 };
    let mut upper = if mode >= k { 1.0 } else { 0.0 };
    let mut mass = 1.0;
    for x in (1..=mode).rev() {
        mass *= f64::from(x) / f64::from(n - x + 1) * (1.0 - p) / p;
        total += mass;
        if x - 1 <= k {
            lower += mass;
        }
        if x > k {
            upper += mass;
        }
    }
    mass = 1.0;
    for x in mode..n {
        mass *= f64::from(n - x) / f64::from(x + 1) * p / (1.0 - p);
        total += mass;
        if x < k {
            lower += mass;
        }
        if x + 1 >= k {
            upper += mass;
        }
    }
    Ok((2.0 * lower.min(upper) / total + 64.0 * f64::from(n) * f64::EPSILON).min(1.0))
}

pub fn compare(
    spec: &Specification,
    counts: &Counts,
    level_index: usize,
    seed_slot: &str,
    samples: u32,
) -> Result<Comparison> {
    spec.validate()?;
    ensure!(
        level_index < 2 && counts.total() == samples,
        "incomplete swing sample"
    );
    let mut differences = Vec::new();
    for (category, name) in CATEGORIES.iter().enumerate() {
        let expected = spec.expected_basis_points[level_index][category];
        let actual = counts.0[category];
        let p = binomial_p_upper(samples, actual, expected)?;
        if p <= FAMILY_ALPHA / f64::from(TESTS_PER_SUBMISSION) {
            differences.push(Difference {
                classification: DifferenceClass::Rule,
                character: None,
                checkpoint: format!("level_{}_{}", spec.target_levels[level_index], seed_slot),
                stream: Some(Stream::Broadcast("swings".into())),
                packet_index: None,
                field_path: vec!["outcomes".into(), (*name).into()],
                opcode: Some(0x014a),
                deviations: vec![],
                rule: "white_melee_distribution".into(),
                reason: format!("{actual}/{samples}, expected {expected}/10000; equal-tail binomial p upper={p:.12e}; Bonferroni alpha={:.12e}", FAMILY_ALPHA / f64::from(TESTS_PER_SUBMISSION)),
                expected: Some(crate::trace::RecordedField::Unsigned(u64::from(expected) * u64::from(samples) / 10_000)),
                actual: Some(crate::trace::RecordedField::Unsigned(u64::from(actual))),
            });
        }
    }
    Ok(Comparison { differences })
}

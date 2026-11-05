use std::collections::BTreeMap;

use wire_client::decode::{decode, Field, Packet};
use wire_client::rolls::{binomial_p_ratio_upper, TESTS_PER_SUBMISSION};
use wire_client::scenario::Scenario;
use wire_client::spell_rolls::{compare, Collector, Counts, Specification};

fn spec() -> Specification {
    Scenario::parse(include_str!(
        "../scenarios/spell_direct_damage_outcomes.toml"
    ))
    .unwrap()
    .spell_rolls
    .unwrap()
}

fn start() -> Vec<u8> {
    let mut bytes = vec![1, 1, 1, 1];
    bytes.extend(2948u32.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(1500u32.to_le_bytes());
    bytes.extend([2, 0, 1, 2]);
    bytes
}

fn go(reason: Option<u8>) -> Vec<u8> {
    let mut bytes = vec![1, 1, 1, 1];
    bytes.extend(2948u32.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.push(u8::from(reason.is_none()));
    if reason.is_none() {
        bytes.extend(2u64.to_le_bytes());
    }
    bytes.push(u8::from(reason.is_some()));
    if let Some(reason) = reason {
        bytes.extend(2u64.to_le_bytes());
        bytes.push(reason);
    }
    bytes.extend([2, 0, 1, 2]);
    bytes
}

fn damage(crit: bool, resisted: u32) -> Vec<u8> {
    let mut bytes = vec![1, 2, 1, 1];
    bytes.extend(2948u32.to_le_bytes());
    bytes.extend(50u32.to_le_bytes());
    bytes.push(2);
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(resisted.to_le_bytes());
    bytes.extend([0, 0]);
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((if crit { 2u32 } else { 0 }).to_le_bytes());
    bytes.push(0);
    bytes
}

fn begin(collector: &mut Collector) {
    collector.begin_cast().unwrap();
    assert!(!collector
        .observe(&decode(0x131, &start()).unwrap())
        .unwrap());
}

#[test]
fn rational_binomial_tails_and_submission_allocation_are_exact() {
    assert!((binomial_p_ratio_upper(2, 0, 1, 3).unwrap() - 8.0 / 9.0).abs() < 1e-12);
    assert!((binomial_p_ratio_upper(3, 3, 1, 3).unwrap() - 2.0 / 27.0).abs() < 1e-12);
    for (p, k, expected) in [(0, 0, 1.0), (0, 1, 0.0), (3, 2, 1.0), (3, 1, 0.0)] {
        assert_eq!(binomial_p_ratio_upper(2, k, p, 3).unwrap(), expected);
    }
    assert!(binomial_p_ratio_upper(2, 0, 1, 0).is_err());
    assert!(binomial_p_ratio_upper(2, 0, u64::MAX - 1, u64::MAX).is_err());
    assert_eq!(TESTS_PER_SUBMISSION, (7 + 5) * 2 * 3);
}

#[test]
fn spell_wire_outcomes_include_overlapping_partial_resists() {
    let mut collector = Collector::new(1, 2, 2948, 2, 1500).unwrap();
    for reason in [Some(1), Some(2)] {
        begin(&mut collector);
        assert!(collector
            .observe(&decode(0x132, &go(reason)).unwrap())
            .unwrap());
    }
    for (crit, resisted) in [(false, 0), (true, 0), (false, 18), (true, 35)] {
        begin(&mut collector);
        assert!(!collector
            .observe(&decode(0x132, &go(None)).unwrap())
            .unwrap());
        assert!(collector
            .observe(&decode(0x250, &damage(crit, resisted)).unwrap())
            .unwrap());
    }
    assert_eq!(collector.counts, Counts([1, 1, 2, 2, 2]));
    assert_eq!(collector.counts.total(), 6);
    assert!(collector.idle());
}

#[test]
fn missing_duplicate_and_changed_target_packets_fail_collection() {
    let mut collector = Collector::new(1, 2, 2948, 2, 1500).unwrap();
    assert!(collector
        .observe(&decode(0x132, &go(None)).unwrap())
        .is_err());
    begin(&mut collector);
    assert!(collector
        .observe(&decode(0x131, &start()).unwrap())
        .is_err());
    assert!(!collector
        .observe(&decode(0x132, &go(None)).unwrap())
        .unwrap());
    assert!(collector.begin_cast().is_err());
    let mut wrong = decode(0x250, &damage(false, 0)).unwrap();
    wrong.fields.insert("target".into(), Field::Guid(3));
    assert!(collector.observe(&wrong).is_err());
    assert!(collector
        .observe(&decode(0x250, &damage(false, 0)).unwrap())
        .unwrap());
    assert!(collector
        .observe(&decode(0x250, &damage(false, 0)).unwrap())
        .is_err());
}

#[test]
fn observer_copies_and_success_results_never_add_casts() {
    let mut collector = Collector::new(3, 2, 2948, 2, 1500).unwrap();
    collector.begin_cast().unwrap();
    for (opcode, bytes) in [
        (0x131, start()),
        (0x132, go(None)),
        (0x250, damage(true, 10)),
    ] {
        assert!(!collector.observe(&decode(opcode, &bytes).unwrap()).unwrap());
    }
    let success = Packet {
        opcode: 0x130,
        fields: BTreeMap::from([
            ("spell".into(), Field::Unsigned(2948)),
            ("result".into(), Field::Unsigned(0)),
        ]),
    };
    assert!(!collector.observe(&success).unwrap());
    let mut refusal = success;
    refusal.fields.insert("result".into(), Field::Unsigned(2));
    assert!(collector.observe(&refusal).is_err());
    assert_eq!(collector.counts.total(), 0);
}

#[test]
fn ineligible_spell_damage_and_miss_reasons_fail_collection() {
    let negative = decode(0x250, &damage(false, u32::MAX)).unwrap();
    assert_eq!(negative.fields["resisted"], Field::Signed(-1));
    let mut collector = Collector::new(1, 2, 2948, 2, 1500).unwrap();
    begin(&mut collector);
    collector
        .observe(&decode(0x132, &go(None)).unwrap())
        .unwrap();
    assert!(collector.observe(&negative).is_err());
    let mut debug = damage(false, 0);
    let flags_offset = debug.len() - 5;
    debug[flags_offset..flags_offset + 4].copy_from_slice(&5u32.to_le_bytes());
    *debug.last_mut().unwrap() = 1;
    for value in [1.0f32, 6.0, 2.0, 96.0] {
        debug.extend(value.to_le_bytes());
    }
    let debug = decode(0x250, &debug).unwrap();
    assert_eq!(debug.fields["crit_needed"], Field::Float(6.0));
    assert_eq!(debug.fields["hit_needed"], Field::Float(96.0));
    for (field, value) in [
        ("absorbed", 1),
        ("blocked", 1),
        ("school", 4),
        ("periodic", 1),
        ("unused", 1),
        ("extended", 1),
        ("hit_info", 8),
        ("damage", 0),
        ("resisted", u32::MAX as u64),
    ] {
        let mut collector = Collector::new(1, 2, 2948, 2, 1500).unwrap();
        begin(&mut collector);
        collector
            .observe(&decode(0x132, &go(None)).unwrap())
            .unwrap();
        let mut packet = decode(0x250, &damage(false, 0)).unwrap();
        packet.fields.insert(field.into(), Field::Unsigned(value));
        assert!(collector.observe(&packet).is_err(), "{field}");
        assert_eq!(collector.counts.total(), 0);
    }
    for reason in [3, 4, 5, 6, 7, 8, 9, 10] {
        let mut collector = Collector::new(1, 2, 2948, 2, 1500).unwrap();
        begin(&mut collector);
        assert!(collector
            .observe(&decode(0x132, &go(Some(reason))).unwrap())
            .is_err());
    }
    for (opcode, bytes) in [
        (0x131, start()),
        (0x132, go(None)),
        (0x250, damage(true, 10)),
    ] {
        for end in 0..bytes.len() {
            assert!(decode(opcode, &bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode(opcode, &trailing).is_err());
    }
}

#[test]
fn fixed_rates_detect_crit_partial_and_miss_shifts_at_both_levels() {
    let spec = spec();
    for level in 0..2 {
        let mut control = [0u32; 5];
        for (count, expected) in control.iter_mut().zip(spec.expected[level]) {
            *count =
                (expected.numerator as f64 / expected.denominator as f64 * 50_000.0).round() as u32;
        }
        control[3] = 50_000 - control[..3].iter().sum::<u32>();
        assert!(compare(&spec, &Counts(control), level, "control", 50_000)
            .unwrap()
            .is_match());
        let mut shifted = control;
        shifted[2] += 1000;
        shifted[3] -= 1000;
        let result = compare(&spec, &Counts(shifted), level, "shifted", 50_000).unwrap();
        assert!(result
            .differences
            .iter()
            .any(|d| d.rule == "spell_distribution" && d.field_path == ["outcomes", "crit"]));
        shifted = control;
        shifted[4] += 1000;
        assert!(compare(&spec, &Counts(shifted), level, "shifted", 50_000)
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field_path == ["outcomes", "partial_resist"]));
        shifted = control;
        shifted[0] += 1;
        shifted[3] -= 1;
        assert!(compare(&spec, &Counts(shifted), level, "shifted", 50_000)
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field_path == ["outcomes", "miss"]));
    }
}

#[test]
fn malformed_sample_totals_and_probability_overflow_return_errors() {
    let mut spec = spec();
    for counts in [Counts([u32::MAX; 5]), Counts([0, 0, 0, 50_000, 50_001])] {
        assert!(compare(&spec, &counts, 0, "bad", 50_000).is_err());
    }
    spec.expected[0][0].denominator = 0;
    assert!(spec.validate().is_err());
}

#[test]
fn recorded_spell_blocks_require_the_declared_levels_and_provenance() {
    let golden = wire_client::catalogue::get("spell_direct_damage_outcomes").unwrap();
    let recording = golden.trace().spell_roll_check.clone().unwrap();
    recording.validate(&spec()).unwrap();
    for mutation in 0..6 {
        let mut bad = recording.clone();
        match mutation {
            0 => bad.classifier_version = 2,
            1 => bad.setup_ms = 0,
            2 => bad.characters[0].level = 63,
            3 => {
                bad.characters.pop();
            }
            4 => bad.characters[0].blocks[0].0 = [u32::MAX; 5],
            5 => bad.characters[0].counts.0 = [u32::MAX; 5],
            _ => unreachable!(),
        }
        assert!(bad.validate(&spec()).is_err(), "mutation {mutation}");
    }
}

#[test]
fn short_tcp_witnesses_and_copied_reference_counts_cannot_claim_spell_rates() {
    let golden = wire_client::catalogue::get("spell_direct_damage_outcomes").unwrap();
    for recording in [None, golden.trace().spell_roll_check.clone()] {
        let mut candidate = golden.trace().clone();
        candidate.header.origin = wire_client::trace::Origin::Candidate {
            adapter: "test".into(),
        };
        candidate.spell_roll_check = recording;
        let comparison = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
        assert!(comparison
            .differences
            .iter()
            .any(|difference| difference.rule == "spell_sample"));
    }
}

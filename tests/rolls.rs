use wire_client::rolls::{binomial_p_upper, compare, Counts, Specification};

fn spec() -> Specification {
    Specification {
        samples_per_level: 50_000,
        reference_samples_per_level: 2_000,
        reference_characters: 16,
        agility: 100,
        weapon_skill: 300,
        defense_skill: 300,
        target_faction: 7,
        target_entry: 38,
        target_levels: [60, 63],
        expected_basis_points: [
            [500, 500, 500, 500, 1000, 500, 6500],
            [800, 650, 1400, 500, 4000, 200, 2450],
        ],
    }
}

#[test]
fn exact_binomial_matches_hand_worked_tails_and_boundaries() {
    assert!((binomial_p_upper(10, 0, 5000).unwrap() - 2.0 / 1024.0).abs() < 1e-12);
    assert!((binomial_p_upper(10, 2, 5000).unwrap() - 112.0 / 1024.0).abs() < 1e-12);
    assert_eq!(binomial_p_upper(50_000, 0, 0).unwrap(), 1.0);
    assert_eq!(binomial_p_upper(50_000, 1, 0).unwrap(), 0.0);
    assert_eq!(binomial_p_upper(50_000, 50_000, 10_000).unwrap(), 1.0);
    assert!(binomial_p_upper(50_000, 2500, 500).unwrap() > 0.99);
    assert!(binomial_p_upper(50_000, 3500, 500).unwrap() < 1e-6 / 42.0);
}

#[test]
fn two_point_crit_shift_has_distribution_signatures_at_both_levels() {
    let spec = spec();
    for level in 0..2 {
        let mut counts = Counts(spec.expected_basis_points[level].map(|p| p * 5));
        assert!(compare(&spec, &counts, level, "control", 50_000)
            .unwrap()
            .is_match());
        counts.0[5] += 1000;
        counts.0[6] -= 1000;
        let report = compare(&spec, &counts, level, "shifted", 50_000).unwrap();
        assert!(report
            .differences
            .iter()
            .any(|d| d.field_path == ["outcomes", "crit"] && d.rule == "white_melee_distribution"));
    }
}

fn swing(flags: u32, state: u32, damage: u32, blocked: u32) -> Vec<u8> {
    let mut bytes = flags.to_le_bytes().to_vec();
    bytes.extend([1, 1, 1, 2]);
    bytes.extend(damage.to_le_bytes());
    bytes.push(1);
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((damage as f32).to_le_bytes());
    bytes.extend(damage.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(state.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(blocked.to_le_bytes());
    bytes
}

#[test]
fn independent_wire_decoder_classifies_each_category_and_rejects_conflicts() {
    let mut counts = Counts::default();
    for (flags, state, damage, blocked) in [
        (0x10, 0, 0, 0),
        (0, 2, 0, 0),
        (0, 3, 0, 0),
        (0x800, 1, 3, 1),
        (0x4000, 1, 7, 0),
        (0x80, 1, 20, 0),
        (2, 1, 10, 0),
    ] {
        let packet =
            wire_client::decode::decode(0x14a, &swing(flags, state, damage, blocked)).unwrap();
        assert!(counts.observe(&packet, 1, 2).unwrap());
    }
    assert_eq!(counts.0, [1; 7]);
    let packet = wire_client::decode::decode(0x14a, &swing(0x80 | 0x4000, 1, 7, 0)).unwrap();
    assert!(counts.observe(&packet, 1, 2).is_err());
    assert!(!counts.observe(&packet, 3, 2).unwrap());
    let packet = wire_client::decode::decode(0x14a, &swing(0x4, 1, 7, 0)).unwrap();
    assert!(counts.observe(&packet, 1, 2).is_err());
    let mut magical = wire_client::decode::decode(0x14a, &swing(2, 1, 7, 0)).unwrap();
    magical
        .fields
        .insert("school".into(), wire_client::decode::Field::Unsigned(1));
    assert!(counts.observe(&magical, 1, 2).is_err());
    let mut truncated = swing(0x80, 1, 7, 0);
    truncated.pop();
    assert!(wire_client::decode::decode(0x14a, &truncated).is_err());
}

#[test]
fn attempted_avoidance_flags_are_not_resolved_outcomes() {
    let packet = wire_client::decode::decode(0x14a, &swing(0x700 | 0x80, 1, 20, 0)).unwrap();
    let mut counts = Counts::default();
    counts.observe(&packet, 1, 2).unwrap();
    assert_eq!(counts.0, [0, 0, 0, 0, 0, 1, 0]);
}

#[test]
fn short_tcp_witnesses_cannot_claim_roll_rates() {
    let golden = wire_client::catalogue::get("white_melee_outcomes").unwrap();
    for recording in [None, golden.trace().roll_check.clone()] {
        let mut candidate = golden.trace().clone();
        candidate.header.origin = wire_client::trace::Origin::Candidate {
            adapter: "test".into(),
        };
        candidate.roll_check = recording;
        let comparison = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
        assert!(comparison
            .differences
            .iter()
            .any(|difference| difference.rule == "white_melee_sample"));
    }
}

#[test]
fn malformed_roll_counts_return_an_error_without_overflow() {
    let recording = wire_client::rolls::CheckRecording {
        classifier_version: 1,
        elapsed_ms: 500_000,
        characters: (0..16)
            .map(|index| wire_client::rolls::RecordedCharacter {
                level: if index < 8 { 60 } else { 63 },
                counts: Counts([u32::MAX; 7]),
                blocks: vec![],
            })
            .collect(),
    };
    assert!(recording.validate(&spec()).is_err());
}

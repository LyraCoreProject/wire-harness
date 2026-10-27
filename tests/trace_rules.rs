use std::collections::BTreeMap;

use wire_client::compare::{compare, compare_observations, DeliberateDeviation, DeviationKind};
use wire_client::decode::{Field, Packet};
use wire_client::scenario::{Observation, Scenario, Stream};
use wire_client::trace::{GoldenTrace, Header, Provenance, RecordedField, Sha256Digest, Trace};

fn scenario(replies: &[u16], rules: &str) -> String {
    let base = include_str!("../scenarios/connection_identity.toml")
        .split("[[steps]]")
        .next()
        .unwrap();
    format!("{base}\n{rules}\n[[steps]]\nid = 'query'\nsend = {{ opcode = 'CMSG_PING', sequence = 7, latency_ms = 0 }}\ncheckpoint = {{ window_ms = 250, replies = {replies:?}, broadcasts = [{{opcode = 463, stream = 'clock'}}, {{opcode = 507, stream = 'rolls'}}] }}\n")
}

fn header(scenario: &str, now: u64) -> Header {
    Header::golden(
        scenario,
        Provenance {
            server: "vmangos".into(),
            binary_release: "test-release".into(),
            binary_sha256: Sha256Digest::of(b"test binary"),
            source_commit: "a".repeat(40),
            database_commit: "b".repeat(40),
        },
        now,
    )
    .unwrap()
}

fn packet(opcode: u16, value: Field) -> Observation {
    let stream = match opcode {
        463 => Stream::Broadcast("clock".into()),
        507 => Stream::Broadcast("rolls".into()),
        _ => Stream::Reply("query".into()),
    };
    Observation {
        checkpoint: "query".into(),
        stream,
        packet: Packet {
            opcode,
            fields: BTreeMap::from([("value".into(), value)]),
        },
    }
}

fn observations(fields: &[Field]) -> Vec<Observation> {
    fields.iter().map(|f| packet(477, f.clone())).collect()
}

fn golden(scenario: &str, observed: &[Observation], now: u64) -> GoldenTrace {
    GoldenTrace::new(
        scenario,
        Trace::record(header(scenario, now), observed).unwrap(),
    )
    .unwrap()
}

fn matches(expected: &[Field], actual: &[Field], rules: &str) -> bool {
    let text = scenario(&vec![477; expected.len()], rules);
    compare_observations(
        &golden(&text, &observations(expected), 1000),
        &observations(actual),
        1000,
        &[],
    )
    .unwrap()
    .is_match()
}

#[test]
fn recorder_hashes_nested_text_and_preserves_numbers() {
    let value = Field::Object(BTreeMap::from([
        ("name".into(), Field::Text("Alice".into())),
        (
            "nested".into(),
            Field::List(vec![
                Field::Text("private chat".into()),
                Field::Text(String::new()),
                Field::Float(12.5),
                Field::Guid(7),
            ]),
        ),
    ]));
    let golden = golden(&scenario(&[477], ""), &[packet(477, value)], 1000);
    let json = serde_json::to_string(golden.trace()).unwrap();
    assert!(!json.contains("Alice") && !json.contains("private chat"));
    assert!(json.contains("3bc51062973c458d5a6f2d8d64a023246354ad7e064b1e4e009ec8a0699a3043"));
    assert!(json.contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    assert_eq!(Trace::parse(json.as_bytes()).unwrap(), *golden.trace());
    let RecordedField::Object(fields) = &golden.trace().packets[0].fields["value"] else {
        panic!()
    };
    let RecordedField::List(values) = &fields["nested"] else {
        panic!()
    };
    assert_eq!(
        values[2..],
        [RecordedField::Float(12.5), RecordedField::Guid(7)]
    );
}

#[test]
fn trace_rejects_plaintext_invalid_digests_unknown_fields_and_unpinned_provenance() {
    let golden = golden(
        &scenario(&[477], ""),
        &[packet(477, Field::Text("abc".into()))],
        1000,
    );
    let json = serde_json::to_string(golden.trace()).unwrap();
    for invalid in [
        json.replace("text_sha256", "text"),
        json.replace(&String::from(Sha256Digest::of(b"abc")), "abc"),
        json.replace("\"schema_version\":2", "\"schema_version\":99"),
        json.replace(&"a".repeat(40), "latest"),
        json.replacen("{", "{\"plaintext\":\"secret\",", 1),
    ] {
        assert!(Trace::parse(invalid.as_bytes()).is_err());
    }
    let mut invalid = golden.trace().clone();
    invalid.packets[0]
        .fields
        .insert("value".into(), RecordedField::Float(f64::NAN));
    assert!(invalid.validate().is_err());
}

#[test]
fn rules_cannot_change_without_invalidating_the_golden_scenario_digest() {
    let text = scenario(&[477], "");
    let golden = golden(&text, &[packet(477, Field::TimeMs(1))], 1000);
    let changed = text.replace(
        "time_tolerance_ms = 100",
        "time_tolerance_ms = 9223372036854775807",
    );
    assert!(GoldenTrace::new(&changed, golden.trace().clone()).is_err());
    let changed = text + "\n[[rules.random]]\nname='everything'\nreason='too broad'\nopcode=477\npath=['value']\nrange={type='unsigned',minimum=0,maximum=9223372036854775807}\n";
    assert!(GoldenTrace::new(&changed, golden.trace().clone()).is_err());
}

#[test]
fn guid_bijection_is_consistent_both_ways_and_reset_per_comparison() {
    let expected = [Field::Guid(7), Field::Guid(8), Field::Guid(7)];
    assert!(matches(
        &expected,
        &[Field::Guid(70), Field::Guid(80), Field::Guid(70)],
        ""
    ));
    assert!(!matches(
        &expected,
        &[Field::Guid(70), Field::Guid(80), Field::Guid(90)],
        ""
    ));
    assert!(!matches(
        &expected,
        &[Field::Guid(70), Field::Guid(70), Field::Guid(70)],
        ""
    ));
    assert!(matches(
        &expected,
        &[Field::Guid(90), Field::Guid(80), Field::Guid(90)],
        ""
    ));
    assert!(!matches(&[Field::Guid(0)], &[Field::Guid(1)], ""));
    assert!(!matches(&[Field::Guid(1)], &[Field::Guid(0)], ""));
    assert!(matches(&[Field::Guid(0)], &[Field::Guid(0)], ""));
}

#[test]
fn guid_mapping_spans_checkpoints_and_nested_broadcast_fields() {
    let text = scenario(&[477], "") + "\n[[steps]]\nid='later'\nsend={opcode='CMSG_PING',sequence=7,latency_ms=0}\ncheckpoint={window_ms=250,replies=[477],broadcasts=[{opcode=507,stream='rolls'}]}\n";
    let mut observed = vec![
        packet(477, Field::Guid(1)),
        packet(477, Field::Guid(1)),
        packet(507, Field::List(vec![Field::Guid(1)])),
    ];
    observed[1].checkpoint = "later".into();
    observed[1].stream = Stream::Reply("later".into());
    observed[2].checkpoint = "later".into();
    let expected = golden(&text, &observed, 1000);
    observed[2]
        .packet
        .fields
        .insert("value".into(), Field::List(vec![Field::Guid(2)]));
    let report = compare_observations(&expected, &observed, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].rule, "guid_bijection");
}

#[test]
fn elapsed_times_use_the_scenario_tolerance_without_overflow() {
    assert!(matches(&[Field::TimeMs(1000)], &[Field::TimeMs(1100)], ""));
    assert!(matches(&[Field::TimeMs(1100)], &[Field::TimeMs(1000)], ""));
    assert!(!matches(&[Field::TimeMs(1000)], &[Field::TimeMs(1101)], ""));
    assert!(!matches(
        &[Field::TimeMs(0)],
        &[Field::TimeMs(u64::MAX)],
        ""
    ));
    let text = scenario(&[477], "").replace("time_tolerance_ms = 100", "time_tolerance_ms = 0");
    let expected = golden(&text, &[packet(477, Field::TimeMs(1))], 1000);
    assert!(
        !compare_observations(&expected, &[packet(477, Field::TimeMs(2))], 1000, &[])
            .unwrap()
            .is_match()
    );
}

#[test]
fn wall_clocks_check_each_recording_time_across_days_and_at_the_tolerance_boundary() {
    let text = scenario(&[477], "");
    let expected = golden(&text, &[packet(477, Field::WallClockMs(10_000))], 10_500);
    let later = 10_500 + 86_400_000;
    for (clock, equivalent) in [
        (later, true),
        (later - 2000, true),
        (later + 2000, true),
        (later - 2001, false),
        (10_000, false),
        (u64::MAX, false),
    ] {
        assert_eq!(
            compare_observations(
                &expected,
                &[packet(477, Field::WallClockMs(clock))],
                later,
                &[]
            )
            .unwrap()
            .is_match(),
            equivalent
        );
    }
    let stale = golden(&text, &[packet(477, Field::WallClockMs(0))], 10_500);
    assert!(!compare_observations(
        &stale,
        &[packet(477, Field::WallClockMs(later))],
        later,
        &[]
    )
    .unwrap()
    .is_match());
}

#[test]
fn replies_keep_order_while_unrelated_broadcasts_interleave() {
    let text = scenario(&[477, 81], "");
    let observed = vec![
        packet(477, Field::Unsigned(1)),
        packet(463, Field::Unsigned(3)),
        packet(81, Field::Unsigned(2)),
        packet(463, Field::Unsigned(4)),
        packet(507, Field::Unsigned(5)),
    ];
    let expected = golden(&text, &observed, 1000);
    let mut actual = [4, 1, 3, 0, 2].map(|i| observed[i].clone());
    assert!(compare_observations(&expected, &actual, 1000, &[])
        .unwrap()
        .is_match());
    actual.swap(3, 4);
    assert_eq!(
        compare_observations(&expected, &actual, 1000, &[])
            .unwrap()
            .differences[0]
            .rule,
        "packet_order"
    );
    actual.swap(3, 4);
    actual.swap(1, 2);
    assert!(!compare_observations(&expected, &actual, 1000, &[])
        .unwrap()
        .is_match());
}

fn random(range: &str, path: &str) -> String {
    format!("[[rules.random]]\nname='roll_result'\nreason='The server chooses this result at random.'\nopcode=477\npath={path}\nrange={range}")
}

#[test]
fn random_rules_check_type_and_inclusive_ranges_on_both_sides() {
    let rules = random("{type='unsigned',minimum=1,maximum=6}", "['value']");
    for (e, a, equivalent) in [
        (Field::Unsigned(1), Field::Unsigned(6), true),
        (Field::Unsigned(0), Field::Unsigned(6), false),
        (Field::Unsigned(1), Field::Unsigned(7), false),
        (Field::Unsigned(1), Field::Signed(1), false),
        (Field::Guid(1), Field::Guid(6), false),
    ] {
        assert_eq!(matches(&[e], &[a], &rules), equivalent);
    }
    assert!(matches(
        &[Field::Signed(-2)],
        &[Field::Signed(3)],
        &random("{type='signed',minimum=-2,maximum=3}", "['value']")
    ));
    assert!(matches(
        &[Field::Float(0.1)],
        &[Field::Float(0.9)],
        &random("{type='float',minimum=0.0,maximum=1.0}", "['value']")
    ));
}

#[test]
fn random_rules_require_unique_names_reasons_ranges_and_matching_paths() {
    let rule = random("{type='unsigned',minimum=1,maximum=6}", "['value']");
    let text = scenario(&[477], &rule);
    for invalid in [
        text.replace(
            "reason='The server chooses this result at random.'",
            "reason=''",
        ),
        text.clone() + "\n" + &rule,
        text.replace("maximum=6", "maximum=0"),
        text.replace(
            "type='unsigned',minimum=1,maximum=6",
            "type='float',minimum=0.0,maximum=inf",
        ),
    ] {
        assert!(Scenario::parse(&invalid).is_err());
    }
    assert!(!matches(
        &[Field::Unsigned(1)],
        &[Field::Unsigned(1)],
        &rule.replace("['value']", "['typo']")
    ));
    assert!(!matches(
        &[Field::Unsigned(1)],
        &[Field::Unsigned(6)],
        &rule.replace("opcode=477", "opcode=81")
    ));
    let nested = rule.replace("['value']", "['value','0']");
    assert!(matches(
        &[Field::List(vec![Field::Unsigned(1), Field::Unsigned(3)])],
        &[Field::List(vec![Field::Unsigned(6), Field::Unsigned(3)])],
        &nested
    ));
    assert!(!matches(
        &[Field::List(vec![Field::Unsigned(1), Field::Unsigned(3)])],
        &[Field::List(vec![Field::Unsigned(6), Field::Unsigned(4)])],
        &nested
    ));
}

#[test]
fn other_values_digests_types_and_container_shapes_are_exact() {
    for (e, a) in [
        (Field::Unsigned(1), Field::Unsigned(2)),
        (Field::Unsigned(1), Field::Signed(1)),
        (Field::Text("name".into()), Field::Text("other".into())),
        (Field::Float(1.0), Field::Float(1.0001)),
        (Field::List(vec![Field::Unsigned(1)]), Field::List(vec![])),
        (
            Field::Object(BTreeMap::from([("x".into(), Field::Unsigned(1))])),
            Field::Object(BTreeMap::new()),
        ),
    ] {
        assert!(!matches(
            std::slice::from_ref(&e),
            std::slice::from_ref(&a),
            ""
        ));
        assert!(!matches(&[a], &[e], ""));
    }
}

#[test]
fn empty_candidates_and_missing_extra_packets_return_differences() {
    let text = scenario(&[477, 477], "");
    let observed = observations(&[Field::Unsigned(1), Field::Unsigned(2)]);
    let expected = golden(&text, &observed, 1000);
    for actual in [
        vec![],
        observed[..1].to_vec(),
        [observed.clone(), observed[..1].to_vec()].concat(),
    ] {
        let report = compare_observations(&expected, &actual, 1000, &[]).unwrap();
        assert_eq!(report.differences[0].rule, "packet_count");
        assert_eq!(report.differences[0].checkpoint, "query");
    }
    let mut changed = expected.trace().clone();
    changed.header.scenario_sha256 = Sha256Digest::of(b"other revision");
    assert_eq!(
        compare(&expected, &changed, &[]).unwrap().differences[0].rule,
        "scenario_identity"
    );
}

#[test]
fn candidate_provenance_is_distinct_and_never_accepted_as_golden() {
    let text = scenario(&[477], "");
    let observed = observations(&[Field::Unsigned(7)]);
    let expected = golden(&text, &observed, 1000);
    let candidate = Trace::record(
        Header::candidate(&text, "lyracore".into(), 2000).unwrap(),
        &observed,
    )
    .unwrap();
    let json = serde_json::to_string(&candidate).unwrap();
    assert!(json.contains("candidate") && !json.contains("vmangos"));
    assert!(
        compare(&expected, &Trace::parse(json.as_bytes()).unwrap(), &[])
            .unwrap()
            .is_match()
    );
    assert!(GoldenTrace::new(&text, candidate).is_err());
}

#[test]
fn unexpected_packets_count_even_when_both_traces_contain_them_unless_scenario_excuses_them() {
    let rule = "[[rules.unexpected_packets]]\nname='ambient_update'\nreason='Object updates are outside this query.'\ncheckpoint='query'\nopcode=169";
    for (rules, count) in [("", 2), (rule, 0)] {
        let text = scenario(&[477], rules);
        let mut observed = observations(&[Field::Unsigned(7)]);
        observed.push(Observation {
            checkpoint: "query".into(),
            stream: Stream::Unexpected,
            packet: Packet {
                opcode: 169,
                fields: BTreeMap::new(),
            },
        });
        let expected = golden(&text, &observed, 1000);
        let report = compare_observations(&expected, &observed, 1000, &[]).unwrap();
        assert_eq!(report.differences.len(), count);
        assert!(report
            .differences
            .iter()
            .all(|d| d.rule == "unexpected_packet"));
        let report = compare_observations(&expected, &observed[..1], 1000, &[]).unwrap();
        assert_eq!(report.is_match(), count == 0);
    }
}

#[test]
fn deliberate_deviations_are_named_counted_and_scoped_to_actual_differences() {
    let text = scenario(&[477], "");
    let expected = golden(&text, &observations(&[Field::Unsigned(7)]), 1000);
    let deviation = DeliberateDeviation {
        name: "different_sequence".into(),
        reason: "This fixture deliberately returns eight.".into(),
        source: "test specification".into(),
        difference: DeviationKind::FieldValue {
            checkpoint: "query".into(),
            opcode: 477,
            path: vec!["value".into()],
            expected: RecordedField::Unsigned(7),
            actual: RecordedField::Unsigned(8),
        },
    };
    let report = compare_observations(
        &expected,
        &observations(&[Field::Unsigned(8)]),
        1000,
        std::slice::from_ref(&deviation),
    )
    .unwrap();
    assert!(!report.is_match());
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.deviation_count(), 1);
    assert_eq!(
        report.differences[0].deviations[0].name,
        "different_sequence"
    );
    for value in [7, 9] {
        let report = compare_observations(
            &expected,
            &observations(&[Field::Unsigned(value)]),
            1000,
            std::slice::from_ref(&deviation),
        )
        .unwrap();
        assert_eq!(report.deviation_count(), 0);
        assert!(report
            .differences
            .iter()
            .any(|d| d.rule == "unused_deviation" && d.reason.contains("different_sequence")));
    }
}

#[test]
fn unexpected_packet_deviations_report_every_occurrence() {
    let text = scenario(&[477], "");
    let observed = observations(&[Field::Unsigned(7)]);
    let expected = golden(&text, &observed, 1000);
    let mut actual = observed;
    for _ in 0..2 {
        actual.push(Observation {
            checkpoint: "query".into(),
            stream: Stream::Unexpected,
            packet: Packet {
                opcode: 169,
                fields: BTreeMap::new(),
            },
        });
    }
    let deviation = DeliberateDeviation {
        name: "extra_update".into(),
        reason: "Candidate sends an extra update.".into(),
        source: "test specification".into(),
        difference: DeviationKind::UnexpectedPacket {
            checkpoint: "query".into(),
            opcode: 169,
        },
    };
    let report = compare_observations(&expected, &actual, 1000, &[deviation]).unwrap();
    assert_eq!(report.differences.len(), 2);
    assert_eq!(report.deviation_count(), 2);
    assert!(!report.is_match());
}

#[test]
fn trace_writer_never_overwrites() {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".cache/lyracore-scratch/conformance/harness/trace-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("trace.json");
    let expected = golden(
        &scenario(&[477], ""),
        &observations(&[Field::Text("private".into())]),
        1000,
    );
    expected.trace().write_new(&path).unwrap();
    assert!(expected.trace().write_new(&path).is_err());
    assert_eq!(
        Trace::parse(&std::fs::read(&path).unwrap()).unwrap(),
        *expected.trace()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

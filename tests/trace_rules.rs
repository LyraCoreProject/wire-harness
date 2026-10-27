use std::collections::BTreeMap;

use wire_client::compare::{compare, EquivalenceRules, RandomRange, RandomRule};
use wire_client::decode::{Field, Packet};
use wire_client::scenario::{Observation, Stream};
use wire_client::trace::{Header, Provenance, RecordedField, Sha256Digest, Trace};

fn header() -> Header {
    Header::new(
        include_str!("../scenarios/connection_identity.toml"),
        Provenance {
            server: "vmangos".into(),
            binary_release: "test-release".into(),
            binary_sha256: Sha256Digest::of(b"test binary"),
            source_commit: "a".repeat(40),
            database_commit: "b".repeat(40),
        },
        1,
    )
    .unwrap()
}

fn packet(stream: Stream, opcode: u16, value: Field) -> Observation {
    Observation {
        checkpoint: "query".into(),
        stream,
        packet: Packet {
            opcode,
            fields: BTreeMap::from([("value".into(), value)]),
        },
    }
}

fn trace(fields: &[Field]) -> Trace {
    Trace::record(
        header(),
        &fields
            .iter()
            .map(|f| packet(Stream::Reply("query".into()), 1, f.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn matches(expected: &[Field], actual: &[Field]) -> bool {
    compare(
        &trace(expected),
        &trace(actual),
        &EquivalenceRules::default(),
    )
    .unwrap()
    .is_match()
}

#[test]
fn recorder_hashes_every_nested_text_and_preserves_numeric_fields() {
    let raw = Field::Object(BTreeMap::from([
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
    let trace = trace(&[raw]);
    let json = serde_json::to_string(&trace).unwrap();
    assert!(!json.contains("Alice") && !json.contains("private chat"));
    assert!(json.contains("3bc51062973c458d5a6f2d8d64a023246354ad7e064b1e4e009ec8a0699a3043"));
    assert!(json.contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    assert_eq!(Trace::parse(json.as_bytes()).unwrap(), trace);
    let RecordedField::Object(fields) = &trace.packets[0].fields["value"] else {
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
fn parser_rejects_plaintext_invalid_digest_unknown_fields_and_unpinned_headers() {
    let json = serde_json::to_string(&trace(&[Field::Text("abc".into())])).unwrap();
    for invalid in [
        json.replace("text_sha256", "text"),
        json.replace(&String::from(Sha256Digest::of(b"abc")), "abc"),
        json.replace("\"schema_version\":1", "\"schema_version\":2"),
        json.replace(&"a".repeat(40), "latest"),
        json.replacen("{", "{\"plaintext\":\"secret\",", 1),
    ] {
        assert!(Trace::parse(invalid.as_bytes()).is_err());
    }
    assert!(Trace::record(header(), &[]).is_err());
    let invalid = trace(&[Field::Unsigned(1)]);
    let mut bad = invalid.clone();
    bad.packets[0]
        .fields
        .insert("bad".into(), RecordedField::Float(f64::NAN));
    assert!(bad.validate().is_err());
}

#[test]
fn guid_bijection_is_global_consistent_in_both_directions_and_reset_per_trace() {
    let expected = [Field::Guid(7), Field::Guid(8), Field::Guid(7)];
    assert!(matches(
        &expected,
        &[Field::Guid(70), Field::Guid(80), Field::Guid(70)]
    ));
    assert!(!matches(
        &expected,
        &[Field::Guid(70), Field::Guid(80), Field::Guid(90)]
    ));
    assert!(!matches(
        &expected,
        &[Field::Guid(70), Field::Guid(70), Field::Guid(70)]
    ));
    assert!(matches(
        &expected,
        &[Field::Guid(90), Field::Guid(80), Field::Guid(90)]
    ));
    assert!(!matches(&[Field::Guid(0)], &[Field::Guid(1)]));
    assert!(!matches(&[Field::Guid(1)], &[Field::Guid(0)]));
    assert!(matches(&[Field::Guid(0)], &[Field::Guid(0)]));
}

#[test]
fn guid_mapping_spans_checkpoints_and_nested_broadcast_fields() {
    let mut e = trace(&[Field::Guid(1), Field::List(vec![Field::Guid(1)])]);
    e.packets[1].checkpoint = "later".into();
    e.packets[1].stream = Stream::Broadcast("actors".into());
    let mut a = e.clone();
    a.packets[0]
        .fields
        .insert("value".into(), RecordedField::Guid(9));
    a.packets[1].fields.insert(
        "value".into(),
        RecordedField::List(vec![RecordedField::Guid(8)]),
    );
    assert!(!compare(&e, &a, &EquivalenceRules::default())
        .unwrap()
        .is_match());
}

#[test]
fn times_accept_the_boundary_in_both_directions_without_overflow() {
    assert!(matches(&[Field::TimeMs(1000)], &[Field::TimeMs(1100)]));
    assert!(matches(&[Field::TimeMs(1100)], &[Field::TimeMs(1000)]));
    assert!(!matches(&[Field::TimeMs(1000)], &[Field::TimeMs(1101)]));
    assert!(!matches(&[Field::TimeMs(0)], &[Field::TimeMs(u64::MAX)]));
    let rules = EquivalenceRules {
        time_tolerance_ms: 0,
        ..Default::default()
    };
    assert!(!compare(
        &trace(&[Field::TimeMs(1)]),
        &trace(&[Field::TimeMs(2)]),
        &rules
    )
    .unwrap()
    .is_match());
}

#[test]
fn replies_keep_order_while_unrelated_broadcasts_may_interleave() {
    let observations = vec![
        packet(Stream::Reply("query".into()), 1, Field::Unsigned(1)),
        packet(Stream::Broadcast("other".into()), 3, Field::Unsigned(3)),
        packet(Stream::Reply("query".into()), 2, Field::Unsigned(2)),
        packet(Stream::Broadcast("other".into()), 4, Field::Unsigned(4)),
        packet(Stream::Broadcast("third".into()), 5, Field::Unsigned(5)),
    ];
    let e = Trace::record(header(), &observations).unwrap();
    let mut a = e.clone();
    a.packets = [4, 1, 3, 0, 2].map(|i| e.packets[i].clone()).into();
    assert!(compare(&e, &a, &EquivalenceRules::default())
        .unwrap()
        .is_match());
    a.packets.swap(3, 4);
    assert_eq!(
        compare(&e, &a, &EquivalenceRules::default())
            .unwrap()
            .differences[0]
            .rule,
        "packet_order"
    );
    a = e.clone();
    a.packets.swap(1, 3);
    assert!(!compare(&e, &a, &EquivalenceRules::default())
        .unwrap()
        .is_match());
}

fn random(range: RandomRange) -> EquivalenceRules {
    EquivalenceRules {
        time_tolerance_ms: 100,
        random: vec![RandomRule {
            name: "roll_result".into(),
            reason: "The server chooses this numeric result at random.".into(),
            opcode: 1,
            path: vec!["value".into()],
            range,
        }],
    }
}

#[test]
fn random_rolls_compare_shape_and_inclusive_range_on_both_sides() {
    let rules = random(RandomRange::Unsigned {
        minimum: 1,
        maximum: 6,
    });
    for (e, a, equivalent) in [
        (Field::Unsigned(1), Field::Unsigned(6), true),
        (Field::Unsigned(0), Field::Unsigned(6), false),
        (Field::Unsigned(1), Field::Unsigned(7), false),
        (Field::Unsigned(1), Field::Signed(1), false),
        (
            Field::Unsigned(1),
            Field::List(vec![Field::Unsigned(1)]),
            false,
        ),
    ] {
        assert_eq!(
            compare(&trace(&[e]), &trace(&[a]), &rules)
                .unwrap()
                .is_match(),
            equivalent
        );
    }
    let rules = random(RandomRange::Unsigned {
        minimum: u64::MAX - 1,
        maximum: u64::MAX,
    });
    assert!(!compare(
        &trace(&[Field::Unsigned(u64::MAX - 2)]),
        &trace(&[Field::Unsigned(u64::MAX)]),
        &rules
    )
    .unwrap()
    .is_match());
    for (range, e, a) in [
        (
            RandomRange::Signed {
                minimum: -2,
                maximum: 3,
            },
            Field::Signed(-2),
            Field::Signed(3),
        ),
        (
            RandomRange::Float {
                minimum: 0.0,
                maximum: 1.0,
            },
            Field::Float(0.1),
            Field::Float(0.9),
        ),
    ] {
        assert!(compare(&trace(&[e]), &trace(&[a]), &random(range))
            .unwrap()
            .is_match());
    }
}

#[test]
fn random_exceptions_require_unique_names_reasons_valid_ranges_and_matching_paths() {
    let valid = random(RandomRange::Unsigned {
        minimum: 1,
        maximum: 6,
    });
    let mut invalid = valid.clone();
    invalid.random[0].reason.clear();
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.random.push(invalid.random[0].clone());
    assert!(invalid.validate().is_err());
    invalid = valid.clone();
    invalid.random[0].range = RandomRange::Float {
        minimum: 0.0,
        maximum: f64::INFINITY,
    };
    assert!(invalid.validate().is_err());
    invalid.random[0].range = RandomRange::Signed {
        minimum: 2,
        maximum: 1,
    };
    assert!(invalid.validate().is_err());
    let mut unused = valid;
    unused.random[0].path = vec!["typo".into()];
    let result = compare(
        &trace(&[Field::Unsigned(1)]),
        &trace(&[Field::Unsigned(1)]),
        &unused,
    )
    .unwrap();
    assert!(!result.is_match());
}

#[test]
fn unruled_values_digests_types_and_container_shapes_are_exact() {
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
        assert!(!matches(std::slice::from_ref(&e), std::slice::from_ref(&a)));
        assert!(!matches(&[a], &[e]));
    }
}

#[test]
fn missing_extra_packets_and_scenario_revisions_fail_with_report_locations() {
    let e = trace(&[Field::Unsigned(1), Field::Unsigned(2)]);
    let a = trace(&[Field::Unsigned(1)]);
    let report = compare(&e, &a, &EquivalenceRules::default()).unwrap();
    assert_eq!(report.differences[0].rule, "packet_count");
    assert_eq!(report.differences[0].checkpoint, "query");
    assert!(serde_json::to_string(&report)
        .unwrap()
        .contains("packet_count"));
    assert!(!compare(&a, &e, &EquivalenceRules::default())
        .unwrap()
        .is_match());
    let mut changed = e.clone();
    changed.header.scenario_sha256 = Sha256Digest::of(b"other revision");
    assert_eq!(
        compare(&e, &changed, &EquivalenceRules::default())
            .unwrap()
            .differences[0]
            .rule,
        "scenario_identity"
    );
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
    let t = trace(&[Field::Text("private".into())]);
    t.write_new(&path).unwrap();
    assert!(t.write_new(&path).is_err());
    let written = std::fs::read(&path).unwrap();
    assert_eq!(Trace::parse(&written).unwrap(), t);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn random_paths_are_scoped_to_the_opcode_and_nested_location() {
    let mut rules = random(RandomRange::Unsigned {
        minimum: 1,
        maximum: 6,
    });
    rules.random[0].path = vec!["value".into(), "0".into()];
    let e = trace(&[Field::List(vec![Field::Unsigned(1), Field::Unsigned(3)])]);
    let a = trace(&[Field::List(vec![Field::Unsigned(6), Field::Unsigned(3)])]);
    assert!(compare(&e, &a, &rules).unwrap().is_match());
    let changed = trace(&[Field::List(vec![Field::Unsigned(6), Field::Unsigned(4)])]);
    assert!(!compare(&e, &changed, &rules).unwrap().is_match());
    rules.random[0].opcode = 2;
    assert!(!compare(&e, &a, &rules).unwrap().is_match());
}

#[test]
fn committed_recording_parses_and_names_the_current_scenario_revision() {
    let recorded = Trace::parse(include_bytes!("../traces/connection_identity.json")).unwrap();
    let scenario = include_str!("../scenarios/connection_identity.toml");
    let expected = Header::new(scenario, recorded.header.provenance.clone(), 0).unwrap();
    assert_eq!(recorded.header.scenario, expected.scenario);
    assert_eq!(recorded.header.behavior, expected.behavior);
    assert_eq!(recorded.header.scenario_sha256, expected.scenario_sha256);
    assert_eq!(recorded.header.provenance.server, "vmangos");
}

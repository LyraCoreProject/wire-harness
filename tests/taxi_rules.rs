use std::collections::BTreeMap;
use wire_client::compare::{compare, compare_character_observations};
use wire_client::decode::{Field, Packet};
use wire_client::scenario::{Observation, Stream};
use wire_client::trace::{GoldenTrace, Header, Provenance, Sha256Digest, Trace};

const SCENARIO: &str = r#"
schema_version = 3
id = "flight_rules"
behavior = "flight_rules"
[opening]
window_ms = 1
[rules]
time_tolerance_ms = 1
wall_clock_tolerance_ms = 2000
[rules.character_scope]
name = "passenger"
reason = "Scope this test to the Character's flight state."
packets = [{opcode=221,paths=[["guid"]]}]
[[rules.identities]]
name = "spline"
reason = "Spline ids are server-local identities."
opcode = 221
path = ["spline_id"]
[[fixtures]]
verb = "character"
name = "Tester"
race = "human"
class = "warrior"
level = 1
at = { map=0, x=0.0, y=0.0, z=0.0, orientation=0.0 }
[[steps]]
id = "flight"
send = { opcode="CMSG_PING", sequence=7, latency_ms=0 }
checkpoint = { window_ms=1, replies=[477], broadcasts=[{opcode=221,stream="movement"},{opcode=169,stream="values"}] }
"#;

fn packet(opcode: u16, fields: BTreeMap<String, Field>) -> Observation {
    Observation {
        received_after_ms: None,
        checkpoint: "flight".into(),
        stream: match opcode {
            477 => Stream::Reply("flight".into()),
            221 => Stream::Broadcast("movement".into()),
            169 => Stream::Broadcast("values".into()),
            _ => Stream::Unexpected,
        },
        packet: Packet { opcode, fields },
    }
}

fn movement(guid: u64, spline: u64) -> Observation {
    packet(
        221,
        BTreeMap::from([
            ("guid".into(), Field::Guid(guid)),
            ("spline_id".into(), Field::Unsigned(spline)),
            ("duration".into(), Field::TimeMs(1000)),
        ]),
    )
}

fn update(guid: u64, values: &[(u16, u64)]) -> Observation {
    packet(
        169,
        BTreeMap::from([(
            "objects".into(),
            Field::List(vec![Field::Object(BTreeMap::from([
                ("guid".into(), Field::Guid(guid)),
                ("kind".into(), Field::Unsigned(0)),
                (
                    "values".into(),
                    Field::Object(
                        values
                            .iter()
                            .map(|(index, value)| {
                                (format!("field_{index}"), Field::Unsigned(*value))
                            })
                            .collect(),
                    ),
                ),
            ]))]),
        )]),
    )
}

fn base(guid: u64) -> Vec<Observation> {
    vec![
        packet(
            477,
            BTreeMap::from([("sequence".into(), Field::Unsigned(7))]),
        ),
        movement(guid, 5),
        update(guid, &[(133, 1147), (1176, 890)]),
        update(guid, &[(133, 0)]),
    ]
}

fn golden(text: &str, observations: &[Observation]) -> GoldenTrace {
    let mut header = Header::golden(
        text,
        Provenance {
            server: "vmangos".into(),
            binary_release: "test".into(),
            binary_sha256: Sha256Digest::of(b"binary"),
            source_commit: "a".repeat(40),
            database_commit: "b".repeat(40),
        },
        1000,
    )
    .unwrap();
    header.character_guid = 7;
    GoldenTrace::new(text, Trace::record(header, observations).unwrap()).unwrap()
}

#[test]
fn passenger_scope_retains_every_own_field_change_across_different_packet_batches() {
    let expected = golden(SCENARIO, &base(7));
    let mut actual = base(70);
    actual[2] = update(70, &[(133, 1147)]);
    actual.insert(3, update(70, &[(1176, 890)]));
    actual.insert(2, movement(99, 123));
    actual.insert(3, update(99, &[(133, 42)]));

    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    actual.push(update(70, &[(133, 0)]));
    assert!(
        !compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
}

#[test]
fn scope_never_excuses_missing_passenger_movement_bad_decodes_or_unknown_packets() {
    let expected = golden(SCENARIO, &base(7));
    for mutation in 0..4 {
        let mut actual = base(70);
        match mutation {
            0 => {
                actual.remove(1);
            }
            1 => {
                actual.push(packet(0xffff, BTreeMap::new()));
            }
            2 => {
                actual[2].stream = Stream::Malformed;
                actual[2].packet.fields.clear();
            }
            _ => {
                actual[1].stream = Stream::Unexpected;
                actual[1].packet.fields.clear();
            }
        }
        assert!(
            !compare_character_observations(&expected, &actual, 70, 1000, &[])
                .unwrap()
                .is_match()
        );
    }
    assert!(compare_character_observations(&expected, &base(7), 0, 1000, &[]).is_err());
}

#[test]
fn spline_identity_is_bijective_and_does_not_mask_duration() {
    let mut observations = base(7);
    observations.push(movement(7, 6));
    let expected = golden(SCENARIO, &observations);
    let mut actual = base(70);
    actual[1] = movement(70, 50);
    actual.push(movement(70, 60));
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    *actual.last_mut().unwrap() = movement(70, 50);
    assert!(
        !compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    *actual.last_mut().unwrap() = movement(70, 0);
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    *actual.last_mut().unwrap() = movement(70, 60);
    actual[1]
        .packet
        .fields
        .insert("duration".into(), Field::TimeMs(1001));
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    actual[1]
        .packet
        .fields
        .insert("duration".into(), Field::TimeMs(1002));
    assert!(
        !compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
}

#[test]
fn a_recording_without_the_declared_landing_cannot_be_golden() {
    let text = SCENARIO.replace("window_ms=1, replies=", "expect=[{opcode=169,path=['objects','0','values','field_133'],value={type='unsigned',value=0},count=1}], window_ms=1, replies=");
    let mut observed = base(7);
    let good = golden(&text, &observed);
    assert!(compare(&good, good.trace(), &[]).unwrap().is_match());
    observed.pop();
    let missing = golden(&text, &observed);
    assert!(compare(&missing, missing.trace(), &[])
        .unwrap()
        .differences
        .iter()
        .any(|d| d.rule == "checkpoint_assertion"));
}

#[test]
fn landing_clock_handles_wrap_and_delivery_tolerance_is_inclusive() {
    let text = SCENARIO.replace(
        "[[fixtures]]",
        r#"
[[rules.clocks]]
name = "clock"
reason = "Server monotonic clocks have different origins."
opcode = 221
path = ["timestamp"]
tolerance_ms = 100
[[rules.packet_timing]]
name = "arrival"
reason = "One map tick plus delivery jitter."
opcode = 221
tolerance_ms = 150
[[fixtures]]"#,
    );
    let make = |guid, clock: u32, arrival| {
        let mut first = movement(guid, 1);
        first
            .packet
            .fields
            .insert("timestamp".into(), Field::Unsigned(clock.into()));
        first.received_after_ms = Some(10);
        let mut last = movement(guid, 2);
        last.packet.fields.insert(
            "timestamp".into(),
            Field::Unsigned(clock.wrapping_add(1000).into()),
        );
        last.received_after_ms = Some(arrival);
        vec![base(guid)[0].clone(), first, last]
    };
    let expected = golden(&text, &make(7, u32::MAX - 500, 1010));
    let mut actual = make(70, 5000, 1160);
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    for arrival in [Some(1161), None] {
        actual[2].received_after_ms = arrival;
        let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
        assert!(report.differences.iter().any(|d| d.rule == "arrival"));
    }
    actual[2].received_after_ms = Some(1010);
    for (timestamp, matches) in [(6100, true), (6101, false), (u32::MAX as u64 + 1, false)] {
        actual[2]
            .packet
            .fields
            .insert("timestamp".into(), Field::Unsigned(timestamp));
        assert_eq!(
            compare_character_observations(&expected, &actual, 70, 1000, &[])
                .unwrap()
                .is_match(),
            matches
        );
    }
}

#[test]
fn scoped_updates_still_report_the_passenger_leaving_visibility() {
    let expected = golden(SCENARIO, &base(7));
    let mut actual = base(70);
    actual.push(packet(
        169,
        BTreeMap::from([(
            "objects".into(),
            Field::List(vec![Field::Object(BTreeMap::from([
                ("kind".into(), Field::Unsigned(4)),
                ("guids".into(), Field::List(vec![Field::Guid(70)])),
            ]))]),
        )]),
    ));
    assert!(
        !compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
}

#[test]
fn nearby_spells_are_excluded_only_when_the_passenger_is_not_a_subject() {
    let text = SCENARIO.replace(
        "packets = [{opcode=221,paths=[[\"guid\"]]}]",
        "packets = [{opcode=221,paths=[[\"guid\"]]}, {opcode=305,paths=[[\"caster\"],[\"targets\"]]}]"
    ).replace("{opcode=169,stream=\"values\"}", "{opcode=169,stream=\"values\"},{opcode=305,stream=\"spells\"}");
    let expected = golden(&text, &base(7));
    let mut actual = base(70);
    let mut spell = packet(
        305,
        BTreeMap::from([
            ("caster".into(), Field::Guid(99)),
            (
                "targets".into(),
                Field::Object(BTreeMap::from([("unit".into(), Field::Guid(100))])),
            ),
        ]),
    );
    spell.stream = Stream::Broadcast("spells".into());
    actual.push(spell);
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    actual.last_mut().unwrap().packet.fields.insert(
        "targets".into(),
        Field::Object(BTreeMap::from([("unit".into(), Field::Guid(70))])),
    );
    assert!(
        !compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
}

#[test]
fn passenger_flags_and_undeclared_passenger_fields_remain_strict() {
    let mut observed = base(7);
    observed.push(update(7, &[(190, 0)]));
    let expected = golden(SCENARIO, &observed);
    for fields in [vec![(190, 1)], vec![(190, 0), (22, 100)]] {
        let mut actual = base(70);
        actual.push(update(70, &fields));
        let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
        assert_eq!(report.differences.len(), 1);
    }
}

#[test]
fn fare_deducted_at_landing_fails_even_when_packet_batching_differs() {
    let text = SCENARIO.replace(
        "[[fixtures]]",
        r#"
[[rules.packet_timing]]
name = "passenger_delivery"
reason = "One map tick plus local delivery jitter."
opcode = 169
tolerance_ms = 150
[[fixtures]]"#,
    );
    let mut observed = base(7);
    observed[2].received_after_ms = Some(60);
    observed[3].received_after_ms = Some(73550);
    let expected = golden(&text, &observed);
    let mut actual = base(70);
    actual[2] = update(70, &[(133, 1147)]);
    actual[2].received_after_ms = Some(60);
    actual[3].received_after_ms = Some(73550);
    let mut fare = update(70, &[(1176, 890)]);
    fare.received_after_ms = Some(210);
    actual.push(fare);
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    actual[4].received_after_ms = Some(73550);
    let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].rule, "passenger_delivery");
}

#[test]
fn absent_packet_does_not_add_unused_identity_clock_or_timing_records() {
    let text = SCENARIO.replace(
        "[[fixtures]]",
        r#"
[[rules.clocks]]
name = "clock"
reason = "Monotonic clock origin; unchecked: single sample."
opcode = 221
path = ["timestamp"]
tolerance_ms = 100
[[rules.packet_timing]]
name = "arrival"
reason = "One map tick plus local delivery jitter."
opcode = 221
tolerance_ms = 150
[[fixtures]]"#,
    );
    let mut observed = base(7);
    observed[1].received_after_ms = Some(10);
    observed[1]
        .packet
        .fields
        .insert("timestamp".into(), Field::Unsigned(100));
    let expected = golden(&text, &observed);
    let mut actual = base(70);
    actual.remove(1);
    let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].rule, "missing_packet");
}

#[test]
fn single_identity_and_clock_samples_require_unsigned_32_bit_values() {
    let text = SCENARIO.replace(
        "[[fixtures]]",
        r#"
[[rules.clocks]]
name = "clock"
reason = "Monotonic clock origin; unchecked: single sample."
opcode = 221
path = ["timestamp"]
tolerance_ms = 100
[[fixtures]]"#,
    );
    let mut observed = base(7);
    observed[1]
        .packet
        .fields
        .insert("timestamp".into(), Field::Unsigned(100));
    let expected = golden(&text, &observed);
    for field in ["timestamp", "spline_id"] {
        for invalid in [
            Field::Unsigned(u64::from(u32::MAX) + 1),
            Field::Signed(1),
            Field::Float(1.0),
        ] {
            let mut actual = observed.clone();
            actual[1].packet.fields.insert(field.into(), invalid);
            let report = compare_character_observations(&expected, &actual, 7, 1000, &[]).unwrap();
            assert_eq!(report.differences.len(), 1);
            assert_eq!(report.differences[0].field_path, [field]);
        }
    }
}

#[test]
fn zone_packets_keep_presence_order_delivery_and_route_identity() {
    let expected = wire_client::catalogue::get("taxi_single_hop").unwrap();
    let zone_packets: Vec<_> = expected
        .trace()
        .packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet.checkpoint == "flight" && matches!(packet.opcode, 706 | 756))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(zone_packets.len(), 4);
    for index in &zone_packets {
        let mut actual = expected.trace().clone();
        actual.packets.remove(*index);
        let report = compare(&expected, &actual, &[]).unwrap();
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].rule, "missing_packet");
    }
    let mut actual = expected.trace().clone();
    actual.packets.swap(zone_packets[0], zone_packets[1]);
    let report = compare(&expected, &actual, &[]).unwrap();
    assert_eq!(report.differences.len(), 2);
    assert!(report
        .differences
        .iter()
        .all(|d| matches!(d.rule.as_str(), "missing_packet" | "extra_packet")));
    for (offset, matches) in [(1150, true), (1151, false)] {
        let mut actual = expected.trace().clone();
        *actual.packets[zone_packets[0]]
            .received_after_ms
            .as_mut()
            .unwrap() += offset;
        assert_eq!(
            compare(&expected, &actual, &[]).unwrap().is_match(),
            matches
        );
    }
    for field in ["map", "zone"] {
        let mut actual = expected.trace().clone();
        actual.packets[zone_packets[0]].fields.insert(
            field.into(),
            wire_client::trace::RecordedField::Unsigned(999),
        );
        let report = compare(&expected, &actual, &[]).unwrap();
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].field_path, [field]);
    }
}

#[test]
fn zone_data_exceptions_cover_only_the_named_fields() {
    use wire_client::trace::RecordedField as Recorded;
    let expected = wire_client::catalogue::get("taxi_single_hop").unwrap();
    let mut actual = expected.trace().clone();
    for packet in actual
        .packets
        .iter_mut()
        .filter(|packet| packet.checkpoint == "flight")
    {
        match packet.opcode {
            706 => {
                packet
                    .fields
                    .insert("states".into(), Recorded::List(vec![]));
            }
            756 => {
                packet
                    .fields
                    .insert("weather_type".into(), Recorded::Unsigned(1));
                packet
                    .fields
                    .insert("sound".into(), Recorded::Unsigned(8533));
                packet.fields.insert("grade".into(), Recorded::Float(0.5));
            }
            _ => (),
        }
    }
    assert!(compare(&expected, &actual, &[]).unwrap().is_match());
    let weather = actual
        .packets
        .iter_mut()
        .find(|packet| packet.checkpoint == "flight" && packet.opcode == 756)
        .unwrap();
    weather
        .fields
        .insert("instant".into(), Recorded::Unsigned(1));
    let report = compare(&expected, &actual, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].field_path, ["instant"]);
}

#[test]
fn missing_passenger_update_does_not_shift_later_changes_with_remapped_guids() {
    let mut observed = base(7);
    observed.extend((0..20).map(|value| update(7, &[(190, value)])));
    let expected = golden(SCENARIO, &observed);
    let mut actual = base(70);
    actual.extend(
        (0..20)
            .filter(|value| *value != 5)
            .map(|value| update(70, &[(190, value)])),
    );
    let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].rule, "missing_packet");
    assert_eq!(report.differences[0].packet_index, Some(5));
}

#[test]
fn zone_flag_timing_does_not_relax_other_passenger_updates() {
    let text = SCENARIO.replace(
        "[[fixtures]]",
        r#"
[[rules.packet_timing]]
name = "passenger_delivery"
reason = "One map tick plus local delivery jitter."
opcode = 169
tolerance_ms = 150
[[rules.packet_timing]]
name = "zone_flag_delivery"
reason = "Zone polling plus a map tick and local delivery jitter."
opcode = 169
path = ["objects", "0", "values", "field_190"]
tolerance_ms = 1150
[[fixtures]]"#,
    );
    let make = |guid| {
        let mut observed = base(guid);
        observed[2].received_after_ms = Some(60);
        observed[3].received_after_ms = Some(73550);
        let mut flag = update(guid, &[(190, 0)]);
        flag.received_after_ms = Some(10800);
        observed.push(flag);
        observed
    };
    let expected = golden(&text, &make(7));
    let mut actual = make(70);
    actual[4].received_after_ms = Some(11950);
    assert!(
        compare_character_observations(&expected, &actual, 70, 1000, &[])
            .unwrap()
            .is_match()
    );
    actual[4].received_after_ms = Some(11951);
    let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].rule, "zone_flag_delivery");
    actual[4].received_after_ms = Some(11950);
    actual[2].received_after_ms = Some(211);
    let report = compare_character_observations(&expected, &actual, 70, 1000, &[]).unwrap();
    assert_eq!(report.differences.len(), 2);
    assert!(report
        .differences
        .iter()
        .all(|difference| difference.rule == "passenger_delivery"));
}

#[test]
fn landing_delivery_keeps_its_150_ms_boundary() {
    let expected = wire_client::catalogue::get("taxi_single_hop").unwrap();
    for (delay, matches) in [(150, true), (151, false)] {
        let mut actual = expected.trace().clone();
        let landing = actual
            .packets
            .iter_mut()
            .find(|packet| packet.checkpoint == "flight" && packet.opcode == 199)
            .unwrap();
        *landing.received_after_ms.as_mut().unwrap() += delay;
        let report = compare(&expected, &actual, &[]).unwrap();
        assert_eq!(report.is_match(), matches);
        if !matches {
            assert_eq!(report.differences.len(), 1);
            assert_eq!(report.differences[0].rule, "landing_delivery");
        }
    }
}

#[test]
fn missing_takeoff_reply_does_not_hide_the_flight_duration_difference() {
    use wire_client::trace::RecordedField;
    let expected = wire_client::catalogue::get("taxi_single_hop").unwrap();
    let mut actual = expected.trace().clone();
    let mount = actual
        .packets
        .iter()
        .position(|packet| packet.checkpoint == "flight" && packet.opcode == 366)
        .unwrap();
    actual.packets.remove(mount);
    let flight = actual
        .packets
        .iter_mut()
        .find(|packet| {
            packet.checkpoint == "flight"
                && packet.opcode == 221
                && packet.stream == Stream::Reply("flight".into())
        })
        .unwrap();
    let RecordedField::TimeMs(duration) = flight.fields.get_mut("duration").unwrap() else {
        panic!()
    };
    *duration += 2;
    let report = compare(&expected, &actual, &[]).unwrap();
    assert_eq!(report.differences.len(), 2);
    assert_eq!(report.differences[0].rule, "missing_packet");
    assert_eq!(report.differences[0].opcode, Some(366));
    assert_eq!(report.differences[1].rule, "time_tolerance");
    assert_eq!(report.differences[1].field_path, ["duration"]);
}

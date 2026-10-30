use std::collections::BTreeMap;

use wire_client::compare::compare;
use wire_client::strictness::{analyze, check_allowlist, StrictnessReport, Survivor};
use wire_client::trace::{GoldenTrace, RecordedField, Sha256Digest, Trace};

fn historical(scenario: &str, trace: &str) -> GoldenTrace {
    GoldenTrace::new(scenario, Trace::parse(trace.as_bytes()).unwrap()).unwrap()
}

fn has_id(report: &StrictnessReport, key: &str, id: &str) -> bool {
    serde_json::to_value(report).unwrap()[key]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(id)))
}

#[test]
fn report_is_byte_stable() {
    let golden = wire_client::catalogue::get("connection_identity").unwrap();
    let report = analyze(&golden).unwrap();
    assert!(
        report.rule_intents["ambient_movement_during_ping"].contains("unsubjected packets false")
    );
    let first = serde_json::to_vec(&report).unwrap();
    let second = serde_json::to_vec(&analyze(&golden).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn allowlist_requires_exact_survivors_and_reasons() {
    let report = StrictnessReport {
        schema_version: 2,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 1,
        detected: 0,
        detected_mutations: Vec::new(),
        excused_by_rule: BTreeMap::new(),
        excused_mutations: BTreeMap::new(),
        rule_intents: BTreeMap::new(),
        unexplained_survivors: vec![Survivor {
            id: "mutation".into(),
            description: "changed packet".into(),
        }],
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[{"scenario":"sample","id":"mutation","reason":"Needs a narrower rule"}]}"#).is_ok());
    let report = StrictnessReport {
        schema_version: 2,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 1,
        detected: 0,
        detected_mutations: Vec::new(),
        excused_by_rule: BTreeMap::new(),
        excused_mutations: BTreeMap::new(),
        rule_intents: BTreeMap::new(),
        unexplained_survivors: vec![Survivor {
            id: "mutation".into(),
            description: "changed packet".into(),
        }],
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[]}"#).is_err());
    let report = StrictnessReport {
        schema_version: 2,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 0,
        detected: 0,
        detected_mutations: Vec::new(),
        excused_by_rule: BTreeMap::new(),
        excused_mutations: BTreeMap::new(),
        rule_intents: BTreeMap::new(),
        unexplained_survivors: Vec::new(),
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[{"scenario":"sample","id":"mutation","reason":"Needs a narrower rule"}]}"#).is_err());
}

#[test]
fn pre37_bank_character_movement_is_unexplained() {
    let golden = historical(
        include_str!("fixtures/strictness/bank_pre37.toml"),
        include_str!("fixtures/strictness/bank_pre37.json"),
    );
    let report = analyze(&golden).unwrap();
    assert!(report
        .unexplained_survivors
        .iter()
        .any(|item| item.id == "inject_open/221/single_character_subject"));
    assert!(report.rule_intents["ambient_movement_during_open"]
        .contains("subject GUID high bits {}, unsubjected packets true"));
}

#[test]
fn bank_reply_must_name_the_banker_created_during_opening() {
    let golden = wire_client::catalogue::get("bank_slot_purchase").unwrap();
    for guid in [golden.trace().header.character_guid, 999999] {
        let mut candidate = golden.trace().clone();
        let reply = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.checkpoint == "open" && packet.opcode == 440)
            .unwrap();
        reply
            .fields
            .insert("guid".into(), RecordedField::Guid(guid));
        assert!(!compare(&golden, &candidate, &[]).unwrap().is_match());
        let invalid = GoldenTrace::new(
            include_str!("../scenarios/bank_slot_purchase.toml"),
            candidate.clone(),
        )
        .unwrap();
        assert!(!compare(&invalid, &candidate, &[]).unwrap().is_match());
    }
}

#[test]
fn pre39_visibility_detects_peer_create_max_health() {
    let golden = historical(
        include_str!("fixtures/strictness/visibility_pre39.toml"),
        include_str!("fixtures/strictness/visibility_pre39.json"),
    );
    let report = analyze(&golden).unwrap();
    let peer_guid = golden.trace().header.characters["arriving"];
    let mut peer_create = None;
    for (index, packet) in golden.trace().packets.iter().enumerate() {
        if packet.checkpoint != "arrival"
            || packet.opcode != 169
            || packet.character.as_deref() != Some("watcher")
        {
            continue;
        }
        let Some(RecordedField::List(objects)) = packet.fields.get("objects") else {
            continue;
        };
        for (object_index, object) in objects.iter().enumerate() {
            let RecordedField::Object(fields) = object else {
                continue;
            };
            if fields.get("guid") != Some(&RecordedField::Guid(peer_guid))
                || fields.get("kind") != Some(&RecordedField::Unsigned(2))
            {
                continue;
            }
            let Some(RecordedField::Object(values)) = fields.get("values") else {
                continue;
            };
            if values.contains_key("field_28") {
                peer_create = Some((index, object_index));
            }
        }
    }
    let (index, object_index) = peer_create.expect("peer CREATE has max health");
    let id = format!("packet_{index}/field_objects.{object_index}.values.field_28/plus_one");
    let mut candidate = golden.trace().clone();
    let RecordedField::List(objects) = candidate.packets[index].fields.get_mut("objects").unwrap()
    else {
        panic!("peer CREATE has objects");
    };
    let RecordedField::Object(fields) = &mut objects[object_index] else {
        panic!("peer CREATE has an object");
    };
    let Some(RecordedField::Object(values)) = fields.get_mut("values") else {
        panic!("peer CREATE has values");
    };
    let Some(RecordedField::Unsigned(max_health)) = values.get_mut("field_28") else {
        panic!("peer CREATE has max health");
    };
    *max_health += 1;
    assert!(!compare(&golden, &candidate, &[]).unwrap().is_match());
    assert!(
        has_id(&report, "detected_mutations", &id),
        "a faulty peer CREATE projection would leave {id} unexplained"
    );
}

#[test]
fn declared_create_labelled_opening_is_detected() {
    let golden = wire_client::catalogue::get("two_character_visibility").unwrap();
    let report = analyze(&golden).unwrap();
    assert!(has_id(
        &report,
        "detected_mutations",
        "inject_arrival/169/arriving_opening"
    ));
}

#[test]
fn pong_timing_bound_is_mutated() {
    let entry = wire_client::catalogue::ENTRIES
        .iter()
        .find(|entry| entry.id == "connection_identity")
        .unwrap();
    let scenario = format!(
        "{}\n[[rules.packet_timing]]\nname = 'pong_delivery'\nreason = 'PONG arrival is allowed within ten seconds.'\nopcode = 477\ntolerance_ms = 10000\n",
        entry.scenario
    );
    let mut trace = Trace::parse(entry.trace.as_bytes()).unwrap();
    trace.header.scenario_sha256 = Sha256Digest::of(scenario.as_bytes());
    let golden = GoldenTrace::new(&scenario, trace).unwrap();
    let report = analyze(&golden).unwrap();
    let index = golden
        .trace()
        .packets
        .iter()
        .position(|p| p.opcode == 477)
        .unwrap();
    assert!(has_id(
        &report,
        "detected_mutations",
        &format!("packet_{index}/received_after_ms/past_bound")
    ));
    assert!(
        serde_json::to_value(&report).unwrap()["excused_mutations"]["pong_delivery"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str()
                    == Some(&format!("packet_{index}/received_after_ms/at_bound"))))
    );
}

#[test]
fn field_time_tolerance_explains_mutations_inside_the_fallback_bound() {
    let entry = wire_client::catalogue::ENTRIES
        .iter()
        .find(|entry| entry.id == "connection_identity")
        .unwrap();
    let scenario = format!("{}\n[[rules.tolerances]]\nname = 'sample_duration'\nreason = 'Sampling may vary by 250 ms.'\nopcode = 477\npath = ['duration']\ntolerance = 250.0\n", entry.scenario);
    let mut trace = Trace::parse(entry.trace.as_bytes()).unwrap();
    trace.header.scenario_sha256 = Sha256Digest::of(scenario.as_bytes());
    let index = trace
        .packets
        .iter()
        .position(|packet| packet.opcode == 477)
        .unwrap();
    trace.packets[index]
        .fields
        .insert("duration".into(), RecordedField::TimeMs(1000));
    let report = analyze(&GoldenTrace::new(&scenario, trace).unwrap()).unwrap();
    for suffix in ["plus_one", "minus_one", "at_bound"] {
        assert!(report.excused_mutations["sample_duration"]
            .contains(&format!("packet_{index}/field_duration/{suffix}")));
    }
    assert!(report
        .detected_mutations
        .contains(&format!("packet_{index}/field_duration/past_bound")));
}

#[test]
fn field_tolerance_indices_follow_the_scoped_stream() {
    let entry = wire_client::catalogue::ENTRIES
        .iter()
        .find(|entry| entry.id == "connection_identity")
        .unwrap();
    let scenario = format!("{}\n[rules.character_scope]\nname = 'actor_only'\nreason = 'Only the actor replies belong to this check.'\npackets = [{{ opcode = 477, paths = [['guid']] }}]\n[[rules.tolerances]]\nname = 'sample_duration'\nreason = 'Sampling may vary by 250 ms.'\nopcode = 477\npath = ['duration']\npacket_index = 0\ntolerance = 250.0\n", entry.scenario);
    let mut trace = Trace::parse(entry.trace.as_bytes()).unwrap();
    trace.header.scenario_sha256 = Sha256Digest::of(scenario.as_bytes());
    let index = trace
        .packets
        .iter()
        .position(|packet| packet.opcode == 477)
        .unwrap();
    trace.packets[index].fields.insert(
        "guid".into(),
        RecordedField::Guid(trace.header.character_guid),
    );
    trace.packets[index]
        .fields
        .insert("duration".into(), RecordedField::TimeMs(1000));
    let mut ambient = trace.packets[index].clone();
    ambient
        .fields
        .insert("guid".into(), RecordedField::Guid(0xf130_0001_0000_0001));
    trace.packets.insert(index, ambient);
    let report = analyze(&GoldenTrace::new(&scenario, trace).unwrap()).unwrap();
    let index = index + 1;
    for suffix in ["plus_one", "minus_one", "at_bound"] {
        assert!(report.excused_mutations["sample_duration"]
            .contains(&format!("packet_{index}/field_duration/{suffix}")));
    }
    assert!(report
        .detected_mutations
        .contains(&format!("packet_{index}/field_duration/past_bound")));
}

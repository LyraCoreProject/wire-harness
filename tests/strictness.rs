use std::collections::BTreeMap;

use wire_client::strictness::{analyze, check_allowlist, StrictnessReport, Survivor};

#[test]
fn report_is_byte_stable() {
    let golden = wire_client::catalogue::get("connection_identity").unwrap();
    let first = serde_json::to_vec(&analyze(&golden).unwrap()).unwrap();
    let second = serde_json::to_vec(&analyze(&golden).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn allowlist_requires_exact_survivors_and_reasons() {
    let report = StrictnessReport {
        schema_version: 1,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 1,
        detected: 0,
        excused_by_rule: BTreeMap::new(),
        unexplained_survivors: vec![Survivor {
            id: "mutation".into(),
            description: "changed packet".into(),
        }],
        broad_rules: Vec::new(),
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[{"scenario":"sample","id":"mutation","reason":"Needs a narrower rule"}]}"#).is_ok());
    let report = StrictnessReport {
        schema_version: 1,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 1,
        detected: 0,
        excused_by_rule: BTreeMap::new(),
        unexplained_survivors: vec![Survivor {
            id: "mutation".into(),
            description: "changed packet".into(),
        }],
        broad_rules: Vec::new(),
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[]}"#).is_err());
    let report = StrictnessReport {
        schema_version: 1,
        scenario: "sample".into(),
        behavior: "sample".into(),
        mutations: 0,
        detected: 0,
        excused_by_rule: BTreeMap::new(),
        unexplained_survivors: Vec::new(),
        broad_rules: Vec::new(),
    };
    assert!(check_allowlist(&[report], r#"{"schema_version":1,"entries":[{"scenario":"sample","id":"mutation","reason":"Needs a narrower rule"}]}"#).is_err());
}

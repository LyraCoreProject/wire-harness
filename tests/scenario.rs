use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{bail, Result};
use wire_client::compare::check_observations;
use wire_client::runner::{run_on, Transport};
use wire_client::scenario::{Scenario, Stream};

const SCENARIO: &str = include_str!("../scenarios/connection_identity.toml");

struct Connection {
    replies: VecDeque<Option<(u16, Vec<u8>)>>,
    sent: Vec<(u32, Vec<u8>)>,
    fail: bool,
    opening: Vec<wire_client::scenario::Observation>,
}

impl Transport for Connection {
    fn take_opening(&mut self) -> Vec<wire_client::scenario::Observation> {
        std::mem::take(&mut self.opening)
    }
    fn character_guid(&self) -> u64 {
        7
    }
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()> {
        self.sent.push((opcode, body.to_vec()));
        Ok(())
    }
    fn receive(&mut self, _: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        if self.fail {
            bail!("connection closed")
        }
        Ok(self.replies.pop_front().flatten())
    }
}

fn connection() -> Connection {
    Connection {
        sent: vec![],
        fail: false,
        opening: vec![],
        replies: [
            None,
            Some((0x01dd, vec![7, 0, 0, 0])),
            None,
            Some((
                0x0051,
                vec![
                    7, 0, 0, 0, 0, 0, 0, 0, b'A', 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
                ],
            )),
            None,
        ]
        .into(),
    }
}

#[test]
fn typed_requests_and_checkpoints_play_through_the_transport_seam() {
    let scenario = Scenario::parse(SCENARIO).unwrap();
    let mut connection = connection();
    let observed = run_on(&scenario, &mut connection).unwrap();
    assert_eq!(
        connection.sent,
        [
            (0x01dc, vec![7, 0, 0, 0, 0, 0, 0, 0]),
            (0x0050, vec![7, 0, 0, 0, 0, 0, 0, 0])
        ]
    );
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].stream, Stream::Reply("ping".into()));
}

#[test]
fn missing_and_duplicate_replies_are_reported_after_the_run() {
    let scenario = Scenario::parse(SCENARIO).unwrap();
    let mut absent = connection();
    absent.replies.clear();
    let observed = run_on(&scenario, &mut absent).unwrap();
    assert_eq!(
        check_observations(&scenario, &observed)
            .unwrap()
            .differences
            .len(),
        2
    );
    let mut duplicate = connection();
    duplicate
        .replies
        .insert(1, Some((0x01dd, vec![7, 0, 0, 0])));
    let observed = run_on(&scenario, &mut duplicate).unwrap();
    assert_eq!(
        check_observations(&scenario, &observed)
            .unwrap()
            .differences[0]
            .rule,
        "checkpoint_replies"
    );
}

#[test]
fn transport_failure_cannot_be_reported_as_a_quiet_checkpoint() {
    let mut connection = connection();
    connection.fail = true;
    let error = run_on(&Scenario::parse(SCENARIO).unwrap(), &mut connection).unwrap_err();
    assert_eq!(error.to_string(), "connection closed");
}

#[test]
fn broadcast_capture_does_not_change_reply_order() {
    let scenario = SCENARIO.replacen(
        "replies = [477]",
        "replies = [477], broadcasts = [{ opcode = 463, stream = 'clock' }]",
        1,
    );
    let mut connection = connection();
    connection
        .replies
        .insert(1, Some((0x01cf, vec![2, 0, 0, 0])));
    let observed = run_on(&Scenario::parse(&scenario).unwrap(), &mut connection).unwrap();
    assert_eq!(observed[0].stream, Stream::Broadcast("clock".into()));
    assert_eq!(observed[1].stream, Stream::Reply("ping".into()));
}

#[test]
fn invalid_scenarios_fail_before_fixture_setup() {
    for invalid in [
        SCENARIO.replace("schema_version = 3", "schema_version = 99"),
        SCENARIO.replace("level = 1", "level = 61"),
        SCENARIO.replace("id = \"name\"", "id = \"ping\""),
        SCENARIO.replace("window_ms = 250", "window_ms = 0"),
        SCENARIO.replace("sequence = 7", "sequence = 7, typo = 1"),
        SCENARIO.replace("x = -8949.95", "x = nan"),
        SCENARIO.replace("replies = [477]", "replies = [81]"),
    ] {
        assert!(Scenario::parse(&invalid).is_err());
    }
}

#[test]
fn every_undeclared_opcode_is_reported_without_retaining_its_body() {
    let scenario = Scenario::parse(SCENARIO).unwrap();
    let mut connection = connection();
    for opcode in [0xffff, 0x01cf, 0xffff] {
        connection
            .replies
            .insert(1, Some((opcode, b"private packet text".to_vec())));
    }
    let observed = run_on(&scenario, &mut connection).unwrap();
    assert_eq!(observed.len(), 5);
    assert!(observed[..3]
        .iter()
        .all(|o| o.stream == Stream::Unexpected && o.packet.fields.is_empty()));
    let report = check_observations(&scenario, &observed).unwrap();
    assert_eq!(report.differences.len(), 3);
    assert!(report
        .differences
        .iter()
        .all(|d| d.rule == "unexpected_packet"));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("private packet text"));
}

#[test]
fn malformed_declared_replies_are_differences_without_stopping_later_checkpoints() {
    let scenario = Scenario::parse(SCENARIO).unwrap();
    let mut connection = connection();
    connection.replies[1] = Some((0x01dd, vec![1]));
    let observed = run_on(&scenario, &mut connection).unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(
        check_observations(&scenario, &observed)
            .unwrap()
            .differences[0]
            .rule,
        "packet_decode"
    );
}

#[test]
fn opening_records_every_frame_before_the_first_request() {
    let scenario = Scenario::parse(SCENARIO).unwrap();
    let mut connection = connection();
    connection
        .opening
        .push(wire_client::scenario::Observation::opening(
            0xfffe,
            b"buffered login",
        ));
    connection
        .replies
        .push_front(Some((0xffff, b"private login text".to_vec())));
    connection
        .replies
        .push_front(Some((0x01dd, 99u32.to_le_bytes().to_vec())));
    let observed = run_on(&scenario, &mut connection).unwrap();
    assert_eq!(observed.len(), 5);
    assert!(observed[..3].iter().all(|o| o.stream == Stream::Opening));
    assert_eq!(observed[0].packet.opcode, 0xfffe);
    assert_eq!(observed[2].packet.opcode, 0xffff);
    assert!(observed[2].packet.fields.is_empty());
    assert!(check_observations(&scenario, &observed).unwrap().is_match());
}

#[test]
fn multiple_fixtures_require_unique_bindings_and_valid_arguments() {
    use wire_client::scenario::FixtureVerb;
    let text = include_str!("../scenarios/taxi_single_hop.toml");
    let scenario = Scenario::parse(text).unwrap();
    assert_eq!(scenario.fixtures.len(), 5);
    for fixture in [
        scenario.fixtures[0].clone(),
        FixtureVerb::Money { copper: 1 },
        FixtureVerb::KnownFlightNode { node_id: 2 },
        FixtureVerb::KnownFlightNode { node_id: 0 },
        FixtureVerb::KnownFlightNode { node_id: 257 },
        FixtureVerb::Creature {
            id: "flight_master".into(),
            entry: 352,
            offset: 1.0,
        },
        FixtureVerb::Creature {
            id: "second".into(),
            entry: 0,
            offset: 1.0,
        },
        FixtureVerb::Creature {
            id: "second".into(),
            entry: 352,
            offset: f32::NAN,
        },
    ] {
        let mut invalid = scenario.clone();
        invalid.fixtures.push(fixture);
        assert!(invalid.validate().is_err());
    }
    let mut reversed = scenario.clone();
    reversed.fixtures.swap(0, 1);
    assert!(reversed.validate().is_err());
    assert!(
        Scenario::parse(&text.replace("fixture = \"flight_master\"", "fixture = \"absent\""))
            .is_err()
    );
    let mut multiple_creatures = scenario;
    multiple_creatures.fixtures.push(FixtureVerb::Creature {
        id: "second".into(),
        entry: 352,
        offset: -2.0,
    });
    assert!(multiple_creatures.validate().is_ok());
}

#[test]
fn landing_acknowledges_only_the_passengers_full_guid_and_counter() {
    let mut scenario = Scenario::parse(SCENARIO).unwrap();
    scenario.steps.truncate(1);
    scenario
        .rules
        .unexpected_packets
        .retain(|rule| rule.checkpoint == "ping");
    scenario.steps[0]
        .checkpoint
        .broadcasts
        .push(wire_client::scenario::Broadcast {
            opcode: 199,
            stream: "landing".into(),
        });
    let mut connection = connection();
    connection.replies.clear();
    connection.replies.push_back(None);
    for guid in [8u8, 7] {
        let mut body = vec![1, guid];
        body.extend(0u32.to_le_bytes());
        body.extend(0u32.to_le_bytes());
        body.extend(1234u32.to_le_bytes());
        body.extend([0; 20]);
        connection.replies.push_back(Some((199, body)));
    }
    connection.replies.push_back(Some((199, vec![1, 7])));
    let observed = run_on(&scenario, &mut connection).unwrap();
    assert_eq!(observed.len(), 3);
    assert_eq!(observed[2].stream, Stream::Malformed);
    assert_eq!(connection.sent.len(), 2);
    let mut acknowledgement = 7u64.to_le_bytes().to_vec();
    acknowledgement.extend([0; 4]);
    acknowledgement.extend(1234u32.to_le_bytes());
    assert_eq!(connection.sent[1], (199, acknowledgement));
    assert!(observed.iter().all(|o| o.received_after_ms.is_some()));
}

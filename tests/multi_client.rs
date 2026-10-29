use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use anyhow::{ensure, Result};
use wire_client::compare::{check_multi_observations, compare};
use wire_client::runner::{run_multi_on, MultiTransport};
use wire_client::scenario::{Observation, Scenario, SessionAction, Stream};
use wire_client::trace::{
    GoldenTrace, Header, Provenance, RecordedField, RecordedPacket, Sha256Digest, Trace,
};

const ACTORS: &str = r#"
schema_version = 4
id = "two_actors"
behavior = "character_visibility"
[opening]
window_ms = 1
[rules]
time_tolerance_ms = 100
wall_clock_tolerance_ms = 1000
[[characters]]
id = "first"
fixtures = [{ verb = "character", name = "Firsta", race = "human", class = "warrior", level = 1, at = { map = 0, x = 1.0, y = 2.0, z = 3.0, orientation = 0.0 } }]
[[characters]]
id = "second"
fixtures = [{ verb = "character", name = "Seconda", race = "orc", class = "warrior", level = 1, at = { map = 0, x = 2.0, y = 2.0, z = 3.0, orientation = 0.0 } }]
[[steps]]
id = "ping"
character = "first"
send = { opcode = "CMSG_PING", sequence = 7, latency_ms = 0 }
checkpoint = { window_ms = 2, characters = [{ character = "first", replies = [477] }, { character = "second" }] }
[[steps]]
id = "leave"
character = "second"
action = { kind = "disconnect" }
checkpoint = { window_ms = 2, characters = [{ character = "first" }, { character = "second" }] }
[[steps]]
id = "return"
character = "second"
action = { kind = "connect" }
checkpoint = { window_ms = 2, characters = [{ character = "first" }, { character = "second" }] }
"#;

struct ClockTransport {
    ms: u64,
    connected: BTreeMap<String, bool>,
    frames: VecDeque<(String, u16, Vec<u8>)>,
    calls: Vec<String>,
    sent: Vec<(String, u32, Vec<u8>)>,
}

impl MultiTransport for ClockTransport {
    fn now(&self) -> Duration {
        Duration::from_millis(self.ms)
    }
    fn character_guid(&self, character: &str) -> Result<u64> {
        Ok(if character == "first" { 10 } else { 20 })
    }
    fn fixture_guids(&self, _: &str) -> Result<BTreeMap<String, u64>> {
        Ok(BTreeMap::new())
    }
    fn take_opening(&mut self, _: &str) -> Result<Vec<Observation>> {
        Ok(Vec::new())
    }
    fn connected(&self, character: &str) -> bool {
        self.connected[character]
    }
    fn connect(&mut self, character: &str) -> Result<()> {
        self.connected.insert(character.into(), true);
        self.calls.push(format!("connect {character}"));
        Ok(())
    }
    fn disconnect(&mut self, character: &str) -> Result<()> {
        self.connected.insert(character.into(), false);
        self.calls.push(format!("disconnect {character}"));
        Ok(())
    }
    fn send(&mut self, character: &str, opcode: u32, body: &[u8]) -> Result<()> {
        self.sent.push((character.into(), opcode, body.to_vec()));
        if opcode == 0x01dc {
            ensure!(
                character == "first" && body == [7, 0, 0, 0, 0, 0, 0, 0],
                "wrong packet"
            );
            self.frames
                .push_back((character.into(), 477, vec![7, 0, 0, 0]));
        }
        Ok(())
    }
    fn receive(&mut self, character: &str, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        if self
            .frames
            .front()
            .is_some_and(|frame| frame.0 == character)
        {
            let (_, opcode, body) = self.frames.pop_front().unwrap();
            return Ok(Some((opcode, body)));
        }
        self.ms += timeout.as_millis() as u64;
        Ok(None)
    }
}

#[test]
fn virtual_clock_captures_each_character_and_connects_during_a_scenario() {
    let scenario = Scenario::parse(ACTORS).unwrap();
    let mut transport = ClockTransport {
        ms: 0,
        connected: BTreeMap::from([("first".into(), true), ("second".into(), true)]),
        frames: VecDeque::new(),
        calls: Vec::new(),
        sent: Vec::new(),
    };
    let observations = run_multi_on(&scenario, &mut transport).unwrap();
    assert_eq!(transport.calls, ["disconnect second", "connect second"]);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].character.as_deref(), Some("first"));
    assert_eq!(observations[0].stream, Stream::Reply("ping".into()));
    assert!(check_multi_observations(
        &scenario,
        &observations,
        &BTreeMap::from([("first".into(), 10), ("second".into(), 20)])
    )
    .unwrap()
    .is_match());
}

#[test]
fn reconnect_reopens_the_same_character_during_a_scenario() {
    let scenario = Scenario::parse(
        &ACTORS
            .replace(
                "action = { kind = \"disconnect\" }",
                "action = { kind = \"reconnect\" }",
            )
            .replace(
                "action = { kind = \"connect\" }",
                "action = { kind = \"reconnect\" }",
            ),
    )
    .unwrap();
    let mut transport = ClockTransport {
        ms: 0,
        connected: BTreeMap::from([("first".into(), true), ("second".into(), true)]),
        frames: VecDeque::new(),
        calls: Vec::new(),
        sent: Vec::new(),
    };
    run_multi_on(&scenario, &mut transport).unwrap();
    assert_eq!(
        transport.calls,
        [
            "disconnect second",
            "connect second",
            "disconnect second",
            "connect second"
        ]
    );
}

#[test]
fn named_steps_reject_missing_characters_and_invalid_session_order() {
    let mut scenario = Scenario::parse(ACTORS).unwrap();
    scenario.steps[0].character = Some("missing".into());
    assert!(scenario.validate().is_err());
    scenario.steps[0].character = Some("first".into());
    scenario.steps[0].checkpoint.characters.pop();
    assert!(scenario.validate().is_err());
    let second = scenario.steps[1].checkpoint.characters[1].clone();
    scenario.steps[0].checkpoint.characters.push(second);
    scenario.steps[1].action = Some(SessionAction::Connect);
    assert!(scenario.validate().is_err());
}

#[test]
fn walking_uses_the_shared_virtual_clock_and_sends_start_heartbeat_stop() {
    let fixtures = ACTORS.split("[[steps]]").next().unwrap();
    let text = format!("{fixtures}[[steps]]\nid = 'walk'\ncharacter = 'first'\naction = {{ kind = 'walk', to = {{ map = 0, x = 3.0, y = 2.0, z = 3.0, orientation = 0.0 }}, speed = 5.0 }}\ncheckpoint = {{ window_ms = 500, characters = [{{ character = 'first' }}, {{ character = 'second' }}] }}\n");
    let scenario = Scenario::parse(&text).unwrap();
    let mut transport = ClockTransport {
        ms: 0,
        connected: BTreeMap::from([("first".into(), true), ("second".into(), true)]),
        frames: VecDeque::new(),
        calls: Vec::new(),
        sent: Vec::new(),
    };
    let observations = run_multi_on(&scenario, &mut transport).unwrap();
    assert!(observations.is_empty());
    assert_eq!(
        transport
            .sent
            .iter()
            .map(|packet| packet.1)
            .collect::<Vec<_>>(),
        [0x00b5, 0x00ee, 0x00b7]
    );
    assert_eq!(
        f32::from_le_bytes(transport.sent[2].2[8..12].try_into().unwrap()),
        3.0
    );
    assert!(transport.ms >= 500);
}

const GUIDS: &str = r#"
schema_version = 4
id = "shared_guids"
behavior = "character_visibility"
[opening]
window_ms = 1
[rules]
time_tolerance_ms = 100
wall_clock_tolerance_ms = 1000
[[characters]]
id = "first"
fixtures = [{ verb = "character", name = "Firsta", race = "human", class = "warrior", level = 1, at = { map = 0, x = 1.0, y = 2.0, z = 3.0, orientation = 0.0 } }]
[[characters]]
id = "second"
fixtures = [{ verb = "character", name = "Seconda", race = "orc", class = "warrior", level = 1, at = { map = 0, x = 2.0, y = 2.0, z = 3.0, orientation = 0.0 } }]
[[steps]]
id = "view"
character = "first"
action = { kind = "reconnect" }
checkpoint = { window_ms = 2, characters = [{ character = "first", broadcasts = [{ opcode = 221, stream = "movement" }] }, { character = "second", broadcasts = [{ opcode = 221, stream = "movement" }] }] }
"#;

fn trace(source: &str, golden: bool, first: u64, second: u64, second_seen: u64) -> Trace {
    let mut header = if golden {
        Header::golden(
            source,
            Provenance {
                server: "vmangos".into(),
                binary_release: "test".into(),
                binary_sha256: Sha256Digest::of(b"bin"),
                source_commit: "0".repeat(40),
                database_commit: "0".repeat(40),
            },
            1,
        )
        .unwrap()
    } else {
        Header::candidate(source, "observations".into(), 2).unwrap()
    };
    header.characters = BTreeMap::from([("first".into(), first), ("second".into(), second)]);
    let packet = |character: &str, guid| RecordedPacket {
        character: Some(character.into()),
        received_after_ms: Some(1),
        checkpoint: "view".into(),
        stream: Stream::Broadcast("movement".into()),
        opcode: 221,
        fields: BTreeMap::from([("guid".into(), RecordedField::Guid(guid))]),
    };
    Trace {
        header,
        packets: vec![packet("first", second), packet("second", second_seen)],
    }
}

#[test]
fn one_guid_mapping_spans_both_character_streams() {
    let expected = GoldenTrace::new(GUIDS, trace(GUIDS, true, 10, 20, 10)).unwrap();
    let matching = trace(GUIDS, false, 100, 200, 100);
    assert!(compare(&expected, &matching, &[]).unwrap().is_match());
    let wrong = trace(GUIDS, false, 100, 200, 200);
    assert!(!compare(&expected, &wrong, &[]).unwrap().is_match());
}

#[test]
fn named_characters_cannot_share_a_guid() {
    let mut candidate = trace(GUIDS, false, 100, 100, 100);
    assert!(candidate.validate().is_err());
    candidate.header.characters.insert("second".into(), 200);
    assert!(candidate.validate().is_ok());
}

#[test]
fn clock_rules_use_each_character_stream_as_their_origin() {
    let source = GUIDS.replacen("[[characters]]", "[[rules.clocks]]\nname = 'movement_clock'\nreason = 'Each Character has its own movement clock.'\nopcode = 221\npath = ['clock']\ntolerance_ms = 0\n[[characters]]", 1);
    let mut expected = trace(&source, true, 10, 20, 10);
    let mut actual = trace(&source, false, 100, 200, 100);
    for (packets, first, second) in [
        (&mut expected.packets, 100u64, 500u64),
        (&mut actual.packets, 200u64, 900u64),
    ] {
        packets[0]
            .fields
            .insert("clock".into(), RecordedField::Unsigned(first));
        packets[1]
            .fields
            .insert("clock".into(), RecordedField::Unsigned(second));
        let mut later_first = packets[0].clone();
        later_first
            .fields
            .insert("clock".into(), RecordedField::Unsigned(first + 10));
        let mut later_second = packets[1].clone();
        later_second
            .fields
            .insert("clock".into(), RecordedField::Unsigned(second + 10));
        packets.extend([later_first, later_second]);
    }
    let golden = GoldenTrace::new(&source, expected).unwrap();
    assert!(compare(&golden, &actual, &[]).unwrap().is_match());
}

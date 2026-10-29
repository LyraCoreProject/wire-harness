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
    pong_recipient: &'static str,
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
                .push_back((self.pong_recipient.into(), 477, vec![7, 0, 0, 0]));
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
        pong_recipient: "first",
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
fn a_ready_second_character_is_captured_before_the_checkpoint_deadline() {
    let scenario = Scenario::parse(ACTORS).unwrap();
    let mut transport = ClockTransport {
        ms: 0,
        connected: BTreeMap::from([("first".into(), true), ("second".into(), true)]),
        frames: VecDeque::new(),
        pong_recipient: "second",
        calls: Vec::new(),
        sent: Vec::new(),
    };
    let observed = run_multi_on(&scenario, &mut transport).unwrap();
    assert!(observed.iter().any(|packet| {
        packet.character.as_deref() == Some("second")
            && packet.checkpoint == "ping"
            && packet.packet.opcode == 477
    }));
}

#[test]
fn a_ready_second_character_is_captured_during_opening() {
    let scenario = Scenario::parse(ACTORS).unwrap();
    let mut transport = ClockTransport {
        ms: 0,
        connected: BTreeMap::from([("first".into(), true), ("second".into(), true)]),
        frames: VecDeque::from([("second".into(), 0xffff, Vec::new())]),
        pong_recipient: "first",
        calls: Vec::new(),
        sent: Vec::new(),
    };
    let observed = run_multi_on(&scenario, &mut transport).unwrap();
    assert!(observed.iter().any(|packet| {
        packet.character.as_deref() == Some("second")
            && packet.checkpoint == "opening"
            && packet.packet.opcode == 0xffff
    }));
}

struct BusyTransport {
    elapsed_us: u64,
    second_frames: usize,
    first_frames: usize,
    pong_due_us: Option<u64>,
    pong_sent: bool,
}

impl MultiTransport for BusyTransport {
    fn now(&self) -> Duration {
        Duration::from_micros(self.elapsed_us)
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
    fn connected(&self, _: &str) -> bool {
        true
    }
    fn connect(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn disconnect(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn send(&mut self, _: &str, _: u32, _: &[u8]) -> Result<()> {
        self.pong_due_us.get_or_insert(self.elapsed_us + 5_000);
        Ok(())
    }
    fn receive(&mut self, character: &str, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        if let Some(frame) = self.receive_ready(character)? {
            return Ok(Some(frame));
        }
        self.elapsed_us += timeout.as_micros() as u64;
        Ok(None)
    }
    fn receive_ready(&mut self, character: &str) -> Result<Option<(u16, Vec<u8>)>> {
        if self.pong_due_us.is_none() {
            return Ok(None);
        }
        if character == "first" {
            if self.first_frames > 0 {
                self.first_frames -= 1;
                self.elapsed_us += 1;
                return Ok(Some((0xffff, Vec::new())));
            }
            if !self.pong_sent && self.pong_due_us.is_some_and(|due| self.elapsed_us >= due) {
                self.pong_sent = true;
                return Ok(Some((477, 7u32.to_le_bytes().to_vec())));
            }
        } else if self.second_frames > 0 {
            self.second_frames -= 1;
            self.elapsed_us += 1;
            return Ok(Some((0xffff, Vec::new())));
        }
        Ok(None)
    }
}

fn busy_scenario() -> Scenario {
    let fixtures = ACTORS.split("[[steps]]").next().unwrap();
    Scenario::parse(&format!(
        "{fixtures}[[steps]]\nid = 'ping'\ncharacter = 'first'\nsend = {{ opcode = 'CMSG_PING', sequence = 7, latency_ms = 0 }}\ncheckpoint = {{ window_ms = 2, characters = [{{ character = 'first', replies = [477] }}, {{ character = 'second' }}] }}\n[[steps]]\nid = 'next'\ncharacter = 'first'\nsend = {{ opcode = 'CMSG_PING', sequence = 8, latency_ms = 0 }}\ncheckpoint = {{ window_ms = 10, characters = [{{ character = 'first', replies = [477] }}, {{ character = 'second' }}] }}\n"
    ))
    .unwrap()
}

#[test]
fn a_late_pong_follows_six_thousand_ready_frames_into_the_next_checkpoint() {
    let scenario = busy_scenario();
    let mut transport = BusyTransport {
        elapsed_us: 0,
        first_frames: 0,
        second_frames: 6_000,
        pong_due_us: None,
        pong_sent: false,
    };
    let observed = run_multi_on(&scenario, &mut transport).unwrap();
    let pong = observed
        .iter()
        .find(|packet| packet.packet.opcode == 477)
        .unwrap();
    assert_eq!(pong.checkpoint, "next");
    assert_eq!(pong.character.as_deref(), Some("first"));
}

#[test]
fn opening_stops_before_a_late_pong() {
    let scenario = busy_scenario();
    let mut transport = BusyTransport {
        elapsed_us: 0,
        first_frames: 0,
        second_frames: 6_000,
        pong_due_us: Some(5_000),
        pong_sent: false,
    };
    let observed = run_multi_on(&scenario, &mut transport).unwrap();
    let pong = observed
        .iter()
        .find(|packet| packet.packet.opcode == 477)
        .unwrap();
    assert_ne!(pong.checkpoint, "opening");
    assert!(
        observed
            .iter()
            .filter(|packet| packet.checkpoint == "opening")
            .count()
            <= 1_000
    );
}

#[test]
fn busy_recipients_each_get_a_turn_before_the_checkpoint_closes() {
    let mut scenario = busy_scenario();
    scenario.steps.truncate(1);
    let mut transport = BusyTransport {
        elapsed_us: 0,
        first_frames: 6_000,
        second_frames: 6_000,
        pong_due_us: None,
        pong_sent: false,
    };
    let observed = run_multi_on(&scenario, &mut transport).unwrap();
    let ping = observed.iter().filter(|packet| packet.checkpoint == "ping");
    let first = ping
        .clone()
        .filter(|packet| packet.character.as_deref() == Some("first"))
        .count();
    let second = ping
        .filter(|packet| packet.character.as_deref() == Some("second"))
        .count();
    assert!(first > 0 && second > 0);
    assert!(first.abs_diff(second) <= 1);
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
        pong_recipient: "first",
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
        pong_recipient: "first",
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

#[test]
fn shared_rules_disclose_one_sample_per_character() {
    for declaration in [
        "[[rules.clocks]]\nname = 'movement_clock'\nreason = 'Each Character has its own movement clock.'\nopcode = 221\npath = ['sample']\ntolerance_ms = 0\n",
        "[[rules.identities]]\nname = 'movement_identity'\nreason = 'Each Character has its own movement identity.'\nopcode = 221\npath = ['sample']\n",
    ] {
        let source = GUIDS.replacen("[[characters]]", &format!("{declaration}[[characters]]"), 1);
        let mut expected = trace(&source, true, 10, 20, 10);
        expected.packets[0]
            .fields
            .insert("sample".into(), RecordedField::Unsigned(100));
        expected.packets[1]
            .fields
            .insert("sample".into(), RecordedField::Unsigned(500));
        let error = GoldenTrace::new(&source, expected).unwrap_err();
        assert!(error.to_string().contains("one sample"), "{error}");
    }
}

#[test]
fn create_fields_remain_compared_in_the_visibility_scenario() {
    let golden = wire_client::catalogue::get("two_character_visibility").unwrap();
    for removed in ["movement", "object_type", "values"] {
        let mut candidate = golden.trace().clone();
        candidate.header.origin = wire_client::trace::Origin::Candidate {
            adapter: "test".into(),
        };
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| {
                packet.checkpoint == "arrival"
                    && packet.character.as_deref() == Some("watcher")
                    && packet.opcode == 169
                    && packet.fields.contains_key("objects")
            })
            .unwrap();
        let RecordedField::List(objects) = packet.fields.get_mut("objects").unwrap() else {
            panic!()
        };
        let RecordedField::Object(fields) = objects
            .iter_mut()
            .find(|object| {
                matches!(object, RecordedField::Object(fields)
                    if fields.get("guid") == Some(&RecordedField::Guid(golden.trace().header.characters["arriving"])))
            })
            .unwrap()
        else {
            panic!()
        };
        fields.remove(removed);
        let report = compare(&golden, &candidate, &[]).unwrap();
        assert!(!report.is_match(), "CREATE {removed} was ignored");
    }
}

#[test]
fn a_declared_create_cannot_be_relabelled_as_opening() {
    let golden = wire_client::catalogue::get("two_character_visibility").unwrap();
    let mut candidate = golden.trace().clone();
    candidate.header.origin = wire_client::trace::Origin::Candidate {
        adapter: "test".into(),
    };
    let create = candidate
        .packets
        .iter()
        .find(|packet| {
            packet.checkpoint == "arrival"
                && packet.character.as_deref() == Some("arriving")
                && packet.opcode == 169
                && packet.stream == Stream::Broadcast("objects".into())
        })
        .unwrap()
        .clone();
    let mut extra = create;
    extra.stream = Stream::Opening;
    candidate.packets.push(extra);
    let report = compare(&golden, &candidate, &[]).unwrap();
    assert!(report.differences.iter().any(|difference| {
        difference.rule == "unexpected_opening"
            && difference.character.as_deref() == Some("arriving")
    }));
}

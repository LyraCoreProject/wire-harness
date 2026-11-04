use anyhow::{bail, ensure, Result};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use wire_client::cli::Invocation;
use wire_client::runner::{FixtureAdapter, MultiTransport, PreparedClient, Transport};
use wire_client::scenario::{FixtureVerb, Observation, Scenario};
use wire_client::{logon_at, WireClient};
use wow_world_messages::vanilla::{opcodes::ServerOpcodeMessage as Smsg, CMSG_CHAR_ENUM};

#[path = "lyracore/fixture.rs"]
mod lyracore_fixture_impl;
#[path = "vmangos/fixture.rs"]
mod vmangos_fixture_impl;

pub fn select(invocation: &Invocation) -> Result<Box<dyn FixtureAdapter>> {
    match std::env::var("WIRE_ADAPTER").as_deref() {
        Ok("lyracore") => Ok(Box::new(lyracore_fixture_impl::Adapter::new(invocation)?)),
        Ok("vmangos") => Ok(Box::new(vmangos_fixture_impl::Adapter(
            invocation.clone(),
            Vec::new(),
            None,
        ))),
        _ => bail!("set WIRE_ADAPTER to lyracore or vmangos"),
    }
}

struct MultiEntry {
    adapter: Box<dyn FixtureAdapter>,
    fixture: FixtureVerb,
    client: Option<PreparedClient>,
    guid: u64,
    fixtures: BTreeMap<String, u64>,
}

struct WireMulti {
    started: Instant,
    entries: BTreeMap<String, MultiEntry>,
}

impl WireMulti {
    fn prepare(scenario: &Scenario, invocation: &Invocation) -> Result<Self> {
        let peers = std::env::var("WIRE_PEER_ACCOUNTS").unwrap_or_default();
        let accounts: Vec<_> = peers
            .split(',')
            .filter(|account| !account.is_empty())
            .collect();
        ensure!(
            accounts.len() + 1 == scenario.characters.len(),
            "WIRE_PEER_ACCOUNTS must name each additional Account"
        );
        let mut unique = std::collections::BTreeSet::new();
        ensure!(
            unique.insert(invocation.account.clone())
                && accounts
                    .iter()
                    .all(|account| unique.insert((*account).to_owned())),
            "Characters need distinct Accounts"
        );
        let gm_password = if std::env::var("WIRE_ADAPTER").as_deref() == Ok("vmangos") {
            Some(invocation.peer_password(accounts.len())?.to_owned())
        } else {
            None
        };
        let mut entries = BTreeMap::new();
        for (index, character) in scenario.characters.iter().enumerate() {
            let mut own = invocation.clone();
            own.character.clear();
            if index > 0 {
                own.account = accounts[index - 1].to_owned();
                own.password = invocation.peer_password(index - 1)?.to_owned();
            }
            if let Some(password) = &gm_password {
                own.peer_passwords = vec![password.clone()];
            }
            let mut adapter = select(&own)?;
            let prepared = adapter.prepare(&character.fixtures)?;
            let guid = prepared.client.self_guid;
            let fixtures = prepared.fixtures.clone();
            let client = if character.initially_connected {
                Some(prepared)
            } else {
                None
            };
            entries.insert(
                character.id.clone(),
                MultiEntry {
                    adapter,
                    fixture: character.fixtures[0].clone(),
                    client,
                    guid,
                    fixtures,
                },
            );
        }
        Ok(Self {
            started: Instant::now(),
            entries,
        })
    }

    fn guids(&self) -> BTreeMap<String, u64> {
        self.entries
            .iter()
            .map(|(id, entry)| (id.clone(), entry.guid))
            .collect()
    }

    fn cleanup(&mut self) -> Result<()> {
        for entry in self.entries.values_mut() {
            entry.client = None;
            entry.adapter.cleanup()?;
        }
        Ok(())
    }
}

impl MultiTransport for WireMulti {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
    fn character_guid(&self, character: &str) -> Result<u64> {
        Ok(self
            .entries
            .get(character)
            .ok_or_else(|| anyhow::anyhow!("unknown Character"))?
            .guid)
    }
    fn fixture_guids(&self, character: &str) -> Result<BTreeMap<String, u64>> {
        Ok(self
            .entries
            .get(character)
            .ok_or_else(|| anyhow::anyhow!("unknown Character"))?
            .fixtures
            .clone())
    }
    fn take_opening(&mut self, character: &str) -> Result<Vec<Observation>> {
        Ok(self
            .entries
            .get_mut(character)
            .ok_or_else(|| anyhow::anyhow!("unknown Character"))?
            .client
            .as_mut()
            .map(Transport::take_opening)
            .unwrap_or_default())
    }
    fn connected(&self, character: &str) -> bool {
        self.entries
            .get(character)
            .is_some_and(|entry| entry.client.is_some())
    }
    fn connect(&mut self, character: &str) -> Result<()> {
        let entry = self
            .entries
            .get_mut(character)
            .ok_or_else(|| anyhow::anyhow!("unknown Character"))?;
        ensure!(entry.client.is_none(), "Character is already connected");
        let mut client = entry.adapter.reconnect(&entry.fixture)?;
        ensure!(
            client.client.self_guid == entry.guid,
            "reconnect selected a different Character"
        );
        client.fixtures = entry.fixtures.clone();
        entry.client = Some(client);
        Ok(())
    }
    fn disconnect(&mut self, character: &str) -> Result<()> {
        let entry = self
            .entries
            .get_mut(character)
            .ok_or_else(|| anyhow::anyhow!("unknown Character"))?;
        ensure!(entry.client.take().is_some(), "Character is disconnected");
        Ok(())
    }
    fn send(&mut self, character: &str, opcode: u32, body: &[u8]) -> Result<()> {
        Transport::send(
            self.entries
                .get_mut(character)
                .and_then(|e| e.client.as_mut())
                .ok_or_else(|| anyhow::anyhow!("Character is disconnected"))?,
            opcode,
            body,
        )
    }
    fn receive(&mut self, character: &str, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        Transport::receive(
            self.entries
                .get_mut(character)
                .and_then(|e| e.client.as_mut())
                .ok_or_else(|| anyhow::anyhow!("Character is disconnected"))?,
            timeout,
        )
    }
    fn receive_ready(&mut self, character: &str) -> Result<Option<(u16, Vec<u8>)>> {
        self.entries
            .get_mut(character)
            .and_then(|entry| entry.client.as_mut())
            .ok_or_else(|| anyhow::anyhow!("Character is disconnected"))?
            .client
            .recv_raw_ready()
    }
}

pub fn run_multi_file(invocation: &Invocation, scenario: &Scenario) -> Result<()> {
    let mut clients = WireMulti::prepare(scenario, invocation)?;
    let characters = clients.guids();
    let result = wire_client::runner::run_multi_on(scenario, &mut clients);
    let cleanup = clients.cleanup();
    let observations = result?;
    cleanup?;
    let report =
        wire_client::compare::check_multi_observations(scenario, &observations, &characters)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report.is_match(),
        "scenario {} has {} differences",
        scenario.id,
        report.differences.len()
    );
    Ok(())
}

fn connect(inv: &Invocation) -> Result<WireClient> {
    let (key, advertised) = if std::env::var("WIRE_ADAPTER").as_deref() == Ok("vmangos") {
        vmangos_fixture_impl::logon(inv)?
    } else {
        logon_at(&inv.endpoints.logon_addr, &inv.account, &inv.password)?
    };
    WireClient::connect_world(&inv.endpoints.world_addr(&advertised), &inv.account, key)
}

fn create(inv: &Invocation, fixture: &FixtureVerb) -> Result<WireClient> {
    let FixtureVerb::Character {
        name, race, class, ..
    } = fixture
    else {
        bail!("Character must be the first fixture")
    };
    ensure!(
        inv.character.is_empty() || inv.character == *name,
        "--character must match the scenario fixture name"
    );
    let mut client = connect(inv)?;
    ensure!(
        !client
            .char_enum()?
            .iter()
            .any(|(_, existing, _)| existing.eq_ignore_ascii_case(name)),
        "fixture Character already exists; reset the disposable fixture before rerunning"
    );
    let guid = client.create_or_find_char_as(
        name,
        wire_client::cli::parse_class(class)?,
        wire_client::cli::parse_race(race)?,
    )?;
    client.player_login(guid)?;
    Ok(client)
}

fn reconnect_checked(inv: &Invocation, fixture: &FixtureVerb) -> Result<PreparedClient> {
    connect_character(inv, fixture, true)
}

fn reconnect_existing(inv: &Invocation, fixture: &FixtureVerb) -> Result<PreparedClient> {
    connect_character(inv, fixture, false)
}

fn connect_character(
    inv: &Invocation,
    fixture: &FixtureVerb,
    check_position: bool,
) -> Result<PreparedClient> {
    let FixtureVerb::Character {
        name,
        race,
        class,
        level,
        at,
    } = fixture
    else {
        bail!("Character must be the first fixture")
    };
    let mut client = connect(inv)?;
    client.send(&CMSG_CHAR_ENUM {})?;
    let Smsg::SMSG_CHAR_ENUM(list) = client.recv_until(|p| matches!(p, Smsg::SMSG_CHAR_ENUM(_)))?
    else {
        unreachable!()
    };
    let character = list
        .characters
        .iter()
        .find(|c| c.name == *name)
        .ok_or_else(|| anyhow::anyhow!("fixture Character missing after setup"))?;
    ensure!(
        character.race == wire_client::cli::parse_race(race)?
            && character.class == wire_client::cli::parse_class(class)?
            && character.level.as_int() == *level,
        "fixture race, class or level did not apply"
    );
    if check_position {
        ensure!(
            character.map.as_int() == at.map
                && (character.position.x - at.x).abs() < 0.1
                && (character.position.y - at.y).abs() < 0.1
                && (character.position.z - at.z).abs() < 0.1,
            "fixture position did not apply"
        );
    }
    let guid = character.guid.guid();
    client.start_player_login(guid)?;
    let mut opening = Vec::new();
    loop {
        let (opcode, body) = client.recv_raw()?;
        let observed = wire_client::scenario::Observation::opening(opcode, &body);
        if opcode == 0x0236 {
            use wire_client::decode::Field;
            let fields = &observed.packet.fields;
            if check_position {
                ensure!(
                    fields.get("map") == Some(&Field::Unsigned(u64::from(at.map))),
                    "fixture login map did not apply"
                );
                for (name, expected, tolerance) in [
                    ("x", at.x, 0.1),
                    ("y", at.y, 0.1),
                    ("z", at.z, 0.1),
                    ("orientation", at.orientation, 0.01),
                ] {
                    ensure!(
                        matches!(fields.get(name), Some(Field::Float(value)) if (value - f64::from(expected)).abs() < tolerance),
                        "fixture login {name} did not apply"
                    );
                }
            }
            opening.push(observed);
            break;
        }
        ensure!(opening.len() < 4096, "login exceeded 4096 frames");
        opening.push(observed);
    }
    Ok(PreparedClient {
        client,
        opening,
        fixtures: Default::default(),
    })
}

fn quest_snapshot_matches(
    observation: &Observation,
    guid: u64,
    fixtures: &[FixtureVerb],
) -> Result<bool> {
    use wire_client::decode::Field;
    let Some(Field::List(objects)) = observation.packet.fields.get("objects") else {
        return Ok(false);
    };
    for object in objects {
        let Field::Object(object) = object else {
            continue;
        };
        if object.get("guid") != Some(&Field::Guid(guid))
            || !matches!(object.get("kind"), Some(Field::Unsigned(2 | 3)))
        {
            continue;
        }
        let Some(Field::Object(values)) = object.get("values") else {
            bail!("Character CREATE has no values");
        };
        let quests: Vec<_> = fixtures
            .iter()
            .filter_map(|fixture| match fixture {
                FixtureVerb::Quest { entry, state } => Some((
                    *entry,
                    match state {
                        wire_client::scenario::QuestState::Incomplete => 0,
                        wire_client::scenario::QuestState::Complete => 0x01000000,
                    },
                )),
                _ => None,
            })
            .collect();
        // Build 5875 has 20 quest slots, three words each, beginning at field 198.
        for slot in 0..20 {
            let word = |offset| -> Result<u64> {
                match values.get(&format!("field_{}", 198 + slot * 3 + offset)) {
                    Some(Field::Unsigned(value)) => Ok(*value),
                    None => Ok(0),
                    _ => bail!("quest log field has the wrong type"),
                }
            };
            let expected_entry = u64::from(quests.get(slot).map_or(0, |quest| quest.0));
            let actual_entry = word(0)?;
            ensure!(actual_entry == expected_entry, "quest fixture slot {slot}: expected entry {expected_entry}, received {actual_entry}");
            ensure!(
                word(1)? == quests.get(slot).map_or(0, |quest| quest.1) && word(2)? == 0,
                "quest fixture state did not apply or has objective progress or a timer"
            );
        }
        return Ok(true);
    }
    Ok(false)
}

fn verify_quest_fixtures(prepared: &mut PreparedClient, fixtures: &[FixtureVerb]) -> Result<()> {
    if !fixtures
        .iter()
        .any(|fixture| matches!(fixture, FixtureVerb::Quest { .. }))
    {
        return Ok(());
    }
    for observation in &prepared.opening {
        if quest_snapshot_matches(observation, prepared.client.self_guid, fixtures)? {
            return Ok(());
        }
    }
    prepared
        .client
        .set_recv_timeout(Duration::from_millis(100))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match prepared.client.recv_raw() {
            Ok((opcode, body)) => {
                let observation = Observation::opening(opcode, &body);
                let verified =
                    quest_snapshot_matches(&observation, prepared.client.self_guid, fixtures)?;
                ensure!(
                    prepared.opening.len() < 4096,
                    "fixture verification exceeded 4096 frames"
                );
                prepared.opening.push(observation);
                if verified {
                    return Ok(());
                }
            }
            Err(error) if wire_client::is_read_timeout(&error) => (),
            Err(error) => return Err(error),
        }
    }
    bail!("quest fixture verification did not receive the Character CREATE")
}

fn item_fixture_bindings(
    opening: &[Observation],
    guid: u64,
    fixtures: &[FixtureVerb],
) -> Result<Option<BTreeMap<String, u64>>> {
    use wire_client::decode::Field;
    let mut inventory = None;
    let mut items = BTreeMap::new();
    for observation in opening {
        let Some(Field::List(objects)) = observation.packet.fields.get("objects") else {
            continue;
        };
        for object in objects {
            let Field::Object(object) = object else {
                continue;
            };
            if !matches!(object.get("kind"), Some(Field::Unsigned(2 | 3))) {
                continue;
            }
            let (Some(Field::Guid(subject)), Some(Field::Object(values))) =
                (object.get("guid"), object.get("values"))
            else {
                continue;
            };
            if *subject == guid {
                inventory = Some(values);
            }
            if *subject >> 48 == 0x4000 {
                items.insert(*subject, values);
            }
        }
    }
    let Some(inventory) = inventory else {
        return Ok(None);
    };
    let word = |values: &BTreeMap<String, Field>, index| -> Result<u64> {
        match values.get(&format!("field_{index}")) {
            Some(Field::Unsigned(value)) => Ok(*value),
            None => Ok(0),
            _ => bail!("item fixture field has the wrong type"),
        }
    };
    let mut bindings = BTreeMap::new();
    for fixture in fixtures {
        let FixtureVerb::Item { id, entry, count } = fixture else {
            continue;
        };
        let mut matching = Vec::new();
        // Build 5875 backpack slots 23..39 use GUID pairs starting at field 486.
        for slot in 23..39 {
            let item_guid =
                word(inventory, 486 + slot * 2)? | (word(inventory, 487 + slot * 2)? << 32);
            let Some(values) = items.get(&item_guid) else {
                continue;
            };
            if word(values, 3)? != u64::from(*entry) {
                continue;
            }
            ensure!(
                word(values, 6)? | (word(values, 7)? << 32) == guid,
                "item fixture has the wrong owner"
            );
            ensure!(
                word(values, 14)? == u64::from(*count),
                "item fixture has the wrong count"
            );
            matching.push(item_guid);
        }
        ensure!(
            matching.len() <= 1,
            "item fixture must occupy one backpack stack"
        );
        let Some(item_guid) = matching.first() else {
            return Ok(None);
        };
        bindings.insert(id.clone(), *item_guid);
    }
    Ok(Some(bindings))
}

fn verify_item_fixtures(
    prepared: &mut PreparedClient,
    fixtures: &[FixtureVerb],
) -> Result<BTreeMap<String, u64>> {
    if !fixtures
        .iter()
        .any(|fixture| matches!(fixture, FixtureVerb::Item { .. }))
    {
        return Ok(BTreeMap::new());
    }
    prepared
        .client
        .set_recv_timeout(Duration::from_millis(100))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(bindings) =
            item_fixture_bindings(&prepared.opening, prepared.client.self_guid, fixtures)?
        {
            return Ok(bindings);
        }
        ensure!(
            Instant::now() < deadline,
            "item fixture missing from final login backpack snapshot"
        );
        match prepared.client.recv_raw() {
            Ok((opcode, body)) => {
                ensure!(
                    prepared.opening.len() < 4096,
                    "item fixture verification exceeded 4096 frames"
                );
                prepared.opening.push(Observation::opening(opcode, &body));
            }
            Err(error) if wire_client::is_read_timeout(&error) => (),
            Err(error) => return Err(error),
        }
    }
}

fn new_creature(
    client: &mut WireClient,
    before: &[u64],
    entry: u32,
    at: &wire_client::scenario::Position,
) -> Result<u64> {
    use wire_client::decode::{decode, Field};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    client.set_recv_timeout(std::time::Duration::from_millis(100))?;
    while std::time::Instant::now() < deadline {
        let (opcode, body) = match client.recv_raw() {
            Ok(frame) => frame,
            Err(error) if wire_client::is_read_timeout(&error) => continue,
            Err(error) => return Err(error),
        };
        if !matches!(opcode, 0x00a9 | 0x01f6) {
            continue;
        }
        let packet = decode(opcode, &body)?;
        let Some(Field::List(objects)) = packet.fields.get("objects") else {
            bail!("missing decoded objects")
        };
        let mut matching = Vec::new();
        for object in objects {
            let Field::Object(object) = object else {
                continue;
            };
            let (Some(Field::Guid(guid)), Some(Field::Object(values))) =
                (object.get("guid"), object.get("values"))
            else {
                continue;
            };
            if !before.contains(guid)
                && values.get("field_3") == Some(&Field::Unsigned(u64::from(entry)))
            {
                ensure!(
                    matches!(object.get("kind"), Some(Field::Unsigned(2 | 3))),
                    "spawn must arrive in a CREATE update"
                );
                let Some(Field::Object(movement)) = object.get("movement") else {
                    bail!("spawn movement missing")
                };
                let Some(Field::Object(position)) = movement.get("position") else {
                    bail!("spawn position missing")
                };
                for (name, expected) in [("x", at.x), ("y", at.y), ("z", at.z)] {
                    ensure!(
                        matches!(position.get(name), Some(Field::Float(v)) if (v-f64::from(expected)).abs() < 0.1),
                        "spawn {name} did not match its offset"
                    );
                }
                matching.push(*guid);
            }
        }
        ensure!(matching.len() <= 1, "ambiguous newly spawned creature");
        if let Some(guid) = matching.first() {
            client.seen_guids.push(*guid);
            return Ok(*guid);
        }
    }
    bail!("fixture creature did not appear")
}

fn prepare_recording<T>(
    scenario_text: &str,
    provenance: Option<wire_client::trace::Provenance>,
    adapter_name: String,
    prepare: impl FnOnce() -> Result<T>,
) -> Result<(T, wire_client::trace::Header)> {
    use wire_client::trace::Header;
    let mut header = match provenance {
        Some(provenance) => Header::golden(scenario_text, provenance, 0)?,
        None => Header::candidate(scenario_text, adapter_name, 0)?,
    };
    let prepared = prepare()?;
    header.recorded_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    Ok((prepared, header))
}

pub fn record(invocation: &Invocation, path: &str, golden: bool) -> Result<()> {
    use wire_client::trace::Trace;
    let adapter_name = std::env::var("WIRE_ADAPTER")?;
    let (provenance, output) = if golden {
        ensure!(
            adapter_name == "vmangos",
            "Golden Traces require the Reference Server adapter"
        );
        let [provenance, output] = invocation.args.as_slice() else {
            bail!("record needs SCENARIO PROVENANCE_JSON OUTPUT");
        };
        let provenance: wire_client::trace::Provenance =
            serde_json::from_slice(&std::fs::read(provenance)?)?;
        ensure!(
            provenance.server == "vmangos",
            "Reference Server provenance must name vmangos"
        );
        (Some(provenance), output)
    } else {
        let [output] = invocation.args.as_slice() else {
            bail!("candidate needs SCENARIO OUTPUT");
        };
        (None, output)
    };
    ensure!(
        !std::path::Path::new(output).exists(),
        "output trace already exists"
    );
    let scenario_text = std::fs::read_to_string(path)?;
    let scenario = wire_client::scenario::Scenario::parse(&scenario_text)?;
    let (header, observed) = if scenario.schema_version == 4 {
        let (mut clients, mut header) =
            prepare_recording(&scenario_text, provenance, adapter_name, || {
                WireMulti::prepare(&scenario, invocation)
            })?;
        header.characters = clients.guids();
        let result = wire_client::runner::run_multi_on(&scenario, &mut clients);
        let cleanup = clients.cleanup();
        let observed = result?;
        cleanup?;
        (header, observed)
    } else {
        let ((mut adapter, mut client), mut header) =
            prepare_recording(&scenario_text, provenance, adapter_name, || {
                let mut adapter = select(invocation)?;
                let client = adapter.prepare(&scenario.fixtures)?;
                Ok((adapter, client))
            })?;
        header.character_guid = client.client.self_guid;
        let result = wire_client::runner::run_on(&scenario, &mut client);
        drop(client);
        let cleanup = adapter.cleanup();
        let observed = result?;
        cleanup?;
        (header, observed)
    };
    let trace = Trace::record(header, &observed)?;
    if golden {
        let expected = wire_client::trace::GoldenTrace::new(&scenario_text, trace.clone())?;
        let report = wire_client::compare::compare(&expected, &trace, &[])?;
        if !report.is_match() {
            println!("{}", serde_json::to_string_pretty(&report)?);
            bail!("Reference Server observations do not satisfy the scenario");
        }
    }
    trace.write_new(std::path::Path::new(output))?;
    println!(
        "[wire] recorded {} {}: {} scrubbed packets",
        if golden { "Golden Trace" } else { "candidate" },
        scenario.id,
        trace.packets.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prepare_recording;

    #[test]
    fn item_fixture_requires_owned_backpack_stack_with_exact_count() {
        use std::collections::BTreeMap;
        use wire_client::decode::{Field, Packet};
        use wire_client::scenario::{FixtureVerb, Observation, Stream};
        let fixtures = [FixtureVerb::Item {
            id: "linen".into(),
            entry: 2589,
            count: 2,
        }];
        let snapshot = |owner, count, slot| {
            let object = |guid, values| {
                Field::Object(BTreeMap::from([
                    ("guid".into(), Field::Guid(guid)),
                    ("kind".into(), Field::Unsigned(3)),
                    ("values".into(), Field::Object(values)),
                ]))
            };
            Observation {
                character: None,
                received_after_ms: None,
                checkpoint: "opening".into(),
                stream: Stream::Opening,
                packet: Packet {
                    opcode: 169,
                    fields: BTreeMap::from([(
                        "objects".into(),
                        Field::List(vec![
                            object(
                                9,
                                BTreeMap::from([
                                    (format!("field_{}", 486 + slot * 2), Field::Unsigned(7)),
                                    (
                                        format!("field_{}", 487 + slot * 2),
                                        Field::Unsigned(0x40000000),
                                    ),
                                ]),
                            ),
                            object(
                                0x4000000000000007,
                                BTreeMap::from([
                                    ("field_3".into(), Field::Unsigned(2589)),
                                    ("field_6".into(), Field::Unsigned(owner)),
                                    ("field_14".into(), Field::Unsigned(count)),
                                ]),
                            ),
                        ]),
                    )]),
                },
            }
        };
        assert_eq!(
            super::item_fixture_bindings(&[snapshot(9, 2, 25)], 9, &fixtures)
                .unwrap()
                .unwrap()["linen"],
            0x4000000000000007
        );
        assert!(super::item_fixture_bindings(&[], 9, &fixtures)
            .unwrap()
            .is_none());
        assert!(
            super::item_fixture_bindings(&[snapshot(9, 2, 40)], 9, &fixtures)
                .unwrap()
                .is_none()
        );
        for (owner, count) in [(10, 2), (9, 1), (9, 3)] {
            assert!(
                super::item_fixture_bindings(&[snapshot(owner, count, 25)], 9, &fixtures).is_err()
            );
        }
    }

    #[test]
    fn quest_fixture_requires_the_full_incomplete_log_in_order() {
        use std::collections::BTreeMap;
        use wire_client::decode::{Field, Packet};
        use wire_client::scenario::{FixtureVerb, Observation, QuestState, Stream};
        let fixtures = [FixtureVerb::Quest {
            entry: 7,
            state: QuestState::Incomplete,
        }];
        let snapshot = |guid, entry, progress| Observation {
            character: None,
            received_after_ms: None,
            checkpoint: "opening".into(),
            stream: Stream::Opening,
            packet: Packet {
                opcode: 169,
                fields: BTreeMap::from([(
                    "objects".into(),
                    Field::List(vec![Field::Object(BTreeMap::from([
                        ("guid".into(), Field::Guid(guid)),
                        ("kind".into(), Field::Unsigned(3)),
                        (
                            "values".into(),
                            Field::Object(BTreeMap::from([
                                ("field_198".into(), Field::Unsigned(entry)),
                                ("field_199".into(), Field::Unsigned(progress)),
                            ])),
                        ),
                    ]))]),
                )]),
            },
        };
        assert!(super::quest_snapshot_matches(&snapshot(9, 7, 0), 9, &fixtures).unwrap());
        assert!(!super::quest_snapshot_matches(&snapshot(10, 7, 0), 9, &fixtures).unwrap());
        for (entry, progress) in [(0, 0), (15, 0), (7, 1), (7, 0x01000000)] {
            assert!(
                super::quest_snapshot_matches(&snapshot(9, entry, progress), 9, &fixtures).is_err()
            );
        }
    }

    #[test]
    fn completed_quest_fixture_requires_only_the_complete_flag() {
        use std::collections::BTreeMap;
        use wire_client::decode::{Field, Packet};
        use wire_client::scenario::{FixtureVerb, Observation, QuestState, Stream};
        let fixture = [FixtureVerb::Quest {
            entry: 2158,
            state: QuestState::Complete,
        }];
        let snapshot = |progress, timer| Observation {
            character: None,
            received_after_ms: None,
            checkpoint: "opening".into(),
            stream: Stream::Opening,
            packet: Packet {
                opcode: 169,
                fields: BTreeMap::from([(
                    "objects".into(),
                    Field::List(vec![Field::Object(BTreeMap::from([
                        ("guid".into(), Field::Guid(9)),
                        ("kind".into(), Field::Unsigned(3)),
                        (
                            "values".into(),
                            Field::Object(BTreeMap::from([
                                ("field_198".into(), Field::Unsigned(2158)),
                                ("field_199".into(), Field::Unsigned(progress)),
                                ("field_200".into(), Field::Unsigned(timer)),
                            ])),
                        ),
                    ]))]),
                )]),
            },
        };
        assert!(super::quest_snapshot_matches(&snapshot(0x01000000, 0), 9, &fixture).unwrap());
        for (progress, timer) in [(0, 0), (0x01000001, 0), (0x02000000, 0), (0x01000000, 1)] {
            assert!(
                super::quest_snapshot_matches(&snapshot(progress, timer), 9, &fixture).is_err()
            );
        }
    }

    #[test]
    fn recording_time_follows_fixture_setup_for_each_schema() {
        for scenario in [
            include_str!("../scenarios/connection_identity.toml"),
            include_str!("../scenarios/two_character_visibility.toml"),
        ] {
            let (setup_finished_at, header) =
                prepare_recording(scenario, None, "test".into(), || {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    Ok(std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64)
                })
                .unwrap();
            assert!(header.recorded_at_unix_ms >= setup_finished_at);
        }
    }
}

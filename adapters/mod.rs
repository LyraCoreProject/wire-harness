use anyhow::{bail, ensure, Result};
use wire_client::cli::Invocation;
use wire_client::runner::{FixtureAdapter, PreparedClient};
use wire_client::scenario::FixtureVerb;
use wire_client::{logon_at, WireClient};
use wow_world_messages::vanilla::{opcodes::ServerOpcodeMessage as Smsg, CMSG_CHAR_ENUM};

#[path = "lyracore/fixture.rs"]
mod lyracore_fixture_impl;
#[path = "vmangos/fixture.rs"]
mod vmangos_fixture_impl;

pub fn select(invocation: &Invocation) -> Result<Box<dyn FixtureAdapter + '_>> {
    match std::env::var("WIRE_ADAPTER").as_deref() {
        Ok("lyracore") => Ok(Box::new(lyracore_fixture_impl::Adapter::new(invocation)?)),
        Ok("vmangos") => Ok(Box::new(vmangos_fixture_impl::Adapter(
            invocation,
            Vec::new(),
            None,
        ))),
        _ => bail!("set WIRE_ADAPTER to lyracore or vmangos"),
    }
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
    ensure!(
        character.map.as_int() == at.map
            && (character.position.x - at.x).abs() < 0.1
            && (character.position.y - at.y).abs() < 0.1
            && (character.position.z - at.z).abs() < 0.1,
        "fixture position did not apply"
    );
    let guid = character.guid.guid();
    client.start_player_login(guid)?;
    let mut opening = Vec::new();
    loop {
        let (opcode, body) = client.recv_raw()?;
        let observed = wire_client::scenario::Observation::opening(opcode, &body);
        if opcode == 0x0236 {
            use wire_client::decode::Field;
            let fields = &observed.packet.fields;
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

pub fn record(invocation: &Invocation, path: &str, golden: bool) -> Result<()> {
    use wire_client::trace::{Header, Trace};
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
    let mut adapter = select(invocation)?;
    let mut client = adapter.prepare(&scenario.fixtures)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    let mut header = match provenance {
        Some(provenance) => Header::golden(&scenario_text, provenance, now)?,
        None => Header::candidate(&scenario_text, adapter_name, now)?,
    };
    header.character_guid = client.client.self_guid;
    let result = wire_client::runner::run_on(&scenario, &mut client);
    drop(client);
    let cleanup = adapter.cleanup();
    let observed = result?;
    cleanup?;
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

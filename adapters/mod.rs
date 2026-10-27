use anyhow::{bail, ensure, Result};
use wire_client::cli::Invocation;
use wire_client::runner::FixtureAdapter;
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
        Ok("vmangos") => Ok(Box::new(vmangos_fixture_impl::Adapter(invocation))),
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
    } = fixture;
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

fn reconnect_checked(inv: &Invocation, fixture: &FixtureVerb) -> Result<WireClient> {
    let FixtureVerb::Character {
        name,
        race,
        class,
        level,
        at,
    } = fixture;
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
    let position = client.player_login_position(guid)?;
    ensure!(
        position.map.as_int() == at.map
            && (position.position.x - at.x).abs() < 0.1
            && (position.position.y - at.y).abs() < 0.1
            && (position.position.z - at.z).abs() < 0.1
            && (position.orientation - at.orientation).abs() < 0.01,
        "fixture login position or orientation did not apply"
    );
    Ok(client)
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
    let header = match provenance {
        Some(provenance) => Header::golden(&scenario_text, provenance, now)?,
        None => Header::candidate(&scenario_text, adapter_name, now)?,
    };
    let observed = wire_client::runner::run_on(&scenario, &mut client)?;
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

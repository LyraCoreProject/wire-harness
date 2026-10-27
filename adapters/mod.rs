use anyhow::{bail, ensure, Result};
use wire_client::cli::Invocation;
use wire_client::scenario::{FixtureAdapter, FixtureVerb};
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
    ensure!(!inv.character.is_empty(), "--character is required");
    let (key, advertised) = logon_at(&inv.endpoints.logon_addr, &inv.account, &inv.password)?;
    WireClient::connect_world(&inv.endpoints.world_addr(&advertised), &inv.account, key)
}

fn create(inv: &Invocation, fixture: &FixtureVerb) -> Result<WireClient> {
    let FixtureVerb::Character { race, class, .. } = fixture;
    let mut client = connect(inv)?;
    ensure!(
        !client
            .char_enum()?
            .iter()
            .any(|(_, name, _)| name.eq_ignore_ascii_case(&inv.character)),
        "fixture Character already exists; choose a fresh name"
    );
    let guid = client.create_or_find_char_as(
        &inv.character,
        wire_client::cli::parse_class(class)?,
        wire_client::cli::parse_race(race)?,
    )?;
    client.player_login(guid)?;
    Ok(client)
}

fn reconnect_checked(inv: &Invocation, fixture: &FixtureVerb) -> Result<WireClient> {
    let FixtureVerb::Character {
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
        .find(|c| c.name.eq_ignore_ascii_case(&inv.character))
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
        (position.orientation - at.orientation).abs() < 0.01,
        "fixture orientation did not apply"
    );
    Ok(client)
}

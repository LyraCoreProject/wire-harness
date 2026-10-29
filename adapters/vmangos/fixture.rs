use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::runner::{FixtureAdapter, PreparedClient};
use wire_client::scenario::FixtureVerb;
use wire_client::{is_read_timeout, logon_at, LogonProofFailure, WireClient};
use wow_world_messages::vanilla::{
    opcodes::ServerOpcodeMessage as Smsg, CMSG_MESSAGECHAT_ChatType, Language, CMSG_MESSAGECHAT,
};

pub struct Adapter(pub Invocation, pub Vec<u64>, pub Option<WireClient>);

pub fn logon(inv: &Invocation) -> Result<([u8; 40], String)> {
    retry_proof(|| logon_at(&inv.endpoints.logon_addr, &inv.account, &inv.password))
}

fn retry_proof<T>(mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    for number in 1..=3 {
        match attempt() {
            Err(error) if error.downcast_ref::<LogonProofFailure>().is_some() => {
                if number == 3 {
                    eprintln!("[vmangos] SRP proof failed on all 3 logon handshakes");
                    return Err(error);
                }
                eprintln!("[vmangos] SRP proof failed on handshake {number}/3; retrying with a fresh challenge");
            }
            result => return result,
        }
    }
    unreachable!()
}

impl FixtureAdapter for Adapter {
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<PreparedClient> {
        ensure!(!fixtures.is_empty(), "Character fixture is required");
        ensure!(
            self.0.endpoints.logon_addr == "127.0.0.1:13724"
                && self.0.endpoints.world_addr_override.as_deref() == Some("127.0.0.1:18085"),
            "vmangos fixtures require the local Reference Server endpoints"
        );
        let checked = Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/adapters/vmangos/setup.py"
            ))
            .args(["--verify-account", &self.0.account])
            .status()?;
        ensure!(
            checked.success(),
            "Reference Server configuration or Account check failed"
        );
        let fixture = &fixtures[0];
        let FixtureVerb::Character {
            name,
            race,
            level,
            at,
            ..
        } = fixture
        else {
            anyhow::bail!("Character must be the first fixture")
        };
        let mut gm_inv = self.0.clone();
        gm_inv.account = std::env::var("WIRE_GM_ACCOUNT").context("WIRE_GM_ACCOUNT is required")?;
        gm_inv.character =
            std::env::var("WIRE_GM_CHARACTER").context("WIRE_GM_CHARACTER is required")?;
        gm_inv.password = self.0.peer_password(0)?.to_owned();
        ensure!(
            gm_inv.account != self.0.account,
            "setup requires a separate GM Account"
        );
        self.2 = Some(super::connect(&gm_inv)?);
        let gm = self.2.as_mut().context("GM connection")?;
        let gm_guid = gm
            .create_or_find_char(
                &gm_inv.character,
                wow_world_messages::vanilla::Class::Warrior,
            )
            .context("create GM setup Character")?;
        gm.player_login(gm_guid)?;
        let mut client = super::create(&self.0, fixture)?;
        client.send(&wow_world_messages::vanilla::CMSG_COMPLETE_CINEMATIC {})?;
        gm.send(&wow_world_messages::vanilla::CMSG_COMPLETE_CINEMATIC {})?;
        drain(gm, &mut client)?;
        if *level > 1 {
            command(gm, format!(".levelup {name} {}", level - 1))?;
        }
        let mut bindings = std::collections::BTreeMap::new();
        for verb in &fixtures[1..] {
            match verb {
                FixtureVerb::KnownFlightNode { node_id } => {
                    command(gm, format!(".go taxinode {node_id}"))?;
                    drain(gm, &mut client)?;
                    command(gm, format!(".namego {name}"))?;
                    drain(gm, &mut client)?;
                    let before = client.seen_guids.clone();
                    let entry = if matches!(race.as_str(), "human" | "dwarf" | "gnome" | "nightelf")
                    {
                        352
                    } else {
                        3310
                    };
                    command(gm, format!(".npc spawn add {entry}"))?;
                    let guid = super::new_creature(
                        &mut client,
                        &before,
                        entry,
                        &taxi_node_position(*node_id)?,
                    )?;
                    self.1.push(guid);
                    let discovered = discover_node(&mut client, guid, *node_id);
                    command(gm, format!(".npc spawn delete {}", guid & 0x00ff_ffff))?;
                    drain(gm, &mut client)?;
                    self.1.pop();
                    discovered?;
                }
                FixtureVerb::Money { copper } => {
                    gm.set_selection(client.self_guid)?;
                    command(gm, ".modify money -2147483647".into())?;
                    drain(gm, &mut client)?;
                    command(gm, format!(".modify money {copper}"))?;
                    drain(gm, &mut client)?;
                }
                FixtureVerb::Creature { .. } => (),
                FixtureVerb::Character { .. } => anyhow::bail!("duplicate Character fixture"),
            }
        }
        command(gm, position_command(at, 0.0))?;
        drain(gm, &mut client)?;
        command(gm, format!(".namego {name}"))?;
        drain(gm, &mut client)?;
        for verb in &fixtures[1..] {
            if let FixtureVerb::Creature { id, entry, offset } = verb {
                command(gm, position_command(at, *offset))?;
                drain(gm, &mut client)?;
                let before = client.seen_guids.clone();
                command(gm, format!(".npc spawn add {entry}"))?;
                let guid = super::new_creature(&mut client, &before, *entry, &at.offset(*offset))?;
                self.1.push(guid);
                bindings.insert(id.clone(), guid);
            }
        }
        drop(client);

        let mut prepared = super::reconnect_checked(&self.0, fixture)?;
        prepared.fixtures = bindings;
        Ok(prepared)
    }

    fn reconnect(&mut self, fixture: &FixtureVerb) -> Result<PreparedClient> {
        super::reconnect_existing(&self.0, fixture)
    }

    fn cleanup(&mut self) -> Result<()> {
        self.2 = None;
        if !self.1.is_empty() {
            let mut gm_inv = self.0.clone();
            gm_inv.account =
                std::env::var("WIRE_GM_ACCOUNT").context("WIRE_GM_ACCOUNT is required")?;
            gm_inv.character =
                std::env::var("WIRE_GM_CHARACTER").context("WIRE_GM_CHARACTER is required")?;
            gm_inv.password = self.0.peer_password(0)?.to_owned();
            let mut gm = super::connect(&gm_inv)?;
            let guid = gm.create_or_find_char(
                &gm_inv.character,
                wow_world_messages::vanilla::Class::Warrior,
            )?;
            gm.player_login(guid)?;
            for guid in &self.1 {
                command(&mut gm, format!(".npc spawn delete {}", guid & 0x00ff_ffff))?;
                drain_one(&mut gm, Duration::from_millis(250))?;
            }
        }
        self.1.clear();
        Ok(())
    }
}

fn taxi_node_position(node: u32) -> Result<wire_client::scenario::Position> {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/adapters/vmangos/setup.py"
        ))
        .args(["--taxi-node", &node.to_string()])
        .output()?;
    ensure!(
        output.status.success(),
        "Reference Server taxi node lookup failed"
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn position_command(at: &wire_client::scenario::Position, offset: f32) -> String {
    let at = at.offset(offset);
    format!(
        ".go xyzo {} {} {} {} {}",
        at.x, at.y, at.z, at.orientation, at.map
    )
}

fn discover_node(client: &mut WireClient, guid: u64, node_id: u32) -> Result<()> {
    client.set_recv_timeout(Duration::from_secs(5))?;
    for _ in 0..2 {
        client.send_raw(0x01ac, &guid.to_le_bytes())?;
        let reply = client
            .recv_until(|p| matches!(p, Smsg::SMSG_SHOWTAXINODES(_) | Smsg::SMSG_NEW_TAXI_PATH))?;
        if let Smsg::SMSG_SHOWTAXINODES(menu) = reply {
            ensure!(
                menu.nearest_node == node_id,
                "discovery reached a different flight node"
            );
            let index = (node_id - 1) as usize;
            ensure!(
                menu.nodes
                    .get(index / 32)
                    .is_some_and(|bits| bits & (1 << (index % 32)) != 0),
                "flight node is missing from the known-node mask"
            );
            return Ok(());
        }
    }
    anyhow::bail!("flight node was not learned")
}

fn drain(gm: &mut WireClient, client: &mut WireClient) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        drain_one(gm, Duration::from_millis(50))?;
        drain_one(client, Duration::from_millis(50))?;
    }
    Ok(())
}

fn drain_one(client: &mut WireClient, window: Duration) -> Result<()> {
    client.set_recv_timeout(Duration::from_millis(50))?;
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match client.recv() {
            Ok(Smsg::MSG_MOVE_TELEPORT_ACK(teleport))
                if teleport.guid.guid() == client.self_guid =>
            {
                client.send_raw(
                    0x00c7,
                    &teleport_ack(
                        teleport.guid.guid(),
                        teleport.movement_counter,
                        teleport.info.timestamp,
                    ),
                )?;
            }
            Ok(_) => (),
            Err(error) if is_read_timeout(&error) => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn teleport_ack(guid: u64, counter: u32, timestamp: u32) -> [u8; 16] {
    // VMangos's MoveTeleportAck reads a full GUID; the shared encoder packs it.
    let mut body = [0; 16];
    body[..8].copy_from_slice(&guid.to_le_bytes());
    body[8..12].copy_from_slice(&counter.to_le_bytes());
    body[12..].copy_from_slice(&timestamp.to_le_bytes());
    body
}

fn command(client: &mut WireClient, message: String) -> Result<()> {
    client.send(&CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Common,
        message,
    })
}

impl Drop for Adapter {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("Reference Server fixture cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teleport_ack_keeps_the_full_guid_on_the_wire() {
        assert_eq!(
            teleport_ack(7, 2, 0x01020304),
            [7, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 4, 3, 2, 1]
        );
    }

    #[test]
    fn proof_failure_gets_at_most_two_new_attempts() {
        for failure in [
            LogonProofFailure::Rejected,
            LogonProofFailure::ServerMismatch,
        ] {
            let mut replies = [Err(failure.into()), Ok(7)].into_iter();
            assert_eq!(retry_proof(|| replies.next().unwrap()).unwrap(), 7);
        }
        let mut attempts = 0;
        assert!(retry_proof::<()>(|| {
            attempts += 1;
            Err(LogonProofFailure::Rejected.into())
        })
        .is_err());
        assert_eq!(attempts, 3);
    }

    #[test]
    fn success_and_non_proof_failures_do_not_retry() {
        for fail in [false, true] {
            let mut attempts = 0;
            let result = retry_proof(|| {
                attempts += 1;
                if fail {
                    anyhow::bail!("connection closed");
                }
                Ok(())
            });
            assert_eq!(result.is_err(), fail);
            assert_eq!(attempts, 1);
        }
    }
}

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::runner::FixtureAdapter;
use wire_client::scenario::FixtureVerb;
use wire_client::{is_read_timeout, logon_at, LogonProofFailure, WireClient};
use wow_world_messages::vanilla::{
    opcodes::ServerOpcodeMessage as Smsg, CMSG_MESSAGECHAT_ChatType, Language,
    MSG_MOVE_TELEPORT_ACK_Client, CMSG_MESSAGECHAT,
};

pub struct Adapter<'a>(pub &'a Invocation);

pub fn logon(inv: &Invocation) -> Result<([u8; 40], String)> {
    retry_proof(|| logon_at(&inv.endpoints.logon_addr, &inv.account, &inv.password))
}

fn retry_proof<T>(mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    match attempt() {
        Err(error) if error.downcast_ref::<LogonProofFailure>().is_some() => {
            eprintln!("[vmangos] SRP proof failure; retrying logon once with a fresh challenge");
            attempt()
        }
        result => result,
    }
}

impl FixtureAdapter for Adapter<'_> {
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<WireClient> {
        ensure!(fixtures.len() == 1, "one Character fixture is required");
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
            name, level, at, ..
        } = fixture;
        let mut gm_inv = self.0.clone();
        gm_inv.account = std::env::var("WIRE_GM_ACCOUNT").context("WIRE_GM_ACCOUNT is required")?;
        gm_inv.character =
            std::env::var("WIRE_GM_CHARACTER").context("WIRE_GM_CHARACTER is required")?;
        gm_inv.password = self.0.peer_password(0)?.to_owned();
        ensure!(
            gm_inv.account != self.0.account,
            "setup requires a separate GM Account"
        );
        let mut gm = super::connect(&gm_inv)?;
        let gm_guid = gm
            .create_or_find_char(
                &gm_inv.character,
                wow_world_messages::vanilla::Class::Warrior,
            )
            .context("create GM setup Character")?;
        gm.player_login(gm_guid)?;
        let mut client = super::create(self.0, fixture)?;
        if *level > 1 {
            command(&mut gm, format!(".levelup {name} {}", level - 1))?;
        }
        command(
            &mut gm,
            format!(
                ".go xyzo {} {} {} {} {}",
                at.x, at.y, at.z, at.orientation, at.map
            ),
        )?;
        drain(&mut gm, &mut client)?;
        command(&mut gm, format!(".namego {name}"))?;
        drain(&mut gm, &mut client)?;
        drop(client);
        drop(gm);
        let mut client = super::reconnect_checked(self.0, fixture)?;
        // Checkpoint windows begin after the remaining login frames have drained.
        drain_one(&mut client, Duration::from_secs(1))?;
        Ok(client)
    }
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
                client.send(&MSG_MOVE_TELEPORT_ACK_Client {
                    guid: teleport.guid,
                    movement_counter: teleport.movement_counter,
                    time: Duration::from_millis(u64::from(teleport.info.timestamp)),
                })?;
            }
            Ok(_) => (),
            Err(error) if is_read_timeout(&error) => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn command(client: &mut WireClient, message: String) -> Result<()> {
    client.send(&CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Common,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_failure_gets_exactly_one_new_attempt() {
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
        assert_eq!(attempts, 2);
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

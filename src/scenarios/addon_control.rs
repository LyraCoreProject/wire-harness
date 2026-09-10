use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use wire_client::{is_read_timeout, WireClient};
use wow_world_messages::vanilla::{
    ClientMessage, MSG_MOVE_HEARTBEAT_Client, MSG_MOVE_START_FORWARD_Client, MSG_MOVE_STOP_Client,
    MovementInfo, MovementInfo_MovementFlags, Vector3d, CMSG_AREATRIGGER,
};

const COMMAND_LIMIT: u32 = 256;
const INPUT_LIMIT: u64 = 8_192;
const OUTPUT_LIMIT: usize = 1_024 * 1_024;
const ADDON_LANGUAGE: u32 = u32::MAX;

#[derive(Debug, Deserialize, Serialize)]
struct Command {
    ordinal: u32,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Addon {
        message: String,
        reply_prefix: String,
        #[serde(default)]
        pause_after_send: bool,
    },
    Move {
        from: [f32; 3],
        to: [f32; 3],
        speed: f32,
    },
    Areatrigger {
        trigger_id: u32,
    },
    Stop,
}

#[derive(Debug, Serialize)]
struct Packet {
    opcode: u32,
    body: Vec<u8>,
}

#[derive(Debug, Serialize, PartialEq)]
struct AddonReply {
    chat_type: u8,
    sender_guid: u64,
    text: String,
}

fn publish(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "control evidence exceeds byte limit"
    );
    let temporary = directory.join(format!(".{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    // A hard link publishes complete bytes atomically and refuses an existing result.
    fs::hard_link(&temporary, directory.join(name))?;
    fs::remove_file(temporary)?;
    Ok(())
}

fn read_command(path: &Path, ordinal: u32) -> Result<Command> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "command must be a regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= INPUT_LIMIT,
        "command exceeds byte limit"
    );
    let command: Command = serde_json::from_slice(&bytes).context("invalid control command")?;
    ensure!(
        command.ordinal == ordinal,
        "command ordinal does not match filename"
    );
    Ok(command)
}

fn addon_packet(message: &str) -> Result<Packet> {
    ensure!(
        !message.is_empty() && message.len() <= 511 && !message.contains('\0'),
        "addon message must contain 1 through 511 bytes without NUL"
    );
    let mut body = Vec::with_capacity(message.len() + 9);
    body.extend_from_slice(&1u32.to_le_bytes()); // PARTY has no recipient field.
    body.extend_from_slice(&ADDON_LANGUAGE.to_le_bytes());
    body.extend_from_slice(message.as_bytes());
    body.push(0);
    Ok(Packet {
        opcode: 0x0095,
        body,
    })
}

fn addon_reply(opcode: u16, body: &[u8]) -> Option<AddonReply> {
    if opcode != 0x0096 || body.len() > 4_096 || body.get(1..5)? != ADDON_LANGUAGE.to_le_bytes() {
        return None;
    }
    let chat_type = *body.first()?;
    let length_offset = match chat_type {
        1 => 21,         // PARTY carries speech-bubble and chat-credit GUIDs.
        2..=4 | 6 => 13, // RAID, GUILD, OFFICER, and WHISPER carry one GUID.
        _ => return None,
    };
    let sender_guid = u64::from_le_bytes(body.get(5..13)?.try_into().ok()?);
    let length = u32::from_le_bytes(
        body.get(length_offset..length_offset + 4)?
            .try_into()
            .ok()?,
    ) as usize;
    let start = length_offset + 4;
    let end = start.checked_add(length)?;
    if length == 0 || end.checked_add(1)? != body.len() || *body.get(end - 1)? != 0 {
        return None;
    }
    let text = std::str::from_utf8(body.get(start..end - 1)?).ok()?;
    if text.contains('\0') {
        return None;
    }
    Some(AddonReply {
        chat_type,
        sender_guid,
        text: text.to_owned(),
    })
}

fn receive(client: &mut WireClient, mut observe: impl FnMut(u16, &[u8])) -> Result<()> {
    match client.recv_observed(|opcode, body| observe(opcode, body)) {
        Ok(_) => Ok(()),
        Err(error) if is_read_timeout(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn send_typed<M: ClientMessage>(client: &mut WireClient, message: &M) -> Result<Packet> {
    let mut encoded = Vec::new();
    message.write_unencrypted_client(&mut encoded)?;
    client.send(message)?;
    Ok(Packet {
        opcode: M::OPCODE,
        body: encoded[6..].to_vec(),
    })
}

struct Walk {
    from: [f32; 3],
    delta: [f32; 3],
    orientation: f32,
    duration: Duration,
}

impl Walk {
    fn new(from: [f32; 3], to: [f32; 3], speed: f32) -> Result<Self> {
        ensure!(
            from.iter()
                .chain(to.iter())
                .all(|n| n.is_finite() && n.abs() <= 100_000.0),
            "movement coordinates are outside the supported range"
        );
        ensure!(
            speed.is_finite() && (0.1..=7.0).contains(&speed),
            "speed must be 0.1 through 7 yards per second"
        );
        let delta = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let distance = delta.iter().map(|n| n * n).sum::<f32>().sqrt();
        let seconds = distance / speed;
        ensure!(
            seconds > 0.0 && seconds <= 120.0,
            "movement must take at most 120 seconds and cover a positive distance"
        );
        Ok(Self {
            from,
            delta,
            orientation: delta[1].atan2(delta[0]),
            duration: Duration::from_secs_f32(seconds),
        })
    }

    fn info(&self, elapsed: Duration, timestamp: u32, moving: bool) -> MovementInfo {
        let fraction = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0);
        MovementInfo {
            flags: if moving {
                MovementInfo_MovementFlags::new_forward()
            } else {
                MovementInfo_MovementFlags::empty()
            },
            timestamp,
            position: Vector3d {
                x: self.from[0] + self.delta[0] * fraction,
                y: self.from[1] + self.delta[1] * fraction,
                z: self.from[2] + self.delta[2] * fraction,
            },
            orientation: self.orientation,
            fall_time: 0.0,
        }
    }
}

fn walk(
    client: &mut WireClient,
    walk: Walk,
    clock: Instant,
    deadline: Instant,
) -> Result<Vec<Packet>> {
    let started = Instant::now();
    ensure!(
        started + walk.duration < deadline,
        "movement exceeds scenario deadline"
    );
    let mut packets = vec![send_typed(
        client,
        &MSG_MOVE_START_FORWARD_Client {
            info: walk.info(Duration::ZERO, clock.elapsed().as_millis() as u32, true),
        },
    )?];
    let mut next_send = started + Duration::from_millis(200);
    loop {
        let now = Instant::now();
        ensure!(now < deadline, "scenario deadline reached during movement");
        if now.duration_since(started) >= walk.duration {
            packets.push(send_typed(
                client,
                &MSG_MOVE_STOP_Client {
                    info: walk.info(walk.duration, clock.elapsed().as_millis() as u32, false),
                },
            )?);
            return Ok(packets);
        }
        if now >= next_send {
            packets.push(send_typed(
                client,
                &MSG_MOVE_HEARTBEAT_Client {
                    info: walk.info(
                        now.duration_since(started),
                        clock.elapsed().as_millis() as u32,
                        true,
                    ),
                },
            )?);
            next_send = now + Duration::from_millis(200);
        }
        receive(client, |_, _| {})?;
    }
}

// Usage: vanilla-wire scenario addon-control <control_dir> <seconds> --account ... --password-stdin
pub(crate) fn try_dispatch(
    mode: &str,
    client: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<bool> {
    if mode != "addon-control" {
        return Ok(false);
    }
    let directory = PathBuf::from(super::require_path_arg(
        args,
        "addon-control <control_dir> <seconds>",
        "control_dir",
    )?);
    let seconds: u64 =
        super::require_path_arg(args, "addon-control <control_dir> <seconds>", "seconds")?
            .parse()?;
    ensure!(
        (1..=3_600).contains(&seconds) && args.next().is_none(),
        "expected a duration of 1 through 3600 seconds"
    );
    ensure!(
        fs::symlink_metadata(&directory)?.file_type().is_dir(),
        "control directory must exist and must not be a symlink"
    );
    client.set_recv_timeout(Duration::from_millis(50))?;
    let clock = Instant::now();
    let deadline = clock + Duration::from_secs(seconds);
    publish(
        &directory,
        "ready.json",
        &json!({ "character_guid": client.self_guid, "pid": std::process::id() }),
    )?;
    for ordinal in 1..=COMMAND_LIMIT {
        let path = directory.join(format!("command-{ordinal}.json"));
        while !path.try_exists()? {
            ensure!(
                Instant::now() < deadline,
                "scenario deadline waiting for command {ordinal}"
            );
            receive(client, |_, _| {})?;
        }
        ensure!(
            Instant::now() < deadline,
            "scenario deadline before command {ordinal}"
        );
        let command = read_command(&path, ordinal)?;
        let packets = match &command.operation {
            Operation::Addon {
                message,
                reply_prefix,
                ..
            } => {
                ensure!(
                    !reply_prefix.is_empty()
                        && reply_prefix.len() <= 511
                        && !reply_prefix.contains('\0'),
                    "invalid addon reply prefix"
                );
                let packet = addon_packet(message)?;
                client.send_raw(packet.opcode, &packet.body)?;
                vec![packet]
            }
            Operation::Move { from, to, speed } => {
                walk(client, Walk::new(*from, *to, *speed)?, clock, deadline)?
            }
            Operation::Areatrigger { trigger_id } => vec![send_typed(
                client,
                &CMSG_AREATRIGGER {
                    trigger_id: *trigger_id,
                },
            )?],
            Operation::Stop => Vec::new(),
        };
        publish(
            &directory,
            &format!("sent-{ordinal}.json"),
            &json!({ "ordinal": ordinal, "command": command, "packets": packets }),
        )?;
        match &command.operation {
            Operation::Addon {
                pause_after_send: true,
                ..
            } => {
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                bail!("scenario deadline while paused after command {ordinal}");
            }
            Operation::Addon { reply_prefix, .. } => {
                let reply_deadline = deadline.min(Instant::now() + Duration::from_secs(60));
                let mut reply = None;
                while reply.is_none() && Instant::now() < reply_deadline {
                    receive(client, |opcode, body| {
                        if let Some(candidate) = addon_reply(opcode, body) {
                            if candidate.text.starts_with(reply_prefix) {
                                reply = Some(candidate);
                            }
                        }
                    })?;
                }
                let reply = reply.context("addon reply deadline reached")?;
                publish(
                    &directory,
                    &format!("result-{ordinal}.json"),
                    &json!({ "ordinal": ordinal, "reply": reply }),
                )?;
            }
            Operation::Stop => {
                publish(
                    &directory,
                    &format!("result-{ordinal}.json"),
                    &json!({ "ordinal": ordinal, "stopped": true }),
                )?;
                return Ok(true);
            }
            _ => publish(
                &directory,
                &format!("result-{ordinal}.json"),
                &json!({ "ordinal": ordinal, "sent": true }),
            )?,
        }
    }
    bail!("control command limit reached without stop")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply_body(chat_type: u8, text: &[u8]) -> Vec<u8> {
        let mut body = vec![chat_type];
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&123u64.to_le_bytes());
        if chat_type == 1 {
            body.extend_from_slice(&456u64.to_le_bytes());
        }
        body.extend_from_slice(&((text.len() + 1) as u32).to_le_bytes());
        body.extend_from_slice(text);
        body.extend_from_slice(&[0, 0]);
        body
    }

    #[test]
    fn addon_party_bytes_have_no_whisper_recipient() {
        let packet = addon_packet("EXAMPLE\trequest").unwrap();
        assert_eq!(packet.opcode, 0x0095);
        assert_eq!(
            packet.body,
            b"\x01\x00\x00\x00\xff\xff\xff\xffEXAMPLE\trequest\0"
        );
        for invalid in [String::new(), "a\0b".to_owned(), "a".repeat(512)] {
            assert!(addon_packet(&invalid).is_err());
        }
    }

    #[test]
    fn addon_replies_preserve_the_complete_text_and_sender() {
        for chat_type in [1, 2, 3, 4, 6] {
            let body = reply_body(chat_type, b"EXAMPLE\treply|one|two");
            assert_eq!(
                addon_reply(0x0096, &body),
                Some(AddonReply {
                    chat_type,
                    sender_guid: 123,
                    text: "EXAMPLE\treply|one|two".to_owned(),
                })
            );
            for length in 0..body.len() {
                assert!(addon_reply(0x0096, &body[..length]).is_none());
            }
        }
    }

    #[test]
    fn addon_reply_refuses_wrong_language_length_and_terminators() {
        let valid = reply_body(6, b"reply");
        let mut invalid = valid.clone();
        invalid[1..5].copy_from_slice(&0u32.to_le_bytes());
        assert!(addon_reply(0x0096, &invalid).is_none());
        invalid = valid.clone();
        invalid[13..17].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(addon_reply(0x0096, &invalid).is_none());
        invalid = valid.clone();
        let nul = invalid.len() - 2;
        invalid[nul] = 1;
        assert!(addon_reply(0x0096, &invalid).is_none());
        assert!(addon_reply(0x0096, &reply_body(6, b"a\0b")).is_none());
        assert!(addon_reply(0x0096, &reply_body(6, &[0xff])).is_none());
        assert!(addon_reply(0x004D, &valid).is_none());
        invalid = valid;
        invalid.push(0);
        assert!(addon_reply(0x0096, &invalid).is_none());
    }

    #[test]
    fn command_parsing_refuses_misspelled_operations_and_fields() {
        let command: Command = serde_json::from_str(r#"{"ordinal":1,"kind":"addon","message":"X","reply_prefix":"Y","pause_after_send":true}"#).unwrap();
        assert!(matches!(
            command.operation,
            Operation::Addon {
                pause_after_send: true,
                ..
            }
        ));
        assert!(
            serde_json::from_str::<Command>(r#"{"ordinal":1,"kind":"adno","message":"X"}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Command>(
            r#"{"ordinal":1,"kind":"stop","unexpected":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Command>(
            r#"{"ordinal":1,"kind":"move","from":[0,0,0],"to":[7,0,0],"speed":7}"#
        )
        .is_ok());
    }

    #[test]
    fn movement_positions_follow_elapsed_time_and_declared_speed() {
        let walk = Walk::new([10.0, 20.0, 30.0], [24.0, 20.0, 30.0], 7.0).unwrap();
        assert_eq!(walk.duration, Duration::from_secs(2));
        let halfway = walk.info(Duration::from_secs(1), 1234, true);
        assert_eq!(
            halfway.position,
            Vector3d {
                x: 17.0,
                y: 20.0,
                z: 30.0
            }
        );
        assert_eq!(halfway.timestamp, 1234);
        let stopped = walk.info(Duration::from_secs(2), 2234, false);
        assert_eq!(stopped.position.x, 24.0);
        assert_eq!(stopped.flags, MovementInfo_MovementFlags::empty());
        for speed in [0.0, f32::NAN, 8.0] {
            assert!(Walk::new([0.0; 3], [7.0, 0.0, 0.0], speed).is_err());
        }
        assert!(Walk::new([0.0; 3], [1_000.0, 0.0, 0.0], 7.0).is_err());
        assert!(Walk::new([f32::INFINITY; 3], [0.0; 3], 7.0).is_err());
    }

    #[test]
    fn completed_evidence_cannot_replace_an_existing_result() {
        let directory = std::env::temp_dir().join(format!(
            "addon-control-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        publish(&directory, "result-1.json", &json!({ "first": true })).unwrap();
        assert!(publish(&directory, "result-1.json", &json!({ "second": true })).is_err());
        let retained: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("result-1.json")).unwrap()).unwrap();
        assert_eq!(retained, json!({ "first": true }));
        fs::remove_dir_all(directory).unwrap();
    }
}

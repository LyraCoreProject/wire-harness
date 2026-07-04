//! Chat/SOCIAL wire modes: who, friends/ignore, rolls, emotes, say-range, inspect --
//! flows that need a speaker and a listener session to observe each other.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::Class;
use wow_world_messages::vanilla::{CMSG_TEXT_EMOTE, TextEmote};
use wow_world_messages::vanilla::{
    Friend_FriendStatus, CMSG_ADD_FRIEND, CMSG_ADD_IGNORE, CMSG_FRIEND_LIST,
};
use wow_world_base::shared::friend_result_vanilla_tbc::FriendResult;
use wow_world_messages::vanilla::MSG_RANDOM_ROLL_Client;
use wow_world_messages::Guid;

use super::{extract_chat_text, ModeCtx};

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    match mode {
        "who" => who(c, args, mcx)?,
        "friend" => friend(c, args, mcx)?,
        "ignore-whisper" => ignore_whisper(c, args, mcx)?,
        "roll" => roll(c, args, mcx)?,
        "text-emote" => text_emote(c, args, mcx)?,
        "say-range" => say_range(c, args, mcx)?,
        "inspect" => inspect(c, args, mcx)?,
        _ => return Ok(false),
    }
    Ok(true)
}

// ---- who probe: send CMSG_WHO (no filters) and assert SMSG_WHO lists the online char ----
// Usage: wire-client [account] [password] [char-name] who [want-name]
// Pass: SMSG_WHO.online_players >= 1 and `want-name` (default = char-name) appears in the list.
fn who(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let want_name = args.next().unwrap_or_else(|| mcx.char_name.to_string());
    eprintln!("[who] sending CMSG_WHO (no filters), expecting {want_name} in response…");
    let (online_count, listed) = c.who_request()?;
    println!("[probe] SMSG_WHO online_players={online_count} listed={}", listed.len());
    for (name, level, class, race) in &listed {
        println!("[probe]   {name} level={level} class={class} race={race}");
    }
    if online_count == 0 {
        bail!("who: SMSG_WHO.online_players == 0 — no online characters (is the char in-world?)");
    }
    let found = listed.iter().any(|(n, _, _, _)| n.eq_ignore_ascii_case(&want_name));
    if !found {
        bail!("who: {want_name:?} not listed in SMSG_WHO players (listed: {listed:?})");
    }
    let (_, level, class, race) = listed.iter().find(|(n, _, _, _)| n.eq_ignore_ascii_case(&want_name)).unwrap();
    println!("[wire] WHO PASS \u{2713}  SMSG_WHO online={online_count} — {want_name} listed (level={level} class={class} race={race})");
    Ok(())
}

// ---- friend probe (work-item 130): CMSG_ADD_FRIEND by name -> SMSG_FRIEND_STATUS, then
// CMSG_FRIEND_LIST -> SMSG_FRIEND_LIST + SMSG_IGNORE_LIST. ----
// Usage: wire-client [account] [password] [char-name] friend <target-name>
// Pass: SMSG_FRIEND_STATUS is Added(Online|Offline) carrying the target's guid, then a fresh
// SMSG_FRIEND_LIST lists that guid as Online (this client and the target are both connected).
fn friend(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let target_name = args.next().unwrap_or_else(|| "dfsdfsd".into());
    eprintln!("[friend] sending CMSG_ADD_FRIEND({target_name:?})…");
    c.send(&CMSG_ADD_FRIEND { name: target_name.clone() })?;
    // Background AOI/relay traffic (nearby SMSG_UPDATE_OBJECT etc.) can interleave — recv_for
    // rides past it.
    let target_guid = match c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_FRIEND_STATUS(s) => {
            eprintln!("[friend] SMSG_FRIEND_STATUS result={:?} guid={:#x}", s.result, s.guid.guid());
            // `Already` is a pass too — a re-run of this probe against an already-added
            // friend still proves the name resolved to the right guid server-side.
            if !matches!(s.result, FriendResult::AddedOnline | FriendResult::AddedOffline | FriendResult::Already) {
                return Some(Err(anyhow!("friend: add_friend({target_name:?}) returned {:?}, want Added(Online|Offline)/Already", s.result)));
            }
            Some(Ok(s.guid.guid()))
        }
        _ => None,
    }) {
        Some(Ok(g)) => g,
        Some(Err(e)) => return Err(e),
        None => bail!("friend: timed out waiting for SMSG_FRIEND_STATUS"),
    };

    eprintln!("[friend] sending CMSG_FRIEND_LIST…");
    c.send(&CMSG_FRIEND_LIST {})?;
    let (mut got_list, mut got_ignore) = (false, false);
    let done = c.recv_for(std::time::Duration::from_secs(5), |m| {
        match m {
            Smsg::SMSG_FRIEND_LIST(l) => {
                got_list = true;
                let Some(row) = l.friends.iter().find(|f| f.guid.guid() == target_guid) else {
                    return Some(Err(anyhow!("friend: SMSG_FRIEND_LIST doesn't carry guid {target_guid:#x} (rows: {})", l.friends.len())));
                };
                eprintln!("[friend] SMSG_FRIEND_LIST: guid={target_guid:#x} status={}", row.status);
                if !matches!(row.status, Friend_FriendStatus::Online { .. }) {
                    return Some(Err(anyhow!("friend: SMSG_FRIEND_LIST carries guid {target_guid:#x} as {:?}, want Online (both clients are connected)", row.status)));
                }
            }
            Smsg::SMSG_IGNORE_LIST(_) => got_ignore = true,
            _ => {}
        }
        (got_list && got_ignore).then_some(Ok(()))
    });
    if let Some(Err(e)) = done {
        return Err(e);
    }
    if !got_list {
        bail!("friend: timed out waiting for SMSG_FRIEND_LIST");
    }
    println!("[wire] FRIEND PASS \u{2713}  add_friend({target_name:?}) -> guid {target_guid:#x}; SMSG_FRIEND_LIST carries them Online");
    Ok(())
}

// ---- ignore-whisper probe (work-item 130): the IGNORER (this client) adds the SPEAKER to their
// ignore list, then the speaker whispers a unique probe line — assert it NEVER arrives. ----
// Usage: wire-client [account] [password] [char-name] ignore-whisper <speaker-account> <speaker-password> <speaker-char>
// Pass: SMSG_FRIEND_STATUS(IgnoreAdded) for the speaker's guid, then no SMSG_MESSAGECHAT Whisper
// carrying the probe text reaches this (ignoring) client within the wait window.
fn ignore_whisper(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let char_name = mcx.char_name;
    let speaker_account = args.next().unwrap_or_else(|| "TEST2".into());
    let speaker_password = args.next().unwrap_or_else(|| "test123".into());
    let speaker_char = args.next().unwrap_or_else(|| "dfsdfsd".into());

    eprintln!("[ignore-whisper] ignorer={char_name} sending CMSG_ADD_IGNORE({speaker_char:?})…");
    c.send(&CMSG_ADD_IGNORE { name: speaker_char.clone() })?;
    match c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_FRIEND_STATUS(s) => {
            eprintln!("[ignore-whisper] SMSG_FRIEND_STATUS result={:?} guid={:#x}", s.result, s.guid.guid());
            if s.result != FriendResult::IgnoreAdded {
                return Some(Err(anyhow!("ignore-whisper: add_ignore({speaker_char:?}) returned {:?}, want IgnoreAdded", s.result)));
            }
            Some(Ok(()))
        }
        _ => None,
    }) {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => bail!("ignore-whisper: timed out waiting for SMSG_FRIEND_STATUS"),
    }

    // Drain anything already buffered before the speaker connects/whispers (predicate never matches).
    let _ = c.recv_raw_for(std::time::Duration::from_millis(300), |_, _| None::<()>);

    eprintln!("[ignore-whisper] connecting speaker as {speaker_account}/{speaker_char}…");
    let mut sc = WireClient::login_as(&speaker_account, &speaker_password, &speaker_char, Class::Warrior)?;
    let probe = format!("ignore-probe-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
    eprintln!("[ignore-whisper] speaker whispers {char_name:?}: {probe:?}");
    sc.send(&wow_world_messages::vanilla::CMSG_MESSAGECHAT {
        chat_type: wow_world_messages::vanilla::CMSG_MESSAGECHAT_ChatType::Whisper { target_player: char_name.to_string() },
        language: wow_world_messages::vanilla::Language::Universal,
        message: probe.clone(),
    })?;

    // The speaker still gets their own "To <ignorer>: ..." echo — drain it, not asserted here.
    let _ = sc.recv_for(std::time::Duration::from_secs(3), |m| match m {
        Smsg::SMSG_MESSAGECHAT(m) => {
            eprintln!("[ignore-whisper] speaker echo: {:?}", extract_chat_text(m));
            Some(())
        }
        _ => None,
    });

    // Assert: the ignorer never receives the probe whisper.
    let ignorer_heard = c
        .recv_for(std::time::Duration::from_secs(3), |m| match m {
            Smsg::SMSG_MESSAGECHAT(m) => {
                let msg_text = extract_chat_text(m);
                eprintln!("[ignore-whisper] ignorer got SMSG_MESSAGECHAT: {msg_text:?}");
                (msg_text.as_deref() == Some(probe.as_str())).then_some(())
            }
            _ => None,
        })
        .is_some();
    if ignorer_heard {
        bail!("ignore-whisper: FAIL — ignorer received the whisper despite ignoring the sender");
    }
    println!("[wire] IGNORE-WHISPER PASS \u{2713}  {speaker_char} added to ignore; whisper from them never arrived");
    Ok(())
}

// ---- roll probe: send MSG_RANDOM_ROLL_Client(1, 100) and assert MSG_RANDOM_ROLL_Server ----
// Usage: wire-client [account] [password] [char-name] roll [min] [max]
// Pass: MSG_RANDOM_ROLL_Server received with result in [min,max] and roller_guid == self_guid.
fn roll(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let min: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let max: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    eprintln!("[roll] sending MSG_RANDOM_ROLL_Client(min={min}, max={max}) as guid {:#x}…", c.self_guid);
    c.send(&MSG_RANDOM_ROLL_Client { minimum: min, maximum: max })?;
    // MSG_RANDOM_ROLL opcode: 0x01FB (same opcode for client and server direction in vanilla)
    const ROLL_OPCODE: u16 = 0x01FB;
    let got = c.recv_raw_for(std::time::Duration::from_secs(5), |opcode, payload| {
        if opcode == ROLL_OPCODE && payload.len() >= 20 {
            // MSG_RANDOM_ROLL_Server layout: u32 minimum, u32 maximum, u32 actual_roll, Guid (u64)
            let minimum = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            let maximum = u32::from_le_bytes(payload[4..8].try_into().unwrap());
            let result  = u32::from_le_bytes(payload[8..12].try_into().unwrap());
            let roller  = u64::from_le_bytes(payload[12..20].try_into().unwrap());
            eprintln!("[roll] MSG_RANDOM_ROLL_Server: min={minimum} max={maximum} result={result} roller={roller:#x}");
            Some((minimum, maximum, result, roller))
        } else {
            None
        }
    });
    match got {
        None => bail!("roll: no MSG_RANDOM_ROLL_Server (opcode 0x{ROLL_OPCODE:04X}) within 5s"),
        Some((minimum, maximum, result, roller)) => {
            if roller != c.self_guid {
                bail!("roll: roller_guid={roller:#x} but self_guid={:#x} (mismatch)", c.self_guid);
            }
            if minimum != min || maximum != max {
                bail!("roll: echoed range [{minimum},{maximum}], want [{min},{max}]");
            }
            if result < minimum || result > maximum {
                bail!("roll: result={result} outside range [{minimum},{maximum}]");
            }
            println!("[wire] ROLL PASS \u{2713}  MSG_RANDOM_ROLL_Server(min={minimum}, max={maximum}, result={result}, roller={roller:#x}) — result in range, guid matches");
            Ok(())
        }
    }
}

// ---- text-emote probe: verify a TARGETED emote resolves the target's name in SMSG_TEXT_EMOTE ----
// Usage: wire-client [account] [password] [char-name] text-emote
// Self-targets (target = own guid) so a single connection exercises the full pipeline: CMSG's
// target guid -> send_emote reducer -> game_emote_event.target_guid -> gateway resolves via
// game_character -> SMSG_TEXT_EMOTE.name. Pass: SMSG_TEXT_EMOTE.name == char_name (non-empty).
fn text_emote(
    c: &mut WireClient,
    _args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let char_name = mcx.char_name;
    eprintln!("[text-emote] sending CMSG_TEXT_EMOTE(wave, target=self {:#x})…", c.self_guid);
    c.send(&CMSG_TEXT_EMOTE {
        text_emote: TextEmote::Wave,
        emote: 0,
        target: Guid::new(c.self_guid),
    })?;
    // SMSG_TEXT_EMOTE opcode: 0x0105
    const TEXT_EMOTE_OPCODE: u16 = 0x0105;
    // The first SMSG_TEXT_EMOTE frame decides the probe (a too-short payload counts as no name,
    // exactly like the pre-recv_raw_for shape which broke on the first opcode match).
    let got: Option<String> = c
        .recv_raw_for(std::time::Duration::from_secs(5), |opcode, payload| {
            if opcode != TEXT_EMOTE_OPCODE {
                return None;
            }
            // guid(8) + text_emote(4) + emote(4) + SizedCString name (u32 len + bytes, no NUL)
            Some(if payload.len() >= 20 {
                let len = u32::from_le_bytes(payload[16..20].try_into().unwrap()) as usize;
                let name = String::from_utf8_lossy(&payload[20..20 + len.min(payload.len().saturating_sub(20))])
                    .trim_end_matches('\0')
                    .to_string();
                eprintln!("[text-emote] SMSG_TEXT_EMOTE name={name:?}");
                Some(name)
            } else {
                None
            })
        })
        .flatten();
    match got {
        None => bail!("text-emote: no SMSG_TEXT_EMOTE (opcode 0x{TEXT_EMOTE_OPCODE:04X}) within 5s"),
        Some(name) => {
            if name != char_name {
                bail!("text-emote: SMSG_TEXT_EMOTE.name={name:?}, want {char_name:?}");
            }
            println!("[wire] TEXT-EMOTE PASS \u{2713}  SMSG_TEXT_EMOTE.name={name:?} matches the targeted character");
            Ok(())
        }
    }
}

// ---- say-range probe: verify range-gated SAY relay ----
// Usage: wire-client [account] [password] [char-name] say-range [listener-account] [listener-password] [listener-char]
// Two connections: speaker (this client) + listener. Asserts:
//   a) Speaker receives their OWN SAY (self-echo, always delivered).
//   b) Listener at >25yd does NOT receive the SAY (range gate).
// Chars must be pre-positioned; this test relies on the stored coordinates in game_character.
fn say_range(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let char_name = mcx.char_name;
    let listener_account  = args.next().unwrap_or_else(|| "TEST2".into());
    let listener_password = args.next().unwrap_or_else(|| "test123".into());
    let listener_char     = args.next().unwrap_or_else(|| "dfsdfsd".into());

    eprintln!("[say-range] speaker={char_name} listener={listener_char}");
    eprintln!("[say-range] connecting listener as {listener_account}/{listener_char}…");
    // Use `create_or_find_char` path for the listener. We don't know the class here, so pick
    // Human Warrior as a safe default (the char must already exist in game_character).
    let mut lc = WireClient::login_as(&listener_account, &listener_password, &listener_char, Class::Warrior)?;

    // Drain any buffered packets from the listener before speaking (predicate never matches).
    let _ = lc.recv_raw_for(std::time::Duration::from_millis(500), |_, _| None::<()>);

    // Unique probe message (timestamped).
    let probe = format!("range-probe-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
    eprintln!("[say-range] speaker sends SAY: {probe:?}");
    c.send_say(&probe)?;

    // Assert 1: Speaker receives their OWN say (self-echo).
    let speaker_heard = c
        .recv_for(std::time::Duration::from_secs(3), |m| match m {
            Smsg::SMSG_MESSAGECHAT(m) => {
                let msg_text = extract_chat_text(m);
                eprintln!("[say-range] speaker got SMSG_MESSAGECHAT: {msg_text:?}");
                (msg_text.as_deref() == Some(probe.as_str())).then_some(())
            }
            _ => None,
        })
        .is_some();
    if !speaker_heard {
        bail!("say-range: FAIL — speaker did not receive their own SAY (self-echo broken)");
    }
    eprintln!("[say-range] speaker self-echo: OK");

    // Assert 2: Listener at >25yd does NOT receive the SAY.
    let listener_heard = lc
        .recv_for(std::time::Duration::from_secs(2), |m| match m {
            Smsg::SMSG_MESSAGECHAT(m) => {
                let msg_text = extract_chat_text(m);
                eprintln!("[say-range] listener got SMSG_MESSAGECHAT: {msg_text:?}");
                (msg_text.as_deref() == Some(probe.as_str())).then_some(())
            }
            _ => None,
        })
        .is_some();
    if listener_heard {
        bail!("say-range: FAIL — listener received SAY despite being >25yd away (range gate not working)");
    }
    eprintln!("[say-range] listener (>25yd) correctly did NOT receive the SAY");
    println!("[wire] SAY-RANGE PASS \u{2713}  speaker self-echo OK; listener >25yd silenced");
    Ok(())
}

// ---- inspect probe: CMSG_INSPECT -> SMSG_INSPECT(target guid) gated on range (work-item 137) ----
// Usage: wire-client [account] [password] [char-name] inspect <near_guid> <far_guid>
// `near_guid` must be an in-world player guid within 10yd of `char_name` (a real target replies
// SMSG_INSPECT carrying that guid); `far_guid` must be an in-world player guid beyond 10yd (the
// module's range gate rejects it, so the gateway sends nothing back). Guids come from
// `spacetime sql … "select guid, x, y, z from game_character"` — this probe only drives the wire.
fn inspect(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let near_guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: … inspect <near_guid> <far_guid>");
    let far_guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: … inspect <near_guid> <far_guid>");

    eprintln!("[inspect] sending CMSG_INSPECT for in-range guid={near_guid}…");
    c.send(&wow_world_messages::vanilla::CMSG_INSPECT { guid: Guid::new(near_guid) })?;
    let near_ok = c.recv_for(std::time::Duration::from_secs(3), |m| match m {
        Smsg::SMSG_INSPECT(r) => Some(r.guid.guid()),
        _ => None,
    });
    if near_ok != Some(near_guid) {
        bail!("inspect: FAIL — expected SMSG_INSPECT(guid={near_guid}) for the in-range target, got {near_ok:?}");
    }
    eprintln!("[inspect] in-range target: OK (SMSG_INSPECT guid={near_guid})");

    eprintln!("[inspect] sending CMSG_INSPECT for out-of-range guid={far_guid}…");
    c.send(&wow_world_messages::vanilla::CMSG_INSPECT { guid: Guid::new(far_guid) })?;
    let far_reply = c.recv_for(std::time::Duration::from_secs(2), |m| match m {
        Smsg::SMSG_INSPECT(r) => Some(r.guid.guid()),
        _ => None,
    });
    if far_reply.is_some() {
        bail!("inspect: FAIL — got SMSG_INSPECT({far_reply:?}) for an out-of-range target (range gate not working)");
    }
    eprintln!("[inspect] out-of-range target correctly got no SMSG_INSPECT");
    println!("[wire] INSPECT PASS \u{2713}  in-range={near_guid} acked; out-of-range={far_guid} silenced");
    Ok(())
}

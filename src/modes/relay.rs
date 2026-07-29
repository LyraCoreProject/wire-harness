//! Multi-client RELAY/AOI/soak modes: movement relay observers+senders, add/remove-on-move
//! AOI assertions, and the long-running soak bot (work-item 141).
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::Guid;

use super::{drain_until_file, read_packed_guid, require_path_arg, ModeCtx};

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    match mode {
        "relay-observer" => relay_observer(c, args, mcx)?,
        "relay-sender" => relay_sender(c, args, mcx)?,
        "aoi-observer" => aoi_observer(c, args, mcx)?,
        "aoi-mover" => aoi_mover(c, args, mcx)?,
        "soak" => soak(c, args, mcx)?,
        _ => return Ok(false),
    }
    Ok(true)
}

// ---- relay-observer: log in, signal ready, then listen for a relayed MSG_MOVE_JUMP_Server ----
// Usage: wire-client TEST2 test123 dfsdfsd relay-observer <ready_file>
// We write the script-owned ready file (the relay-sender side polls the same path), then listen
// for opcode 0xBB (MSG_MOVE_JUMP / MSG_MOVE_JUMP_Server — same opcode value 0x00BB = 187).
// Pass: opcode 0xBB received from a *different* guid (the sender's guid) within 5s.
fn relay_observer(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let ready = require_path_arg(args, "relay-observer <ready_file>", "ready_file")?;
    let char_name = mcx.char_name;
    eprintln!("[relay-observer] in-world as {} (guid {:#x}); signalling ready…", char_name, c.self_guid);
    std::fs::write(&ready, "1").ok();
    // Drain until the sender's jump arrives, keeping the socket alive.
    let got_jump = c
        .recv_raw_for(std::time::Duration::from_secs(10), |opcode, _payload| {
            // MSG_MOVE_JUMP_Server — received a relayed jump from another player
            (opcode == 0x00BB).then(|| {
                println!("[probe] received opcode 0x{opcode:04X} (MSG_MOVE_JUMP_Server) — relay confirmed");
            })
        })
        .is_some();
    std::fs::remove_file(&ready).ok();
    if got_jump {
        println!("[wire] RELAY-JUMP PASS \u{2713}  observer received MSG_MOVE_JUMP_Server from peer");
        return Ok(());
    }
    bail!("relay-observer: no MSG_MOVE_JUMP_Server (opcode 0xBB) received within 10s");
}

// ---- relay-sender: wait for observer ready, then send MSG_MOVE_JUMP ----
// Usage: wire-client TEST test123 Ginger relay-sender <ready_file>
// Waits for the script-owned ready file (written by relay-observer), then sends MSG_MOVE_JUMP.
fn relay_sender(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, MSG_MOVE_JUMP_Client};
    use wow_world_messages::vanilla::Vector3d;
    let ready = require_path_arg(args, "relay-sender <ready_file>", "ready_file")?;
    let char_name = mcx.char_name;
    eprintln!("[relay-sender] in-world as {} (guid {:#x}); waiting for observer…", char_name, c.self_guid);
    // Drain + wait for observer to signal ready. A recv error inside drain_until_file is NOT
    // terminal: with no ambient packet traffic near the pad, recv simply rides its socket
    // read-timeout — treating that as "break" collapsed the whole wait to a single file check
    // and flaked the suite whenever the observer's login was still in flight. Use a short
    // read-timeout so the ready-file poll stays responsive; only the 20s deadline ends the wait.
    let _ = c.set_recv_timeout(std::time::Duration::from_millis(500));
    let observer_ready = drain_until_file(c, &ready, 20);
    let _ = c.set_recv_timeout(std::time::Duration::from_secs(10));
    if !observer_ready {
        bail!("relay-sender: observer never became ready within 20s");
    }
    eprintln!("[relay-sender] observer ready — sending MSG_MOVE_JUMP…");
    // Send a minimal MSG_MOVE_JUMP: the char's current position (approximate), no extra flags.
    // The relay is keyed on the opcode (0xBB), not the movement flags.
    //
    // Sent 3× at 2s intervals, NOT once: the observer signals ready the moment its world
    // handshake completes, but its per-player game_movement_event SUBSCRIPTION can still be
    // applying for a beat after that (observed as a suite-context-only B->A flake — the
    // observer relogs fast off the previous direction's disconnect, and a single immediate
    // jump lands before its subscription is live). Any ONE received jump proves the relay, so
    // the retries only harden the handshake, never weaken the assertion.
    for attempt in 0..3u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        c.send(&MSG_MOVE_JUMP_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: 0,
                position: Vector3d { x: -8968.0, y: -129.0, z: 83.39 },
                orientation: 0.0,
                fall_time: 0.0,
            },
        })?;
    }
    println!("[relay-sender] MSG_MOVE_JUMP sent; observer should receive MSG_MOVE_JUMP_Server");
    Ok(())
}

// ---- aoi-observer <peer_guid> <cmd_file> <ack_file> <ready_file>: command-driven assertions ----
// The orchestrator writes a command into cmd_file ("expect-create" / "expect-move" /
// "expect-destroy" / "done"); the observer satisfies it against the live packet stream within
// 30s and answers "OK <cmd>" or "FAIL <cmd>" in ack_file. Login-time precondition: the peer
// must NOT be visible (it starts outside the 125yd AOI box).
fn aoi_observer(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    const USAGE: &str = "aoi-observer <peer_guid> <cmd_file> <ack_file> <ready_file>";
    let peer: u64 = args.next().and_then(|s| s.parse().ok()).expect("peer guid");
    let cmd_file = require_path_arg(args, USAGE, "cmd_file")?;
    let ack_file = require_path_arg(args, USAGE, "ack_file")?;
    let ready_file = require_path_arg(args, USAGE, "ready_file")?;
    let _ = std::fs::remove_file(&cmd_file);
    let _ = std::fs::remove_file(&ack_file);
    if c.seen_guids.contains(&peer) {
        bail!("aoi-observer: peer {peer:#x} already visible at login — stage it outside the AOI box first");
    }
    println!("[aoi] login precondition OK — peer {peer:#x} not visible (outside AOI)");
    c.set_recv_timeout(std::time::Duration::from_millis(300))?;
    std::fs::write(&ready_file, "1").ok();
    let mut moves_from_peer = 0u32;
    loop {
        // poll for a command, draining the socket meanwhile (records CREATE guids)
        let cmd = loop {
            if let Ok(cmdtext) = std::fs::read_to_string(&cmd_file) {
                let cmdtext = cmdtext.trim().to_string();
                if !cmdtext.is_empty() {
                    let _ = std::fs::remove_file(&cmd_file);
                    break cmdtext;
                }
            }
            let _ = c.recv();
        };
        if cmd == "done" {
            println!("[wire] AOI-OBSERVER PASS \u{2713}  all commanded assertions satisfied ({moves_from_peer} relayed peer moves seen)");
            return Ok(());
        }
        let window = std::time::Duration::from_secs(30);
        // "walk <x> <y> <z>" (109): the OBSERVER walks +60yd in x from that point — past a 50yd
        // GRID_CELL_SIZE boundary, so the gateway's AreaOfInterestTracker recenters its box. Every
        // other command here moves the PEER, which never exercises the observer's own recenter;
        // that gap is exactly how a recenter that silently dropped the motion subscription shipped.
        if let Some(rest) = cmd.strip_prefix("walk ") {
            use wow_world_messages::vanilla::{
                MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client,
            };
            let p: Vec<f32> = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if p.len() != 3 {
                std::fs::write(&ack_file, "FAIL walk (need <x> <y> <z>)").ok();
                bail!("aoi-observer: walk needs <x> <y> <z>, got {rest:?}");
            }
            for i in 0..12u32 {
                c.send(&MSG_MOVE_HEARTBEAT_Client {
                    info: MovementInfo {
                        flags: MovementInfo_MovementFlags::empty(),
                        timestamp: i * 300,
                        position: Vector3d { x: p[0] + (i as f32) * 5.0, y: p[1], z: p[2] },
                        orientation: 0.0,
                        fall_time: 0.0,
                    },
                })?;
                std::thread::sleep(std::time::Duration::from_millis(150));
                let _ = c.recv();
            }
            println!("[aoi] walked 60yd in +x from ({}, {}, {}) — box recentered", p[0], p[1], p[2]);
            std::fs::write(&ack_file, "OK walk").ok();
            continue;
        }
        // a command may carry an explicit guid ("expect-seen 42") overriding the launch peer —
        // used when the interesting guid (a freshly spawned bot) isn't known at observer start
        let (cmd, peer) = match cmd.split_once(' ') {
            Some((c, g)) => (c.to_string(), g.trim().parse().unwrap_or(peer)),
            None => (cmd, peer),
        };
        // A CREATE for `peer` in an SMSG_UPDATE_OBJECT is exactly what note_guids records into
        // seen_guids, so the recv_for predicates below match the peer's CREATE directly.
        let peer_created = |m: &Smsg| match m {
            Smsg::SMSG_UPDATE_OBJECT(u)
                if u.objects.iter().any(|o| wire_client::create_object_guid(o) == Some(peer)) =>
            {
                Some(())
            }
            _ => None,
        };
        let ok = match cmd.as_str() {
            // like expect-create but WITHOUT clearing the sighting: passes if the guid was
            // already CREATEd (e.g. it spawned moments ago) or shows up within the window
            "expect-seen" => {
                c.seen_guids.contains(&peer) || c.recv_for(window, peer_created).is_some()
            }
            "expect-create" => {
                // fresh CREATE only: clear the stale sighting so a re-enter is really asserted
                c.seen_guids.retain(|g| *g != peer);
                c.recv_for(window, peer_created).is_some()
            }
            "expect-move" => c
                .recv_raw_for(window, |op, payload| {
                    // MSG_MOVE_* server relays: 0x00B5..=0x00EE range; payload starts with the
                    // mover's PACKED guid.
                    if (0x00B5..=0x00EE).contains(&op) && read_packed_guid(payload) == Some(peer) {
                        moves_from_peer += 1;
                        Some(())
                    } else {
                        None
                    }
                })
                .is_some(),
            "expect-destroy" => c
                .recv_raw_for(window, |op, payload| {
                    // SMSG_DESTROY_OBJECT = 0x00AA, payload = plain LE u64 guid
                    if op == 0x00AA && payload.len() >= 8 {
                        let g = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                        (g == peer).then_some(())
                    } else {
                        None
                    }
                })
                .is_some(),
            other => {
                std::fs::write(&ack_file, format!("FAIL unknown-command {other}")).ok();
                bail!("aoi-observer: unknown command {other:?}");
            }
        };
        let verdict = if ok { "OK" } else { "FAIL" };
        println!("[aoi] {cmd} -> {verdict}");
        std::fs::write(&ack_file, format!("{verdict} {cmd}")).ok();
        if !ok { bail!("aoi-observer: {cmd} not satisfied within 30s"); }
    }
}

// ---- aoi-mover <cmd_file> <ready_file>: holds a session; on "burst <x> <y> <z>" sends 10
// MSG_MOVE_HEARTBEATs stepping around that position (the relay+persist path); on "exit"
// disconnects abruptly. ----
fn aoi_mover(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client};
    const USAGE: &str = "aoi-mover <cmd_file> <ready_file>";
    let cmd_file = require_path_arg(args, USAGE, "cmd_file")?;
    let ready_file = require_path_arg(args, USAGE, "ready_file")?;
    let _ = std::fs::remove_file(&cmd_file);
    c.set_recv_timeout(std::time::Duration::from_millis(300))?;
    std::fs::write(&ready_file, "1").ok();
    loop {
        let _ = c.recv();
        let Ok(cmdtext) = std::fs::read_to_string(&cmd_file) else { continue };
        let cmdtext = cmdtext.trim().to_string();
        if cmdtext.is_empty() { continue; }
        let _ = std::fs::remove_file(&cmd_file);
        if cmdtext == "exit" {
            println!("[wire] AOI-MOVER exiting (abrupt disconnect — the observer should see DESTROY)");
            return Ok(());
        }
        if let Some(rest) = cmdtext.strip_prefix("burst ") {
            let p: Vec<f32> = rest.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if p.len() == 3 {
                for i in 0..10u32 {
                    c.send(&MSG_MOVE_HEARTBEAT_Client {
                        info: MovementInfo {
                            flags: MovementInfo_MovementFlags::empty(),
                            timestamp: i * 300,
                            position: Vector3d { x: p[0] + (i as f32) * 0.4, y: p[1], z: p[2] },
                            orientation: 0.0,
                            fall_time: 0.0,
                        },
                    })?;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    let _ = c.recv();
                }
                println!("[aoi-mover] burst of 10 heartbeats sent around ({}, {}, {})", p[0], p[1], p[2]);
            }
        }
    }
}

// ---- soak <seconds> <cx> <cy> <cz>: random-walk movement heartbeats + periodic casts +
// engage/disengage cycles against whatever creatures are visible; prints a run summary. ----
fn soak(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{
        MovementInfo, MovementInfo_MovementFlags, Vector3d, CMSG_ATTACKSTOP, CMSG_ATTACKSWING,
        MSG_MOVE_HEARTBEAT_Client,
    };
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(600);
    let cx: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(-8920.0);
    let cy: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(-180.0);
    let cz: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(82.0);
    let cast_spell: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2050);
    // explicit engage targets (the orchestrator passes the guids it spawned); falls back to
    // creature guids seen in the login burst
    let targets: Vec<u64> = args.filter_map(|s: String| s.parse().ok()).collect();
    c.set_recv_timeout(std::time::Duration::from_millis(150))?;
    let start = std::time::Instant::now();
    let deadline = start + std::time::Duration::from_secs(secs);
    let (mut hb, mut casts, mut engages, mut packets, mut recv_errs) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut rng: u64 = 0x5eed_5eed; // deterministic LCG — reproducible walk
    let mut engaged: Option<u64> = None;
    let (mut last_cast, mut last_engage_flip) = (std::time::Instant::now(), std::time::Instant::now());
    // Kept hand-rolled: not a wait-for-packet loop — this is the soak DRIVER (movement/cast/
    // engage cadence between drains), which recv_for cannot express.
    while std::time::Instant::now() < deadline {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let dx = ((rng >> 33) % 300) as f32 / 10.0 - 15.0;
        let dy = ((rng >> 43) % 300) as f32 / 10.0 - 15.0;
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: hb as u32 * 400,
                position: Vector3d { x: cx + dx, y: cy + dy, z: cz },
                orientation: 0.0,
                fall_time: 0.0,
            },
        })?;
        hb += 1;
        if last_cast.elapsed() >= std::time::Duration::from_secs(10) {
            c.cast_spell(cast_spell, c.self_guid)?;
            casts += 1;
            last_cast = std::time::Instant::now();
        }
        if last_engage_flip.elapsed() >= std::time::Duration::from_secs(8) {
            match engaged.take() {
                Some(t) => { c.send(&CMSG_ATTACKSTOP {})?; let _ = t; }
                None => {
                    // explicit target list (round-robin), else the first visible CREATURE
                    // (HIGHGUID_UNIT = 0xF130 high bits) from the login burst
                    let pick = if targets.is_empty() {
                        c.seen_guids.iter().copied().find(|g| (*g >> 48) == 0xF130)
                    } else {
                        Some(targets[(engages as usize) % targets.len()])
                    };
                    if let Some(t) = pick {
                        c.set_selection(t)?;
                        c.send(&CMSG_ATTACKSWING { guid: Guid::new(t) })?;
                        engages += 1;
                        engaged = Some(t);
                    }
                }
            }
            last_engage_flip = std::time::Instant::now();
        }
        // drain whatever arrived (counts decoded + undecodable-but-consumed frames alike)
        // Kept hand-rolled: this drain counts consumed frames vs read-timeouts separately for
        // the summary and breaks at the first quiet read — recv_raw_for swallows the errors.
        let drain_until = std::time::Instant::now() + std::time::Duration::from_millis(250);
        while std::time::Instant::now() < drain_until {
            match c.recv_raw() {
                Ok(_) => packets += 1,
                Err(_) => { recv_errs += 1; break; }
            }
        }
    }
    println!(
        "[wire] SOAK SUMMARY \u{2713}  duration={}s heartbeats={hb} casts={casts} engage_cycles={engages} packets_seen={packets} recv_timeouts={recv_errs}",
        start.elapsed().as_secs()
    );
    Ok(())
}

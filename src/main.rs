//! Manual driver for the headless wire test-client.
//!
//! Usage: wire-client [account] [password] [char-name] [spell-id]
//! Defaults: TEST / test123 / Ginger.  With a spell-id it runs M2 (cast assertion):
//! it logs in, WAITS for a creature to appear nearby (spawn one externally via
//! `debug_spawn_at_feet <char_guid> <entry> <offset>`), then targets it and casts,
//! asserting the timed-cast SMSG sequence.

use anyhow::{bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::Class;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let account = args.next().unwrap_or_else(|| "TEST".into());
    let password = args.next().unwrap_or_else(|| "test123".into());
    let char_name = args.next().unwrap_or_else(|| "Ginger".into());
    let mode: Option<String> = args.next();

    eprintln!("[wire] logon + world handshake as {account} -> char {char_name}…");
    let mut c = WireClient::login_as(&account, &password, &char_name, Class::Warlock)?;
    println!(
        "[wire] M1 OK — in world as guid {} ({} nearby objects)",
        c.self_guid,
        c.seen_guids.len()
    );

    // ---- gossip probe: dump the SMSG the gateway sends for CMSG_GOSSIP_HELLO to an NPC ----
    if mode.as_deref() == Some("gossip") {
        let npc: u64 = args
            .next()
            .and_then(|s| s.parse().ok())
            .expect("usage: … gossip <npc_guid>");
        eprintln!("[wire] gossip-probe: CMSG_GOSSIP_HELLO -> {npc:#x}");
        c.gossip_hello(npc)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut saw_gossip = false;
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_GOSSIP_MESSAGE(g)) => {
                    saw_gossip = true;
                    let opts: Vec<String> =
                        g.gossips.iter().map(|o| o.message.clone()).collect();
                    println!(
                        "[probe] SMSG_GOSSIP_MESSAGE guid={:#x} title={:#x} quests={} options={:?}",
                        g.guid.guid(),
                        g.title_text_id,
                        g.quests.len(),
                        opts
                    );
                }
                Ok(Smsg::SMSG_GOSSIP_COMPLETE) => println!("[probe] SMSG_GOSSIP_COMPLETE"),
                Ok(Smsg::SMSG_QUESTGIVER_QUEST_LIST(_)) => println!("[probe] SMSG_QUESTGIVER_QUEST_LIST"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!(
            "[probe] done — {}",
            if saw_gossip {
                "gateway SENT SMSG_GOSSIP_MESSAGE (handler works → real-client wire-reject)"
            } else {
                "gateway sent NO gossip message (handler aborted server-side)"
            }
        );
        return Ok(());
    }

    let Some(spell_id) = mode.and_then(|s| s.parse::<u32>().ok()) else { return Ok(()) };

    // ---- M2: the orchestrator spawns a mob at Ginger's feet (she must be live) and writes
    // its exact guid to a file; we KEEP DRAINING the socket while polling that file, then
    // target + cast + assert. (Blocking on the file without draining would stall the socket
    // and the gateway would drop the connection — so we recv() every loop.)
    let target_file =
        std::env::var("WIRE_TARGET_FILE").unwrap_or_else(|_| "/tmp/wc_target".into());
    let _ = std::fs::remove_file(&target_file);
    eprintln!("[wire] M2 ready — draining socket, waiting for a target guid in {target_file}…");
    let mob = loop {
        c.recv()?; // keep the socket drained so the gateway doesn't backpressure + drop us
        if let Ok(s) = std::fs::read_to_string(&target_file) {
            if let Ok(g) = s.trim().parse::<u64>() {
                let _ = std::fs::remove_file(&target_file);
                break g;
            }
        }
    };
    println!("[wire] M2 — target {mob:#x}; casting spell {spell_id}…");

    c.set_selection(mob)?;
    c.cast_spell(spell_id, mob)?;

    let mut begin_timer: Option<u32> = None;
    let mut completion_start = false;
    let mut go_spell: Option<u32> = None;
    let mut go_unit: Option<u64> = None;
    let mut go_hits: Vec<u64> = vec![];
    let mut dmg: Option<u32> = None;
    let mut cooldown = false;

    // The completion fires ~cast_time later; read on a wall-clock deadline (the busy world
    // floods packets, so a fixed count can elapse before 1.7s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let m = match c.recv() {
            Ok(m) => m,
            Err(_) => break,
        };
        match m {
            Smsg::SMSG_SPELL_START(s) => {
                if s.timer == 0 {
                    completion_start = true;
                } else {
                    begin_timer = Some(s.timer);
                }
            }
            Smsg::SMSG_SPELL_GO(g) => {
                go_spell = Some(g.spell);
                go_hits = g.hits.iter().map(|h| h.guid()).collect();
                go_unit = g.targets.target_flags.get_unit().map(|u| u.unit_target.guid());
            }
            Smsg::SMSG_SPELLNONMELEEDAMAGELOG(d) => dmg = Some(d.damage),
            Smsg::SMSG_SPELL_COOLDOWN(_) => {
                cooldown = true;
                break;
            }
            Smsg::SMSG_CAST_RESULT(r) => bail!("cast REJECTED by server (bad target/gate): {r:?}"),
            _ => {}
        }
    }

    let mut fails: Vec<String> = vec![];
    if begin_timer != Some(1700) {
        fails.push(format!("begin SMSG_SPELL_START.timer = {begin_timer:?}, want Some(1700)"));
    }
    if !completion_start {
        fails.push("missing completion SMSG_SPELL_START(timer=0) — the START->GO close pair (cast-lock fix)".into());
    }
    if go_spell != Some(spell_id) {
        fails.push(format!("SMSG_SPELL_GO.spell = {go_spell:?}, want Some({spell_id})"));
    }
    if go_unit != Some(mob) {
        fails.push(format!(
            "SMSG_SPELL_GO.targets.unit_target = {go_unit:?}, want Some({mob:#x}) — projectile trajectory (projectile fix)"
        ));
    }
    if go_hits != vec![mob] {
        fails.push(format!("SMSG_SPELL_GO.hits = {go_hits:x?}, want [{mob:#x}]"));
    }
    if !cooldown {
        fails.push("missing SMSG_SPELL_COOLDOWN (lock release)".into());
    }

    if fails.is_empty() {
        println!(
            "[wire] M2 PASS \u{2713}  START(1700) -> START(0)+GO(unit={mob:#x}, hits=[mob], spell={spell_id}) -> dmg={dmg:?} -> COOLDOWN"
        );
        Ok(())
    } else {
        for f in &fails {
            eprintln!("[wire] FAIL: {f}");
        }
        bail!("M2: {} assertion(s) failed", fails.len())
    }
}

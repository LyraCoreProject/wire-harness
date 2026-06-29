//! Manual driver for the headless wire test-client.
//!
//! Usage: wire-client [account] [password] [char-name] [spell-id]
//! Defaults: TEST / test123 / Ginger.  With a spell-id it runs M2 (cast assertion):
//! it logs in, WAITS for a creature to appear nearby (spawn one externally via
//! `debug_spawn_at_feet <char_guid> <entry> <offset>`), then targets it and casts,
//! asserting the timed-cast SMSG sequence.

use anyhow::{bail, Result};
use wire_client::{logon, WireClient};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{Class, LogoutResult};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let account = args.next().unwrap_or_else(|| "TEST".into());
    let password = args.next().unwrap_or_else(|| "test123".into());
    let char_name = args.next().unwrap_or_else(|| "Ginger".into());
    let mode: Option<String> = args.next();

    // ---- char-enum-gear probe: verify SMSG_CHAR_ENUM equipment slots carry real display_ids ----
    // Usage: wire-client TEST test123 <char-name> char-enum-gear [slot] [want_display_id]
    // slot defaults to 15 (main-hand weapon). want_display_id defaults to 0 (asserts nonzero).
    // Pass: the named character's equipment slot has a non-zero display_id (or == want if given).
    if mode.as_deref() == Some("char-enum-gear") {
        let slot: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
        let want: Option<u32> = args.next().and_then(|s| s.parse().ok());
        eprintln!("[wire] char-enum-gear: logon + CMSG_CHAR_ENUM, checking {char_name} slot {slot}…");
        let (k, world_addr) = logon(&account, &password)?;
        let mut c = WireClient::connect_world(&world_addr, &account, k)?;
        let chars = c.char_enum_gear()?;
        let Some((_, _, display_ids)) = chars.iter().find(|(_, n, _)| n.eq_ignore_ascii_case(&char_name)) else {
            bail!("char-enum-gear: character {char_name:?} not found in SMSG_CHAR_ENUM ({} chars)", chars.len());
        };
        let got = display_ids.get(slot).copied().unwrap_or(0);
        println!("[probe] SMSG_CHAR_ENUM {char_name} slot {slot} display_id={got}");
        // Print full non-zero gear for inspection
        for (i, &did) in display_ids.iter().enumerate() {
            if did != 0 {
                println!("[probe]   slot {i} display_id={did}");
            }
        }
        let pass = match want {
            Some(w) => got == w,
            None => got != 0,
        };
        if pass {
            let desc = want.map(|w| format!("=={w}")).unwrap_or_else(|| "!=0".into());
            println!("[wire] CHAR-ENUM-GEAR PASS \u{2713}  {char_name} slot {slot} display_id={got} ({desc})");
            return Ok(());
        }
        let desc = want.map(|w| format!("want {w}, got {got}")).unwrap_or_else(|| format!("want non-zero, got 0 (naked)"));
        bail!("char-enum-gear: {char_name} slot {slot}: {desc}");
    }

    eprintln!("[wire] logon + world handshake as {account} -> char {char_name}…");
    let mut c = WireClient::login_as(&account, &password, &char_name, Class::Warlock)?;
    println!(
        "[wire] M1 OK — in world as guid {} ({} nearby objects)",
        c.self_guid,
        c.seen_guids.len()
    );

    // ---- item-query probe: assert SMSG_ITEM_QUERY_SINGLE_RESPONSE carries armor + stats ----
    // Usage: wire-client TEST test123 Ginger query-item <entry> [want_armor]
    // Exits 0 if armor == want_armor (default 105 for Blackrock Gauntlets entry 1448).
    if mode.as_deref() == Some("query-item") {
        let entry: u32 = args
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1448); // Blackrock Gauntlets — stat_armor=105, stat_strength=3
        let want_armor: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(105);
        eprintln!("[wire] query-item {entry} expecting armor={want_armor}");
        let (armor, block, sell_price, stats) = c.query_item(entry)?;
        println!(
            "[probe] SMSG_ITEM_QUERY_SINGLE_RESPONSE item={entry} armor={armor} block={block} sell_price={sell_price} stats={stats:?}"
        );
        let nonzero_stats: Vec<_> = stats.iter().filter(|&&(_, v)| v != 0).collect();
        if armor != want_armor {
            eprintln!("[wire] FAIL: armor={armor}, want {want_armor}");
            std::process::exit(1);
        }
        println!(
            "[wire] ITEM-QUERY PASS \u{2713}  armor={armor} block={block} sell_price={sell_price}c nonzero_stats={nonzero_stats:?}"
        );
        return Ok(());
    }

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

    // ---- ghost-reveal probe (PIECE 2): a viewer who dies + becomes a GHOST should get the spirit-healer
    // entity CREATE'd (the GW_AOI=0 on_update reveal). The orchestrator kills+repops via debug reducers
    // (the char's small guid). PASS = healer hidden while alive, then revealed on the ghost transition.
    if mode.as_deref() == Some("ghost") {
        let healer: u64 = args
            .next()
            .and_then(|s| s.parse().ok())
            .expect("usage: … ghost <healer_guid>");
        let before = c.seen_guids.contains(&healer);
        println!("[ghost] healer {healer:#x} visible while ALIVE: {before}  (want false — the alive-gate)");
        std::fs::write("/tmp/wc_ghost_ready", "1").ok();
        eprintln!("[ghost] ready — waiting for kill+repop, then the reveal CREATE…");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            c.recv()?; // recv() records CREATE guids into seen_guids
            if c.seen_guids.contains(&healer) {
                break;
            }
        }
        let after = c.seen_guids.contains(&healer);
        println!("[ghost] healer visible after GHOST transition: {after}  (want true — the reveal)");
        if !before && after {
            println!("[wire] GHOST-REVEAL PASS \u{2713}  hidden while alive, CREATE'd on the ghost transition");
            return Ok(());
        }
        bail!("ghost-reveal: before={before} after={after} (want false->true)");
    }

    // ---- logout probe: assert out-of-combat logout replies Success + LOGOUT_COMPLETE ----
    // Usage: wire-client [account] [password] [char-name] logout
    // Pass: SMSG_LOGOUT_RESPONSE(Success, Instant) → SMSG_LOGOUT_COMPLETE
    // Fail: FailureInCombat or timeout.
    if mode.as_deref() == Some("logout") {
        eprintln!("[wire] logout probe — sending CMSG_LOGOUT_REQUEST…");
        let result = c.logout_request()?;
        match result {
            LogoutResult::Success => {
                println!("[wire] LOGOUT PASS \u{2713}  SMSG_LOGOUT_RESPONSE(Success, Instant) + SMSG_LOGOUT_COMPLETE");
                return Ok(());
            }
            other => bail!("logout: expected Success, got {other:?}"),
        }
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
    let mut failure: Option<u32> = None;
    // WIRE_EXPECT_INTERRUPT: the caster is meant to be hit mid-cast → assert SMSG_SPELL_FAILURE + no GO
    // (the cast-interrupt relay), instead of the normal START->GO->COOLDOWN completion.
    let expect_interrupt = std::env::var("WIRE_EXPECT_INTERRUPT").is_ok();

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
            Smsg::SMSG_SPELL_FAILURE(f) => {
                failure = Some(f.spell);
                if expect_interrupt {
                    break;
                }
            }
            Smsg::SMSG_SPELL_COOLDOWN(_) => {
                cooldown = true;
                break;
            }
            Smsg::SMSG_CAST_RESULT(r) => bail!("cast REJECTED by server (bad target/gate): {r:?}"),
            _ => {}
        }
    }

    if expect_interrupt {
        let mut fails: Vec<String> = vec![];
        if begin_timer != Some(1700) {
            fails.push(format!("begin SMSG_SPELL_START.timer = {begin_timer:?}, want Some(1700)"));
        }
        if failure != Some(spell_id) {
            fails.push(format!(
                "NO SMSG_SPELL_FAILURE(spell={spell_id}) — the cast was NOT interrupted (failure={failure:?})"
            ));
        }
        if go_spell == Some(spell_id) {
            fails.push("got SMSG_SPELL_GO — the cast COMPLETED instead of being interrupted".into());
        }
        if fails.is_empty() {
            println!(
                "[wire] INTERRUPT PASS \u{2713}  START(1700) -> SMSG_SPELL_FAILURE(spell={spell_id}) with NO GO — damage cancelled the cast"
            );
            return Ok(());
        }
        for f in &fails {
            eprintln!("[wire] FAIL: {f}");
        }
        bail!("interrupt: {} assertion(s) failed", fails.len());
    }

    let mut fails: Vec<String> = vec![];
    if begin_timer != Some(1700) {
        fails.push(format!("begin SMSG_SPELL_START.timer = {begin_timer:?}, want Some(1700)"));
    }
    if completion_start {
        fails.push("UNEXPECTED completion SMSG_SPELL_START(timer=0) — a TIMED completion must send GO ALONE (the begin START already opened the bar). A 2nd START(0) resets the cast bar to a zero-length cast → 'stuck on full'. The relay gates the START on !is_completion.".into());
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
            "[wire] M2 PASS \u{2713}  START(1700) -> GO(unit={mob:#x}, hits=[mob], spell={spell_id}) [no 2nd START] -> dmg={dmg:?} -> COOLDOWN"
        );
        Ok(())
    } else {
        for f in &fails {
            eprintln!("[wire] FAIL: {f}");
        }
        bail!("M2: {} assertion(s) failed", fails.len())
    }
}

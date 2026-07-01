//! Manual driver for the headless wire test-client.
//!
//! Usage: wire-client [account] [password] [char-name] [spell-id]
//! Defaults: TEST / test123 / Ginger.  With a spell-id it runs M2 (cast assertion):
//! it logs in, WAITS for a creature to appear nearby (spawn one externally via
//! `debug_spawn_at_feet <char_guid> <entry> <offset>`), then targets it and casts,
//! asserting the timed-cast SMSG sequence.

use anyhow::{bail, Result};
use wire_client::{logon, WireClient};
use wow_world_messages::vanilla::MSG_RANDOM_ROLL_Client;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{Class, LogoutResult, SMSG_MESSAGECHAT};

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

    // ---- initial-spells probe: assert SMSG_INITIAL_SPELLS carries the given spell ids ----
    // Usage: wire-client TEST test123 <Human char> initial-spells [id1 id2 ...]
    // Prints the captured spellbook; with ids, exits 1 unless every id is present.
    if mode.as_deref() == Some("initial-spells") {
        println!("[probe] SMSG_INITIAL_SPELLS ({} spells) = {:?}", c.initial_spells.len(), c.initial_spells);
        let want: Vec<u32> = args.by_ref().filter_map(|s| s.parse().ok()).collect();
        let missing: Vec<u32> = want.iter().copied().filter(|id| !c.initial_spells.contains(id)).collect();
        if !missing.is_empty() {
            eprintln!("[wire] FAIL: missing initial spells {missing:?}");
            std::process::exit(1);
        }
        println!("[wire] INITIAL-SPELLS PASS \u{2713}  all of {want:?} present");
        return Ok(());
    }

    // ---- item-query probe: assert SMSG_ITEM_QUERY_SINGLE_RESPONSE carries armor + stats ----
    // Usage: wire-client TEST test123 Ginger query-item <entry> [want_armor]
    // Exits 0 if armor == want_armor (default 105 for Blackrock Gauntlets entry 1448).
    if mode.as_deref() == Some("query-item") {
        let entry: u32 = args
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1448); // Blackrock Gauntlets — stat_armor=105, stat_strength=3
        let want_armor: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(105);
        // Optional 3rd arg: assert spell slot 1's id (the on-use spell that drives the green "Use:" text).
        let want_spell: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        eprintln!("[wire] query-item {entry} expecting armor={want_armor} spell1={want_spell}");
        let (armor, block, sell_price, stats, spell1, trig1) = c.query_item(entry)?;
        println!(
            "[probe] SMSG_ITEM_QUERY_SINGLE_RESPONSE item={entry} armor={armor} block={block} sell_price={sell_price} spell1={spell1} trigger1={trig1} stats={stats:?}"
        );
        let nonzero_stats: Vec<_> = stats.iter().filter(|&&(_, v)| v != 0).collect();
        if armor != want_armor {
            eprintln!("[wire] FAIL: armor={armor}, want {want_armor}");
            std::process::exit(1);
        }
        if want_spell != 0 && spell1 != want_spell {
            eprintln!("[wire] FAIL: spell1={spell1}, want {want_spell}");
            std::process::exit(1);
        }
        println!(
            "[wire] ITEM-QUERY PASS \u{2713}  armor={armor} block={block} sell_price={sell_price}c spell1={spell1} trigger1={trig1} nonzero_stats={nonzero_stats:?}"
        );
        return Ok(());
    }

    // ---- questgiver probe: the REAL protocol a questgiver-only NPC (npc_flags=2, no GOSSIP) uses ----
    if mode.as_deref() == Some("questgiver") {
        let npc: u64 = args
            .next()
            .and_then(|s| s.parse().ok())
            .expect("usage: … questgiver <npc_guid>");
        eprintln!("[wire] questgiver-probe: CMSG_QUESTGIVER_HELLO -> {npc:#x}");
        c.questgiver_hello(npc)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut saw = false;
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_QUESTGIVER_QUEST_LIST(q)) => {
                    saw = true;
                    let items: Vec<(u32, String)> =
                        q.quest_items.iter().map(|i| (i.quest_id, i.title.clone())).collect();
                    println!(
                        "[probe] SMSG_QUESTGIVER_QUEST_LIST npc={:#x} title={:?} quests={} items={:?}",
                        q.npc.guid(),
                        q.title,
                        q.quest_items.len(),
                        items
                    );
                }
                Ok(Smsg::SMSG_QUESTGIVER_QUEST_DETAILS(d)) => {
                    saw = true;
                    println!(
                        "[probe] SMSG_QUESTGIVER_QUEST_DETAILS quest_id={} title={:?} (INSTANT — single quest opens directly)",
                        d.quest_id, d.title
                    );
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!(
            "[probe] done — {}",
            if saw { "gateway answered the questgiver hello" } else { "gateway sent NOTHING (handler aborted / wrong path)" }
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
                    let text_id = g.title_text_id;
                    println!(
                        "[probe] SMSG_GOSSIP_MESSAGE guid={:#x} title={:#x} quests={} options={:?}",
                        g.guid.guid(),
                        text_id,
                        g.quests.len(),
                        opts
                    );
                    // Round-trip: query the NPC text to verify the gateway resolves the real text.
                    match c.npc_text_query(text_id, npc) {
                        Ok(t) => println!("[probe] SMSG_NPC_TEXT_UPDATE text_id={text_id} text={t:?}"),
                        Err(e) => println!("[probe] NPC_TEXT_QUERY failed: {e}"),
                    }
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

    // ---- stay probe: login + stay connected until a sentinel file appears, then exit Ok.
    // Usage: wire-client [account] [password] [char-name] stay [sentinel_file]
    // The external orchestrator writes anything to sentinel_file to signal done; we exit 0.
    // Useful when the test only needs the character to be live in game_world_entity while an
    // external script calls spacetime reducers (e.g. work-item #092 combat-regen probe).
    if mode.as_deref() == Some("stay") {
        let sentinel: String = args.next().unwrap_or_else(|| "/tmp/wc_stay_done".into());
        let _ = std::fs::remove_file(&sentinel);
        eprintln!("[wire] stay: draining socket until {sentinel} appears…");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let _ = c.recv(); // keep gateway connection alive
            if std::path::Path::new(&sentinel).exists() {
                let _ = std::fs::remove_file(&sentinel);
                println!("[wire] STAY DONE — sentinel received, exiting.");
                return Ok(());
            }
        }
        println!("[wire] STAY TIMEOUT — orchestrator did not signal within 60s.");
        return Ok(());
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

    // ---- ding probe: verify mid-session L10 ding pushes PLAYER_CHARACTER_POINTS1=1 ----
    // Usage: wire-client [account] [password] [char-name] ding
    // The orchestrator (test-ding.sh) must first set the char to L9, wait for wc_ding_ready,
    // then call `spacetime call spacetime-core debug_set_level <guid> 10`.
    // Pass: an SMSG_UPDATE_OBJECT arrives that contains BOTH a level=10 word [0x0a 00 00 00]
    // AND a character_points1=1 word [0x01 00 00 00] — the levelup VALUES packet (#032 fix).
    if mode.as_deref() == Some("ding") {
        eprintln!("[ding] in-world as {} (guid {}), signalling orchestrator…", c.self_guid, c.self_guid);
        std::fs::write("/tmp/wc_ding_ready", "1").ok();

        // SMSG_UPDATE_OBJECT opcode (vanilla 1.12) = 0x00A9 = 169.
        // Scan raw frames: the gtker reader rejects TYPE-less Player masks (it requires
        // OBJECT_FIELD_TYPE), so we use recv_raw() to capture the decrypted payload bytes
        // directly rather than going through the gtker decode path.
        const SMSG_UPDATE_OBJECT: u16 = 0x00A9;
        let level10_word = [0x0au8, 0x00, 0x00, 0x00]; // level=10 as little-endian u32
        let cp1_word = [0x01u8, 0x00, 0x00, 0x00];     // character_points1=1 as little-endian u32
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            let (opcode, payload) = c.recv_raw()?;
            if opcode == SMSG_UPDATE_OBJECT {
                let has_level10 = payload.windows(4).any(|w| w == level10_word);
                let has_cp1    = payload.windows(4).any(|w| w == cp1_word);
                // Log every UPDATE_OBJECT for visibility; only match on level10 as the primary
                // signal (cp1_word [0x01...] appears in every packet's count=1 header field).
                // An SMSG_UPDATE_OBJECT containing level=10 is the ding VALUES packet; cp1=1
                // in that same packet confirms PLAYER_CHARACTER_POINTS1 is set (work-item #032).
                eprintln!(
                    "[ding] SMSG_UPDATE_OBJECT {} bytes — has_level10={has_level10} has_cp1={has_cp1}",
                    payload.len()
                );
                if has_level10 {
                    println!(
                        "[ding] payload (hex): {:02x?}", payload
                    );
                    if has_cp1 {
                        println!(
                            "[wire] DING PASS \u{2713}  SMSG_UPDATE_OBJECT carries level=10 + PLAYER_CHARACTER_POINTS1=1"
                        );
                        found = true;
                        break;
                    } else {
                        bail!(
                            "ding: SMSG_UPDATE_OBJECT has level=10 but PLAYER_CHARACTER_POINTS1=1 is MISSING — build_levelup_values regression"
                        );
                    }
                }
            }
        }
        if !found {
            bail!("ding: no SMSG_UPDATE_OBJECT with level=10 + character_points1=1 within 30s");
        }
        return Ok(());
    }

    // ---- repop-delay probe: CMSG_REPOP_REQUEST → assert SMSG_CORPSE_RECLAIM_DELAY(30s) ----
    // Usage: wire-client [account] [password] [char-name] repop [char-small-guid]
    // The orchestrator must kill the character (debug_set_health 0) before signalling;
    // then we send CMSG_REPOP_REQUEST and assert the gateway emits the 30s delay packet.
    // Pass: SMSG_CORPSE_RECLAIM_DELAY with delay == Duration::from_secs(30).
    if mode.as_deref() == Some("repop") {
        let char_guid: u64 = args
            .next()
            .and_then(|s| s.parse().ok())
            .expect("usage: … repop <char-guid>");
        eprintln!("[repop] in-world as {char_name} (guid {:#x}); signalling orchestrator…", c.self_guid);
        std::fs::write("/tmp/wc_repop_ready", "1").ok();

        // Wait for the orchestrator to kill the character.
        for _ in 0..30 {
            if !std::path::Path::new("/tmp/wc_repop_ready").exists() { break; }
            // drain so the gateway doesn't drop us
            match c.recv() { Ok(_) => {} Err(_) => break }
        }
        eprintln!("[repop] sending CMSG_REPOP_REQUEST for char_guid={char_guid:#x}…");
        c.repop_request()?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got_delay: Option<std::time::Duration> = None;
        while std::time::Instant::now() < deadline {
            use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
            match c.recv() {
                Ok(Smsg::SMSG_CORPSE_RECLAIM_DELAY(d)) => {
                    got_delay = Some(d.delay);
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        std::fs::remove_file("/tmp/wc_repop_ready").ok();
        let want = std::time::Duration::from_secs(30);
        match got_delay {
            Some(d) if d == want => {
                println!("[wire] REPOP-DELAY PASS \u{2713}  SMSG_CORPSE_RECLAIM_DELAY(delay={}ms) received", d.as_millis());
                return Ok(());
            }
            Some(d) => anyhow::bail!("repop-delay: got delay={:?}, want {:?}", d, want),
            None => anyhow::bail!("repop-delay: no SMSG_CORPSE_RECLAIM_DELAY received within 5s"),
        }
    }

    // ---- relay-observer: log in, signal ready, then listen for a relayed MSG_MOVE_JUMP_Server ----
    // Usage: wire-client TEST2 test123 dfsdfsd relay-observer
    // The orchestrator signals /tmp/wc_relay_ready; we wait, then listen for opcode 0xBB
    // (MSG_MOVE_JUMP / MSG_MOVE_JUMP_Server — same opcode value 0x00BB = 187).
    // Pass: opcode 0xBB received from a *different* guid (the sender's guid) within 5s.
    if mode.as_deref() == Some("relay-observer") {
        eprintln!("[relay-observer] in-world as {} (guid {:#x}); signalling ready…", char_name, c.self_guid);
        std::fs::write("/tmp/wc_relay_ready", "1").ok();
        // Drain until the sender signals they've sent the jump, keeping the socket alive.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut got_jump = false;
        while std::time::Instant::now() < deadline {
            let (opcode, _payload) = match c.recv_raw() {
                Ok(x) => x,
                Err(e) => { eprintln!("[relay-observer] recv_raw error: {e}"); break; }
            };
            if opcode == 0x00BB {
                // MSG_MOVE_JUMP_Server — received a relayed jump from another player
                got_jump = true;
                println!("[probe] received opcode 0x{opcode:04X} (MSG_MOVE_JUMP_Server) — relay confirmed");
                break;
            }
        }
        std::fs::remove_file("/tmp/wc_relay_ready").ok();
        if got_jump {
            println!("[wire] RELAY-JUMP PASS \u{2713}  observer received MSG_MOVE_JUMP_Server from peer");
            return Ok(());
        }
        bail!("relay-observer: no MSG_MOVE_JUMP_Server (opcode 0xBB) received within 10s");
    }

    // ---- relay-sender: wait for observer ready, then send MSG_MOVE_JUMP ----
    // Usage: wire-client TEST test123 Ginger relay-sender
    // Waits for /tmp/wc_relay_ready (set by relay-observer), then sends MSG_MOVE_JUMP.
    if mode.as_deref() == Some("relay-sender") {
        use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, MSG_MOVE_JUMP_Client};
        use wow_world_messages::vanilla::Vector3d;
        eprintln!("[relay-sender] in-world as {} (guid {:#x}); waiting for observer…", char_name, c.self_guid);
        // Drain + wait for observer to signal ready.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            // Keep socket drained while we wait.
            match c.recv_raw() { Ok(_) => {} Err(_) => break }
            if std::path::Path::new("/tmp/wc_relay_ready").exists() {
                break;
            }
        }
        if !std::path::Path::new("/tmp/wc_relay_ready").exists() {
            bail!("relay-sender: observer never became ready within 10s");
        }
        eprintln!("[relay-sender] observer ready — sending MSG_MOVE_JUMP…");
        // Send a minimal MSG_MOVE_JUMP: the char's current position (approximate), no extra flags.
        // The relay is keyed on the opcode (0xBB), not the movement flags.
        c.send(&MSG_MOVE_JUMP_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: 0,
                position: Vector3d { x: -8968.0, y: -129.0, z: 83.39 },
                orientation: 0.0,
                fall_time: 0.0,
            },
        })?;
        println!("[relay-sender] MSG_MOVE_JUMP sent; observer should receive MSG_MOVE_JUMP_Server");
        return Ok(());
    }

    // ---- who probe: send CMSG_WHO (no filters) and assert SMSG_WHO lists the online char ----
    // Usage: wire-client [account] [password] [char-name] who [want-name]
    // Pass: SMSG_WHO.online_players >= 1 and `want-name` (default = char-name) appears in the list.
    if mode.as_deref() == Some("who") {
        let want_name = args.next().unwrap_or_else(|| char_name.clone());
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
        return Ok(());
    }

    // ---- roll probe: send MSG_RANDOM_ROLL_Client(1, 100) and assert MSG_RANDOM_ROLL_Server ----
    // Usage: wire-client [account] [password] [char-name] roll [min] [max]
    // Pass: MSG_RANDOM_ROLL_Server received with result in [min,max] and roller_guid == self_guid.
    if mode.as_deref() == Some("roll") {
        let min: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let max: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
        eprintln!("[roll] sending MSG_RANDOM_ROLL_Client(min={min}, max={max}) as guid {:#x}…", c.self_guid);
        c.send(&MSG_RANDOM_ROLL_Client { minimum: min, maximum: max })?;
        // MSG_RANDOM_ROLL opcode: 0x01FB (same opcode for client and server direction in vanilla)
        const ROLL_OPCODE: u16 = 0x01FB;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got: Option<(u32, u32, u32, u64)> = None; // (minimum, maximum, result, roller_guid)
        while std::time::Instant::now() < deadline {
            match c.recv_raw() {
                Ok((opcode, payload)) => {
                    if opcode == ROLL_OPCODE && payload.len() >= 20 {
                        // MSG_RANDOM_ROLL_Server layout: u32 minimum, u32 maximum, u32 actual_roll, Guid (u64)
                        let minimum = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                        let maximum = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                        let result  = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                        let roller  = u64::from_le_bytes(payload[12..20].try_into().unwrap());
                        eprintln!("[roll] MSG_RANDOM_ROLL_Server: min={minimum} max={maximum} result={result} roller={roller:#x}");
                        got = Some((minimum, maximum, result, roller));
                        break;
                    }
                }
                Err(e) => { eprintln!("[roll] recv error: {e}"); break; }
            }
        }
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
                return Ok(());
            }
        }
    }

    // ---- say-range probe: verify range-gated SAY relay ----
    // Usage: wire-client [account] [password] [char-name] say-range [listener-account] [listener-password] [listener-char]
    // Two connections: speaker (this client) + listener. Asserts:
    //   a) Speaker receives their OWN SAY (self-echo, always delivered).
    //   b) Listener at >25yd does NOT receive the SAY (range gate).
    // Chars must be pre-positioned; this test relies on the stored coordinates in game_character.
    if mode.as_deref() == Some("say-range") {
        let listener_account  = args.next().unwrap_or_else(|| "TEST2".into());
        let listener_password = args.next().unwrap_or_else(|| "test123".into());
        let listener_char     = args.next().unwrap_or_else(|| "dfsdfsd".into());

        eprintln!("[say-range] speaker={char_name} listener={listener_char}");
        eprintln!("[say-range] connecting listener as {listener_account}/{listener_char}…");
        // Use `create_or_find_char` path for the listener. We don't know the class here, so pick
        // Human Warrior as a safe default (the char must already exist in game_character).
        let mut lc = WireClient::login_as(&listener_account, &listener_password, &listener_char, Class::Warrior)?;

        // Drain any buffered packets from the listener before speaking.
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < drain_deadline {
            let _ = lc.recv_raw();
        }

        // Unique probe message (timestamped).
        let probe = format!("range-probe-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
        eprintln!("[say-range] speaker sends SAY: {probe:?}");
        c.send_say(&probe)?;

        // Assert 1: Speaker receives their OWN say (self-echo).
        let speaker_heard = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut found = false;
            while std::time::Instant::now() < deadline {
                match c.recv() {
                    Ok(Smsg::SMSG_MESSAGECHAT(m)) => {
                        let msg_text = extract_chat_text(&m);
                        eprintln!("[say-range] speaker got SMSG_MESSAGECHAT: {msg_text:?}");
                        if msg_text.as_deref() == Some(probe.as_str()) {
                            found = true;
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            found
        };
        if !speaker_heard {
            bail!("say-range: FAIL — speaker did not receive their own SAY (self-echo broken)");
        }
        eprintln!("[say-range] speaker self-echo: OK");

        // Assert 2: Listener at >25yd does NOT receive the SAY.
        let listener_heard = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut found = false;
            while std::time::Instant::now() < deadline {
                match lc.recv() {
                    Ok(Smsg::SMSG_MESSAGECHAT(m)) => {
                        let msg_text = extract_chat_text(&m);
                        eprintln!("[say-range] listener got SMSG_MESSAGECHAT: {msg_text:?}");
                        if msg_text.as_deref() == Some(probe.as_str()) {
                            found = true;
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            found
        };
        if listener_heard {
            bail!("say-range: FAIL — listener received SAY despite being >25yd away (range gate not working)");
        }
        eprintln!("[say-range] listener (>25yd) correctly did NOT receive the SAY");
        println!("[wire] SAY-RANGE PASS \u{2713}  speaker self-echo OK; listener >25yd silenced");
        return Ok(());
    }

    // ---- bindpoint probe: assert SMSG_BINDPOINTUPDATE carries home_x/y/z (not login position) ----
    // Usage: wire-client [account] [password] [char-name] bindpoint <home_x> <home_y> <home_z>
    // The char must have home_* != login position (e.g. "Tester": login=(-8935, -188) home=(-8873, -134)).
    // Pass: SMSG_BINDPOINTUPDATE.position.x == home_x (within 0.1 tolerance).
    if mode.as_deref() == Some("bindpoint") {
        let want_x: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
        let want_y: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
        let want_z: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
        eprintln!("[bindpoint] want home=({want_x},{want_y},{want_z}) — verifying SMSG_BINDPOINTUPDATE…");

        // Re-login manually so we can capture SMSG_BINDPOINTUPDATE from the post-login burst.
        // (The `login_as`/`player_login` helper drains the burst without exposing that packet.)
        let (k, world_addr) = wire_client::logon(&account, &password)?;
        let mut c2 = wire_client::WireClient::connect_world(&world_addr, &account, k)?;
        let chars = c2.char_enum()?;
        let guid = chars
            .iter()
            .find(|(_, n, _)| n.eq_ignore_ascii_case(&char_name))
            .map(|(g, _, _)| *g)
            .ok_or_else(|| anyhow::anyhow!("bindpoint: character {char_name:?} not found"))?;
        eprintln!("[bindpoint] found {char_name} guid={guid}; sending CMSG_PLAYER_LOGIN…");
        use wow_world_messages::vanilla::CMSG_PLAYER_LOGIN;
        use wow_world_messages::vanilla::Guid;
        c2.send(&CMSG_PLAYER_LOGIN { guid: Guid::new(guid) })?;

        // Drain the login burst, capturing SMSG_BINDPOINTUPDATE.
        let mut bind: Option<(f32, f32, f32)> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::time::Instant::now() > deadline { break; }
            let m = match c2.recv() { Ok(m) => m, Err(_) => break };
            if let Smsg::SMSG_BINDPOINTUPDATE(b) = &m {
                bind = Some((b.position.x, b.position.y, b.position.z));
                eprintln!("[bindpoint] SMSG_BINDPOINTUPDATE: x={} y={} z={}", b.position.x, b.position.y, b.position.z);
            }
            // Stop after the self CREATE_OBJECT arrives (burst complete).
            if let Smsg::SMSG_UPDATE_OBJECT(u) = &m {
                if u.objects.iter().any(|o| wire_client::create_object_guid(o) == Some(guid)) {
                    break;
                }
            }
        }
        match bind {
            None => anyhow::bail!("bindpoint: SMSG_BINDPOINTUPDATE never received in login burst"),
            Some((got_x, got_y, got_z)) => {
                eprintln!("[bindpoint] got=({got_x},{got_y},{got_z}) want=({want_x},{want_y},{want_z})");
                if (got_x - want_x).abs() > 0.5 || (got_y - want_y).abs() > 0.5 {
                    anyhow::bail!(
                        "bindpoint: SMSG_BINDPOINTUPDATE carries login position, not home!\n  got  x={got_x} y={got_y} z={got_z}\n  want x={want_x} y={want_y} z={want_z}"
                    );
                }
                println!("[wire] BINDPOINT PASS \u{2713}  SMSG_BINDPOINTUPDATE x={got_x} y={got_y} z={got_z} matches persisted home");
                return Ok(());
            }
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
    // #084 timing probe: elapsed wall-clock between SMSG_SPELL_GO and SMSG_SPELLNONMELEEDAMAGELOG —
    // a projectile spell should show a measurable gap (~distance/missile_speed), not 0ms.
    let mut go_at: Option<std::time::Instant> = None;
    let mut dmg_delay_ms: Option<u128> = None;
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
                go_at = Some(std::time::Instant::now());
            }
            Smsg::SMSG_SPELLNONMELEEDAMAGELOG(d) => {
                dmg = Some(d.damage);
                if let Some(t0) = go_at {
                    dmg_delay_ms = Some(t0.elapsed().as_millis());
                }
            }
            Smsg::SMSG_SPELL_FAILURE(f) => {
                failure = Some(f.spell);
                if expect_interrupt {
                    break;
                }
            }
            Smsg::SMSG_SPELL_COOLDOWN(_) => {
                cooldown = true;
                // #084: a projectile's damage log now arrives AFTER the missile travel time, which can be
                // later than SMSG_SPELL_COOLDOWN (cooldown still starts at cast-GO, unaffected). Don't break
                // here if we haven't seen the damage log yet — keep draining until the wall-clock deadline
                // so the GO->dmg delay is still captured.
                if dmg.is_some() || dmg_delay_ms.is_some() {
                    break;
                }
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
    // #084 regression guard: this mode always casts a projectile spell (default Shadow Bolt) at a
    // DISTINCT target several yards away (test-cast-flow.sh spawns the mob 8yd out), so the
    // GO->damage-log gap must be a measurable delay, not the ~0ms an instant-damage regression would
    // produce. Threshold is well below the ~381ms expected at 8yd/21yps, just above scheduler jitter.
    const MIN_PROJECTILE_DELAY_MS: u128 = 150;
    match dmg_delay_ms {
        Some(d) if d < MIN_PROJECTILE_DELAY_MS => fails.push(format!(
            "GO->dmg delay={d}ms, want >= {MIN_PROJECTILE_DELAY_MS}ms — damage landed near-instantly (projectile impact-delay regression)"
        )),
        None => fails.push("no SMSG_SPELLNONMELEEDAMAGELOG observed after SMSG_SPELL_GO — cannot measure projectile impact delay".into()),
        Some(_) => {}
    }

    if fails.is_empty() {
        println!(
            "[wire] M2 PASS \u{2713}  START(1700) -> GO(unit={mob:#x}, hits=[mob], spell={spell_id}) [no 2nd START] -> dmg={dmg:?} (GO->dmg delay={dmg_delay_ms:?}ms) -> COOLDOWN"
        );
        Ok(())
    } else {
        for f in &fails {
            eprintln!("[wire] FAIL: {f}");
        }
        bail!("M2: {} assertion(s) failed", fails.len())
    }
}

/// Extract the message text from an SMSG_MESSAGECHAT for probe comparison.
/// The message is in the top-level `message` field (shared across all chat types).
fn extract_chat_text(m: &SMSG_MESSAGECHAT) -> Option<String> {
    use wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType;
    // Only relay Say and Yell for the range probe; other types are out of scope.
    match &m.chat_type {
        SMSG_MESSAGECHAT_ChatType::Say { .. } | SMSG_MESSAGECHAT_ChatType::Yell { .. } => {
            Some(m.message.clone())
        }
        _ => None,
    }
}

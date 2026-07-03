//! Single-session PROBE modes: each asserts one wire exchange (a packet or a small
//! sequence) against the live gateway, with at most a handshake file toward the orchestrator.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{bail, Result};
use wire_client::{logon, WireClient};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{Class, LogoutResult};
use wow_world_messages::Guid;

use super::ModeCtx;

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    let ModeCtx { account, password, char_name } = *mcx;

    // ---- initial-spells probe: assert SMSG_INITIAL_SPELLS carries the given spell ids ----
    // Usage: wire-client TEST test123 <Human char> initial-spells [id1 id2 ...]
    // Prints the captured spellbook; with ids, exits 1 unless every id is present.
    if mode == "initial-spells" {
        println!("[probe] SMSG_INITIAL_SPELLS ({} spells) = {:?}", c.initial_spells.len(), c.initial_spells);
        let want: Vec<u32> = (&mut *args).filter_map(|s: String| s.parse().ok()).collect();
        let missing: Vec<u32> = want.iter().copied().filter(|id| !c.initial_spells.contains(id)).collect();
        if !missing.is_empty() {
            eprintln!("[wire] FAIL: missing initial spells {missing:?}");
            std::process::exit(1);
        }
        println!("[wire] INITIAL-SPELLS PASS \u{2713}  all of {want:?} present");
        return Ok(true);
    }

    // ---- init-factions probe: assert SMSG_INITIALIZE_FACTIONS carries a persisted standing on RELOG (076) ----
    // Usage: wire-client TEST test123 <char-name> init-factions <reputation_index> <want_standing>
    // `c` is already logged in once above; this probe reconnects fresh (a real relog) and re-runs
    // player_login, so the SMSG_INITIALIZE_FACTIONS captured is the one built by the *second* login
    // burst — exactly what a relogging client sees. Asserts slot[index] == want_standing.
    if mode == "init-factions" {
        let index: usize = args.next().and_then(|s| s.parse().ok()).expect("usage: init-factions <index> <want_standing>");
        let want: i32 = args.next().and_then(|s| s.parse().ok()).expect("usage: init-factions <index> <want_standing>");
        eprintln!("[wire] init-factions: relogging {char_name} to capture a fresh SMSG_INITIALIZE_FACTIONS…");
        let (k2, world_addr2) = logon(&account, &password)?;
        let mut c2 = WireClient::connect_world(&world_addr2, &account, k2)?;
        let guid = c2.create_or_find_char(&char_name, Class::Warlock)?;
        c2.player_login(guid)?;
        println!("[probe] SMSG_INITIALIZE_FACTIONS ({} slots) slot[{index}]={:?}", c2.init_factions.len(), c2.init_factions.get(index));
        let got = c2.init_factions.get(index).copied().unwrap_or(0);
        if got != want {
            bail!("init-factions: slot[{index}] = {got}, want {want}");
        }
        println!("[wire] INIT-FACTIONS PASS \u{2713}  slot[{index}] == {want} on relog");
        return Ok(true);
    }

    // ---- item-query probe: assert SMSG_ITEM_QUERY_SINGLE_RESPONSE carries armor + stats ----
    // Usage: wire-client TEST test123 Ginger query-item <entry> [want_armor] [want_spell] [want_bonding]
    // Exits 0 if armor == want_armor (default 105 for Blackrock Gauntlets entry 1448).
    // want_bonding: 4th optional arg, -1 (default) = don't check (0=NoBind,1=BoP,2=BoE,3=BoU work-item 127).
    if mode == "query-item" {
        let entry: u32 = args
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1448); // Blackrock Gauntlets — stat_armor=105, stat_strength=3
        let want_armor: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(105);
        // Optional 3rd arg: assert spell slot 1's id (the on-use spell that drives the green "Use:" text).
        let want_spell: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        // Optional 4th arg: assert the item-binding byte (work-item 127) — -1 skips the check.
        let want_bonding: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(-1);
        eprintln!("[wire] query-item {entry} expecting armor={want_armor} spell1={want_spell} bonding={want_bonding}");
        let (armor, block, sell_price, stats, spell1, trig1, bonding) = c.query_item(entry)?;
        println!(
            "[probe] SMSG_ITEM_QUERY_SINGLE_RESPONSE item={entry} armor={armor} block={block} sell_price={sell_price} spell1={spell1} trigger1={trig1} bonding={bonding} stats={stats:?}"
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
        if want_bonding >= 0 && i32::from(bonding) != want_bonding {
            eprintln!("[wire] FAIL: bonding={bonding}, want {want_bonding}");
            std::process::exit(1);
        }
        println!(
            "[wire] ITEM-QUERY PASS \u{2713}  armor={armor} block={block} sell_price={sell_price}c spell1={spell1} trigger1={trig1} bonding={bonding} nonzero_stats={nonzero_stats:?}"
        );
        return Ok(true);
    }

    // ---- played-time probe: work-item 029 — CMSG_PLAYED_TIME -> SMSG_PLAYED_TIME (/played) ----
    // Usage: wire-client TEST test123 <char-name> played-time
    if mode == "played-time" {
        eprintln!("[wire] played-time probe: CMSG_PLAYED_TIME…");
        let (total, level) = c.played_time_request()?;
        println!("[probe] SMSG_PLAYED_TIME total_played_time={total} level_played_time={level}");
        println!("[wire] PLAYED-TIME PASS \u{2713}  got a reply (total={total}s)");
        return Ok(true);
    }

    // ---- played-time-live probe: two CMSG_PLAYED_TIME queries with a sleep between, asserting the
    // second total is strictly greater — proves the live session's elapsed span is folded into the
    // reply in real time (not just accrued at logout). ----
    // Usage: wire-client TEST test123 <char-name> played-time-live [sleep_secs]
    if mode == "played-time-live" {
        let sleep_secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
        let (t1, _) = c.played_time_request()?;
        println!("[probe] first SMSG_PLAYED_TIME total_played_time={t1}");
        eprintln!("[wire] sleeping {sleep_secs}s…");
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
        let (t2, _) = c.played_time_request()?;
        println!("[probe] second SMSG_PLAYED_TIME total_played_time={t2}");
        if t2 > t1 {
            println!("[wire] PLAYED-TIME-LIVE PASS \u{2713}  {t1} -> {t2} (+{})", t2 - t1);
            return Ok(true);
        }
        bail!("PLAYED-TIME-LIVE FAIL: total did not increase across the sleep ({t1} -> {t2})");
    }

    // ---- levelup-info probe: decode SMSG_LEVELUP_INFO on a REAL XP-driven ding, asserting the popup
    // deltas (033). Signals /tmp/wc_levelup_ready, then the orchestrator grants kill-XP (through
    // grant_xp, e.g. debug_kill_nearest) until the character dings; we decode the resulting
    // SMSG_LEVELUP_INFO and print every field. For a mana class the mana delta must be non-zero and at
    // least one stat delta non-zero (the pre-033 gateway hardcoded all of them 0). ----
    // Usage: wire-client TEST test123 <char-name> levelup-info [expect_mana: 0|1 (default 1)]
    if mode == "levelup-info" {
        let expect_mana: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        eprintln!("[levelup] in-world as guid {:#x}; signalling orchestrator…", c.self_guid);
        std::fs::write("/tmp/wc_levelup_ready", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_LEVELUP_INFO(m)) => {
                    println!(
                        "[probe] SMSG_LEVELUP_INFO new_level={} health={} mana={} str={} agi={} sta={} int={} spi={}",
                        m.new_level.as_int(), m.health, m.mana,
                        m.strength, m.agility, m.stamina, m.intellect, m.spirit
                    );
                    let mana_ok = if expect_mana == 1 { m.mana > 0 } else { m.mana == 0 };
                    let any_stat = m.intellect > 0 || m.spirit > 0 || m.strength > 0 || m.stamina > 0 || m.agility > 0;
                    if mana_ok && any_stat {
                        println!("[wire] LEVELUP-INFO PASS \u{2713}  non-zero stat deltas decoded (mana={})", m.mana);
                        return Ok(true);
                    }
                    bail!(
                        "LEVELUP-INFO FAIL: deltas all zero or mana mismatch (mana={} expect_mana={expect_mana})",
                        m.mana
                    );
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        bail!("LEVELUP-INFO FAIL: no SMSG_LEVELUP_INFO within 60s of signalling ready");
    }

    // ---- questgiver probe: the REAL protocol a questgiver-only NPC (npc_flags=2, no GOSSIP) uses ----
    if mode == "questgiver" {
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
        return Ok(true);
    }

    // ---- gossip probe: dump the SMSG the gateway sends for CMSG_GOSSIP_HELLO to an NPC ----
    if mode == "gossip" {
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
        return Ok(true);
    }

    // ---- ghost-reveal probe (PIECE 2): a viewer who dies + becomes a GHOST should get the spirit-healer
    // entity CREATE'd (the GW_AOI=0 on_update reveal). The orchestrator kills+repops via debug reducers
    // (the char's small guid). PASS = healer hidden while alive, then revealed on the ghost transition.
    if mode == "ghost" {
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
            return Ok(true);
        }
        bail!("ghost-reveal: before={before} after={after} (want false->true)");
    }

    // ---- stay probe: login + stay connected until a sentinel file appears, then exit Ok.
    // Usage: wire-client [account] [password] [char-name] stay [sentinel_file]
    // The external orchestrator writes anything to sentinel_file to signal done; we exit 0.
    // Useful when the test only needs the character to be live in game_world_entity while an
    // external script calls spacetime reducers (e.g. work-item #092 combat-regen probe).
    if mode == "stay" {
        let sentinel: String = args.next().unwrap_or_else(|| "/tmp/wc_stay_done".into());
        let _ = std::fs::remove_file(&sentinel);
        eprintln!("[wire] stay: draining socket until {sentinel} appears…");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let _ = c.recv(); // keep gateway connection alive
            if std::path::Path::new(&sentinel).exists() {
                let _ = std::fs::remove_file(&sentinel);
                println!("[wire] STAY DONE — sentinel received, exiting.");
                return Ok(true);
            }
        }
        println!("[wire] STAY TIMEOUT — orchestrator did not signal within 60s.");
        return Ok(true);
    }

    // ---- logout probe: assert out-of-combat logout replies Success + LOGOUT_COMPLETE ----
    // Usage: wire-client [account] [password] [char-name] logout
    // Pass: SMSG_LOGOUT_RESPONSE(Success, Instant) → SMSG_LOGOUT_COMPLETE
    // Fail: FailureInCombat or timeout.
    if mode == "logout" {
        eprintln!("[wire] logout probe — sending CMSG_LOGOUT_REQUEST…");
        let result = c.logout_request()?;
        match result {
            LogoutResult::Success => {
                println!("[wire] LOGOUT PASS \u{2713}  SMSG_LOGOUT_RESPONSE(Success, Instant) + SMSG_LOGOUT_COMPLETE");
                return Ok(true);
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
    if mode == "ding" {
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
        return Ok(true);
    }

    // ---- repop-delay probe: CMSG_REPOP_REQUEST → assert SMSG_CORPSE_RECLAIM_DELAY(30s) ----
    // Usage: wire-client [account] [password] [char-name] repop [char-small-guid]
    // The orchestrator must kill the character (debug_set_health 0) before signalling;
    // then we send CMSG_REPOP_REQUEST and assert the gateway emits the 30s delay packet.
    // Pass: SMSG_CORPSE_RECLAIM_DELAY with delay == Duration::from_secs(30).
    if mode == "repop" {
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
                return Ok(true);
            }
            Some(d) => anyhow::bail!("repop-delay: got delay={:?}, want {:?}", d, want),
            None => anyhow::bail!("repop-delay: no SMSG_CORPSE_RECLAIM_DELAY received within 5s"),
        }
    }

    if mode == "cast-dump" {
        let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
        c.cast_spell(spell, c.self_guid)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            match c.recv_raw() {
                Ok((op, payload)) => println!("[dump] opcode 0x{op:04X} len={}", payload.len()),
                Err(e) => { println!("[dump] recv_raw err: {e}"); break; }
            }
        }
        return Ok(true);
    }

    // ---- raw-audit <seconds>: diagnostic — every SMSG_UPDATE_OBJECT frame is decode-attempted;
    // reports created guids vs undecodable frames (gtker gaps surface here). ----
    if mode == "raw-audit" {
        
        let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
        println!("[audit] initial seen_guids: {:x?}", c.seen_guids);
        c.set_recv_timeout(std::time::Duration::from_millis(500))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            match c.recv_raw() {
                Ok((op, payload)) => {
                    if op == 0x00A9 {
                        println!("[audit] UPDATE_OBJECT len={}", payload.len());
                    }
                }
                Err(_) => {}
            }
        }
        return Ok(true);
    }

    // ---- name-query <guid> <want_name>: CMSG_NAME_QUERY -> SMSG_NAME_QUERY_RESPONSE(name) ----
    // (work-item 142: proves a session-less playerbot's name resolves like any player's.)
    if mode == "name-query" {
        use wow_world_messages::vanilla::CMSG_NAME_QUERY;
        let guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("guid");
        let want: String = args.next().expect("want name");
        c.send(&CMSG_NAME_QUERY { guid: Guid::new(guid) })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_NAME_QUERY_RESPONSE(r)) => {
                    if r.guid.guid() != guid { continue; }
                    println!("[probe] SMSG_NAME_QUERY_RESPONSE guid={guid:#x} name={:?}", r.character_name);
                    if r.character_name == want {
                        println!("[wire] NAME-QUERY PASS \u{2713}  {guid:#x} resolves to {want:?}");
                        return Ok(true);
                    }
                    bail!("name-query: guid {guid:#x} resolved to {:?}, want {want:?}", r.character_name);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        bail!("name-query: no SMSG_NAME_QUERY_RESPONSE for {guid:#x} within 5s");
    }

    // ---- bindpoint probe: assert SMSG_BINDPOINTUPDATE carries home_x/y/z (not login position) ----
    // Usage: wire-client [account] [password] [char-name] bindpoint <home_x> <home_y> <home_z>
    // The char must have home_* != login position (e.g. "Tester": login=(-8935, -188) home=(-8873, -134)).
    // Pass: SMSG_BINDPOINTUPDATE.position.x == home_x (within 0.1 tolerance).
    if mode == "bindpoint" {
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
                return Ok(true);
            }
        }
    }

    Ok(false)
}

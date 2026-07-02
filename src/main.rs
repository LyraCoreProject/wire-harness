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
use wow_world_messages::vanilla::{Class, LogoutResult, WorldResult, SMSG_MESSAGECHAT};
use wow_world_messages::vanilla::{CMSG_TEXT_EMOTE, TextEmote};
use wow_world_messages::vanilla::{
    Friend_FriendStatus, CMSG_ADD_FRIEND, CMSG_ADD_IGNORE, CMSG_FRIEND_LIST,
};
use wow_world_base::shared::friend_result_vanilla_tbc::FriendResult;
use wow_world_messages::Guid;

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

    // ---- char-delete probe: CMSG_CHAR_DELETE -> SMSG_CHAR_DELETE(success), row gone (081) ----
    // Usage: wire-client <account> <password> <char-name-to-create-then-delete> char-delete
    // Char-select tier only (no player_login/world-entry). Creates (or finds) a throwaway
    // character named `char_name`, deletes it, and asserts SMSG_CHAR_DELETE(CharDeleteSuccess)
    // AND that a follow-up CMSG_CHAR_ENUM no longer lists that guid. The gateway's own DB-row
    // check (game_character gone + no owned item/quest/spell rows) is verified separately via
    // `spacetime sql`.
    if mode.as_deref() == Some("char-delete") {
        eprintln!("[wire] char-delete probe: logon + char-select only (no world entry)…");
        let (k, world_addr) = logon(&account, &password)?;
        let mut c2 = WireClient::connect_world(&world_addr, &account, k)?;
        let guid = c2.create_or_find_char(&char_name, Class::Warrior)?;
        eprintln!("[wire] deleting {char_name} (guid={guid})…");
        let result = c2.char_delete(guid)?;
        println!("[probe] SMSG_CHAR_DELETE result={result:?}");
        if result != WorldResult::CharDeleteSuccess {
            bail!("char-delete: SMSG_CHAR_DELETE result={result:?}, want CharDeleteSuccess");
        }
        let remaining = c2.char_enum()?;
        if remaining.iter().any(|(g, _, _)| *g == guid) {
            bail!("char-delete: guid={guid} still present in CMSG_CHAR_ENUM after delete");
        }
        println!(
            "[wire] CHAR-DELETE PASS \u{2713}  SMSG_CHAR_DELETE(CharDeleteSuccess); guid={guid} no longer in SMSG_CHAR_ENUM"
        );
        return Ok(());
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

    // ---- init-factions probe: assert SMSG_INITIALIZE_FACTIONS carries a persisted standing on RELOG (076) ----
    // Usage: wire-client TEST test123 <char-name> init-factions <reputation_index> <want_standing>
    // `c` is already logged in once above; this probe reconnects fresh (a real relog) and re-runs
    // player_login, so the SMSG_INITIALIZE_FACTIONS captured is the one built by the *second* login
    // burst — exactly what a relogging client sees. Asserts slot[index] == want_standing.
    if mode.as_deref() == Some("init-factions") {
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
        return Ok(());
    }

    // ---- item-query probe: assert SMSG_ITEM_QUERY_SINGLE_RESPONSE carries armor + stats ----
    // Usage: wire-client TEST test123 Ginger query-item <entry> [want_armor] [want_spell] [want_bonding]
    // Exits 0 if armor == want_armor (default 105 for Blackrock Gauntlets entry 1448).
    // want_bonding: 4th optional arg, -1 (default) = don't check (0=NoBind,1=BoP,2=BoE,3=BoU work-item 127).
    if mode.as_deref() == Some("query-item") {
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
        return Ok(());
    }

    // ---- played-time probe: work-item 029 — CMSG_PLAYED_TIME -> SMSG_PLAYED_TIME (/played) ----
    // Usage: wire-client TEST test123 <char-name> played-time
    if mode.as_deref() == Some("played-time") {
        eprintln!("[wire] played-time probe: CMSG_PLAYED_TIME…");
        let (total, level) = c.played_time_request()?;
        println!("[probe] SMSG_PLAYED_TIME total_played_time={total} level_played_time={level}");
        println!("[wire] PLAYED-TIME PASS \u{2713}  got a reply (total={total}s)");
        return Ok(());
    }

    // ---- played-time-live probe: two CMSG_PLAYED_TIME queries with a sleep between, asserting the
    // second total is strictly greater — proves the live session's elapsed span is folded into the
    // reply in real time (not just accrued at logout). ----
    // Usage: wire-client TEST test123 <char-name> played-time-live [sleep_secs]
    if mode.as_deref() == Some("played-time-live") {
        let sleep_secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
        let (t1, _) = c.played_time_request()?;
        println!("[probe] first SMSG_PLAYED_TIME total_played_time={t1}");
        eprintln!("[wire] sleeping {sleep_secs}s…");
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
        let (t2, _) = c.played_time_request()?;
        println!("[probe] second SMSG_PLAYED_TIME total_played_time={t2}");
        if t2 > t1 {
            println!("[wire] PLAYED-TIME-LIVE PASS \u{2713}  {t1} -> {t2} (+{})", t2 - t1);
            return Ok(());
        }
        bail!("PLAYED-TIME-LIVE FAIL: total did not increase across the sleep ({t1} -> {t2})");
    }

    // ---- levelup-info probe: decode SMSG_LEVELUP_INFO on a REAL XP-driven ding, asserting the popup
    // deltas (033). Signals /tmp/wc_levelup_ready, then the orchestrator grants kill-XP (through
    // grant_xp, e.g. debug_kill_nearest) until the character dings; we decode the resulting
    // SMSG_LEVELUP_INFO and print every field. For a mana class the mana delta must be non-zero and at
    // least one stat delta non-zero (the pre-033 gateway hardcoded all of them 0). ----
    // Usage: wire-client TEST test123 <char-name> levelup-info [expect_mana: 0|1 (default 1)]
    if mode.as_deref() == Some("levelup-info") {
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
                        return Ok(());
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

    // ---- friend probe (work-item 130): CMSG_ADD_FRIEND by name -> SMSG_FRIEND_STATUS, then
    // CMSG_FRIEND_LIST -> SMSG_FRIEND_LIST + SMSG_IGNORE_LIST. ----
    // Usage: wire-client [account] [password] [char-name] friend <target-name>
    // Pass: SMSG_FRIEND_STATUS is Added(Online|Offline) carrying the target's guid, then a fresh
    // SMSG_FRIEND_LIST lists that guid as Online (this client and the target are both connected).
    if mode.as_deref() == Some("friend") {
        let target_name = args.next().unwrap_or_else(|| "dfsdfsd".into());
        eprintln!("[friend] sending CMSG_ADD_FRIEND({target_name:?})…");
        c.send(&CMSG_ADD_FRIEND { name: target_name.clone() })?;
        // Background AOI/relay traffic (nearby SMSG_UPDATE_OBJECT etc.) can interleave — loop past it.
        let target_guid = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut found = None;
            while std::time::Instant::now() < deadline && found.is_none() {
                match c.recv()? {
                    Smsg::SMSG_FRIEND_STATUS(s) => {
                        eprintln!("[friend] SMSG_FRIEND_STATUS result={:?} guid={:#x}", s.result, s.guid.guid());
                        // `Already` is a pass too — a re-run of this probe against an already-added
                        // friend still proves the name resolved to the right guid server-side.
                        if !matches!(s.result, FriendResult::AddedOnline | FriendResult::AddedOffline | FriendResult::Already) {
                            bail!("friend: add_friend({target_name:?}) returned {:?}, want Added(Online|Offline)/Already", s.result);
                        }
                        found = Some(s.guid.guid());
                    }
                    _ => {}
                }
            }
            found.ok_or_else(|| anyhow::anyhow!("friend: timed out waiting for SMSG_FRIEND_STATUS"))?
        };

        eprintln!("[friend] sending CMSG_FRIEND_LIST…");
        c.send(&CMSG_FRIEND_LIST {})?;
        let mut got_list = false;
        let mut got_ignore = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !(got_list && got_ignore) {
            match c.recv()? {
                Smsg::SMSG_FRIEND_LIST(l) => {
                    got_list = true;
                    let row = l.friends.iter().find(|f| f.guid.guid() == target_guid);
                    match row {
                        Some(f) => eprintln!("[friend] SMSG_FRIEND_LIST: guid={target_guid:#x} status={}", f.status),
                        None => bail!("friend: SMSG_FRIEND_LIST doesn't carry guid {target_guid:#x} (rows: {})", l.friends.len()),
                    }
                    let is_online = matches!(row.unwrap().status, Friend_FriendStatus::Online { .. });
                    if !is_online {
                        bail!("friend: SMSG_FRIEND_LIST carries guid {target_guid:#x} as {:?}, want Online (both clients are connected)", row.unwrap().status);
                    }
                }
                Smsg::SMSG_IGNORE_LIST(_) => got_ignore = true,
                _ => {}
            }
        }
        if !got_list {
            bail!("friend: timed out waiting for SMSG_FRIEND_LIST");
        }
        println!("[wire] FRIEND PASS \u{2713}  add_friend({target_name:?}) -> guid {target_guid:#x}; SMSG_FRIEND_LIST carries them Online");
        return Ok(());
    }

    // ---- ignore-whisper probe (work-item 130): the IGNORER (this client) adds the SPEAKER to their
    // ignore list, then the speaker whispers a unique probe line — assert it NEVER arrives. ----
    // Usage: wire-client [account] [password] [char-name] ignore-whisper <speaker-account> <speaker-password> <speaker-char>
    // Pass: SMSG_FRIEND_STATUS(IgnoreAdded) for the speaker's guid, then no SMSG_MESSAGECHAT Whisper
    // carrying the probe text reaches this (ignoring) client within the wait window.
    if mode.as_deref() == Some("ignore-whisper") {
        let speaker_account = args.next().unwrap_or_else(|| "TEST2".into());
        let speaker_password = args.next().unwrap_or_else(|| "test123".into());
        let speaker_char = args.next().unwrap_or_else(|| "dfsdfsd".into());

        eprintln!("[ignore-whisper] ignorer={char_name} sending CMSG_ADD_IGNORE({speaker_char:?})…");
        c.send(&CMSG_ADD_IGNORE { name: speaker_char.clone() })?;
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut got = false;
            while std::time::Instant::now() < deadline && !got {
                match c.recv()? {
                    Smsg::SMSG_FRIEND_STATUS(s) => {
                        eprintln!("[ignore-whisper] SMSG_FRIEND_STATUS result={:?} guid={:#x}", s.result, s.guid.guid());
                        if s.result != FriendResult::IgnoreAdded {
                            bail!("ignore-whisper: add_ignore({speaker_char:?}) returned {:?}, want IgnoreAdded", s.result);
                        }
                        got = true;
                    }
                    _ => {}
                }
            }
            if !got {
                bail!("ignore-whisper: timed out waiting for SMSG_FRIEND_STATUS");
            }
        }

        // Drain anything already buffered before the speaker connects/whispers.
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < drain_deadline {
            let _ = c.recv_raw();
        }

        eprintln!("[ignore-whisper] connecting speaker as {speaker_account}/{speaker_char}…");
        let mut sc = WireClient::login_as(&speaker_account, &speaker_password, &speaker_char, Class::Warrior)?;
        let probe = format!("ignore-probe-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
        eprintln!("[ignore-whisper] speaker whispers {char_name:?}: {probe:?}");
        sc.send(&wow_world_messages::vanilla::CMSG_MESSAGECHAT {
            chat_type: wow_world_messages::vanilla::CMSG_MESSAGECHAT_ChatType::Whisper { target_player: char_name.clone() },
            language: wow_world_messages::vanilla::Language::Universal,
            message: probe.clone(),
        })?;

        // The speaker still gets their own "To <ignorer>: ..." echo — drain it, not asserted here.
        let echo_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < echo_deadline {
            match sc.recv() {
                Ok(Smsg::SMSG_MESSAGECHAT(m)) => {
                    eprintln!("[ignore-whisper] speaker echo: {:?}", extract_chat_text(&m));
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        // Assert: the ignorer never receives the probe whisper.
        let ignorer_heard = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut found = false;
            while std::time::Instant::now() < deadline {
                match c.recv() {
                    Ok(Smsg::SMSG_MESSAGECHAT(m)) => {
                        let msg_text = extract_chat_text(&m);
                        eprintln!("[ignore-whisper] ignorer got SMSG_MESSAGECHAT: {msg_text:?}");
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
        if ignorer_heard {
            bail!("ignore-whisper: FAIL — ignorer received the whisper despite ignoring the sender");
        }
        println!("[wire] IGNORE-WHISPER PASS \u{2713}  {speaker_char} added to ignore; whisper from them never arrived");
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

    // ---- text-emote probe: verify a TARGETED emote resolves the target's name in SMSG_TEXT_EMOTE ----
    // Usage: wire-client [account] [password] [char-name] text-emote
    // Self-targets (target = own guid) so a single connection exercises the full pipeline: CMSG's
    // target guid -> send_emote reducer -> game_emote_event.target_guid -> gateway resolves via
    // game_character -> SMSG_TEXT_EMOTE.name. Pass: SMSG_TEXT_EMOTE.name == char_name (non-empty).
    if mode.as_deref() == Some("text-emote") {
        eprintln!("[text-emote] sending CMSG_TEXT_EMOTE(wave, target=self {:#x})…", c.self_guid);
        c.send(&CMSG_TEXT_EMOTE {
            text_emote: TextEmote::Wave,
            emote: 0,
            target: Guid::new(c.self_guid),
        })?;
        // SMSG_TEXT_EMOTE opcode: 0x0105
        const TEXT_EMOTE_OPCODE: u16 = 0x0105;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got: Option<String> = None;
        while std::time::Instant::now() < deadline {
            match c.recv_raw() {
                Ok((opcode, payload)) => {
                    if opcode == TEXT_EMOTE_OPCODE {
                        // guid(8) + text_emote(4) + emote(4) + SizedCString name (u32 len + bytes, no NUL)
                        if payload.len() >= 20 {
                            let len = u32::from_le_bytes(payload[16..20].try_into().unwrap()) as usize;
                            let name = String::from_utf8_lossy(&payload[20..20 + len.min(payload.len().saturating_sub(20))])
                                .trim_end_matches('\0')
                                .to_string();
                            eprintln!("[text-emote] SMSG_TEXT_EMOTE name={name:?}");
                            got = Some(name);
                        }
                        break;
                    }
                }
                Err(e) => { eprintln!("[text-emote] recv error: {e}"); break; }
            }
        }
        match got {
            None => bail!("text-emote: no SMSG_TEXT_EMOTE (opcode 0x{TEXT_EMOTE_OPCODE:04X}) within 5s"),
            Some(name) => {
                if name != char_name {
                    bail!("text-emote: SMSG_TEXT_EMOTE.name={name:?}, want {char_name:?}");
                }
                println!("[wire] TEXT-EMOTE PASS \u{2713}  SMSG_TEXT_EMOTE.name={name:?} matches the targeted character");
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

    // ---- inspect probe: CMSG_INSPECT -> SMSG_INSPECT(target guid) gated on range (work-item 137) ----
    // Usage: wire-client [account] [password] [char-name] inspect <near_guid> <far_guid>
    // `near_guid` must be an in-world player guid within 10yd of `char_name` (a real target replies
    // SMSG_INSPECT carrying that guid); `far_guid` must be an in-world player guid beyond 10yd (the
    // module's range gate rejects it, so the gateway sends nothing back). Guids come from
    // `spacetime sql … "select guid, x, y, z from game_character"` — this probe only drives the wire.
    if mode.as_deref() == Some("inspect") {
        let near_guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: … inspect <near_guid> <far_guid>");
        let far_guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: … inspect <near_guid> <far_guid>");

        eprintln!("[inspect] sending CMSG_INSPECT for in-range guid={near_guid}…");
        c.send(&wow_world_messages::vanilla::CMSG_INSPECT { guid: Guid::new(near_guid) })?;
        let near_ok = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut got = None;
            while std::time::Instant::now() < deadline && got.is_none() {
                match c.recv() {
                    Ok(Smsg::SMSG_INSPECT(r)) => got = Some(r.guid.guid()),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            got
        };
        if near_ok != Some(near_guid) {
            bail!("inspect: FAIL — expected SMSG_INSPECT(guid={near_guid}) for the in-range target, got {near_ok:?}");
        }
        eprintln!("[inspect] in-range target: OK (SMSG_INSPECT guid={near_guid})");

        eprintln!("[inspect] sending CMSG_INSPECT for out-of-range guid={far_guid}…");
        c.send(&wow_world_messages::vanilla::CMSG_INSPECT { guid: Guid::new(far_guid) })?;
        let far_reply = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut got = None;
            while std::time::Instant::now() < deadline && got.is_none() {
                match c.recv() {
                    Ok(Smsg::SMSG_INSPECT(r)) => got = Some(r.guid.guid()),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            got
        };
        if far_reply.is_some() {
            bail!("inspect: FAIL — got SMSG_INSPECT({far_reply:?}) for an out-of-range target (range gate not working)");
        }
        eprintln!("[inspect] out-of-range target correctly got no SMSG_INSPECT");
        println!("[wire] INSPECT PASS \u{2713}  in-range={near_guid} acked; out-of-range={far_guid} silenced");
        return Ok(());
    }

    // ===============================================================================
    //  Scenario runner (work-item 140) — multi-step gameplay flows over the wire.
    //  Each step prints "[scenario] STEP <n> OK …" and bails naming the failed step, so a
    //  mid-scenario breakage fails at exactly the right seam. Server-state assertions
    //  (money/XP/rep/rows) live in the orchestrating test-scenario-*.sh via spacetime sql.
    // ===============================================================================

    // ---- scenario-quest: accept -> kill 2 objective wolves (real swings) -> loot -> turn in ----
    // Usage: wire-client TEST test123 Ginger scenario-quest <giver_guid> <quest_entry> <wolf1> <wolf2>
    if mode.as_deref() == Some("scenario-quest") {
        use wow_world_messages::vanilla::{
            CMSG_ATTACKSWING, CMSG_LOOT, CMSG_LOOT_MONEY, CMSG_LOOT_RELEASE,
            CMSG_QUESTGIVER_ACCEPT_QUEST, CMSG_QUESTGIVER_CHOOSE_REWARD,
            CMSG_QUESTGIVER_COMPLETE_QUEST,
        };
        let giver: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: … scenario-quest <giver> <quest> <wolf1> <wolf2>");
        let quest: u32 = args.next().and_then(|s| s.parse().ok()).expect("quest entry");
        let wolf1: u64 = args.next().and_then(|s| s.parse().ok()).expect("wolf1 guid");
        let wolf2: u64 = args.next().and_then(|s| s.parse().ok()).expect("wolf2 guid");

        // STEP 1: hello at the giver — the single fixture quest opens QUEST_DETAILS directly.
        c.questgiver_hello(giver)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got_details = false;
        while std::time::Instant::now() < deadline && !got_details {
            match c.recv() {
                Ok(Smsg::SMSG_QUESTGIVER_QUEST_DETAILS(d)) => {
                    if d.quest_id != quest { bail!("STEP 1 FAIL: QUEST_DETAILS quest_id={} want {quest}", d.quest_id); }
                    got_details = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !got_details { bail!("STEP 1 FAIL: no SMSG_QUESTGIVER_QUEST_DETAILS({quest}) within 5s of hello"); }
        println!("[scenario] STEP 1 OK — SMSG_QUESTGIVER_QUEST_DETAILS({quest})");

        // STEP 2: accept. (No dedicated ack SMSG — the orchestrator sql-asserts the log row.)
        c.send(&CMSG_QUESTGIVER_ACCEPT_QUEST { guid: Guid::new(giver), quest_id: quest })?;
        println!("[scenario] STEP 2 OK — CMSG_QUESTGIVER_ACCEPT_QUEST sent");

        // STEP 3+4: melee both wolves down, asserting the two SMSG_QUESTUPDATE_ADD_KILL ticks.
        for (i, wolf) in [(1u32, wolf1), (2u32, wolf2)] {
            c.set_selection(wolf)?;
            c.send(&CMSG_ATTACKSWING { guid: Guid::new(wolf) })?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
            let mut got_kill = false;
            while std::time::Instant::now() < deadline && !got_kill {
                match c.recv() {
                    Ok(Smsg::SMSG_QUESTUPDATE_ADD_KILL(k)) => {
                        if k.quest_id != quest { continue; }
                        if k.kill_count != i { bail!("STEP {} FAIL: ADD_KILL count={} want {i}", 2 + i, k.kill_count); }
                        got_kill = true;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if !got_kill { bail!("STEP {} FAIL: no SMSG_QUESTUPDATE_ADD_KILL({i}/2) within 90s", 2 + i); }
            println!("[scenario] STEP {} OK — SMSG_QUESTUPDATE_ADD_KILL {i}/2", 2 + i);
        }

        // STEP 5: loot the second wolf's corpse — money window + release.
        c.send(&CMSG_LOOT { guid: Guid::new(wolf2) })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut gold: Option<u32> = None;
        while std::time::Instant::now() < deadline && gold.is_none() {
            match c.recv() {
                Ok(Smsg::SMSG_LOOT_RESPONSE(l)) => gold = Some(l.gold.as_int()),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let Some(gold) = gold else { bail!("STEP 5 FAIL: no SMSG_LOOT_RESPONSE within 5s of CMSG_LOOT") };
        if gold == 0 { bail!("STEP 5 FAIL: SMSG_LOOT_RESPONSE.gold == 0 (fixture wolves carry money)"); }
        c.send(&CMSG_LOOT_MONEY {})?;
        c.send(&CMSG_LOOT_RELEASE { guid: Guid::new(wolf2) })?;
        println!("[scenario] STEP 5 OK — looted corpse (gold={gold}) + released");

        // STEP 6: complete -> the giver offers the reward.
        c.send(&CMSG_QUESTGIVER_COMPLETE_QUEST { guid: Guid::new(giver), quest_id: quest })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got_offer = false;
        while std::time::Instant::now() < deadline && !got_offer {
            match c.recv() {
                Ok(Smsg::SMSG_QUESTGIVER_OFFER_REWARD(o)) => {
                    if o.quest_id != quest { bail!("STEP 6 FAIL: OFFER_REWARD quest_id={} want {quest}", o.quest_id); }
                    got_offer = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !got_offer { bail!("STEP 6 FAIL: no SMSG_QUESTGIVER_OFFER_REWARD within 5s of COMPLETE_QUEST"); }
        println!("[scenario] STEP 6 OK — SMSG_QUESTGIVER_OFFER_REWARD({quest})");

        // STEP 7: choose reward 0 -> QUEST_COMPLETE closes the loop.
        c.send(&CMSG_QUESTGIVER_CHOOSE_REWARD { guid: Guid::new(giver), quest_id: quest, reward: 0 })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut done = false;
        while std::time::Instant::now() < deadline && !done {
            match c.recv() {
                Ok(Smsg::SMSG_QUESTGIVER_QUEST_COMPLETE(q)) => {
                    if q.quest_id != quest { bail!("STEP 7 FAIL: QUEST_COMPLETE quest_id={} want {quest}", q.quest_id); }
                    done = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !done { bail!("STEP 7 FAIL: no SMSG_QUESTGIVER_QUEST_COMPLETE within 5s of CHOOSE_REWARD"); }
        println!("[scenario] STEP 7 OK — SMSG_QUESTGIVER_QUEST_COMPLETE({quest})");
        println!("[wire] SCENARIO-QUEST PASS \u{2713}  details->accept->2 kills->loot->offer->complete");
        return Ok(());
    }

    // ---- vendor micro-modes: single wire actions the vendor orchestrator sequences with sql ----
    // vendor-list <vendor> <want_entry> — SMSG_LIST_INVENTORY must include want_entry.
    if mode.as_deref() == Some("vendor-list") {
        use wow_world_messages::vanilla::CMSG_LIST_INVENTORY;
        let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
        let want: u32 = args.next().and_then(|s| s.parse().ok()).expect("want entry");
        c.send(&CMSG_LIST_INVENTORY { guid: Guid::new(vendor) })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_LIST_INVENTORY(l)) => {
                    let entries: Vec<u32> = l.items.iter().map(|i| i.item).collect();
                    println!("[probe] SMSG_LIST_INVENTORY items={entries:?}");
                    if entries.contains(&want) {
                        println!("[wire] VENDOR-LIST PASS \u{2713}  vendor stocks {want}");
                        return Ok(());
                    }
                    bail!("vendor-list: {want} not in vendor inventory {entries:?}");
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        bail!("vendor-list: no SMSG_LIST_INVENTORY within 5s");
    }
    // vendor-buy <vendor> <entry> — passes unless SMSG_BUY_FAILED arrives (state asserted via sql).
    if mode.as_deref() == Some("vendor-buy") {
        use wow_world_messages::vanilla::CMSG_BUY_ITEM;
        let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
        let entry: u32 = args.next().and_then(|s| s.parse().ok()).expect("item entry");
        c.send(&CMSG_BUY_ITEM { vendor: Guid::new(vendor), item: entry, amount: 1, unknown1: 0 })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_BUY_FAILED(f)) => bail!("vendor-buy: SMSG_BUY_FAILED result={:?}", f.result),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!("[wire] VENDOR-BUY PASS \u{2713}  CMSG_BUY_ITEM({entry}) accepted (no SMSG_BUY_FAILED)");
        return Ok(());
    }
    // equip-from <backpack_slot> / unequip-from <equip_slot> — fail on SMSG_INVENTORY_CHANGE_FAILURE.
    if matches!(mode.as_deref(), Some("equip-from") | Some("unequip-from")) {
        use wow_world_messages::vanilla::{CMSG_AUTOEQUIP_ITEM, CMSG_AUTOSTORE_BAG_ITEM};
        let slot: u8 = args.next().and_then(|s| s.parse().ok()).expect("slot");
        let equipping = mode.as_deref() == Some("equip-from");
        if equipping {
            c.send(&CMSG_AUTOEQUIP_ITEM { source_bag: 255, source_slot: slot })?;
        } else {
            c.send(&CMSG_AUTOSTORE_BAG_ITEM { source_bag: 255, source_slot: slot, destination_bag: 255 })?;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_INVENTORY_CHANGE_FAILURE(f)) => bail!("{}: SMSG_INVENTORY_CHANGE_FAILURE {:?}", mode.as_deref().unwrap(), f),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!("[wire] {} PASS \u{2713}  slot {slot} accepted (no INVENTORY_CHANGE_FAILURE)", if equipping { "EQUIP-FROM" } else { "UNEQUIP-FROM" });
        return Ok(());
    }
    // vendor-sell <vendor> <item_guid> / vendor-repair <vendor> <item_guid> / vendor-buyback <vendor> <packet_slot>
    if matches!(mode.as_deref(), Some("vendor-sell") | Some("vendor-repair") | Some("vendor-buyback")) {
        use wow_world_messages::vanilla::{BuybackSlot, CMSG_BUYBACK_ITEM, CMSG_REPAIR_ITEM, CMSG_SELL_ITEM};
        let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
        let arg: u64 = args.next().and_then(|s| s.parse().ok()).expect("item guid / packet slot");
        match mode.as_deref().unwrap() {
            "vendor-sell" => c.send(&CMSG_SELL_ITEM { vendor: Guid::new(vendor), item: Guid::new(arg), amount: 1 })?,
            "vendor-repair" => c.send(&CMSG_REPAIR_ITEM { npc: Guid::new(vendor), item: Guid::new(arg) })?,
            _ => c.send(&CMSG_BUYBACK_ITEM { guid: Guid::new(vendor), slot: BuybackSlot::try_from(arg as u32).map_err(|_| anyhow::anyhow!("bad buyback slot {arg}"))? })?,
        }
        // These actions have no success SMSG in this gateway (errors are log+ignore server-side);
        // drain briefly so the send flushes, then let the orchestrator's sql assertion be the judge.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline { let _ = c.recv_raw(); }
        println!("[wire] {} SENT \u{2713}  (state asserted via sql by the orchestrator)", mode.as_deref().unwrap().to_uppercase());
        return Ok(());
    }

    // ---- vendor-sell-buyback <vendor> <item_guid>: sell + buyback in ONE session (the module
    // clears the buyback ring on logout — vanilla — so the two must share a connection). The
    // orchestrator sql-asserts at the two handshake files while the session is held open. ----
    if mode.as_deref() == Some("vendor-sell-buyback") {
        use wow_world_messages::vanilla::{BuybackSlot, CMSG_BUYBACK_ITEM, CMSG_SELL_ITEM};
        let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
        let item: u64 = args.next().and_then(|s| s.parse().ok()).expect("item guid");
        c.send(&CMSG_SELL_ITEM { vendor: Guid::new(vendor), item: Guid::new(item), amount: 1 })?;
        std::fs::write("/tmp/ws_vendor_sold", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && std::path::Path::new("/tmp/ws_vendor_sold").exists() {
            let _ = c.recv_raw();
        }
        if std::path::Path::new("/tmp/ws_vendor_sold").exists() { bail!("sell: orchestrator never confirmed the sell-state asserts"); }
        println!("[scenario] SELL OK (orchestrator confirmed money + buyback ring)");
        c.send(&CMSG_BUYBACK_ITEM { guid: Guid::new(vendor), slot: BuybackSlot::try_from(69u32).unwrap() })?;
        std::fs::write("/tmp/ws_vendor_bought", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && std::path::Path::new("/tmp/ws_vendor_bought").exists() {
            let _ = c.recv_raw();
        }
        if std::path::Path::new("/tmp/ws_vendor_bought").exists() { bail!("buyback: orchestrator never confirmed the buyback-state asserts"); }
        println!("[wire] VENDOR-SELL-BUYBACK PASS \u{2713}  sell + buyback round-trip in one session");
        return Ok(());
    }

    // ---- scenario-train: trainer list -> buy spell -> cast it, asserting the full sequence ----
    // Usage: wire-client TEST test123 Ginger scenario-train <trainer_guid> <spell_id> <cast_ms>
    if mode.as_deref() == Some("scenario-train") {
        use wow_world_messages::vanilla::{CMSG_TRAINER_BUY_SPELL, CMSG_TRAINER_LIST};
        let trainer: u64 = args.next().and_then(|s| s.parse().ok()).expect("trainer guid");
        let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
        let cast_ms: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1500);

        // Handshake: the orchestrator damages the caster (so the heal is observable) once we're live.
        std::fs::write("/tmp/ws_train_ready", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && std::path::Path::new("/tmp/ws_train_ready").exists() {
            let _ = c.recv_raw(); // drain while the orchestrator works
        }
        if std::path::Path::new("/tmp/ws_train_ready").exists() { bail!("orchestrator never staged the caster (ready file not consumed)"); }

        // STEP 1: the trainer window lists the offering.
        c.send(&CMSG_TRAINER_LIST { guid: Guid::new(trainer) })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut listed = false;
        while std::time::Instant::now() < deadline && !listed {
            match c.recv() {
                Ok(Smsg::SMSG_TRAINER_LIST(t)) => {
                    let ids: Vec<u32> = t.spells.iter().map(|sp| sp.spell).collect();
                    println!("[probe] SMSG_TRAINER_LIST spells={ids:?}");
                    if !ids.contains(&spell) { bail!("STEP 1 FAIL: trainer list lacks {spell}"); }
                    listed = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !listed { bail!("STEP 1 FAIL: no SMSG_TRAINER_LIST within 5s"); }
        println!("[scenario] STEP 1 OK — SMSG_TRAINER_LIST carries {spell}");

        // STEP 2: buy -> BUY_SUCCEEDED + LEARNED_SPELL.
        c.send(&CMSG_TRAINER_BUY_SPELL { guid: Guid::new(trainer), id: spell })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (mut ok_buy, mut ok_learn) = (false, false);
        while std::time::Instant::now() < deadline && !(ok_buy && ok_learn) {
            match c.recv() {
                Ok(Smsg::SMSG_TRAINER_BUY_SUCCEEDED(b)) => { if b.id == spell { ok_buy = true; } }
                Ok(Smsg::SMSG_TRAINER_BUY_FAILED(f)) => bail!("STEP 2 FAIL: SMSG_TRAINER_BUY_FAILED {f:?}"),
                Ok(Smsg::SMSG_LEARNED_SPELL(l)) => { if l.id == spell { ok_learn = true; } }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !(ok_buy && ok_learn) { bail!("STEP 2 FAIL: buy={ok_buy} learned={ok_learn} (want both) within 5s"); }
        println!("[scenario] STEP 2 OK — SMSG_TRAINER_BUY_SUCCEEDED + SMSG_LEARNED_SPELL({spell})");

        // STEP 3: cast the bought spell at self -> START(cast_ms) then GO.
        c.cast_spell(spell, c.self_guid)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let (mut started, mut went) = (false, false);
        while std::time::Instant::now() < deadline && !(started && went) {
            match c.recv() {
                Ok(Smsg::SMSG_SPELL_START(sp)) => { if sp.timer == cast_ms { started = true; } }
                Ok(Smsg::SMSG_SPELL_GO(g)) => { if g.spell == spell { went = true; } }
                Ok(Smsg::SMSG_CAST_RESULT(r)) => bail!("STEP 3 FAIL: cast rejected: {r:?}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !(started && went) { bail!("STEP 3 FAIL: START(timer={cast_ms})={started} GO={went} (want both) within 8s"); }
        println!("[scenario] STEP 3 OK — SMSG_SPELL_START({cast_ms}) -> SMSG_SPELL_GO({spell})");
        println!("[wire] SCENARIO-TRAIN PASS \u{2713}  list->buy->learn->cast");
        return Ok(());
    }

    if mode.as_deref() == Some("cast-dump") {
        let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
        c.cast_spell(spell, c.self_guid)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            match c.recv_raw() {
                Ok((op, payload)) => println!("[dump] opcode 0x{op:04X} len={}", payload.len()),
                Err(e) => { println!("[dump] recv_raw err: {e}"); break; }
            }
        }
        return Ok(());
    }

    // ---- scenario-death: die (orchestrated) -> release -> wait the reclaim delay -> reclaim ----
    // Usage: wire-client TEST test123 Ginger scenario-death <corpse_guid>
    if mode.as_deref() == Some("scenario-death") {
        use wow_world_messages::vanilla::CMSG_RECLAIM_CORPSE;
        let corpse: u64 = args.next().and_then(|s| s.parse().ok()).expect("corpse guid");

        // STEP 1: signal ready; the orchestrator arranges a real death-by-mob, then removes the file.
        std::fs::write("/tmp/ws_death_ready", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline && std::path::Path::new("/tmp/ws_death_ready").exists() {
            let _ = c.recv_raw();
        }
        if std::path::Path::new("/tmp/ws_death_ready").exists() { bail!("STEP 1 FAIL: orchestrator never confirmed the death"); }
        println!("[scenario] STEP 1 OK — orchestrator confirmed death (server-side)");

        // STEP 2: release -> the 30s reclaim-delay packet.
        c.repop_request()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut delay: Option<std::time::Duration> = None;
        while std::time::Instant::now() < deadline && delay.is_none() {
            match c.recv() {
                Ok(Smsg::SMSG_CORPSE_RECLAIM_DELAY(d)) => delay = Some(d.delay),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let Some(delay) = delay else { bail!("STEP 2 FAIL: no SMSG_CORPSE_RECLAIM_DELAY within 5s of CMSG_REPOP_REQUEST") };
        if delay != std::time::Duration::from_secs(30) { bail!("STEP 2 FAIL: reclaim delay {delay:?}, want 30s"); }
        println!("[scenario] STEP 2 OK — SMSG_CORPSE_RECLAIM_DELAY(30s)");

        // STEP 3: wait out the delay (draining), then reclaim the corpse.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(31);
        while std::time::Instant::now() < until { let _ = c.recv_raw(); }
        c.send(&CMSG_RECLAIM_CORPSE { guid: Guid::new(corpse) })?;
        println!("[scenario] STEP 3 OK — CMSG_RECLAIM_CORPSE sent after the 30s window");

        // STEP 4: hold the session while the orchestrator sql-asserts the resurrected state.
        std::fs::write("/tmp/ws_death_reclaimed", "1").ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline && std::path::Path::new("/tmp/ws_death_reclaimed").exists() {
            let _ = c.recv_raw();
        }
        if std::path::Path::new("/tmp/ws_death_reclaimed").exists() { bail!("STEP 4 FAIL: orchestrator never confirmed the resurrect"); }
        println!("[scenario] STEP 4 OK — orchestrator confirmed alive-at-50% state");
        println!("[wire] SCENARIO-DEATH PASS \u{2713}  death->release->30s delay->reclaim");
        return Ok(());
    }

    // ===============================================================================
    //  Multi-client AOI/relay regression + soak (work-item 141)
    // ===============================================================================

    // ---- aoi-observer <peer_guid> <cmd_file> <ack_file>: command-driven event assertions ----
    // The orchestrator writes a command into cmd_file ("expect-create" / "expect-move" /
    // "expect-destroy" / "done"); the observer satisfies it against the live packet stream within
    // 30s and answers "OK <cmd>" or "FAIL <cmd>" in ack_file. Login-time precondition: the peer
    // must NOT be visible (it starts outside the 125yd AOI box).
    if mode.as_deref() == Some("aoi-observer") {
        let peer: u64 = args.next().and_then(|s| s.parse().ok()).expect("peer guid");
        let cmd_file: String = args.next().unwrap_or_else(|| "/tmp/ws_aoi_cmd".into());
        let ack_file: String = args.next().unwrap_or_else(|| "/tmp/ws_aoi_ack".into());
        let _ = std::fs::remove_file(&cmd_file);
        let _ = std::fs::remove_file(&ack_file);
        if c.seen_guids.contains(&peer) {
            bail!("aoi-observer: peer {peer:#x} already visible at login — stage it outside the AOI box first");
        }
        println!("[aoi] login precondition OK — peer {peer:#x} not visible (outside AOI)");
        c.set_recv_timeout(std::time::Duration::from_millis(300))?;
        std::fs::write("/tmp/ws_aoi_obs_ready", "1").ok();
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
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut ok = false;
            match cmd.as_str() {
                "expect-create" => {
                    // fresh CREATE only: clear the stale sighting so a re-enter is really asserted
                    c.seen_guids.retain(|g| *g != peer);
                    while std::time::Instant::now() < deadline {
                        let _ = c.recv();
                        if c.seen_guids.contains(&peer) { ok = true; break; }
                    }
                }
                "expect-move" => {
                    while std::time::Instant::now() < deadline && !ok {
                        if let Ok((op, payload)) = c.recv_raw() {
                            // MSG_MOVE_* server relays: 0x00B5..=0x00EE range; payload starts with the
                            // mover's PACKED guid.
                            if (0x00B5..=0x00EE).contains(&op) {
                                if let Some(g) = read_packed_guid(&payload) {
                                    if g == peer { moves_from_peer += 1; ok = true; }
                                }
                            }
                        }
                    }
                }
                "expect-destroy" => {
                    while std::time::Instant::now() < deadline && !ok {
                        if let Ok((op, payload)) = c.recv_raw() {
                            // SMSG_DESTROY_OBJECT = 0x00AA, payload = plain LE u64 guid
                            if op == 0x00AA && payload.len() >= 8 {
                                let g = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                                if g == peer { ok = true; }
                            }
                        }
                    }
                }
                other => {
                    std::fs::write(&ack_file, format!("FAIL unknown-command {other}")).ok();
                    bail!("aoi-observer: unknown command {other:?}");
                }
            }
            let verdict = if ok { "OK" } else { "FAIL" };
            println!("[aoi] {cmd} -> {verdict}");
            std::fs::write(&ack_file, format!("{verdict} {cmd}")).ok();
            if !ok { bail!("aoi-observer: {cmd} not satisfied within 30s"); }
        }
    }

    // ---- aoi-mover <cmd_file>: holds a session; on "burst <x> <y> <z>" sends 10 MSG_MOVE_HEARTBEATs
    // stepping around that position (the relay+persist path); on "exit" disconnects abruptly. ----
    if mode.as_deref() == Some("aoi-mover") {
        use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client};
        let cmd_file: String = args.next().unwrap_or_else(|| "/tmp/ws_aoi_mover_cmd".into());
        let _ = std::fs::remove_file(&cmd_file);
        c.set_recv_timeout(std::time::Duration::from_millis(300))?;
        std::fs::write("/tmp/ws_aoi_mover_ready", "1").ok();
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
    if mode.as_deref() == Some("soak") {
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
        let targets: Vec<u64> = args.by_ref().filter_map(|s| s.parse().ok()).collect();
        c.set_recv_timeout(std::time::Duration::from_millis(150))?;
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(secs);
        let (mut hb, mut casts, mut engages, mut packets, mut recv_errs) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut rng: u64 = 0x5eed_5eed; // deterministic LCG — reproducible walk
        let mut engaged: Option<u64> = None;
        let (mut last_cast, mut last_engage_flip) = (std::time::Instant::now(), std::time::Instant::now());
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

/// Parse a vanilla PACKED guid from the head of a payload: mask byte, then one byte per set mask
/// bit (LSB-first). Returns None on truncation.
fn read_packed_guid(payload: &[u8]) -> Option<u64> {
    let mask = *payload.first()?;
    let mut guid: u64 = 0;
    let mut idx = 1usize;
    for bit in 0..8 {
        if mask & (1 << bit) != 0 {
            guid |= (*payload.get(idx)? as u64) << (8 * bit);
            idx += 1;
        }
    }
    Some(guid)
}

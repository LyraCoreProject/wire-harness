//! Single-session PROBE modes: each asserts one wire exchange (a packet or a small
//! sequence) against the live gateway, with at most a handshake file toward the orchestrator.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{anyhow, bail, Result};
use wire_client::{logon, WireClient};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{Class, LogoutResult, WorldResult};
use wow_world_messages::Guid;

use super::{drain_until_file, require_path_arg, signal_and_wait_consumed, ModeCtx};

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    match mode {
        "initial-spells" => initial_spells(c, args, mcx)?,
        "init-factions" => init_factions(c, args, mcx)?,
        "query-item" => query_item(c, args, mcx)?,
        "played-time" => played_time(c, args, mcx)?,
        "played-time-live" => played_time_live(c, args, mcx)?,
        "levelup-info" => levelup_info(c, args, mcx)?,
        "questgiver" => questgiver(c, args, mcx)?,
        "gossip" => gossip(c, args, mcx)?,
        "ghost" => ghost(c, args, mcx)?,
        "stay" => stay(c, args, mcx)?,
        "logout" => logout_probe(c, args, mcx)?,
        "ding" => ding(c, args, mcx)?,
        "repop" => repop(c, args, mcx)?,
        "cast-dump" => cast_dump(c, args, mcx)?,
        "raw-audit" => raw_audit(c, args, mcx)?,
        "name-query" => name_query(c, args, mcx)?,
        "bindpoint" => bindpoint(c, args, mcx)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Run `mode` if it is a CHAR-SELECT-TIER probe: these run BEFORE `login_as` (no world entry),
/// each opening its own logon + world-handshake connection. `Ok(true)` = recognized and
/// completed; `Ok(false)` = not a char-select-tier mode (main.rs proceeds to `login_as`).
pub(crate) fn try_dispatch_charselect(
    mode: &str,
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<bool> {
    match mode {
        "char-enum-gear" => char_enum_gear(account, password, char_name, args)?,
        "char-create-gear" => char_create_gear(account, password, char_name, args)?,
        "char-delete" => char_delete(account, password, char_name, args)?,
        _ => return Ok(false),
    }
    Ok(true)
}

// ---- char-enum-gear probe: verify SMSG_CHAR_ENUM equipment slots carry real display_ids ----
// Usage: wire-client TEST test123 <char-name> char-enum-gear [slot] [want_display_id]
// slot defaults to 15 (main-hand weapon). want_display_id defaults to 0 (asserts nonzero).
// Pass: the named character's equipment slot has a non-zero display_id (or == want if given).
fn char_enum_gear(
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    let slot: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let want: Option<u32> = args.next().and_then(|s| s.parse().ok());
    eprintln!("[wire] char-enum-gear: logon + CMSG_CHAR_ENUM, checking {char_name} slot {slot}…");
    let (k, world_addr) = logon(account, password)?;
    let mut c = WireClient::connect_world(&world_addr, account, k)?;
    let chars = c.char_enum_gear()?;
    let Some((_, _, display_ids)) = chars.iter().find(|(_, n, _)| n.eq_ignore_ascii_case(char_name)) else {
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

// ---- char-create-gear probe: a FRESH, never-logged-in character must already show gear ----
// Usage: wire-client TEST test123 <throwaway-name> char-create-gear [slot]
// Work-item 180: the loadout is granted at CREATION, so SMSG_CHAR_ENUM renders the model armed
// on the very first char-select — before any world entry. The existing char-enum-gear probe
// can't catch a regression here (its subject has logged in, and the first-login safety-net
// grant would have dressed it). Creates <throwaway-name>, asserts, then deletes it.
fn char_create_gear(
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    let slot: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    eprintln!("[wire] char-create-gear: create {char_name}, check slot {slot} WITHOUT logging in…");
    let (k, world_addr) = logon(account, password)?;
    let mut c = WireClient::connect_world(&world_addr, account, k)?;
    if c.char_enum()?.iter().any(|(_, n, _)| n.eq_ignore_ascii_case(char_name)) {
        bail!("char-create-gear: {char_name:?} already exists — pass a throwaway name (it may have logged in before, which would mask the creation-time grant)");
    }
    let guid = c.create_or_find_char(char_name, Class::Warrior)?;
    let chars = c.char_enum_gear()?;
    let Some((_, _, display_ids)) = chars.iter().find(|(g, _, _)| *g == guid) else {
        bail!("char-create-gear: created guid={guid} missing from SMSG_CHAR_ENUM");
    };
    let got = display_ids.get(slot).copied().unwrap_or(0);
    println!("[probe] SMSG_CHAR_ENUM fresh {char_name} slot {slot} display_id={got}");
    // Delete the throwaway BEFORE judging, so a failed assert doesn't leak it into later runs.
    let del = c.char_delete(guid)?;
    if del != WorldResult::CharDeleteSuccess {
        eprintln!("[wire] warning: cleanup delete of {char_name} returned {del:?}");
    }
    if got == 0 {
        bail!("char-create-gear: {char_name} slot {slot} display_id=0 — a never-logged-in character is NAKED on char select (creation-time loadout grant regressed)");
    }
    println!("[wire] CHAR-CREATE-GEAR PASS \u{2713}  fresh {char_name} slot {slot} display_id={got} before any login");
    Ok(())
}

// ---- char-delete probe: CMSG_CHAR_DELETE -> SMSG_CHAR_DELETE(success), row gone (081) ----
// Usage: wire-client <account> <password> <char-name-to-create-then-delete> char-delete
// Char-select tier only (no player_login/world-entry). Creates (or finds) a throwaway
// character named `char_name`, deletes it, and asserts SMSG_CHAR_DELETE(CharDeleteSuccess)
// AND that a follow-up CMSG_CHAR_ENUM no longer lists that guid. The gateway's own DB-row
// check (game_character gone + no owned item/quest/spell rows) is verified separately via
// `spacetime sql`.
fn char_delete(
    account: &str,
    password: &str,
    char_name: &str,
    _args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    eprintln!("[wire] char-delete probe: logon + char-select only (no world entry)…");
    let (k, world_addr) = logon(account, password)?;
    let mut c2 = WireClient::connect_world(&world_addr, account, k)?;
    let guid = c2.create_or_find_char(char_name, Class::Warrior)?;
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
    Ok(())
}

// ---- initial-spells probe: assert SMSG_INITIAL_SPELLS carries the given spell ids ----
// Usage: wire-client TEST test123 <Human char> initial-spells [id1 id2 ...]
// Prints the captured spellbook; with ids, exits 1 unless every id is present.
fn initial_spells(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    println!("[probe] SMSG_INITIAL_SPELLS ({} spells) = {:?}", c.initial_spells.len(), c.initial_spells);
    let want: Vec<u32> = args.filter_map(|s: String| s.parse().ok()).collect();
    let missing: Vec<u32> = want.iter().copied().filter(|id| !c.initial_spells.contains(id)).collect();
    if !missing.is_empty() {
        eprintln!("[wire] FAIL: missing initial spells {missing:?}");
        std::process::exit(1);
    }
    println!("[wire] INITIAL-SPELLS PASS \u{2713}  all of {want:?} present");
    Ok(())
}

// ---- init-factions probe: assert SMSG_INITIALIZE_FACTIONS carries a persisted standing on RELOG (076) ----
// Usage: wire-client TEST test123 <char-name> init-factions <reputation_index> <want_standing>
// The client is already logged in once; this probe reconnects fresh (a real relog) and re-runs
// player_login, so the SMSG_INITIALIZE_FACTIONS captured is the one built by the *second* login
// burst — exactly what a relogging client sees. Asserts slot[index] == want_standing.
fn init_factions(
    _c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let ModeCtx { account, password, char_name } = *mcx;
    let index: usize = args.next().and_then(|s| s.parse().ok()).expect("usage: init-factions <index> <want_standing>");
    let want: i32 = args.next().and_then(|s| s.parse().ok()).expect("usage: init-factions <index> <want_standing>");
    eprintln!("[wire] init-factions: relogging {char_name} to capture a fresh SMSG_INITIALIZE_FACTIONS…");
    let (k2, world_addr2) = logon(account, password)?;
    let mut c2 = WireClient::connect_world(&world_addr2, account, k2)?;
    let guid = c2.create_or_find_char(char_name, Class::Warlock)?;
    c2.player_login(guid)?;
    println!("[probe] SMSG_INITIALIZE_FACTIONS ({} slots) slot[{index}]={:?}", c2.init_factions.len(), c2.init_factions.get(index));
    let got = c2.init_factions.get(index).copied().unwrap_or(0);
    if got != want {
        bail!("init-factions: slot[{index}] = {got}, want {want}");
    }
    println!("[wire] INIT-FACTIONS PASS \u{2713}  slot[{index}] == {want} on relog");
    Ok(())
}

// ---- item-query probe: assert SMSG_ITEM_QUERY_SINGLE_RESPONSE carries armor + stats ----
// Usage: wire-client TEST test123 Ginger query-item <entry> [want_armor] [want_spell] [want_bonding]
// Exits 0 if armor == want_armor (default 105 for Blackrock Gauntlets entry 1448).
// want_bonding: 4th optional arg, -1 (default) = don't check (0=NoBind,1=BoP,2=BoE,3=BoU work-item 127).
fn query_item(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
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
    Ok(())
}

// ---- played-time probe: work-item 029 — CMSG_PLAYED_TIME -> SMSG_PLAYED_TIME (/played) ----
// Usage: wire-client TEST test123 <char-name> played-time
fn played_time(
    c: &mut WireClient,
    _args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    eprintln!("[wire] played-time probe: CMSG_PLAYED_TIME…");
    let (total, level) = c.played_time_request()?;
    println!("[probe] SMSG_PLAYED_TIME total_played_time={total} level_played_time={level}");
    println!("[wire] PLAYED-TIME PASS \u{2713}  got a reply (total={total}s)");
    Ok(())
}

// ---- played-time-live probe: two CMSG_PLAYED_TIME queries with a sleep between, asserting the
// second total is strictly greater — proves the live session's elapsed span is folded into the
// reply in real time (not just accrued at logout). ----
// Usage: wire-client TEST test123 <char-name> played-time-live [sleep_secs]
fn played_time_live(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
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
// deltas (033). Signals the script-owned ready file, then the orchestrator grants kill-XP (through
// grant_xp, e.g. debug_kill_nearest) until the character dings; we decode the resulting
// SMSG_LEVELUP_INFO and print every field. For a mana class the mana delta must be non-zero and at
// least one stat delta non-zero (the pre-033 gateway hardcoded all of them 0). ----
// Usage: wire-client TEST test123 <char-name> levelup-info <ready_file> [expect_mana: 0|1 (default 1)]
fn levelup_info(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let ready = require_path_arg(args, "levelup-info <ready_file> [expect_mana]", "ready_file")?;
    let expect_mana: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    eprintln!("[levelup] in-world as guid {:#x}; signalling orchestrator…", c.self_guid);
    std::fs::write(&ready, "1").ok();
    let done = c.recv_for(std::time::Duration::from_secs(60), |m| match m {
        Smsg::SMSG_LEVELUP_INFO(m) => {
            println!(
                "[probe] SMSG_LEVELUP_INFO new_level={} health={} mana={} str={} agi={} sta={} int={} spi={}",
                m.new_level.as_int(), m.health, m.mana,
                m.strength, m.agility, m.stamina, m.intellect, m.spirit
            );
            let mana_ok = if expect_mana == 1 { m.mana > 0 } else { m.mana == 0 };
            let any_stat = m.intellect > 0 || m.spirit > 0 || m.strength > 0 || m.stamina > 0 || m.agility > 0;
            if mana_ok && any_stat {
                println!("[wire] LEVELUP-INFO PASS \u{2713}  non-zero stat deltas decoded (mana={})", m.mana);
                Some(Ok(()))
            } else {
                Some(Err(anyhow!(
                    "LEVELUP-INFO FAIL: deltas all zero or mana mismatch (mana={} expect_mana={expect_mana})",
                    m.mana
                )))
            }
        }
        _ => None,
    });
    match done {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(e),
        None => bail!("LEVELUP-INFO FAIL: no SMSG_LEVELUP_INFO within 60s of signalling ready"),
    }
}

// ---- questgiver probe: the REAL protocol a questgiver-only NPC (npc_flags=2, no GOSSIP) uses ----
fn questgiver(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let npc: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: … questgiver <npc_guid>");
    eprintln!("[wire] questgiver-probe: CMSG_QUESTGIVER_HELLO -> {npc:#x}");
    c.questgiver_hello(npc)?;
    // Diagnostic drain: log every reply for the whole window (the predicate never matches).
    let mut saw = false;
    let _ = c.recv_for(std::time::Duration::from_secs(3), |m| {
        match m {
            Smsg::SMSG_QUESTGIVER_QUEST_LIST(q) => {
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
            Smsg::SMSG_QUESTGIVER_QUEST_DETAILS(d) => {
                saw = true;
                println!(
                    "[probe] SMSG_QUESTGIVER_QUEST_DETAILS quest_id={} title={:?} (INSTANT — single quest opens directly)",
                    d.quest_id, d.title
                );
            }
            _ => {}
        }
        None::<()>
    });
    println!(
        "[probe] done — {}",
        if saw { "gateway answered the questgiver hello" } else { "gateway sent NOTHING (handler aborted / wrong path)" }
    );
    Ok(())
}

// ---- gossip probe: dump the SMSG the gateway sends for CMSG_GOSSIP_HELLO to an NPC ----
fn gossip(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let npc: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: … gossip <npc_guid>");
    eprintln!("[wire] gossip-probe: CMSG_GOSSIP_HELLO -> {npc:#x}");
    c.gossip_hello(npc)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut saw_gossip = false;
    // Kept hand-rolled: the SMSG_GOSSIP_MESSAGE arm re-enters the client (the npc_text_query
    // round-trip), which recv_for's predicate — already borrowing the client — cannot do.
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
    // Optional: `… gossip <npc> select <index>` — click that option and report what routes back
    // (work-item 247 follow-up: the trainer option closed the window because every imported
    // option's action was a broadcast_text id, so the TRAINER match never fired).
    if args.next().as_deref() == Some("select") {
        use wow_world_messages::vanilla::CMSG_GOSSIP_SELECT_OPTION;
        let idx: u32 = args.next().and_then(|s| s.parse().ok()).expect("select <index>");
        eprintln!("[wire] gossip-probe: CMSG_GOSSIP_SELECT_OPTION({idx}) -> {npc:#x}");
        c.send(&CMSG_GOSSIP_SELECT_OPTION { guid: Guid::new(npc), gossip_list_id: idx, unknown: None })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_TRAINER_LIST(t)) => {
                    println!(
                        "[probe] SMSG_TRAINER_LIST guid={:#x} spells={} greeting={:?}",
                        t.guid.guid(), t.spells.len(), t.greeting
                    );
                    return Ok(());
                }
                Ok(Smsg::SMSG_GOSSIP_COMPLETE) => {
                    println!("[probe] SMSG_GOSSIP_COMPLETE (window closed — option routed nowhere)");
                    return Ok(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!("[probe] select: NO routed response within 3s");
    }
    Ok(())
}

// ---- ghost-reveal probe (PIECE 2): a viewer who dies + becomes a GHOST should get the spirit-healer
// entity CREATE'd (the GW_AOI=0 on_update reveal). The orchestrator kills+repops via debug reducers
// (the char's small guid). PASS = healer hidden while alive, then revealed on the ghost transition.
fn ghost(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let healer: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: … ghost <healer_guid> <ready_file>");
    let ready = require_path_arg(args, "ghost <healer_guid> <ready_file>", "ready_file")?;
    let before = c.seen_guids.contains(&healer);
    println!("[ghost] healer {healer:#x} visible while ALIVE: {before}  (want false — the alive-gate)");
    std::fs::write(&ready, "1").ok();
    eprintln!("[ghost] ready — waiting for kill+repop, then the reveal CREATE…");
    // recv() records every CREATE into seen_guids en route; matching the healer's own CREATE
    // closes the window as soon as the reveal lands.
    let _ = c.recv_for(std::time::Duration::from_secs(15), |m| match m {
        Smsg::SMSG_UPDATE_OBJECT(u)
            if u.objects.iter().any(|o| wire_client::create_object_guid(o) == Some(healer)) =>
        {
            Some(())
        }
        _ => None,
    });
    let after = c.seen_guids.contains(&healer);
    println!("[ghost] healer visible after GHOST transition: {after}  (want true — the reveal)");
    if !before && after {
        println!("[wire] GHOST-REVEAL PASS \u{2713}  hidden while alive, CREATE'd on the ghost transition");
        return Ok(());
    }
    bail!("ghost-reveal: before={before} after={after} (want false->true)");
}

// ---- stay probe: login + stay connected until a sentinel file appears, then exit Ok.
// Usage: wire-client [account] [password] [char-name] stay <sentinel_file> [deadline_secs]
// The external orchestrator writes anything to sentinel_file to signal done; we exit 0.
// Useful when the test only needs the character to be live in game_world_entity while an
// external script calls spacetime reducers (e.g. work-item #092 combat-regen probe).
//
// `deadline_secs` is OPTIONAL (defaults to 60, the historical hardcoded value) so every
// pre-existing 1-arg caller is unaffected. Work-item 157: scenario-vendor's stay session from
// Step 3 was held live across Step 4's up-to-120s fight-durability poll — the old hardcoded 60s
// self-deadline could silently end the session (exit 0, "STAY TIMEOUT" — NOT an error) mid-poll,
// dropping the character's connection and starving the fight of real swings. Callers that hold a
// stay session across a longer window now pass a deadline that exceeds it (scenario-lib's
// `stay_start`'s optional 4th arg).
fn stay(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let sentinel = require_path_arg(args, "stay <sentinel_file> [deadline_secs]", "sentinel_file")?;
    // An ABSENT deadline defaults to 60 (every pre-existing 1-arg caller). A PRESENT-but-malformed
    // deadline is a hard error, NOT a default: silently falling back to 60 would recreate the exact
    // silent mid-poll self-exit this arg exists to fix (a typo'd call site would reintroduce the
    // 157 flake with no error anywhere — stay's log defaults to /dev/null under stay_start).
    let secs: u64 = match args.next() {
        None => 60,
        Some(s) => s
            .parse()
            .map_err(|_| anyhow!("stay: deadline_secs must be an integer, got {s:?}"))?,
    };
    let _ = std::fs::remove_file(&sentinel);
    eprintln!("[wire] stay: draining socket until {sentinel} appears (deadline {secs}s)…");
    if drain_until_file(c, &sentinel, secs) {
        let _ = std::fs::remove_file(&sentinel);
        println!("[wire] STAY DONE — sentinel received, exiting.");
    } else {
        println!("[wire] STAY TIMEOUT — orchestrator did not signal within {secs}s.");
    }
    Ok(())
}

// ---- logout probe: assert out-of-combat logout replies Success + LOGOUT_COMPLETE ----
// Usage: wire-client [account] [password] [char-name] logout
// Pass: SMSG_LOGOUT_RESPONSE(Success, Instant) → SMSG_LOGOUT_COMPLETE
// Fail: FailureInCombat or timeout.
fn logout_probe(
    c: &mut WireClient,
    _args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    eprintln!("[wire] logout probe — sending CMSG_LOGOUT_REQUEST…");
    let result = c.logout_request()?;
    match result {
        LogoutResult::Success => {
            println!("[wire] LOGOUT PASS \u{2713}  SMSG_LOGOUT_RESPONSE(Success, Instant) + SMSG_LOGOUT_COMPLETE");
            Ok(())
        }
        other => bail!("logout: expected Success, got {other:?}"),
    }
}

// ---- ding probe: verify mid-session L10 ding pushes PLAYER_CHARACTER_POINTS1=1 ----
// Usage: wire-client [account] [password] [char-name] ding <ready_file>
// The orchestrator (test-ding.sh) must first set the char to L9, wait for its ready file,
// then call `spacetime call spacetime-core debug_set_level <guid> 10`.
// Pass: an SMSG_UPDATE_OBJECT arrives that contains BOTH a level=10 word [0x0a 00 00 00]
// AND a character_points1=1 word [0x01 00 00 00] — the levelup VALUES packet (#032 fix).
fn ding(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let ready = require_path_arg(args, "ding <ready_file>", "ready_file")?;
    eprintln!("[ding] in-world as {} (guid {}), signalling orchestrator…", c.self_guid, c.self_guid);
    std::fs::write(&ready, "1").ok();

    // SMSG_UPDATE_OBJECT opcode (vanilla 1.12) = 0x00A9 = 169.
    // Scan raw frames: the gtker reader rejects TYPE-less Player masks (it requires
    // OBJECT_FIELD_TYPE), so we use recv_raw_for() to capture the decrypted payload bytes
    // directly rather than going through the gtker decode path.
    const SMSG_UPDATE_OBJECT: u16 = 0x00A9;
    let level10_word = [0x0au8, 0x00, 0x00, 0x00]; // level=10 as little-endian u32
    let cp1_word = [0x01u8, 0x00, 0x00, 0x00];     // character_points1=1 as little-endian u32
    let done = c.recv_raw_for(std::time::Duration::from_secs(30), |opcode, payload| {
        if opcode != SMSG_UPDATE_OBJECT {
            return None;
        }
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
        if !has_level10 {
            return None;
        }
        println!("[ding] payload (hex): {:02x?}", payload);
        if has_cp1 {
            println!(
                "[wire] DING PASS \u{2713}  SMSG_UPDATE_OBJECT carries level=10 + PLAYER_CHARACTER_POINTS1=1"
            );
            Some(Ok(()))
        } else {
            Some(Err(anyhow!(
                "ding: SMSG_UPDATE_OBJECT has level=10 but PLAYER_CHARACTER_POINTS1=1 is MISSING — build_levelup_values regression"
            )))
        }
    });
    match done {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(e),
        None => bail!("ding: no SMSG_UPDATE_OBJECT with level=10 + character_points1=1 within 30s"),
    }
}

// ---- repop-delay probe: CMSG_REPOP_REQUEST → assert SMSG_CORPSE_RECLAIM_DELAY(30s) ----
// Usage: wire-client [account] [password] [char-name] repop <char-small-guid> <ready_file>
// The orchestrator must kill the character (debug_set_health 0) then consume the ready file;
// then we send CMSG_REPOP_REQUEST and assert the gateway emits the 30s delay packet.
// Pass: SMSG_CORPSE_RECLAIM_DELAY with delay == Duration::from_secs(30).
fn repop(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let char_name = mcx.char_name;
    let char_guid: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: … repop <char-guid> <ready_file>");
    let ready = require_path_arg(args, "repop <char-guid> <ready_file>", "ready_file")?;
    eprintln!("[repop] in-world as {char_name} (guid {:#x}); signalling orchestrator…", c.self_guid);
    // Signal ready, then wait for the orchestrator to kill the character (it consumes the file).
    signal_and_wait_consumed(c, &ready, 30, "repop: orchestrator never confirmed the kill")?;
    eprintln!("[repop] sending CMSG_REPOP_REQUEST for char_guid={char_guid:#x}…");
    c.repop_request()?;

    let got_delay = c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_CORPSE_RECLAIM_DELAY(d) => Some(d.delay),
        _ => None,
    });
    let want = std::time::Duration::from_secs(30);
    match got_delay {
        Some(d) if d == want => {
            println!("[wire] REPOP-DELAY PASS \u{2713}  SMSG_CORPSE_RECLAIM_DELAY(delay={}ms) received", d.as_millis());
            Ok(())
        }
        Some(d) => bail!("repop-delay: got delay={:?}, want {:?}", d, want),
        None => bail!("repop-delay: no SMSG_CORPSE_RECLAIM_DELAY received within 5s"),
    }
}

fn cast_dump(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
    c.cast_spell(spell, c.self_guid)?;
    // Diagnostic dump: print every raw frame for the window (the predicate never matches).
    let _ = c.recv_raw_for(std::time::Duration::from_secs(6), |op, payload| {
        println!("[dump] opcode 0x{op:04X} len={}", payload.len());
        None::<()>
    });
    Ok(())
}

// ---- raw-audit <seconds>: diagnostic — every SMSG_UPDATE_OBJECT frame is decode-attempted;
// reports created guids vs undecodable frames (gtker gaps surface here). ----
fn raw_audit(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    // Optional "move": stream MSG_MOVE_HEARTBEATs while auditing (254 — self-echo detection).
    let moving = args.next().as_deref() == Some("move");
    println!("[audit] initial seen_guids: {:x?} moving={moving}", c.seen_guids);
    c.set_recv_timeout(std::time::Duration::from_millis(250))?;
    let t0 = std::time::Instant::now();
    if moving {
        use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client};
        let (sx, sy, sz) = (-8940.0f32, -120.0f32, 78.0f32); // the Northshire test pad
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut step = 0f32;
        while std::time::Instant::now() < deadline {
            step += 1.0;
            c.send(&MSG_MOVE_HEARTBEAT_Client {
                info: MovementInfo {
                    flags: MovementInfo_MovementFlags::empty(),
                    timestamp: t0.elapsed().as_millis() as u32,
                    position: Vector3d { x: sx + step * 2.0, y: sy, z: sz },
                    orientation: 0.0,
                    fall_time: 0.0,
                },
            })?;
            let _ = c.recv_raw_for(std::time::Duration::from_millis(480), |op, payload| {
                if (0x00B5..=0x00EE).contains(&op) {
                    println!("[audit] t={}ms SELF-WINDOW MOVE-OP 0x{op:04X} len={}", t0.elapsed().as_millis(), payload.len());
                }
                None::<()>
            });
        }
        return Ok(());
    }
    let _ = c.recv_raw_for(std::time::Duration::from_secs(secs), |op, payload| {
        if op == 0x00A9 {
            println!("[audit] t={}ms UPDATE_OBJECT len={}", t0.elapsed().as_millis(), payload.len());
        }
        // 254: MSG_MOVE_* / SMSG_MONSTER_MOVE arrival timing — rhythmic relay gaps show here.
        if op == 0x00DD {
            println!("[audit] t={}ms MONSTER_MOVE len={}", t0.elapsed().as_millis(), payload.len());
        }
        // Item-gain feedback (185/#15): surface the push-result + quest-item toast opcodes.
        if op == 0x0166 {
            let item = u32::from_le_bytes([payload[25], payload[26], payload[27], payload[28]]);
            let count = u32::from_le_bytes([payload[37], payload[38], payload[39], payload[40]]);
            println!("[audit] ITEM_PUSH_RESULT len={} item={item} count={count}", payload.len());
        }
        if op == 0x019A {
            println!("[audit] QUESTUPDATE_ADD_ITEM len={}", payload.len());
        }
        if op == 0x0150 {
            let amount = u32::from_le_bytes([payload[payload.len()-5], payload[payload.len()-4], payload[payload.len()-3], payload[payload.len()-2]]);
            println!("[audit] SPELLHEALLOG len={} amount~{amount}", payload.len());
        }
        None::<()>
    });
    Ok(())
}

// ---- name-query <guid> <want_name>: CMSG_NAME_QUERY -> SMSG_NAME_QUERY_RESPONSE(name) ----
// (work-item 142: proves a session-less playerbot's name resolves like any player's.)
fn name_query(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::CMSG_NAME_QUERY;
    let guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("guid");
    let want: String = args.next().expect("want name");
    c.send(&CMSG_NAME_QUERY { guid: Guid::new(guid) })?;
    let got = c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_NAME_QUERY_RESPONSE(r) if r.guid.guid() == guid => {
            Some(r.character_name.clone())
        }
        _ => None,
    });
    match got {
        Some(name) => {
            println!("[probe] SMSG_NAME_QUERY_RESPONSE guid={guid:#x} name={name:?}");
            if name == want {
                println!("[wire] NAME-QUERY PASS \u{2713}  {guid:#x} resolves to {want:?}");
                Ok(())
            } else {
                bail!("name-query: guid {guid:#x} resolved to {name:?}, want {want:?}");
            }
        }
        None => bail!("name-query: no SMSG_NAME_QUERY_RESPONSE for {guid:#x} within 5s"),
    }
}

// ---- bindpoint probe: assert SMSG_BINDPOINTUPDATE carries home_x/y/z (not login position) ----
// Usage: wire-client [account] [password] [char-name] bindpoint <home_x> <home_y> <home_z>
// The char must have home_* != login position (e.g. "Tester": login=(-8935, -188) home=(-8873, -134)).
// Pass: SMSG_BINDPOINTUPDATE.position.x == home_x (within 0.1 tolerance).
fn bindpoint(
    _c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<()> {
    let ModeCtx { account, password, char_name } = *mcx;
    let want_x: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
    let want_y: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
    let want_z: f32 = args.next().and_then(|s| s.parse().ok()).expect("usage: … bindpoint <home_x> <home_y> <home_z>");
    eprintln!("[bindpoint] want home=({want_x},{want_y},{want_z}) — verifying SMSG_BINDPOINTUPDATE…");

    // Re-login manually so we can capture SMSG_BINDPOINTUPDATE from the post-login burst.
    // (The `login_as`/`player_login` helper drains the burst without exposing that packet.)
    let (k, world_addr) = wire_client::logon(account, password)?;
    let mut c2 = wire_client::WireClient::connect_world(&world_addr, account, k)?;
    let chars = c2.char_enum()?;
    let guid = chars
        .iter()
        .find(|(_, n, _)| n.eq_ignore_ascii_case(char_name))
        .map(|(g, _, _)| *g)
        .ok_or_else(|| anyhow!("bindpoint: character {char_name:?} not found"))?;
    eprintln!("[bindpoint] found {char_name} guid={guid}; sending CMSG_PLAYER_LOGIN…");
    use wow_world_messages::vanilla::CMSG_PLAYER_LOGIN;
    c2.send(&CMSG_PLAYER_LOGIN { guid: Guid::new(guid) })?;

    // Drain the login burst, capturing SMSG_BINDPOINTUPDATE; the burst is complete (and the
    // window closes) once the self CREATE_OBJECT arrives.
    let mut bind: Option<(f32, f32, f32)> = None;
    let _ = c2.recv_for(std::time::Duration::from_secs(10), |m| {
        if let Smsg::SMSG_BINDPOINTUPDATE(b) = m {
            bind = Some((b.position.x, b.position.y, b.position.z));
            eprintln!("[bindpoint] SMSG_BINDPOINTUPDATE: x={} y={} z={}", b.position.x, b.position.y, b.position.z);
        }
        match m {
            Smsg::SMSG_UPDATE_OBJECT(u)
                if u.objects.iter().any(|o| wire_client::create_object_guid(o) == Some(guid)) =>
            {
                Some(())
            }
            _ => None,
        }
    });
    match bind {
        None => bail!("bindpoint: SMSG_BINDPOINTUPDATE never received in login burst"),
        Some((got_x, got_y, got_z)) => {
            eprintln!("[bindpoint] got=({got_x},{got_y},{got_z}) want=({want_x},{want_y},{want_z})");
            if (got_x - want_x).abs() > 0.5 || (got_y - want_y).abs() > 0.5 {
                bail!(
                    "bindpoint: SMSG_BINDPOINTUPDATE carries login position, not home!\n  got  x={got_x} y={got_y} z={got_z}\n  want x={want_x} y={want_y} z={want_z}"
                );
            }
            println!("[wire] BINDPOINT PASS \u{2713}  SMSG_BINDPOINTUPDATE x={got_x} y={got_y} z={got_z} matches persisted home");
            Ok(())
        }
    }
}

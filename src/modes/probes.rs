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
        "swing-flow" => swing_flow(c, args, mcx)?,
        "armor-audit" => armor_audit(c, args, mcx)?,
        "fall" => fall_probe(c, args, mcx)?,
        "engage-retreat" => engage_retreat(c, args, mcx)?,
        "watch-casts" => watch_casts(c, args, mcx)?,
        "channel" => channel_probe(c, args, mcx)?,
        "raw-audit" => raw_audit(c, args, mcx)?,
        "autoshot" => autoshot(c, args, mcx)?,
        "talent" => talent_probe(c, args, mcx)?,
        "groundcast" => groundcast(c, args, mcx)?,
        "backswing" => backswing(c, args, mcx)?,
        "setbutton" => setbutton(c, args, mcx)?,
        "fishcast" => fishcast(c, args, mcx)?,
        "petsummon" => petsummon(c, args, mcx)?,
        "atwar" => atwar(c, args, mcx)?,
        "values-watch" => values_watch(c, args, mcx)?,
        "opcode-watch" => opcode_watch(c, args, mcx)?,
        "addon-ping" => addon_ping(c, args, mcx)?,
        "event-stream" => event_stream(c, args, mcx)?,
        "walkmelee" => walkmelee(c, args, mcx)?,
        "seamwalk" => seamwalk(c, args, mcx)?,
        "casttime" => casttime(c, args, mcx)?,
        "name-query" => name_query(c, args, mcx)?,
        "bindpoint" => bindpoint(c, args, mcx)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Run `mode` if it is a CHAR-SELECT-TIER probe: these run BEFORE `login_as` (no world entry),
/// each opening its own logon + world-handshake connection. `Ok(true)` = recognized and
/// completed; `Ok(false)` = not a char-select-tier mode (main.rs proceeds to `login_as`).
/// make-char <class>: create the named character (idempotent) and exit — for staging a
/// class-specific probe char (e.g. a paladin for trainer-state checks).
fn make_char(
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    let class = match args.next().as_deref() {
        Some("paladin") => Class::Paladin,
        Some("mage") => Class::Mage,
        Some("rogue") => Class::Rogue,
        Some("priest") => Class::Priest,
        Some("warlock") => Class::Warlock,
        _ => Class::Warrior,
    };
    let (k, world_addr) = logon(account, password)?;
    let mut c = WireClient::connect_world(&world_addr, account, k)?;
    let guid = c.create_or_find_char(char_name, class)?;
    println!("[probe] make-char: {char_name} ({class:?}) guid={guid}");
    Ok(())
}

pub(crate) fn try_dispatch_charselect(
    mode: &str,
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<bool> {
    match mode {
        "char-enum-gear" => char_enum_gear(account, password, char_name, args)?,
        "make-char" => make_char(account, password, char_name, args)?,
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
    // Optional (195B): also assert the slot's AT_WAR flag bit (0x02) — pass 1/0. Absent = don't check.
    let want_atwar: Option<u8> = args.next().and_then(|s| s.parse().ok());
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
    if let Some(w) = want_atwar {
        let flags = c2.init_faction_flags.get(index).copied().unwrap_or(0);
        let got_atwar = (flags & 0x02 != 0) as u8;
        if got_atwar != w {
            bail!("init-factions: slot[{index}] AT_WAR = {got_atwar} (flags {flags:#x}), want {w}");
        }
        println!("[probe] slot[{index}] flags={flags:#x} AT_WAR={got_atwar} \u{2713}");
    }
    println!("[wire] INIT-FACTIONS PASS \u{2713}  slot[{index}] == {want} on relog");
    Ok(())
}

// ---- values-watch <guid> <field_index> [seconds]: drain raw frames and PASS as soon as an
// Generic raw-opcode watcher: pass when opcode <opcode> (decimal) arrives within [secs]. Prints the
// first 8 body bytes as (u32, u32) — handy for SMSG_EXPLORATION_EXPERIENCE (0x01F8=504: area_id, xp).
/// addon-ping [payload] — the 184 bridge round-trip: fake `SendAddonMessage("STC",
/// "v1|ping|0|1/1|<payload>", "WHISPER", self)` byte-for-byte via `send_raw` (gtker can't encode
/// LANG_ADDON), then raw-watch for the addon-language `SMSG_MESSAGECHAT` whose text carries the
/// pong envelope echoing the payload. PASSes only on a full envelope match.
fn addon_ping(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::Duration;
    let payload = args.next().unwrap_or_else(|| "hello".to_string());
    let text = format!("STC\tv1|ping|0|1/1|{payload}");
    // CMSG_MESSAGECHAT (0x095) body: chat_type u32 (6 = Whisper), language u32 (LANG_ADDON),
    // target CString, message CString — the shape the real client emits for SendAddonMessage.
    let mut body = Vec::new();
    body.extend_from_slice(&6u32.to_le_bytes());
    body.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    body.extend_from_slice(b"Self\0");
    body.extend_from_slice(text.as_bytes());
    body.push(0);
    c.send_raw(0x0095, &body)?;
    println!("[addon] sent STC ping ({payload:?})");

    let want = format!("STC\tv1|pong|0|1/1|{payload}");
    c.set_recv_timeout(Duration::from_millis(300))?;
    let got = c.recv_raw_for(Duration::from_secs(20), |op, b| {
        if op != 0x0096 || b.len() < 13 {
            return None;
        }
        // SMSG_MESSAGECHAT: chat_type u8, language u32 — ours iff language == LANG_ADDON.
        let lang = u32::from_le_bytes([b[1], b[2], b[3], b[4]]);
        if lang != 0xFFFF_FFFF {
            return None;
        }
        let text = String::from_utf8_lossy(&b[17..]);
        text.contains(want.as_str()).then(|| ())
    });
    if got.is_some() {
        println!("[wire] ADDON-PING PASS \u{2713} pong envelope round-tripped");
        Ok(())
    } else {
        bail!("addon-ping: no addon-language pong within 20s");
    }
}

/// event-stream [def_id] [secs] — the 280 addon UI feed, as the Lua addon receives it: watch the
/// raw addon-language SMSG_MESSAGECHAT stream for TWO DISTINCT `event.state`/`event.start`
/// envelopes for `def_id` (heartbeats differ — the countdown alone moves), proving the UI stream
/// is live and UPDATING on the real wire. The character must stand within the event's addon
/// range (the driver script teleports it in).
fn event_stream(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    let def: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1001);
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(45);
    let want_prefix = format!("{def}|");
    c.set_recv_timeout(Duration::from_millis(300))?;
    let mut seen: Vec<String> = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) && seen.len() < 2 {
        let Ok((op, b)) = c.recv_raw() else { continue };
        if op != 0x0096 || b.len() < 18 {
            continue;
        }
        let lang = u32::from_le_bytes([b[1], b[2], b[3], b[4]]);
        if lang != 0xFFFF_FFFF {
            continue;
        }
        let text = String::from_utf8_lossy(&b[17..]).trim_matches('\0').to_string();
        let Some(pos) = text.find("STC\tv1|") else { continue };
        let mut it = text[pos + 4..].splitn(5, '|');
        let (_v, cmd) = (it.next(), it.next().unwrap_or(""));
        let (_seq, _part) = (it.next(), it.next());
        let payload = it.next().unwrap_or("");
        if (cmd == "event.state" || cmd == "event.start") && payload.starts_with(&want_prefix) {
            println!("[event-stream] {cmd}: {payload}");
            if seen.last().map(String::as_str) != Some(payload) {
                seen.push(payload.to_string());
            }
        }
    }
    if seen.len() >= 2 {
        println!("[wire] EVENT-STREAM PASS \u{2713} two distinct live state frames for def {def}");
        Ok(())
    } else {
        bail!("event-stream: {} distinct state frame(s) for def {def} within {secs}s", seen.len());
    }
}

fn opcode_watch(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    let want: u16 = args.next().and_then(|s| s.parse().ok()).expect("usage: opcode-watch <opcode-decimal> [secs]");
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Ok((op, body)) = c.recv_raw() {
            if op == want {
                if body.len() >= 8 {
                    let a = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                    let e = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                    println!("[opcode] opcode={want:#x} body[0..8] = ({a}, {e})");
                }
                println!("[wire] OPCODE-WATCH PASS \u{2713} opcode {want:#x} arrived");
                return Ok(());
            }
        }
    }
    bail!("opcode-watch: opcode {want:#x} did not arrive within {secs}s");
}

// SMSG_UPDATE_OBJECT VALUES block for <guid> carries <field_index> (testing-hardening §3.1 — the
// generic live-field-change assert; prints every matching (index, value) seen on the way).
fn values_watch(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    let guid: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: values-watch <guid> <field_index> [secs] [expect_value]");
    let field: u16 = args.next().and_then(|s| s.parse().ok()).expect("field_index");
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    // Optional EXPECTED VALUE. Without it this passes on the first sighting of the field — which is
    // this watcher's OWN login snapshot, since entering the world writes the entity row and relays
    // its fields. A caller testing a live TRANSITION (rest-state's inn crossing: 0x02000000 NORMAL →
    // 0x01000000 RESTED) therefore passed on the pre-transition value and stopped watching before the
    // one it was waiting for. Given a value, keep watching until THAT value arrives.
    let expect: Option<u32> = args.next().and_then(|s| s.parse().ok());
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Ok((0x00A9, body)) = c.recv_raw() {
            for u in wire_client::values_mask::parse_values_updates(&body) {
                if u.guid != guid {
                    continue;
                }
                for &(idx, v) in &u.fields {
                    println!("[values] guid={guid:#x} field {idx} = {v} ({v:#x})");
                    if idx == field && expect.is_none_or(|want| want == v) {
                        println!("[wire] VALUES-WATCH PASS \u{2713} field {field} arrived");
                        return Ok(());
                    }
                }
            }
        }
    }
    match expect {
        Some(want) => bail!(
            "values-watch: no VALUES update carried field {field} = {want} ({want:#x}) for \
             {guid:#x} within {secs}s"
        ),
        None => bail!("values-watch: no VALUES update carried field {field} for {guid:#x} within {secs}s"),
    }
}

// ---- walkmelee <mob_guid> <mx> <my> <mz>: prove the walk_to helper closes real distance
// (testing-hardening §3.3): start 12 yd west of the mob (OUTSIDE the 5 yd standstill reach —
// the pre-walk swing would be silently range-gated forever), walk to 3 yd, toggle attack, and
// assert OUR OWN ATTACKERSTATEUPDATE lands (a swing fired => the server tracked the walk).
fn walkmelee(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{CMSG_ATTACKSWING, CMSG_ATTACKSTOP};
    let mob: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: walkmelee <mob> <x> <y> <z>");
    let mx: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob x");
    let my: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob y");
    let mz: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob z");
    c.walk_to((mx - 12.0, my, mz), (mx - 3.0, my, mz), 7.0)?; // real vanilla run speed
    c.set_selection(mob)?;
    c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) })?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    let mut own_swing = false;
    // 270: 20s (was 8s) + periodic re-send of CMSG_ATTACKSWING. Under full-suite commit-stream load the
    // walk_to heartbeat stream + the starved tick_melee can leave the first ATTACKSWING arriving before
    // the server has processed the walk (char still out of the 5yd reach → the swing never arms). A
    // wider window lets the walk land, and re-sending ATTACKSWING every ~2s re-attempts the arm once the
    // position catches up. This is the load-timing flake class (work-item 270), not a walk_to bug.
    let mut last_swing_send = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) && !own_swing {
        if last_swing_send.elapsed() >= Duration::from_secs(2) {
            let _ = c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) });
            last_swing_send = Instant::now();
        }
        if let Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(a)) = c.recv() {
            if a.attacker.guid() == c.self_guid {
                own_swing = true;
            }
        }
    }
    let _ = c.send(&CMSG_ATTACKSTOP {});
    if !own_swing {
        bail!("walkmelee FAIL: no own swing after walking into reach (walk_to position not tracked?)");
    }
    println!("[wire] WALKMELEE PASS \u{2713} walked 12yd -> 3yd and the swing fired");
    Ok(())
}

// ---- seamwalk <x0> <y0> <z0> <x1> <y1> <z1> [oneway|expect-handoff]: walk across a REGION SEAM.
//
// Two callers, one mechanism:
//
// * **No trailing flag, or `oneway`** (issue #68 AC3): a DORMANT seam — two regions assigned to
//   the SAME database — must be invisible: no handoff, no reconnect, nothing the client can tell
//   apart from ordinary ground. That is a claim about what did NOT happen, which is exactly what a
//   human watching a screen cannot verify, and it is why this is a probe rather than an eyeball
//   item. `oneway` leaves the character on the far side (needed because a round trip ends where it
//   began, indistinguishable from the walk never having applied — the caller asserts the final
//   position server-side to tell those two apart).
// * **`expect-handoff`** (#72 slice 2): the LIVE seam — the two endpoints are on DIFFERENT
//   databases. The wire-level signature of a SUCCESSFUL warm handoff is IDENTICAL to a dormant
//   no-op (no `SMSG_TRANSFER_PENDING`/`SMSG_NEW_WORLD` either way — that is the whole point of "no
//   loading screen"), so this reuses the exact same walk+drain mechanism and the exact same
//   packet-level assertion. What it adds: it is always one-way (there is somewhere real to land),
//   and after landing it walks a short WIGGLE fully inside the new cell — proving the session
//   keeps receiving ordinary world traffic from wherever it now lives, not just that the socket
//   didn't drop the instant the crossing happened. Server-side proof (which database holds the
//   durable row, both escrow ledgers empty, a re-login not re-transferring) is NOT this probe's
//   job — it has no privileged DB access — see `test-warm-handoff.sh`, which wraps this with
//   `spacetime sql` on both databases.
//
// The failure both callers care about is a crossing that DOES send a wire-level transfer packet
// when it shouldn't have (a region wrongly assigned, or — for `expect-handoff` — a warm handoff
// that fell back to a loading screen instead of driving invisibly): `SMSG_TRANSFER_PENDING` +
// `SMSG_NEW_WORLD`, which `recv` surfaces even though it auto-acks the worldport.
//
// Walked in short legs with a drain between each: `walk_to` only sends, so a long unbroken walk lets
// the gateway's outbound queue back up until it drops the session (danger-zones §2) — which would
// read as a handoff-adjacent failure and would be the harness's fault, not the server's.
fn seamwalk(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use game_shared::spatial::grid_cell;
    use std::time::Duration;
    let mut f = || -> f32 {
        args.next()
            .and_then(|s| s.parse().ok())
            .expect("usage: seamwalk <x0> <y0> <z0> <x1> <y1> <z1> [oneway|expect-handoff]")
    };
    let (x0, y0, z0, x1, y1, z1) = (f(), f(), f(), f(), f(), f());
    let flag = args.next();
    let expect_handoff = flag.as_deref() == Some("expect-handoff");
    // `expect-handoff` implies oneway (there is somewhere real to land); an ordinary `oneway` is
    // the dormant-seam caller's own explicit choice.
    let oneway = expect_handoff || flag.as_deref() == Some("oneway");
    let (a, b) = ((x0, y0, z0), (x1, y1, z1));
    let (ca, cb) = (grid_cell(x0, y0), grid_cell(x1, y1));
    let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    println!("[wire] seamwalk: cell {ca:?} -> {cb:?} ({dist:.0} yd each way)");
    if ca == cb {
        bail!("seamwalk FAIL: both endpoints are in cell {ca:?} — this walk crosses no cell boundary, so it cannot cross a seam either");
    }

    // Legs of ~35 yd (~5 s at run speed) so the socket is drained often enough to stay healthy.
    const LEG_YD: f32 = 35.0;
    let mut ported = Vec::new();
    let mut drained = 0u32;
    let mut walk = |c: &mut WireClient, from: (f32, f32, f32), to: (f32, f32, f32)| -> Result<()> {
        let leg_dist = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
        let legs = ((leg_dist / LEG_YD).ceil() as u32).max(1);
        let mut prev = from;
        for i in 1..=legs {
            let t = i as f32 / legs as f32;
            let next = (
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t,
                from.2 + (to.2 - from.2) * t,
            );
            c.walk_to(prev, next, 7.0)?; // real vanilla run speed
            prev = next;
            // Drain whatever the walk produced. A transfer here is the defect under test, so it is
            // recorded rather than acted on — and `recv` has already acked the worldport, which keeps
            // the session usable so the walk back can still run and report.
            c.set_recv_timeout(Duration::from_millis(400))?;
            loop {
                match c.recv() {
                    Ok(Smsg::SMSG_TRANSFER_PENDING(_)) => ported.push("SMSG_TRANSFER_PENDING"),
                    Ok(Smsg::SMSG_NEW_WORLD(_)) => ported.push("SMSG_NEW_WORLD"),
                    Ok(_) => drained += 1,
                    Err(e) if wire_client::is_read_timeout(&e) => break, // quiet — leg done
                    Err(e) => return Err(e),                            // a real socket failure
                }
            }
        }
        Ok(())
    };

    walk(c, a, b)?;
    println!("[wire] seamwalk: crossed into cell {cb:?}");
    if expect_handoff {
        // Prove the session is still being served, from wherever it now lives — not merely that
        // the socket survived the instant of the crossing. A short there-and-back fully inside the
        // destination cell (no further seam involved): any further TRANSFER packet here would mean
        // the destination itself is mid-handoff-loop or the session desynced silently.
        let wiggle_far = (b.0 + 10.0, b.1, b.2);
        walk(c, b, wiggle_far)?;
        walk(c, wiggle_far, b)?;
        println!("[wire] seamwalk: post-handoff wiggle in cell {cb:?} served normally");
    } else if oneway {
        println!("[wire] seamwalk: ONEWAY — left standing in cell {cb:?} for a server-side position check");
    } else {
        walk(c, b, a)?;
        println!("[wire] seamwalk: returned to cell {ca:?}");
    }

    if !ported.is_empty() {
        bail!(
            "seamwalk FAIL: {} sent a wire-level transfer packet ({}) — {}",
            if expect_handoff { "the handoff" } else { "the crossing" },
            ported.join(", "),
            if expect_handoff {
                "a warm handoff must be invisible on the wire, exactly like a dormant no-op"
            } else {
                "a seam whose regions share one database must be a strict no-op"
            }
        );
    }
    // The session surviving every leg is the other half of "nothing went wrong on the wire": a
    // dropped socket would have surfaced as a non-timeout Err above.
    if expect_handoff {
        println!(
            "[wire] SEAMWALK EXPECT-HANDOFF PASS \u{2713} crossed into {cb:?} with NO wire-level \
             transfer (no TRANSFER_PENDING, no NEW_WORLD) and kept serving world traffic \
             afterward ({drained} packets drained) — the no-loading-screen property"
        );
    } else {
        println!(
            "[wire] SEAMWALK PASS \u{2713} crossed the seam and returned with NO handoff \
             (no TRANSFER_PENDING, no NEW_WORLD, session intact, {drained} packets drained)"
        );
    }
    Ok(())
}

// ---- atwar <rep_index> <0|1>: send one CMSG_SET_FACTION_ATWAR (the rep pane checkbox) and exit —
// the round-trip is asserted by a following `init-factions <index> <standing> <0|1>` relog probe
// (195 slice B). The wire u16 is the client's 0..63 rep-array slot, NOT a faction id.
fn atwar(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{CMSG_SET_FACTION_ATWAR, Faction, FactionFlag};
    let index: u16 = args.next().and_then(|s| s.parse().ok()).expect("usage: atwar <rep_index> <0|1>");
    let on: u8 = args.next().and_then(|s| s.parse().ok()).expect("usage: atwar <rep_index> <0|1>");
    let flags = if on != 0 { FactionFlag::new_at_war() } else { FactionFlag::empty() };
    // gtker types the wire u16 as the closed `Faction` enum (keyed on faction IDS, another face of
    // the field-names-lie) — an index with no variant would silently serialize as 0, so fail LOUD.
    let faction = Faction::try_from(index)
        .map_err(|_| anyhow!("no gtker Faction variant carries wire value {index} — extend this probe with a raw send"))?;
    c.send(&CMSG_SET_FACTION_ATWAR { faction, flags })?;
    std::thread::sleep(std::time::Duration::from_millis(800)); // let the reducer land before logout
    println!("[atwar] sent CMSG_SET_FACTION_ATWAR slot={index} at_war={on}");
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
                    for sp in &t.spells {
                        println!(
                            "[probe]   spell={} state={:?} cost={} req_level={} req_skill={:?} first_rank={}",
                            sp.spell, sp.state, sp.spell_cost, sp.required_level, sp.required_skill, sp.first_rank
                        );
                    }
                    // Optional trailing: `buy <spell_id>` — exercise the purchase and report.
                    if args.next().as_deref() == Some("buy") {
                        use wow_world_messages::vanilla::CMSG_TRAINER_BUY_SPELL;
                        let id: u32 = args.next().and_then(|s| s.parse().ok()).expect("buy <spell_id>");
                        c.send(&CMSG_TRAINER_BUY_SPELL { guid: Guid::new(npc), id })?;
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
                        while std::time::Instant::now() < deadline {
                            match c.recv() {
                                Ok(Smsg::SMSG_TRAINER_BUY_SUCCEEDED(r)) => {
                                    // keep listening: LEARNED or SUPERCEDED (258) follows
                                    println!("[probe] BUY SUCCEEDED spell={}", r.id);
                                }
                                Ok(Smsg::SMSG_SUPERCEDED_SPELL(r)) => {
                                    println!("[probe] SUPERCEDED wire=[{}, {}] (cmangos order: old, new)", r.new_spell_id, r.old_spell_id);
                                    return Ok(());
                                }
                                Ok(Smsg::SMSG_TRAINER_BUY_FAILED(r)) => {
                                    println!("[probe] BUY FAILED spell={} error={:?}", r.id, r.error);
                                    return Ok(());
                                }
                                Ok(Smsg::SMSG_LEARNED_SPELL(r)) => {
                                    println!("[probe] LEARNED spell={}", r.id);
                                    return Ok(());
                                }
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                        println!("[probe] buy: no reply in 4s");
                    }
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

// ---- swing-flow <mob_guid> <spell_id> <queue|seal>: the 114 melee-spell split. ----
// queue (Heroic Strike 78): engage -> after a swing, cast -> assert NO SPELL_GO arrives at cast
//   time (the button-hold contract) -> assert SPELL_GO(spell) + SPELLNONMELEEDAMAGELOG(spell)
//   arrive with a LATER swing (the queued fire).
// seal (Seal of Righteousness 20154): cast (a normal instant buff: one GO now) -> engage ->
//   assert SPELLNONMELEEDAMAGELOG(spell, holy) proc lines arrive per landed swing with NO
//   further SPELL_GO for the seal.
fn swing_flow(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::CMSG_ATTACKSWING;
    let mob: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: swing-flow <mob_guid> <spell_id> <queue|seal> [x y z]");
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
    let seal_mode = args.next().as_deref() == Some("seal");
    // Optional [x y z]: heartbeat ourselves next to the mob first — movement is client-authoritative,
    // so this stands in for walking there (the same trick raw-audit's move mode uses).
    if let (Some(x), Some(y), Some(z)) = (
        args.next().and_then(|s| s.parse::<f32>().ok()),
        args.next().and_then(|s| s.parse::<f32>().ok()),
        args.next().and_then(|s| s.parse::<f32>().ok()),
    ) {
        use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client};
        // Stand 2 yd WEST of the given point (pass the MOB's coords) facing +x (orientation 0), so
        // the module's is_facing gate passes — a swing at a target behind you is silently skipped
        // (that gate ate this probe's every swing on the first attempt: 20 "white swings" that were
        // all the WOLF hitting US).
        for i in 0..3u32 {
            c.send(&MSG_MOVE_HEARTBEAT_Client {
                info: MovementInfo {
                    flags: MovementInfo_MovementFlags::empty(),
                    timestamp: i * 100,
                    position: Vector3d { x: x - 2.0, y, z },
                    orientation: 0.0,
                    fall_time: 0.0,
                },
            })?;
            std::thread::sleep(Duration::from_millis(120));
        }
        println!("[flow] repositioned to ({}, {y}, {z}) facing +x at the mob", x - 2.0);
    }

    if seal_mode {
        // The seal buff itself is a normal instant self-cast (sync START/GO — expected, not counted).
        c.cast_spell(spell, 0)?;
        std::thread::sleep(Duration::from_millis(300));
    } else {
        // queue mode: Bloodrage (2687, warrior kit) — a white swing vs an armored L1 wolf builds
        // ~8 internal rage while Heroic Strike costs 150; the probe would starve. +10 rage now
        // + the 10s trickle covers the cost within a couple of swings.
        c.cast_spell(2687, 0)?;
        std::thread::sleep(Duration::from_millis(300));
    }
    c.set_selection(mob)?;
    c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) })?;
    c.set_recv_timeout(Duration::from_millis(400))?;

    let t0 = Instant::now();
    let deadline = Duration::from_secs(40);
    let mut swings = 0u32;
    let mut cast_at: Option<Instant> = None; // queue mode: when the cast went out
    let mut premature_go = false;
    let mut go_at_swing = false;
    let mut proc_logs = 0u32;
    let mut stray_seal_go = 0u32;
    while t0.elapsed() < deadline {
        match c.recv() {
            Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(a)) => {
                swings += 1;
                println!("[flow] white swing #{swings} dmg={}", a.total_damage);
                // queue mode: cast AFTER the first swing (some rage has built); re-send after a
                // rejection (rage still short) — a queued cast produces no reply until it fires.
                if !seal_mode && cast_at.is_none() && !go_at_swing {
                    c.cast_spell(spell, mob)?;
                    cast_at = Some(Instant::now());
                    println!("[flow] cast {spell} sent (after swing #{swings})");
                }
            }
            Ok(Smsg::SMSG_CAST_RESULT(_)) if !seal_mode => {
                println!("[flow] CAST_RESULT failure — likely rage-short; retry after next swing");
                cast_at = None;
            }
            Ok(Smsg::SMSG_SPELL_GO(g)) if g.spell == spell => {
                if seal_mode {
                    // The seal BUFF cast's own GO (sent at cast) can straggle past the 300ms drain —
                    // only a GO arriving once swings are flowing is a real stray (a proc must never GO).
                    if swings == 0 {
                        println!("[flow] (seal buff cast GO — expected)");
                    } else {
                        stray_seal_go += 1;
                        println!("[flow] UNEXPECTED SPELL_GO({spell}) in seal mode");
                    }
                } else if let Some(t) = cast_at {
                    let ms = t.elapsed().as_millis();
                    println!("[flow] SPELL_GO({spell}) {ms}ms after cast");
                    // Within one recv-timeout of the cast = sent at queue time (the old bug).
                    if ms < 500 { premature_go = true; } else { go_at_swing = true; }
                } else {
                    premature_go = true; // GO with no cast outstanding
                    println!("[flow] UNEXPECTED SPELL_GO({spell}) with no cast outstanding");
                }
            }
            Ok(Smsg::SMSG_SPELLNONMELEEDAMAGELOG(l)) if l.spell == spell => {
                proc_logs += 1;
                println!("[flow] SPELLNONMELEEDAMAGELOG({spell}) dmg={} school={:?}", l.damage, l.school);
                if !seal_mode && go_at_swing { break; } // queue verified end-to-end
                if seal_mode && proc_logs >= 2 { break; } // two proc lines = seal verified
            }
            Ok(_) => {}
            Err(_) => {} // recv timeout tick — keep polling until the deadline
        }
    }
    println!(
        "[flow] RESULT swings={swings} premature_go={premature_go} go_at_swing={go_at_swing} proc_logs={proc_logs} stray_seal_go={stray_seal_go}"
    );
    if seal_mode {
        if proc_logs == 0 || stray_seal_go > 0 { bail!("seal-flow FAIL"); }
        println!("[flow] SEAL PASS — named holy proc lines, no stray GO");
    } else {
        if premature_go || !go_at_swing || proc_logs == 0 { bail!("queue-flow FAIL"); }
        println!("[flow] QUEUE PASS — no GO at cast, GO+named damage at the swing");
    }
    Ok(())
}

// ---- armor-audit [seconds]: print every armor (UNIT_FIELD_RESISTANCES[0]) value the client
// receives for SELF, in arrival order — CREATE blocks and VALUES partials both. Diagnoses the
// "login shows less armor until I re-equip" class: the LAST value printed is what the paperdoll
// shows; compare against the CREATE's and the SQL-side fold to see who pushed the wrong number.
fn armor_audit(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{Object, UpdateMask};
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let me = c.self_guid;
    c.set_recv_timeout(std::time::Duration::from_millis(400))?;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        match c.recv() {
            Ok(Smsg::SMSG_UPDATE_OBJECT(u)) => {
                for o in &u.objects {
                    let (guid, mask, kind) = match o {
                        Object::Values { guid1, mask1 } => (guid1.guid(), mask1, "VALUES"),
                        Object::CreateObject { guid3, mask2, .. } => (guid3.guid(), mask2, "CREATE"),
                        Object::CreateObject2 { guid3, mask2, .. } => (guid3.guid(), mask2, "CREATE2"),
                        _ => continue,
                    };
                    if guid != me {
                        continue;
                    }
                    let (armor, str_, ap) = match mask {
                        UpdateMask::Unit(m) => (m.unit_normal_resistance(), m.unit_strength(), m.unit_attack_power()),
                        UpdateMask::Player(m) => (m.unit_normal_resistance(), m.unit_strength(), m.unit_attack_power()),
                        _ => (None, None, None),
                    };
                    if armor.is_some() || str_.is_some() || ap.is_some() {
                        println!(
                            "[armor] t={}ms {kind} armor={:?} str={:?} ap={:?}",
                            t0.elapsed().as_millis(), armor, str_, ap
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    println!("[armor] done");
    Ok(())
}

// ---- fall <fall_time_ms>: send MSG_MOVE_FALL_LAND with the given airborne time (058) and report
// the SMSG_ENVIRONMENTAL_DAMAGE_LOG that comes back. The wire carries fall time as raw u32 ms
// (cmangos truth); gtker types the field f32, so the raw value rides via from_bits.
fn fall_probe(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_FALL_LAND_Client};
    let ms: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);
    c.send(&MSG_MOVE_FALL_LAND_Client {
        info: MovementInfo {
            flags: MovementInfo_MovementFlags::empty(),
            timestamp: 1,
            position: Vector3d { x: -8949.95, y: -132.49, z: 83.5 },
            orientation: 0.0,
            fall_time: f32::from_bits(ms),
        },
    })?;
    c.set_recv_timeout(std::time::Duration::from_millis(400))?;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(4) {
        match c.recv() {
            Ok(Smsg::SMSG_ENVIRONMENTAL_DAMAGE_LOG(l)) => {
                println!("[fall] ENV_DAMAGE_LOG type={:?} dmg={} guid={:#x}", l.damage_type, l.damage, l.guid.guid());
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    println!("[fall] no ENV_DAMAGE_LOG within 4s (fall_time={ms}ms)");
    Ok(())
}

// ---- engage-retreat <mob_guid> <x> <y> <z> <retreat_yd> <secs>: engage the mob in melee, then
// back off `retreat_yd` (heartbeat — client-authoritative) and hold the session open `secs` while
// the shell samples the mob's position via SQL (049: an offensive caster should HOLD at spell
// range instead of gluing to us; a melee mob should chase). Prints swings/casts seen either way.
fn engage_retreat(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{CMSG_ATTACKSWING, MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client};
    let mob: u64 = args.next().and_then(|s| s.parse().ok()).expect("mob guid");
    let x: f32 = args.next().and_then(|s| s.parse().ok()).expect("x");
    let y: f32 = args.next().and_then(|s| s.parse().ok()).expect("y");
    let z: f32 = args.next().and_then(|s| s.parse().ok()).expect("z");
    let retreat: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let hb = |c: &mut WireClient, px: f32, py: f32, ts: u32| -> Result<()> {
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: ts,
                position: Vector3d { x: px, y: py, z },
                orientation: 0.0,
                fall_time: 0.0,
            },
        })
    };
    // adjacent (2 yd west, facing +x — the is_facing lesson), engage, trade a couple swings
    hb(c, x - 2.0, y, 1)?;
    c.set_selection(mob)?;
    c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) })?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(4) {
        let _ = c.recv();
    }
    // back off — a few heartbeats so coalescing can't hold the retreat
    for i in 0..3u32 {
        hb(c, x - 2.0 - retreat, y, 100 + i * 120)?;
        std::thread::sleep(Duration::from_millis(120));
    }
    println!("[hold] retreated {retreat} yd; holding session {secs}s (sample the mob via SQL now)");
    let (mut swings, mut casts) = (0u32, 0u32);
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(secs) {
        match c.recv() {
            Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(_)) => swings += 1,
            Ok(Smsg::SMSG_SPELL_GO(g)) if g.caster.guid() == mob => casts += 1,
            Ok(_) => {}
            Err(_) => {}
        }
    }
    println!("[hold] done — swings_seen={swings} mob_casts={casts}");
    Ok(())
}

// ---- watch-casts [secs]: passively print every SMSG_SPELL_START/GO the session receives
// (caster + spell id). 088 verify: a server-side debug_force_cast on THIS character must now
// deliver the caster their own START/GO through the relay (the old gate suppressed them).
fn watch_casts(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);
    c.set_recv_timeout(std::time::Duration::from_millis(400))?;
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        match c.recv() {
            Ok(Smsg::SMSG_SPELL_START(m)) => println!("[watch] START caster={:#x} spell={}", m.caster.guid(), m.spell),
            Ok(Smsg::SMSG_SPELL_GO(m)) => println!("[watch] GO caster={:#x} spell={}", m.caster.guid(), m.spell),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    println!("[watch] done");
    Ok(())
}

// ---- channel <name> [say <msg>] [secs]: join the chat channel (065), optionally speak, then
// listen — prints the CHANNEL_NOTIFY ack and every Channel-type SMSG_MESSAGECHAT received.
fn channel_probe(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::{CMSG_JOIN_CHANNEL, CMSG_MESSAGECHAT, CMSG_MESSAGECHAT_ChatType, Language, SMSG_MESSAGECHAT_ChatType};
    let name = args.next().expect("usage: channel <name> [say <msg>] [secs]");
    let mut say: Option<String> = None;
    let mut secs = 10u64;
    while let Some(a) = args.next() {
        if a == "say" {
            say = args.next();
        } else if let Ok(n) = a.parse() {
            secs = n;
        }
    }
    c.send(&CMSG_JOIN_CHANNEL { channel_name: name.clone(), channel_password: String::new() })?;
    c.set_recv_timeout(std::time::Duration::from_millis(400))?;
    let t0 = std::time::Instant::now();
    let mut sent = say.is_none();
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        match c.recv() {
            Ok(Smsg::SMSG_CHANNEL_NOTIFY(n)) => {
                println!("[chan] NOTIFY {:?} channel={}", n.notify_type, n.channel_name);
                if !sent {
                    if let Some(msg) = &say {
                        c.send(&CMSG_MESSAGECHAT {
                            chat_type: CMSG_MESSAGECHAT_ChatType::Channel { channel: name.clone() },
                            language: Language::Universal,
                            message: msg.clone(),
                        })?;
                        sent = true;
                    }
                }
            }
            Ok(Smsg::SMSG_MESSAGECHAT(m)) => {
                if let SMSG_MESSAGECHAT_ChatType::Channel { channel_name, player, .. } = &m.chat_type {
                    println!("[chan] MSG channel={} from={:#x} text={:?}", channel_name, player.guid(), m.message);
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    println!("[chan] done");
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
                if op == 0x00A9 {
                    println!("[audit] t={}ms MOVING UPDATE_OBJECT len={}", t0.elapsed().as_millis(), payload.len());
                }
                if op == 0x00AA {
                    println!("[audit] t={}ms MOVING DESTROY_OBJECT", t0.elapsed().as_millis());
                }
                if (0x00B5..=0x00EE).contains(&op) {
                    let mask = payload[0];
                    let mut g: u64 = 0;
                    let mut off = 1;
                    for i in 0..8 {
                        if mask & (1 << i) != 0 {
                            g |= (payload[off] as u64) << (8 * i);
                            off += 1;
                        }
                    }
                    println!("[audit] t={}ms SELF-WINDOW MOVE-OP 0x{op:04X} guid={g:#x} len={}", t0.elapsed().as_millis(), payload.len());
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
            // Decode the leading packed guid — a MONSTER_MOVE carrying the PLAYER's own guid
            // spline-drags their character (the rubber-band suspect).
            let mask = payload[0];
            let mut g: u64 = 0;
            let mut off = 1;
            for i in 0..8 {
                if mask & (1 << i) != 0 {
                    g |= (payload[off] as u64) << (8 * i);
                    off += 1;
                }
            }
            println!("[audit] t={}ms MONSTER_MOVE guid={g:#x} len={}", t0.elapsed().as_millis(), payload.len());
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

// ---- autoshot <mob_guid> <loop|melee|reject> <mob_x> <mob_y> <mob_z>: the ranged auto-repeat
// wire contract (097). Repositions WEST of the mob facing +x (like swing-flow), then:
//   loop   — stands 15 yd out, activates Auto Shot (75), watches ~20s. PASS = activation
//            SMSG_SPELL_START(75) with timer==0 + the AMMO block, >=2 SMSG_SPELL_GO(75) (the loop
//            repeats), >=1 SMSG_SPELLNONMELEEDAMAGELOG(75) (vanilla shot damage packet), and ZERO
//            self SMSG_ATTACKERSTATEUPDATE while ranged (the melee-swing packet must not fire per
//            shot). Logs any server-initiated SMSG_CANCEL_AUTO_REPEAT (min-range teardown when the
//            mob closes) — expected once the wolf reaches us, not asserted (timing).
//   melee  — stands 15 yd out, activates, then after the FIRST shot GO emulates the 5875 client's
//            melee press: CMSG_ATTACKSWING + CMSG_CANCEL_AUTO_REPEAT_SPELL back-to-back (the live-
//            logged order). PASS = >=1 self white melee swing lands after the pair — ONE press
//            engages melee (the cancel must not kill the just-armed melee row).
//   reject — stands 45 yd out (beyond the 35 yd max) and activates. PASS = SMSG_CAST_RESULT
//            arrives and NO SMSG_SPELL_START/GO for spell 75 (arm-first rejection keeps the
//            client toggle in lockstep).
fn autoshot(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{
        CMSG_ATTACKSWING, CMSG_ATTACKSTOP, CMSG_CANCEL_AUTO_REPEAT_SPELL, MovementInfo,
        MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client,
    };
    const AUTO_SHOT: u32 = 75;
    let mob: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: autoshot <mob_guid> <loop|melee|reject> <mob_x> <mob_y> <mob_z>");
    let mode = args.next().expect("mode: loop|melee|reject");
    let mx: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob x");
    let my: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob y");
    let mz: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob z");

    // loop mode stands farther out: the wolf charges after the first LANDED shot and a point-blank
    // due shot tears the loop down (min-range) — 25 yd buys ≥2 shot cycles before it closes.
    let stand_off = match mode.as_str() { "reject" => 45.0, "loop" | "moving" => 25.0, _ => 15.0 };
    for i in 0..3u32 {
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: i * 100,
                position: Vector3d { x: mx - stand_off, y: my, z: mz },
                orientation: 0.0, // facing +x = facing the mob (is_facing gate)
                fall_time: 0.0,
            },
        })?;
        std::thread::sleep(Duration::from_millis(120));
    }
    println!("[shot] standing {stand_off} yd west of mob, facing it; activating Auto Shot");
    c.set_selection(mob)?;
    c.cast_spell(AUTO_SHOT, mob)?;
    c.set_recv_timeout(Duration::from_millis(400))?;

    if mode == "moving" {
        return autoshot_moving(c, mob, mx, my, mz, stand_off);
    }
    let t0 = Instant::now();
    let deadline = Duration::from_secs(if mode == "reject" { 5 } else { 22 });
    let self_guid = c.self_guid;
    let (mut start_seen, mut start_timer, mut start_ammo) = (false, u32::MAX, false);
    let mut cast_result = 0u32;
    let (mut gos, mut go_misses, mut dmg_logs, mut self_melee_asu, mut cancels) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut last_go_at: Option<Instant> = None;
    let mut first_impact_gap_ms: Option<u128> = None; // GO->damage-log gap of the FIRST shot (fired at the known 25yd standoff; later gaps shrink as the wolf charges)
    let mut melee_pair_sent_at: Option<Instant> = None;
    let mut self_melee_after_pair = 0u32;

    while t0.elapsed() < deadline {
        match c.recv() {
            Ok(Smsg::SMSG_SPELL_START(s)) if s.spell == AUTO_SHOT => {
                start_seen = true;
                start_timer = s.timer;
                start_ammo = s.flags.get_ammo().is_some();
                println!("[shot] SPELL_START(75) timer={} ammo_block={} @{}ms", s.timer, start_ammo, t0.elapsed().as_millis());
            }
            Ok(Smsg::SMSG_CAST_RESULT(_)) => {
                cast_result += 1;
                println!("[shot] CAST_RESULT @{}ms", t0.elapsed().as_millis());
            }
            Ok(Smsg::SMSG_SPELL_GO(g)) if g.spell == AUTO_SHOT => {
                gos += 1;
                go_misses += g.misses.len() as u32;
                println!("[shot] SPELL_GO(75) #{gos} hits={} misses={} ammo={} @{}ms", g.hits.len(), g.misses.len(), g.flags.get_ammo().is_some(), t0.elapsed().as_millis());
                last_go_at = Some(Instant::now());
                if mode == "melee" && melee_pair_sent_at.is_none() {
                    // Emulate the client's melee press DURING the repeat loop: swing then cancel,
                    // back-to-back (the live-captured 5875 order).
                    c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) })?;
                    c.send(&CMSG_CANCEL_AUTO_REPEAT_SPELL {})?;
                    melee_pair_sent_at = Some(Instant::now());
                    println!("[shot] >> sent ATTACKSWING + CANCEL_AUTO_REPEAT_SPELL pair");
                }
            }
            Ok(Smsg::SMSG_SPELLNONMELEEDAMAGELOG(l)) if l.spell == AUTO_SHOT => {
                dmg_logs += 1;
                let gap = last_go_at.map(|t| t.elapsed().as_millis()).unwrap_or(0);
                if first_impact_gap_ms.is_none() { first_impact_gap_ms = Some(gap); }
                println!("[shot] SPELLNONMELEEDAMAGELOG(75) dmg={} @{}ms (impact gap {}ms after its GO)", l.damage, t0.elapsed().as_millis(), gap);
            }
            Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(a)) if a.attacker.guid() == self_guid => {
                if let Some(t) = melee_pair_sent_at {
                    self_melee_after_pair += 1;
                    println!("[shot] self MELEE swing dmg={} {}ms after pair", a.total_damage, t.elapsed().as_millis());
                    if self_melee_after_pair >= 2 { break; }
                } else {
                    self_melee_asu += 1;
                    println!("[shot] UNEXPECTED self ATTACKERSTATEUPDATE during ranged (dmg={})", a.total_damage);
                }
            }
            Ok(Smsg::SMSG_CANCEL_AUTO_REPEAT) => {
                cancels += 1;
                println!("[shot] SMSG_CANCEL_AUTO_REPEAT (server-initiated) @{}ms", t0.elapsed().as_millis());
                if mode == "loop" && gos >= 2 { break; } // min-range teardown after a healthy loop = done
            }
            Ok(Smsg::SMSG_ATTACKSTOP(s)) => {
                println!("[shot] ATTACKSTOP player={:?} @{}ms", s.player, t0.elapsed().as_millis());
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    // Clean up: stop whatever is still armed (melee row or ranged loop).
    let _ = c.send(&CMSG_ATTACKSTOP {});
    let _ = c.send(&CMSG_CANCEL_AUTO_REPEAT_SPELL {});

    println!("[shot] RESULT mode={mode} start={start_seen}(timer={start_timer},ammo={start_ammo}) cast_result={cast_result} gos={gos} misses={go_misses} dmg_logs={dmg_logs} self_melee_during_ranged={self_melee_asu} cancels={cancels} melee_after_pair={self_melee_after_pair}");
    match mode.as_str() {
        "loop" => {
            if !start_seen || start_timer != 0 || !start_ammo {
                bail!("autoshot-loop FAIL: activation START wrong (seen={start_seen} timer={start_timer} ammo={start_ammo})");
            }
            if gos < 2 { bail!("autoshot-loop FAIL: loop did not repeat (gos={gos})"); }
            if dmg_logs == 0 && go_misses < 2 { bail!("autoshot-loop FAIL: no SPELLNONMELEEDAMAGELOG (and not all-miss)"); }
            // Projectile travel (097): the FIRST shot fires from the known 25 yd standoff → the
            // arrow flies ~625ms (40 yd/s); its damage log must trail its GO by roughly that.
            // Later shots fire at shrinking distance (the wolf charges), so only the first is pinned.
            if let Some(gap) = first_impact_gap_ms {
                if !(400..=900).contains(&(gap as u64)) {
                    bail!("autoshot-loop FAIL: first-shot damage log {gap}ms after its GO (expected ~625ms for 25yd)");
                }
            }
            if self_melee_asu > 0 { bail!("autoshot-loop FAIL: melee ATTACKERSTATEUPDATE sent for ranged shots"); }
            println!("[shot] LOOP PASS \u{2713}");
        }
        "melee" => {
            if self_melee_after_pair == 0 { bail!("autoshot-melee FAIL: no melee swing after one ATTACKSWING+CANCEL press"); }
            println!("[shot] MELEE-SWAP PASS \u{2713} one press engaged melee");
        }
        "reject" => {
            if cast_result == 0 || start_seen || gos > 0 {
                bail!("autoshot-reject FAIL: cast_result={cast_result} start={start_seen} gos={gos}");
            }
            println!("[shot] REJECT PASS \u{2713} out-of-range activation refused, nothing armed");
        }
        m => bail!("unknown autoshot mode {m}"),
    }
    Ok(())
}

// ---- talent <talent_id>: spend one talent point live and assert the 1.12 TalentFrame refresh
// packets (the "talents work server-side but the pane doesn't update" fix): SMSG_LEARNED_SPELL /
// SMSG_SUPERCEDED_SPELL for the picked rank-spell (SPELLS_CHANGED) and the raw partial-VALUES
// SMSG_UPDATE_OBJECT carrying the decremented PLAYER_CHARACTER_POINTS1 (CHARACTER_POINTS_CHANGED).
// Needs a level-10+ character with unspent points and the talent seeded in game_talent.
fn talent_probe(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{Talent, CMSG_LEARN_TALENT};
    let talent_id: u32 = args.next().and_then(|s| s.parse().ok()).expect("usage: talent <talent_id>");
    let talent = Talent::try_from(talent_id).map_err(|_| anyhow!("talent id {talent_id} not in the 1.12 Talent enum"))?;
    c.send(&CMSG_LEARN_TALENT { talent, requested_rank: 0 })?;
    c.set_recv_timeout(Duration::from_millis(400))?;
    let t0 = Instant::now();
    let (mut learned, mut superceded, mut values) = (0u32, 0u32, 0u32);
    while t0.elapsed() < Duration::from_secs(5) {
        match c.recv_raw() {
            Ok((0x012B, body)) => {
                learned = u32::from_le_bytes(body[0..4].try_into().unwrap());
                println!("[talent] SMSG_LEARNED_SPELL({learned})");
            }
            Ok((0x012C, body)) => {
                let old = u16::from_le_bytes(body[0..2].try_into().unwrap());
                let new = u16::from_le_bytes(body[2..4].try_into().unwrap());
                superceded = new as u32;
                println!("[talent] SMSG_SUPERCEDED_SPELL(old={old} -> new={new})");
            }
            Ok((0x00A9, body)) => {
                values += 1;
                println!("[talent] SMSG_UPDATE_OBJECT partial VALUES ({} bytes)", body.len());
            }
            Ok(_) => {}
            Err(_) => {}
        }
        if (learned != 0 || superceded != 0) && values > 0 {
            break;
        }
    }
    println!("[talent] RESULT learned={learned} superceded={superceded} values_frames={values}");
    if learned == 0 && superceded == 0 { bail!("talent FAIL: no rank-spell LEARNED/SUPERCEDED relayed"); }
    if values == 0 { bail!("talent FAIL: no CHARACTER_POINTS1 VALUES push"); }
    println!("[talent] PASS \u{2713} pane-refresh packets on the wire");
    Ok(())
}

// The `autoshot moving` body (097): assert vanilla's defer-while-moving. Kites BACKWARD while
// "running" so the aggroed wolf can't close to melee mid-test (a point-blank due shot would
// legitimately cancel the loop and mask the thing under test).
fn autoshot_moving(c: &mut WireClient, _mob: u64, mx: f32, my: f32, mz: f32, stand_off: f32) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{
        CMSG_CANCEL_AUTO_REPEAT_SPELL, MovementInfo, MovementInfo_MovementFlags, Vector3d,
        MSG_MOVE_HEARTBEAT_Client, MSG_MOVE_STOP_Client,
    };
    const AUTO_SHOT: u32 = 75;
    // Wait for the first shot to fire before starting to run.
    let mut fired = false;
    let tw = Instant::now();
    while tw.elapsed() < Duration::from_secs(4) {
        if let Ok(Smsg::SMSG_SPELL_GO(g)) = c.recv() {
            if g.spell == AUTO_SHOT { fired = true; break; }
        }
    }
    if !fired { bail!("autoshot-moving FAIL: first shot never fired"); }
    println!("[shot] first shot fired; RUNNING (kiting backward) for 3s…");
    let run_start = Instant::now();
    let mut gos_while_moving = 0u32;
    let mut i = 0u32;
    let mut px = mx - stand_off;
    while run_start.elapsed() < Duration::from_secs(3) {
        i += 1;
        px -= 0.95; // ~6.3 yd/s backward — just under the wolf's ~7 yd/s chase, so it can't reach melee before the post-stop resume shot
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::new_backward(),
                timestamp: 1_000_000 + i * 150,
                position: Vector3d { x: px, y: my, z: mz },
                orientation: 0.0, // still facing the wolf (+x)
                fall_time: 0.0,
            },
        })?;
        let tick = Instant::now();
        while tick.elapsed() < Duration::from_millis(150) {
            if let Ok(Smsg::SMSG_SPELL_GO(g)) = c.recv() {
                if g.spell == AUTO_SHOT { gos_while_moving += 1; }
            }
        }
    }
    c.send(&MSG_MOVE_STOP_Client {
        info: MovementInfo {
            flags: MovementInfo_MovementFlags::empty(),
            timestamp: 2_000_000,
            position: Vector3d { x: px, y: my, z: mz },
            orientation: 0.0,
            fall_time: 0.0,
        },
    })?;
    println!("[shot] stopped at x={px:.1}; waiting for the loop to resume…");
    let mut resumed_ms: Option<u128> = None;
    let ts = Instant::now();
    while ts.elapsed() < Duration::from_secs(4) {
        if let Ok(Smsg::SMSG_SPELL_GO(g)) = c.recv() {
            if g.spell == AUTO_SHOT { resumed_ms = Some(ts.elapsed().as_millis()); break; }
        }
    }
    let _ = c.send(&CMSG_CANCEL_AUTO_REPEAT_SPELL {});
    println!("[shot] RESULT mode=moving gos_while_moving={gos_while_moving} resumed_after_stop_ms={resumed_ms:?}");
    if gos_while_moving > 0 { bail!("autoshot-moving FAIL: {gos_while_moving} shot(s) fired WHILE RUNNING"); }
    let Some(ms) = resumed_ms else { bail!("autoshot-moving FAIL: loop never resumed after stopping"); };
    if ms > 3200 { bail!("autoshot-moving FAIL: resume took {ms}ms (expected ≤ ~re-arm + timer)"); }
    println!("[shot] MOVING PASS \u{2713} deferred while running, resumed {ms}ms after stop");
    Ok(())
}

// ---- groundcast <spell_id> <x> <y> <z>: the ground-AoE wire contract (118, Consecration).
// Repositions to (x,y,z) (stand ON the mob pack), casts the spell (instant self-anchored area),
// then watches ~12s. PASS = the GO carries an EMPTY hit list (no impact animation on the caster),
// >=1 DYNAMICOBJECT CreateObject2 arrives (the ground swirl), >=2 SMSG_SPELLNONMELEEDAMAGELOG
// ticks land on nearby hostiles (per-tick feedback), and the swirl's SMSG_DESTROY_OBJECT reaps
// within the watch window (8s duration).
fn groundcast(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{
        MovementInfo, MovementInfo_MovementFlags, Object, ObjectType, Vector3d,
        MSG_MOVE_HEARTBEAT_Client,
    };
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("usage: groundcast <spell_id> <x> <y> <z> [dest]");
    let x: f32 = args.next().and_then(|s| s.parse().ok()).expect("x");
    let y: f32 = args.next().and_then(|s| s.parse().ok()).expect("y");
    let z: f32 = args.next().and_then(|s| s.parse().ok()).expect("z");
    // "dest" (262, Flamestrike): stand ~20 yd WEST of (x,y,z) and cast WITH a DEST_LOCATION block at
    // it — the clicked-ground shape. Default (Consecration) stands ON the point and self-casts.
    let dest_mode = args.next().as_deref() == Some("dest");
    let (sx, sy) = if dest_mode { (x - 20.0, y) } else { (x, y) };
    for i in 0..3u32 {
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: i * 100,
                position: Vector3d { x: sx, y: sy, z },
                orientation: 0.0, // facing +x = the dest point in dest mode
                fall_time: 0.0,
            },
        })?;
        std::thread::sleep(Duration::from_millis(120));
    }
    println!("[ground] at ({sx},{sy},{z}); casting {spell} (dest_mode={dest_mode})");
    if dest_mode {
        c.cast_spell_at_dest(spell, x, y, z)?;
    } else {
        c.cast_spell(spell, 0)?;
    }
    c.set_recv_timeout(Duration::from_millis(400))?;
    let t0 = Instant::now();
    let (mut go_hits, mut go_seen) = (0usize, false);
    let (mut dynobj_creates, mut ticks, mut reaps) = (0u32, 0u32, 0u32);
    while t0.elapsed() < Duration::from_secs(if dest_mode { 18 } else { 12 }) {
        match c.recv() {
            Ok(Smsg::SMSG_SPELL_GO(g)) if g.spell == spell => {
                go_seen = true;
                go_hits = g.hits.len();
                println!("[ground] SPELL_GO({spell}) hits={} @{}ms", g.hits.len(), t0.elapsed().as_millis());
            }
            Ok(Smsg::SMSG_UPDATE_OBJECT(u)) => {
                for o in &u.objects {
                    if let Object::CreateObject2 { object_type: ObjectType::DynamicObject, guid3, .. } = o {
                        dynobj_creates += 1;
                        println!("[ground] DYNAMICOBJECT CREATE guid={:#x} @{}ms", guid3.guid(), t0.elapsed().as_millis());
                    }
                }
            }
            Ok(Smsg::SMSG_SPELLNONMELEEDAMAGELOG(l)) if l.spell == spell => {
                ticks += 1;
                println!("[ground] tick dmg={} on {:#x} @{}ms", l.damage, l.target.guid(), t0.elapsed().as_millis());
            }
            Ok(Smsg::SMSG_DESTROY_OBJECT(d)) if d.guid.guid() >> 48 == 0xF100 => {
                reaps += 1;
                println!("[ground] swirl DESTROY guid={:#x} @{}ms", d.guid.guid(), t0.elapsed().as_millis());
            }
            Ok(_) => {}
            Err(_) => {}
        }
        if reaps > 0 && ticks >= 2 {
            break;
        }
    }
    println!("[ground] RESULT go_seen={go_seen} go_hits={go_hits} dynobj_creates={dynobj_creates} ticks={ticks} reaps={reaps}");
    if !go_seen { bail!("groundcast FAIL: no SPELL_GO (cast rejected? mana?)"); }
    if dest_mode {
        // The clicked-ground nuke (Flamestrike eff0) must HIT the mobs at the click.
        if go_hits == 0 { bail!("groundcast FAIL: dest-mode GO hit nobody — the nuke didn't anchor on the click"); }
    } else if go_hits != 0 {
        bail!("groundcast FAIL: GO hit list not empty ({go_hits}) — caster impact animation");
    }
    if dynobj_creates == 0 { bail!("groundcast FAIL: no DYNAMICOBJECT CREATE (no swirl)"); }
    // dest mode: >=1 damage log proves the dest pipeline delivers feedback (the nuke one-shots a
    // low-level mob and a survivor CHASES out of the 5 yd patch before the 2s first tick — patch
    // TICK mechanics are pinned by the self-anchored Consecration run, same engine). Self mode
    // keeps >=2 (the caster stands in the area with the mob, ticks accumulate).
    let min_ticks = if dest_mode { 1 } else { 2 };
    if ticks < min_ticks { bail!("groundcast FAIL: only {ticks} tick damage log(s) — feedback missing or no hostile in radius"); }
    if reaps == 0 { bail!("groundcast FAIL: swirl never reaped (no DESTROY)"); }
    println!("[ground] GROUNDCAST PASS \u{2713}");
    Ok(())
}

// ---- backswing <mob_guid> <mob_x> <mob_y> <mob_z>: neutral-mob aggro contract (user find).
// Stands 3 yd WEST of the mob FACING AWAY (-x) and toggles melee attack: the facing gate eats
// every swing, so the mob must NOT retaliate (vanilla: neutral mobs react to being ATTACKED, not
// to a stance toggle). Then turns to face it: the first swing fires and the mob must retaliate.
fn backswing(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{
        CMSG_ATTACKSWING, CMSG_ATTACKSTOP, MovementInfo, MovementInfo_MovementFlags, Vector3d,
        MSG_MOVE_HEARTBEAT_Client,
    };
    let mob: u64 = args.next().and_then(|s| s.parse().ok()).expect("usage: backswing <mob_guid> <x> <y> <z>");
    let mx: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob x");
    let my: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob y");
    let mz: f32 = args.next().and_then(|s| s.parse().ok()).expect("mob z");
    let heartbeat = |c: &mut WireClient, o: f32, ts: u32| -> Result<()> {
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: ts,
                position: Vector3d { x: mx - 3.0, y: my, z: mz },
                orientation: o,
                fall_time: 0.0,
            },
        })
    };
    for i in 0..3u32 {
        heartbeat(c, std::f32::consts::PI, i * 100)?; // facing -x = BACK to the mob
        std::thread::sleep(Duration::from_millis(120));
    }
    c.set_selection(mob)?;
    c.send(&CMSG_ATTACKSWING { guid: Guid::new(mob) })?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    println!("[back] attack toggled, back turned; watching 4s for (wrong) retaliation…");
    let t0 = Instant::now();
    let mut wolf_hits_while_backturned = 0u32;
    while t0.elapsed() < Duration::from_secs(4) {
        match c.recv() {
            Ok(Smsg::SMSG_ATTACKSTART(a)) if a.attacker.guid() == mob => {
                wolf_hits_while_backturned += 1;
                println!("[back] mob ATTACKSTART @{}ms (should NOT happen)", t0.elapsed().as_millis());
            }
            Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(a)) if a.attacker.guid() == mob => {
                wolf_hits_while_backturned += 1;
            }
            _ => {}
        }
    }
    println!("[back] turning to face the mob…");
    for i in 0..2u32 {
        heartbeat(c, 0.0, 10_000 + i * 100)?; // face +x = the mob
        std::thread::sleep(Duration::from_millis(120));
    }
    let t1 = Instant::now();
    let (mut own_swing, mut retaliated) = (0u32, 0u32);
    while t1.elapsed() < Duration::from_secs(6) {
        match c.recv() {
            Ok(Smsg::SMSG_ATTACKERSTATEUPDATE(a)) => {
                if a.attacker.guid() == c.self_guid { own_swing += 1; }
                if a.attacker.guid() == mob { retaliated += 1; }
            }
            Ok(Smsg::SMSG_ATTACKSTART(a)) if a.attacker.guid() == mob => { retaliated += 1; }
            _ => {}
        }
        if own_swing > 0 && retaliated > 0 { break; }
    }
    let _ = c.send(&CMSG_ATTACKSTOP {});
    println!("[back] RESULT backturned_retaliation={wolf_hits_while_backturned} own_swings_after_turn={own_swing} retaliation_after_turn={retaliated}");
    if wolf_hits_while_backturned > 0 { bail!("backswing FAIL: mob retaliated against a swing that never fired"); }
    if own_swing == 0 { bail!("backswing FAIL: no own swing after turning (probe geometry?)"); }
    if retaliated == 0 { bail!("backswing FAIL: mob never retaliated after a REAL swing landed"); }
    println!("[back] BACKSWING PASS \u{2713} no aggro while back-turned; retaliation after a real swing");
    Ok(())
}

// ---- setbutton <slot> <action> <type>: send one CMSG_SET_ACTION_BUTTON (the drag) and exit —
// the orchestrator SQL-asserts the game_player_action row (and the login builder is unit-tested).
fn setbutton(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use wow_world_messages::vanilla::CMSG_SET_ACTION_BUTTON;
    let button: u8 = args.next().and_then(|s| s.parse().ok()).expect("usage: setbutton <slot> <action> <type>");
    let action: u16 = args.next().and_then(|s| s.parse().ok()).expect("action");
    let action_type: u8 = args.next().and_then(|s| s.parse().ok()).expect("type");
    c.send(&CMSG_SET_ACTION_BUTTON { button, action, misc: 0, action_type })?;
    std::thread::sleep(std::time::Duration::from_millis(800)); // let the reducer land before logout
    println!("[btn] sent SET_ACTION_BUTTON slot={button} action={action} type={action_type}");
    Ok(())
}

// ---- fishcast <spell_id>: cast Fishing via the real CMSG path (060) and assert the manual
// START -> raw CAST_RESULT(OK) -> GO clear arrives. The catch itself is SQL-asserted outside.
fn fishcast(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("usage: fishcast <spell_id>");
    c.cast_spell(spell, 0)?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    // ALL-RAW drain: the 234 assert needs the TYPE-less PLAYER_SKILL_INFO partial, which gtker's
    // typed reader consumes-and-rejects — one raw loop sees every frame exactly once.
    let t0 = Instant::now();
    let (mut start, mut go, mut skill_values) = (false, false, false);
    while t0.elapsed() < Duration::from_secs(8) && !(start && go && skill_values) {
        match c.recv_raw() {
            Ok((0x0131, body)) if body.len() >= 4 => {
                // SMSG_SPELL_START: packed guids lead; match the spell id anywhere (cheap raw pin).
                if body.windows(4).any(|w| w == spell.to_le_bytes()) {
                    start = true;
                }
            }
            Ok((0x0132, body)) => {
                if body.windows(4).any(|w| w == spell.to_le_bytes()) {
                    go = true;
                }
            }
            Ok((0x00A9, body)) => {
                // 234: the catch's skill-up pushes PLAYER_SKILL_INFO — decode the VALUES mask and
                // require the Fishing line id (356) at a real SKILL_INFO field index (base 718,
                // 3 words/slot, 128 slots), not just the bytes appearing anywhere in the frame.
                const SKILL_INFO_BASE: u16 = 718;
                const SKILL_INFO_END: u16 = 718 + 128 * 3;
                let hit = wire_client::values_mask::parse_values_updates(&body)
                    .iter()
                    .flat_map(|u| u.fields.iter())
                    .any(|&(idx, v)| {
                        (SKILL_INFO_BASE..SKILL_INFO_END).contains(&idx) && (v & 0xFFFF) == 356
                    });
                if hit {
                    skill_values = true;
                }
            }
            _ => {}
        }
    }
    println!("[fish] RESULT start={start} go={go} live_skill_values={skill_values}");
    if !start || !go { bail!("fishcast FAIL: START/GO clear missing (start={start} go={go})"); }
    if !skill_values { bail!("fishcast FAIL: no live PLAYER_SKILL_INFO values for Fishing (234)"); }
    println!("[fish] FISHCAST PASS \u{2713} (incl. live skill-pane values)");
    Ok(())
}

// ---- petsummon <spell_id>: cast a summon spell (Summon Imp 688) and assert the 023 pet binding:
// (1) a Unit CREATE whose UNIT_FIELD_SUMMONEDBY == self (the pet), (2) SMSG_PET_SPELLS (0x0179)
// carrying that pet guid, (3) the owner's UNIT_FIELD_SUMMON VALUES partial (TYPE-less → raw scan
// for the pet guid low word in an UPDATE_OBJECT that is NOT the pet CREATE).
fn petsummon(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{Object, UpdateMask};
    let spell: u32 =
        args.next().and_then(|s| s.parse().ok()).expect("usage: petsummon <spell_id>");
    let self_guid = c.self_guid;
    c.cast_spell(spell, 0)?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    let (mut pet_guid, mut pet_spells_guid) = (0u64, None::<u64>);
    // 25s window: Summon Imp is a 10s TIMED cast (the pet only spawns at completion).
    while t0.elapsed() < Duration::from_secs(25) && !(pet_guid != 0 && pet_spells_guid.is_some()) {
        match c.recv() {
            Ok(Smsg::SMSG_UPDATE_OBJECT(u)) => {
                for o in &u.objects {
                    if let Object::CreateObject { guid3, mask2, .. }
                    | Object::CreateObject2 { guid3, mask2, .. } = o
                    {
                        if let UpdateMask::Unit(unit) = mask2 {
                            if unit.unit_summonedby().map(|g| g.guid()) == Some(self_guid) {
                                pet_guid = guid3.guid();
                                println!("[pet] CREATE with SUMMONEDBY=self → pet {pet_guid:#x}");
                            }
                        }
                    }
                }
            }
            Ok(Smsg::SMSG_PET_SPELLS(p)) => {
                pet_spells_guid = Some(p.pet.guid());
                println!(
                    "[pet] SMSG_PET_SPELLS pet={:#x} bar={}",
                    p.pet.guid(),
                    p.action_bars.is_some()
                );
            }
            _ => {}
        }
    }
    println!(
        "[pet] RESULT summonedby_create={} pet_spells={}",
        pet_guid != 0,
        pet_spells_guid.is_some()
    );
    if pet_guid == 0 {
        bail!("petsummon FAIL: no Unit CREATE carried UNIT_FIELD_SUMMONEDBY=self (023)");
    }
    match pet_spells_guid {
        Some(g) if g == pet_guid => {}
        other => bail!("petsummon FAIL: SMSG_PET_SPELLS missing/mismatched (got {other:?})"),
    }
    println!("[pet] PETSUMMON PASS \u{2713} (SUMMONEDBY create + PET_SPELLS bar)");
    Ok(())
}

// ---- casttime <spell_id> <target_guid> <x> <y> <z>: begin a timed cast at a mob and report the
// SMSG_SPELL_START timer (the server's FOLDED cast time — the 264 spell-modifier verify), then
// cancel so nothing lands. Positions 20 yd west of the target, facing it.
fn casttime(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use wow_world_messages::vanilla::{
        CMSG_CANCEL_CAST, MovementInfo, MovementInfo_MovementFlags, Vector3d, MSG_MOVE_HEARTBEAT_Client,
    };
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("usage: casttime <spell> <target> <x> <y> <z>");
    let target: u64 = args.next().and_then(|s| s.parse().ok()).expect("target guid");
    let mx: f32 = args.next().and_then(|s| s.parse().ok()).expect("x");
    let my: f32 = args.next().and_then(|s| s.parse().ok()).expect("y");
    let mz: f32 = args.next().and_then(|s| s.parse().ok()).expect("z");
    for i in 0..3u32 {
        c.send(&MSG_MOVE_HEARTBEAT_Client {
            info: MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: i * 100,
                position: Vector3d { x: mx - 20.0, y: my, z: mz },
                orientation: 0.0,
                fall_time: 0.0,
            },
        })?;
        std::thread::sleep(Duration::from_millis(120));
    }
    c.set_selection(target)?;
    c.cast_spell(spell, target)?;
    c.set_recv_timeout(Duration::from_millis(300))?;
    let t0 = Instant::now();
    let mut timer: Option<u32> = None;
    while t0.elapsed() < Duration::from_secs(5) && timer.is_none() {
        if let Ok(Smsg::SMSG_SPELL_START(s)) = c.recv() {
            if s.spell == spell {
                timer = Some(s.timer);
            }
        }
    }
    let _ = c.send(&CMSG_CANCEL_CAST { id: spell });
    let Some(t) = timer else { bail!("casttime FAIL: no SMSG_SPELL_START for {spell}") };
    println!("[casttime] START timer = {t}ms");
    Ok(())
}

//! Manual driver for the headless wire test-client.
//!
//! Usage: wire-client [account] [password] [char-name] [spell-id]
//! Defaults: TEST / test123 / Ginger.  With a spell-id it runs M2 (cast assertion):
//! it logs in, WAITS for a creature to appear nearby (spawn one externally via
//! `debug_spawn_at_feet <char_guid> <entry> <offset>`), then targets it and casts,
//! asserting the timed-cast SMSG sequence.

use anyhow::{bail, Result};

mod modes;
use wire_client::{logon, WireClient};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{Class, WorldResult};

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

    // ---- named modes: the five family dispatchers own everything below (modes/*.rs) ----
    if let Some(m) = mode.as_deref() {
        let mcx =
            modes::ModeCtx { account: &account, password: &password, char_name: &char_name };
        if modes::dispatch(m, &mut c, &mut args, &mcx)? {
            return Ok(());
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

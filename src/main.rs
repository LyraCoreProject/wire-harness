//! Manual driver for the headless wire test-client.
//!
//! Usage: wire-client [account] [password] [char-name] [spell-id]
//! Defaults: TEST / test123 / Ginger.  With a spell-id it runs M2 (cast assertion):
//! it logs in, WAITS for a creature to appear nearby (spawn one externally via
//! `debug_spawn_at_feet <char_guid> <entry> <offset>`), then targets it and casts,
//! asserting the timed-cast SMSG sequence.

use anyhow::{bail, Result};

mod modes;
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::Class;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let account = args.next().unwrap_or_else(|| "TEST".into());
    let password = args.next().unwrap_or_else(|| "test123".into());
    let char_name = args.next().unwrap_or_else(|| "Ginger".into());
    let mode: Option<String> = args.next();

    // ---- char-select-tier probes (modes/probes.rs): these run BEFORE login_as — they operate
    // at the character-select screen (char-enum-gear, char-delete) and never enter the world. ----
    if let Some(m) = mode.as_deref() {
        if modes::dispatch_charselect(m, &account, &password, &char_name, &mut args)? {
            return Ok(());
        }
    }

    eprintln!("[wire] logon + world handshake as {account} -> char {char_name}…");
    let mut c = WireClient::login_as(&account, &password, &char_name, Class::Warlock)?;
    println!(
        "[wire] M1 OK — in world as guid {} ({} nearby objects)",
        c.self_guid,
        c.seen_guids.len()
    );

    // ---- named modes: the five family dispatchers own everything below (modes/*.rs) ----
    // No mode at all is the M1 login smoke (we already printed M1 OK). A mode NOBODY claims
    // must bail: a silent Ok(()) here turns a renamed/typo'd mode into a green suite entry.
    let Some(mode) = mode else { return Ok(()) };
    let mcx = modes::ModeCtx { account: &account, password: &password, char_name: &char_name };
    if modes::dispatch(&mode, &mut c, &mut args, &mcx)? {
        return Ok(());
    }

    let spell_id: u32 = match mode.parse() {
        Ok(id) => id,
        Err(_) => bail!("unknown mode {mode:?} — no family dispatcher claimed it and it is not an M2 spell-id"),
    };

    // ---- M2: the orchestrator spawns a mob at Ginger's feet (she must be live) and writes
    // its exact guid to a file; we KEEP DRAINING the socket while polling that file, then
    // target + cast + assert. (Blocking on the file without draining would stall the socket
    // and the gateway would drop the connection — so we recv() every loop.) The path is
    // script-owned and REQUIRED — no /tmp default (work-item 161), so concurrent runs can
    // never collide on the same target file.
    let Ok(target_file) = std::env::var("WIRE_TARGET_FILE") else {
        bail!(
            "M2 cast mode requires WIRE_TARGET_FILE=<run-scoped path> (the orchestrator writes \
             the target guid there; there is no /tmp default)"
        );
    };
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

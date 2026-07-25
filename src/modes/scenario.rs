//! Orchestrated SCENARIO modes (work-item 140): quest loop, vendor economy, trainer flow,
//! death/corpse-run -- the wire half of tools/wire-client/test-scenario-*.sh.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::Guid;

use super::{require_path_arg, signal_and_wait_consumed, ModeCtx};

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<bool> {
    match mode {
        "scenario-quest" => scenario_quest(mode, c, args)?,
        "vendor-list" => vendor_list(mode, c, args)?,
        "vendor-buy" => vendor_buy(mode, c, args)?,
        "equip-from" | "unequip-from" => equip_or_unequip(mode, c, args)?,
        "vendor-sell" | "vendor-repair" | "vendor-buyback" => vendor_sell_repair_buyback(mode, c, args)?,
        "vendor-sell-buyback" => vendor_sell_buyback(mode, c, args)?,
        "scenario-train" => scenario_train(mode, c, args)?,
        "scenario-death" => scenario_death(mode, c, args)?,
        _ => return Ok(false),
    }
    Ok(true)
}

// ---- scenario-quest: accept -> kill 2 objective wolves (real swings) -> loot -> turn in ----
// Usage: wire-client TEST test123 Ginger scenario-quest <giver_guid> <quest_entry> <wolf1> <wolf2>
fn scenario_quest(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
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
    match c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_QUESTGIVER_QUEST_DETAILS(d) => Some(d.quest_id),
        _ => None,
    }) {
        Some(id) if id == quest => {}
        Some(id) => bail!("STEP 1 FAIL: QUEST_DETAILS quest_id={id} want {quest}"),
        None => bail!("STEP 1 FAIL: no SMSG_QUESTGIVER_QUEST_DETAILS({quest}) within 5s of hello"),
    }
    println!("[scenario] STEP 1 OK — SMSG_QUESTGIVER_QUEST_DETAILS({quest})");

    // STEP 2: accept. (No dedicated ack SMSG — the orchestrator sql-asserts the log row.)
    c.send(&CMSG_QUESTGIVER_ACCEPT_QUEST { guid: Guid::new(giver), quest_id: quest })?;
    println!("[scenario] STEP 2 OK — CMSG_QUESTGIVER_ACCEPT_QUEST sent");

    // STEP 3+4: melee both wolves down, asserting the two SMSG_QUESTUPDATE_ADD_KILL ticks.
    // The 90s window rides out a fleeing low-health wolf going silent (no swings, no packets)
    // until the leash walks it back into range — recv_for treats those read-timeout lulls as
    // window time, not as terminal (the starved-recv class from 146's suite archaeology).
    for (i, wolf) in [(1u32, wolf1), (2u32, wolf2)] {
        c.set_selection(wolf)?;
        c.send(&CMSG_ATTACKSWING { guid: Guid::new(wolf) })?;
        match c.recv_for(std::time::Duration::from_secs(90), |m| match m {
            Smsg::SMSG_QUESTUPDATE_ADD_KILL(k) if k.quest_id == quest => Some(k.kill_count),
            _ => None,
        }) {
            Some(count) if count == i => {}
            Some(count) => bail!("STEP {} FAIL: ADD_KILL count={count} want {i}", 2 + i),
            None => bail!("STEP {} FAIL: no SMSG_QUESTUPDATE_ADD_KILL({i}/2) within 90s", 2 + i),
        }
        println!("[scenario] STEP {} OK — SMSG_QUESTUPDATE_ADD_KILL {i}/2", 2 + i);
    }

    // STEP 5: loot the second wolf's corpse — money window + release.
    c.send(&CMSG_LOOT { guid: Guid::new(wolf2) })?;
    let gold = c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_LOOT_RESPONSE(l) => Some(l.gold.as_int()),
        _ => None,
    });
    let Some(gold) = gold else { bail!("STEP 5 FAIL: no SMSG_LOOT_RESPONSE within 5s of CMSG_LOOT") };
    if gold == 0 { bail!("STEP 5 FAIL: SMSG_LOOT_RESPONSE.gold == 0 (fixture wolves carry money)"); }
    c.send(&CMSG_LOOT_MONEY {})?;
    c.send(&CMSG_LOOT_RELEASE { guid: Guid::new(wolf2) })?;
    println!("[scenario] STEP 5 OK — looted corpse (gold={gold}) + released");

    // STEP 6: complete -> the giver offers the reward.
    c.send(&CMSG_QUESTGIVER_COMPLETE_QUEST { guid: Guid::new(giver), quest_id: quest })?;
    match c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_QUESTGIVER_OFFER_REWARD(o) => Some(o.quest_id),
        _ => None,
    }) {
        Some(id) if id == quest => {}
        Some(id) => bail!("STEP 6 FAIL: OFFER_REWARD quest_id={id} want {quest}"),
        None => bail!("STEP 6 FAIL: no SMSG_QUESTGIVER_OFFER_REWARD within 5s of COMPLETE_QUEST"),
    }
    println!("[scenario] STEP 6 OK — SMSG_QUESTGIVER_OFFER_REWARD({quest})");

    // STEP 7: choose reward 0 -> QUEST_COMPLETE closes the loop.
    c.send(&CMSG_QUESTGIVER_CHOOSE_REWARD { guid: Guid::new(giver), quest_id: quest, reward: 0 })?;
    match c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_QUESTGIVER_QUEST_COMPLETE(q) => Some(q.quest_id),
        _ => None,
    }) {
        Some(id) if id == quest => {}
        Some(id) => bail!("STEP 7 FAIL: QUEST_COMPLETE quest_id={id} want {quest}"),
        None => bail!("STEP 7 FAIL: no SMSG_QUESTGIVER_QUEST_COMPLETE within 5s of CHOOSE_REWARD"),
    }
    println!("[scenario] STEP 7 OK — SMSG_QUESTGIVER_QUEST_COMPLETE({quest})");
    println!("[wire] SCENARIO-QUEST PASS \u{2713}  details->accept->2 kills->loot->offer->complete");
    Ok(())
}

// ---- vendor micro-modes: single wire actions the vendor orchestrator sequences with sql ----
// vendor-list <vendor> <want_entry> — SMSG_LIST_INVENTORY must include want_entry.
fn vendor_list(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::CMSG_LIST_INVENTORY;
    let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
    let want: u32 = args.next().and_then(|s| s.parse().ok()).expect("want entry");
    c.send(&CMSG_LIST_INVENTORY { guid: Guid::new(vendor) })?;
    let entries = c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_LIST_INVENTORY(l) => Some(l.items.iter().map(|i| i.item).collect::<Vec<u32>>()),
        _ => None,
    });
    let Some(entries) = entries else { bail!("vendor-list: no SMSG_LIST_INVENTORY within 5s") };
    println!("[probe] SMSG_LIST_INVENTORY items={entries:?}");
    if entries.contains(&want) {
        println!("[wire] VENDOR-LIST PASS \u{2713}  vendor stocks {want}");
        return Ok(());
    }
    bail!("vendor-list: {want} not in vendor inventory {entries:?}");
}

// vendor-buy <vendor> <entry> — passes unless SMSG_BUY_FAILED arrives (state asserted via sql).
fn vendor_buy(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::CMSG_BUY_ITEM;
    let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
    let entry: u32 = args.next().and_then(|s| s.parse().ok()).expect("item entry");
    c.send(&CMSG_BUY_ITEM { vendor: Guid::new(vendor), item: entry, amount: 1, unknown1: 0 })?;
    // Negative assertion: ride the whole window; only SMSG_BUY_FAILED fails the probe.
    if let Some(result) = c.recv_for(std::time::Duration::from_secs(3), |m| match m {
        Smsg::SMSG_BUY_FAILED(f) => Some(f.result),
        _ => None,
    }) {
        bail!("vendor-buy: SMSG_BUY_FAILED result={result:?}");
    }
    println!("[wire] VENDOR-BUY PASS \u{2713}  CMSG_BUY_ITEM({entry}) accepted (no SMSG_BUY_FAILED)");
    Ok(())
}

// equip-from <backpack_slot> / unequip-from <equip_slot> — fail on SMSG_INVENTORY_CHANGE_FAILURE.
fn equip_or_unequip(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::{CMSG_AUTOEQUIP_ITEM, CMSG_AUTOSTORE_BAG_ITEM};
    let slot: u8 = args.next().and_then(|s| s.parse().ok()).expect("slot");
    let equipping = mode == "equip-from";
    if equipping {
        c.send(&CMSG_AUTOEQUIP_ITEM { source_bag: 255, source_slot: slot })?;
    } else {
        c.send(&CMSG_AUTOSTORE_BAG_ITEM { source_bag: 255, source_slot: slot, destination_bag: 255 })?;
    }
    // Negative assertion: ride the whole window; only INVENTORY_CHANGE_FAILURE fails the probe.
    if let Some(f) = c.recv_for(std::time::Duration::from_secs(3), |m| match m {
        Smsg::SMSG_INVENTORY_CHANGE_FAILURE(f) => Some(format!("{f:?}")),
        _ => None,
    }) {
        bail!("{}: SMSG_INVENTORY_CHANGE_FAILURE {}", mode, f);
    }
    println!("[wire] {} PASS \u{2713}  slot {slot} accepted (no INVENTORY_CHANGE_FAILURE)", if equipping { "EQUIP-FROM" } else { "UNEQUIP-FROM" });
    Ok(())
}

// vendor-sell <vendor> <item_guid> / vendor-repair <vendor> <item_guid> / vendor-buyback <vendor> <packet_slot>
fn vendor_sell_repair_buyback(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::{BuybackSlot, CMSG_BUYBACK_ITEM, CMSG_REPAIR_ITEM, CMSG_SELL_ITEM};
    let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
    let arg: u64 = args.next().and_then(|s| s.parse().ok()).expect("item guid / packet slot");
    match mode {
        "vendor-sell" => c.send(&CMSG_SELL_ITEM { vendor: Guid::new(vendor), item: Guid::new(arg), amount: 1 })?,
        "vendor-repair" => c.send(&CMSG_REPAIR_ITEM { npc: Guid::new(vendor), item: Guid::new(arg) })?,
        _ => c.send(&CMSG_BUYBACK_ITEM { guid: Guid::new(vendor), slot: BuybackSlot::try_from(arg as u32).map_err(|_| anyhow!("bad buyback slot {arg}"))? })?,
    }
    // These actions have no success SMSG in this gateway (errors are log+ignore server-side);
    // drain briefly so the send flushes, then let the orchestrator's sql assertion be the judge.
    let _ = c.recv_raw_for(std::time::Duration::from_secs(2), |_, _| None::<()>);
    println!("[wire] {} SENT \u{2713}  (state asserted via sql by the orchestrator)", mode.to_uppercase());
    Ok(())
}

// ---- vendor-sell-buyback <vendor> <item_guid> <sold_file> <bought_file>: sell + buyback in ONE
// session (the module clears the buyback ring on logout — vanilla — so the two must share a
// connection). The orchestrator sql-asserts at the two handshake files while the session is
// held open. ----
fn vendor_sell_buyback(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::{BuybackSlot, CMSG_BUYBACK_ITEM, CMSG_SELL_ITEM};
    const USAGE: &str = "vendor-sell-buyback <vendor_guid> <item_guid> <sold_file> <bought_file>";
    let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
    let item: u64 = args.next().and_then(|s| s.parse().ok()).expect("item guid");
    let sold = require_path_arg(args, USAGE, "sold_file")?;
    let bought = require_path_arg(args, USAGE, "bought_file")?;
    c.send(&CMSG_SELL_ITEM { vendor: Guid::new(vendor), item: Guid::new(item), amount: 1 })?;
    signal_and_wait_consumed(c, &sold, 30, "sell: orchestrator never confirmed the sell-state asserts")?;
    println!("[scenario] SELL OK (orchestrator confirmed money + buyback ring)");
    c.send(&CMSG_BUYBACK_ITEM { guid: Guid::new(vendor), slot: BuybackSlot::try_from(69u32).unwrap() })?;
    signal_and_wait_consumed(c, &bought, 30, "buyback: orchestrator never confirmed the buyback-state asserts")?;
    println!("[wire] VENDOR-SELL-BUYBACK PASS \u{2713}  sell + buyback round-trip in one session");
    Ok(())
}

// ---- scenario-train: trainer list -> buy spell -> cast it, asserting the full sequence ----
// Usage: wire-client TEST test123 Ginger scenario-train <trainer_guid> <spell_id> <cast_ms> <ready_file>
// NOTE: <cast_ms> is positional BEFORE <ready_file> — callers must pass both (the ready file is
// required, so an omitted cast_ms would swallow the path and bail on the missing arg).
fn scenario_train(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::{CMSG_TRAINER_BUY_SPELL, CMSG_TRAINER_LIST};
    const USAGE: &str = "scenario-train <trainer_guid> <spell_id> <cast_ms> <ready_file>";
    let trainer: u64 = args.next().and_then(|s| s.parse().ok()).expect("trainer guid");
    let spell: u32 = args.next().and_then(|s| s.parse().ok()).expect("spell id");
    let cast_ms: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1500);
    let ready = require_path_arg(args, USAGE, "ready_file")?;

    // Handshake: the orchestrator damages the caster (so the heal is observable) once we're live.
    signal_and_wait_consumed(c, &ready, 30, "orchestrator never staged the caster")?;

    // STEP 1: the trainer window lists the offering.
    c.send(&CMSG_TRAINER_LIST { guid: Guid::new(trainer) })?;
    let ids = c.recv_for(std::time::Duration::from_secs(5), |m| match m {
        Smsg::SMSG_TRAINER_LIST(t) => Some(t.spells.iter().map(|sp| sp.spell).collect::<Vec<u32>>()),
        _ => None,
    });
    let Some(ids) = ids else { bail!("STEP 1 FAIL: no SMSG_TRAINER_LIST within 5s") };
    println!("[probe] SMSG_TRAINER_LIST spells={ids:?}");
    if !ids.contains(&spell) { bail!("STEP 1 FAIL: trainer list lacks {spell}"); }
    println!("[scenario] STEP 1 OK — SMSG_TRAINER_LIST carries {spell}");

    // STEP 2: buy -> BUY_SUCCEEDED + LEARNED_SPELL.
    c.send(&CMSG_TRAINER_BUY_SPELL { guid: Guid::new(trainer), id: spell })?;
    let (mut ok_buy, mut ok_learn) = (false, false);
    let done = c.recv_for(std::time::Duration::from_secs(5), |m| {
        match m {
            Smsg::SMSG_TRAINER_BUY_SUCCEEDED(b) => { if b.id == spell { ok_buy = true; } }
            Smsg::SMSG_TRAINER_BUY_FAILED(f) => return Some(Err(anyhow!("STEP 2 FAIL: SMSG_TRAINER_BUY_FAILED {f:?}"))),
            Smsg::SMSG_LEARNED_SPELL(l) => { if l.id == spell { ok_learn = true; } }
            _ => {}
        }
        (ok_buy && ok_learn).then_some(Ok(()))
    });
    match done {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => bail!("STEP 2 FAIL: buy={ok_buy} learned={ok_learn} (want both) within 5s"),
    }
    println!("[scenario] STEP 2 OK — SMSG_TRAINER_BUY_SUCCEEDED + SMSG_LEARNED_SPELL({spell})");

    // STEP 3: cast the bought spell at self -> START(cast_ms) then GO.
    c.cast_spell(spell, c.self_guid)?;
    let (mut started, mut went) = (false, false);
    let done = c.recv_for(std::time::Duration::from_secs(8), |m| {
        match m {
            Smsg::SMSG_SPELL_START(sp) => { if sp.timer == cast_ms { started = true; } }
            Smsg::SMSG_SPELL_GO(g) => { if g.spell == spell { went = true; } }
            Smsg::SMSG_CAST_RESULT(r) => return Some(Err(anyhow!("STEP 3 FAIL: cast rejected: {r:?}"))),
            _ => {}
        }
        (started && went).then_some(Ok(()))
    });
    match done {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => bail!("STEP 3 FAIL: START(timer={cast_ms})={started} GO={went} (want both) within 8s"),
    }
    println!("[scenario] STEP 3 OK — SMSG_SPELL_START({cast_ms}) -> SMSG_SPELL_GO({spell})");
    println!("[wire] SCENARIO-TRAIN PASS \u{2713}  list->buy->learn->cast");
    Ok(())
}

// ---- scenario-death: die (orchestrated) -> release -> wait the reclaim delay -> reclaim ----
// Usage: wire-client TEST test123 Ginger scenario-death <corpse_guid> <ready_file> <reclaimed_file>
fn scenario_death(
    _mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    use wow_world_messages::vanilla::CMSG_RECLAIM_CORPSE;
    const USAGE: &str = "scenario-death <corpse_guid> <ready_file> <reclaimed_file>";
    let corpse: u64 = args.next().and_then(|s| s.parse().ok()).expect("corpse guid");
    let ready = require_path_arg(args, USAGE, "ready_file")?;
    let reclaimed = require_path_arg(args, USAGE, "reclaimed_file")?;

    // STEP 1: signal ready; the orchestrator arranges a real death-by-mob, then consumes the file.
    signal_and_wait_consumed(c, &ready, 60, "STEP 1 FAIL: orchestrator never confirmed the death")?;
    println!("[scenario] STEP 1 OK — orchestrator confirmed death (server-side)");

    // STEP 2: release -> the 30s reclaim-delay packet.
    // 270: 12s (was 5s) — the repop reducer + the SMSG_CORPSE_RECLAIM_DELAY relay share the single
    // serialized commit stream, so under full-suite congestion the packet can slide past 5s (same
    // class as the cast_flow impact deadline). 12s absorbs it; the packet arrives near-instant idle.
    c.repop_request()?;
    let delay = c.recv_for(std::time::Duration::from_secs(12), |m| match m {
        Smsg::SMSG_CORPSE_RECLAIM_DELAY(d) => Some(d.delay),
        _ => None,
    });
    let Some(delay) = delay else { bail!("STEP 2 FAIL: no SMSG_CORPSE_RECLAIM_DELAY within 12s of CMSG_REPOP_REQUEST") };
    if delay != std::time::Duration::from_secs(30) { bail!("STEP 2 FAIL: reclaim delay {delay:?}, want 30s"); }
    println!("[scenario] STEP 2 OK — SMSG_CORPSE_RECLAIM_DELAY(30s)");

    // STEP 3: wait out the delay (draining — the predicate never matches), then reclaim the corpse.
    let _ = c.recv_raw_for(std::time::Duration::from_secs(31), |_, _| None::<()>);
    c.send(&CMSG_RECLAIM_CORPSE { guid: Guid::new(corpse) })?;
    println!("[scenario] STEP 3 OK — CMSG_RECLAIM_CORPSE sent after the 30s window");

    // STEP 4: hold the session while the orchestrator sql-asserts the resurrected state.
    signal_and_wait_consumed(c, &reclaimed, 30, "STEP 4 FAIL: orchestrator never confirmed the resurrect")?;
    println!("[scenario] STEP 4 OK — orchestrator confirmed alive-at-50% state");
    println!("[wire] SCENARIO-DEATH PASS \u{2713}  death->release->30s delay->reclaim");
    Ok(())
}

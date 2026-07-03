//! Orchestrated SCENARIO modes (work-item 140): quest loop, vendor economy, trainer flow,
//! death/corpse-run -- the wire half of tools/wire-client/test-scenario-*.sh.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use anyhow::{bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::Guid;

use super::ModeCtx;

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<bool> {
    // ---- scenario-quest: accept -> kill 2 objective wolves (real swings) -> loot -> turn in ----
    // Usage: wire-client TEST test123 Ginger scenario-quest <giver_guid> <quest_entry> <wolf1> <wolf2>
    if mode == "scenario-quest" {
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
                    // A recv read-timeout is NOT terminal here: a low-health wolf FLEES and goes
                    // silent (no swings, no packets) until the leash walks it back into range and
                    // the still-armed auto-attack finishes it — the 90s deadline exists exactly
                    // for that cycle, so ride the quiet out instead of bailing at the first 10s
                    // lull (the starved-recv class from 146's suite archaeology).
                    Err(_) => {}
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
        return Ok(true);
    }

    // ---- vendor micro-modes: single wire actions the vendor orchestrator sequences with sql ----
    // vendor-list <vendor> <want_entry> — SMSG_LIST_INVENTORY must include want_entry.
    if mode == "vendor-list" {
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
                        return Ok(true);
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
    if mode == "vendor-buy" {
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
        return Ok(true);
    }

    // equip-from <backpack_slot> / unequip-from <equip_slot> — fail on SMSG_INVENTORY_CHANGE_FAILURE.
    if matches!(mode, "equip-from" | "unequip-from") {
        use wow_world_messages::vanilla::{CMSG_AUTOEQUIP_ITEM, CMSG_AUTOSTORE_BAG_ITEM};
        let slot: u8 = args.next().and_then(|s| s.parse().ok()).expect("slot");
        let equipping = mode == "equip-from";
        if equipping {
            c.send(&CMSG_AUTOEQUIP_ITEM { source_bag: 255, source_slot: slot })?;
        } else {
            c.send(&CMSG_AUTOSTORE_BAG_ITEM { source_bag: 255, source_slot: slot, destination_bag: 255 })?;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match c.recv() {
                Ok(Smsg::SMSG_INVENTORY_CHANGE_FAILURE(f)) => bail!("{}: SMSG_INVENTORY_CHANGE_FAILURE {:?}", mode, f),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        println!("[wire] {} PASS \u{2713}  slot {slot} accepted (no INVENTORY_CHANGE_FAILURE)", if equipping { "EQUIP-FROM" } else { "UNEQUIP-FROM" });
        return Ok(true);
    }

    // vendor-sell <vendor> <item_guid> / vendor-repair <vendor> <item_guid> / vendor-buyback <vendor> <packet_slot>
    if matches!(mode, "vendor-sell" | "vendor-repair" | "vendor-buyback") {
        use wow_world_messages::vanilla::{BuybackSlot, CMSG_BUYBACK_ITEM, CMSG_REPAIR_ITEM, CMSG_SELL_ITEM};
        let vendor: u64 = args.next().and_then(|s| s.parse().ok()).expect("vendor guid");
        let arg: u64 = args.next().and_then(|s| s.parse().ok()).expect("item guid / packet slot");
        match mode {
            "vendor-sell" => c.send(&CMSG_SELL_ITEM { vendor: Guid::new(vendor), item: Guid::new(arg), amount: 1 })?,
            "vendor-repair" => c.send(&CMSG_REPAIR_ITEM { npc: Guid::new(vendor), item: Guid::new(arg) })?,
            _ => c.send(&CMSG_BUYBACK_ITEM { guid: Guid::new(vendor), slot: BuybackSlot::try_from(arg as u32).map_err(|_| anyhow::anyhow!("bad buyback slot {arg}"))? })?,
        }
        // These actions have no success SMSG in this gateway (errors are log+ignore server-side);
        // drain briefly so the send flushes, then let the orchestrator's sql assertion be the judge.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline { let _ = c.recv_raw(); }
        println!("[wire] {} SENT \u{2713}  (state asserted via sql by the orchestrator)", mode.to_uppercase());
        return Ok(true);
    }

    // ---- vendor-sell-buyback <vendor> <item_guid>: sell + buyback in ONE session (the module
    // clears the buyback ring on logout — vanilla — so the two must share a connection). The
    // orchestrator sql-asserts at the two handshake files while the session is held open. ----
    if mode == "vendor-sell-buyback" {
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
        return Ok(true);
    }

    // ---- scenario-train: trainer list -> buy spell -> cast it, asserting the full sequence ----
    // Usage: wire-client TEST test123 Ginger scenario-train <trainer_guid> <spell_id> <cast_ms>
    if mode == "scenario-train" {
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
        return Ok(true);
    }

    // ---- scenario-death: die (orchestrated) -> release -> wait the reclaim delay -> reclaim ----
    // Usage: wire-client TEST test123 Ginger scenario-death <corpse_guid>
    if mode == "scenario-death" {
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
        return Ok(true);
    }

    Ok(false)
}

//! Bank flow: open, deposit, relog, withdraw, and buy one bank bag slot.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_base::shared::buy_bank_slot_result_vanilla_tbc_wrath::BuyBankSlotResult;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{
    LogoutResult, CMSG_AUTOBANK_ITEM, CMSG_AUTOSTORE_BANK_ITEM, CMSG_BANKER_ACTIVATE,
    CMSG_BUY_BANK_SLOT,
};
use wow_world_messages::Guid;

use super::{require_path_arg, signal_and_wait_consumed, ModeCtx};

const MAIN_BAG: u8 = 255;
const BANK_SLOT_START: u8 = 39;
const BANK_SLOT_END: u8 = 62;
const CARRY_SLOT_START: u8 = 23;
const CARRY_SLOT_END: u8 = 38;
const USAGE: &str = "scenario-bank <banker_guid> <item_guid> <carry_slot> <bank_slot> \
                     <deposited_file> <persisted_file> <withdrawn_file>";

pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    if mode != "scenario-bank" {
        return Ok(false);
    }
    scenario_bank(c, args, mcx)?;
    Ok(true)
}

fn parse_arg<T: std::str::FromStr>(
    args: &mut dyn Iterator<Item = String>,
    name: &str,
) -> Result<T> {
    let value = args
        .next()
        .ok_or_else(|| anyhow!("usage: {USAGE}: missing <{name}>"))?;
    value
        .parse()
        .map_err(|_| anyhow!("usage: {USAGE}: invalid <{name}>: {value:?}"))
}

fn open_bank(c: &mut WireClient, banker: u64, step: u8) -> Result<()> {
    c.send(&CMSG_BANKER_ACTIVATE {
        guid: Guid::new(banker),
    })?;
    match c.recv_for(Duration::from_secs(5), |message| match message {
        Smsg::SMSG_SHOW_BANK(reply) => Some(reply.guid.guid()),
        _ => None,
    }) {
        Some(guid) if guid == banker => Ok(()),
        Some(guid) => bail!("STEP {step} FAIL: SMSG_SHOW_BANK named {guid:#x}, want {banker:#x}"),
        None => bail!("STEP {step} FAIL: no SMSG_SHOW_BANK within 5s of CMSG_BANKER_ACTIVATE"),
    }
}

fn wait_for_item_move(
    c: &mut WireClient,
    item: u64,
    from_slot: u8,
    to_slot: u8,
    step: u8,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match c.recv() {
            Ok(Smsg::SMSG_INVENTORY_CHANGE_FAILURE(failure)) => {
                bail!("STEP {step} FAIL: SMSG_INVENTORY_CHANGE_FAILURE {failure:?}")
            }
            Ok(_) => {}
            Err(error) if wire_client::is_read_timeout(&error) => {}
            Err(error) => {
                if c.own_item_in_slot(from_slot) == Some(0)
                    && c.own_item_in_slot(to_slot) == Some(item)
                {
                    return Ok(());
                }
                return Err(error);
            }
        }
        if c.own_item_in_slot(from_slot) == Some(0) && c.own_item_in_slot(to_slot) == Some(item) {
            return Ok(());
        }
    }
    bail!(
        "STEP {step} FAIL: no complete item move within 5s (item {item:#x}, slot {from_slot} -> {to_slot}; observed source={:?}, destination={:?})",
        c.own_item_in_slot(from_slot),
        c.own_item_in_slot(to_slot)
    )
}

fn scenario_bank(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<()> {
    let banker: u64 = parse_arg(args, "banker_guid")?;
    let item: u64 = parse_arg(args, "item_guid")?;
    let carry_slot: u8 = parse_arg(args, "carry_slot")?;
    let bank_slot: u8 = parse_arg(args, "bank_slot")?;
    let deposited = require_path_arg(args, USAGE, "deposited_file")?;
    let persisted = require_path_arg(args, USAGE, "persisted_file")?;
    let withdrawn = require_path_arg(args, USAGE, "withdrawn_file")?;
    if args.next().is_some() {
        bail!("usage: {USAGE}: unexpected extra argument");
    }
    if banker == 0 || item == 0 {
        bail!("usage: {USAGE}: banker_guid and item_guid must be nonzero");
    }
    if !(CARRY_SLOT_START..=CARRY_SLOT_END).contains(&carry_slot) {
        bail!("usage: {USAGE}: carry_slot must be 23..=38");
    }
    if !(BANK_SLOT_START..=BANK_SLOT_END).contains(&bank_slot) {
        bail!("usage: {USAGE}: bank_slot must be 39..=62");
    }
    if c.own_item_in_slot(carry_slot) != Some(item) {
        bail!(
            "STEP 0 FAIL: login snapshot slot {carry_slot} held {:?}, want item {item:#x}",
            c.own_item_in_slot(carry_slot)
        );
    }
    let character = c.self_guid;

    open_bank(c, banker, 1)?;
    println!("[scenario] STEP 1 OK: SMSG_SHOW_BANK named banker {banker:#x}");

    c.send(&CMSG_AUTOBANK_ITEM {
        bag_index: MAIN_BAG,
        slot_index: carry_slot,
    })?;
    wait_for_item_move(c, item, carry_slot, bank_slot, 2)?;
    signal_and_wait_consumed(
        c,
        &deposited,
        30,
        "STEP 2 FAIL: adapter did not confirm the durable deposit",
    )?;
    println!("[scenario] STEP 2 OK: item {item:#x} moved to bank slot {bank_slot}");

    if c.logout_request()? != LogoutResult::Success {
        bail!("STEP 3 FAIL: logout was refused");
    }
    c.player_login(character)?;
    if c.self_guid != character {
        bail!(
            "STEP 3 FAIL: relog selected Character {:#x}, want {character:#x}",
            c.self_guid
        );
    }
    if c.own_item_in_slot(bank_slot) != Some(item) {
        bail!(
            "STEP 3 FAIL: relog snapshot bank slot {bank_slot} held {:?}, want item {item:#x}",
            c.own_item_in_slot(bank_slot)
        );
    }
    signal_and_wait_consumed(
        c,
        &persisted,
        30,
        "STEP 3 FAIL: adapter did not confirm relog persistence",
    )?;
    println!("[scenario] STEP 3 OK: relog preserved Character and item identity");

    open_bank(c, banker, 4)?;
    c.send(&CMSG_AUTOSTORE_BANK_ITEM {
        bag_index: MAIN_BAG,
        slot_index: bank_slot,
    })?;
    wait_for_item_move(c, item, bank_slot, carry_slot, 4)?;
    signal_and_wait_consumed(
        c,
        &withdrawn,
        30,
        "STEP 4 FAIL: adapter did not confirm the durable withdrawal",
    )?;
    println!("[scenario] STEP 4 OK: item {item:#x} returned to carry slot {carry_slot}");

    c.send(&CMSG_BUY_BANK_SLOT {
        guid: Guid::new(banker),
    })?;
    match c.recv_for(Duration::from_secs(5), |message| match message {
        Smsg::SMSG_BUY_BANK_SLOT_RESULT(reply) => Some(reply.result),
        _ => None,
    }) {
        Some(BuyBankSlotResult::Ok) => {}
        Some(result) => bail!("STEP 5 FAIL: SMSG_BUY_BANK_SLOT_RESULT={result}"),
        None => bail!("STEP 5 FAIL: no SMSG_BUY_BANK_SLOT_RESULT within 5s"),
    }
    println!("[wire] SCENARIO-BANK PASS \u{2713}  open->deposit->relog->withdraw->buy-slot");
    Ok(())
}

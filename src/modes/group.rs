//! Party/GROUP wire modes (work-items 066/148): leader invite/disband flows, the bot party
//! former, joiner/decliner sides. Holds are orchestrator handshake files.
//! Split out of main.rs (PR-5 review): every family exposes one `try_dispatch`.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::CMSG_LOOT_ROLL;
use wow_world_messages::vanilla::{
    GroupLootSetting, ItemQuality, PartyResult, RollVote, CMSG_GROUP_ACCEPT, CMSG_GROUP_DECLINE,
    CMSG_GROUP_DISBAND, CMSG_GROUP_INVITE, CMSG_LOOT_METHOD,
};
use wow_world_messages::Guid;

use super::{drain_until_file, require_path_arg, ModeCtx};

/// Run `mode` if it belongs to this family. `Ok(true)` = recognized and completed
/// (bail!/exit on failure inside); `Ok(false)` = not this family's mode.
pub(crate) fn try_dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    _mcx: &ModeCtx<'_>,
) -> Result<bool> {
    match mode {
        "group-leader" => group_leader(c, args)?,
        "party-bots" => party_bots(c, args)?,
        "group-join" => group_join(c, args)?,
        "group-invite-expect-decline" => group_invite_expect_decline(c, args)?,
        "group-decline" => group_decline(c, args)?,
        "loot-leader" => loot_leader(c, args)?,
        "loot-voter" => loot_voter(c, args)?,
        _ => return Ok(false),
    }
    Ok(true)
}

// ---- group-leader <target_name> <hold_file>: invite -> PARTY_COMMAND_RESULT(Success) ->
// (target accepts) SMSG_GROUP_LIST carrying the target -> write <hold_file>.ingroup -> hold the
// session until <hold_file> appears -> CMSG_GROUP_DISBAND -> SMSG_GROUP_DESTROYED. ----
fn group_leader(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let target: String = args.next().expect("target name");
    let hold = require_path_arg(args, "group-leader <target_name> <hold_file>", "hold_file")?;
    let _ = std::fs::remove_file(&hold);
    let _ = std::fs::remove_file(format!("{hold}.ingroup"));
    c.set_recv_timeout(Duration::from_millis(500))?;

    c.send(&CMSG_GROUP_INVITE {
        name: target.clone(),
    })?;
    match c.recv_for(Duration::from_secs(10), |m| match m {
        Smsg::SMSG_PARTY_COMMAND_RESULT(r) => Some(r.result),
        _ => None,
    }) {
        Some(PartyResult::Success) => {}
        Some(r) => bail!("STEP 1 FAIL: SMSG_PARTY_COMMAND_RESULT {r:?} (want Success)"),
        None => bail!("STEP 1 FAIL: no SMSG_PARTY_COMMAND_RESULT within 10s of CMSG_GROUP_INVITE"),
    }
    println!("[group] STEP 1 OK — invite acknowledged (PARTY_COMMAND_RESULT Success)");

    // The target's accept triggers the roster push: SMSG_GROUP_LIST listing the target.
    let Some((leader, names)) = c.recv_for(Duration::from_secs(60), |m| match m {
        Smsg::SMSG_GROUP_LIST(l) => {
            let names: Vec<String> = l.members.iter().map(|m| m.name.clone()).collect();
            println!(
                "[group] SMSG_GROUP_LIST members={names:?} leader={:#x}",
                l.leader.guid()
            );
            Some((l.leader.guid(), names))
        }
        _ => None,
    }) else {
        bail!("STEP 2 FAIL: no SMSG_GROUP_LIST within 60s (did the target accept?)");
    };
    if leader != c.self_guid {
        bail!("STEP 2 FAIL: leader {leader:#x} != self {:#x}", c.self_guid);
    }
    if !names.iter().any(|n| n.eq_ignore_ascii_case(&target)) {
        bail!("STEP 2 FAIL: GROUP_LIST lacks {target:?} (members {names:?})");
    }
    println!("[group] STEP 2 OK — SMSG_GROUP_LIST reflects the two-member party (self is leader)");

    hold_then_disband(c, &hold, 180, "group-leader")?;
    println!("[wire] GROUP-LEADER PASS \u{2713}  invite->list->hold->disband->destroyed");
    Ok(())
}

// ---- party-bots <hold_file> <name>... : invite each named BOT (they auto-accept server-side
// via the on_group_invite hook — no second client), assert every invite lands Success + the
// final SMSG_GROUP_LIST carries ALL names, write <hold>.ingroup, hold for the orchestrator's
// fight phase, then disband and expect SMSG_GROUP_DESTROYED. (Work-item 148.) ----
fn party_bots(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let hold = require_path_arg(args, "party-bots <hold_file> <bot names…>", "hold_file")?;
    let names: Vec<String> = args.collect();
    if names.is_empty() {
        bail!("party-bots: need at least one bot name");
    }
    let _ = std::fs::remove_file(&hold);
    let _ = std::fs::remove_file(format!("{hold}.ingroup"));
    c.set_recv_timeout(Duration::from_millis(500))?;

    let mut latest_roster: Vec<String> = Vec::new();
    for name in &names {
        c.send(&CMSG_GROUP_INVITE { name: name.clone() })?;
        // Each bot accepts in the SAME transaction as the invite, so both the command result
        // and the roster push arrive promptly; collect both before the next invite.
        let self_guid = c.self_guid;
        let (mut acked, mut listed) = (false, false);
        let done = c.recv_for(Duration::from_secs(15), |m| {
            match m {
                Smsg::SMSG_PARTY_COMMAND_RESULT(r) => {
                    if r.result != PartyResult::Success {
                        return Some(Err(anyhow!(
                            "party-bots: invite {name:?} -> {:?} (want Success)",
                            r.result
                        )));
                    }
                    acked = true;
                }
                Smsg::SMSG_GROUP_LIST(l) => {
                    latest_roster = l.members.iter().map(|m| m.name.clone()).collect();
                    if l.leader.guid() != self_guid {
                        return Some(Err(anyhow!(
                            "party-bots: leader {:#x} != self {:#x}",
                            l.leader.guid(),
                            self_guid
                        )));
                    }
                    if latest_roster.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                        listed = true;
                    }
                }
                _ => {}
            }
            (acked && listed).then_some(Ok(()))
        });
        match done {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(e),
            None if !acked => {
                bail!("party-bots: no SMSG_PARTY_COMMAND_RESULT for {name:?} within 15s")
            }
            None => bail!(
                "party-bots: no SMSG_GROUP_LIST carrying {name:?} within 15s (roster {latest_roster:?})"
            ),
        }
        println!("[party] invited {name} — roster now {latest_roster:?}");
    }
    for name in &names {
        if !latest_roster.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            bail!("party-bots: final roster {latest_roster:?} lacks {name:?}");
        }
    }
    println!(
        "[party] STEP OK — all {} bots in the roster {latest_roster:?}",
        names.len()
    );

    hold_then_disband(c, &hold, 300, "party-bots")?;
    println!("[wire] PARTY-BOTS PASS \u{2713}  {} bots invited (auto-accept) -> full roster -> hold -> disband -> destroyed", names.len());
    Ok(())
}

// ---- group-join <hold_file>: wait for SMSG_GROUP_INVITE -> CMSG_GROUP_ACCEPT ->
// SMSG_GROUP_LIST -> write <hold_file>.ingroup -> hold until <hold_file> -> expect
// SMSG_GROUP_DESTROYED (the leader's disband dissolves the 2-man group). ----
fn group_join(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let hold = require_path_arg(args, "group-join <hold_file>", "hold_file")?;
    let _ = std::fs::remove_file(&hold);
    let _ = std::fs::remove_file(format!("{hold}.ingroup"));
    c.set_recv_timeout(Duration::from_millis(500))?;

    let Some(inviter) = c.recv_for(Duration::from_secs(60), |m| match m {
        Smsg::SMSG_GROUP_INVITE(i) => Some(i.name.clone()),
        _ => None,
    }) else {
        bail!("STEP 1 FAIL: no SMSG_GROUP_INVITE within 60s");
    };
    println!("[group] STEP 1 OK — SMSG_GROUP_INVITE from {inviter:?}; accepting");

    c.send(&CMSG_GROUP_ACCEPT {})?;
    let Some(names) = c.recv_for(Duration::from_secs(10), |m| match m {
        Smsg::SMSG_GROUP_LIST(l) => {
            let names: Vec<String> = l.members.iter().map(|m| m.name.clone()).collect();
            println!(
                "[group] SMSG_GROUP_LIST members={names:?} leader={:#x}",
                l.leader.guid()
            );
            Some(names)
        }
        _ => None,
    }) else {
        bail!("STEP 2 FAIL: no SMSG_GROUP_LIST within 10s of CMSG_GROUP_ACCEPT");
    };
    if !names.iter().any(|n| n.eq_ignore_ascii_case(&inviter)) {
        bail!("STEP 2 FAIL: GROUP_LIST lacks the inviter {inviter:?} (members {names:?})");
    }
    println!("[group] STEP 2 OK — joined; SMSG_GROUP_LIST carries the leader");

    std::fs::write(format!("{hold}.ingroup"), "1").ok();
    // Drain toward the leader's disband; the release marker is only cleanup on this side
    // (the DESTROYED is the exit condition), so consume it opportunistically.
    let destroyed = c.recv_for(Duration::from_secs(180), |m| {
        if std::path::Path::new(&hold).exists() {
            let _ = std::fs::remove_file(&hold);
        }
        matches!(m, Smsg::SMSG_GROUP_DESTROYED).then_some(())
    });
    if destroyed.is_none() {
        bail!("STEP 3 FAIL: no SMSG_GROUP_DESTROYED after the leader's disband");
    }
    println!("[wire] GROUP-JOIN PASS \u{2713}  invite->accept->list->destroyed on disband");
    Ok(())
}

// ---- group-invite-expect-decline <target_name>: invite, then assert SMSG_GROUP_DECLINE. ----
fn group_invite_expect_decline(
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
) -> Result<()> {
    let target: String = args.next().expect("target name");
    c.set_recv_timeout(Duration::from_millis(500))?;
    c.send(&CMSG_GROUP_INVITE {
        name: target.clone(),
    })?;
    let (mut acked, mut declined) = (false, false);
    let done = c.recv_for(Duration::from_secs(30), |m| {
        match m {
            Smsg::SMSG_PARTY_COMMAND_RESULT(r) => {
                if r.result != PartyResult::Success {
                    return Some(Err(anyhow!("invite rejected: {:?}", r.result)));
                }
                acked = true;
            }
            Smsg::SMSG_GROUP_DECLINE(d) => {
                if !d.name.eq_ignore_ascii_case(&target) {
                    return Some(Err(anyhow!(
                        "SMSG_GROUP_DECLINE from {:?}, want {target:?}",
                        d.name
                    )));
                }
                declined = true;
            }
            _ => {}
        }
        (acked && declined).then_some(Ok(()))
    });
    match done {
        Some(Ok(())) => {}
        Some(Err(e)) => return Err(e),
        None => bail!("decline flow incomplete: acked={acked} declined={declined}"),
    }
    println!("[wire] GROUP-DECLINE-FLOW PASS \u{2713}  invite acked, SMSG_GROUP_DECLINE({target}) received");
    Ok(())
}

// ---- group-decline: wait for SMSG_GROUP_INVITE, decline it. ----
fn group_decline(c: &mut WireClient, _args: &mut dyn Iterator<Item = String>) -> Result<()> {
    c.set_recv_timeout(Duration::from_millis(500))?;
    let invited = c.recv_for(Duration::from_secs(30), |m| {
        matches!(m, Smsg::SMSG_GROUP_INVITE(_)).then_some(())
    });
    if invited.is_none() {
        bail!("no SMSG_GROUP_INVITE within 30s");
    }
    c.send(&CMSG_GROUP_DECLINE {})?;
    // linger briefly so the decline flushes before the socket drops
    let _ = c.recv_for(Duration::from_secs(2), |_| None::<()>);
    println!("[wire] GROUP-DECLINE PASS \u{2713}  invite received and declined");
    Ok(())
}

/// The leader-side tail every hold-style group flow shares: write `<hold>.ingroup` (telling the
/// orchestrator the party is formed), drain the socket until the orchestrator releases `<hold>`,
/// then CMSG_GROUP_DISBAND and expect SMSG_GROUP_DESTROYED.
fn hold_then_disband(c: &mut WireClient, hold: &str, hold_secs: u64, tag: &str) -> Result<()> {
    std::fs::write(format!("{hold}.ingroup"), "1").ok();
    // Drain the socket while held; only the release file (or the per-flow deadline) ends the wait.
    if !drain_until_file(c, hold, hold_secs) {
        bail!("{tag}: orchestrator never released the hold");
    }
    let _ = std::fs::remove_file(hold);

    c.send(&CMSG_GROUP_DISBAND {})?;
    if c.recv_for(Duration::from_secs(10), |m| {
        matches!(m, Smsg::SMSG_GROUP_DESTROYED).then_some(())
    })
    .is_none()
    {
        bail!("{tag}: no SMSG_GROUP_DESTROYED within 10s of CMSG_GROUP_DISBAND");
    }
    Ok(())
}

/// One group-loot ROLL cycle (187): wait for `SMSG_LOOT_START_ROLL`, answer with `vote`
/// ("need"/"greed"), then record the `SMSG_LOOT_ROLL_WON` outcome into `<hold>.won` as
/// "winner_guid item_entry". Both party members run this concurrently after the orchestrator's
/// debug kill; the module resolves the roll when every member has voted (else the 60s deadline).
fn loot_roll_cycle(c: &mut WireClient, vote: &str, hold: &str, tag: &str) -> Result<()> {
    let vote = match vote {
        "need" => RollVote::Need,
        "greed" => RollVote::Greed,
        v => bail!("{tag}: vote must be need|greed, got {v:?}"),
    };
    let Some((creature, slot, item)) = c.recv_for(Duration::from_secs(60), |m| match m {
        Smsg::SMSG_LOOT_START_ROLL(r) => Some((r.creature.guid(), r.loot_slot, r.item)),
        _ => None,
    }) else {
        bail!("{tag}: no SMSG_LOOT_START_ROLL within 60s of the kill");
    };
    println!("[loot] START_ROLL item={item} slot={slot}; voting {vote:?}");
    c.send(&CMSG_LOOT_ROLL {
        item: Guid::new(creature),
        item_slot: slot,
        vote,
    })?;
    let Some((winner, won_item)) = c.recv_for(Duration::from_secs(90), |m| match m {
        Smsg::SMSG_LOOT_ROLL_WON(w) => Some((w.winning_player.guid(), w.item)),
        _ => None,
    }) else {
        bail!("{tag}: no SMSG_LOOT_ROLL_WON (votes in, resolve missed?)");
    };
    println!("[loot] ROLL_WON winner={winner:#x} item={won_item}");
    std::fs::write(format!("{hold}.won"), format!("{winner} {won_item}")).ok();
    Ok(())
}

// ---- loot-leader <target_name> <vote> <hold_file>: the 187 done-when scenario, leader side.
// Forms the party (group-leader steps 1-2), sets GROUP LOOT at UNCOMMON threshold and asserts the
// SMSG_GROUP_LIST echo carries it (clause 3), writes <hold>.method, then runs one roll cycle
// (clause 1) and holds until the orchestrator releases -> disband. ----
fn loot_leader(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let target: String = args
        .next()
        .expect("usage: loot-leader <target_name> <vote> <hold_file>");
    let vote: String = args.next().expect("vote (need|greed)");
    let hold = require_path_arg(
        args,
        "loot-leader <target_name> <vote> <hold_file>",
        "hold_file",
    )?;
    let _ = std::fs::remove_file(&hold);
    for suffix in [".ingroup", ".method", ".won"] {
        let _ = std::fs::remove_file(format!("{hold}{suffix}"));
    }
    c.set_recv_timeout(Duration::from_millis(500))?;

    c.send(&CMSG_GROUP_INVITE {
        name: target.clone(),
    })?;
    match c.recv_for(Duration::from_secs(10), |m| match m {
        Smsg::SMSG_PARTY_COMMAND_RESULT(r) => Some(r.result),
        _ => None,
    }) {
        Some(PartyResult::Success) => {}
        Some(r) => bail!("loot-leader STEP 1 FAIL: SMSG_PARTY_COMMAND_RESULT {r:?}"),
        None => bail!("loot-leader STEP 1 FAIL: no SMSG_PARTY_COMMAND_RESULT"),
    }
    if c.recv_for(Duration::from_secs(60), |m| match m {
        Smsg::SMSG_GROUP_LIST(l) => l
            .members
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(&target))
            .then_some(()),
        _ => None,
    })
    .is_none()
    {
        bail!("loot-leader STEP 2 FAIL: no SMSG_GROUP_LIST with {target:?} (accept missing?)");
    }
    std::fs::write(format!("{hold}.ingroup"), "1").ok();
    println!("[loot] party formed; setting GROUP LOOT @ Uncommon");

    // Clause 3: CMSG_LOOT_METHOD round-trips — the module stores it and every member's roster
    // re-render (SMSG_GROUP_LIST) carries the new loot_setting.
    c.send(&CMSG_LOOT_METHOD {
        loot_setting: GroupLootSetting::GroupLoot,
        loot_master: Guid::new(0),
        loot_threshold: ItemQuality::Uncommon,
    })?;
    if c.recv_for(Duration::from_secs(10), |m| match m {
        Smsg::SMSG_GROUP_LIST(l) => l
            .group_not_empty
            .as_ref()
            .filter(|b| b.loot_setting == GroupLootSetting::GroupLoot)
            .map(|_| ()),
        _ => None,
    })
    .is_none()
    {
        bail!("loot-leader STEP 3 FAIL: no SMSG_GROUP_LIST echoing GroupLoot within 10s");
    }
    println!("[loot] STEP 3 OK — GROUP_LIST echoes GroupLoot/Uncommon");
    std::fs::write(format!("{hold}.method"), "1").ok();

    loot_roll_cycle(c, &vote, &hold, "loot-leader")?;
    hold_then_disband(c, &hold, 240, "loot-leader")?;
    println!("[wire] LOOT-LEADER PASS \u{2713}  method-echo + roll cycle + disband");
    Ok(())
}

// ---- loot-voter <vote> <hold_file>: the joiner side — accept the invite, run one roll cycle,
// then drain until the leader's disband (mirrors group-join's exit condition). ----
fn loot_voter(c: &mut WireClient, args: &mut dyn Iterator<Item = String>) -> Result<()> {
    let vote: String = args.next().expect("usage: loot-voter <vote> <hold_file>");
    let hold = require_path_arg(args, "loot-voter <vote> <hold_file>", "hold_file")?;
    let _ = std::fs::remove_file(&hold);
    for suffix in [".ingroup", ".won"] {
        let _ = std::fs::remove_file(format!("{hold}{suffix}"));
    }
    c.set_recv_timeout(Duration::from_millis(500))?;

    if c.recv_for(Duration::from_secs(60), |m| {
        matches!(m, Smsg::SMSG_GROUP_INVITE(_)).then_some(())
    })
    .is_none()
    {
        bail!("loot-voter STEP 1 FAIL: no SMSG_GROUP_INVITE within 60s");
    }
    c.send(&CMSG_GROUP_ACCEPT {})?;
    if c.recv_for(Duration::from_secs(10), |m| {
        matches!(m, Smsg::SMSG_GROUP_LIST(_)).then_some(())
    })
    .is_none()
    {
        bail!("loot-voter STEP 2 FAIL: no SMSG_GROUP_LIST after accept");
    }
    std::fs::write(format!("{hold}.ingroup"), "1").ok();
    println!("[loot] joined the party; awaiting the roll");

    loot_roll_cycle(c, &vote, &hold, "loot-voter")?;
    // Drain toward the leader's disband (the roll files are the assertions; DESTROYED is the exit).
    if c.recv_for(Duration::from_secs(240), |m| {
        if std::path::Path::new(&hold).exists() {
            let _ = std::fs::remove_file(&hold);
        }
        matches!(m, Smsg::SMSG_GROUP_DESTROYED).then_some(())
    })
    .is_none()
    {
        bail!("loot-voter STEP 4 FAIL: no SMSG_GROUP_DESTROYED after the leader disband");
    }
    println!("[wire] LOOT-VOTER PASS \u{2713}  accept + roll cycle + destroyed");
    Ok(())
}

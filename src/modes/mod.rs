//! Mode-family dispatch for the wire-client driver binary (split out of main.rs, PR-5 review).
//! Each family module exposes `try_dispatch(mode, client, args, ctx) -> Result<bool>`:
//! `Ok(true)` means the mode was recognized and ran to completion, `Ok(false)` means
//! "not mine, try the next family". Unrecognized modes fall through every family back to
//! main.rs's numeric M2 cast-assertion fallback.

mod group;
mod probes;
mod relay;
mod scenario;
mod social;

use anyhow::{anyhow, bail, Result};
use wire_client::WireClient;
use wow_world_messages::vanilla::SMSG_MESSAGECHAT;

/// Session identity shared with every mode family (several spawn extra logons with it).
#[derive(Clone, Copy)]
pub(crate) struct ModeCtx<'a> {
    pub account: &'a str,
    pub password: &'a str,
    pub char_name: &'a str,
}

/// Run `mode` through the CHAR-SELECT-TIER probes (probes that must run BEFORE `login_as`,
/// e.g. `char-enum-gear` / `char-delete` — they never enter the world). `Ok(true)` = claimed
/// and completed; `Ok(false)` = not a char-select mode, proceed to world login + `dispatch`.
pub(crate) fn dispatch_charselect(
    mode: &str,
    account: &str,
    password: &str,
    char_name: &str,
    args: &mut dyn Iterator<Item = String>,
) -> Result<bool> {
    probes::try_dispatch_charselect(mode, account, password, char_name, args)
}

/// Run `mode` through the families until one claims it.
pub(crate) fn dispatch(
    mode: &str,
    c: &mut WireClient,
    args: &mut dyn Iterator<Item = String>,
    mcx: &ModeCtx<'_>,
) -> Result<bool> {
    Ok(probes::try_dispatch(mode, c, args, mcx)?
        || social::try_dispatch(mode, c, args, mcx)?
        || relay::try_dispatch(mode, c, args, mcx)?
        || scenario::try_dispatch(mode, c, args, mcx)?
        || group::try_dispatch(mode, c, args, mcx)?)
}

/// Fetch the next CLI argument as a script-owned handshake path. Handshake paths have NO /tmp
/// defaults (work-item 161): the orchestrator script defines each run-scoped path once (the
/// scenario-lib `/tmp/sc_stay_$$` precedent) and passes it here, so a rename can never half-land
/// as a byte-mismatch runtime timeout and two concurrent runs can never collide on a sentinel.
/// A missing arg is a one-line usage bail naming the arg.
pub(crate) fn require_path_arg(
    args: &mut dyn Iterator<Item = String>,
    mode_usage: &str,
    name: &str,
) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("usage: {mode_usage} — missing <{name}>"))
}

/// Write-sentinel half of the Rust↔shell file handshake: write "1" to `path` (a script-owned
/// path from [`require_path_arg`]), then drain the socket until the orchestrator CONSUMES
/// (deletes) the file or `secs` elapse. The drain is load-bearing — blocking on the file without
/// recv()ing would stall the socket and the gateway would backpressure + drop the connection —
/// and the file check must run even on quiet stretches, which recv_for's packet-driven predicate
/// cannot express; that formerly per-site hand-rolled loop lives here, once. Bails with `what`
/// (plus the path and window) if the file is still present at the deadline.
pub(crate) fn signal_and_wait_consumed(
    c: &mut WireClient,
    path: &str,
    secs: u64,
    what: &str,
) -> Result<()> {
    std::fs::write(path, "1").ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline && std::path::Path::new(path).exists() {
        // Drain with recv_RAW deliberately (sonnet-review finding): a drain needs no decoded
        // view of the traffic, and raw never chokes on gtker-undecodable frames (TYPE-less
        // partial-VALUES updates) nor mutates seen_guids — recv() does both. Standardized for
        // every handshake site, including the ones that historically drained decoded.
        let _ = c.recv_raw(); // keep the socket drained while the orchestrator works
    }
    if std::path::Path::new(path).exists() {
        bail!("{what} ({path} not consumed within {secs}s)");
    }
    Ok(())
}

/// Wait-for-sentinel half of the file handshake: drain the socket until `path` APPEARS or `secs`
/// elapse, returning whether it exists at exit. Same rationale as [`signal_and_wait_consumed`]:
/// the file poll must run even on quiet stretches, and a recv error is NOT terminal — with no
/// ambient packet traffic it is just the socket read-timeout expiring, so only the deadline ends
/// the wait. Callers own the per-site timeout value and any consume/remove of the file after.
pub(crate) fn drain_until_file(c: &mut WireClient, path: &str, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline && !std::path::Path::new(path).exists() {
        // DECODING recv, not recv_raw (276): the held client must run the client reflexes —
        // above all the MSG_MOVE_WORLDPORT_ACK answer to SMSG_NEW_WORLD (recv()'s transfer
        // auto-ack). A raw drain ate the transfer packets silently, so a held party leader
        // ported into an instance stayed despawned forever. recv() skips undecodable frames
        // itself; Err here = just a poll-interval read timeout on a quiet stretch.
        let _ = c.recv();
    }
    std::path::Path::new(path).exists()
}

/// Extract the message text from an SMSG_MESSAGECHAT for probe comparison.
/// The message is in the top-level `message` field (shared across all chat types).
pub(crate) fn extract_chat_text(m: &SMSG_MESSAGECHAT) -> Option<String> {
    use wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType;
    // Only relay Say and Yell for the range probe; other types are out of scope.
    match &m.chat_type {
        SMSG_MESSAGECHAT_ChatType::Say { .. } | SMSG_MESSAGECHAT_ChatType::Yell { .. } => {
            Some(m.message.clone())
        }
        _ => None,
    }
}

/// #74 seam-chat: the SPEAKER's guid out of a Say/Yell SMSG_MESSAGECHAT — `speech_bubble_credit`
/// on both variants (the gateway's `codec::build_chat_message` stamps `sender_guid` into both
/// credit fields; see its own test asserting exactly that). Lets `expect-chat`/`expect-no-chat`
/// (the away-shard seam-chat probe) filter on WHO spoke, not just what they said — needed once a
/// scenario has more than one possible speaker in flight.
pub(crate) fn extract_chat_sender(m: &SMSG_MESSAGECHAT) -> Option<u64> {
    use wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType;
    match &m.chat_type {
        SMSG_MESSAGECHAT_ChatType::Say { speech_bubble_credit, .. }
        | SMSG_MESSAGECHAT_ChatType::Yell { speech_bubble_credit, .. } => Some(speech_bubble_credit.guid()),
        _ => None,
    }
}

/// Parse a vanilla PACKED guid from the head of a payload: mask byte, then one byte per set mask
/// bit (LSB-first). Returns None on truncation.
pub(crate) fn read_packed_guid(payload: &[u8]) -> Option<u64> {
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

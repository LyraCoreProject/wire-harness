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

use anyhow::Result;
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

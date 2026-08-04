//! Headless 1.12.1 (build 5875) wire test-client for the lyracore gateway.
//!
//! Speaks the real WoW protocol — SRP6 logon (port 3724) then the encrypted world session
//! (port 8085) — so tests can drive CMSG and ASSERT on decoded SMSG, instead of QAing
//! through the wine client. It is the client-side routines already proven in
//! `gateway/src/logon/mod.rs` tests + `gateway/src/world/tests.rs::client_handshake`,
//! lifted onto real `TcpStream`s. All blocking std I/O — no async.

pub use lyracore_shared::values_mask;

use std::io::{Cursor, Read};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

// --- logon tier (auth server) ---
use wow_login_messages::all::{
    CMD_AUTH_LOGON_CHALLENGE_Client, Locale, Os, Platform, ProtocolVersion, Version,
};
use wow_login_messages::version_3::opcodes::ServerOpcodeMessage as LogonSmsg;
use wow_login_messages::version_3::{
    CMD_AUTH_LOGON_CHALLENGE_Server, CMD_AUTH_LOGON_PROOF_Client,
    CMD_AUTH_LOGON_PROOF_Client_SecurityFlag, CMD_AUTH_LOGON_PROOF_Server,
};
use wow_login_messages::Message;

// --- SRP + header crypto ---
use wow_srp::client::SrpClientChallenge;
use wow_srp::normalized_string::NormalizedString;
use wow_srp::vanilla_header::{DecrypterHalf, EncrypterHalf, ProofSeed};
use wow_srp::PublicKey;

// --- world tier ---
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as WorldSmsg;
use wow_world_messages::vanilla::ClientMessage;
use wow_world_messages::vanilla::{
    CMSG_MESSAGECHAT_ChatType, Class, Gender, Language, LogoutResult, Race, SpellCastTargets,
    SpellCastTargets_SpellCastTargetFlags, SpellCastTargets_SpellCastTargetFlags_DestLocation,
    SpellCastTargets_SpellCastTargetFlags_Unit, Vector3d as CastVector3d, WorldResult,
    CMSG_AUTH_SESSION, CMSG_CAST_SPELL, CMSG_CHAR_CREATE, CMSG_CHAR_DELETE, CMSG_CHAR_ENUM,
    CMSG_GOSSIP_HELLO, CMSG_ITEM_QUERY_SINGLE, CMSG_LOGOUT_REQUEST, CMSG_MESSAGECHAT,
    CMSG_NPC_TEXT_QUERY, CMSG_PLAYED_TIME, CMSG_PLAYER_LOGIN, CMSG_REPOP_REQUEST,
    CMSG_SET_SELECTION, CMSG_WHO, MSG_MOVE_WORLDPORT_ACK, SMSG_AUTH_RESPONSE,
};
use wow_world_messages::Guid;

const LOGON_PORT: u16 = 3724;
pub const DEFAULT_WORLD_ADDR: &str = "127.0.0.1:8085";
/// Default logon-tier address — the same `127.0.0.1:3724` every existing caller assumed.
pub const DEFAULT_LOGON_ADDR: &str = "127.0.0.1:3724";

/// `SMSG_COMPRESSED_MOVES` carries a u32 decompressed-size prefix inside the protocol's already
/// bounded u16 frame. The generated wow_messages 0.3 decoder trusts that prefix for two allocations
/// and unwraps zlib errors, so inspect it before handing the frame over. Four MiB is orders of
/// magnitude above a legitimate 1.12 movement batch while keeping one corrupt session from asking
/// the process allocator for hundreds of GiB (issue #210).
const SMSG_COMPRESSED_MOVES_OPCODE: u16 = 0x02FB;
const MAX_DECOMPRESSED_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Also doubles as the #209 crash-dump body preview length (deliverable 1 asks for 64 bytes of the
/// failing frame's body) — bumped from the original 16 so ONE constant drives both the inline
/// error-message preview and the on-disk diagnostic, instead of two windows that could drift.
const FRAME_DIAGNOSTIC_BYTES: usize = 64;
/// How many trailing successfully-decoded frames [`FrameLog`] keeps for the #209 crash dump — enough
/// to see the sequence leading into a failure without an unbounded dump file over a long session.
const CRASH_RING_CAPACITY: usize = 32;
/// Where a fatal per-session decode failure's diagnostic lands (issue #209 probe: "capture the exact
/// byte sequence at the crash moment" instead of just knowing "it crashed"). Overridable so tests
/// don't litter the real `/tmp/wire-crash/` and two concurrent test runs can't collide.
fn crash_dump_dir() -> std::path::PathBuf {
    std::env::var_os("WIRE_CRASH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/wire-crash"))
}

/// One frame this session actually decoded: `declared_body_len` is the header's own size field
/// (minus the 2-byte opcode); `actual_body_len` is how many body bytes the decoder actually consumed
/// off the wire before returning. The two SHOULD always match — a mismatch would be exactly #209's
/// hunted mechanism (a wrong declared size desyncing every later header) surfacing on a frame that
/// nonetheless decoded "successfully", so it rides along in every ring-buffer entry rather than only
/// being checked on failure.
#[derive(Clone, Copy, Debug)]
struct DecodedFrame {
    opcode: u16,
    declared_body_len: usize,
    actual_body_len: usize,
}

/// A frame this session is in the middle of assembling from the wire, spanning however many
/// [`read_guarded_world_message`] calls it takes to get all the bytes (issue #209 fix): the header
/// has already been read in full and decrypted EXACTLY ONCE (`header_raw`/`opcode`/`payload_len`),
/// `parser_dec` is the pre-header-decrypt cipher clone the generated decoder needs, and `body` is
/// however many of `payload_len` raw (still-encrypted) body bytes have been pulled off the socket
/// so far. Kept on [`FrameLog`] so a read timeout mid-body doesn't lose it.
struct PendingFrame {
    opcode: u16,
    payload_len: usize,
    header_raw: [u8; 4],
    parser_dec: DecrypterHalf,
    body: Vec<u8>,
}

/// Per-session crash-capture state (issue #209 probe, deliverable 1): a ring buffer of the last
/// [`CRASH_RING_CAPACITY`] successfully-decoded frames, plus whatever [`read_guarded_world_message`]
/// read of the CURRENT (possibly failing) frame's header/body before it gave up. [`WireClient::recv`]
/// updates this on every attempt and — only at the point a decode failure actually ends the session
/// (the panic path, the huge-alloc guard, or the accumulated-skip desync bailout) — dumps it under
/// `crash_dump_dir()`, turning "it crashed" into the exact byte sequence that did it.
///
/// Also carries the #209 read-timeout fix's resumable-read state: `pending_header` (bytes of the
/// 4-byte header collected so far, when a header read is in flight) and `pending_frame` (the rest,
/// once the header is complete). Both survive across separate [`read_guarded_world_message`] calls
/// — the shape a caller like `recv_for` produces when it swallows a socket-timeout `Err` and retries
/// — so a timeout landing mid multi-byte read never drops already-consumed bytes onto the floor.
/// Losing those bytes was issue #209's actual bug: the next call would resume reading from whatever
/// stream position the partial read left off at, decrypt THOSE (wrong) 4 bytes as if they were a
/// fresh header, and permanently desync the header-cipher's `index`/`previous_value` state from the
/// real byte stream — every later frame decodes as garbage from that point on.
struct FrameLog {
    label: String,
    ring: std::collections::VecDeque<DecodedFrame>,
    last_header_raw: [u8; 4],
    last_opcode: Option<u16>,
    last_body_preview: Vec<u8>,
    pending_header: Vec<u8>,
    pending_frame: Option<PendingFrame>,
}

impl FrameLog {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ring: std::collections::VecDeque::with_capacity(CRASH_RING_CAPACITY),
            last_header_raw: [0; 4],
            last_opcode: None,
            last_body_preview: Vec::new(),
            pending_header: Vec::new(),
            pending_frame: None,
        }
    }

    /// Record what was read of the frame CURRENTLY being attempted, before it's known whether the
    /// decode will succeed — so a header/payload read failure (no decoded opcode to blame) still
    /// leaves the dump something to show.
    fn record_attempt(&mut self, header_raw: [u8; 4], opcode: u16, body_preview: &[u8]) {
        self.last_header_raw = header_raw;
        self.last_opcode = Some(opcode);
        self.last_body_preview = body_preview.to_vec();
    }

    fn record_decoded(&mut self, opcode: u16, declared_body_len: usize, actual_body_len: usize) {
        if self.ring.len() == CRASH_RING_CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(DecodedFrame {
            opcode,
            declared_body_len,
            actual_body_len,
        });
    }

    /// Best-effort diagnostic dump for a decode failure that is about to end the session. Never
    /// propagates its own I/O errors — a missing `/tmp` must not mask the REAL error this exists to
    /// explain, so a write failure here is only ever logged to stderr.
    fn dump(&self, err: &anyhow::Error) {
        let dir = crash_dump_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("[wire-crash] could not create {dir:?}: {e}");
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("{}-{ts}.txt", sanitize_label(&self.label)));

        let mut out = String::new();
        use std::fmt::Write as _;
        let _ = writeln!(out, "session: {}", self.label);
        let _ = writeln!(out, "error: {err:#}");
        let _ = writeln!(
            out,
            "last {} decoded frames (oldest first):",
            self.ring.len()
        );
        for f in &self.ring {
            let mismatch = if f.declared_body_len != f.actual_body_len {
                "  <-- MISMATCH"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  opcode=0x{:04X} declared_body_len={} actual_body_len={}{mismatch}",
                f.opcode, f.declared_body_len, f.actual_body_len
            );
        }
        let _ = writeln!(
            out,
            "failing header raw bytes: {}",
            hex_bytes(&self.last_header_raw)
        );
        let _ = writeln!(
            out,
            "failing frame opcode: {}",
            self.last_opcode
                .map(|o| format!("0x{o:04X}"))
                .unwrap_or_else(|| "<unknown>".into())
        );
        let _ = writeln!(
            out,
            "failing body[0..{}]: {}",
            self.last_body_preview.len(),
            hex_bytes(&self.last_body_preview)
        );

        match std::fs::write(&path, out) {
            Ok(()) => eprintln!("[wire-crash] dumped session diagnostic to {path:?}"),
            Err(e) => eprintln!("[wire-crash] failed to write {path:?}: {e}"),
        }
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<empty>".to_string();
    }
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Filenames must survive account names like `BENCH0042` unmodified and never escape
/// `crash_dump_dir()` via a stray `/` or `..`.
fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A tail reader that counts the bytes actually pulled through it — wraps the post-preview portion
/// of a frame's body so [`read_guarded_world_message`] can compare what the decoder actually consumed
/// against what the header declared (see [`DecodedFrame`]). `Rc<Cell<_>>` is enough: this reader
/// never crosses a thread boundary — it lives entirely inside one synchronous decode attempt.
struct CountingRead<R> {
    inner: R,
    consumed: std::rc::Rc<std::cell::Cell<usize>>,
}

impl<R: Read> Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.consumed.set(self.consumed.get() + n);
        Ok(n)
    }
}

#[derive(Debug)]
struct UnsafeServerFrame {
    opcode: u16,
    payload_prefix: Vec<u8>,
    reason: String,
}

impl std::fmt::Display for UnsafeServerFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsafe inbound server frame: opcode=0x{:04X} ",
            self.opcode
        )?;
        write!(f, "payload[0..{}]=", self.payload_prefix.len())?;
        if self.payload_prefix.is_empty() {
            f.write_str("<empty>")?;
        } else {
            for (i, byte) in self.payload_prefix.iter().enumerate() {
                if i != 0 {
                    f.write_str(" ")?;
                }
                write!(f, "{byte:02X}")?;
            }
        }
        write!(f, ": {}", self.reason)
    }
}

impl std::error::Error for UnsafeServerFrame {}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("non-string panic payload")
}

fn unsafe_frame(opcode: u16, payload_prefix: &[u8], reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UnsafeServerFrame {
        opcode,
        payload_prefix: payload_prefix.to_vec(),
        reason: reason.into(),
    })
}

/// Fill `acc` up to `target` bytes by reading from `r`, appending every successfully-read chunk
/// immediately (not just on full completion).
///
/// This is the issue #209 fix's core primitive: a caller like `recv_for` treats a socket-read
/// timeout as "the world is quiet, try again" and calls back in — but the ORIGINAL code read every
/// multi-byte field (the 4-byte header, the body) into a fresh stack buffer via `read_exact`, whose
/// std impl discards whatever it had already read into that buffer the moment any call errors. A
/// timeout landing after 1-3 of a header's 4 bytes had already arrived (more likely the fuller a
/// socket gets under crowd load, since more bytes in flight means more chances a read syscall
/// returns short before the rest lands within the read-timeout window) meant those bytes were gone:
/// the retry started a fresh `read_exact` at the CURRENT stream position — now offset by however
/// many bytes were silently dropped — decrypted THOSE wrong bytes as a header, and the header
/// cipher's `index`/`previous_value` (a running, per-byte-position stream state) never recovered.
/// Buffering into `acc`, which the caller persists across retries in [`FrameLog`], means a timeout
/// only ever pauses a read — it can never rewind or skip the stream.
fn fill_resumable<R: Read>(r: &mut R, acc: &mut Vec<u8>, target: usize) -> std::io::Result<()> {
    while acc.len() < target {
        let mut chunk = vec![0u8; target - acc.len()];
        let n = r.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed mid-frame",
            ));
        }
        acc.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}

/// Read one full frame (header + body) off the wire and hand it to the generated decoder.
///
/// Issue #209 fix: the header and body are each assembled into a persistent buffer on `log`
/// ([`FrameLog::pending_header`] / [`FrameLog::pending_frame`]) via [`fill_resumable`] rather than a
/// stack-local `read_exact` — so if this call returns a socket-timeout error partway through either
/// one, the bytes already read are NOT lost; the next call to this function resumes filling the same
/// buffer instead of restarting at (and permanently desyncing from) the wrong stream position. The
/// header is decrypted via `dec.decrypt_server_header` exactly once, only after all 4 raw bytes are
/// in hand — never partially, never twice. Once the full body is buffered, decoding runs entirely
/// in memory (a `Cursor`, not the live socket), so nothing past this point can time out or leave the
/// frame half-consumed.
///
/// `log` records the attempt (issue #209 crash-capture, deliverable 1) regardless of outcome: the
/// header/opcode/body-preview are stashed BEFORE the decode is attempted (so a header/payload read
/// failure still leaves something in the dump), and a successful decode is appended to the ring
/// buffer with its declared vs. actually-consumed body length.
fn read_guarded_world_message<R: Read>(
    r: &mut R,
    dec: &mut DecrypterHalf,
    log: &mut FrameLog,
) -> Result<WorldSmsg> {
    if log.pending_frame.is_none() {
        // Header phase: resume (or start) filling the 4 raw header bytes. On error, whatever was
        // read this call and any prior call is already in `log.pending_header` — nothing lost.
        fill_resumable(r, &mut log.pending_header, 4)
            .map_err(|e| anyhow!("world frame header: {e}"))?;

        // Clone BEFORE the real `dec` advances — the generated decoder re-derives the header from
        // this pre-advance clone as part of its own decode, exactly as the pre-#209-fix code did.
        let parser_dec = dec.clone();
        let mut header_raw = [0u8; 4];
        header_raw.copy_from_slice(&log.pending_header);
        log.pending_header.clear();

        let header = dec.decrypt_server_header(header_raw);
        if header.size < 2 {
            log.record_attempt(header_raw, header.opcode, &[]);
            return Err(unsafe_frame(
                header.opcode,
                &[],
                format!(
                    "invalid server-frame size {} (the size must include the 2-byte opcode)",
                    header.size
                ),
            ));
        }
        let payload_len = usize::from(header.size - 2);
        log.pending_frame = Some(PendingFrame {
            opcode: header.opcode,
            payload_len,
            header_raw,
            parser_dec,
            body: Vec::new(),
        });
    }

    // Body phase: resume (or start) filling `payload_len` raw body bytes. Left in `log.pending_frame`
    // (not yet taken) so a timeout here keeps the header decrypt result AND the partial body.
    {
        let pf = log.pending_frame.as_mut().expect("just ensured Some above");
        fill_resumable(r, &mut pf.body, pf.payload_len)
            .map_err(|e| anyhow!("world frame payload: {e}"))?;
    }
    // The full frame is in memory now — take ownership; a later call starts the NEXT frame fresh.
    let PendingFrame {
        opcode,
        payload_len,
        header_raw,
        mut parser_dec,
        body,
    } = log.pending_frame.take().expect("just ensured Some above");

    let preview_len = payload_len.min(FRAME_DIAGNOSTIC_BYTES);
    let preview = body[..preview_len].to_vec();
    log.record_attempt(header_raw, opcode, &preview);

    if opcode == SMSG_COMPRESSED_MOVES_OPCODE {
        if payload_len < 4 {
            return Err(unsafe_frame(
                opcode,
                &preview,
                format!(
                    "compressed-moves payload is {payload_len} bytes; the decompressed-size prefix needs 4"
                ),
            ));
        }
        let declared = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
        if declared > MAX_DECOMPRESSED_FRAME_BYTES {
            return Err(unsafe_frame(
                opcode,
                &preview,
                format!(
                    "declares {declared} decompressed bytes; limit is {MAX_DECOMPRESSED_FRAME_BYTES}"
                ),
            ));
        }
    }

    let consumed = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counting_body = CountingRead {
        inner: Cursor::new(body),
        consumed: consumed.clone(),
    };
    let mut frame = Cursor::new(header_raw).chain(counting_body);
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        WorldSmsg::read_encrypted(&mut frame, &mut parser_dec)
    }));
    match decoded {
        Ok(Ok(m)) => {
            let actual_body_len = consumed.get();
            log.record_decoded(opcode, payload_len, actual_body_len);
            Ok(m)
        }
        Ok(Err(e)) => Err(anyhow::Error::new(e)),
        Err(payload) => Err(unsafe_frame(
            opcode,
            &preview,
            format!("decoder panicked: {}", panic_message(payload.as_ref())),
        )),
    }
}

fn ns(s: &str) -> Result<NormalizedString> {
    NormalizedString::new(s).map_err(|e| anyhow!("normalized string {s:?}: {e:?}"))
}

/// Complete SRP6 logon and return `(session key K, world server address)`.
/// Mirrors `gateway/src/logon/mod.rs` `full_srp6_handshake_and_realm_list`.
pub fn logon(account: &str, password: &str) -> Result<([u8; 40], String)> {
    logon_at(DEFAULT_LOGON_ADDR, account, password)
}

/// [`logon`] against an explicit `host:port` (a bare host defaults to the standard logon port).
/// Added for the capacity benchmark, which must be re-runnable against an arbitrary shard's
/// gateway (work-item #18 AC#4); every other caller goes through [`logon`].
pub fn logon_at(logon_addr: &str, account: &str, password: &str) -> Result<([u8; 40], String)> {
    let addr = if logon_addr.contains(':') {
        logon_addr.to_string()
    } else {
        format!("{logon_addr}:{LOGON_PORT}")
    };
    let mut s = TcpStream::connect(&addr).with_context(|| format!("connect logon {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;

    CMD_AUTH_LOGON_CHALLENGE_Client {
        protocol_version: ProtocolVersion::Three,
        version: Version {
            major: 1,
            minor: 12,
            patch: 1,
            build: 5875,
        },
        platform: Platform::X86,
        os: Os::Windows,
        locale: Locale::EnUs,
        utc_timezone_offset: 0,
        client_ip_address: std::net::Ipv4Addr::new(127, 0, 0, 1),
        account_name: account.to_string(),
    }
    .write(&mut s)?;

    let (g, n, salt, server_pubkey) = match LogonSmsg::read(&mut s)? {
        LogonSmsg::CMD_AUTH_LOGON_CHALLENGE(CMD_AUTH_LOGON_CHALLENGE_Server::Success {
            generator,
            large_safe_prime,
            salt,
            server_public_key,
            ..
        }) => (generator, large_safe_prime, salt, server_public_key),
        other => bail!("logon challenge failed (account provisioned?): {other:?}"),
    };
    let n: [u8; 32] = n.try_into().map_err(|_| anyhow!("N not 32 bytes"))?;

    let challenge = SrpClientChallenge::new(
        ns(account)?,
        ns(password)?,
        g[0],
        n,
        PublicKey::from_le_bytes(server_pubkey).map_err(|e| anyhow!("server pubkey: {e:?}"))?,
        salt,
    );

    CMD_AUTH_LOGON_PROOF_Client {
        client_public_key: *challenge.client_public_key(),
        client_proof: *challenge.client_proof(),
        crc_hash: [0u8; 20],
        telemetry_keys: vec![],
        security_flag: CMD_AUTH_LOGON_PROOF_Client_SecurityFlag::None,
    }
    .write(&mut s)?;

    let server_proof = match LogonSmsg::read(&mut s)? {
        LogonSmsg::CMD_AUTH_LOGON_PROOF(CMD_AUTH_LOGON_PROOF_Server::Success {
            server_proof,
            ..
        }) => server_proof,
        other => bail!("logon proof failed (wrong password / unprovisioned): {other:?}"),
    };
    let srp = challenge
        .verify_server_proof(server_proof)
        .map_err(|e| anyhow!("server proof mismatch: {e:?}"))?;
    let k: [u8; 40] = *srp.session_key();

    wow_login_messages::version_8::CMD_REALM_LIST_Client {}.write(&mut s)?;
    let world_addr = match LogonSmsg::read(&mut s)? {
        LogonSmsg::CMD_REALM_LIST(reply) => reply
            .realms
            .first()
            .map(|r| r.address.clone())
            .unwrap_or_else(|| DEFAULT_WORLD_ADDR.to_string()),
        other => bail!("realm list failed: {other:?}"),
    };
    Ok((k, world_addr))
}

/// A live, authenticated world connection. Send CMSG via [`WireClient::send`], read SMSG
/// via [`WireClient::recv`] (which skips the gateway's hand-rolled type-stripped VALUES
/// packets that gtker can't decode — their bytes are still consumed, keystream stays synced).
pub struct WireClient {
    stream: TcpStream,
    enc: EncrypterHalf,
    dec: DecrypterHalf,
    /// The logged-in character's guid (set by [`WireClient::player_login`]).
    pub self_guid: u64,
    /// guids seen in CREATE_OBJECT updates (mobs/peers), newest last.
    pub seen_guids: Vec<u64>,
    /// #72 (warm-handoff self-relay-suppression, seamwalk expect-handoff assertion): the SUBSET of
    /// `seen_guids` whose CREATE carried `ObjectType::Item` or `ObjectType::Container` — this
    /// session's own item/bag objects (the login CREATE burst, plus any gained mid-session).
    /// `note_guids` populates it from the SAME `SMSG_UPDATE_OBJECT` scan that feeds `seen_guids`,
    /// so it needs no separate decode pass.
    pub item_guids: Vec<u64>,
    /// Spell ids from SMSG_INITIAL_SPELLS, captured during the `player_login` burst drain.
    pub initial_spells: Vec<u32>,
    /// Per-slot (standing, flag-empty) from SMSG_INITIALIZE_FACTIONS, captured during the
    /// `player_login` burst drain — index is the Faction.dbc reputation_index (0..63), value is
    /// the raw i32 standing (work-item #076: relog rep-restore verification).
    pub init_factions: Vec<i32>,
    /// The login SMSG_INITIALIZE_FACTIONS flag byte per slot (195B: bit 0x02 = AT_WAR).
    pub init_faction_flags: Vec<u8>,
    /// #180: every `queue_position` seen in an `AuthWaitQueue` response during `connect_world`,
    /// in the order received (empty if the world admitted immediately — the common case, and
    /// exactly what every pre-#180 caller of this client still gets). Used by the `login-queue`
    /// probe to assert the FIFO position sequence a queued connection actually observed.
    pub queue_positions_seen: Vec<u32>,
    /// #209 crash-capture state (deliverable 1): the ring buffer of recently-decoded frames plus the
    /// current attempt's raw bytes, dumped to `crash_dump_dir()` if a decode ever proves fatal to
    /// this session. See [`FrameLog`].
    frame_log: FrameLog,
}

impl WireClient {
    /// One-shot bring-up: logon -> world handshake -> create-or-find `char_name` of
    /// `class` -> player login. Leaves the client in-world.
    pub fn login_as(account: &str, password: &str, char_name: &str, class: Class) -> Result<Self> {
        let (k, world_addr) = logon(account, password)?;
        let mut c = Self::connect_world(&world_addr, account, k)?;
        let guid = c.create_or_find_char(char_name, class)?;
        c.player_login(guid)?;
        Ok(c)
    }

    /// The world handshake: plaintext SMSG_AUTH_CHALLENGE -> CMSG_AUTH_SESSION -> encrypted
    /// SMSG_AUTH_RESPONSE(AuthOk). Mirrors `world/tests.rs::client_handshake`.
    ///
    /// #180: the gateway's admission gate can answer with `AuthWaitQueue { queue_position }`
    /// instead of `AuthOk` any number of times before eventually admitting — this loops through
    /// every resend, recording each position into [`WireClient::queue_positions_seen`], and lifts
    /// the read timeout for the duration of the wait (a queued connection can legitimately sit for
    /// as long as it takes seats to free, which is NOT bounded by the normal 10s packet-arrival
    /// timeout every other read in this client uses). Every pre-#180 caller sees no behavior change
    /// at all: an unlimited gateway (`LYRACORE_MAX_SESSIONS` unset) never sends `AuthWaitQueue`, so the
    /// loop body below never runs and `queue_positions_seen` stays empty.
    pub fn connect_world(world_addr: &str, account: &str, k: [u8; 40]) -> Result<Self> {
        let mut stream = TcpStream::connect(world_addr)
            .with_context(|| format!("connect world {world_addr}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;

        let server_seed = match WorldSmsg::read_unencrypted(&mut stream)? {
            WorldSmsg::SMSG_AUTH_CHALLENGE(c) => c.server_seed,
            other => bail!("expected SMSG_AUTH_CHALLENGE, got {other}"),
        };
        let client_seed = ProofSeed::new();
        let client_seed_value = client_seed.seed();
        let (client_proof, crypto) =
            client_seed.into_client_header_crypto(&ns(account)?, k, server_seed);
        let (enc, mut dec) = crypto.split();

        CMSG_AUTH_SESSION {
            build: 5875,
            server_id: 1,
            username: account.to_string(),
            client_seed: client_seed_value,
            client_proof,
            addon_info: vec![],
        }
        .write_unencrypted_client(&mut stream)?;

        let mut queue_positions_seen = Vec::new();
        // The handshake gets its own `FrameLog` (there's no `Self` yet to own one) and carries it
        // forward into the constructed client below — a queue-wait storm's frames are as legitimate
        // a lead-in to a later crash as anything post-login, so nothing is thrown away here.
        let mut frame_log = FrameLog::new(account);
        loop {
            match read_guarded_world_message(&mut stream, &mut dec, &mut frame_log)? {
                WorldSmsg::SMSG_AUTH_RESPONSE(r) => match *r {
                    SMSG_AUTH_RESPONSE::AuthOk { .. } => break,
                    SMSG_AUTH_RESPONSE::AuthWaitQueue { queue_position } => {
                        if queue_positions_seen.is_empty() {
                            // First time we learn we're queued: admission can legitimately take
                            // an arbitrarily long time (bounded by how fast OTHER sessions end,
                            // not by anything this client controls), so stop timing out.
                            stream.set_read_timeout(None)?;
                        }
                        queue_positions_seen.push(queue_position);
                    }
                    other => bail!("world auth rejected: {other:?}"),
                },
                other => bail!("expected SMSG_AUTH_RESPONSE, got {other}"),
            }
        }
        if !queue_positions_seen.is_empty() {
            // Admitted — restore the normal per-read timeout for the rest of the session.
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        }
        Ok(Self {
            stream,
            enc,
            dec,
            self_guid: 0,
            seen_guids: Vec::new(),
            item_guids: Vec::new(),
            initial_spells: Vec::new(),
            init_factions: Vec::new(),
            init_faction_flags: Vec::new(),
            queue_positions_seen,
            frame_log,
        })
    }

    /// Walk the character server-side from `from` to `to` at `speed` yd/s (testing-hardening
    /// §3.3): MSG_MOVE_START_FORWARD, a heartbeat stream every ~200ms along the straight line
    /// (each carrying the interpolated position + facing toward `to`), then MSG_MOVE_STOP at the
    /// destination. This is what the real client sends when the player holds W — it unlocks
    /// walk-into-range / leash / AOI-crossing scenarios headlessly (fixtures no longer need to
    /// spawn inside the 5 yd standstill melee reach). Vanilla run speed is 7.0 yd/s; the server's
    /// NaN/teleport guards see a plausible stream, and its `last_move_ms`/moving flags behave as
    /// for a real runner. Blocks for the walk duration.
    pub fn walk_to(
        &mut self,
        from: (f32, f32, f32),
        to: (f32, f32, f32),
        speed: f32,
    ) -> Result<()> {
        use wow_world_messages::vanilla::{
            MSG_MOVE_HEARTBEAT_Client, MSG_MOVE_START_FORWARD_Client, MSG_MOVE_STOP_Client,
            MovementInfo, MovementInfo_MovementFlags, Vector3d,
        };
        let (dx, dy, dz) = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        let orientation = dy.atan2(dx);
        let speed = speed.max(0.1);
        let total_ms = ((dist / speed) * 1000.0) as u32;
        let info = |x: f32, y: f32, z: f32, moving: bool, ts: u32| MovementInfo {
            flags: if moving {
                MovementInfo_MovementFlags::new_forward()
            } else {
                MovementInfo_MovementFlags::empty()
            },
            timestamp: ts,
            position: Vector3d { x, y, z },
            orientation,
            fall_time: 0.0,
        };
        self.send(&MSG_MOVE_START_FORWARD_Client {
            info: info(from.0, from.1, from.2, true, 0),
        })?;
        const STEP_MS: u32 = 200;
        let mut t = STEP_MS;
        while t < total_ms {
            let f = t as f32 / total_ms.max(1) as f32;
            self.send(&MSG_MOVE_HEARTBEAT_Client {
                info: info(from.0 + dx * f, from.1 + dy * f, from.2 + dz * f, true, t),
            })?;
            std::thread::sleep(Duration::from_millis(STEP_MS as u64));
            t += STEP_MS;
        }
        self.send(&MSG_MOVE_STOP_Client {
            info: info(to.0, to.1, to.2, false, total_ms),
        })?;
        Ok(())
    }

    /// Send an encrypted CMSG.
    pub fn send<M: ClientMessage>(&mut self, m: &M) -> Result<()> {
        m.write_encrypted_client(&mut self.stream, &mut self.enc)
            .map_err(|e| anyhow!("send: {e}"))?;
        Ok(())
    }

    /// Send a raw client frame: `opcode` + `body`, header encrypted like any CMSG. The escape
    /// hatch for opcodes gtker cannot ENCODE — the addon bridge's LANG_ADDON chat (184) is the
    /// canonical user (`SendAddonMessage` on the real client; this fakes it byte-for-byte).
    pub fn send_raw(&mut self, opcode: u32, body: &[u8]) -> Result<()> {
        use std::io::Write;
        let size = (body.len() + 4) as u16; // size counts the u32 opcode, not itself
        self.enc
            .write_encrypted_client_header(&mut self.stream, size, opcode)
            .map_err(|e| anyhow!("send_raw header: {e}"))?;
        self.stream
            .write_all(body)
            .map_err(|e| anyhow!("send_raw body: {e}"))?;
        Ok(())
    }

    /// Read the next raw encrypted frame without gtker-decoding the payload.
    /// Returns `(opcode, payload_bytes)`. Advances the cipher state exactly like `recv()`,
    /// so `recv()` and `recv_raw()` can be interleaved freely — one packet at a time.
    /// Use this for packets gtker rejects (e.g. TYPE-less partial-VALUES updates).
    /// Override the world socket's read timeout (default 10s). The soak/AOI harness modes drain on
    /// a sub-second cadence between synthesized movement sends, so the default would stall them.
    pub fn set_recv_timeout(&mut self, d: Duration) -> Result<()> {
        self.stream.set_read_timeout(Some(d))?;
        Ok(())
    }

    pub fn recv_raw(&mut self) -> Result<(u16, Vec<u8>)> {
        use std::io::Read;
        let hdr = self
            .dec
            .read_and_decrypt_server_header(&mut self.stream)
            .map_err(|e| anyhow!("recv_raw header: {e}"))?;
        // `hdr.size` = opcode (2 bytes) + payload; subtract 2 to get payload-only length.
        let payload_len = (hdr.size as usize).saturating_sub(2);
        let mut payload = vec![0u8; payload_len];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| anyhow!("recv_raw payload: {e}"))?;
        Ok((hdr.opcode, payload))
    }

    /// Read the next *decodable* SMSG. Skips packets gtker can't parse (the gateway's
    /// hand-rolled type-stripped partial-VALUES updates — health bars / quest log); their
    /// frame is still consumed off the cipher stream so the keystream stays in lockstep.
    /// Records any CREATE_OBJECT guids it passes for later targeting.
    pub fn recv(&mut self) -> Result<WorldSmsg> {
        let mut skipped = 0u32;
        loop {
            match read_guarded_world_message(&mut self.stream, &mut self.dec, &mut self.frame_log) {
                Ok(m) => {
                    self.note_guids(&m);
                    // The real 5875 client answers a cross-map transfer (SMSG_NEW_WORLD after
                    // TRANSFER_PENDING) with MSG_MOVE_WORLDPORT_ACK once its load screen ends —
                    // without the ack the server never rebuilds the entity and the character is
                    // limbo'd. Reflex lives HERE in the one decode point so EVERY mode survives
                    // being ported (a held party leader entering an instance, 276).
                    if matches!(m, WorldSmsg::SMSG_NEW_WORLD(_)) {
                        let _ = self.send(&MSG_MOVE_WORLDPORT_ACK {});
                    }
                    return Ok(m);
                }
                Err(e) => {
                    // An unsafe length prefix or a decoder panic is not an ordinary gtker gap.
                    // Surface the diagnostic, close only this connection, and let a multi-session
                    // harness keep every other player alive (issue #210).
                    if is_unsafe_server_frame(&e) {
                        eprintln!("[wire] {e}");
                        self.frame_log.dump(&e);
                        let _ = self.stream.shutdown(Shutdown::Both);
                        return Err(e);
                    }
                    // Diagnostic: surface every skipped (undecodable) frame when asked — the gtker
                    // wire gaps (danger-zones §2) are otherwise silent.
                    if std::env::var_os("WIRE_LOG_SKIPS").is_some() {
                        eprintln!("[wire-skip] {e}");
                    }
                    // A SOCKET timeout is terminal, not a skip: the skip loop exists for packets
                    // gtker can't decode (which fail instantly). Swallowing timeouts here made a
                    // quiet-world recv() spin up to 64 × the 10s read timeout (scenario pads with
                    // nothing moving nearby hit this as a ten-minute hang).
                    let msg = e.to_string().to_lowercase();
                    // Linux EAGAIN reads "resource temporarily unavailable" (os error 11).
                    if msg.contains("timed out")
                        || msg.contains("would block")
                        || msg.contains("temporarily unavailable")
                    {
                        return Err(anyhow!("recv: socket read timeout: {e}"));
                    }
                    // A CLOSED connection ("connection closed mid-frame" — [`fill_resumable`] hitting
                    // EOF) is also terminal, and for the same reason a timeout is: this is a "broken
                    // stream", not a "nothing yet". Issue #209 fix, found live: without this, a
                    // connection that ends for ANY reason (a graceful peer close, a session the
                    // gateway shed under load — nothing to do with #209's decode-corruption at all)
                    // fell into the `skipped`/dump path below. A closed socket's reads return
                    // instantly (no blocking), so a caller that keeps calling `recv()` once per poll
                    // tick for the rest of a benchmark's hold window hit the 64-skip threshold on
                    // EVERY call and wrote a fresh crash dump EVERY time — one dead session produced
                    // thousands of files, burying the (correctly now-absent) real corruption signal
                    // under noise that has nothing to do with it. No dump here: an ended stream is
                    // not decode evidence worth capturing.
                    if msg.contains("connection closed") {
                        return Err(anyhow!("recv: connection closed: {e}"));
                    }
                    skipped += 1;
                    if skipped > 64 {
                        // 64 STRAIGHT ordinary `read_encrypted` errors (as opposed to the ONE-shot
                        // panic/huge-alloc guard above) is the OTHER shape a real desync can take: no
                        // single frame trips a guard, but nothing ever decodes cleanly again. Dump the
                        // same diagnostic — this is exactly the "any read_encrypted error" case #209's
                        // probe asks to capture, just detected after it accumulates instead of once.
                        let terminal = anyhow!("recv: stream desync/closed after 64 skips: {e}");
                        self.frame_log.dump(&terminal);
                        return Err(terminal);
                    }
                    continue;
                }
            }
        }
    }

    /// Read until a message satisfies `pred`, returning it. Other (decodable) messages are
    /// discarded (but their guids are still recorded).
    pub fn recv_until<F: Fn(&WorldSmsg) -> bool>(&mut self, pred: F) -> Result<WorldSmsg> {
        for _ in 0..256 {
            let m = self.recv()?;
            if pred(&m) {
                return Ok(m);
            }
        }
        bail!("recv_until: predicate never matched in 256 messages")
    }

    /// Read until `pred` returns `Some`, bounded by a WALL-CLOCK window (not a message count).
    /// Socket read timeouts and undecodable packets just consume window time — a quiet stretch is
    /// NOT terminal. This is the canonical form of the driver's "wait up to N seconds for packet X"
    /// loops; the hand-rolled `while Instant::now() < deadline { match recv() ... }` copies kept
    /// breeding starved-recv bugs through their ad-hoc `Err(_)` arms (`break` where a read timeout
    /// merely means the world is quiet). Returns `None` if the window closes without a match.
    /// Callers wanting snappy polling should `set_recv_timeout` to something short first — the
    /// window can overshoot by up to one socket read timeout.
    pub fn recv_for<T>(
        &mut self,
        window: std::time::Duration,
        mut pred: impl FnMut(&WorldSmsg) -> Option<T>,
    ) -> Option<T> {
        let deadline = std::time::Instant::now() + window;
        while std::time::Instant::now() < deadline {
            match self.recv() {
                Ok(m) => {
                    if let Some(t) = pred(&m) {
                        return Some(t);
                    }
                }
                Err(e) if is_unsafe_server_frame(&e) => return None,
                Err(_) => {}
            }
        }
        None
    }

    /// The raw-frame twin of [`WireClient::recv_for`]: read `(opcode, payload)` frames via
    /// [`WireClient::recv_raw`] until `pred` returns `Some`, bounded by the same WALL-CLOCK
    /// window. Socket read timeouts just consume window time — a quiet stretch is NOT terminal —
    /// and the window can overshoot by up to one socket read timeout, so callers wanting snappy
    /// polling should `set_recv_timeout` to something short first. Use this where the awaited
    /// packet is one gtker rejects (e.g. TYPE-less partial-VALUES updates) or where the probe
    /// decodes payload bytes by hand. Returns `None` if the window closes without a match.
    pub fn recv_raw_for<T>(
        &mut self,
        window: std::time::Duration,
        mut pred: impl FnMut(u16, &[u8]) -> Option<T>,
    ) -> Option<T> {
        let deadline = std::time::Instant::now() + window;
        while std::time::Instant::now() < deadline {
            if let Ok((opcode, payload)) = self.recv_raw() {
                if let Some(t) = pred(opcode, &payload) {
                    return Some(t);
                }
            }
        }
        None
    }

    fn note_guids(&mut self, m: &WorldSmsg) {
        use wow_world_messages::vanilla::ObjectType;
        if let WorldSmsg::SMSG_UPDATE_OBJECT(u) = m {
            for o in &u.objects {
                if let Some(g) = create_object_guid(o) {
                    if !self.seen_guids.contains(&g) {
                        self.seen_guids.push(g);
                    }
                    // #72: ITEM and CONTAINER creates are this session's own item/bag objects (the
                    // login burst, plus anything gained mid-session) — see `item_guids`'s field doc.
                    if matches!(
                        create_object_type(o),
                        Some(ObjectType::Item | ObjectType::Container)
                    ) && !self.item_guids.contains(&g)
                    {
                        self.item_guids.push(g);
                    }
                }
            }
        }
    }

    /// Request the character list. Returns `(guid, name, class)` per character.
    pub fn char_enum(&mut self) -> Result<Vec<(u64, String, Class)>> {
        self.send(&CMSG_CHAR_ENUM {})?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_ENUM(_)))?;
        let WorldSmsg::SMSG_CHAR_ENUM(e) = m else {
            unreachable!()
        };
        Ok(e.characters
            .iter()
            .map(|c| (c.guid.guid(), c.name.clone(), c.class))
            .collect())
    }

    /// Request the character list and return the raw equipment display_ids for each character.
    /// Returns `(guid, name, display_ids)` where display_ids is a 19-element vec indexed by
    /// equipment slot (slot 15 = main-hand weapon).
    /// Used to verify SMSG_CHAR_ENUM carries real display_ids (not all-zero).
    pub fn char_enum_gear(&mut self) -> Result<Vec<(u64, String, Vec<u32>)>> {
        self.send(&CMSG_CHAR_ENUM {})?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_ENUM(_)))?;
        let WorldSmsg::SMSG_CHAR_ENUM(e) = m else {
            unreachable!()
        };
        Ok(e.characters
            .iter()
            .map(|c| {
                let display_ids: Vec<u32> =
                    c.equipment.iter().map(|g| g.equipment_display_id).collect();
                (c.guid.guid(), c.name.clone(), display_ids)
            })
            .collect())
    }

    /// Find a character by name, or create a Human/Male of `class` with that name. Returns
    /// its guid. Rerunnable: a name-in-use create just falls back to the existing char.
    pub fn create_or_find_char(&mut self, name: &str, class: Class) -> Result<u64> {
        self.create_or_find_char_as(name, class, Race::Human)
    }

    /// [`create_or_find_char`](Self::create_or_find_char) with the race chosen by the caller.
    ///
    /// The race decides which DATABASE the character is born on, which is why this exists. A new
    /// character is placed at its race's `game_start_position`, and those straddle the continent
    /// split: Human is Elwynn on map 0 (`lyracore`), Orc and Troll are the Valley of Trials on
    /// map 1 (`lyracore-world-1`). So a benchmark that wants to load the KALIMDOR writer cannot use
    /// the default — 200 Humans created against a world-1 gateway are all born on map 0 and every
    /// one of them immediately transfers to core, measuring the shard it was trying to leave alone
    /// (work-item #71).
    pub fn create_or_find_char_as(&mut self, name: &str, class: Class, race: Race) -> Result<u64> {
        if let Some((g, _, _)) = self
            .char_enum()?
            .into_iter()
            .find(|(_, n, _)| n.eq_ignore_ascii_case(name))
        {
            return Ok(g);
        }
        self.send(&CMSG_CHAR_CREATE {
            name: name.to_string(),
            race,
            class,
            gender: Gender::Male,
            skin_color: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        })?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_CREATE(_)))?;
        let WorldSmsg::SMSG_CHAR_CREATE(r) = m else {
            unreachable!()
        };
        match r.result {
            WorldResult::CharCreateSuccess | WorldResult::CharCreateNameInUse => {}
            other => bail!("char create failed: {other:?}"),
        }
        self.char_enum()?
            .into_iter()
            .find(|(_, n, _)| n.eq_ignore_ascii_case(name))
            .map(|(g, _, _)| g)
            .ok_or_else(|| anyhow!("character {name:?} not found after create"))
    }

    /// Delete `guid` (`CMSG_CHAR_DELETE`, work-item 081) and return the resulting `WorldResult`.
    pub fn char_delete(&mut self, guid: u64) -> Result<WorldResult> {
        self.send(&CMSG_CHAR_DELETE {
            guid: Guid::new(guid),
        })?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_DELETE(_)))?;
        let WorldSmsg::SMSG_CHAR_DELETE(r) = m else {
            unreachable!()
        };
        Ok(r.result)
    }

    /// Enter the world as `guid`, draining the post-login burst up to (and including) the
    /// self CREATE_OBJECT. Sets `self_guid`.
    pub fn player_login(&mut self, guid: u64) -> Result<()> {
        self.send(&CMSG_PLAYER_LOGIN {
            guid: Guid::new(guid),
        })?;
        // The burst starts with SMSG_LOGIN_VERIFY_WORLD and ends with the self CREATE_OBJECT.
        self.recv_until(|m| matches!(m, WorldSmsg::SMSG_LOGIN_VERIFY_WORLD(_)))?;
        self.self_guid = guid;
        // Drain through the self-spawn CREATE_OBJECT so subsequent reads are gameplay traffic, capturing
        // SMSG_INITIAL_SPELLS (the client spellbook) en route.
        loop {
            let m = self.recv()?;
            match &m {
                WorldSmsg::SMSG_INITIAL_SPELLS(s) => {
                    self.initial_spells = s
                        .initial_spells
                        .iter()
                        .map(|e| u32::from(e.spell_id))
                        .collect();
                }
                WorldSmsg::SMSG_INITIALIZE_FACTIONS(f) => {
                    self.init_factions = f.factions.iter().map(|s| s.standing as i32).collect();
                    self.init_faction_flags = f.factions.iter().map(|s| s.flag.as_int()).collect();
                }
                WorldSmsg::SMSG_UPDATE_OBJECT(u)
                    if u.objects
                        .iter()
                        .any(|o| create_object_guid(o) == Some(guid)) =>
                {
                    break
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Select a target by guid (CMSG_SET_SELECTION).
    pub fn set_selection(&mut self, target: u64) -> Result<()> {
        self.send(&CMSG_SET_SELECTION {
            target: Guid::new(target),
        })
    }

    /// Right-click a gossip NPC (CMSG_GOSSIP_HELLO) — the gateway should reply with
    /// SMSG_GOSSIP_MESSAGE (its quests for a gossip+questgiver like McBride).
    pub fn gossip_hello(&mut self, npc: u64) -> Result<()> {
        self.send(&CMSG_GOSSIP_HELLO {
            guid: Guid::new(npc),
        })
    }

    /// Right-click a QUESTGIVER-ONLY NPC (CMSG_QUESTGIVER_HELLO) — the real protocol the client uses
    /// when an NPC has npc_flags QUESTGIVER but NOT GOSSIP (e.g. Deputy Willem). Reply: SMSG_QUESTGIVER_QUEST_LIST.
    pub fn questgiver_hello(&mut self, npc: u64) -> Result<()> {
        self.send(&wow_world_messages::vanilla::CMSG_QUESTGIVER_HELLO {
            guid: Guid::new(npc),
        })
    }

    /// Send `CMSG_NPC_TEXT_QUERY` for `text_id` and wait for `SMSG_NPC_TEXT_UPDATE`. Returns the
    /// text in slot 0 (the greeting slot the gateway always fills). Used to verify per-NPC text.
    pub fn npc_text_query(&mut self, text_id: u32, npc_guid: u64) -> Result<String> {
        use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
        self.send(&CMSG_NPC_TEXT_QUERY {
            text_id,
            guid: Guid::new(npc_guid),
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match self.recv()? {
                Smsg::SMSG_NPC_TEXT_UPDATE(t) if t.text_id == text_id => {
                    return Ok(t.texts[0].texts[0].clone());
                }
                _ => continue,
            }
        }
        anyhow::bail!("timed out waiting for SMSG_NPC_TEXT_UPDATE(text_id={text_id})")
    }

    /// Send CMSG_ITEM_QUERY_SINGLE for `item_entry` and wait for the response. Returns
    /// `(armor, block, sell_price_copper, stats[(stat_type_int, value)], spell1, trigger1, bonding)`
    /// decoded from the SMSG reply — `bonding` is the raw `Bonding` enum int (work-item 127:
    /// 0=NoBind,1=BoP,2=BoE,3=BoU,4/5=QuestItem). The server replies for any valid entry regardless
    /// of whether the character holds it.
    pub fn query_item(
        &mut self,
        item_entry: u32,
    ) -> Result<(i32, u32, u32, Vec<(u8, i32)>, u32, u32, u8)> {
        use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
        // The vanilla packet carries both item entry and a guid (the item object guid when the
        // client holds the item; 0 when querying "cold" without the object — both are accepted).
        self.send(&CMSG_ITEM_QUERY_SINGLE {
            item: item_entry,
            guid: Guid::new(0),
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match self.recv()? {
                Smsg::SMSG_ITEM_QUERY_SINGLE_RESPONSE(r) if r.item == item_entry => {
                    let Some(found) = r.found else {
                        bail!("item {item_entry} not found in server item_template table");
                    };
                    let stats: Vec<(u8, i32)> = found
                        .stats
                        .iter()
                        .map(|s| (s.stat_type.as_int(), s.value))
                        .collect();
                    // spell slot 1 (id + ItemSpellTriggerType) — drives the client green "Use:" text.
                    let spell1 = found.spells[0].spell;
                    let trig1 = u32::from(found.spells[0].spell_trigger.as_int());
                    let bonding = found.bonding.as_int();
                    return Ok((
                        found.armor,
                        found.block,
                        found.sell_price.as_int(),
                        stats,
                        spell1,
                        trig1,
                        bonding,
                    ));
                }
                _ => continue,
            }
        }
        bail!("timed out waiting for SMSG_ITEM_QUERY_SINGLE_RESPONSE(item={item_entry})")
    }

    /// Send CMSG_LOGOUT_REQUEST and return the LogoutResult from SMSG_LOGOUT_RESPONSE.
    /// On Success, also drains the SMSG_LOGOUT_COMPLETE that follows.
    pub fn logout_request(&mut self) -> Result<LogoutResult> {
        self.send(&CMSG_LOGOUT_REQUEST {})?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match self.recv()? {
                WorldSmsg::SMSG_LOGOUT_RESPONSE(r) => {
                    if r.result == LogoutResult::Success {
                        // Drain SMSG_LOGOUT_COMPLETE (it's sent in the same batch).
                        let deadline2 =
                            std::time::Instant::now() + std::time::Duration::from_secs(3);
                        while std::time::Instant::now() < deadline2 {
                            match self.recv()? {
                                WorldSmsg::SMSG_LOGOUT_COMPLETE => break,
                                _ => continue,
                            }
                        }
                    }
                    return Ok(r.result);
                }
                _ => continue,
            }
        }
        bail!("timed out waiting for SMSG_LOGOUT_RESPONSE")
    }

    /// Send CMSG_REPOP_REQUEST (empty body — release spirit on the death screen).
    pub fn repop_request(&mut self) -> Result<()> {
        self.send(&CMSG_REPOP_REQUEST {})
    }

    /// Send CMSG_PLAYED_TIME (`/played`, work-item 029; empty body) and return
    /// `(total_played_time, level_played_time)` from the SMSG_PLAYED_TIME reply.
    pub fn played_time_request(&mut self) -> Result<(u32, u32)> {
        self.send(&CMSG_PLAYED_TIME {})?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let WorldSmsg::SMSG_PLAYED_TIME(p) = self.recv()? {
                return Ok((p.total_played_time, p.level_played_time));
            }
        }
        bail!("timed out waiting for SMSG_PLAYED_TIME")
    }

    /// Send CMSG_WHO (empty filters — request all online players) and return the WHO response.
    /// Returns `(online_count, [(name, level, class_int, race_int)])`.
    pub fn who_request(&mut self) -> Result<(u32, Vec<(String, u8, u8, u8)>)> {
        use wow_world_messages::vanilla::Level;
        self.send(&CMSG_WHO {
            minimum_level: Level::new(0),
            maximum_level: Level::new(100),
            player_name: String::new(),
            guild_name: String::new(),
            race_mask: 0xFFFF_FFFF,
            class_mask: 0xFFFF_FFFF,
            zones: vec![],
            search_strings: vec![],
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match self.recv()? {
                WorldSmsg::SMSG_WHO(r) => {
                    let players: Vec<(String, u8, u8, u8)> = r
                        .players
                        .iter()
                        .map(|p| {
                            (
                                p.name.clone(),
                                p.level.as_int(),
                                p.class.as_int(),
                                p.race.as_int(),
                            )
                        })
                        .collect();
                    return Ok((r.online_players, players));
                }
                _ => continue,
            }
        }
        bail!("timed out waiting for SMSG_WHO")
    }

    /// Cast `spell_id` at `target` guid (CMSG_CAST_SPELL with TARGET_FLAG_UNIT).
    pub fn cast_spell(&mut self, spell_id: u32, target: u64) -> Result<()> {
        self.send(&CMSG_CAST_SPELL {
            spell: spell_id,
            targets: SpellCastTargets {
                target_flags: SpellCastTargets_SpellCastTargetFlags::new_unit(
                    SpellCastTargets_SpellCastTargetFlags_Unit {
                        unit_target: Guid::new(target),
                    },
                ),
            },
        })
    }

    /// Cast `spell_id` at a clicked GROUND point (CMSG_CAST_SPELL with TARGET_FLAG_DEST_LOCATION) —
    /// the ground-targeted AoE shape (Flamestrike/Blizzard). The gateway routes the dest block to
    /// `cast_spell_at`, which anchors the area/nuke at these coords (118 phase 2 / 262).
    pub fn cast_spell_at_dest(&mut self, spell_id: u32, x: f32, y: f32, z: f32) -> Result<()> {
        self.send(&CMSG_CAST_SPELL {
            spell: spell_id,
            targets: SpellCastTargets {
                target_flags: SpellCastTargets_SpellCastTargetFlags::new_dest_location(
                    SpellCastTargets_SpellCastTargetFlags_DestLocation {
                        destination: CastVector3d { x, y, z },
                    },
                ),
            },
        })
    }

    /// Send a SAY (chat_type=0) line to the world. The gateway range-gates relay
    /// to listeners within ~25yd; the speaker always hears their own message.
    pub fn send_say(&mut self, message: &str) -> Result<()> {
        self.send(&CMSG_MESSAGECHAT {
            chat_type: CMSG_MESSAGECHAT_ChatType::Say,
            language: Language::Universal,
            message: message.to_string(),
        })
    }

    /// Send a YELL (chat_type=1) line to the world. Gateway range-gates to ~300yd.
    pub fn send_yell(&mut self, message: &str) -> Result<()> {
        self.send(&CMSG_MESSAGECHAT {
            chat_type: CMSG_MESSAGECHAT_ChatType::Yell,
            language: Language::Universal,
            message: message.to_string(),
        })
    }
}

/// Extract the guid from a CREATE-flavored `Object` (CreateObject / CreateObject2), else None.
pub fn create_object_guid(o: &wow_world_messages::vanilla::Object) -> Option<u64> {
    use wow_world_messages::vanilla::Object;
    match o {
        Object::CreateObject { guid3, .. } | Object::CreateObject2 { guid3, .. } => {
            Some(guid3.guid())
        }
        _ => None,
    }
}

/// The `ObjectType` a CREATE/CREATE2 block carries, or `None` for any other `Object` variant —
/// the type-discriminating sibling of [`create_object_guid`] (same match shape, so the two can
/// never disagree on which `Object` variants count as a create). #72's `seamwalk expect-handoff`
/// assertion uses this to separate this session's own ITEM/CONTAINER objects from every other
/// CREATE (mobs, peers, its own player entity) — a `SMSG_DESTROY_OBJECT` for a peer or a creature
/// leaving AOI on the far side of the seam is normal; one for an item guid is the #72 defect.
pub fn create_object_type(
    o: &wow_world_messages::vanilla::Object,
) -> Option<wow_world_messages::vanilla::ObjectType> {
    use wow_world_messages::vanilla::Object;
    match o {
        Object::CreateObject { object_type, .. } | Object::CreateObject2 { object_type, .. } => {
            Some(*object_type)
        }
        _ => None,
    }
}

/// Is this `recv()` error just the socket read timeout — "nothing arrived in the last window" —
/// rather than a broken stream?
///
/// A hand-rolled drain loop that reads on a WALL-CLOCK deadline must keep going through these, or
/// the first quiet gap ends it early: `test-cast-interrupt.sh` failed under full-suite load for
/// exactly that reason, breaking out of a 12s deadline within a second and reporting "no interrupt"
/// for a packet that had not been sent yet. Deadline-based helpers (`recv_for`/`recv_raw_for`)
/// already handle this internally; this is for the loops that don't.
pub fn is_read_timeout(e: &anyhow::Error) -> bool {
    e.to_string().contains("socket read timeout")
}

/// Did [`WireClient::recv`] reject a frame before an unbounded generated-decoder allocation, or
/// contain a panic raised by that decoder? Deadline helpers and multi-session harnesses use this to
/// stop only the affected session instead of treating the failure like a transient quiet socket.
pub fn is_unsafe_server_frame(e: &anyhow::Error) -> bool {
    e.downcast_ref::<UnsafeServerFrame>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    const SMSG_COMPRESSED_MOVES_OPCODE: u16 = 0x02FB;
    const MAX_DECOMPRESSED_FRAME_BYTES: u32 = 4 * 1024 * 1024;

    fn client_receiving_frames(frames: &[(u16, &[u8])]) -> WireClient {
        client_receiving_frames_labeled(frames, "TEST")
    }

    fn client_receiving_frames_labeled(frames: &[(u16, &[u8])], label: &str) -> WireClient {
        let (_, crypto) =
            ProofSeed::new().into_client_header_crypto(&ns("TEST").unwrap(), [7; 40], 0x1234_5678);
        let (mut enc, dec) = crypto.split();

        let mut wire = Vec::new();
        for (opcode, payload) in frames {
            wire.extend_from_slice(&enc.encrypt_server_header((payload.len() + 2) as u16, *opcode));
            wire.extend_from_slice(payload);
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.write_all(&wire).unwrap();
        drop(server);

        WireClient {
            stream,
            enc,
            dec,
            self_guid: 0,
            seen_guids: Vec::new(),
            item_guids: Vec::new(),
            initial_spells: Vec::new(),
            init_factions: Vec::new(),
            init_faction_flags: Vec::new(),
            queue_positions_seen: Vec::new(),
            frame_log: FrameLog::new(label),
        }
    }

    fn client_receiving_frame(opcode: u16, payload: &[u8]) -> WireClient {
        client_receiving_frames(&[(opcode, payload)])
    }

    #[test]
    fn oversized_compressed_frame_is_rejected_before_the_declared_allocation() {
        let declared = MAX_DECOMPRESSED_FRAME_BYTES + 1;
        let mut payload = declared.to_le_bytes().to_vec();
        payload.push(0); // invalid zlib is immaterial: the size prefix must reject first
        let mut client = client_receiving_frame(SMSG_COMPRESSED_MOVES_OPCODE, &payload);

        let error = client
            .recv()
            .expect_err("an oversized decompressed frame must fail the session");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("opcode=0x02FB"),
            "missing opcode: {diagnostic}"
        );
        assert!(
            diagnostic.contains("payload[0..5]=01 00 40 00 00"),
            "missing frame preview: {diagnostic}"
        );
        assert!(
            diagnostic.contains("declares 4194305 decompressed bytes; limit is 4194304"),
            "missing bounded-allocation diagnosis: {diagnostic}"
        );
    }

    #[test]
    fn corrupt_compressed_frame_becomes_a_diagnostic_instead_of_a_decoder_panic() {
        let mut payload = 32u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mut client = client_receiving_frame(SMSG_COMPRESSED_MOVES_OPCODE, &payload);

        let error = client
            .recv()
            .expect_err("corrupt zlib must fail only this session");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("opcode=0x02FB"),
            "missing opcode: {diagnostic}"
        );
        assert!(
            diagnostic.contains("payload[0..8]=20 00 00 00 DE AD BE EF"),
            "missing frame preview: {diagnostic}"
        );
        assert!(
            diagnostic.contains("decoder panicked"),
            "missing panic diagnosis: {diagnostic}"
        );
    }

    #[test]
    fn guarded_reader_preserves_header_cipher_state_across_valid_frames() {
        let mut client = client_receiving_frames(&[
            (0x004D, &[]), // SMSG_LOGOUT_COMPLETE
            (0x004F, &[]), // SMSG_LOGOUT_CANCEL_ACK
        ]);
        assert!(matches!(
            client.recv().unwrap(),
            WorldSmsg::SMSG_LOGOUT_COMPLETE
        ));
        assert!(matches!(
            client.recv().unwrap(),
            WorldSmsg::SMSG_LOGOUT_CANCEL_ACK
        ));
    }

    /// Remove every file this test wrote so re-running the suite doesn't accumulate cruft in the
    /// real `/tmp/wire-crash/` (these tests deliberately exercise the REAL dump path, not an
    /// env-var-overridden temp dir, since `crash_dump_dir()` reads `WIRE_CRASH_DIR` once per call
    /// and mutating process env vars from parallel test threads would just trade one race for
    /// another).
    fn cleanup_dumps_labeled(label: &str) {
        let dir = crash_dump_dir();
        let prefix = format!("{}-", sanitize_label(label));
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Issue #209 deliverable 1: on the SAME "decoder panicked" failure
    /// [`corrupt_compressed_frame_becomes_a_diagnostic_instead_of_a_decoder_panic`] pins, the session
    /// must ALSO dump a file recording the frames that led into it — not just the inline error
    /// string. This is the actual point of the capture: turning "it crashed" into "here is the exact
    /// byte sequence", per two clean decodes then the corrupt one.
    #[test]
    fn a_fatal_decode_dumps_the_preceding_ring_and_the_failing_frame_to_disk() {
        let label = "CRASHTEST-basic";
        cleanup_dumps_labeled(label);

        let mut corrupt = 32u32.to_le_bytes().to_vec();
        corrupt.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mut client = client_receiving_frames_labeled(
            &[
                (0x004D, &[]),                            // SMSG_LOGOUT_COMPLETE (clean)
                (0x004F, &[]),                            // SMSG_LOGOUT_CANCEL_ACK (clean)
                (SMSG_COMPRESSED_MOVES_OPCODE, &corrupt), // the fatal one
            ],
            label,
        );
        assert!(matches!(
            client.recv().unwrap(),
            WorldSmsg::SMSG_LOGOUT_COMPLETE
        ));
        assert!(matches!(
            client.recv().unwrap(),
            WorldSmsg::SMSG_LOGOUT_CANCEL_ACK
        ));
        let err = client
            .recv()
            .expect_err("the corrupt frame must end the session");
        assert!(is_unsafe_server_frame(&err));

        let dir = crash_dump_dir();
        let prefix = format!("{}-", sanitize_label(label));
        let dumped: Vec<_> = std::fs::read_dir(&dir)
            .expect("dump dir must exist after a fatal decode")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .collect();
        assert_eq!(
            dumped.len(),
            1,
            "expected exactly one dump file for this session's one failure"
        );
        let text = std::fs::read_to_string(dumped[0].path()).unwrap();

        assert!(
            text.contains(&format!("session: {label}")),
            "missing session label: {text}"
        );
        assert!(
            text.contains("last 2 decoded frames"),
            "missing ring size header: {text}"
        );
        assert!(
            text.contains("opcode=0x004D declared_body_len=0 actual_body_len=0"),
            "missing first ring entry: {text}"
        );
        assert!(
            text.contains("opcode=0x004F declared_body_len=0 actual_body_len=0"),
            "missing second ring entry: {text}"
        );
        assert!(
            !text.contains("MISMATCH"),
            "no real mismatch occurred here: {text}"
        );
        assert!(
            text.contains("failing frame opcode: 0x02FB"),
            "missing failing opcode: {text}"
        );
        assert!(
            text.contains("failing body[0..8]: 20 00 00 00 DE AD BE EF"),
            "missing failing body preview: {text}"
        );
        assert!(
            text.contains("decoder panicked"),
            "missing the actual error text: {text}"
        );

        cleanup_dumps_labeled(label);
    }

    /// Mock [`Read`] for the #209 read-timeout-fix tests: a scripted sequence of "read steps" that
    /// can deliver fewer bytes than requested (a legitimate short read, same as a real socket under
    /// load) or fail with a `WouldBlock` error (what `TcpStream` read-timeout expiry actually
    /// produces). Once the script is exhausted it falls back to delivering everything remaining in
    /// one shot, so a test only has to script the interesting fragmentation points.
    struct ScriptedReader {
        data: Vec<u8>,
        pos: usize,
        script: std::collections::VecDeque<ReadStep>,
    }

    enum ReadStep {
        /// Deliver up to this many bytes (capped by the caller's buffer and what's left).
        Bytes(usize),
        /// Fail this call exactly like an expired socket read timeout.
        Timeout,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let take_from = |pos: &mut usize, data: &[u8], buf: &mut [u8], n: usize| {
                let take = n.min(buf.len()).min(data.len() - *pos);
                buf[..take].copy_from_slice(&data[*pos..*pos + take]);
                *pos += take;
                take
            };
            match self.script.pop_front() {
                Some(ReadStep::Timeout) => Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "would block (scripted timeout)",
                )),
                Some(ReadStep::Bytes(n)) => Ok(take_from(&mut self.pos, &self.data, buf, n)),
                None => Ok(take_from(&mut self.pos, &self.data, buf, buf.len())),
            }
        }
    }

    /// Raw wire bytes for a fixed 3-frame sequence shared by the fragmentation tests below: an
    /// empty-body frame, an 8-byte-body frame (so body fragmentation has something to bite), then
    /// another empty-body frame.
    fn fragmentation_test_wire(enc: &mut EncrypterHalf) -> Vec<u8> {
        let mut wire = Vec::new();
        wire.extend_from_slice(&enc.encrypt_server_header(2, 0x004D)); // SMSG_LOGOUT_COMPLETE
        wire.extend_from_slice(&enc.encrypt_server_header(10, 0x01CD)); // SMSG_PLAYED_TIME
        wire.extend_from_slice(&1234u32.to_le_bytes());
        wire.extend_from_slice(&99u32.to_le_bytes());
        wire.extend_from_slice(&enc.encrypt_server_header(2, 0x004F)); // SMSG_LOGOUT_CANCEL_ACK
        wire
    }

    /// Drive [`read_guarded_world_message`] the way `WireClient::recv`'s callers actually do under a
    /// socket read timeout (`recv_for`, `recv_raw_for`, and the hand-rolled loops `is_read_timeout`
    /// exists for): swallow a timeout error and call straight back in with the SAME `dec`/`log` —
    /// exactly the retry shape that exposed issue #209 (a lost partial read permanently desyncing the
    /// header cipher). Returns the decoded messages in order; panics on any OTHER kind of error.
    fn drain_with_retries(
        r: &mut ScriptedReader,
        dec: &mut DecrypterHalf,
        log: &mut FrameLog,
        count: usize,
    ) -> Vec<WorldSmsg> {
        let mut out = Vec::new();
        let mut retries = 0;
        while out.len() < count {
            match read_guarded_world_message(r, dec, log) {
                Ok(m) => out.push(m),
                Err(e) => {
                    let msg = e.to_string().to_lowercase();
                    assert!(
                        msg.contains("would block"),
                        "unexpected non-timeout error: {e}"
                    );
                    retries += 1;
                    assert!(
                        retries < 1000,
                        "retried too many times — likely an infinite loop"
                    );
                }
            }
        }
        out
    }

    /// Issue #209's actual fix, pinned: fragmenting the exact same wire bytes with 1-byte dribbles
    /// and timeouts landing mid-header and mid-body — then retrying exactly like a real caller does
    /// — must decode to the IDENTICAL sequence of messages as reading the same bytes with no
    /// fragmentation at all. Before the fix, a timeout mid multi-byte read silently dropped the bytes
    /// already read and resumed at the wrong stream offset, permanently desyncing the header cipher;
    /// this pins that it no longer does.
    #[test]
    fn resumable_read_survives_adversarial_fragmentation_and_matches_the_unfragmented_decode() {
        let (_, crypto) =
            ProofSeed::new().into_client_header_crypto(&ns("TEST").unwrap(), [7; 40], 0x1234_5678);
        let (mut enc, dec) = crypto.split();
        let wire = fragmentation_test_wire(&mut enc);

        // Baseline: the mock's "script exhausted" fallback delivers everything a single `read()`
        // call asks for — i.e. no artificial fragmentation at all.
        let mut baseline_reader = ScriptedReader {
            data: wire.clone(),
            pos: 0,
            script: Default::default(),
        };
        let mut baseline_dec = dec.clone();
        let mut baseline_log = FrameLog::new("BASELINE");
        let baseline = drain_with_retries(
            &mut baseline_reader,
            &mut baseline_dec,
            &mut baseline_log,
            3,
        );

        // Adversarial: 1-byte dribbles, a timeout mid-header (on two different frames), and a
        // timeout mid-body (twice, on the 8-byte-body frame) — then the script runs out and the
        // fallback delivers the rest (frame 3's header) in one shot.
        let script = std::collections::VecDeque::from([
            ReadStep::Bytes(1), // header1 byte 0
            ReadStep::Timeout,  // timeout mid-header1
            ReadStep::Bytes(1), // header1 byte 1
            ReadStep::Bytes(2), // header1 bytes 2-3 (complete) — empty body, no body read
            ReadStep::Bytes(1), // header2 byte 0
            ReadStep::Bytes(1), // header2 byte 1
            ReadStep::Timeout,  // timeout mid-header2
            ReadStep::Bytes(2), // header2 bytes 2-3 (complete)
            ReadStep::Bytes(3), // body2 bytes 0-2 (of 8)
            ReadStep::Timeout,  // timeout mid-body2
            ReadStep::Bytes(2), // body2 bytes 3-4
            ReadStep::Timeout,  // timeout mid-body2 again
            ReadStep::Bytes(3), // body2 bytes 5-7 (complete)
        ]);
        let mut adversarial_reader = ScriptedReader {
            data: wire,
            pos: 0,
            script,
        };
        let mut adversarial_dec = dec.clone();
        let mut adversarial_log = FrameLog::new("ADVERSARIAL");
        let adversarial = drain_with_retries(
            &mut adversarial_reader,
            &mut adversarial_dec,
            &mut adversarial_log,
            3,
        );

        assert_eq!(baseline.len(), 3);
        assert_eq!(
            baseline, adversarial,
            "fragmented decode diverged from the clean decode"
        );
        assert!(matches!(baseline[0], WorldSmsg::SMSG_LOGOUT_COMPLETE));
        assert!(matches!(baseline[2], WorldSmsg::SMSG_LOGOUT_CANCEL_ACK));
        match &baseline[1] {
            WorldSmsg::SMSG_PLAYED_TIME(p) => {
                assert_eq!(p.total_played_time, 1234);
                assert_eq!(p.level_played_time, 99);
            }
            other => panic!("expected SMSG_PLAYED_TIME, got {other:?}"),
        }
    }

    /// A single byte at a time, for EVERY byte in the frame (not just at a few chosen points) — the
    /// most extreme legal fragmentation a real (if pathological) socket could produce, with no
    /// timeouts involved at all. Guards the resumable-fill loop itself, independent of the
    /// timeout-retry path the test above exercises.
    #[test]
    fn resumable_read_survives_single_byte_dribbles_with_no_timeouts() {
        let (_, crypto) =
            ProofSeed::new().into_client_header_crypto(&ns("TEST").unwrap(), [7; 40], 0x1234_5678);
        let (mut enc, dec) = crypto.split();
        let wire = fragmentation_test_wire(&mut enc);

        let mut baseline_reader = ScriptedReader {
            data: wire.clone(),
            pos: 0,
            script: Default::default(),
        };
        let mut baseline_dec = dec.clone();
        let mut baseline_log = FrameLog::new("BASELINE-DRIBBLE");
        let baseline = drain_with_retries(
            &mut baseline_reader,
            &mut baseline_dec,
            &mut baseline_log,
            3,
        );

        let script =
            std::collections::VecDeque::from_iter((0..wire.len()).map(|_| ReadStep::Bytes(1)));
        let mut dribble_reader = ScriptedReader {
            data: wire,
            pos: 0,
            script,
        };
        let mut dribble_dec = dec.clone();
        let mut dribble_log = FrameLog::new("DRIBBLE");
        let dribbled =
            drain_with_retries(&mut dribble_reader, &mut dribble_dec, &mut dribble_log, 3);

        assert_eq!(
            baseline, dribbled,
            "byte-at-a-time decode diverged from the clean decode"
        );
    }

    /// Found live during the #209 repro (not in the original hunt list): a connection that's
    /// genuinely CLOSED (the peer hung up — nothing to do with #209's decode-desync) used to fall
    /// into the same `skipped`/64-strikes bucket as an ordinary undecodable frame. A closed socket's
    /// reads return instantly (no blocking), so a caller that polls `recv()` once per tick for the
    /// rest of a benchmark's hold window hit the 64-skip threshold on EVERY call and wrote a fresh
    /// crash dump EVERY time — one dead session produced thousands of files (observed on the live
    /// 150-mage repro), burying the real corruption signal (there wasn't any left) under noise. This
    /// pins that a closed connection fails fast, every time, and is never mistaken for #209 evidence.
    #[test]
    fn a_closed_connection_is_terminal_and_does_not_flood_crash_dumps() {
        let label = "CRASHTEST-closed";
        cleanup_dumps_labeled(label);

        let mut client = client_receiving_frames_labeled(&[(0x004D, &[])], label); // one clean frame, then the server drops
        assert!(matches!(
            client.recv().unwrap(),
            WorldSmsg::SMSG_LOGOUT_COMPLETE
        ));

        for _ in 0..10 {
            let err = client
                .recv()
                .expect_err("a closed connection must not be swallowed as a skip");
            assert!(
                !is_unsafe_server_frame(&err),
                "a closed connection is not decode corruption: {err}"
            );
            assert!(
                err.to_string().contains("connection closed"),
                "unexpected error: {err}"
            );
        }

        let dir = crash_dump_dir();
        let prefix = format!("{}-", sanitize_label(label));
        let dumped = std::fs::read_dir(&dir)
            .ok()
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(
            dumped, 0,
            "a closed connection is not decode evidence — it must not write a dump"
        );
    }

    /// The ring buffer is BOUNDED (`CRASH_RING_CAPACITY`) — a long-lived session must not grow its
    /// dump file without bound; only the frames closest to the crash matter for this probe.
    #[test]
    fn the_ring_buffer_keeps_only_the_last_32_frames() {
        let label = "CRASHTEST-ring-cap";
        cleanup_dumps_labeled(label);

        let mut frames: Vec<(u16, &[u8])> = vec![(0x004D, &[]); 40]; // 40 clean frames > capacity
        let mut corrupt = 32u32.to_le_bytes().to_vec();
        corrupt.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        frames.push((SMSG_COMPRESSED_MOVES_OPCODE, &corrupt));
        let mut client = client_receiving_frames_labeled(&frames, label);

        for _ in 0..40 {
            client
                .recv()
                .expect("the first 40 frames all decode cleanly");
        }
        let err = client
            .recv()
            .expect_err("frame 41 is the fatal corrupt one");
        assert!(is_unsafe_server_frame(&err));

        let dir = crash_dump_dir();
        let prefix = format!("{}-", sanitize_label(label));
        let dumped = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .expect("a dump file must exist");
        let text = std::fs::read_to_string(dumped.path()).unwrap();

        assert!(
            text.contains("last 32 decoded frames"),
            "ring must cap at 32: {text}"
        );
        let entry_count = text.matches("opcode=0x004D").count();
        assert_eq!(
            entry_count, 32,
            "expected exactly 32 surviving ring entries, got {entry_count}"
        );

        cleanup_dumps_labeled(label);
    }
}

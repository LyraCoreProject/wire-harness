//! Headless 1.12.1 (build 5875) wire test-client for the spacetime-core gateway.
//!
//! Speaks the real WoW protocol — SRP6 logon (port 3724) then the encrypted world session
//! (port 8085) — so tests can drive CMSG and ASSERT on decoded SMSG, instead of QAing
//! through the wine client. It is the client-side routines already proven in
//! `gateway/src/logon/mod.rs` tests + `gateway/src/world/tests.rs::client_handshake`,
//! lifted onto real `TcpStream`s. All blocking std I/O — no async.

use std::net::TcpStream;
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
use wow_world_messages::vanilla::{
    Class, Gender, Language, LogoutResult, Race, SpellCastTargets,
    SpellCastTargets_SpellCastTargetFlags, SpellCastTargets_SpellCastTargetFlags_Unit, WorldResult,
    CMSG_AUTH_SESSION, CMSG_CAST_SPELL, CMSG_CHAR_CREATE, CMSG_CHAR_DELETE, CMSG_CHAR_ENUM, CMSG_GOSSIP_HELLO,
    CMSG_ITEM_QUERY_SINGLE, CMSG_LOGOUT_REQUEST, CMSG_MESSAGECHAT, CMSG_MESSAGECHAT_ChatType,
    CMSG_NPC_TEXT_QUERY, CMSG_PLAYED_TIME, CMSG_PLAYER_LOGIN, CMSG_REPOP_REQUEST, CMSG_SET_SELECTION, CMSG_WHO,
    SMSG_AUTH_RESPONSE,
};
use wow_world_messages::vanilla::ClientMessage;
use wow_world_messages::Guid;

const LOGON_PORT: u16 = 3724;
pub const DEFAULT_WORLD_ADDR: &str = "127.0.0.1:8085";

fn ns(s: &str) -> Result<NormalizedString> {
    NormalizedString::new(s).map_err(|e| anyhow!("normalized string {s:?}: {e:?}"))
}

/// Complete SRP6 logon and return `(session key K, world server address)`.
/// Mirrors `gateway/src/logon/mod.rs` `full_srp6_handshake_and_realm_list`.
pub fn logon(account: &str, password: &str) -> Result<([u8; 40], String)> {
    let mut s = TcpStream::connect(("127.0.0.1", LOGON_PORT))
        .with_context(|| format!("connect logon :{LOGON_PORT}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;

    CMD_AUTH_LOGON_CHALLENGE_Client {
        protocol_version: ProtocolVersion::Three,
        version: Version { major: 1, minor: 12, patch: 1, build: 5875 },
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
    /// Spell ids from SMSG_INITIAL_SPELLS, captured during the `player_login` burst drain.
    pub initial_spells: Vec<u32>,
    /// Per-slot (standing, flag-empty) from SMSG_INITIALIZE_FACTIONS, captured during the
    /// `player_login` burst drain — index is the Faction.dbc reputation_index (0..63), value is
    /// the raw i32 standing (work-item #076: relog rep-restore verification).
    pub init_factions: Vec<i32>,
}

impl WireClient {
    /// One-shot bring-up: logon -> world handshake -> create-or-find `char_name` of
    /// `class` -> player login. Leaves the client in-world.
    pub fn login_as(
        account: &str,
        password: &str,
        char_name: &str,
        class: Class,
    ) -> Result<Self> {
        let (k, world_addr) = logon(account, password)?;
        let mut c = Self::connect_world(&world_addr, account, k)?;
        let guid = c.create_or_find_char(char_name, class)?;
        c.player_login(guid)?;
        Ok(c)
    }

    /// The world handshake: plaintext SMSG_AUTH_CHALLENGE -> CMSG_AUTH_SESSION -> encrypted
    /// SMSG_AUTH_RESPONSE(AuthOk). Mirrors `world/tests.rs::client_handshake`.
    pub fn connect_world(world_addr: &str, account: &str, k: [u8; 40]) -> Result<Self> {
        let mut stream =
            TcpStream::connect(world_addr).with_context(|| format!("connect world {world_addr}"))?;
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

        match WorldSmsg::read_encrypted(&mut stream, &mut dec)? {
            WorldSmsg::SMSG_AUTH_RESPONSE(r) if matches!(*r, SMSG_AUTH_RESPONSE::AuthOk { .. }) => {}
            other => bail!("world auth rejected: {other}"),
        }
        Ok(Self {
            stream,
            enc,
            dec,
            self_guid: 0,
            seen_guids: Vec::new(),
            initial_spells: Vec::new(),
            init_factions: Vec::new(),
        })
    }

    /// Send an encrypted CMSG.
    pub fn send<M: ClientMessage>(&mut self, m: &M) -> Result<()> {
        m.write_encrypted_client(&mut self.stream, &mut self.enc)
            .map_err(|e| anyhow!("send: {e}"))?;
        Ok(())
    }

    /// Read the next raw encrypted frame without gtker-decoding the payload.
    /// Returns `(opcode, payload_bytes)`. Advances the cipher state exactly like `recv()`,
    /// so `recv()` and `recv_raw()` can be interleaved freely — one packet at a time.
    /// Use this for packets gtker rejects (e.g. TYPE-less partial-VALUES updates).
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
            match WorldSmsg::read_encrypted(&mut self.stream, &mut self.dec) {
                Ok(m) => {
                    self.note_guids(&m);
                    return Ok(m);
                }
                Err(e) => {
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
                    skipped += 1;
                    if skipped > 64 {
                        return Err(anyhow!("recv: stream desync/closed after 64 skips: {e}"));
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

    fn note_guids(&mut self, m: &WorldSmsg) {
        if let WorldSmsg::SMSG_UPDATE_OBJECT(u) = m {
            for o in &u.objects {
                if let Some(g) = create_object_guid(o) {
                    if !self.seen_guids.contains(&g) {
                        self.seen_guids.push(g);
                    }
                }
            }
        }
    }

    /// Request the character list. Returns `(guid, name, class)` per character.
    pub fn char_enum(&mut self) -> Result<Vec<(u64, String, Class)>> {
        self.send(&CMSG_CHAR_ENUM {})?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_ENUM(_)))?;
        let WorldSmsg::SMSG_CHAR_ENUM(e) = m else { unreachable!() };
        Ok(e.characters.iter().map(|c| (c.guid.guid(), c.name.clone(), c.class)).collect())
    }

    /// Request the character list and return the raw equipment display_ids for each character.
    /// Returns `(guid, name, display_ids)` where display_ids is a 19-element vec indexed by
    /// equipment slot (slot 15 = main-hand weapon).
    /// Used to verify SMSG_CHAR_ENUM carries real display_ids (not all-zero).
    pub fn char_enum_gear(&mut self) -> Result<Vec<(u64, String, Vec<u32>)>> {
        self.send(&CMSG_CHAR_ENUM {})?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_ENUM(_)))?;
        let WorldSmsg::SMSG_CHAR_ENUM(e) = m else { unreachable!() };
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
        if let Some((g, _, _)) =
            self.char_enum()?.into_iter().find(|(_, n, _)| n.eq_ignore_ascii_case(name))
        {
            return Ok(g);
        }
        self.send(&CMSG_CHAR_CREATE {
            name: name.to_string(),
            race: Race::Human,
            class,
            gender: Gender::Male,
            skin_color: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        })?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_CREATE(_)))?;
        let WorldSmsg::SMSG_CHAR_CREATE(r) = m else { unreachable!() };
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
        self.send(&CMSG_CHAR_DELETE { guid: Guid::new(guid) })?;
        let m = self.recv_until(|m| matches!(m, WorldSmsg::SMSG_CHAR_DELETE(_)))?;
        let WorldSmsg::SMSG_CHAR_DELETE(r) = m else { unreachable!() };
        Ok(r.result)
    }

    /// Enter the world as `guid`, draining the post-login burst up to (and including) the
    /// self CREATE_OBJECT. Sets `self_guid`.
    pub fn player_login(&mut self, guid: u64) -> Result<()> {
        self.send(&CMSG_PLAYER_LOGIN { guid: Guid::new(guid) })?;
        // The burst starts with SMSG_LOGIN_VERIFY_WORLD and ends with the self CREATE_OBJECT.
        self.recv_until(|m| matches!(m, WorldSmsg::SMSG_LOGIN_VERIFY_WORLD(_)))?;
        self.self_guid = guid;
        // Drain through the self-spawn CREATE_OBJECT so subsequent reads are gameplay traffic, capturing
        // SMSG_INITIAL_SPELLS (the client spellbook) en route.
        loop {
            let m = self.recv()?;
            match &m {
                WorldSmsg::SMSG_INITIAL_SPELLS(s) => {
                    self.initial_spells =
                        s.initial_spells.iter().map(|e| u32::from(e.spell_id)).collect();
                }
                WorldSmsg::SMSG_INITIALIZE_FACTIONS(f) => {
                    self.init_factions = f.factions.iter().map(|s| s.standing as i32).collect();
                }
                WorldSmsg::SMSG_UPDATE_OBJECT(u)
                    if u.objects.iter().any(|o| create_object_guid(o) == Some(guid)) =>
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
        self.send(&CMSG_SET_SELECTION { target: Guid::new(target) })
    }

    /// Right-click a gossip NPC (CMSG_GOSSIP_HELLO) — the gateway should reply with
    /// SMSG_GOSSIP_MESSAGE (its quests for a gossip+questgiver like McBride).
    pub fn gossip_hello(&mut self, npc: u64) -> Result<()> {
        self.send(&CMSG_GOSSIP_HELLO { guid: Guid::new(npc) })
    }

    /// Right-click a QUESTGIVER-ONLY NPC (CMSG_QUESTGIVER_HELLO) — the real protocol the client uses
    /// when an NPC has npc_flags QUESTGIVER but NOT GOSSIP (e.g. Deputy Willem). Reply: SMSG_QUESTGIVER_QUEST_LIST.
    pub fn questgiver_hello(&mut self, npc: u64) -> Result<()> {
        self.send(&wow_world_messages::vanilla::CMSG_QUESTGIVER_HELLO { guid: Guid::new(npc) })
    }

    /// Send `CMSG_NPC_TEXT_QUERY` for `text_id` and wait for `SMSG_NPC_TEXT_UPDATE`. Returns the
    /// text in slot 0 (the greeting slot the gateway always fills). Used to verify per-NPC text.
    pub fn npc_text_query(&mut self, text_id: u32, npc_guid: u64) -> Result<String> {
        use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
        self.send(&CMSG_NPC_TEXT_QUERY { text_id, guid: Guid::new(npc_guid) })?;
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
        self.send(&CMSG_ITEM_QUERY_SINGLE { item: item_entry, guid: Guid::new(0) })?;
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
                    return Ok((found.armor, found.block, found.sell_price.as_int(), stats, spell1, trig1, bonding));
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
                        let deadline2 = std::time::Instant::now() + std::time::Duration::from_secs(3);
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
                    let players: Vec<(String, u8, u8, u8)> = r.players.iter().map(|p| {
                        (p.name.clone(), p.level.as_int(), p.class.as_int(), p.race.as_int())
                    }).collect();
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
                    SpellCastTargets_SpellCastTargetFlags_Unit { unit_target: Guid::new(target) },
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

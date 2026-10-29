use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use crate::scenario::{FixtureVerb, Observation, Position, Scenario, SessionAction};
use crate::{is_read_timeout, WireClient};

pub trait FixtureAdapter {
    /// Returns a connected client after fixture setup and the adapter's checks succeed.
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<PreparedClient>;
    fn reconnect(&mut self, _fixture: &FixtureVerb) -> Result<PreparedClient> {
        anyhow::bail!("adapter does not support reconnect")
    }
    fn cleanup(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct PreparedClient {
    pub client: WireClient,
    pub fixtures: BTreeMap<String, u64>,
    pub opening: Vec<Observation>,
}

pub trait Transport {
    /// Monotonic time; receive must advance it when no packet arrives.
    fn now(&self) -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }

    fn fixture_guids(&self) -> BTreeMap<String, u64> {
        BTreeMap::new()
    }
    fn take_opening(&mut self) -> Vec<Observation> {
        Vec::new()
    }
    fn character_guid(&self) -> u64;
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()>;
    /// Returns None when the receive deadline expires; other failures must propagate.
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>>;
}

impl Transport for PreparedClient {
    fn fixture_guids(&self) -> BTreeMap<String, u64> {
        self.fixtures.clone()
    }
    fn take_opening(&mut self) -> Vec<Observation> {
        std::mem::take(&mut self.opening)
    }
    fn character_guid(&self) -> u64 {
        self.client.self_guid
    }
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()> {
        self.client.send_raw(opcode, body)
    }
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        self.client.receive(timeout)
    }
}

impl Transport for WireClient {
    fn character_guid(&self) -> u64 {
        self.self_guid
    }
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()> {
        self.send_raw(opcode, body)
    }
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>> {
        self.set_recv_timeout(timeout)?;
        match self.recv_raw() {
            Ok(packet) => Ok(Some(packet)),
            Err(error) if is_read_timeout(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub struct Run {
    pub character_guid: u64,
    pub observations: Vec<Observation>,
}

pub fn run(scenario: &Scenario, adapter: &mut dyn FixtureAdapter) -> Result<Run> {
    scenario.validate()?;
    ensure!(
        scenario.schema_version == 3,
        "single-Character runner requires schema 3"
    );
    let mut client = adapter.prepare(&scenario.fixtures)?;
    let character_guid = client.client.self_guid;
    let result = run_on(scenario, &mut client);
    drop(client);
    let cleanup = adapter.cleanup();
    let observations = result?;
    cleanup?;
    Ok(Run {
        character_guid,
        observations,
    })
}

pub fn run_on(scenario: &Scenario, transport: &mut impl Transport) -> Result<Vec<Observation>> {
    scenario.validate()?;
    ensure!(
        scenario.schema_version == 3,
        "single-Character runner requires schema 3"
    );
    ensure!(
        transport.character_guid() != 0,
        "scenario requires a Character"
    );
    let mut observations = transport.take_opening();
    ensure!(
        observations.len() <= 4096,
        "opening exceeded 4096 received frames"
    );
    let deadline = transport.now() + Duration::from_millis(scenario.opening.window_ms);
    while let Some(remaining) = deadline.checked_sub(transport.now()) {
        if remaining.is_zero() {
            break;
        }
        let Some((opcode, body)) = transport.receive(remaining)? else {
            break;
        };
        ensure!(
            observations.len() < 4096,
            "opening exceeded 4096 received frames"
        );
        observations.push(Observation::opening(opcode, &body));
    }
    let fixtures = transport.fixture_guids();
    for step in &scenario.steps {
        let (opcode, body) = step
            .send
            .as_ref()
            .context("step needs a packet")?
            .encode(transport.character_guid(), &fixtures)?;
        let sent_at = transport.now();
        transport.send(opcode, &body)?;
        let deadline = transport.now() + Duration::from_millis(step.checkpoint.window_ms);
        let mut previous_sample = sent_at;
        let mut frames = 0;
        while let Some(remaining) = deadline.checked_sub(transport.now()) {
            if remaining.is_zero() {
                break;
            }
            let Some((opcode, body)) = transport.receive(remaining)? else {
                break;
            };
            frames += 1;
            ensure!(frames <= 4096, "checkpoint exceeded 4096 received frames");
            let mut observation = Observation::from_frame_with_rules(
                step,
                opcode,
                &body,
                &scenario.rules.unexpected_packets,
            );
            let received_at = transport.now();
            ensure!(
                received_at >= previous_sample,
                "transport clock moved backwards"
            );
            previous_sample = received_at;
            observation.received_after_ms = Some(
                received_at
                    .checked_sub(sent_at)
                    .context("transport clock moved backwards")?
                    .as_millis()
                    .try_into()?,
            );
            if opcode == 0x00c7 {
                use crate::decode::Field;
                let fields = &observation.packet.fields;
                if let (
                    Some(Field::Guid(guid)),
                    Some(Field::Unsigned(counter)),
                    Some(Field::Unsigned(timestamp)),
                ) = (
                    fields.get("guid"),
                    fields.get("counter"),
                    fields.get("timestamp"),
                ) {
                    if *guid == transport.character_guid() {
                        let mut ack = guid.to_le_bytes().to_vec();
                        ack.extend(u32::try_from(*counter)?.to_le_bytes());
                        ack.extend(u32::try_from(*timestamp)?.to_le_bytes());
                        transport.send(0x00c7, &ack)?;
                    }
                }
            }
            observations.push(observation);
        }
    }
    Ok(observations)
}

pub trait MultiTransport {
    /// One monotonic clock for every Character; receive advances it when no packet arrives.
    fn now(&self) -> Duration;
    fn character_guid(&self, character: &str) -> Result<u64>;
    fn fixture_guids(&self, character: &str) -> Result<BTreeMap<String, u64>>;
    fn take_opening(&mut self, character: &str) -> Result<Vec<Observation>>;
    fn connected(&self, character: &str) -> bool;
    /// Reopens the same Character; its GUID must remain stable across sessions.
    fn connect(&mut self, character: &str) -> Result<()>;
    fn disconnect(&mut self, character: &str) -> Result<()>;
    fn send(&mut self, character: &str, opcode: u32, body: &[u8]) -> Result<()>;
    fn receive(&mut self, character: &str, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>>;
    /// Returns a ready frame without advancing the shared clock to wait for one.
    fn receive_ready(&mut self, character: &str) -> Result<Option<(u16, Vec<u8>)>> {
        self.receive(character, Duration::ZERO)
    }
}

pub fn run_multi_on(
    scenario: &Scenario,
    transport: &mut impl MultiTransport,
) -> Result<Vec<Observation>> {
    scenario.validate()?;
    ensure!(
        scenario.schema_version == 4,
        "named Character runner requires schema 4"
    );
    let mut observations = Vec::new();
    for character in &scenario.characters {
        let mut opening = transport.take_opening(&character.id)?;
        for frame in &mut opening {
            frame.character = Some(character.id.clone());
        }
        observations.extend(opening);
    }
    ensure!(
        observations.len() <= 4096 * scenario.characters.len(),
        "opening exceeded frame limit"
    );
    let opening_deadline = transport.now() + Duration::from_millis(scenario.opening.window_ms);
    collect_opening(scenario, transport, opening_deadline, &mut observations)?;
    let mut positions: BTreeMap<String, Position> = scenario
        .characters
        .iter()
        .map(|c| {
            let FixtureVerb::Character { at, .. } = &c.fixtures[0] else {
                unreachable!()
            };
            (c.id.clone(), at.clone())
        })
        .collect();
    for step in &scenario.steps {
        let actor = step
            .character
            .as_deref()
            .context("step needs a Character")?;
        let started = transport.now();
        let deadline = started + Duration::from_millis(step.checkpoint.window_ms);
        if let Some(send) = &step.send {
            ensure!(transport.connected(actor), "step actor is disconnected");
            let characters = scenario
                .characters
                .iter()
                .map(|character| {
                    Ok((
                        character.id.clone(),
                        transport.character_guid(&character.id)?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            let (opcode, body) = send.encode_with(
                transport.character_guid(actor)?,
                &transport.fixture_guids(actor)?,
                &characters,
            )?;
            transport.send(actor, opcode, &body)?;
        } else if let Some(action) = &step.action {
            match action {
                SessionAction::Connect => {
                    ensure!(
                        !transport.connected(actor),
                        "Character is already connected"
                    );
                    transport.connect(actor)?;
                }
                SessionAction::Disconnect => {
                    ensure!(transport.connected(actor), "Character is disconnected");
                    transport.disconnect(actor)?;
                }
                SessionAction::Reconnect => {
                    ensure!(transport.connected(actor), "Character is disconnected");
                    transport.disconnect(actor)?;
                    transport.connect(actor)?;
                }
                SessionAction::Walk { to, speed } => {
                    ensure!(transport.connected(actor), "walker is disconnected");
                    let from = positions.get(actor).context("walker has no position")?;
                    ensure!(from.map == to.map, "walk cannot change maps");
                    let distance = ((to.x - from.x).powi(2)
                        + (to.y - from.y).powi(2)
                        + (to.z - from.z).powi(2))
                    .sqrt();
                    let duration_ms = ((distance / speed) * 1000.0).ceil() as u64;
                    ensure!(
                        duration_ms > 0 && duration_ms < step.checkpoint.window_ms,
                        "walk exceeds checkpoint window"
                    );
                    let orientation = (to.y - from.y).atan2(to.x - from.x);
                    transport.send(
                        actor,
                        0x00b5,
                        &movement(from, orientation, started.as_millis() as u32, true),
                    )?;
                    let mut elapsed = 200;
                    while elapsed < duration_ms {
                        collect_multi(
                            scenario,
                            step,
                            transport,
                            started + Duration::from_millis(elapsed),
                            started,
                            &mut observations,
                        )?;
                        let fraction = elapsed as f32 / duration_ms as f32;
                        let at = Position {
                            map: from.map,
                            x: from.x + (to.x - from.x) * fraction,
                            y: from.y + (to.y - from.y) * fraction,
                            z: from.z + (to.z - from.z) * fraction,
                            orientation,
                        };
                        transport.send(
                            actor,
                            0x00ee,
                            &movement(&at, orientation, transport.now().as_millis() as u32, true),
                        )?;
                        elapsed += 200;
                    }
                    collect_multi(
                        scenario,
                        step,
                        transport,
                        started + Duration::from_millis(duration_ms),
                        started,
                        &mut observations,
                    )?;
                    transport.send(
                        actor,
                        0x00b7,
                        &movement(to, orientation, transport.now().as_millis() as u32, false),
                    )?;
                    positions.insert(actor.to_owned(), to.clone());
                }
            }
        }
        let mut opening = transport.take_opening(actor)?;
        for mut observed in opening.drain(..) {
            observed.character = Some(actor.to_owned());
            observed.checkpoint = step.id.clone();
            observed.stream = step
                .checkpoint
                .for_character(actor)
                .unwrap()
                .stream(&step.id, observed.packet.opcode);
            if observed.stream == crate::scenario::Stream::Unexpected {
                observed.stream = crate::scenario::Stream::Opening;
            }
            observed.received_after_ms = Some(
                transport
                    .now()
                    .checked_sub(started)
                    .context("clock moved backwards")?
                    .as_millis()
                    .try_into()?,
            );
            observations.push(observed);
        }
        collect_multi(
            scenario,
            step,
            transport,
            deadline,
            started,
            &mut observations,
        )?;
    }
    Ok(observations)
}

fn movement(at: &Position, orientation: f32, timestamp: u32, forward: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(28);
    body.extend((if forward { 1u32 } else { 0 }).to_le_bytes());
    body.extend(timestamp.to_le_bytes());
    for value in [at.x, at.y, at.z, orientation, 0.0] {
        body.extend(value.to_le_bytes());
    }
    body
}

fn collect_opening(
    scenario: &Scenario,
    transport: &mut impl MultiTransport,
    deadline: Duration,
    observations: &mut Vec<Observation>,
) -> Result<()> {
    loop {
        if transport.now() >= deadline {
            break;
        }
        let mut ready = false;
        for character in &scenario.characters {
            if transport.now() >= deadline {
                break;
            }
            if !transport.connected(&character.id) {
                continue;
            }
            let before = transport.now();
            let frame = transport.receive_ready(&character.id)?;
            ensure!(transport.now() >= before, "transport clock moved backwards");
            if let Some((opcode, body)) = frame {
                let mut observed = Observation::opening(opcode, &body);
                observed.character = Some(character.id.clone());
                observations.push(observed);
                ready = true;
            }
        }
        ensure!(
            observations.len() <= 4096 * scenario.characters.len(),
            "opening exceeded frame limit"
        );
        if ready {
            continue;
        }
        if transport.now() >= deadline {
            break;
        }
        let connected = scenario
            .characters
            .iter()
            .filter(|character| transport.connected(&character.id))
            .count();
        ensure!(
            connected > 0,
            "no connected Character can advance the clock"
        );
        let before = transport.now();
        let mut remaining_characters = connected;
        let mut received = false;
        for character in &scenario.characters {
            if !transport.connected(&character.id) {
                continue;
            }
            let remaining = deadline.saturating_sub(transport.now());
            if remaining.is_zero() {
                break;
            }
            let timeout = (remaining / remaining_characters as u32)
                .min(Duration::from_millis(20))
                .max(Duration::from_millis(1));
            remaining_characters -= 1;
            let polled_at = transport.now();
            let frame = transport.receive(&character.id, timeout)?;
            ensure!(
                transport.now() >= polled_at,
                "transport clock moved backwards"
            );
            if let Some((opcode, body)) = frame {
                let mut observed = Observation::opening(opcode, &body);
                observed.character = Some(character.id.clone());
                observations.push(observed);
                received = true;
                break;
            }
        }
        ensure!(
            transport.now() > before || received,
            "transport clock did not advance"
        );
    }
    Ok(())
}

fn collect_multi(
    scenario: &Scenario,
    step: &crate::scenario::Step,
    transport: &mut impl MultiTransport,
    deadline: Duration,
    started: Duration,
    observations: &mut Vec<Observation>,
) -> Result<()> {
    let mut frames = 0;
    loop {
        if transport.now() >= deadline {
            break;
        }
        let mut ready = false;
        for character in &scenario.characters {
            if transport.now() >= deadline {
                break;
            }
            if !transport.connected(&character.id) {
                continue;
            }
            let before = transport.now();
            let frame = transport.receive_ready(&character.id)?;
            ensure!(transport.now() >= before, "transport clock moved backwards");
            if let Some(frame) = frame {
                record_multi_frame(
                    scenario,
                    step,
                    transport,
                    started,
                    observations,
                    character,
                    frame,
                )?;
                frames += 1;
                ready = true;
            }
        }
        ensure!(
            frames <= 4096 * scenario.characters.len(),
            "checkpoint exceeded frame limit"
        );
        if ready {
            continue;
        }
        if transport.now() >= deadline {
            break;
        }
        let connected = scenario
            .characters
            .iter()
            .filter(|character| transport.connected(&character.id))
            .count();
        ensure!(
            connected > 0,
            "no connected Character can advance the clock"
        );
        let before = transport.now();
        let mut remaining_characters = connected;
        let mut received = false;
        for character in &scenario.characters {
            if !transport.connected(&character.id) {
                continue;
            }
            let remaining = deadline.saturating_sub(transport.now());
            if remaining.is_zero() {
                break;
            }
            let timeout = (remaining / remaining_characters as u32)
                .min(Duration::from_millis(20))
                .max(Duration::from_millis(1));
            remaining_characters -= 1;
            let polled_at = transport.now();
            let frame = transport.receive(&character.id, timeout)?;
            ensure!(
                transport.now() >= polled_at,
                "transport clock moved backwards"
            );
            if let Some(frame) = frame {
                record_multi_frame(
                    scenario,
                    step,
                    transport,
                    started,
                    observations,
                    character,
                    frame,
                )?;
                frames += 1;
                received = true;
                break;
            }
        }
        ensure!(
            frames <= 4096 * scenario.characters.len(),
            "checkpoint exceeded frame limit"
        );
        ensure!(
            transport.now() > before || received,
            "transport clock did not advance"
        );
    }
    Ok(())
}

fn record_multi_frame(
    scenario: &Scenario,
    step: &crate::scenario::Step,
    transport: &mut impl MultiTransport,
    started: Duration,
    observations: &mut Vec<Observation>,
    character: &crate::scenario::ScenarioCharacter,
    (opcode, body): (u16, Vec<u8>),
) -> Result<()> {
    let mut observation = Observation::from_frame_for_rules(
        step,
        Some(&character.id),
        opcode,
        &body,
        &scenario.rules.unexpected_packets,
    );
    let received_at = transport.now();
    ensure!(received_at >= started, "transport clock moved backwards");
    observation.received_after_ms = Some(
        received_at
            .checked_sub(started)
            .unwrap()
            .as_millis()
            .try_into()?,
    );
    if opcode == 0x00c7 {
        use crate::decode::Field;
        let fields = &observation.packet.fields;
        if let (
            Some(Field::Guid(guid)),
            Some(Field::Unsigned(counter)),
            Some(Field::Unsigned(timestamp)),
        ) = (
            fields.get("guid"),
            fields.get("counter"),
            fields.get("timestamp"),
        ) {
            if *guid == transport.character_guid(&character.id)? {
                let mut ack = guid.to_le_bytes().to_vec();
                ack.extend(u32::try_from(*counter)?.to_le_bytes());
                ack.extend(u32::try_from(*timestamp)?.to_le_bytes());
                transport.send(&character.id, 0x00c7, &ack)?;
            }
        }
    }
    observations.push(observation);
    Ok(())
}

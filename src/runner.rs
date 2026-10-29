use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use crate::scenario::{FixtureVerb, Observation, Scenario};
use crate::{is_read_timeout, WireClient};

pub trait FixtureAdapter {
    /// Returns a connected client after fixture setup and the adapter's checks succeed.
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<PreparedClient>;
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
        let (opcode, body) = step.send.encode(transport.character_guid(), &fixtures)?;
        let sent_at = transport.now();
        transport.send(opcode, &body)?;
        let deadline = transport.now() + Duration::from_millis(step.checkpoint.window_ms);
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
            let mut observation = Observation::from_frame(step, opcode, &body);
            observation.received_after_ms = Some(
                transport
                    .now()
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

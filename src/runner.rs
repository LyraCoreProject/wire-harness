use std::time::{Duration, Instant};

use anyhow::{ensure, Result};

use crate::scenario::{FixtureVerb, Observation, Scenario};
use crate::{is_read_timeout, WireClient};

pub trait FixtureAdapter {
    /// Returns a connected client after every requested fixture has been applied and checked.
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<WireClient>;
}

pub trait Transport {
    fn character_guid(&self) -> u64;
    fn send(&mut self, opcode: u32, body: &[u8]) -> Result<()>;
    /// Returns None when the receive deadline expires; other failures must propagate.
    fn receive(&mut self, timeout: Duration) -> Result<Option<(u16, Vec<u8>)>>;
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

pub fn run(scenario: &Scenario, adapter: &mut dyn FixtureAdapter) -> Result<Vec<Observation>> {
    scenario.validate()?;
    let mut client = adapter.prepare(&scenario.fixtures)?;
    run_on(scenario, &mut client)
}

pub fn run_on(scenario: &Scenario, transport: &mut impl Transport) -> Result<Vec<Observation>> {
    scenario.validate()?;
    ensure!(
        transport.character_guid() != 0,
        "scenario requires a Character"
    );
    let mut observations = Vec::new();
    for step in &scenario.steps {
        let (opcode, body) = step.send.encode(transport.character_guid());
        transport.send(opcode, &body)?;
        let deadline = Instant::now() + Duration::from_millis(step.checkpoint.window_ms);
        let mut frames = 0;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            let Some((opcode, body)) = transport.receive(remaining)? else {
                break;
            };
            frames += 1;
            ensure!(frames <= 4096, "checkpoint exceeded 4096 received frames");
            observations.push(Observation::from_frame(step, opcode, &body));
        }
    }
    Ok(observations)
}

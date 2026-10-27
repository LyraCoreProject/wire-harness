use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use wire_client::cli::Invocation;
use wire_client::scenario::{FixtureAdapter, FixtureVerb};
use wire_client::{is_read_timeout, WireClient};

pub struct Adapter<'a>(pub &'a Invocation);

impl FixtureAdapter for Adapter<'_> {
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<WireClient> {
        ensure!(fixtures.len() == 1, "one Character fixture is required");
        let fixture = &fixtures[0];
        let FixtureVerb::Character { level, at, .. } = fixture;
        let mut client = super::create(self.0, fixture)?;
        if *level > 1 {
            client.send_say(&format!(".levelup {}", level - 1))?;
        }
        client.send_say(&format!(
            ".go xyzo {} {} {} {} {}",
            at.x, at.y, at.z, at.orientation, at.map
        ))?;
        client.set_recv_timeout(Duration::from_millis(100))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match client.recv() {
                Ok(_) => (),
                Err(error) if is_read_timeout(&error) => (),
                Err(error) => return Err(error),
            }
        }
        drop(client);
        super::reconnect_checked(self.0, fixture)
    }
}

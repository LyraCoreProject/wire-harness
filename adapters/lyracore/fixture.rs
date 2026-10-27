use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::runner::FixtureAdapter;
use wire_client::scenario::FixtureVerb;
use wire_client::WireClient;

pub struct Adapter<'a> {
    invocation: &'a Invocation,
    endpoint: String,
    token: String,
    shard: String,
}

impl<'a> Adapter<'a> {
    pub fn new(invocation: &'a Invocation) -> Result<Self> {
        let endpoint = std::env::var("WIRE_REALM_URL").context("WIRE_REALM_URL is required")?;
        let port = endpoint
            .strip_prefix("http://127.0.0.1:")
            .context("fixture endpoint must be http://127.0.0.1:PORT")?;
        ensure!(port.parse::<u16>()? != 0, "fixture port must be nonzero");
        let token = std::fs::read_to_string(
            std::env::var("WIRE_OPERATOR_TOKEN_FILE")
                .context("WIRE_OPERATOR_TOKEN_FILE is required")?,
        )?;
        let token = token.trim().to_owned();
        ensure!(
            !token.is_empty()
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "invalid Operator token file"
        );
        let shard = std::env::var("WIRE_SHARD").context("WIRE_SHARD is required")?;
        ensure!(
            !shard.is_empty()
                && shard
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid Shard name"
        );
        Ok(Self {
            invocation,
            endpoint,
            token,
            shard,
        })
    }

    fn call(&self, reducer: &str, args: serde_json::Value) -> Result<()> {
        let mut child = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "10",
                "--config",
                "-",
                "--request",
                "POST",
            ])
            .arg(format!(
                "{}/v1/database/{}/call/{reducer}",
                self.endpoint, self.shard
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let config = format!("header = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\ndata = {}\n", self.token, serde_json::to_string(&args.to_string())?);
        child
            .stdin
            .take()
            .context("curl stdin")?
            .write_all(config.as_bytes())?;
        let status = child.wait_with_output()?;
        ensure!(status.status.success(), "fixture reducer {reducer} failed");
        Ok(())
    }
}

impl FixtureAdapter for Adapter<'_> {
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<WireClient> {
        ensure!(fixtures.len() == 1, "one Character fixture is required");
        let fixture = &fixtures[0];
        let FixtureVerb::Character { level, at, .. } = fixture;
        let client = super::create(self.invocation, fixture)?;
        self.call(
            "debug_set_level",
            serde_json::json!([client.self_guid, level]),
        )?;
        self.call(
            "debug_teleport",
            serde_json::json!([client.self_guid, at.map, at.x, at.y, at.z, at.orientation]),
        )?;
        drop(client);
        super::reconnect_checked(self.invocation, fixture)
    }
}

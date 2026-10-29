use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{ensure, Context, Result};
use wire_client::cli::Invocation;
use wire_client::runner::{FixtureAdapter, PreparedClient};
use wire_client::scenario::FixtureVerb;

pub struct Adapter {
    invocation: Invocation,
    endpoint: String,
    token: String,
    shard: String,
}

impl Adapter {
    pub fn new(invocation: &Invocation) -> Result<Self> {
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
            invocation: invocation.clone(),
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

fn reducer_calls(guid: u64, fixtures: &[FixtureVerb]) -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    fixtures
        .iter()
        .flat_map(|fixture| match fixture {
            FixtureVerb::Character { level, at, .. } => vec![
                ("debug_set_level", json!([guid, level])),
                (
                    "debug_teleport",
                    json!([guid, at.map, at.x, at.y, at.z, at.orientation]),
                ),
            ],
            FixtureVerb::Money { copper } => vec![("debug_set_money", json!([guid, copper]))],
            FixtureVerb::KnownFlightNode { node_id } => {
                vec![("debug_learn_taxi_node", json!([guid, node_id]))]
            }
            FixtureVerb::Creature { entry, offset, .. } => {
                vec![("debug_spawn_at_feet", json!([guid, entry, offset]))]
            }
        })
        .collect()
}

impl FixtureAdapter for Adapter {
    fn prepare(&mut self, fixtures: &[FixtureVerb]) -> Result<PreparedClient> {
        let fixture = fixtures.first().context("Character fixture is required")?;
        let FixtureVerb::Character { at, .. } = fixture else {
            anyhow::bail!("Character must be the first fixture")
        };
        let client = super::create(&self.invocation, fixture)?;
        let guid = client.self_guid;
        for verb in fixtures
            .iter()
            .filter(|verb| !matches!(verb, FixtureVerb::Creature { .. }))
        {
            for (reducer, args) in reducer_calls(guid, std::slice::from_ref(verb)) {
                self.call(reducer, args)?;
            }
        }
        drop(client);
        let mut client = super::connect(&self.invocation)?;
        client.player_login(guid)?;
        let mut bindings = std::collections::BTreeMap::new();
        for verb in fixtures {
            if let FixtureVerb::Creature { id, entry, offset } = verb {
                let before = client.seen_guids.clone();
                for (reducer, args) in reducer_calls(guid, std::slice::from_ref(verb)) {
                    self.call(reducer, args)?;
                }
                bindings.insert(
                    id.clone(),
                    super::new_creature(&mut client, &before, *entry, &at.offset(*offset))?,
                );
            }
        }
        drop(client);
        let mut prepared = super::reconnect_checked(&self.invocation, fixture)?;
        prepared.fixtures = bindings;
        Ok(prepared)
    }

    fn reconnect(&mut self, fixture: &FixtureVerb) -> Result<PreparedClient> {
        super::reconnect_existing(&self.invocation, fixture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxi_fixtures_call_the_agreed_debug_reducers() {
        let fixtures = vec![
            FixtureVerb::Money { copper: 1000 },
            FixtureVerb::KnownFlightNode { node_id: 2 },
            FixtureVerb::KnownFlightNode { node_id: 4 },
            FixtureVerb::Creature {
                id: "master".into(),
                entry: 352,
                offset: 2.0,
            },
        ];
        assert_eq!(
            reducer_calls(17, &fixtures),
            vec![
                ("debug_set_money", serde_json::json!([17, 1000])),
                ("debug_learn_taxi_node", serde_json::json!([17, 2])),
                ("debug_learn_taxi_node", serde_json::json!([17, 4])),
                ("debug_spawn_at_feet", serde_json::json!([17, 352, 2.0])),
            ]
        );
    }
}

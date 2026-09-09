use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

const BOT: &str = "pkg_playerbots_bot";
const RUNNER: &str = "pkg_playerbots_runner";
const SCHEDULER: &str = "pkg_playerbots_scheduler";
pub const TABLES: [&str; 3] = [BOT, RUNNER, SCHEDULER];

#[derive(Default)]
pub struct Stream {
    bots: BTreeMap<u64, Value>,
    runners: BTreeMap<u64, Value>,
    scheduler: Option<Value>,
    last_observed_micros: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Pass {
    pub observed_micros: i64,
    pub processed_guids: Vec<u64>,
    pub bots: Vec<Value>,
    pub runners: Vec<Value>,
    pub excess_due: bool,
    pub oldest_deferred_lag_micros: i64,
}

fn guid(row: &Value) -> Result<u64> {
    row["character_guid"]
        .as_u64()
        .context("row has no numeric Character GUID")
}

fn replace_rows(rows: &mut BTreeMap<u64, Value>, update: &Value) -> Result<()> {
    for row in update["deletes"].as_array().context("missing deletes")? {
        let key = guid(row)?;
        let old = rows.remove(&key).context("deleted row was not observed")?;
        if old != *row {
            bail!("deleted row differs from the last observed Character {key}");
        }
    }
    for row in update["inserts"].as_array().context("missing inserts")? {
        let key = guid(row)?;
        if rows.insert(key, row.clone()).is_some() {
            bail!("insert replaced Character {key} without a matching deletion");
        }
    }
    Ok(())
}

impl Stream {
    /// Apply a complete CLI transaction before correlating its scheduler and Character rows.
    pub fn apply(&mut self, update: &Value, expected: &BTreeSet<u64>) -> Result<Option<Pass>> {
        let tables = update
            .as_object()
            .context("subscription update is not an object")?;
        for (table, changes) in tables {
            match table.as_str() {
                BOT => replace_rows(&mut self.bots, changes)?,
                RUNNER => replace_rows(&mut self.runners, changes)?,
                SCHEDULER => {
                    for row in changes["deletes"]
                        .as_array()
                        .context("missing scheduler deletes")?
                    {
                        if self.scheduler.take().as_ref() != Some(row) {
                            bail!("deleted scheduler row differs from the last observation");
                        }
                    }
                    for row in changes["inserts"]
                        .as_array()
                        .context("missing scheduler inserts")?
                    {
                        if row["id"].as_u64() != Some(0)
                            || self.scheduler.replace(row.clone()).is_some()
                        {
                            bail!("scheduler must contain exactly one row with id zero");
                        }
                    }
                }
                _ => bail!("unexpected subscribed table {table}"),
            }
        }
        if self.bots.keys().copied().collect::<BTreeSet<_>>() != *expected {
            bail!("observed bot roster differs from the staged roster");
        }
        if self.runners.keys().any(|key| !expected.contains(key)) {
            bail!("runner belongs to an unstaged Character");
        }
        let Some(row) = self
            .scheduler
            .as_ref()
            .filter(|_| tables.contains_key(SCHEDULER))
        else {
            return Ok(None);
        };
        let observed_micros = row["observed_micros"]
            .as_i64()
            .context("invalid scheduler timestamp")?;
        if self
            .last_observed_micros
            .is_some_and(|last| observed_micros <= last)
        {
            bail!("scheduler timestamp did not advance");
        }
        self.last_observed_micros = Some(observed_micros);
        let processed_guids: Vec<u64> = serde_json::from_value(row["processed_guids"].clone())?;
        let unique: BTreeSet<_> = processed_guids.iter().copied().collect();
        if unique.len() != processed_guids.len()
            || !unique.is_subset(expected)
            || processed_guids.len() > 16
            || row["processed"].as_u64() != Some(processed_guids.len() as u64)
        {
            bail!("scheduler processed list is inconsistent with the staged roster or batch limit");
        }
        let bots = processed_guids
            .iter()
            .map(|guid| self.bots[guid].clone())
            .collect();
        let runners = processed_guids
            .iter()
            .map(|guid| {
                self.runners
                    .get(guid)
                    .cloned()
                    .with_context(|| format!("processed Character {guid} has no runner"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Pass {
            observed_micros,
            processed_guids,
            bots,
            runners,
            excess_due: row["excess_due"].as_bool().context("invalid excess_due")?,
            oldest_deferred_lag_micros: row["oldest_deferred_lag_micros"]
                .as_i64()
                .context("invalid deferred lag")?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn initial() -> Value {
        json!({
            BOT: {"deletes":[],"inserts":[{"character_guid":11,"next_think_micros":2000},{"character_guid":12,"next_think_micros":1000}]},
            RUNNER: {"deletes":[],"inserts":[{"character_guid":11,"observed_micros":1000}]},
            SCHEDULER: {"deletes":[],"inserts":[{"id":0,"observed_micros":1000,"processed":1,"processed_guids":[11],"excess_due":true,"oldest_deferred_lag_micros":0}]}
        })
    }

    #[test]
    fn a_committed_pass_joins_the_same_transactions_bot_and_runner_rows() {
        let mut stream = Stream::default();
        let first = stream
            .apply(&initial(), &BTreeSet::from([11, 12]))
            .unwrap()
            .unwrap();
        assert_eq!(first.processed_guids, [11]);
        let update = json!({
            BOT: {"deletes":[{"character_guid":12,"next_think_micros":1000}],"inserts":[{"character_guid":12,"next_think_micros":2100}]},
            RUNNER: {"deletes":[],"inserts":[{"character_guid":12,"observed_micros":1100}]},
            SCHEDULER: {"deletes":initial()[SCHEDULER]["inserts"],"inserts":[{"id":0,"observed_micros":1100,"processed":1,"processed_guids":[12],"excess_due":false,"oldest_deferred_lag_micros":0}]}
        });
        let pass = stream
            .apply(&update, &BTreeSet::from([11, 12]))
            .unwrap()
            .unwrap();
        assert_eq!(pass.processed_guids, [12]);
        assert_eq!(pass.bots[0]["next_think_micros"], 2100);
        assert_eq!(pass.runners[0]["observed_micros"], 1100);
        assert!(!pass.excess_due);
    }

    #[test]
    fn an_unknown_or_missing_bot_refuses_the_observation() {
        assert!(Stream::default()
            .apply(&initial(), &BTreeSet::from([11]))
            .is_err());
        assert!(Stream::default()
            .apply(&initial(), &BTreeSet::from([11, 12, 13]))
            .is_err());
    }

    #[test]
    fn a_lost_intervening_row_update_refuses_the_stream() {
        let mut stream = Stream::default();
        stream.apply(&initial(), &BTreeSet::from([11, 12])).unwrap();
        let update =
            json!({BOT:{"deletes":[{"character_guid":11,"next_think_micros":1999}],"inserts":[]}});
        assert!(stream
            .apply(&update, &BTreeSet::from([11, 12]))
            .unwrap_err()
            .to_string()
            .contains("differs"));
    }

    #[test]
    fn an_invalid_or_replayed_scheduler_pass_cannot_count_as_fair_work() {
        for invalid in [vec![11, 11], vec![99]] {
            let mut update = initial();
            update[SCHEDULER]["inserts"][0]["processed"] = json!(invalid.len());
            update[SCHEDULER]["inserts"][0]["processed_guids"] = json!(invalid);
            assert!(Stream::default()
                .apply(&update, &BTreeSet::from([11, 12]))
                .is_err());
        }
        let mut stream = Stream::default();
        stream.apply(&initial(), &BTreeSet::from([11, 12])).unwrap();
        let update = json!({SCHEDULER:{"deletes":initial()[SCHEDULER]["inserts"],"inserts":initial()[SCHEDULER]["inserts"]}});
        assert!(stream
            .apply(&update, &BTreeSet::from([11, 12]))
            .unwrap_err()
            .to_string()
            .contains("timestamp"));
    }
}

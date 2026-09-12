use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

const BOT: &str = "pkg_playerbots_bot";
const RUNNER: &str = "pkg_playerbots_runner";
const SCHEDULER: &str = "pkg_playerbots_scheduler";
pub const TABLES: [&str; 3] = [BOT, RUNNER, SCHEDULER];
pub const OWNERSHIP_TABLES: [&str; 5] = [
    "game_creature_quest_tap",
    "game_creature_quest_tap_member",
    "game_creature_loot_tag_group",
    "game_group_member",
    "game_corpse_loot_eligible",
];

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
    pub deferred_guids: Vec<u64>,
    pub accelerated_guids: Vec<u64>,
}

fn due_order(bots: &BTreeMap<u64, Value>, now: i64) -> Result<Vec<(i64, u64, u64)>> {
    let mut due = Vec::new();
    let mut ids = BTreeSet::new();
    for (&guid, bot) in bots {
        let id = bot["id"].as_u64().context("bot has no numeric row id")?;
        if !ids.insert(id) {
            bail!("bots share row id {id}");
        }
        let at = bot["next_think_micros"]
            .as_i64()
            .context("invalid bot due time")?;
        if at <= now {
            due.push((at, id, guid));
        }
    }
    due.sort_unstable();
    Ok(due)
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

fn validate_evidence_changes(table: &str, changes: &Value) -> Result<()> {
    let changes = changes
        .as_object()
        .with_context(|| format!("{table} update is not an object"))?;
    for key in ["deletes", "inserts"] {
        if !changes.get(key).is_some_and(serde_json::Value::is_array) {
            bail!("{table} has no {key} row list");
        }
    }
    Ok(())
}

impl Stream {
    /// Apply a complete CLI transaction before correlating its scheduler and Character rows.
    pub fn apply(&mut self, update: &Value, expected: &BTreeSet<u64>) -> Result<Option<Pass>> {
        let mut previous_bots = self.bots.clone();
        let initial = self.last_observed_micros.is_none();
        let tables = update
            .as_object()
            .context("subscription update is not an object")?;
        for (table, changes) in tables {
            match table.as_str() {
                BOT => replace_rows(&mut self.bots, changes)?,
                RUNNER => replace_rows(&mut self.runners, changes)?,
                table if crate::movement::TABLES.contains(&table) => {}
                table if OWNERSHIP_TABLES.contains(&table) || table == "game_creature_spawn" => {
                    validate_evidence_changes(table, changes)?;
                }
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
        if initial {
            // The subscription snapshot describes earlier work; it is not a new scheduled pass.
            return Ok(None);
        }
        let mut accelerated_guids = Vec::new();
        for (&guid, old) in &mut previous_bots {
            let prior = old["next_think_micros"]
                .as_i64()
                .context("invalid prior due time")?;
            let current = &self.bots[&guid];
            let due = if unique.contains(&guid) {
                let lag = current["scheduler_lag_micros"]
                    .as_i64()
                    .filter(|&lag| lag >= 0)
                    .context("invalid processed lag")?;
                observed_micros
                    .checked_sub(lag)
                    .context("processed due time overflow")?
            } else {
                current["next_think_micros"]
                    .as_i64()
                    .context("invalid current due time")?
            };
            if due != prior {
                // Damage can advance a bot's due time earlier in this same Core transaction.
                if prior <= observed_micros || due != observed_micros {
                    bail!("Character {guid} changed due time outside the scheduled pass contract");
                }
                accelerated_guids.push(guid);
                old["next_think_micros"] = due.into();
            }
        }
        let previous_due = due_order(&previous_bots, observed_micros)?;
        let expected_processed: Vec<_> = previous_due.iter().take(16).map(|row| row.2).collect();
        if processed_guids != expected_processed {
            bail!("scheduler did not process the earliest due bots in indexed order");
        }
        for &(due, _, guid) in previous_due.iter().take(16) {
            let bot = &self.bots[&guid];
            if bot["next_think_micros"].as_i64() != observed_micros.checked_add(1_000_000)
                || bot["scheduler_lag_micros"].as_i64() != Some(observed_micros.saturating_sub(due))
                || self
                    .runners
                    .get(&guid)
                    .and_then(|row| row["observed_micros"].as_i64())
                    != Some(observed_micros)
            {
                bail!("processed Character {guid} has inconsistent due, lag or runner time");
            }
        }
        let deferred = due_order(&self.bots, observed_micros)?;
        let excess_due = row["excess_due"].as_bool().context("invalid excess_due")?;
        let oldest_deferred_lag_micros = row["oldest_deferred_lag_micros"]
            .as_i64()
            .context("invalid deferred lag")?;
        if excess_due == deferred.is_empty()
            || oldest_deferred_lag_micros
                != deferred
                    .first()
                    .map_or(0, |due| observed_micros.saturating_sub(due.0))
        {
            bail!("scheduler deferred summary disagrees with the complete bot roster");
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
            excess_due,
            oldest_deferred_lag_micros,
            deferred_guids: deferred.into_iter().map(|row| row.2).collect(),
            accelerated_guids,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn initial() -> Value {
        json!({
            BOT: {"deletes":[],"inserts":[{"id":1,"character_guid":11,"next_think_micros":1_001_000,"scheduler_lag_micros":0},{"id":2,"character_guid":12,"next_think_micros":1000,"scheduler_lag_micros":0}]},
            RUNNER: {"deletes":[],"inserts":[{"character_guid":11,"observed_micros":1000}]},
            SCHEDULER: {"deletes":[],"inserts":[{"id":0,"observed_micros":1000,"processed":1,"processed_guids":[11],"excess_due":true,"oldest_deferred_lag_micros":0}]}
        })
    }

    #[test]
    fn a_committed_pass_joins_the_same_transactions_bot_and_runner_rows() {
        let mut stream = Stream::default();
        assert!(stream
            .apply(&initial(), &BTreeSet::from([11, 12]))
            .unwrap()
            .is_none());
        let update = json!({
            BOT: {"deletes":[initial()[BOT]["inserts"][1]],"inserts":[{"id":2,"character_guid":12,"next_think_micros":1_001_100,"scheduler_lag_micros":100}]},
            RUNNER: {"deletes":[],"inserts":[{"character_guid":12,"observed_micros":1100}]},
            SCHEDULER: {"deletes":initial()[SCHEDULER]["inserts"],"inserts":[{"id":0,"observed_micros":1100,"processed":1,"processed_guids":[12],"excess_due":false,"oldest_deferred_lag_micros":0}]}
        });
        let pass = stream
            .apply(&update, &BTreeSet::from([11, 12]))
            .unwrap()
            .unwrap();
        assert_eq!(pass.processed_guids, [12]);
        assert_eq!(pass.bots[0]["next_think_micros"], 1_001_100);
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
    fn ownership_evidence_tables_are_accepted_without_entering_scheduler_state() {
        let mut stream = Stream::default();
        stream.apply(&initial(), &BTreeSet::from([11, 12])).unwrap();
        for table in OWNERSHIP_TABLES {
            let update = json!({(table):{"deletes":[],"inserts":[{"id":1}]}});
            assert!(stream
                .apply(&update, &BTreeSet::from([11, 12]))
                .unwrap()
                .is_none());
        }
        assert!(stream
            .apply(
                &json!({"game_creature_spawn":{"deletes":[],"inserts":[{"guid":90,"entry":6}]}}),
                &BTreeSet::from([11, 12]),
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_or_unknown_evidence_tables_are_refused() {
        let expected = BTreeSet::from([11, 12]);
        for update in [
            json!({"game_creature_quest_tap":{"deletes":[]}}),
            json!({"unscoped_table":{"deletes":[],"inserts":[]}}),
        ] {
            assert!(Stream::default().apply(&update, &expected).is_err());
        }
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

    fn seventeen_due_bots() -> (Value, Value, BTreeSet<u64>) {
        let bots: Vec<_> = (1..=17).map(|guid| json!({
            "id":guid,"character_guid":guid,"next_think_micros":1000,"scheduler_lag_micros":0
        })).collect();
        let first = json!({
            BOT:{"deletes":[],"inserts":bots},
            RUNNER:{"deletes":[],"inserts":[]},
            SCHEDULER:{"deletes":[],"inserts":[{"id":0,"observed_micros":500,"processed":0,"processed_guids":[],"excess_due":false,"oldest_deferred_lag_micros":0}]}
        });
        let updates: Vec<_> = (1..=16).map(|guid| json!({
            "id":guid,"character_guid":guid,"next_think_micros":1_001_100,"scheduler_lag_micros":100
        })).collect();
        let runners: Vec<_> = (1..=16)
            .map(|guid| json!({"character_guid":guid,"observed_micros":1100}))
            .collect();
        let second = json!({
            BOT:{"deletes":bots[..16],"inserts":updates},
            RUNNER:{"deletes":[],"inserts":runners},
            SCHEDULER:{"deletes":first[SCHEDULER]["inserts"],"inserts":[{"id":0,"observed_micros":1100,"processed":16,"processed_guids":(1..=16).collect::<Vec<_>>(),"excess_due":true,"oldest_deferred_lag_micros":100}]}
        });
        (first, second, (1..=17).collect())
    }

    #[test]
    fn a_full_batch_accounts_for_the_bot_left_due() {
        let (first, second, expected) = seventeen_due_bots();
        let mut stream = Stream::default();
        stream.apply(&first, &expected).unwrap();
        let pass = stream.apply(&second, &expected).unwrap().unwrap();
        assert_eq!(pass.deferred_guids, [17]);
        assert_eq!(pass.oldest_deferred_lag_micros, 100);
    }

    #[test]
    fn a_false_deferred_summary_cannot_hide_the_oldest_due_bot() {
        let (first, mut second, expected) = seventeen_due_bots();
        let mut stream = Stream::default();
        stream.apply(&first, &expected).unwrap();
        second[SCHEDULER]["inserts"][0]["oldest_deferred_lag_micros"] = json!(0);
        assert!(stream
            .apply(&second, &expected)
            .unwrap_err()
            .to_string()
            .contains("deferred summary"));
    }

    #[test]
    fn equal_due_times_still_require_the_row_id_order() {
        let (first, mut second, expected) = seventeen_due_bots();
        let mut stream = Stream::default();
        stream.apply(&first, &expected).unwrap();
        second[SCHEDULER]["inserts"][0]["processed_guids"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(stream
            .apply(&second, &expected)
            .unwrap_err()
            .to_string()
            .contains("indexed order"));
    }

    #[test]
    fn a_stale_runner_cannot_be_counted_as_processed() {
        let (first, mut second, expected) = seventeen_due_bots();
        let mut stream = Stream::default();
        stream.apply(&first, &expected).unwrap();
        second[RUNNER]["inserts"][0]["observed_micros"] = json!(1000);
        assert!(stream
            .apply(&second, &expected)
            .unwrap_err()
            .to_string()
            .contains("runner time"));
    }

    #[test]
    fn damage_can_advance_a_future_due_bot_in_the_same_transaction() {
        let (mut first, second, expected) = seventeen_due_bots();
        first[BOT]["inserts"][15]["next_think_micros"] = json!(9000);
        let mut second = second;
        // The older due bot 17 must be served before the newly due bot 16.
        second[SCHEDULER]["inserts"][0]["processed_guids"][15] = json!(17);
        second[BOT]["deletes"][15] = first[BOT]["inserts"][16].clone();
        second[BOT]["inserts"][15] = json!({"id":17,"character_guid":17,"next_think_micros":1_001_100,"scheduler_lag_micros":100});
        second[BOT]["deletes"]
            .as_array_mut()
            .unwrap()
            .push(first[BOT]["inserts"][15].clone());
        second[BOT]["inserts"].as_array_mut().unwrap().push(
            json!({"id":16,"character_guid":16,"next_think_micros":1100,"scheduler_lag_micros":0}),
        );
        second[RUNNER]["inserts"][15]["character_guid"] = json!(17);
        second[SCHEDULER]["inserts"][0]["oldest_deferred_lag_micros"] = json!(0);
        let mut stream = Stream::default();
        stream.apply(&first, &expected).unwrap();
        let pass = stream.apply(&second, &expected).unwrap().unwrap();
        assert_eq!(pass.accelerated_guids, [16]);
        assert_eq!(pass.deferred_guids, [16]);
    }
}

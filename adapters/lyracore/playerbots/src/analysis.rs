use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::stream::Pass;

#[derive(Default)]
pub struct Measurement {
    counts: BTreeMap<u64, u64>,
    previous: BTreeMap<u64, Value>,
    progress: BTreeSet<u64>,
    movement: crate::movement::Movement,
    lag: Vec<(u64, u64)>,
    routes: Vec<(u64, u64)>,
    deferred_observations: u64,
    accelerated_observations: u64,
    slowest_due: Option<Value>,
}

fn signed(row: &Value, key: &str) -> Result<i64> {
    row[key].as_i64().with_context(|| format!("invalid {key}"))
}

pub(super) fn unsigned(row: &Value, key: &str) -> Result<u64> {
    row[key].as_u64().with_context(|| format!("invalid {key}"))
}

fn optional<'a>(row: &'a Value, key: &str) -> Result<Option<&'a Value>> {
    let value = row[key]
        .as_object()
        .with_context(|| format!("invalid SATS option {key}"))?;
    if value.len() != 1 {
        bail!("invalid SATS option {key}");
    }
    if let Some(value) = value.get("some") {
        return Ok(Some(value));
    }
    if value.get("none") == Some(&json!({})) {
        return Ok(None);
    }
    bail!("unknown SATS option {key}")
}

fn newer(current: &Value, previous: &Value, key: &str) -> Result<bool> {
    let Some(current) = optional(current, key)? else {
        return Ok(false);
    };
    let before = optional(previous, key)?
        .map(|row| signed(row, "observed_micros"))
        .transpose()?
        .unwrap_or(0);
    Ok(signed(current, "observed_micros")? > before)
}

pub(super) fn coordinate(row: &Value, key: &str) -> Result<f64> {
    row[key]
        .as_f64()
        .filter(|value| value.is_finite())
        .with_context(|| format!("invalid {key}"))
}

fn movement_advanced(current: &Value, previous: &Value) -> Result<bool> {
    if !newer(current, previous, "movement_progress")? {
        return Ok(false);
    }
    let Some(progress) = optional(current, "movement_progress")? else {
        return Ok(false);
    };
    let prior = if let Some(progress) = optional(previous, "movement_progress")? {
        Some((coordinate(progress, "x")?, coordinate(progress, "y")?))
    } else if let Some(movement) =
        optional(previous, "foreground")?.and_then(|fg| fg["running"].get("movement"))
    {
        Some((
            coordinate(movement, "from_x")?,
            coordinate(movement, "from_y")?,
        ))
    } else {
        None
    };
    let Some((x, y)) = prior else {
        return Ok(false);
    };
    Ok(
        (coordinate(progress, "x")? - x).powi(2) + (coordinate(progress, "y")? - y).powi(2)
            > 0.05 * 0.05,
    )
}

fn has_progress(current: &Value, previous: &Value) -> Result<bool> {
    // A resolved cast can have no effect. Progress needs movement, lost target health or credit.
    if movement_advanced(current, previous)? {
        return Ok(true);
    }
    if newer(current, previous, "combat_progress")? {
        return Ok(true);
    }
    let before = previous["quest_progress"]
        .as_array()
        .context("invalid prior Quest progress")?;
    for quest in current["quest_progress"]
        .as_array()
        .context("invalid Quest progress")?
    {
        let entry = unsigned(quest, "quest")?;
        let prior = before
            .iter()
            .find(|row| row["quest"].as_u64() == Some(entry));
        let time = signed(quest, "observed_micros")?;
        if time
            > prior
                .map(|row| signed(row, "observed_micros"))
                .transpose()?
                .unwrap_or(0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn movement_identity(runner: &Value) -> Result<Option<Value>> {
    let Some(fg) = optional(runner, "foreground")? else {
        return Ok(None);
    };
    let Some(movement) = fg["running"].get("movement") else {
        return Ok(None);
    };
    if !fg["candidate"].is_object() || !movement["destination"].is_object() {
        bail!("movement has no candidate or destination identity");
    }
    Ok(Some(json!([
        unsigned(fg, "generation")?,
        unsigned(fg, "map_id")?,
        unsigned(fg, "instance_id")?,
        signed(fg, "started_micros")?,
        fg["candidate"],
        movement["destination"]
    ])))
}

pub(super) fn distribution(samples: &[(u64, u64)]) -> Value {
    if samples.is_empty() {
        return json!({"count":0,"p50":null,"p95":null,"max":null,"max_guid":null,"total":0});
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100) - 1].0;
    let maximum = sorted[sorted.len() - 1];
    json!({"count":sorted.len(),"p50":percentile(50),"p95":percentile(95),
        "max":maximum.0,"max_guid":maximum.1,"total":sorted.iter().map(|row| row.0).sum::<u64>()})
}

impl Measurement {
    pub fn new(expected: &BTreeSet<u64>) -> Self {
        Self {
            counts: expected.iter().map(|&guid| (guid, 0)).collect(),
            movement: crate::movement::Movement::new(expected),
            ..Self::default()
        }
    }

    pub fn observe(&mut self, pass: &Pass, measured: bool) -> Result<()> {
        for (index, &guid) in pass.processed_guids.iter().enumerate() {
            let runner = &pass.runners[index];
            if measured {
                *self
                    .counts
                    .get_mut(&guid)
                    .context("unstaged measured bot")? += 1;
                self.lag
                    .push((unsigned(&pass.bots[index], "scheduler_lag_micros")?, guid));
                let new_movement = optional(runner, "foreground")?.is_some_and(|fg| {
                    fg["running"].get("movement").is_some()
                        && fg["started_micros"].as_i64() == Some(pass.observed_micros)
                });
                self.routes.push((
                    if new_movement {
                        unsigned(runner, "route_expansions")?
                    } else {
                        0
                    },
                    guid,
                ));
            }
        }
        if measured {
            self.deferred_observations += pass.deferred_guids.len() as u64;
            self.accelerated_observations += pass.accelerated_guids.len() as u64;
            if let Some(guid) = pass.deferred_guids.first() {
                let replace = self.slowest_due.as_ref().map_or(true, |old| {
                    old["lag_micros"].as_i64().unwrap_or(-1) < pass.oldest_deferred_lag_micros
                });
                if replace {
                    self.slowest_due = Some(
                        json!({"guid":guid,"lag_micros":pass.oldest_deferred_lag_micros,
                        "observed_micros":pass.observed_micros,"deferred_count":pass.deferred_guids.len()}),
                    );
                }
            }
        }
        Ok(())
    }

    /// Observe every committed runner update, including updates between scheduled passes.
    pub fn observe_transaction(&mut self, update: &Value, measured: bool) -> Result<()> {
        self.movement.observe(update, measured)?;
        let Some(changes) = update.get("pkg_playerbots_runner") else {
            return Ok(());
        };
        for runner in changes["inserts"]
            .as_array()
            .context("invalid runner insert list")?
        {
            let guid = unsigned(runner, "character_guid")?;
            if !self.counts.contains_key(&guid) {
                bail!("unstaged runner observation");
            }
            let previous = self.previous.remove(&guid);
            if measured {
                if let Some(previous) = &previous {
                    if has_progress(runner, previous)? {
                        self.progress.insert(guid);
                    }
                }
            }
            self.previous.insert(guid, runner.clone());
        }
        for deleted in changes["deletes"]
            .as_array()
            .context("invalid runner delete list")?
        {
            let guid = unsigned(deleted, "character_guid")?;
            let replaced = changes["inserts"]
                .as_array()
                .context("invalid runner insert list")?
                .iter()
                .any(|row| row["character_guid"].as_u64() == Some(guid));
            if !replaced {
                self.previous.remove(&guid);
            }
        }
        Ok(())
    }

    pub fn ready(&self) -> Result<()> {
        self.movement.ready()
    }

    pub fn report(&self) -> Value {
        let min = self.counts.values().min().copied().unwrap_or(0);
        let max = self.counts.values().max().copied().unwrap_or(0);
        let missing: Vec<_> = self
            .counts
            .keys()
            .filter(|guid| !self.progress.contains(guid))
            .collect();
        let never_processed: Vec<_> = self
            .counts
            .iter()
            .filter_map(|(guid, count)| (*count == 0).then_some(guid))
            .collect();
        let lag_bound = (self.counts.len().div_ceil(16) as u64 + 1) * 1_000_000;
        json!({"per_bot_passes":self.counts,"pass_count_spread":max-min,"never_processed_guids":never_processed,
            "no_observed_progress_guids":missing,"scheduler_lag_micros":distribution(&self.lag),
            "scheduler_lag_bound_micros":lag_bound,"lag_within_bound":self.lag.iter().all(|sample| sample.0 <= lag_bound),
            "route_expansions_per_processed_bot":distribution(&self.routes),
            "movement_legs":self.movement.report(),
            "deferred_bot_observations":self.deferred_observations,"accelerated_due_observations":self.accelerated_observations,
            "slowest_due_bot":self.slowest_due,
            "progress_scope":"new authoritative movement, lost target health or Quest progress in measured transactions",
            "route_scope":"one sample per processed bot; retained work is zero unless this pass starts a new movement attempt",
            "fairness_scope":"each pass verifies earliest due order; count spread also reflects damage-triggered scheduling"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> Value {
        json!({"character_guid":11,"observed_micros":0,"generation":1,"movement_progress":{"none":{}},"combat_progress":{"none":{}},
            "cast_progress":{"none":{}},"quest_progress":[],"foreground":{"none":{}},"route_expansions":0})
    }

    fn transaction(measurement: &Measurement, runner: &Value) -> Value {
        let deletes: Vec<_> = measurement
            .previous
            .get(&runner["character_guid"].as_u64().unwrap())
            .into_iter()
            .collect();
        json!({"pkg_playerbots_runner":{"deletes":deletes,"inserts":[runner]}})
    }

    #[test]
    fn a_deleted_runner_cannot_supply_a_later_progress_baseline() {
        let mut measurement = Measurement::new(&BTreeSet::from([11]));
        let state = runner();
        measurement
            .observe_transaction(&transaction(&measurement, &state), false)
            .unwrap();
        measurement
            .observe_transaction(
                &json!({"pkg_playerbots_runner":{"deletes":[state],"inserts":[]}}),
                true,
            )
            .unwrap();
        let mut resumed = runner();
        resumed["combat_progress"] =
            json!({"some":{"observed_micros":200,"target":44,"health":10}});
        measurement
            .observe_transaction(&transaction(&measurement, &resumed), true)
            .unwrap();
        assert_eq!(
            measurement.report()["no_observed_progress_guids"],
            json!([11])
        );
    }

    fn pass(runner: &Value) -> Pass {
        Pass {
            observed_micros: runner["observed_micros"].as_i64().unwrap(),
            processed_guids: vec![11],
            bots: vec![json!({"scheduler_lag_micros":0})],
            runners: vec![runner.clone()],
            excess_due: false,
            oldest_deferred_lag_micros: 0,
            deferred_guids: vec![],
            accelerated_guids: vec![],
        }
    }

    fn moving(started: i64) -> Value {
        json!({"some":{"generation":1,"map_id":0,"instance_id":0,"started_micros":started,
            "candidate":{"id":{"action":{"move":{"home":{}}},"objective":1}},
            "running":{"movement":{"from_x":10.0,"from_y":10.0,"destination":{"map_id":0,"instance_id":0,"x":20.0,"y":10.0,"z":0.0}}}}})
    }

    #[test]
    fn accepted_work_is_not_observed_progress() {
        let before = runner();
        let mut after = before.clone();
        after["last_outcome"] = json!({"accepted":{}});
        assert!(!has_progress(&after, &before).unwrap());
        after["cast_progress"] =
            json!({"some":{"observed_micros":900,"scheduled_id":7,"spell":585,"target":44}});
        assert!(!has_progress(&after, &before).unwrap());
        after["combat_progress"] = json!({"some":{"observed_micros":900,"target":44,"health":20}});
        assert!(has_progress(&after, &before).unwrap());
        assert!(!has_progress(&after, &after).unwrap());
    }

    #[test]
    fn percentiles_use_the_nearest_rank_and_retain_the_slowest_bot() {
        let report = distribution(&[(40, 3), (10, 1), (30, 4), (20, 2)]);
        assert_eq!(report["p50"], 20);
        assert_eq!(report["p95"], 40);
        assert_eq!(report["max_guid"], 3);
        assert_eq!(report["total"], 100);
        assert_eq!(distribution(&[])["p95"], Value::Null);
    }

    #[test]
    fn a_stale_route_count_is_not_added_again_while_movement_runs() {
        let mut measurement = Measurement::new(&BTreeSet::from([11]));
        let mut state = runner();
        measurement
            .observe_transaction(&transaction(&measurement, &state), false)
            .unwrap();
        state["observed_micros"] = json!(100);
        state["foreground"] = moving(100);
        state["route_expansions"] = json!(27);
        measurement
            .observe_transaction(&transaction(&measurement, &state), true)
            .unwrap();
        measurement.observe(&pass(&state), true).unwrap();
        state["observed_micros"] = json!(200);
        measurement
            .observe_transaction(&transaction(&measurement, &state), true)
            .unwrap();
        measurement.observe(&pass(&state), true).unwrap();
        state["observed_micros"] = json!(900);
        state["foreground"] = json!({"none":{}});
        state["movement_progress"] =
            json!({"some":{"observed_micros":900,"arrived":true,"x":20.0,"y":10.0}});
        measurement
            .observe_transaction(&transaction(&measurement, &state), true)
            .unwrap();
        measurement.observe(&pass(&state), true).unwrap();
        assert_eq!(
            measurement.report()["route_expansions_per_processed_bot"]["total"],
            27
        );
        assert_eq!(
            measurement.report()["route_expansions_per_processed_bot"]["count"],
            3
        );
        assert_eq!(
            measurement.report()["route_expansions_per_processed_bot"]["p50"],
            0
        );
    }

    #[test]
    fn arrival_at_an_unchanged_position_is_not_movement_progress() {
        let mut before = runner();
        before["movement_progress"] =
            json!({"some":{"observed_micros":100,"arrived":false,"x":10.0,"y":20.0}});
        let mut after = before.clone();
        after["movement_progress"]["some"]["observed_micros"] = json!(200);
        after["movement_progress"]["some"]["arrived"] = json!(true);
        assert!(!has_progress(&after, &before).unwrap());
        after["movement_progress"]["some"]["x"] = json!(11.0);
        assert!(has_progress(&after, &before).unwrap());
    }

    #[test]
    fn a_runner_update_before_the_window_cannot_be_counted_in_the_next_pass() {
        let mut measurement = Measurement::new(&BTreeSet::from([11]));
        let mut state = runner();
        measurement
            .observe_transaction(&transaction(&measurement, &state), false)
            .unwrap();
        state["combat_progress"] = json!({"some":{"observed_micros":100,"target":44,"health":20}});
        state["observed_micros"] = json!(100);
        measurement
            .observe_transaction(&transaction(&measurement, &state), false)
            .unwrap();
        state["observed_micros"] = json!(200);
        measurement
            .observe_transaction(&transaction(&measurement, &state), true)
            .unwrap();
        measurement.observe(&pass(&state), true).unwrap();
        assert_eq!(
            measurement.report()["no_observed_progress_guids"],
            json!([11])
        );
    }

    #[test]
    fn an_actual_confirmed_cli_snapshot_preserves_named_sum_values() {
        let update: Value =
            serde_json::from_str(include_str!("../fixtures/initial-transaction.json")).unwrap();
        let expected = BTreeSet::from([1_000_001]);
        let mut stream = crate::stream::Stream::default();
        assert!(stream.apply(&update, &expected).unwrap().is_none());
        let mut measurement = Measurement::new(&expected);
        measurement.observe_transaction(&update, false).unwrap();
        let state = &measurement.previous[&1_000_001];
        assert!(optional(state, "foreground").unwrap().is_none());
        assert_eq!(
            measurement.report()["no_observed_progress_guids"],
            json!([1_000_001])
        );
    }
}

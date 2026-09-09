use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::stream::Pass;

#[derive(Default)]
pub struct Measurement {
    counts: BTreeMap<u64, u64>,
    previous: BTreeMap<u64, Value>,
    progress: BTreeSet<u64>,
    movement_starts: BTreeMap<u64, (u64, i64, f64, f64)>,
    lag: Vec<(u64, u64)>,
    routes: Vec<(u64, u64)>,
    movements: Vec<(u64, u64)>,
    deferred_observations: u64,
    accelerated_observations: u64,
    slowest_due: Option<Value>,
}

fn signed(row: &Value, key: &str) -> Result<i64> {
    row[key].as_i64().with_context(|| format!("invalid {key}"))
}

fn unsigned(row: &Value, key: &str) -> Result<u64> {
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
    if value.get("none") == Some(&json!([])) {
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

fn coordinate(row: &Value, key: &str) -> Result<f64> {
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
        optional(previous, "foreground")?.and_then(|fg| fg["running"].get("Movement"))
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
    // These fields are populated by observed movement, lost target health, resolved casts or credit.
    // Accepted, Waiting and objective selection never count as progress here.
    if movement_advanced(current, previous)? {
        return Ok(true);
    }
    for key in ["combat_progress", "cast_progress"] {
        if newer(current, previous, key)? {
            return Ok(true);
        }
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

fn distribution(samples: &[(u64, u64)]) -> Value {
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
                if let Some(previous) = self.previous.get(&guid) {
                    if has_progress(runner, previous)? {
                        self.progress.insert(guid);
                    }
                }
                self.movement(runner, guid, pass.observed_micros)?;
            }
            self.previous.insert(guid, runner.clone());
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

    fn movement(&mut self, runner: &Value, guid: u64, now: i64) -> Result<()> {
        let generation = unsigned(runner, "generation")?;
        if let Some(progress) = optional(runner, "movement_progress")? {
            if signed(progress, "observed_micros")? == now && progress["arrived"] == true {
                if let Some((started_generation, started, x, y)) =
                    self.movement_starts.remove(&guid)
                {
                    let advanced = (coordinate(progress, "x")? - x).powi(2)
                        + (coordinate(progress, "y")? - y).powi(2)
                        > 0.05 * 0.05;
                    if generation == started_generation && now > started && advanced {
                        self.movements.push(((now - started) as u64, guid));
                    }
                }
            }
        }
        if let Some(foreground) = optional(runner, "foreground")? {
            if let Some(movement) = foreground["running"].get("Movement") {
                let started = signed(foreground, "started_micros")?;
                if started == now {
                    self.movement_starts.insert(
                        guid,
                        (
                            generation,
                            started,
                            coordinate(movement, "from_x")?,
                            coordinate(movement, "from_y")?,
                        ),
                    );
                    self.routes
                        .push((unsigned(runner, "route_expansions")?, guid));
                }
                return Ok(());
            }
        }
        self.movement_starts.remove(&guid);
        Ok(())
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
            "route_expansions_per_movement_attempt":distribution(&self.routes),
            "completed_movement_micros":distribution(&self.movements),
            "deferred_bot_observations":self.deferred_observations,"accelerated_due_observations":self.accelerated_observations,
            "slowest_due_bot":self.slowest_due,
            "progress_scope":"new authoritative movement, lost target health, resolved cast or Quest progress fields",
            "movement_scope":"foregrounds started and observed arrived with a changed position inside the measured window",
            "fairness_scope":"each pass verifies earliest due order; count spread also reflects damage-triggered scheduling"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> Value {
        json!({"generation":1,"movement_progress":{"none":[]},"combat_progress":{"none":[]},
            "cast_progress":{"none":[]},"quest_progress":[],"foreground":{"none":[]},"route_expansions":0})
    }

    #[test]
    fn accepted_work_is_not_observed_progress() {
        let before = runner();
        let mut after = before.clone();
        after["last_outcome"] = json!({"Accepted":[]});
        assert!(!has_progress(&after, &before).unwrap());
        after["cast_progress"] =
            json!({"some":{"observed_micros":900,"scheduled_id":7,"spell":585,"target":44}});
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
        state["foreground"] = json!({"some":{"started_micros":100,"running":{"Movement":{"from_x":10.0,"from_y":10.0}}}});
        state["route_expansions"] = json!(27);
        measurement.movement(&state, 11, 100).unwrap();
        measurement.movement(&state, 11, 200).unwrap();
        state["foreground"] = json!({"none":[]});
        state["movement_progress"] =
            json!({"some":{"observed_micros":900,"arrived":true,"x":20.0,"y":10.0}});
        measurement.movement(&state, 11, 900).unwrap();
        assert_eq!(
            measurement.report()["route_expansions_per_movement_attempt"]["total"],
            27
        );
        assert_eq!(
            measurement.report()["completed_movement_micros"]["max"],
            800
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
}

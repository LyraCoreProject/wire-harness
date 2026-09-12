use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::analysis::{coordinate, distribution, movement_identity, unsigned};

pub const ENTITY: &str = "game_world_entity";
pub const TABLES: [&str; 2] = ["game_creature_spline", ENTITY];
const RUNNER: &str = "pkg_playerbots_runner";

#[derive(Default)]
pub struct Movement {
    expected: BTreeSet<u64>,
    runners: BTreeMap<u64, Value>,
    entities: BTreeMap<u64, Value>,
    creatures: BTreeMap<u64, Value>,
    creature_entries: BTreeSet<u32>,
    splines: BTreeMap<u64, Value>,
    active: BTreeMap<u64, Leg>,
    completed: Vec<(u64, u64)>,
    interrupted: u64,
}

struct Leg {
    spline: Value,
    owner: Value,
}

fn replace(
    rows: &mut BTreeMap<u64, Value>,
    update: &Value,
    table: &str,
    key: &str,
    expected: &BTreeSet<u64>,
) -> Result<()> {
    let Some(changes) = update.get(table) else {
        return Ok(());
    };
    for row in changes["deletes"]
        .as_array()
        .context("missing movement deletes")?
    {
        let guid = unsigned(row, key)?;
        if rows.remove(&guid).as_ref() != Some(row) {
            bail!("deleted {table} row differs from the last observation for {guid}");
        }
    }
    for row in changes["inserts"]
        .as_array()
        .context("missing movement inserts")?
    {
        let guid = unsigned(row, key)?;
        if !expected.contains(&guid) || rows.insert(guid, row.clone()).is_some() {
            bail!("unstaged or duplicate movement row for {guid}");
        }
    }
    Ok(())
}

fn replace_entities(
    bots: &mut BTreeMap<u64, Value>,
    creatures: &mut BTreeMap<u64, Value>,
    update: &Value,
    expected: &BTreeSet<u64>,
    creature_entries: &BTreeSet<u32>,
) -> Result<()> {
    let Some(changes) = update.get(ENTITY) else {
        return Ok(());
    };
    for row in changes["deletes"]
        .as_array()
        .context("missing entity deletes")?
    {
        let guid = unsigned(row, "guid")?;
        let rows = if expected.contains(&guid) {
            &mut *bots
        } else {
            let entry =
                u32::try_from(unsigned(row, "entry")?).context("entity entry exceeds u32")?;
            if !creature_entries.contains(&entry) {
                bail!("unexpected evidence entity {guid} with entry {entry}");
            }
            &mut *creatures
        };
        if rows.remove(&guid).as_ref() != Some(row) {
            bail!("deleted entity row differs from the last observation for {guid}");
        }
    }
    for row in changes["inserts"]
        .as_array()
        .context("missing entity inserts")?
    {
        let guid = unsigned(row, "guid")?;
        let rows = if expected.contains(&guid) {
            &mut *bots
        } else {
            let entry =
                u32::try_from(unsigned(row, "entry")?).context("entity entry exceeds u32")?;
            if !creature_entries.contains(&entry) {
                bail!("unexpected evidence entity {guid} with entry {entry}");
            }
            &mut *creatures
        };
        if rows.insert(guid, row.clone()).is_some() {
            bail!("insert replaced entity {guid} without a matching deletion");
        }
    }
    Ok(())
}

fn owner(rows: &BTreeMap<u64, Value>, guid: u64) -> Result<Option<Value>> {
    rows.get(&guid)
        .map(movement_identity)
        .transpose()
        .map(Option::flatten)
}

fn at_endpoint(entity: &Value, spline: &Value) -> Result<bool> {
    let same_partition = unsigned(entity, "map_id")? == unsigned(spline, "map_id")?
        && unsigned(entity, "instance_id")? == unsigned(spline, "instance_id")?;
    let mut distance_squared = 0.0;
    let mut travelled_squared = 0.0;
    for (at, start, end) in [("x", "sx", "dx"), ("y", "sy", "dy"), ("z", "sz", "dz")] {
        let destination = coordinate(spline, end)?;
        distance_squared += (coordinate(entity, at)? - destination).powi(2);
        travelled_squared += (destination - coordinate(spline, start)?).powi(2);
    }
    Ok(same_partition && distance_squared <= 0.05 * 0.05 && travelled_squared > 0.05 * 0.05)
}

impl Movement {
    pub fn new(expected: &BTreeSet<u64>, creature_entries: BTreeSet<u32>) -> Self {
        Self {
            expected: expected.clone(),
            creature_entries,
            ..Self::default()
        }
    }

    pub fn ready(&self) -> Result<()> {
        if self.entities.keys().copied().collect::<BTreeSet<_>>() != self.expected {
            bail!("movement observation has not received every staged Core entity");
        }
        Ok(())
    }

    pub fn observe(&mut self, update: &Value, measured: bool) -> Result<()> {
        let previous_owners = self
            .active
            .keys()
            .map(|&guid| Ok((guid, owner(&self.runners, guid)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        replace(
            &mut self.runners,
            update,
            RUNNER,
            "character_guid",
            &self.expected,
        )?;
        replace_entities(
            &mut self.entities,
            &mut self.creatures,
            update,
            &self.expected,
            &self.creature_entries,
        )?;
        replace(&mut self.splines, update, TABLES[0], "guid", &self.expected)?;
        if !measured {
            self.active.clear();
            return Ok(());
        }
        let now = update["pkg_playerbots_scheduler"]["inserts"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["observed_micros"].as_u64());
        if let Some(deletes) = update[TABLES[0]]["deletes"].as_array() {
            for spline in deletes {
                let guid = unsigned(spline, "guid")?;
                if let Some(leg) = self.active.remove(&guid) {
                    let start = unsigned(spline, "start_micros")?;
                    let due = start
                        .checked_add(unsigned(spline, "dur_ms")?.saturating_mul(1000))
                        .context("movement arrival time overflow")?;
                    let same_owner = previous_owners.get(&guid).and_then(Option::as_ref)
                        == Some(&leg.owner)
                        && self.runners.get(&guid).map(|row| &row["generation"])
                            == Some(&leg.owner[0]);
                    let arrived = self
                        .entities
                        .get(&guid)
                        .map(|entity| at_endpoint(entity, spline))
                        .transpose()?
                        .unwrap_or(false);
                    if leg.spline == *spline
                        && same_owner
                        && arrived
                        && now.is_some_and(|now| now >= due)
                    {
                        self.completed
                            .push((now.context("arrival tick missing")? - start, guid));
                    } else {
                        self.interrupted += 1;
                    }
                }
            }
        }
        let replaced = self
            .active
            .iter()
            .filter_map(|(&guid, leg)| match owner(&self.runners, guid) {
                Ok(Some(identity)) if identity == leg.owner => None,
                Ok(_) => Some(Ok(guid)),
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;
        for guid in replaced {
            self.active.remove(&guid);
            self.interrupted += 1;
        }
        if let Some(inserts) = update[TABLES[0]]["inserts"].as_array() {
            for spline in inserts {
                let guid = unsigned(spline, "guid")?;
                let Some(identity) = owner(&self.runners, guid)? else {
                    continue;
                };
                let same_transaction = update[RUNNER]["inserts"].as_array().is_some_and(|rows| {
                    rows.iter().any(|runner| {
                        runner["character_guid"].as_u64() == Some(guid)
                            && runner["observed_micros"] == spline["start_micros"]
                            && runner["generation"] == identity[0]
                    })
                });
                if unsigned(spline, "dur_ms")? == 0
                    || !same_transaction
                    || identity[3].as_u64() != Some(unsigned(spline, "start_micros")?)
                    || identity[1].as_u64() != Some(unsigned(spline, "map_id")?)
                    || identity[2].as_u64() != Some(unsigned(spline, "instance_id")?)
                {
                    continue;
                }
                self.active.insert(
                    guid,
                    Leg {
                        spline: spline.clone(),
                        owner: identity,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn report(&self) -> Value {
        json!({"completed_leg_micros":distribution(&self.completed),"interrupted_observed_legs":self.interrupted,
            "unfinished_observed_legs":self.active.len(),
            "retained_declared_creatures":self.creatures.len(),
            "declared_creature_entries":self.creature_entries,
            "scope":"Core spline start to tick-observed deletion at its reached endpoint; exact runner generation, partition, candidate and start identity; both boundaries inside the measured window"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(start: u64) -> Value {
        json!({"character_guid":11,"generation":2,"observed_micros":start,"foreground":{"some":{
            "generation":2,"map_id":0,"instance_id":0,"started_micros":start,
            "candidate":{"id":1},"running":{"movement":{"destination":{"x":700}}}}}})
    }
    fn entity(x: f64) -> Value {
        json!({"guid":11,"map_id":0,"instance_id":0,"x":x,"y":10.0,"z":50.0})
    }
    fn spline(start: u64) -> Value {
        json!({"guid":11,"start_micros":start,"dur_ms":1000,"map_id":0,"instance_id":0,
            "sx":0.0,"sy":10.0,"sz":50.0,"dx":7.0,"dy":10.0,"dz":50.0})
    }
    fn started(measured: bool) -> Movement {
        let mut movement = Movement::new(&BTreeSet::from([11]), BTreeSet::new());
        movement
            .observe(
                &json!({RUNNER:{"deletes":[],"inserts":[runner(100)]},
            TABLES[1]:{"deletes":[],"inserts":[entity(0.0)]},
            TABLES[0]:{"deletes":[],"inserts":[spline(100)]}}),
                measured,
            )
            .unwrap();
        movement.ready().unwrap();
        movement
    }
    fn finish(at: f64, now: u64) -> Value {
        json!({TABLES[0]:{"deletes":[spline(100)],"inserts":[]},
            TABLES[1]:{"deletes":[entity(0.0)],"inserts":[entity(at)]},
            "pkg_playerbots_scheduler":{"inserts":[{"observed_micros":now}]}})
    }

    #[test]
    fn a_short_core_leg_completes_before_the_final_destination() {
        let mut movement = started(true);
        movement.observe(&finish(7.0, 1_000_150), true).unwrap();
        assert_eq!(movement.report()["completed_leg_micros"]["max"], 1_000_050);
    }

    #[test]
    fn a_cancelled_or_early_leg_does_not_supply_an_arrival() {
        for (at, now) in [(3.0, 1_000_150), (7.0, 500_000)] {
            let mut movement = started(true);
            movement.observe(&finish(at, now), true).unwrap();
            assert_eq!(movement.report()["completed_leg_micros"]["count"], 0);
        }
    }

    #[test]
    fn warmup_and_replaced_ownership_cannot_supply_a_measured_leg() {
        let mut movement = started(false);
        movement.observe(&finish(7.0, 1_000_150), true).unwrap();
        assert_eq!(movement.report()["completed_leg_micros"]["count"], 0);
        let mut movement = started(true);
        movement
            .observe(
                &json!({RUNNER:{"deletes":[runner(100)],"inserts":[runner(200)]}}),
                true,
            )
            .unwrap();
        movement.observe(&finish(7.0, 1_000_150), true).unwrap();
        assert_eq!(movement.report()["completed_leg_micros"]["count"], 0);
    }

    #[test]
    fn a_declared_rabbit_stays_outside_bot_movement_accounting() {
        let expected = BTreeSet::from([11]);
        let mut movement = Movement::new(&expected, BTreeSet::from([721]));
        let bot = json!({"guid":11,"entry":0,"map_id":0,"instance_id":0,"x":0.0,"y":10.0,"z":50.0});
        let creature =
            json!({"guid":90,"entry":721,"map_id":0,"instance_id":0,"x":1.0,"y":10.0,"z":50.0});
        movement
            .observe(
                &json!({ENTITY:{"deletes":[],"inserts":[bot,creature.clone()]}}),
                false,
            )
            .unwrap();
        movement.ready().unwrap();
        let moved =
            json!({"guid":90,"entry":721,"map_id":0,"instance_id":0,"x":2.0,"y":10.0,"z":50.0});
        movement
            .observe(
                &json!({ENTITY:{"deletes":[creature],"inserts":[moved]}}),
                true,
            )
            .unwrap();
        let report = movement.report();
        assert_eq!(report["completed_leg_micros"]["count"], 0);
        assert_eq!(report["retained_declared_creatures"], 1);
        assert_eq!(report["declared_creature_entries"], json!([721]));
    }

    #[test]
    fn an_undeclared_creature_cannot_enter_the_entity_stream() {
        let mut movement = Movement::new(&BTreeSet::from([11]), BTreeSet::from([721]));
        let error = movement
            .observe(
                &json!({ENTITY:{"deletes":[],"inserts":[{"guid":90,"entry":7}]}}),
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("unexpected evidence entity"));
    }
}

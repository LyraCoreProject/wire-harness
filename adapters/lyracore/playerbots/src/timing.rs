use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub type Decision = (u64, u64, i64);

pub fn decision(runner: &Value) -> Result<Decision> {
    Ok((
        runner["character_guid"]
            .as_u64()
            .context("missing timing Character")?,
        runner["generation"]
            .as_u64()
            .context("missing timing generation")?,
        runner["observed_micros"]
            .as_i64()
            .context("missing timing observation")?,
    ))
}

fn parse(row: &Value) -> Result<Option<(Decision, f64)>> {
    let message = row["message"].as_str().context("log row has no message")?;
    if !message.contains("playerbots_decision ") {
        return Ok(None);
    }
    let (identity, duration) = message
        .strip_prefix("Timing span \"playerbots_decision ")
        .and_then(|value| value.split_once("\": "))
        .context("malformed decision timing message")?;
    let fields: Vec<_> = identity.split(' ').collect();
    if fields.len() != 3 {
        bail!("decision timing has unexpected identity fields");
    }
    let key = (
        fields[0]
            .strip_prefix("guid=")
            .context("missing timing GUID")?
            .parse::<u64>()?,
        fields[1]
            .strip_prefix("generation=")
            .context("missing timing generation")?
            .parse::<u64>()?,
        fields[2]
            .strip_prefix("observed_micros=")
            .context("missing timing observation")?
            .parse::<i64>()?,
    );
    if key.0 == 0 || key.1 == 0 || key.2 <= 0 {
        bail!("decision timing has an invalid identity");
    }
    let micros = [
        ("ns", 0.001),
        ("µs", 1.0),
        ("ms", 1000.0),
        ("s", 1_000_000.0),
    ]
    .into_iter()
    .find_map(|(unit, scale)| duration.strip_suffix(unit).map(|value| (value, scale)))
    .context("unsupported timing duration unit")?;
    let micros = micros.0.parse::<f64>()? * micros.1;
    if !micros.is_finite() || micros < 0.0 {
        bail!("decision timing is negative or non-finite");
    }
    Ok(Some((key, micros)))
}

pub fn report(raw: &str, expected: &BTreeSet<Decision>) -> Result<Value> {
    if expected.is_empty() {
        bail!("no measured decisions to correlate");
    }
    let mut samples = BTreeMap::new();
    let mut excluded = 0_u64;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let row: Value = serde_json::from_str(line)?;
        let Some((key, micros)) = parse(&row)? else {
            continue;
        };
        if !expected.contains(&key) {
            excluded += 1;
            continue;
        }
        if row["function"] != "tick_creatures" {
            bail!("measured decision was produced outside the ordinary scheduled tick");
        }
        if samples.insert(key, micros).is_some() {
            bail!("duplicate timing for measured decision {key:?}");
        }
    }
    if samples.len() != expected.len() {
        let missing: Vec<_> = expected
            .iter()
            .filter(|key| !samples.contains_key(key))
            .take(10)
            .collect();
        bail!(
            "missing timing samples: expected {}, found {}; first missing {missing:?}",
            expected.len(),
            samples.len()
        );
    }
    let mut sorted: Vec<_> = samples.into_iter().collect();
    sorted.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let percentile = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100) - 1].1;
    let maximum = sorted.last().context("no correlated decision durations")?;
    let total: f64 = sorted.iter().map(|row| row.1).sum();
    if !total.is_finite() {
        bail!("decision duration total is non-finite");
    }
    Ok(
        json!({"count":sorted.len(),"p50_micros":percentile(50),"p95_micros":percentile(95),
        "max_micros":maximum.1,"max_guid":maximum.0.0,"max_generation":maximum.0.1,
        "max_observed_micros":maximum.0.2,"total_micros":total,"excluded_timing_samples":excluded,
        "scope":"debug host elapsed duration around runner::run, including timer overhead; not operating-system CPU time"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(guid: u64, observed: i64, duration: &str) -> String {
        json!({"function":"tick_creatures","message":format!(
            "Timing span \"playerbots_decision guid={guid} generation=1 observed_micros={observed}\": {duration}")}).to_string()
    }

    #[test]
    fn emitted_units_and_exact_decision_identity_define_the_distribution() {
        let rows = [
            log(1, 100, "408.756µs"),
            log(2, 100, "1.5ms"),
            log(1, 200, "700ns"),
            log(2, 200, "2s"),
            log(1, 99, "90s"),
        ]
        .join("\n");
        let expected = BTreeSet::from([(1, 1, 100), (2, 1, 100), (1, 1, 200), (2, 1, 200)]);
        let result = report(&rows, &expected).unwrap();
        assert_eq!(result["count"], 4);
        assert_eq!(result["p50_micros"], 408.756);
        assert_eq!(result["p95_micros"], 2_000_000.0);
        assert_eq!(result["max_guid"], 2);
        assert_eq!(result["max_observed_micros"], 200);
        assert_eq!(result["excluded_timing_samples"], 1);
    }

    #[test]
    fn missing_duplicate_and_fixture_decisions_cannot_produce_a_report() {
        let expected = BTreeSet::from([(1, 1, 100)]);
        assert!(report(&log(1, 99, "1ms"), &expected).is_err());
        assert!(report(
            &[log(1, 100, "1ms"), log(1, 100, "2ms")].join("\n"),
            &expected
        )
        .is_err());
        assert!(report(
            &log(1, 100, "1ms").replace("tick_creatures", "fixture_once"),
            &expected
        )
        .is_err());
        assert!(report(&log(1, 100, "NaNs"), &expected).is_err());
        assert!(report(&log(1, 100, "-1ms"), &expected).is_err());
    }
}

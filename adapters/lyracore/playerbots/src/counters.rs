use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use wire_client::metrics::{label_value, Snapshot};

fn selected(key: &str, name: &str, labels: &[(&str, &str)]) -> bool {
    let split = key.find('{').unwrap_or(key.len());
    key[..split] == *name
        && labels
            .iter()
            .all(|(label, value)| label_value(&key[split..], label).as_deref() == Some(*value))
}

fn grouped_delta(
    before: &Snapshot,
    after: &Snapshot,
    name: &str,
    labels: &[(&str, &str)],
    group: &str,
) -> Result<BTreeMap<String, f64>> {
    before.counter_delta(after, name, labels)?;
    let mut groups = BTreeMap::new();
    for (key, value) in &after.0 {
        if selected(key, name, labels) {
            let encoded = &key[key.find('{').unwrap_or(key.len())..];
            let label = label_value(encoded, group)
                .with_context(|| format!("counter {name} has no {group} label"))?;
            *groups.entry(label).or_insert(0.0) +=
                value - before.0.get(key).copied().unwrap_or(0.0);
        }
    }
    Ok(groups)
}

fn histogram(
    before: &Snapshot,
    after: &Snapshot,
    name: &str,
    labels: &[(&str, &str)],
) -> Result<Value> {
    let count = before.counter_delta(after, &format!("{name}_count"), labels)?;
    let sum = before.counter_delta(after, &format!("{name}_sum"), labels)?;
    if count.fract() != 0.0 {
        bail!("histogram count is fractional");
    }
    let mut buckets = grouped_delta(before, after, &format!("{name}_bucket"), labels, "le")?
        .into_iter()
        .map(|(bound, count)| Ok((bound.parse::<f64>()?, count)))
        .collect::<Result<Vec<_>>>()?;
    if buckets
        .iter()
        .any(|(bound, count)| bound.is_nan() || *bound < 0.0 || count.fract() != 0.0)
    {
        bail!("invalid histogram bucket");
    }
    buckets.sort_by(|left, right| left.0.total_cmp(&right.0));
    if buckets.last() != Some(&(f64::INFINITY, count))
        || buckets
            .windows(2)
            .any(|pair| pair[0].1 > pair[1].1 || pair[0].0 == pair[1].0)
    {
        bail!("histogram buckets disagree with its count or cumulative ordering");
    }
    let upper = |fraction: f64| -> Option<f64> {
        if count == 0.0 {
            return None;
        }
        buckets
            .iter()
            .find(|(_, cumulative)| *cumulative >= (count * fraction).ceil())
            .map(|(bound, _)| *bound)
            .filter(|bound| bound.is_finite())
    };
    Ok(
        json!({"count":count,"sum_seconds":sum,"mean_seconds":(count > 0.0).then(||sum/count),
        "p50_upper_bound_seconds":upper(0.50),"p95_upper_bound_seconds":upper(0.95),
        "max_upper_bound_seconds":upper(1.0),"exceeds_largest_finite_bucket":count > 0.0 && upper(1.0).is_none(),
        "scope":"bucket upper bounds, not exact percentiles or an exact maximum"}),
    )
}

pub fn report(before: &Snapshot, after: &Snapshot, identity: &str, seconds: f64) -> Result<Value> {
    if !seconds.is_finite() || seconds <= 0.0 {
        bail!("invalid metric window duration");
    }
    let reducer_labels = [("db", identity), ("txn_type", "Reducer")];
    let database_labels = [("db", identity)];
    let execution = histogram(before, after, "spacetime_txn_cpu_time_sec", &reducer_labels)?;
    let elapsed = histogram(
        before,
        after,
        "spacetime_txn_elapsed_time_sec",
        &reducer_labels,
    )?;
    let wait = histogram(
        before,
        after,
        "spacetime_reducer_wait_time_sec",
        &database_labels,
    )?;
    let transactions = grouped_delta(
        before,
        after,
        "spacetime_num_txns_total",
        &reducer_labels,
        "committed",
    )?;
    if transactions
        .keys()
        .any(|key| key != "true" && key != "false")
    {
        bail!("unknown transaction outcome label");
    }
    let inserted = grouped_delta(
        before,
        after,
        "spacetime_num_rows_inserted_total",
        &reducer_labels,
        "table_name",
    )?;
    let deleted = grouped_delta(
        before,
        after,
        "spacetime_num_rows_deleted_total",
        &reducer_labels,
        "table_name",
    )?;
    let execution_seconds = execution["sum_seconds"]
        .as_f64()
        .context("missing transaction execution duration")?;
    Ok(
        json!({"window_seconds":seconds,"selected_database":identity,
        "reducer_execution":execution,"reducer_elapsed":elapsed,"module_operation_queue_wait":wait,
        "committed_reducer_transactions":transactions.get("true").copied().unwrap_or(0.0),
        "rolled_back_reducer_transactions":transactions.get("false").copied().unwrap_or(0.0),
        "physical_rows_inserted_by_table":inserted,"physical_rows_deleted_by_table":deleted,
        "reducer_writer_time_ratio":execution_seconds/seconds,
        "occupancy_scope":"reported reducer execution durations divided by the widest scrape interval; excludes later commit callbacks and lock-release work; asynchronous metrics and boundary transactions prevent an exact occupancy claim",
        "execution_scope":"spacetime_txn_cpu_time_sec is elapsed minus lock wait, not operating-system CPU time",
        "queue_scope":"all selected Module operations; the metric labels do not include transaction type"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire_client::metrics::parse;

    #[test]
    fn zero_rollbacks_require_an_observed_transaction_family() {
        let before =
            parse("spacetime_num_txns_total{db=\"a\",txn_type=\"Reducer\",committed=\"true\"} 2\n")
                .unwrap();
        let after =
            parse("spacetime_num_txns_total{db=\"a\",txn_type=\"Reducer\",committed=\"true\"} 7\n")
                .unwrap();
        let groups = grouped_delta(
            &before,
            &after,
            "spacetime_num_txns_total",
            &[("db", "a")],
            "committed",
        )
        .unwrap();
        assert_eq!(groups.get("true"), Some(&5.0));
        assert!(!groups.contains_key("false"));
        assert!(grouped_delta(
            &Snapshot::default(),
            &Snapshot::default(),
            "spacetime_num_txns_total",
            &[("db", "a")],
            "committed"
        )
        .is_err());
    }

    #[test]
    fn a_queue_histogram_reports_bounds_instead_of_inventing_an_exact_maximum() {
        let before = parse("q_count{db=\"a\"} 0\nq_sum{db=\"a\"} 0\nq_bucket{db=\"a\",le=\"0.001\"} 0\nq_bucket{db=\"a\",le=\"0.01\"} 0\nq_bucket{db=\"a\",le=\"+Inf\"} 0\n").unwrap();
        let after = parse("q_count{db=\"a\"} 4\nq_sum{db=\"a\"} 0.024\nq_bucket{db=\"a\",le=\"0.001\"} 2\nq_bucket{db=\"a\",le=\"0.01\"} 3\nq_bucket{db=\"a\",le=\"+Inf\"} 4\n").unwrap();
        let report = histogram(&before, &after, "q", &[("db", "a")]).unwrap();
        assert_eq!(report["p50_upper_bound_seconds"], 0.001);
        assert_eq!(report["p95_upper_bound_seconds"], Value::Null);
        assert_eq!(report["exceeds_largest_finite_bucket"], true);
        assert_eq!(report["mean_seconds"], 0.006);
    }

    #[test]
    fn a_nonmonotonic_window_cannot_produce_a_latency_report() {
        let before = parse("q_count{db=\"a\"} 0\nq_sum{db=\"a\"} 0\nq_bucket{db=\"a\",le=\"0.001\"} 0\nq_bucket{db=\"a\",le=\"+Inf\"} 0\n").unwrap();
        let after = parse("q_count{db=\"a\"} 1\nq_sum{db=\"a\"} 0.001\nq_bucket{db=\"a\",le=\"0.001\"} 2\nq_bucket{db=\"a\",le=\"+Inf\"} 1\n").unwrap();
        assert!(histogram(&before, &after, "q", &[("db", "a")]).is_err());
    }
}

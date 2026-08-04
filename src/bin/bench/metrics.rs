//! Server-side measurement, taken from OUTSIDE the server: SpacetimeDB's own Prometheus
//! endpoint (`GET /v1/metrics` on the node's HTTP port).
//!
//! This is what lets the benchmark report writer occupancy, per-reducer tx/s and event-table
//! insert/reap rates without a single line of instrumentation inside `module/` or `gateway/`
//! (work-item #18: "prefer reading it from outside"). The four numbers the ticket asks for map
//! onto these node metrics:
//!
//! | benchmark metric        | node metric                                            |
//! |-------------------------|--------------------------------------------------------|
//! | writer occupancy %      | `spacetime_txn_cpu_time_sec_sum` Δ / wall-clock Δ      |
//! | tx/s by reducer         | `spacetime_num_txns_total{txn_type="Reducer"}` Δ / Δt  |
//! | event insert/reap rates | `spacetime_num_rows_{inserted,deleted}_total` Δ / Δt   |
//! | queueing (saturation)   | `spacetime_reducer_wait_time_sec_{sum,count}` Δ        |
//!
//! `spacetime_txn_cpu_time_sec` is "time spent executing a transaction, EXCLUDING time spent
//! waiting to acquire database locks" — i.e. exactly the serialized writer's busy time, which is
//! the quantity `docs/capacity-analysis.md` §3.3 models as "writer occupancy".
//!
//! ponytail: the scrape is a 20-line HTTP/1.1 GET and the parser is `rsplit_once(' ')`, because a
//! Prometheus text exposition line is `name{labels} value` and nothing here needs more. Ceiling:
//! label values containing a space or `}` would mis-split, and exemplars (`# {trace_id=...}`)
//! aren't handled — SpacetimeDB emits neither. Upgrade path if that ever changes: `prometheus-parse`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

/// One scrape, keyed on the VERBATIM `name{labels}` text of each sample line.
///
/// Keeping the raw key (rather than a parsed label map) makes [`Snapshot::delta`] a trivial
/// key-wise subtraction and keeps filtering to substring matches on the label blob.
#[derive(Clone, Debug, Default)]
pub struct Snapshot(pub BTreeMap<String, f64>);

/// Fetch a `http://host[:port]/path` URL. No TLS, no redirects, no deps.
pub fn http_get(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// URLs are supported (got {url:?})"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let mut s = TcpStream::connect(&addr).with_context(|| format!("connect metrics {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).context("read metrics response")?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response from {url}"))?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200") {
        bail!("metrics endpoint {url} returned {status:?}");
    }
    Ok(body.to_string())
}

/// Parse the Prometheus text exposition format into a [`Snapshot`].
pub fn parse(text: &str) -> Snapshot {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(v) = val.trim().parse::<f64>() else {
            continue;
        };
        m.insert(key.to_string(), v);
    }
    Snapshot(m)
}

/// Scrape + parse in one go.
pub fn scrape(url: &str) -> Result<Snapshot> {
    Ok(parse(&http_get(url)?))
}

/// Split a sample key into `(metric_name, "{labels}" or "")`.
/// The counter families this benchmark deltas, and therefore the only ones where a negative delta
/// means anything. Every figure in the report comes from one of these; anything else in the scrape
/// is either a gauge or unread. Keep this in step with `report.rs`/`main.rs` — a family added there
/// and not here loses its reset protection, which is a quieter failure than the false alarms this
/// list exists to stop, so it is worth checking when a new metric is read.
pub const MONOTONIC_FAMILIES: &[&str] = &[
    "spacetime_txn_cpu_time_sec_sum",
    "spacetime_num_txns_total",
    "spacetime_num_rows_inserted_total",
    "spacetime_num_rows_deleted_total",
    "spacetime_num_rows_scanned_total",
    "spacetime_num_bytes_sent_to_clients_total",
    "spacetime_reducer_wait_time_sec_sum",
    "spacetime_reducer_wait_time_sec_count",
];

fn split_key(key: &str) -> (&str, &str) {
    match key.find('{') {
        Some(i) => (&key[..i], &key[i..]),
        None => (key, ""),
    }
}

/// Extract a label value out of a `{a="1",b="2"}` blob.
pub fn label_value(labels: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = labels.find(&pat)? + pat.len();
    let rest = &labels[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

impl Snapshot {
    /// `later - self`, key-wise. Keys absent from `self` count as having started at 0 (a series
    /// that only appeared during the window — e.g. the first call of a reducer).
    pub fn delta(&self, later: &Snapshot) -> Snapshot {
        Snapshot(
            later
                .0
                .iter()
                .map(|(k, v)| (k.clone(), v - self.0.get(k).copied().unwrap_or(0.0)))
                .collect(),
        )
    }

    /// Sum every sample of `name` whose label blob contains all of `filters`
    /// (raw substrings, e.g. `txn_type="Reducer"`).
    pub fn sum(&self, name: &str, filters: &[&str]) -> f64 {
        self.matching(name, filters).map(|(_, v)| *v).sum()
    }

    /// Every `(key, value)` of `name` whose label blob contains all of `filters`.
    fn matching<'a>(
        &'a self,
        name: &'a str,
        filters: &'a [&'a str],
    ) -> impl Iterator<Item = (&'a String, &'a f64)> + 'a {
        self.0.iter().filter(move |(k, _)| {
            let (n, labels) = split_key(k);
            n == name && filters.iter().all(|f| labels.contains(f))
        })
    }

    /// Does ANY sample match? [`Snapshot::sum`] cannot answer this: it returns `0.0` both for "the
    /// series summed to zero" and for "no such series exists". The difference is the whole ball
    /// game — a `--db` prefix that matches nothing makes every server-side number in the report
    /// `0.0`, which reads exactly like a completely idle writer. Preflight uses this to refuse the
    /// run instead of publishing a confident zero.
    pub fn has_any(&self, name: &str, filters: &[&str]) -> bool {
        self.matching(name, filters).next().is_some()
    }

    /// On a DELTA snapshot: the first sample that went BACKWARDS, if any — restricted to
    /// [`MONOTONIC_FAMILIES`], the counters this benchmark actually deltas. For those, a negative
    /// delta can only mean the counters were reset underneath the measured window (node restart,
    /// module republish), in which case every rate and the occupancy figure for that window are
    /// garbage and must be flagged rather than reported.
    ///
    /// THE RESTRICTION IS THE WHOLE POINT. This used to scan every sample in the snapshot, and the
    /// scrape also carries GAUGES — `spacetime_num_table_rows` and
    /// `spacetime_data_size_bytes_used_by_rows` — which go DOWN whenever rows are deleted. The
    /// module reaps every `game_*_event` table once a second and deletes creature splines
    /// continuously, so a healthy server produces negative gauge deltas in essentially every window.
    /// On the first real ramp that voided rungs 50 and 100 (on `game_creature_move_event` shrinking
    /// by 24 rows, and `game_creature_spline` by 240 bytes) — i.e. it reported the server working
    /// correctly as a measurement failure, and would have voided the entire benchmark this way.
    pub fn first_negative(&self, filters: &[&str]) -> Option<(&String, f64)> {
        self.0
            .iter()
            .filter(|(k, _)| {
                let (name, labels) = split_key(k);
                MONOTONIC_FAMILIES.contains(&name) && filters.iter().all(|f| labels.contains(f))
            })
            .find(|(_, v)| **v < 0.0)
            .map(|(k, v)| (k, *v))
    }

    /// Every distinct `db=` label value on the node, sorted.
    pub fn databases(&self) -> Vec<String> {
        self.0
            .keys()
            .map(|k| split_key(k).1)
            .filter_map(|labels| label_value(labels, "db"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The `db=` identities that expose `name` — i.e. the databases this report could actually
    /// measure. Distinct from [`Snapshot::databases`], which also lists the node's placeholder
    /// label (it carries only zeroed bookkeeping series, never transaction CPU).
    pub fn databases_with(&self, name: &str) -> Vec<String> {
        self.matching(name, &[])
            .filter_map(|(k, _)| label_value(split_key(k).1, "db"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Sum `name` grouped by one label, sorted by value descending. Empty label values are
    /// dropped (SpacetimeDB emits `reducer=""` for non-reducer transaction types), and so are
    /// zero-valued groups — over a DELTA snapshot those are series that simply didn't move during
    /// the window, and they would otherwise crowd out real work in a top-N list.
    pub fn group_by(&self, name: &str, label: &str, filters: &[&str]) -> Vec<(String, f64)> {
        let mut acc: BTreeMap<String, f64> = BTreeMap::new();
        for (k, v) in &self.0 {
            let (n, labels) = split_key(k);
            if n != name || !filters.iter().all(|f| labels.contains(f)) {
                continue;
            }
            let Some(val) = label_value(labels, label) else {
                continue;
            };
            if val.is_empty() {
                continue;
            }
            *acc.entry(val).or_default() += *v;
        }
        let mut out: Vec<(String, f64)> = acc.into_iter().filter(|(_, v)| *v != 0.0).collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// Substring filter selecting one database on the node: `db="<prefix>`. An empty `db` yields an
/// always-true filter (aggregate every database — only safe on a node hosting ONE database; see
/// `main.rs::validate_db_selection`).
pub fn db_filter(db: &str) -> String {
    if db.is_empty() {
        String::new()
    } else {
        format!("db=\"{db}")
    }
}

/// The node metric families the server-side half of the report is built from: `(report field,
/// metric name)`. Preflight refuses the run if the `--db` selection exposes NONE of them, and
/// parks (in the report's own `parked[]`) any individual family it cannot see — so a field that
/// reads `0` because the metric was missing is never mistaken for a measured zero.
pub const REQUIRED_FAMILIES: &[(&str, &str)] = &[
    ("writer occupancy %", "spacetime_txn_cpu_time_sec_sum"),
    ("tx/s by reducer", "spacetime_num_txns_total"),
    ("event inserts/s", "spacetime_num_rows_inserted_total"),
    ("event reaps/s", "spacetime_num_rows_deleted_total"),
    (
        "queue wait (saturation)",
        "spacetime_reducer_wait_time_sec_sum",
    ),
    (
        "egress bytes/s",
        "spacetime_num_bytes_sent_to_clients_total",
    ),
];

/// The one family whose presence defines "this database is measurable at all" — occupancy is THE
/// capacity number, and it is the family the `--db` selection is validated against.
pub const OCCUPANCY_FAMILY: &str = "spacetime_txn_cpu_time_sec_sum";

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# HELP spacetime_txn_cpu_time_sec The time spent executing a transaction
# TYPE spacetime_txn_cpu_time_sec histogram
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Reducer"} 10.5
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Subscribe"} 1.5
spacetime_txn_cpu_time_sec_sum{db="zzz",reducer="",txn_type="Reducer"} 99.0
spacetime_num_txns_total{committed="true",db="abc",reducer="movement_update",txn_type="Reducer"} 400
spacetime_num_txns_total{committed="false",db="abc",reducer="movement_update",txn_type="Reducer"} 4
spacetime_num_txns_total{committed="true",db="abc",reducer="tick_melee",txn_type="Reducer"} 100
spacetime_num_rows_inserted_total{db="abc",table_name="game_movement_event",txn_type="Reducer"} 2000
"#;

    #[test]
    fn parse_reads_labelled_samples_and_skips_comments() {
        let s = parse(SAMPLE);
        assert_eq!(s.0.len(), 7, "7 sample lines, comments and blanks dropped");
        assert_eq!(
            s.0.get(r#"spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Reducer"}"#),
            Some(&10.5)
        );
    }

    #[test]
    fn sum_filters_by_label_substring() {
        let s = parse(SAMPLE);
        // db filter isolates one database; without it both databases are aggregated.
        assert_eq!(
            s.sum("spacetime_txn_cpu_time_sec_sum", &[r#"db="abc"#]),
            12.0
        );
        assert_eq!(
            s.sum(
                "spacetime_txn_cpu_time_sec_sum",
                &[r#"db="abc"#, r#"txn_type="Reducer""#]
            ),
            10.5
        );
        assert_eq!(s.sum("spacetime_txn_cpu_time_sec_sum", &[]), 111.0);
        assert_eq!(s.sum("no_such_metric", &[]), 0.0);
    }

    #[test]
    fn group_by_reducer_sums_committed_and_rolled_back_and_sorts_desc() {
        let s = parse(SAMPLE);
        let g = s.group_by(
            "spacetime_num_txns_total",
            "reducer",
            &[r#"txn_type="Reducer""#],
        );
        assert_eq!(
            g,
            vec![
                ("movement_update".into(), 404.0),
                ("tick_melee".into(), 100.0)
            ]
        );
    }

    #[test]
    fn delta_subtracts_key_wise_and_treats_new_series_as_starting_at_zero() {
        let before = parse(SAMPLE);
        let after = parse(&SAMPLE.replace(
            r#"spacetime_num_txns_total{committed="true",db="abc",reducer="tick_melee",txn_type="Reducer"} 100"#,
            "spacetime_num_txns_total{committed=\"true\",db=\"abc\",reducer=\"tick_melee\",txn_type=\"Reducer\"} 160\n\
             spacetime_num_txns_total{committed=\"true\",db=\"abc\",reducer=\"brand_new\",txn_type=\"Reducer\"} 7",
        ));
        let d = before.delta(&after);
        let g = d.group_by(
            "spacetime_num_txns_total",
            "reducer",
            &[r#"txn_type="Reducer""#],
        );
        assert_eq!(
            g,
            vec![("tick_melee".into(), 60.0), ("brand_new".into(), 7.0)],
            "movement_update did not move during the window, so it is not a group at all"
        );
        assert_eq!(
            d.sum(
                "spacetime_num_txns_total",
                &[r#"reducer="movement_update""#]
            ),
            0.0
        );
    }

    /// The bug this guards is the nastiest one the harness can have: a `--db` prefix that matches
    /// nothing makes EVERY server-side number `0.0`, which renders as a perfectly plausible report
    /// of a completely idle writer. `sum` cannot tell the two apart; `has_any` can.
    #[test]
    fn has_any_separates_an_absent_series_from_a_zero_valued_one() {
        let s = parse(SAMPLE);
        assert!(s.has_any("spacetime_txn_cpu_time_sec_sum", &[r#"db="abc"#]));
        assert!(!s.has_any("spacetime_txn_cpu_time_sec_sum", &[r#"db="deadbeef"#]));
        assert!(!s.has_any("no_such_metric", &[]));
        // The trap: both of these sum to 0.0, but only one of them was measured.
        let zeroed = parse(r#"spacetime_num_txns_total{db="abc",reducer="idle"} 0"#);
        assert_eq!(zeroed.sum("spacetime_num_txns_total", &[r#"db="abc"#]), 0.0);
        assert_eq!(zeroed.sum("spacetime_num_txns_total", &[r#"db="zzz"#]), 0.0);
        assert!(zeroed.has_any("spacetime_num_txns_total", &[r#"db="abc"#]));
        assert!(!zeroed.has_any("spacetime_num_txns_total", &[r#"db="zzz"#]));
    }

    #[test]
    fn first_negative_catches_a_counter_reset_mid_window() {
        let before = parse(SAMPLE);
        // The node restarted: cumulative counters are back near zero, so the delta goes backwards.
        let after = parse(&SAMPLE.replace("} 400", "} 12"));
        let d = before.delta(&after);
        let (key, v) = d.first_negative(&[]).expect("a reset must be detectable");
        assert!(key.contains("movement_update"), "got {key}");
        assert_eq!(v, -388.0);
        // A well-behaved window has no negative sample at all.
        assert!(before.delta(&before).first_negative(&[]).is_none());
    }

    /// A GAUGE going backwards is the server working, not a counter reset.
    ///
    /// `spacetime_num_table_rows` and `spacetime_data_size_bytes_used_by_rows` shrink whenever rows
    /// are deleted, and this module reaps every `game_*_event` table once a second and deletes
    /// creature splines continuously. Scanning them for a "reset" voided rungs 50, 100 AND 150 of
    /// the first real capacity ramp — three for three on a perfectly healthy node — which would have
    /// made every server-side number in the benchmark unusable, and the benchmark is the entire
    /// evidence base for the Phase C gate (#71).
    #[test]
    fn a_shrinking_gauge_is_not_a_counter_reset() {
        let before = parse(
            r#"spacetime_num_table_rows{db="abc",table_name="game_creature_move_event"} 240
spacetime_data_size_bytes_used_by_rows{db="abc",table_name="game_creature_spline"} 4096
spacetime_num_txns_total{db="abc",reducer="movement_update"} 400"#,
        );
        // The reaper ran: both gauges fall. The real counter keeps climbing.
        let after = parse(
            r#"spacetime_num_table_rows{db="abc",table_name="game_creature_move_event"} 216
spacetime_data_size_bytes_used_by_rows{db="abc",table_name="game_creature_spline"} 3856
spacetime_num_txns_total{db="abc",reducer="movement_update"} 700"#,
        );
        assert!(
            before.delta(&after).first_negative(&[]).is_none(),
            "a shrinking gauge was reported as a counter reset — that voids the stage's server-side \
             numbers on a healthy server, which is what it did to three consecutive rungs of the \
             first real ramp"
        );
        // …and a genuine reset of a real counter is still caught.
        let restarted = parse(r#"spacetime_num_txns_total{db="abc",reducer="movement_update"} 12"#);
        assert!(
            before.delta(&restarted).first_negative(&[]).is_some(),
            "a real counter going backwards must still be caught — that is a node restart or a \
             republish underneath the window, and every rate in it is garbage"
        );
    }

    #[test]
    fn databases_lists_identities_and_which_ones_are_measurable() {
        let s = parse(SAMPLE);
        assert_eq!(s.databases(), vec!["abc".to_string(), "zzz".to_string()]);
        // Both databases carry transaction CPU here, so both are measurable — this is exactly the
        // multi-database case where an empty --db would silently conflate two writers.
        assert_eq!(
            s.databases_with(OCCUPANCY_FAMILY),
            vec!["abc".to_string(), "zzz".to_string()]
        );
        // A node whose second identity is the placeholder label (zeroed bookkeeping only) has one
        // measurable database, and an empty --db is safe there.
        let one = parse(
            r#"
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Reducer"} 10.5
spacetime_num_table_rows{db="000abcd",table_name="x"} 135
"#,
        );
        assert_eq!(one.databases().len(), 2);
        assert_eq!(
            one.databases_with(OCCUPANCY_FAMILY),
            vec!["abc".to_string()]
        );
    }

    #[test]
    fn db_filter_is_empty_for_an_unset_db() {
        assert_eq!(db_filter(""), "");
        assert_eq!(db_filter("c200"), r#"db="c200"#);
        // An empty filter must match every sample (used as a `&[&str]` element).
        let s = parse(SAMPLE);
        let f = db_filter("");
        assert_eq!(s.sum("spacetime_txn_cpu_time_sec_sum", &[&f]), 111.0);
    }
}

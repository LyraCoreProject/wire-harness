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
    let addr =
        if hostport.contains(':') { hostport.to_string() } else { format!("{hostport}:80") };
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
        let Some((key, val)) = line.rsplit_once(' ') else { continue };
        let Ok(v) = val.trim().parse::<f64>() else { continue };
        m.insert(key.to_string(), v);
    }
    Snapshot(m)
}

/// Scrape + parse in one go.
pub fn scrape(url: &str) -> Result<Snapshot> {
    Ok(parse(&http_get(url)?))
}

/// Split a sample key into `(metric_name, "{labels}" or "")`.
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
        self.0
            .iter()
            .filter(|(k, _)| {
                let (n, labels) = split_key(k);
                n == name && filters.iter().all(|f| labels.contains(f))
            })
            .map(|(_, v)| *v)
            .sum()
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
            let Some(val) = label_value(labels, label) else { continue };
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
/// always-true filter (aggregate every database — the normal single-module local node).
pub fn db_filter(db: &str) -> String {
    if db.is_empty() {
        String::new()
    } else {
        format!("db=\"{db}")
    }
}

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
        assert_eq!(s.sum("spacetime_txn_cpu_time_sec_sum", &[r#"db="abc"#]), 12.0);
        assert_eq!(
            s.sum("spacetime_txn_cpu_time_sec_sum", &[r#"db="abc"#, r#"txn_type="Reducer""#]),
            10.5
        );
        assert_eq!(s.sum("spacetime_txn_cpu_time_sec_sum", &[]), 111.0);
        assert_eq!(s.sum("no_such_metric", &[]), 0.0);
    }

    #[test]
    fn group_by_reducer_sums_committed_and_rolled_back_and_sorts_desc() {
        let s = parse(SAMPLE);
        let g = s.group_by("spacetime_num_txns_total", "reducer", &[r#"txn_type="Reducer""#]);
        assert_eq!(g, vec![("movement_update".into(), 404.0), ("tick_melee".into(), 100.0)]);
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
        let g = d.group_by("spacetime_num_txns_total", "reducer", &[r#"txn_type="Reducer""#]);
        assert_eq!(
            g,
            vec![("tick_melee".into(), 60.0), ("brand_new".into(), 7.0)],
            "movement_update did not move during the window, so it is not a group at all"
        );
        assert_eq!(d.sum("spacetime_num_txns_total", &[r#"reducer="movement_update""#]), 0.0);
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

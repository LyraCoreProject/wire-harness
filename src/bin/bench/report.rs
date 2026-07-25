//! The benchmark's data model: the ramp plan, the latency percentiles, and the report that gets
//! written as BOTH machine-readable JSON (`--json`) and human-readable text (`--text`).
//!
//! ponytail: the report is a `serde` struct with a `Display`, not a templating engine, and the
//! percentiles come off a sorted `Vec<u32>` (nearest-rank), not a stats crate. Ceiling: no
//! streaming/approximate quantiles, so a very long run holds every sample in RAM (~4 bytes each;
//! a 200-player 60s window is ~20 MB). Upgrade path if that ever bites: t-digest, or thin the
//! sample stream at the source with `--latency-every`.

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
//  Ramp planning
// ---------------------------------------------------------------------------------------------

/// One rung of the ramp: how many players should be live, and how many new ones this rung has to
/// log in to get there. The ramp is ADDITIVE — 50/100/150/200 means "log in 50 more, four times",
/// never "tear down and start over" — so each rung measures a world that has been running since
/// the rung before it, which is what a real population curve looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RampStep {
    pub target: usize,
    pub spawn: usize,
}

/// Turn `--stages 50,100,150,200` into the additive spawn plan.
/// Errors on a non-increasing or zero stage — a shrinking ramp would need player teardown, which
/// this harness deliberately does not do (see the `RampStep` note).
pub fn plan_ramp(stages: &[usize]) -> Result<Vec<RampStep>, String> {
    if stages.is_empty() {
        return Err("empty ramp: pass --stages 50,100,150,200".into());
    }
    let mut plan = Vec::with_capacity(stages.len());
    let mut live = 0usize;
    for &target in stages {
        if target == 0 {
            return Err("a ramp stage must be > 0".into());
        }
        if target <= live {
            return Err(format!(
                "ramp stages must strictly increase (got {target} after {live}) — this harness \
                 only ever ADDS players"
            ));
        }
        plan.push(RampStep { target, spawn: target - live });
        live = target;
    }
    Ok(plan)
}

/// Deterministic, collision-free, letters-only character name for synthetic player `idx`.
/// Vanilla character names are alphabetic, so the index is encoded base-26 in three letters
/// (17,576 distinct names — far past the 200 this benchmark needs).
pub fn char_name_for(prefix: &str, idx: usize) -> String {
    let mut suffix = [b'a'; 3];
    let mut n = idx;
    for slot in suffix.iter_mut().rev() {
        *slot = b'a' + (n % 26) as u8;
        n /= 26;
    }
    format!("{prefix}{}", std::str::from_utf8(&suffix).expect("ascii"))
}

// ---------------------------------------------------------------------------------------------
//  Percentiles
// ---------------------------------------------------------------------------------------------

/// Nearest-rank percentile over an ALREADY-SORTED slice. `p` is 0..=100.
pub fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Client-observed movement latency for one measurement window.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Latency {
    pub samples: usize,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
    pub max_ms: u32,
    pub mean_ms: f64,
}

impl Latency {
    /// Render one percentile for the human report. A window with NO samples reports every
    /// percentile as `0`, which in a latency column reads as "0ms — excellent" rather than
    /// "nothing was observed". Print an em dash instead, so an empty rung is unmistakable.
    pub fn cell(&self, value: u32) -> String {
        if self.samples == 0 {
            "—".to_string()
        } else {
            value.to_string()
        }
    }

    pub fn from_samples(mut v: Vec<u32>) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        v.sort_unstable();
        let sum: u64 = v.iter().map(|&x| x as u64).sum();
        Self {
            samples: v.len(),
            p50_ms: percentile(&v, 50.0),
            p95_ms: percentile(&v, 95.0),
            p99_ms: percentile(&v, 99.0),
            max_ms: *v.last().expect("non-empty"),
            mean_ms: sum as f64 / v.len() as f64,
        }
    }
}

// ---------------------------------------------------------------------------------------------
//  Report
// ---------------------------------------------------------------------------------------------

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Target {
    /// Logon-tier address of the gateway fronting the shard under test.
    pub logon: String,
    /// World-tier address actually connected to (realm-list answer, or the `--world` override).
    pub world: String,
    /// SpacetimeDB node metrics URL.
    pub metrics_url: String,
    /// `db=` label prefix selecting one database on the node; empty = aggregate all.
    pub db: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunConfig {
    pub stages: Vec<usize>,
    pub warmup_secs: u64,
    pub hold_secs: u64,
    pub heartbeat_ms: u64,
    pub combat_pct: usize,
    pub spread_yards: f32,
    pub center: [f32; 3],
    pub login_stagger_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NamedRate {
    pub name: String,
    pub per_sec: f64,
}

/// Insert / delete rates for one table. For the `game_*_event` delivery-buffer tables the delete
/// rate IS the reap rate (they are only ever removed by the 1s event reaper).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableRate {
    pub table: String,
    pub inserts_per_sec: f64,
    pub reaps_per_sec: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Writer {
    /// Serialized-writer busy time as a percentage of wall-clock — THE capacity number.
    /// Derived from `spacetime_txn_cpu_time_sec_sum` (lock-wait excluded).
    pub occupancy_pct: f64,
    pub reducer_cpu_sec: f64,
    pub subscribe_cpu_sec: f64,
    /// Everything else `txn_cpu_time` is labelled with — `Update`, `Unsubscribe`, `Sql`,
    /// `Internal`. Occupancy is built from the TOTAL, so without this bucket the human report's
    /// "Xs reducer + Ys subscribe" decomposition does not add up to the headline percentage.
    pub other_cpu_sec: f64,
    pub total_cpu_sec: f64,
    pub txns_per_sec: f64,
    /// Mean time a reducer spent QUEUED before running. Queueing delay is the saturation tell:
    /// it grows sharply once occupancy passes ~50-60%.
    pub mean_queue_wait_ms: f64,
    pub egress_bytes_per_sec: f64,
    pub rows_scanned_per_sec: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCounters {
    pub heartbeats_sent: u64,
    pub heartbeats_per_sec: f64,
    pub peer_moves_observed: u64,
    pub peer_moves_per_sec: f64,
    pub swings_sent: u64,
    pub frames_received: u64,
    /// Times a player thread's drain budget expired with packets still queued. Non-zero means the
    /// HARNESS may be the bottleneck, not the server — read the latency numbers with suspicion.
    pub harness_backpressure_events: u64,
}

/// The harness measuring ITSELF. Every movement-latency sample is stamped when the observing
/// player's thread dequeues the packet, not when the kernel delivered it, so any delay in getting
/// that thread onto a CPU is added to the reported server latency. With 200 OS threads running SRP6
/// and per-packet decryption on the same box as the server, that is a real risk of reporting
/// client-side scheduling delay as server saturation.
///
/// This is the control: each player also records how LATE its own heartbeat fired against its own
/// schedule (`Instant::now() - next_hb` at send time), measured on the same clock and subject to
/// the same scheduler. It is a lower bound on the harness's contribution — if `p95` here is a
/// meaningful fraction of the movement `p95`, the rung is measuring the benchmark, not the server.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HarnessHealth {
    /// Heartbeat scheduling lateness, ms.
    pub wakeup_lag_ms: Latency,
    /// Player threads that entered the world and then lost their session before this window ended.
    pub players_dropped: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stage {
    pub players_target: usize,
    /// Players STILL in the world when the measured window closed — not a cumulative login tally.
    /// A session that dies mid-run must shrink the offered load in the report too, or a rung looks
    /// like it sustained 200 players when 60 of them had gone quiet.
    pub players_connected: usize,
    pub players_failed: usize,
    pub window_secs: f64,
    pub movement_latency_ms: Latency,
    pub client: ClientCounters,
    pub harness: HarnessHealth,
    pub writer: Writer,
    pub tx_per_sec_by_reducer: Vec<NamedRate>,
    pub event_tables: Vec<TableRate>,
    /// A node restart / module republish reset the node's counters inside this window, so every
    /// server-side number above is a difference across the reset. Reported, never silently
    /// swallowed; the stage's numbers are void.
    pub counter_reset: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    /// Free-form run label, e.g. `main-baseline` or `post-tier1`.
    pub label: String,
    pub generated_at_unix: u64,
    pub git_rev: String,
    pub target: Target,
    pub config: RunConfig,
    pub stages: Vec<Stage>,
    /// Metrics this run could NOT capture, and why. Kept in the artifact so a report can never
    /// silently look complete.
    pub parked: Vec<String>,
    pub login_errors: Vec<String>,
}

impl Report {
    pub fn new(label: &str, target: Target, config: RunConfig) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            label: label.to_string(),
            generated_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
            git_rev: git_rev(),
            target,
            config,
            stages: Vec::new(),
            parked: Vec::new(),
            login_errors: Vec::new(),
        }
    }
}

fn git_rev() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "# spacetime-core capacity benchmark — {}", self.label)?;
        writeln!(f)?;
        writeln!(f, "rev            {}", self.git_rev)?;
        writeln!(f, "generated      {} (unix)", self.generated_at_unix)?;
        writeln!(f, "logon / world  {} / {}", self.target.logon, self.target.world)?;
        writeln!(
            f,
            "metrics / db   {} / {}",
            self.target.metrics_url,
            if self.target.db.is_empty() { "<all databases on the node>" } else { &self.target.db }
        )?;
        writeln!(
            f,
            "ramp           {:?}, {}s warm-up + {}s measured window per rung, {}ms heartbeat, \
             {}% of players in combat, {}yd spread",
            self.config.stages,
            self.config.warmup_secs,
            self.config.hold_secs,
            self.config.heartbeat_ms,
            self.config.combat_pct,
            self.config.spread_yards
        )?;
        writeln!(f)?;

        writeln!(f, "## Headline")?;
        writeln!(f)?;
        writeln!(
            f,
            "| players | connected | writer occupancy % | queue wait ms | tx/s | move p50 | p95 | p99 | max | samples |"
        )?;
        writeln!(
            f,
            "|--------:|----------:|-------------------:|--------------:|-----:|---------:|----:|----:|----:|--------:|"
        )?;
        for s in &self.stages {
            let l = &s.movement_latency_ms;
            writeln!(
                f,
                "| {} | {} | {} | {:.1} | {:.0} | {} | {} | {} | {} | {} |",
                s.players_target,
                s.players_connected,
                // A stage whose counters were reset mid-window has no valid occupancy at all —
                // print the fact, not a number nobody can tell is wrong.
                if s.counter_reset {
                    "VOID (counter reset)".to_string()
                } else {
                    format!("{:.1}", s.writer.occupancy_pct)
                },
                s.writer.mean_queue_wait_ms,
                s.writer.txns_per_sec,
                l.cell(l.p50_ms),
                l.cell(l.p95_ms),
                l.cell(l.p99_ms),
                l.cell(l.max_ms),
                l.samples,
            )?;
        }
        writeln!(f)?;

        for s in &self.stages {
            writeln!(f, "## Stage: {} players", s.players_target)?;
            writeln!(f)?;
            writeln!(
                f,
                "connected {}/{} at window close ({} failed to log in, {} dropped mid-run), \
                 measured window {:.1}s",
                s.players_connected,
                s.players_target,
                s.players_failed,
                s.harness.players_dropped,
                s.window_secs
            )?;
            writeln!(
                f,
                "client:  {} heartbeats sent ({:.1}/s), {} peer moves observed ({:.1}/s), \
                 {} swings, {} frames in, {} harness-backpressure events",
                s.client.heartbeats_sent,
                s.client.heartbeats_per_sec,
                s.client.peer_moves_observed,
                s.client.peer_moves_per_sec,
                s.client.swings_sent,
                s.client.frames_received,
                s.client.harness_backpressure_events
            )?;
            let w = &s.harness.wakeup_lag_ms;
            writeln!(
                f,
                "harness: heartbeat wake-up lag p50/p95/p99 {}/{}/{}ms — the harness's OWN \
                 scheduling delay, which is included in the movement numbers above",
                w.cell(w.p50_ms),
                w.cell(w.p95_ms),
                w.cell(w.p99_ms)
            )?;
            if s.counter_reset {
                // Print NO server-side figure for this stage. Every one of them is a difference
                // taken across a counter reset, and a number a reader has to remember to discount
                // is a number that eventually gets quoted. The JSON keeps the raw values (with
                // `counter_reset: true`) for anyone debugging the run itself.
                writeln!(
                    f,
                    "writer:  VOID — the node's counters reset inside this window (restart / \
                     republish), so occupancy, tx/s, queue wait, egress and the per-reducer and \
                     per-table rates below are all differences across the reset. Re-run this rung."
                )?;
            } else {
                writeln!(
                    f,
                    "writer:  occupancy {:.1}% ({:.2}s reducer + {:.2}s subscribe + {:.2}s other \
                     = {:.2}s CPU over {:.1}s wall), {:.0} tx/s, mean queue wait {:.2}ms",
                    s.writer.occupancy_pct,
                    s.writer.reducer_cpu_sec,
                    s.writer.subscribe_cpu_sec,
                    s.writer.other_cpu_sec,
                    s.writer.total_cpu_sec,
                    s.window_secs,
                    s.writer.txns_per_sec,
                    s.writer.mean_queue_wait_ms
                )?;
                writeln!(
                    f,
                    "egress:  {:.0} KiB/s to clients, {:.0} rows scanned/s",
                    s.writer.egress_bytes_per_sec / 1024.0,
                    s.writer.rows_scanned_per_sec
                )?;
            }
            writeln!(f)?;
            writeln!(f, "tx/s by reducer (top {}):", s.tx_per_sec_by_reducer.len())?;
            for r in &s.tx_per_sec_by_reducer {
                writeln!(f, "  {:<34} {:>10.1}", r.name, r.per_sec)?;
            }
            writeln!(f)?;
            writeln!(f, "event tables (inserts/s -> reaps/s):")?;
            for t in &s.event_tables {
                writeln!(
                    f,
                    "  {:<34} {:>10.1} -> {:>10.1}",
                    t.table, t.inserts_per_sec, t.reaps_per_sec
                )?;
            }
            writeln!(f)?;
        }

        if !self.parked.is_empty() {
            writeln!(f, "## Parked / not captured")?;
            writeln!(f)?;
            for p in &self.parked {
                writeln!(f, "  - {p}")?;
            }
            writeln!(f)?;
        }
        if !self.login_errors.is_empty() {
            writeln!(f, "## Login / session errors ({})", self.login_errors.len())?;
            writeln!(f)?;
            for e in self.login_errors.iter().take(20) {
                writeln!(f, "  - {e}")?;
            }
            if self.login_errors.len() > 20 {
                writeln!(f, "  … {} more", self.login_errors.len() - 20)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank_and_handles_the_edges() {
        let v: Vec<u32> = (1..=100).collect(); // already sorted
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 95.0), 95);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&v, 100.0), 100);
        assert_eq!(percentile(&v, 0.0), 1, "p0 clamps to the first element");
        assert_eq!(percentile(&[], 95.0), 0, "empty window reports 0, never panics");
        assert_eq!(percentile(&[7], 50.0), 7);
        // Nearest-rank on a short odd set: ceil(0.95*3)=3 -> the 3rd element.
        assert_eq!(percentile(&[10, 20, 30], 95.0), 30);
        assert_eq!(percentile(&[10, 20, 30], 50.0), 20);
    }

    #[test]
    fn latency_summarizes_an_unsorted_sample_set() {
        let l = Latency::from_samples(vec![30, 10, 20, 40, 1000]);
        assert_eq!(l.samples, 5);
        assert_eq!(l.p50_ms, 30, "ceil(0.5*5)=3 -> the 3rd smallest");
        assert_eq!(l.p99_ms, 1000);
        assert_eq!(l.max_ms, 1000);
        assert!((l.mean_ms - 220.0).abs() < 1e-9);
        assert_eq!(Latency::from_samples(vec![]), Latency::default());
    }

    /// A rung that observed NOTHING must not render as a rung with excellent latency. `0ms p99`
    /// is the single most flattering number in the report, and it is exactly what an empty window
    /// produces.
    #[test]
    fn an_empty_latency_window_renders_as_a_dash_not_as_zero_ms() {
        let empty = Latency::default();
        assert_eq!(empty.cell(empty.p99_ms), "—");
        let real = Latency::from_samples(vec![0, 0, 0]);
        assert_eq!(real.cell(real.p99_ms), "0", "a MEASURED zero still prints as zero");

        let mut r = Report::new("empty", Target::default(), RunConfig::default());
        r.stages.push(Stage { players_target: 200, ..Default::default() });
        let text = r.to_string();
        assert!(text.contains("| 200 | 0 | 0.0 | 0.0 | 0 | — | — | — | — | 0 |"), "{text}");
    }

    /// A stage whose window straddled a node restart has no valid server-side number at all — the
    /// deltas are differences across the reset. It must say so where the headline is read.
    #[test]
    fn a_counter_reset_voids_the_headline_occupancy_cell() {
        let mut r = Report::new("reset", Target::default(), RunConfig::default());
        r.stages.push(Stage {
            players_target: 100,
            counter_reset: true,
            writer: Writer { occupancy_pct: -412.7, ..Default::default() },
            ..Default::default()
        });
        let text = r.to_string();
        assert!(text.contains("VOID (counter reset)"), "{text}");
        assert!(!text.contains("-412.7"), "the bogus number must not be printed at all:\n{text}");
    }

    #[test]
    fn plan_ramp_is_additive_and_rejects_a_shrinking_ramp() {
        assert_eq!(
            plan_ramp(&[50, 100, 150, 200]).unwrap(),
            vec![
                RampStep { target: 50, spawn: 50 },
                RampStep { target: 100, spawn: 50 },
                RampStep { target: 150, spawn: 50 },
                RampStep { target: 200, spawn: 50 },
            ]
        );
        // Uneven rungs still add up to the final target.
        let plan = plan_ramp(&[10, 25, 200]).unwrap();
        assert_eq!(plan.iter().map(|s| s.spawn).sum::<usize>(), 200);
        assert!(plan_ramp(&[100, 50]).is_err());
        assert!(plan_ramp(&[50, 50]).is_err());
        assert!(plan_ramp(&[]).is_err());
        assert!(plan_ramp(&[0]).is_err());
    }

    #[test]
    fn char_names_are_letters_only_and_unique_across_the_whole_ramp() {
        assert_eq!(char_name_for("Bench", 0), "Benchaaa");
        assert_eq!(char_name_for("Bench", 1), "Benchaab");
        assert_eq!(char_name_for("Bench", 26), "Benchaba");
        let names: std::collections::HashSet<String> =
            (0..200).map(|i| char_name_for("Bench", i)).collect();
        assert_eq!(names.len(), 200, "200 distinct names");
        assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_alphabetic())));
    }

    #[test]
    fn report_round_trips_through_json_and_renders_a_headline_row() {
        let mut r = Report::new(
            "unit-test",
            Target {
                logon: "127.0.0.1:3724".into(),
                world: "127.0.0.1:8085".into(),
                metrics_url: "http://127.0.0.1:3000/v1/metrics".into(),
                db: String::new(),
            },
            RunConfig { stages: vec![50], hold_secs: 60, ..Default::default() },
        );
        r.stages.push(Stage {
            players_target: 50,
            players_connected: 50,
            window_secs: 60.0,
            movement_latency_ms: Latency::from_samples(vec![5, 9, 40]),
            writer: Writer { occupancy_pct: 31.5, txns_per_sec: 210.0, ..Default::default() },
            tx_per_sec_by_reducer: vec![NamedRate {
                name: "movement_update".into(),
                per_sec: 100.0,
            }],
            event_tables: vec![TableRate {
                table: "game_movement_event".into(),
                inserts_per_sec: 2000.0,
                reaps_per_sec: 1990.0,
            }],
            ..Default::default()
        });
        r.parked.push("nothing".into());

        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.stages.len(), 1);
        assert_eq!(back.stages[0].movement_latency_ms.p50_ms, 9);
        assert_eq!(back.stages[0].event_tables[0].reaps_per_sec, 1990.0);

        let text = r.to_string();
        assert!(text.contains("| 50 | 50 | 31.5 |"), "headline row missing:\n{text}");
        assert!(text.contains("movement_update"));
        assert!(text.contains("## Parked / not captured"));
    }
}

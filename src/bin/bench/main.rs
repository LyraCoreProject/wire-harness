//! `bench` — the 50→200 synthetic-player capacity benchmark (work-item #18).
//!
//! One command drives a ramp of synthetic players through login, continuous movement in a shared
//! zone and light combat, then writes a machine-readable (JSON) + human-readable (text) report.
//! It is the instrument that converts every estimate in `docs/capacity-analysis.md` into a
//! measurement, and it is re-runnable per shard (`--logon` / `--world` / `--metrics` / `--db`).
//!
//! Each synthetic player is a REAL 1.12.1 wire session — the same `wire_client::WireClient` the
//! functional tests use — so it exercises the genuine path: SRP6 logon, encrypted world
//! handshake, `CMSG_PLAYER_LOGIN`, `MSG_MOVE_HEARTBEAT`, `CMSG_ATTACKSWING`. Nothing is faked at
//! the protocol layer, and nothing was added inside `module/` or `gateway/` to measure it.
//!
//! ## How movement latency is measured without a server-side clock
//!
//! The module relays a mover's `MovementInfo` to nearby players **verbatim**
//! (`gateway/src/codec/movement.rs::build_movement_relay`), including its `timestamp` field. Every
//! synthetic player therefore stamps each heartbeat with "milliseconds since this benchmark
//! process started", and any player that OBSERVES a peer's relayed heartbeat subtracts that stamp
//! from its own reading of the same shared epoch. Both ends live in this one process, so the
//! clocks are identical by construction and no clock-sync machinery is needed.
//!
//! What that yields is the **one-way, client-observed movement relay latency**: sender's
//! `send()` → gateway → `movement_update` on the serialized writer → `game_movement_event`
//! subscription → observer's socket. That is the latency a player actually perceives as "peers
//! are lagging", and it is the number `capacity-analysis.md` §3.3 predicts will degrade first.
//!
//! Everything server-side (writer occupancy, tx/s by reducer, event insert/reap rates) is scraped
//! from SpacetimeDB's own Prometheus endpoint — see `metrics.rs`.
//!
//! Run `bench --help` for the full argument list; `tools/wire-client/test-bench.sh` is the
//! runbook wrapper that provisions accounts first.

mod metrics;
mod report;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use report::{
    char_name_for, plan_ramp, ClientCounters, Latency, NamedRate, Report, RunConfig, Stage,
    TableRate, Target, Writer,
};
use wire_client::WireClient;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage as Smsg;
use wow_world_messages::vanilla::{
    Class, MovementInfo, MovementInfo_MovementFlags, Vector3d, CMSG_ATTACKSTOP, CMSG_ATTACKSWING,
    MSG_MOVE_HEARTBEAT_Client,
};
use wow_world_messages::Guid;

/// Vanilla `HIGHGUID_UNIT` — the top 16 bits of a creature guid. Used to pick a combat target out
/// of whatever the login/AOI burst spawned nearby.
const HIGHGUID_UNIT: u64 = 0xF130;
/// How often a combat-enabled player flips between swinging and disengaging.
const COMBAT_FLIP_SECS: u64 = 8;
/// Socket read timeout inside a player's drain loop. Doubles as the idle poll interval.
const DRAIN_POLL_MS: u64 = 20;
/// Latency samples a player buffers locally before taking the shared lock (keeps 200 threads from
/// contending on every observed heartbeat).
const LATENCY_FLUSH_BATCH: usize = 64;
/// `MOVEMENT_FLAG_FORWARD` — what a real 1.12 client has set while running, and therefore what the
/// module stamps into `WorldEntity::movement_flags` for a peer's CREATE block. A crowd heartbeating
/// with EMPTY flags is a crowd of standing-still players as far as every downstream consumer is
/// concerned, which is not the workload this benchmark claims to measure.
const MOVE_FLAG_FORWARD: MovementInfo_MovementFlags =
    MovementInfo_MovementFlags::new(0x1, None, None, None, None);

const USAGE: &str = "\
bench — 50→200 synthetic-player capacity benchmark

USAGE:
  bench [OPTIONS]

TARGET (re-runnable per shard):
  --logon HOST[:PORT]     logon tier of the gateway under test     [127.0.0.1:3724]
  --world HOST:PORT       world tier override                      [realm-list answer]
  --metrics URL           SpacetimeDB node metrics endpoint        [http://127.0.0.1:3000/v1/metrics]
  --db PREFIX             db= label prefix selecting ONE database  [required if the node hosts >1]
  --witness-db PREFIX     a SECOND database sampled over the same measured windows and reported
                          alongside the first. Nothing in this harness drives it — it is how one
                          run states both halves of \"instance load scales on the pool while the
                          open world stays flat\" (#21) from the same window.  [none]

RAMP:
  --stages 50,100,150,200 additive player rungs                    [50,100,150,200]
  --stages 0              OBSERVE-ONLY: connect nobody, hold one window, report both writers.
                          For load this harness does not generate (e.g. N scripted dungeon runs).
  --warmup SECS           settle time after each rung before measuring   [15]
  --hold SECS             measured window per rung                       [60]
  --login-stagger-ms MS   delay between consecutive logins              [40]

WORKLOAD:
  --center X,Y,Z          shared-zone center the crowd walks in    [-8920,-180,82]
  --spread YARDS          crowd radius around the center           [40]
  --heartbeat-ms MS       per-player movement cadence              [500]
  --combat-pct N          % of players that engage a nearby creature [25]

IDENTITY:
  --account-prefix P      accounts are P0000..                     [BENCH]
  --password PASS         shared password for those accounts       [benchpass]
  --char-prefix P         characters are Paaa, Paab, …             [Bench]

OUTPUT:
  --tables-filter SUB     only report tables whose name contains SUB    [all non-system tables]
  --label NAME            run label recorded in the report         [REQUIRED for a real run]
  --json PATH             write the machine-readable report
  --text PATH             write the human-readable report (also printed to stdout)

PREFLIGHT:
  --dry-run 1             scrape the metrics endpoint once, print what the report would be able
                          to measure, and EXIT without connecting a single player. Safe to run
                          against a node that is in use.

A real run CONNECTS UP TO 200 PLAYERS and saturates the target node's writer by design, so it
demands an explicit --label. With no arguments this binary refuses to do anything.
";

// ---------------------------------------------------------------------------------------------
//  Arguments
// ---------------------------------------------------------------------------------------------

/// ponytail: `--key value` pairs into a map, not a CLI framework. Ceiling: no short flags, no
/// `--key=value`, no subcommands. Upgrade path if this ever grows: `clap`.
struct Args(HashMap<String, String>);

impl Args {
    fn parse() -> Result<Self> {
        let mut m = HashMap::new();
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            if k == "--help" || k == "-h" {
                print!("{USAGE}");
                std::process::exit(0);
            }
            let Some(key) = k.strip_prefix("--") else {
                bail!("unexpected positional argument {k:?}\n\n{USAGE}");
            };
            let v = it.next().with_context(|| format!("--{key} needs a value\n\n{USAGE}"))?;
            m.insert(key.to_string(), v);
        }
        Ok(Self(m))
    }

    fn str(&self, k: &str, default: &str) -> String {
        self.0.get(k).cloned().unwrap_or_else(|| default.to_string())
    }

    fn opt(&self, k: &str) -> Option<String> {
        self.0.get(k).cloned()
    }

    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T> {
        match self.0.get(k) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| anyhow::anyhow!("--{k}: cannot parse {v:?}")),
        }
    }

    fn list(&self, k: &str, default: &[usize]) -> Result<Vec<usize>> {
        match self.0.get(k) {
            None => Ok(default.to_vec()),
            Some(v) => v
                .split(',')
                .map(|s| s.trim().parse::<usize>().map_err(|_| anyhow::anyhow!("--{k}: {s:?}")))
                .collect(),
        }
    }

    fn vec3(&self, k: &str, default: [f32; 3]) -> Result<[f32; 3]> {
        let Some(v) = self.0.get(k) else { return Ok(default) };
        let parts: Vec<f32> =
            v.split(',').filter_map(|s| s.trim().parse::<f32>().ok()).collect();
        if parts.len() != 3 {
            bail!("--{k}: expected X,Y,Z (got {v:?})");
        }
        Ok([parts[0], parts[1], parts[2]])
    }
}

// ---------------------------------------------------------------------------------------------
//  Shared state
// ---------------------------------------------------------------------------------------------

struct Cfg {
    logon: String,
    world: Option<String>,
    account_prefix: String,
    password: String,
    char_prefix: String,
    center: [f32; 3],
    spread: f32,
    heartbeat_ms: u64,
    combat_pct: usize,
}

#[derive(Default)]
struct Counters {
    heartbeats: AtomicU64,
    peer_moves: AtomicU64,
    swings: AtomicU64,
    frames: AtomicU64,
    backpressure: AtomicU64,
}

impl Counters {
    /// Read every counter at once (window boundaries take a `snapshot` before and after).
    fn snapshot(&self) -> [u64; 5] {
        [
            self.heartbeats.load(Ordering::Relaxed),
            self.peer_moves.load(Ordering::Relaxed),
            self.swings.load(Ordering::Relaxed),
            self.frames.load(Ordering::Relaxed),
            self.backpressure.load(Ordering::Relaxed),
        ]
    }
}

struct Shared {
    /// The one clock every synthetic player stamps heartbeats against.
    epoch: Instant,
    stop: AtomicBool,
    /// Cumulative: players that ever reached the world. Used only to decide when a rung has
    /// finished logging in — NEVER as the offered load (see `live`).
    connected: AtomicUsize,
    /// Players currently in the world. Decremented when a session dies, so a rung that quietly
    /// loses half its players reports the load it actually offered.
    live: AtomicUsize,
    dropped: AtomicUsize,
    failed: AtomicUsize,
    counters: Counters,
    latencies: Mutex<Vec<u32>>,
    /// The harness's own scheduling delay — see `report::HarnessHealth`.
    wakeup_lags: Mutex<Vec<u32>>,
    errors: Mutex<Vec<String>>,
}

impl Shared {
    fn now_ms(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }
}

// ---------------------------------------------------------------------------------------------
//  One synthetic player
// ---------------------------------------------------------------------------------------------

fn run_player(idx: usize, cfg: Arc<Cfg>, sh: Arc<Shared>) {
    let account = format!("{}{:04}", cfg.account_prefix, idx);
    let char_name = char_name_for(&cfg.char_prefix, idx);
    let mut c = match connect(&cfg, &account, &char_name) {
        Ok(c) => c,
        Err(e) => {
            sh.failed.fetch_add(1, Ordering::Relaxed);
            let mut errs = sh.errors.lock().expect("errors lock");
            if errs.len() < 200 {
                errs.push(format!("{account}/{char_name}: {e:#}"));
            }
            return;
        }
    };
    sh.connected.fetch_add(1, Ordering::Relaxed);
    sh.live.fetch_add(1, Ordering::Relaxed);

    // Spread the crowd over a disc of radius `spread` using the golden angle, so successive
    // players never line up and every player sits inside everyone else's AOI box.
    let ang = idx as f32 * 2.399_963_2;
    let radius = cfg.spread * (((idx % 23) as f32 + 0.5) / 23.0).sqrt();
    let (bx, by) = (cfg.center[0] + radius * ang.cos(), cfg.center[1] + radius * ang.sin());
    let combat = idx % 100 < cfg.combat_pct;

    let hb_interval = Duration::from_millis(cfg.heartbeat_ms);
    let mut next_hb = Instant::now();
    let mut next_combat_flip = Instant::now() + Duration::from_secs(COMBAT_FLIP_SECS);
    let mut engaged: Option<u64> = None;
    let mut hb_count: u32 = 0;
    let mut local_lat: Vec<u32> = Vec::with_capacity(LATENCY_FLUSH_BATCH);
    let mut local_lag: Vec<u32> = Vec::with_capacity(LATENCY_FLUSH_BATCH);
    let mut last_flush = Instant::now();

    while !sh.stop.load(Ordering::Relaxed) {
        let now = Instant::now();

        if now >= next_hb {
            // The harness measuring itself: how late did this thread wake up for its own
            // heartbeat? Same clock and same scheduler as the movement-latency samples, so it is a
            // lower bound on how much of the reported latency is the benchmark process.
            local_lag.push(now.saturating_duration_since(next_hb).as_millis() as u32);
            // A 3yd circular stroll around the player's base point. Heading follows the circle's
            // TANGENT and the FORWARD movement flag is set, because that is what a real running
            // client sends — and the gateway classifies a heartbeat by (flags, heading) against the
            // last one it forwarded (`gateway/src/world/coalesce.rs`). A constant heading would
            // make this stream perfectly coalescible in a way real crowd movement is not
            // (docs/perf-fix-catalog.md 1.8: heading drift while turning defeats coalescing), which
            // would let a future coalescing-window change look better here than it is in the world.
            let t = hb_count as f32 * 0.35;
            let info = MovementInfo {
                flags: MOVE_FLAG_FORWARD,
                timestamp: sh.now_ms(),
                position: Vector3d {
                    x: bx + 3.0 * t.cos(),
                    y: by + 3.0 * t.sin(),
                    z: cfg.center[2],
                },
                // Tangent to the stroll, wrapped into 0..2π like a client's facing.
                orientation: (t + std::f32::consts::FRAC_PI_2).rem_euclid(std::f32::consts::TAU),
                fall_time: 0.0,
            };
            if c.send(&MSG_MOVE_HEARTBEAT_Client { info }).is_err() {
                break; // socket gone — the session is over
            }
            hb_count = hb_count.wrapping_add(1);
            sh.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
            next_hb += hb_interval;
            // A stalled thread must not try to catch up with a burst of back-dated heartbeats.
            if next_hb < now {
                next_hb = now + hb_interval;
            }
        }

        if combat && now >= next_combat_flip {
            next_combat_flip = now + Duration::from_secs(COMBAT_FLIP_SECS);
            match engaged.take() {
                Some(_) => {
                    let _ = c.send(&CMSG_ATTACKSTOP {});
                }
                None => {
                    // Whatever creature the login/AOI burst spawned nearby; best effort.
                    if let Some(t) =
                        c.seen_guids.iter().copied().find(|g| (*g >> 48) == HIGHGUID_UNIT)
                    {
                        if c.set_selection(t).is_ok()
                            && c.send(&CMSG_ATTACKSWING { guid: Guid::new(t) }).is_ok()
                        {
                            engaged = Some(t);
                            sh.counters.swings.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }

        // ---- drain: read every queued frame, timing the peer heartbeats among them ----
        let budget = next_hb.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
        let deadline = Instant::now() + budget;
        let mut last_was_ok = false;
        loop {
            if Instant::now() >= deadline {
                if last_was_ok {
                    // Packets were still arriving when the budget ran out: the HARNESS may be the
                    // bottleneck. Surfaced in the report so the latency numbers can be distrusted.
                    sh.counters.backpressure.fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
            match c.recv() {
                Ok(m) => {
                    last_was_ok = true;
                    sh.counters.frames.fetch_add(1, Ordering::Relaxed);
                    if let Smsg::MSG_MOVE_HEARTBEAT(hb) = m {
                        sh.counters.peer_moves.fetch_add(1, Ordering::Relaxed);
                        let now_ms = sh.now_ms();
                        // The relay carries our stamp back verbatim; a clock that ran backwards
                        // (impossible here) or a wrapped u32 would underflow, so guard it.
                        if let Some(d) = now_ms.checked_sub(hb.info.timestamp) {
                            local_lat.push(d);
                        }
                    }
                }
                // Both a quiet socket (read timeout) and an undecodable-frame give-up are
                // NON-fatal: `recv` consumed the bytes and the keystream stays in lockstep.
                Err(_) => break,
            }
        }

        // Flush on EITHER buffer filling or the 1s tick — a player that observes no peers still
        // produces wake-up-lag samples, and those must reach the window they were measured in.
        let pending = local_lat.len().max(local_lag.len());
        if pending >= LATENCY_FLUSH_BATCH
            || (pending > 0 && last_flush.elapsed() >= Duration::from_secs(1))
        {
            sh.latencies.lock().expect("latency lock").extend(local_lat.drain(..));
            sh.wakeup_lags.lock().expect("lag lock").extend(local_lag.drain(..));
            last_flush = Instant::now();
        }
    }
    // The stop flag is the only clean way out; anything else `break`s, and that is a lost session.
    let left_early = !sh.stop.load(Ordering::Relaxed);
    if !local_lat.is_empty() {
        sh.latencies.lock().expect("latency lock").extend(local_lat.drain(..));
    }
    if !local_lag.is_empty() {
        sh.wakeup_lags.lock().expect("lag lock").extend(local_lag.drain(..));
    }
    sh.live.fetch_sub(1, Ordering::Relaxed);
    if left_early {
        sh.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// logon → world handshake → create-or-find the character → enter the world.
/// Open-coded (rather than `WireClient::login_as`) only so `--logon` / `--world` can point at an
/// arbitrary shard's gateway.
fn connect(cfg: &Cfg, account: &str, char_name: &str) -> Result<WireClient> {
    let (k, realm_world) = wire_client::logon_at(&cfg.logon, account, &cfg.password)?;
    let world = cfg.world.clone().unwrap_or(realm_world);
    let mut c = WireClient::connect_world(&world, account, k)?;
    let guid = c.create_or_find_char(char_name, Class::Warrior)?;
    c.player_login(guid)?;
    c.set_recv_timeout(Duration::from_millis(DRAIN_POLL_MS))?;
    Ok(c)
}

// ---------------------------------------------------------------------------------------------
//  Server-side metric extraction
// ---------------------------------------------------------------------------------------------

/// The headline capacity numbers, reduced out of a before/after scrape delta over `secs` seconds.
fn writer_stats(d: &metrics::Snapshot, secs: f64, dbf: &str) -> Writer {
    let reducer_cpu = d.sum("spacetime_txn_cpu_time_sec_sum", &[dbf, r#"txn_type="Reducer""#]);
    let subscribe_cpu = d.sum("spacetime_txn_cpu_time_sec_sum", &[dbf, r#"txn_type="Subscribe""#]);
    let total_cpu = d.sum("spacetime_txn_cpu_time_sec_sum", &[dbf]);
    let wait_sum = d.sum("spacetime_reducer_wait_time_sec_sum", &[dbf]);
    let wait_count = d.sum("spacetime_reducer_wait_time_sec_count", &[dbf]);
    Writer {
        // Occupancy = the fraction of wall-clock the writer was busy. Lock-wait is excluded from
        // txn_cpu_time by SpacetimeDB, so for ONE database this is the serialized writer's busy
        // fraction. It is the TOTAL across every txn_type, not just Reducer — `other_cpu_sec`
        // carries the remainder (Update/Unsubscribe/Sql/Internal) so the decomposition reconciles.
        // Two things can push it past 100%, and both are caught rather than reported: a `--db`
        // selection spanning several databases (refused at preflight) and a counter reset inside
        // the window (flagged on the stage).
        occupancy_pct: if secs > 0.0 { total_cpu / secs * 100.0 } else { 0.0 },
        reducer_cpu_sec: reducer_cpu,
        subscribe_cpu_sec: subscribe_cpu,
        other_cpu_sec: total_cpu - reducer_cpu - subscribe_cpu,
        total_cpu_sec: total_cpu,
        txns_per_sec: rate(d.sum("spacetime_num_txns_total", &[dbf]), secs),
        mean_queue_wait_ms: if wait_count > 0.0 { wait_sum / wait_count * 1000.0 } else { 0.0 },
        egress_bytes_per_sec: rate(d.sum("spacetime_num_bytes_sent_to_clients_total", &[dbf]), secs),
        rows_scanned_per_sec: rate(d.sum("spacetime_num_rows_scanned_total", &[dbf]), secs),
    }
}

fn rate(delta: f64, secs: f64) -> f64 {
    if secs > 0.0 {
        delta / secs
    } else {
        0.0
    }
}

fn tx_by_reducer(d: &metrics::Snapshot, secs: f64, dbf: &str, top: usize) -> Vec<NamedRate> {
    d.group_by("spacetime_num_txns_total", "reducer", &[dbf, r#"txn_type="Reducer""#])
        .into_iter()
        .take(top)
        .map(|(name, n)| NamedRate { name, per_sec: rate(n, secs) })
        .collect()
}

/// Per-table insert / delete rates. For the `game_*_event` delivery buffers the delete rate IS the
/// reap rate — the 1s event reaper is their only remover.
fn table_rates(
    d: &metrics::Snapshot,
    secs: f64,
    dbf: &str,
    filter: &str,
    top: usize,
) -> Vec<TableRate> {
    let ins: HashMap<String, f64> =
        d.group_by("spacetime_num_rows_inserted_total", "table_name", &[dbf]).into_iter().collect();
    let del: HashMap<String, f64> =
        d.group_by("spacetime_num_rows_deleted_total", "table_name", &[dbf]).into_iter().collect();
    // A table can appear in either half (insert-only, or delete-only when the reaper drains a
    // backlog), so the row set is the UNION of both keyspaces.
    let mut names: Vec<&String> = ins.keys().chain(del.keys()).collect();
    names.sort_unstable();
    names.dedup();
    let mut out: Vec<TableRate> = names
        .into_iter()
        .filter(|t| t.contains(filter) && !t.starts_with("st_"))
        .map(|table| TableRate {
            inserts_per_sec: rate(ins.get(table).copied().unwrap_or(0.0), secs),
            reaps_per_sec: rate(del.get(table).copied().unwrap_or(0.0), secs),
            table: table.clone(),
        })
        .filter(|t| t.inserts_per_sec > 0.0 || t.reaps_per_sec > 0.0)
        .collect();
    out.sort_by(|a, b| {
        (b.inserts_per_sec + b.reaps_per_sec)
            .total_cmp(&(a.inserts_per_sec + a.reaps_per_sec))
            .then_with(|| a.table.cmp(&b.table))
    });
    out.truncate(top);
    out
}

// ---------------------------------------------------------------------------------------------
//  Preflight gates
// ---------------------------------------------------------------------------------------------

/// Refuse to produce a report whose server-side half is silently all-zero.
///
/// Two ways that happens, both of which render as a perfectly plausible "the writer is idle":
///
/// 1. **`--db` matches nothing.** The filter is a substring match on the `db="` label, so one
///    wrong hex character in a shard identity makes every `sum` return `0.0` — occupancy `0.0%`,
///    `0` tx/s, no event tables — while the client-side latency numbers stay real and believable.
///    Reading that against the Phase C gate says "a tuned writer is nowhere near saturating", which
///    is the single most expensive wrong conclusion this instrument can produce.
/// 2. **`--db` is empty on a node hosting more than one measurable database.** `sum` then adds the
///    databases together, so two shards at 60% report one writer at 120% — and, worse, two shards
///    at 35% report 70%, which looks like a plausible single-writer reading. That is exactly the
///    topology issue #12 creates, so the aggregate default is only safe while the node hosts one.
fn validate_db_selection(s: &metrics::Snapshot, db: &str, dbf: &str) -> Result<()> {
    let measurable = s.databases_with(metrics::OCCUPANCY_FAMILY);
    if !db.is_empty() && !s.has_any(metrics::OCCUPANCY_FAMILY, &[dbf]) {
        bail!(
            "--db {db:?} matches no database on this node — every server-side number would be a \
             silent 0.\nmeasurable databases: {measurable:?}\n(--db takes a PREFIX of the database \
             identity; `bench --dry-run 1` lists them)"
        );
    }
    if db.is_empty() && measurable.len() > 1 {
        bail!(
            "this node hosts {} measurable databases, and an empty --db would SUM them into one \
             bogus occupancy figure.\npass --db <prefix> to select the shard under test: {:?}",
            measurable.len(),
            measurable
        );
    }
    if measurable.is_empty() {
        bail!(
            "the metrics endpoint exposes no `{}` series at all — the node is up but has no \
             database to measure (nothing published?)",
            metrics::OCCUPANCY_FAMILY
        );
    }
    Ok(())
}

/// The `--witness-db`'s own preflight (#21), and its failure mode is nastier than `--db`'s.
///
/// A witness that matches nothing reports **0.0% occupancy** — which reads exactly like "the open
/// world stayed perfectly flat while the instance pool absorbed the load", i.e. the conclusion the
/// witness exists to *test*. An instrument that can confirm its own hypothesis by typo is not an
/// instrument. A witness equal to the primary `--db` is refused for the same reason: it reports one
/// writer twice and the two columns agree by construction.
fn validate_witness_selection(s: &metrics::Snapshot, db: &str, witness: &str, wdbf: &str) -> Result<()> {
    if witness.is_empty() {
        return Ok(()); // no witness: the pre-#21 single-database report
    }
    if !s.has_any(metrics::OCCUPANCY_FAMILY, &[wdbf]) {
        bail!(
            "--witness-db {witness:?} matches no database on this node — it would report 0.0% \
             occupancy, which is indistinguishable from the flat second writer this comparison is \
             supposed to demonstrate.\nmeasurable databases: {:?}",
            s.databases_with(metrics::OCCUPANCY_FAMILY)
        );
    }
    if !db.is_empty() && (witness.starts_with(db) || db.starts_with(witness)) {
        bail!(
            "--witness-db {witness:?} and --db {db:?} select the same database — the report would \
             show one writer in two columns, which agree by construction and prove nothing"
        );
    }
    Ok(())
}

/// Park every required metric family the current selection cannot see. `parked[]` is the report's
/// honesty mechanism, and a hard-coded entry cannot detect a metric that actually went missing —
/// this makes the list a function of the live scrape, so a field reading `0` because its family was
/// absent is never mistaken for a measured zero.
fn park_missing_families(s: &metrics::Snapshot, dbf: &str) -> Vec<String> {
    metrics::REQUIRED_FAMILIES
        .iter()
        .filter(|(_, name)| !s.has_any(name, &[dbf]))
        .map(|(what, name)| {
            format!(
                "{what}: the node exposes no `{name}` series under this --db selection, so that \
                 field is 0 BY ABSENCE, not by measurement."
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
//  Driver
// ---------------------------------------------------------------------------------------------

fn main() -> Result<()> {
    let args = Args::parse()?;

    let stages = args.list("stages", &[50, 100, 150, 200])?;
    let plan = plan_ramp(&stages).map_err(anyhow::Error::msg)?;
    let warmup = args.num::<u64>("warmup", 15)?;
    let hold = args.num::<u64>("hold", 60)?;
    let stagger = args.num::<u64>("login-stagger-ms", 40)?;
    let metrics_url = args.str("metrics", "http://127.0.0.1:3000/v1/metrics");
    let db = args.str("db", "");
    let dbf = metrics::db_filter(&db);
    // #21: the second database, sampled but never driven. Empty = absent; the whole feature is one
    // extra `writer_stats` call per window, so an unused witness costs a run nothing.
    let witness_db = args.str("witness-db", "");
    let witness_dbf = metrics::db_filter(&witness_db);
    let table_filter = args.str("tables-filter", "");
    // SAFETY GATE. Every default in this binary points at the LOCAL DEVELOPMENT STACK, so a bare
    // `cargo run -p wire-client --bin bench` used to log 200 synthetic players into whatever node
    // is running and saturate its writer for eight minutes. The run has to be deliberate: an
    // explicit --label is the smallest thing that can't be typed by accident, and it is the same
    // argument that names the recorded artifact, so a real run always passes it anyway.
    let label = args.opt("label").filter(|l| !l.is_empty());

    let cfg = Arc::new(Cfg {
        logon: args.str("logon", wire_client::DEFAULT_LOGON_ADDR),
        world: args.opt("world"),
        account_prefix: args.str("account-prefix", "BENCH"),
        password: args.str("password", "benchpass"),
        char_prefix: args.str("char-prefix", "Bench"),
        center: args.vec3("center", [-8920.0, -180.0, 82.0])?,
        spread: args.num::<f32>("spread", 40.0)?,
        heartbeat_ms: args.num::<u64>("heartbeat-ms", 500)?,
        combat_pct: args.num::<usize>("combat-pct", 25)?,
    });

    // Fail fast on an unreachable metrics endpoint — a whole ramp that produces no server-side
    // numbers is worse than no run at all.
    let preflight = metrics::scrape(&metrics_url)
        .with_context(|| format!("metrics preflight against {metrics_url}"))?;
    if args.opt("dry-run").is_some() {
        dry_run(&preflight, &metrics_url, &db, &dbf, &table_filter)?;
        // Validate AFTER printing, so the operator sees the node's actual identities alongside the
        // complaint — but still fail, so the runner script's `|| exit 1` catches a bad selection
        // before it commits the machine to an eight-minute ramp that measures nothing.
        validate_db_selection(&preflight, &db, &dbf)?;
        return validate_witness_selection(&preflight, &db, &witness_db, &witness_dbf);
    }
    let Some(label) = label else {
        bail!(
            "refusing to run: --label is required, because THIS CONNECTS {} SYNTHETIC PLAYERS to \
             {} and saturates that node's writer by design.\n\n  preflight only (connects nobody, \
             writes nothing):  bench --dry-run 1\n  a real, deliberate run:                       \
             bench --label main-baseline\n\nSee docs/capacity-benchmark.md §3 — the run needs \
             exclusive access to the stack.",
            plan.last().map(|s| s.target).unwrap_or(0),
            cfg.logon,
        );
    };
    validate_db_selection(&preflight, &db, &dbf)?;
    validate_witness_selection(&preflight, &db, &witness_db, &witness_dbf)?;

    let sh = Arc::new(Shared {
        epoch: Instant::now(),
        stop: AtomicBool::new(false),
        connected: AtomicUsize::new(0),
        live: AtomicUsize::new(0),
        dropped: AtomicUsize::new(0),
        failed: AtomicUsize::new(0),
        counters: Counters::default(),
        latencies: Mutex::new(Vec::new()),
        wakeup_lags: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
    });

    let run_cfg = RunConfig {
        stages: stages.clone(),
        warmup_secs: warmup,
        hold_secs: hold,
        heartbeat_ms: cfg.heartbeat_ms,
        combat_pct: cfg.combat_pct,
        spread_yards: cfg.spread,
        center: cfg.center,
        login_stagger_ms: stagger,
    };
    let mut rep = Report::new(
        &label,
        Target {
            logon: cfg.logon.clone(),
            world: cfg.world.clone().unwrap_or_else(|| "<realm-list>".into()),
            metrics_url: metrics_url.clone(),
            db: db.clone(),
            witness_db: witness_db.clone(),
        },
        run_cfg,
    );
    rep.parked.push(
        "Per-reducer WASM execution time is reported by the node under a placeholder db label \
         (`reducer_wasm_time_usec`), so it is not attributed per database here; the per-reducer \
         breakdown in this report is TRANSACTION COUNT, and occupancy is whole-database CPU."
            .into(),
    );
    rep.parked.push(
        "Movement latency is sampled when the OBSERVING player's thread dequeues the relayed \
         heartbeat, so it includes this harness's own scheduling delay. `harness.wakeup_lag_ms` is \
         the control: compare it against the movement percentiles before attributing a rung's \
         latency to the server."
            .into(),
    );
    // Derived, not hard-coded: park whatever the live node does not actually expose here.
    rep.parked.extend(park_missing_families(&preflight, &dbf));

    let mut handles = Vec::new();
    let mut next_idx = 0usize;

    for step in &plan {
        eprintln!(
            "[bench] rung {} players — logging in {} more ({}ms apart)…",
            step.target, step.spawn, stagger
        );
        for _ in 0..step.spawn {
            let (cfg, sh) = (cfg.clone(), sh.clone());
            let idx = next_idx;
            next_idx += 1;
            handles.push(thread::spawn(move || run_player(idx, cfg, sh)));
            thread::sleep(Duration::from_millis(stagger));
        }

        // Wait for every player of this rung to have either entered the world or failed.
        let settle = Instant::now() + Duration::from_secs(120);
        while Instant::now() < settle
            && sh.connected.load(Ordering::Relaxed) + sh.failed.load(Ordering::Relaxed)
                < step.target
        {
            thread::sleep(Duration::from_millis(200));
        }
        let connected = sh.connected.load(Ordering::Relaxed);
        let failed = sh.failed.load(Ordering::Relaxed);
        eprintln!("[bench] rung {}: {connected} in world, {failed} failed", step.target);

        eprintln!("[bench] warm-up {warmup}s…");
        thread::sleep(Duration::from_secs(warmup));

        // ---- measured window ----
        sh.latencies.lock().expect("latency lock").clear();
        sh.wakeup_lags.lock().expect("lag lock").clear();
        let dropped0 = sh.dropped.load(Ordering::Relaxed);
        let c0 = sh.counters.snapshot();
        let m0 = metrics::scrape(&metrics_url)?;
        let t0 = Instant::now();
        eprintln!("[bench] measuring {hold}s at {} players…", step.target);
        thread::sleep(Duration::from_secs(hold));
        let secs = t0.elapsed().as_secs_f64();
        let m1 = metrics::scrape(&metrics_url)?;
        let c1 = sh.counters.snapshot();
        let lat: Vec<u32> = std::mem::take(&mut *sh.latencies.lock().expect("latency lock"));
        let lag: Vec<u32> = std::mem::take(&mut *sh.wakeup_lags.lock().expect("lag lock"));
        // Read the LIVE population at window close, not the cumulative login tally: a rung that
        // quietly lost sessions must not report the load it briefly had.
        let live = sh.live.load(Ordering::Relaxed);
        let dropped = sh.dropped.load(Ordering::Relaxed).saturating_sub(dropped0);

        let d = m0.delta(&m1);
        // Every metric read here is a monotonic counter, so a negative delta means the node's
        // counters were reset inside the window (restart / republish) and the stage is void.
        // The witness's series are part of the window too: a counter reset that only touched the
        // witness would otherwise void nothing and quietly hand back a bogus second column.
        let mut reset_filters: Vec<&str> = vec![&dbf];
        if !witness_db.is_empty() {
            reset_filters.push(&witness_dbf);
        }
        let counter_reset = d.first_negative(&reset_filters).map(|(k, v)| {
            eprintln!("[bench] rung {}: COUNTER RESET during the window ({k} went {v:+.3}) — this stage's server-side numbers are void", step.target);
            k.clone()
        });
        let dc: Vec<u64> = c0.iter().zip(c1.iter()).map(|(a, b)| b.saturating_sub(*a)).collect();
        let stage = Stage {
            players_target: step.target,
            players_connected: live,
            players_failed: failed,
            window_secs: secs,
            counter_reset: counter_reset.is_some(),
            harness: report::HarnessHealth {
                wakeup_lag_ms: Latency::from_samples(lag),
                players_dropped: dropped,
            },
            movement_latency_ms: Latency::from_samples(lat),
            client: ClientCounters {
                heartbeats_sent: dc[0],
                heartbeats_per_sec: rate(dc[0] as f64, secs),
                peer_moves_observed: dc[1],
                peer_moves_per_sec: rate(dc[1] as f64, secs),
                swings_sent: dc[2],
                frames_received: dc[3],
                harness_backpressure_events: dc[4],
            },
            writer: writer_stats(&d, secs, &dbf),
            witness_writer: (!witness_db.is_empty())
                .then(|| writer_stats(&d, secs, &witness_dbf)),
            tx_per_sec_by_reducer: tx_by_reducer(&d, secs, &dbf, 15),
            event_tables: table_rates(&d, secs, &dbf, &table_filter, 20),
        };
        eprintln!(
            "[bench] rung {}: writer {:.1}% · {:.0} tx/s · move p50/p95/p99 {}/{}/{}ms ({} samples)",
            step.target,
            stage.writer.occupancy_pct,
            stage.writer.txns_per_sec,
            stage.movement_latency_ms.p50_ms,
            stage.movement_latency_ms.p95_ms,
            stage.movement_latency_ms.p99_ms,
            stage.movement_latency_ms.samples
        );
        if let Some(k) = counter_reset {
            rep.parked.push(format!(
                "Rung {}: the node's counters RESET inside the measured window (`{k}` went \
                 backwards) — every server-side number for that stage is void. Re-run it.",
                step.target
            ));
        }
        rep.stages.push(stage);
    }

    eprintln!("[bench] ramp complete — disconnecting {} players…", handles.len());
    sh.stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    rep.login_errors = sh.errors.lock().expect("errors lock").clone();

    let text = rep.to_string();
    print!("{text}");
    if let Some(p) = args.opt("json") {
        write_out(&p, &serde_json::to_string_pretty(&rep)?)?;
    }
    if let Some(p) = args.opt("text") {
        write_out(&p, &text)?;
    }
    Ok(())
}

/// `--dry-run`: prove the server-side half of the report BEFORE committing a machine to a ramp.
/// Scrapes once and shows which databases the node exposes, whether each of the four required
/// metric families is present, and which reducers/tables the `--db` / `--tables-filter`
/// selection currently matches. Connects no players and writes nothing.
fn dry_run(
    s: &metrics::Snapshot,
    url: &str,
    db: &str,
    dbf: &str,
    table_filter: &str,
) -> Result<()> {
    println!("metrics endpoint   {url} — {} sample lines", s.0.len());
    println!("databases on node  {:?}", s.databases());
    println!("  …measurable      {:?}", s.databases_with(metrics::OCCUPANCY_FAMILY));
    println!(
        "--db selection     {}",
        if db.is_empty() { "<all databases aggregated>" } else { db }
    );
    println!();
    println!("required metric families:");
    for (what, name) in metrics::REQUIRED_FAMILIES {
        // "absent" and "present but zero" are different answers, and only one of them is a
        // problem — `sum` alone cannot tell them apart (that is the whole point of `has_any`).
        let status = if s.has_any(name, &[dbf]) {
            format!("cumulative={:.3}", s.sum(name, &[dbf]))
        } else {
            "!! ABSENT under this --db selection".to_string()
        };
        println!("  {:<26} {:<44} {status}", what, name);
    }
    println!();
    println!("reducers seen so far (top 10 by lifetime tx):");
    for r in tx_by_reducer(s, 1.0, dbf, 10) {
        println!("  {:<34} {:>12.0}", r.name, r.per_sec);
    }
    println!();
    println!("tables the report would list (top 10 by lifetime rows):");
    for t in table_rates(s, 1.0, dbf, table_filter, 10) {
        println!("  {:<34} {:>12.0} ins {:>12.0} del", t.table, t.inserts_per_sec, t.reaps_per_sec);
    }
    println!();
    println!("(movement latency is measured client-side and needs a real ramp — nothing to preflight)");
    Ok(())
}

fn write_out(path: &str, body: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(path, body).with_context(|| format!("write {path}"))?;
    eprintln!("[bench] wrote {path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metric→report reduction is the one piece of glue that can silently report zeros, so
    /// drive it with a synthetic before/after scrape pair over a known 10s window.
    #[test]
    fn stage_metrics_are_derived_from_a_scrape_delta() {
        let before = metrics::parse(
            r#"
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Reducer"} 100.0
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Subscribe"} 10.0
spacetime_num_txns_total{committed="true",db="abc",reducer="movement_update",txn_type="Reducer"} 1000
spacetime_num_txns_total{committed="true",db="abc",reducer="tick_melee",txn_type="Reducer"} 500
spacetime_reducer_wait_time_sec_sum{db="abc",reducer="movement_update"} 1.0
spacetime_reducer_wait_time_sec_count{db="abc",reducer="movement_update"} 1000
spacetime_num_rows_inserted_total{db="abc",table_name="game_movement_event",txn_type="Reducer"} 5000
spacetime_num_rows_deleted_total{db="abc",table_name="game_movement_event",txn_type="Reducer"} 4000
spacetime_num_rows_inserted_total{db="abc",table_name="st_table",txn_type="Internal"} 1
spacetime_num_bytes_sent_to_clients_total{db="abc"} 0
spacetime_num_rows_scanned_total{db="abc"} 0
"#,
        );
        let after = metrics::parse(
            r#"
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Reducer"} 103.0
spacetime_txn_cpu_time_sec_sum{db="abc",reducer="",txn_type="Subscribe"} 11.0
spacetime_num_txns_total{committed="true",db="abc",reducer="movement_update",txn_type="Reducer"} 3000
spacetime_num_txns_total{committed="true",db="abc",reducer="tick_melee",txn_type="Reducer"} 600
spacetime_reducer_wait_time_sec_sum{db="abc",reducer="movement_update"} 3.0
spacetime_reducer_wait_time_sec_count{db="abc",reducer="movement_update"} 3000
spacetime_num_rows_inserted_total{db="abc",table_name="game_movement_event",txn_type="Reducer"} 25000
spacetime_num_rows_deleted_total{db="abc",table_name="game_movement_event",txn_type="Reducer"} 23000
spacetime_num_rows_inserted_total{db="abc",table_name="st_table",txn_type="Internal"} 9
spacetime_num_bytes_sent_to_clients_total{db="abc"} 10240
spacetime_num_rows_scanned_total{db="abc"} 1000
"#,
        );
        let d = before.delta(&after);
        let dbf = metrics::db_filter("abc");

        let w = writer_stats(&d, 10.0, &dbf);
        // 3s reducer + 1s subscribe CPU over a 10s window = 40% of one serialized writer.
        assert!((w.occupancy_pct - 40.0).abs() < 1e-9, "occupancy was {}", w.occupancy_pct);
        assert!((w.reducer_cpu_sec - 3.0).abs() < 1e-9);
        assert!((w.txns_per_sec - 210.0).abs() < 1e-9, "2000+100 txns over 10s");
        // 2s of queue wait spread over 2000 newly-waited reducers = 1ms mean.
        assert!((w.mean_queue_wait_ms - 1.0).abs() < 1e-9, "wait was {}", w.mean_queue_wait_ms);
        assert!((w.egress_bytes_per_sec - 1024.0).abs() < 1e-9);

        let tx = tx_by_reducer(&d, 10.0, &dbf, 15);
        assert_eq!(tx[0], NamedRate { name: "movement_update".into(), per_sec: 200.0 });
        assert_eq!(tx[1], NamedRate { name: "tick_melee".into(), per_sec: 10.0 });

        let tables = table_rates(&d, 10.0, &dbf, "", 20);
        assert_eq!(
            tables,
            vec![TableRate {
                table: "game_movement_event".into(),
                inserts_per_sec: 2000.0,
                reaps_per_sec: 1900.0,
            }],
            "system st_ tables are excluded; the event table's delete rate IS its reap rate"
        );
    }

    const TWO_DBS: &str = r#"
spacetime_txn_cpu_time_sec_sum{db="aaa",reducer="",txn_type="Reducer"} 30.0
spacetime_txn_cpu_time_sec_sum{db="bbb",reducer="",txn_type="Reducer"} 30.0
spacetime_num_txns_total{committed="true",db="aaa",reducer="movement_update",txn_type="Reducer"} 10
spacetime_num_txns_total{committed="true",db="bbb",reducer="movement_update",txn_type="Reducer"} 10
"#;
    const ONE_DB: &str = r#"
spacetime_txn_cpu_time_sec_sum{db="aaa",reducer="",txn_type="Reducer"} 30.0
spacetime_num_txns_total{committed="true",db="aaa",reducer="movement_update",txn_type="Reducer"} 10
spacetime_num_rows_inserted_total{db="aaa",table_name="game_movement_event",txn_type="Reducer"} 1
spacetime_num_rows_deleted_total{db="aaa",table_name="game_movement_event",txn_type="Reducer"} 1
spacetime_reducer_wait_time_sec_sum{db="aaa",reducer="movement_update"} 1.0
spacetime_num_bytes_sent_to_clients_total{db="aaa",txn_type="Reducer"} 5
"#;

    /// The most dangerous failure this harness can have: a `--db` that matches nothing yields a
    /// report reading "writer occupancy 0.0%, 0 tx/s" — an idle writer — while the client-side
    /// latency numbers stay real and plausible. Against the Phase C gate that reads as "one writer
    /// is nowhere near saturating; sharding is not needed". Preflight must refuse instead.
    #[test]
    fn a_db_selection_that_matches_nothing_is_refused_not_reported_as_zero() {
        let s = metrics::parse(ONE_DB);
        let dbf = metrics::db_filter("deadbeef");
        // What the run WOULD have published, had it proceeded:
        let w = writer_stats(&s, 60.0, &dbf);
        assert_eq!(w.occupancy_pct, 0.0, "a wrong --db looks exactly like an idle writer");
        assert_eq!(tx_by_reducer(&s, 60.0, &dbf, 15), vec![]);
        // …so it must not proceed.
        let err = validate_db_selection(&s, "deadbeef", &dbf).unwrap_err().to_string();
        assert!(err.contains("matches no database"), "{err}");
        assert!(err.contains("aaa"), "the error must name the identities that DO exist: {err}");
        // The correct prefix passes.
        assert!(validate_db_selection(&s, "aaa", &metrics::db_filter("aaa")).is_ok());
    }

    /// The aggregate default is only safe while the node hosts one database. Issue #12 exists to
    /// put several on it, at which point summing them reports two half-busy writers as one busy
    /// one — a plausible number that is simply not about any single writer.
    #[test]
    fn an_empty_db_is_refused_on_a_multi_database_node() {
        let two = metrics::parse(TWO_DBS);
        let dbf = metrics::db_filter("");
        // Two writers at 50% each would be published as one writer at 100%.
        assert!((writer_stats(&two, 60.0, &dbf).occupancy_pct - 100.0).abs() < 1e-9);
        let err = validate_db_selection(&two, "", &dbf).unwrap_err().to_string();
        assert!(err.contains("2 measurable databases"), "{err}");
        // One database on the node → the aggregate default stays convenient and correct.
        assert!(validate_db_selection(&metrics::parse(ONE_DB), "", &dbf).is_ok());
        // A node with nothing published is refused too, rather than measuring an empty ramp.
        assert!(validate_db_selection(&metrics::parse(""), "", &dbf).is_err());
    }

    /// #21: the witness column's failure modes, both of which would *confirm the hypothesis*.
    /// A witness that matches nothing reports 0.0% — "the open world stayed flat" — and a witness
    /// that selects the same database as `--db` reports one writer twice, so the two columns track
    /// each other perfectly no matter what the pool does.
    #[test]
    fn a_witness_that_matches_nothing_or_duplicates_the_primary_db_is_refused() {
        let two = metrics::parse(TWO_DBS);
        // Matches nothing → would publish a beautifully flat 0.0%.
        let ghost = metrics::db_filter("deadbeef");
        assert_eq!(writer_stats(&two, 60.0, &ghost).occupancy_pct, 0.0);
        let err = validate_witness_selection(&two, "aaa", "deadbeef", &ghost).unwrap_err().to_string();
        assert!(err.contains("matches no database"), "{err}");
        assert!(err.contains("bbb"), "the error must name the identities that DO exist: {err}");
        // Same database in both columns.
        let err = validate_witness_selection(&two, "aaa", "aaa", &metrics::db_filter("aaa"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("same database"), "{err}");
        // …and both are PREFIX selectors, so overlap is the realistic typo, not equality:
        // `--db spacetime-instances --witness-db spacetime-instances-1` makes the primary column
        // aggregate the witness. Tightening the guard to `witness == db` left this suite green.
        let err = validate_witness_selection(&two, "aa", "aaa", &metrics::db_filter("aaa"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("same database"), "a witness the primary's prefix swallows: {err}");
        let err = validate_witness_selection(&two, "aaa", "aa", &metrics::db_filter("aa"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("same database"), "a witness that swallows the primary: {err}");
        // Two genuinely different databases is the whole point, and no witness at all is the
        // pre-#21 report — neither may be refused.
        assert!(validate_witness_selection(&two, "aaa", "bbb", &metrics::db_filter("bbb")).is_ok());
        assert!(validate_witness_selection(&two, "aaa", "", &metrics::db_filter("")).is_ok());
    }

    /// …and that the witness is actually SAMPLED. The ramp loop in `main` has no unit harness (it
    /// scrapes a live node), so this is a source scan, like the module's reducer-body tripwires.
    /// Adversarial review: hard-coding `witness_writer: None` and dropping the witness from the
    /// counter-reset filters each left all 22 wire-client tests green — the preflight refusals above
    /// pin only that a bad selection is REFUSED, never that a good one produces a second column.
    #[test]
    fn the_witness_column_is_sampled_over_the_same_window_and_voids_on_its_own_counter_reset() {
        let src = include_str!("main.rs");
        let at = src.find("fn main() -> Result<()> {").expect("`main` moved");
        // Bound the scan at the test module, or the assertion strings BELOW would satisfy it
        // themselves — a self-matching scanner passes on an empty `main`. (Caught by re-running the
        // two mutations this test exists for: both stayed green until this line was added.)
        let end = src[at..].find("\n#[cfg(test)]").expect("the test module follows `main`");
        let body: String = src[at..at + end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("witness_writer: (!witness_db.is_empty())")
                && body.contains("writer_stats(&d, secs, &witness_dbf)"),
            "the ramp loop no longer derives `witness_writer` from THIS window's scrape delta — \
             `--witness-db` would be accepted at preflight and then report nothing, and #21 AC#2's \
             whole flat-vs-scales comparison would silently collapse to the single-writer report"
        );
        assert!(
            body.contains("reset_filters.push(&witness_dbf)"),
            "the counter-reset check no longer covers the witness's series — a node restart that \
             touched only the witness database would void nothing and hand back a bogus second \
             column for a window whose counters were reset mid-flight"
        );
    }

    #[test]
    fn parked_is_derived_from_the_live_scrape_not_hard_coded() {
        let dbf = metrics::db_filter("aaa");
        // A node exposing every required family parks nothing.
        assert_eq!(park_missing_families(&metrics::parse(ONE_DB), &dbf), Vec::<String>::new());
        // Drop one family: the report must say the field is zero BY ABSENCE.
        let missing = metrics::parse(
            &ONE_DB.lines().filter(|l| !l.contains("reducer_wait_time")).collect::<Vec<_>>().join("\n"),
        );
        let parked = park_missing_families(&missing, &dbf);
        assert_eq!(parked.len(), 1, "{parked:?}");
        assert!(parked[0].contains("queue wait"), "{}", parked[0]);
        assert!(parked[0].contains("BY ABSENCE"), "{}", parked[0]);
    }

    #[test]
    fn args_parse_key_value_pairs_and_lists() {
        // Args::parse reads the process argv, so exercise the typed getters directly.
        let a = Args(HashMap::from([
            ("stages".to_string(), "10,20".to_string()),
            ("center".to_string(), "-1.5,2,3".to_string()),
            ("hold".to_string(), "42".to_string()),
        ]));
        assert_eq!(a.list("stages", &[1]).unwrap(), vec![10, 20]);
        assert_eq!(a.list("missing", &[1]).unwrap(), vec![1]);
        assert_eq!(a.vec3("center", [0.0; 3]).unwrap(), [-1.5, 2.0, 3.0]);
        assert_eq!(a.num::<u64>("hold", 60).unwrap(), 42);
        assert_eq!(a.num::<u64>("missing", 60).unwrap(), 60);
        assert!(a.num::<u64>("center", 0).is_err(), "a bad numeric value must not fall back");
        assert_eq!(a.str("missing", "dflt"), "dflt");
        assert_eq!(a.opt("missing"), None);
    }
}

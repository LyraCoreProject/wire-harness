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

const USAGE: &str = "\
bench — 50→200 synthetic-player capacity benchmark

USAGE:
  bench [OPTIONS]

TARGET (re-runnable per shard):
  --logon HOST[:PORT]     logon tier of the gateway under test     [127.0.0.1:3724]
  --world HOST:PORT       world tier override                      [realm-list answer]
  --metrics URL           SpacetimeDB node metrics endpoint        [http://127.0.0.1:3000/v1/metrics]
  --db PREFIX             db= label prefix selecting ONE database  [all databases on the node]

RAMP:
  --stages 50,100,150,200 additive player rungs                    [50,100,150,200]
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
  --label NAME            run label recorded in the report         [adhoc]
  --json PATH             write the machine-readable report
  --text PATH             write the human-readable report (also printed to stdout)

PREFLIGHT:
  --dry-run 1             scrape the metrics endpoint once, print what the report would be able
                          to measure, and EXIT without connecting a single player. Safe to run
                          against a node that is in use.
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
    connected: AtomicUsize,
    failed: AtomicUsize,
    counters: Counters,
    latencies: Mutex<Vec<u32>>,
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
    let mut last_flush = Instant::now();

    while !sh.stop.load(Ordering::Relaxed) {
        let now = Instant::now();

        if now >= next_hb {
            // A 3yd circular stroll around the player's base point: continuous movement with a
            // stable heading class, which is what the gateway's 150ms coalescer sees from a real
            // client running in a straight line.
            let t = hb_count as f32 * 0.35;
            let info = MovementInfo {
                flags: MovementInfo_MovementFlags::empty(),
                timestamp: sh.now_ms(),
                position: Vector3d {
                    x: bx + 3.0 * t.cos(),
                    y: by + 3.0 * t.sin(),
                    z: cfg.center[2],
                },
                orientation: 0.0,
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

        if local_lat.len() >= LATENCY_FLUSH_BATCH
            || (!local_lat.is_empty() && last_flush.elapsed() >= Duration::from_secs(1))
        {
            sh.latencies.lock().expect("latency lock").extend(local_lat.drain(..));
            last_flush = Instant::now();
        }
    }
    if !local_lat.is_empty() {
        sh.latencies.lock().expect("latency lock").extend(local_lat.drain(..));
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
        // Occupancy = the fraction of wall-clock the single serialized writer was busy. Lock-wait
        // is excluded from txn_cpu_time by SpacetimeDB, so this cannot exceed 100% per database.
        occupancy_pct: if secs > 0.0 { total_cpu / secs * 100.0 } else { 0.0 },
        reducer_cpu_sec: reducer_cpu,
        subscribe_cpu_sec: subscribe_cpu,
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
    let table_filter = args.str("tables-filter", "");
    let label = args.str("label", "adhoc");

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
        return dry_run(&preflight, &metrics_url, &db, &dbf, &table_filter);
    }

    let sh = Arc::new(Shared {
        epoch: Instant::now(),
        stop: AtomicBool::new(false),
        connected: AtomicUsize::new(0),
        failed: AtomicUsize::new(0),
        counters: Counters::default(),
        latencies: Mutex::new(Vec::new()),
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
        },
        run_cfg,
    );
    rep.parked.push(
        "Per-reducer WASM execution time is reported by the node under a placeholder db label \
         (`reducer_wasm_time_usec`), so it is not attributed per database here; the per-reducer \
         breakdown in this report is TRANSACTION COUNT, and occupancy is whole-database CPU."
            .into(),
    );

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
        let c0 = sh.counters.snapshot();
        let m0 = metrics::scrape(&metrics_url)?;
        let t0 = Instant::now();
        eprintln!("[bench] measuring {hold}s at {} players…", step.target);
        thread::sleep(Duration::from_secs(hold));
        let secs = t0.elapsed().as_secs_f64();
        let m1 = metrics::scrape(&metrics_url)?;
        let c1 = sh.counters.snapshot();
        let lat: Vec<u32> = std::mem::take(&mut *sh.latencies.lock().expect("latency lock"));

        let d = m0.delta(&m1);
        let dc: Vec<u64> = c0.iter().zip(c1.iter()).map(|(a, b)| b.saturating_sub(*a)).collect();
        let stage = Stage {
            players_target: step.target,
            players_connected: connected,
            players_failed: failed,
            window_secs: secs,
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
    let dbs: Vec<String> = s
        .0
        .keys()
        .filter_map(|k| k.find('{').map(|i| &k[i..]))
        .filter_map(|labels| metrics::label_value(labels, "db"))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    println!("databases on node  {dbs:?}");
    println!(
        "--db selection     {}",
        if db.is_empty() { "<all databases aggregated>" } else { db }
    );
    println!();
    println!("required metric families:");
    for (what, name) in [
        ("writer occupancy %", "spacetime_txn_cpu_time_sec_sum"),
        ("tx/s by reducer", "spacetime_num_txns_total"),
        ("event inserts/s", "spacetime_num_rows_inserted_total"),
        ("event reaps/s", "spacetime_num_rows_deleted_total"),
        ("queue wait (saturation)", "spacetime_reducer_wait_time_sec_sum"),
        ("egress bytes/s", "spacetime_num_bytes_sent_to_clients_total"),
    ] {
        let v = s.sum(name, &[dbf]);
        println!("  {:<26} {:<44} cumulative={v:.3}", what, name);
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

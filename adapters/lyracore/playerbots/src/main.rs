mod analysis;
mod counters;
mod evidence;
mod movement;
mod stream;
mod timing;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use wire_client::metrics;

const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 256 * 1024 * 1024;
const WARMUP_SECONDS: u64 = 30;
const OWNERSHIP_EVIDENCE_SCHEMA: u32 = 1;
const AUTONOMOUS_CREATURE_ENTRIES: [u32; 8] = [6, 38, 69, 257, 299, 721, 883, 890];

#[derive(Deserialize, Serialize)]
struct OwnershipEvidence {
    schema: u32,
    creature_entries: Vec<u32>,
    queries: Vec<String>,
}

fn ownership_queries(entries: &[u32]) -> Vec<String> {
    let mut queries: Vec<_> = stream::OWNERSHIP_TABLES
        .iter()
        .map(|table| format!("SELECT * FROM {table}"))
        .collect();
    for table in [movement::ENTITY, "game_creature_spawn"] {
        queries.extend(
            entries
                .iter()
                .map(|entry| format!("SELECT * FROM {table} WHERE entry = {entry}")),
        );
    }
    queries
}

#[derive(Deserialize, Serialize)]
struct Inputs {
    spacetime: PathBuf,
    config_path: PathBuf,
    server: String,
    database_identity: String,
    bot_guids: Vec<u64>,
    seconds: u64,
    core_revision: String,
    package_revision: String,
    content_identity: String,
    geometry_revision: String,
    wasm_sha256: String,
    cli_sha256: String,
    fixture_resources: Value,
    ownership_evidence: OwnershipEvidence,
}

impl Inputs {
    fn check(&self) -> Result<BTreeSet<u64>> {
        let address: SocketAddr = self
            .server
            .strip_prefix("http://")
            .context("acceptance requires an explicit HTTP loopback endpoint")?
            .parse()?;
        if !address.ip().is_loopback() || address.port() == 0 {
            bail!("acceptance endpoint must be a private loopback listener");
        }
        for (name, value, length) in [
            ("database identity", &self.database_identity, 64),
            ("Core revision", &self.core_revision, 40),
            ("Package revision", &self.package_revision, 40),
            ("content identity", &self.content_identity, 64),
            ("Wasm SHA-256", &self.wasm_sha256, 64),
            ("CLI SHA-256", &self.cli_sha256, 64),
        ] {
            if value.len() != length || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("invalid {name}");
            }
        }
        if self.geometry_revision.trim().is_empty() || !self.fixture_resources.is_object() {
            bail!("geometry and fixture resources must be recorded");
        }
        if !self.spacetime.is_absolute()
            || !self.config_path.is_absolute()
            || !self.config_path.is_file()
        {
            bail!("CLI and existing private configuration need absolute paths");
        }
        let actual = format!("{:x}", Sha256::digest(fs::read(&self.spacetime)?));
        if actual != self.cli_sha256 {
            bail!("CLI executable differs from the staged source manifest");
        }
        let guids: BTreeSet<_> = self.bot_guids.iter().copied().collect();
        if ![10, 25, 100].contains(&guids.len())
            || guids.len() != self.bot_guids.len()
            || guids.contains(&0)
        {
            bail!("expected ten, twenty-five or one hundred distinct nonzero bot GUIDs");
        }
        if !(60..=3600).contains(&self.seconds) {
            bail!("measurement duration must be sixty through 3600 seconds");
        }
        if self.ownership_evidence.schema != OWNERSHIP_EVIDENCE_SCHEMA
            || self.ownership_evidence.creature_entries != AUTONOMOUS_CREATURE_ENTRIES
            || self.ownership_evidence.queries
                != ownership_queries(&self.ownership_evidence.creature_entries)
        {
            bail!("ownership evidence query identity differs from this observer");
        }
        Ok(guids)
    }
}

struct Subscription(Child);
impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct DecisionLogs {
    child: Subscription,
    path: PathBuf,
}

impl DecisionLogs {
    fn start(inputs: &Inputs, directory: &Path) -> Result<Self> {
        let path = directory.join("decision-logs.jsonl");
        let mut child = Subscription(
            Command::new(&inputs.spacetime)
                .arg("--config-path")
                .arg(&inputs.config_path)
                .args([
                    "logs",
                    "--no-config",
                    "--format",
                    "json",
                    "--num-lines",
                    "1",
                    "--follow",
                    "-s",
                    &inputs.server,
                    &inputs.database_identity,
                ])
                .stdout(File::create(&path)?)
                .stderr(File::create(directory.join("decision-logs.stderr"))?)
                .spawn()?,
        );
        let started = Instant::now();
        loop {
            check_log_size(&path)?;
            if let Some(status) = child.0.try_wait()? {
                bail!("decision log collection exited before its initial snapshot: {status}");
            }
            let bytes = fs::read(&path)?;
            if !bytes.is_empty() && bytes.last() == Some(&b'\n') {
                for line in bytes
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                {
                    serde_json::from_slice::<Value>(line)
                        .context("decision log initial snapshot is not complete JSON")?;
                }
                return Ok(Self { child, path });
            }
            if started.elapsed() > Duration::from_secs(20) {
                bail!("decision log initial snapshot exceeded twenty seconds");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn check(&mut self) -> Result<()> {
        check_log_size(&self.path)?;
        if let Some(status) = self.child.0.try_wait()? {
            bail!("decision log collection exited during measurement: {status}");
        }
        Ok(())
    }

    fn finish(mut self, expected: &BTreeSet<timing::Decision>) -> Result<Value> {
        let started = Instant::now();
        loop {
            self.check()?;
            if let Some(raw) = complete_log_text(&self.path)? {
                if timing::report(&raw, expected).is_ok() {
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(20) {
                let raw = complete_log_text(&self.path)?
                    .context("decision log collection has no complete record")?;
                return timing::report(&raw, expected)
                    .context("decision log collection did not receive every measured decision");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.child
            .0
            .kill()
            .context("failed to stop decision log collection")?;
        self.child
            .0
            .wait()
            .context("failed to wait for decision log collection")?;
        retain_complete_log_lines(&self.path)?;
        timing::report(&fs::read_to_string(&self.path)?, expected)
    }
}

fn check_log_size(path: &Path) -> Result<()> {
    if fs::metadata(path)?.len() > MAX_LOG_BYTES {
        bail!("decision log evidence exceeds the stream limit");
    }
    Ok(())
}

fn complete_log_text(path: &Path) -> Result<Option<String>> {
    let mut bytes = fs::read(path)?;
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(None);
    };
    bytes.truncate(end + 1);
    Ok(Some(
        String::from_utf8(bytes).context("decision log output is not UTF-8")?,
    ))
}

fn retain_complete_log_lines(path: &Path) -> Result<()> {
    let bytes = fs::read(path)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .context("decision log collection has no complete record")?
        + 1;
    if end < bytes.len() {
        let file = File::options().write(true).open(path)?;
        file.set_len(end as u64)?;
        file.sync_all()?;
    }
    Ok(())
}

struct Received {
    micros: u64,
    line: String,
}

fn subscribe(
    inputs: &Inputs,
    directory: &Path,
    origin: Instant,
) -> Result<(Subscription, Receiver<Result<Received>>)> {
    let mut command = Command::new(&inputs.spacetime);
    command
        .args(["--config-path"])
        .arg(&inputs.config_path)
        .args([
            "subscribe",
            "--no-config",
            "-s",
            &inputs.server,
            "--confirmed",
            "true",
            "--print-initial-update",
            "--timeout",
        ])
        .arg((inputs.seconds + WARMUP_SECONDS + 30).to_string())
        .arg(&inputs.database_identity);
    for table in stream::TABLES {
        command.arg(format!("SELECT * FROM {table}"));
    }
    for table in movement::TABLES {
        for guid in &inputs.bot_guids {
            command.arg(format!("SELECT * FROM {table} WHERE guid = {guid}"));
        }
    }
    for query in &inputs.ownership_evidence.queries {
        command.arg(query);
    }
    let mut child = Subscription(
        command
            .stdout(Stdio::piped())
            .stderr(File::create(directory.join("subscription.stderr"))?)
            .spawn()?,
    );
    let stdout = child
        .0
        .stdout
        .take()
        .context("subscription stdout unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(64);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let result = (|| -> Result<Option<Received>> {
                let mut bytes = Vec::new();
                let length = reader
                    .by_ref()
                    .take(MAX_LINE_BYTES + 1)
                    .read_until(b'\n', &mut bytes)?;
                if length == 0 {
                    return Ok(None);
                }
                if length as u64 > MAX_LINE_BYTES || bytes.last() != Some(&b'\n') {
                    bail!("subscription transaction exceeds the line limit or is incomplete");
                }
                Ok(Some(Received {
                    micros: origin.elapsed().as_micros() as u64,
                    line: String::from_utf8(bytes).context("subscription output is not UTF-8")?,
                }))
            })();
            match result {
                Ok(Some(row)) => match sender.try_send(Ok(row)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                    Err(mpsc::TrySendError::Full(_)) => {
                        let _ = sender.send(Err(anyhow::anyhow!(
                            "subscription observer queue exceeded 64 transactions"
                        )));
                        break;
                    }
                },
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    Ok((child, receiver))
}

fn retain_scrape(
    inputs: &Inputs,
    directory: &Path,
    name: &str,
    origin: Instant,
) -> Result<(metrics::Snapshot, Value)> {
    let began = origin.elapsed().as_micros();
    let raw = metrics::http_get(&format!("{}/v1/metrics", inputs.server))?;
    let ended = origin.elapsed().as_micros();
    fs::write(directory.join(format!("{name}.prom")), &raw)?;
    Ok((
        metrics::parse(&raw)?,
        serde_json::json!({"start_micros":began,"end_micros":ended}),
    ))
}

fn retain_measurement_window(directory: &Path, start_micros: u64, seconds: u64) -> Result<Value> {
    let duration_micros = seconds
        .checked_mul(1_000_000)
        .context("measurement duration overflow")?;
    let end_micros = start_micros
        .checked_add(duration_micros)
        .context("measurement window overflow")?;
    let window = serde_json::json!({
        "start_micros": start_micros,
        "end_micros": end_micros,
        "duration_micros": duration_micros,
        "end_boundary": "exclusive",
    });
    let mut file = File::create(directory.join("measurement-window.json"))?;
    file.write_all(&serde_json::to_vec_pretty(&window)?)?;
    file.sync_all()?;
    Ok(window)
}

fn collect(
    inputs: &Inputs,
    directory: &Path,
    expected: &BTreeSet<u64>,
    archives: &mut evidence::Archives,
) -> Result<Value> {
    let mut decision_logs = DecisionLogs::start(inputs, directory)?;
    let origin = Instant::now();
    let (mut subscription, receiver) = subscribe(inputs, directory, origin)?;
    let mut stream = stream::Stream::default();
    let creature_entries = inputs
        .ownership_evidence
        .creature_entries
        .iter()
        .copied()
        .collect();
    let mut measurement = analysis::Measurement::new(expected, creature_entries);
    let mut warmed = BTreeSet::new();
    let mut stream_bytes = 0;
    let mut before = None;
    let mut before_window = Value::Null;
    let mut measurement_window = Value::Null;
    let mut start_micros = None;
    let mut measured_passes = 0_u64;
    let mut measured_decisions = BTreeSet::new();
    loop {
        decision_logs.check()?;
        let now = origin.elapsed();
        if start_micros
            .is_some_and(|start| now.as_micros() as u64 >= start + (inputs.seconds + 5) * 1_000_000)
        {
            bail!("subscription did not observe the end of the measurement window");
        }
        if before.is_none() && now > Duration::from_secs(WARMUP_SECONDS) {
            bail!("not every staged bot completed a scheduled warm-up pass");
        }
        let received = receiver.recv_timeout(Duration::from_secs(1));
        let received = match received {
            Ok(row) => row?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if subscription.0.try_wait()?.is_some() {
                    bail!("subscription exited before measurement completed");
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("subscription closed before measurement completed")
            }
        };
        stream_bytes += received.line.len() as u64;
        if stream_bytes > MAX_STREAM_BYTES {
            bail!("subscription evidence exceeds the stream limit");
        }
        archives
            .transactions
            .append(&serde_json::json!({"received_micros":received.micros,"line":received.line}))?;
        let update: Value = serde_json::from_str(&received.line)?;
        let after_window =
            start_micros.is_some_and(|start| received.micros >= start + inputs.seconds * 1_000_000);
        let measured = !after_window && start_micros.is_some_and(|start| received.micros >= start);
        let pass = stream.apply(&update, expected)?;
        if !after_window {
            measurement.observe_transaction(&update, measured)?;
        }
        if let Some(pass) = pass {
            archives.passes.append(
                &serde_json::json!({"received_micros":received.micros,"measured":measured,"pass":pass}),
            )?;
            measurement.observe(&pass, measured)?;
            if measured {
                measured_passes += 1;
                for runner in &pass.runners {
                    if !measured_decisions.insert(timing::decision(runner)?) {
                        bail!("a decision appears in more than one measured pass");
                    }
                }
            }
            warmed.extend(pass.processed_guids);
        }
        if before.is_none() && warmed == *expected {
            measurement.ready()?;
            let (snapshot, window) = retain_scrape(inputs, directory, "before", origin)?;
            snapshot.counter_delta(
                &snapshot,
                "spacetime_txn_cpu_time_sec_sum",
                &[("db", &inputs.database_identity), ("txn_type", "Reducer")],
            )?;
            before = Some(snapshot);
            before_window = window;
            let start = origin.elapsed().as_micros() as u64;
            measurement_window = retain_measurement_window(directory, start, inputs.seconds)?;
            start_micros = Some(start);
        }
        if after_window {
            break;
        }
    }
    if subscription.0.try_wait()?.is_some() {
        bail!("subscription exited during measurement");
    }
    let (after, after_window) = retain_scrape(inputs, directory, "after", origin)?;
    let before = before.context("missing initial measurement scrape")?;
    let labels = [
        ("db", inputs.database_identity.as_str()),
        ("txn_type", "Reducer"),
    ];
    let scanned_rows = before.counter_delta(&after, "spacetime_num_rows_scanned_total", &labels)?;
    let inserted_rows =
        before.counter_delta(&after, "spacetime_num_rows_inserted_total", &labels)?;
    let deleted_rows = before.counter_delta(&after, "spacetime_num_rows_deleted_total", &labels)?;
    // Keep both scrape latencies in the denominator of the writer-time estimate.
    let metric_window_micros = after_window["end_micros"]
        .as_u64()
        .context("missing final scrape end")?
        .checked_sub(
            before_window["start_micros"]
                .as_u64()
                .context("missing initial scrape start")?,
        )
        .context("scrape clock moved backwards")?;
    let counter_report = counters::report(
        &before,
        &after,
        &inputs.database_identity,
        metric_window_micros as f64 / 1_000_000.0,
    )?;
    drop(subscription);
    let timing_report = decision_logs.finish(&measured_decisions)?;
    Ok(
        serde_json::json!({"status":"captured", "measured_passes":measured_passes,
        "stream_bytes":stream_bytes,"warmup_micros":start_micros,
        "measurement_window":measurement_window,
        "before_scrape":before_window,"after_scrape":after_window,
        "transaction_measurements":counter_report,"rows_scanned":scanned_rows,
        "decision_timings":timing_report,
        "physical_rows_inserted":inserted_rows,"physical_rows_deleted":deleted_rows,
        "behavior_measurements":measurement.report(),
        "row_accounting":"an update contributes one deletion and one insertion",
        "acceptance":"pending an observed load fixture and capacity assessment"}),
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        bail!("usage: vanilla-wire-playerbots INPUT.json NEW_EVIDENCE_DIRECTORY");
    }
    let inputs: Inputs = serde_json::from_slice(&fs::read(&args[0])?)?;
    let expected = inputs.check()?;
    let directory = Path::new(&args[1]);
    fs::create_dir(directory).context("evidence directory must be new")?;
    fs::write(
        directory.join("inputs.json"),
        serde_json::to_vec_pretty(&inputs)?,
    )?;
    let result = evidence::retain(directory, |archives| {
        collect(&inputs, directory, &expected, archives)
    });
    let report = match &result {
        Ok(report) => report.clone(),
        Err(error) => serde_json::json!({"status":"failed","error":format!("{error:#}")}),
    };
    fs::write(
        directory.join("capture.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "playerbots-observer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn inputs(spacetime: PathBuf, config_path: PathBuf) -> Inputs {
        Inputs {
            spacetime,
            config_path,
            server: "http://127.0.0.1:3000".into(),
            database_identity: "1".repeat(64),
            bot_guids: (1..=10).collect(),
            seconds: 60,
            core_revision: "2".repeat(40),
            package_revision: "3".repeat(40),
            content_identity: "4".repeat(64),
            geometry_revision: "fixture".into(),
            wasm_sha256: "5".repeat(64),
            cli_sha256: "6".repeat(64),
            fixture_resources: serde_json::json!({}),
            ownership_evidence: OwnershipEvidence {
                schema: OWNERSHIP_EVIDENCE_SCHEMA,
                creature_entries: AUTONOMOUS_CREATURE_ENTRIES.into(),
                queries: ownership_queries(&AUTONOMOUS_CREATURE_ENTRIES),
            },
        }
    }

    #[test]
    fn ownership_query_identity_is_exact_and_bounded() {
        let queries = ownership_queries(&AUTONOMOUS_CREATURE_ENTRIES);
        assert_eq!(queries.len(), 21);
        assert_eq!(queries[0], "SELECT * FROM game_creature_quest_tap");
        assert_eq!(queries[4], "SELECT * FROM game_corpse_loot_eligible");
        assert_eq!(
            &queries[5..13],
            [
                "SELECT * FROM game_world_entity WHERE entry = 6",
                "SELECT * FROM game_world_entity WHERE entry = 38",
                "SELECT * FROM game_world_entity WHERE entry = 69",
                "SELECT * FROM game_world_entity WHERE entry = 257",
                "SELECT * FROM game_world_entity WHERE entry = 299",
                "SELECT * FROM game_world_entity WHERE entry = 721",
                "SELECT * FROM game_world_entity WHERE entry = 883",
                "SELECT * FROM game_world_entity WHERE entry = 890",
            ]
        );
        assert_eq!(
            &queries[13..],
            [
                "SELECT * FROM game_creature_spawn WHERE entry = 6",
                "SELECT * FROM game_creature_spawn WHERE entry = 38",
                "SELECT * FROM game_creature_spawn WHERE entry = 69",
                "SELECT * FROM game_creature_spawn WHERE entry = 257",
                "SELECT * FROM game_creature_spawn WHERE entry = 299",
                "SELECT * FROM game_creature_spawn WHERE entry = 721",
                "SELECT * FROM game_creature_spawn WHERE entry = 883",
                "SELECT * FROM game_creature_spawn WHERE entry = 890",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn following_decision_logs_survive_backing_file_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let directory = Directory::new();
        let config = directory.0.join("config.toml");
        fs::write(&config, "").unwrap();
        let backing = directory.0.join("current.log");
        let arguments = directory.0.join("arguments");
        let command = directory.0.join("spacetime");
        let first = serde_json::json!({"function":"tick_creatures","message":
            "Timing span \"playerbots_decision guid=1 generation=1 observed_micros=1\": 1ms"});
        let second = serde_json::json!({"function":"tick_creatures","message":
            "Timing span \"playerbots_decision guid=2 generation=1 observed_micros=2\": 2ms"});
        fs::write(
            &command,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{{\"message\":\"ready\"}}'\nsleep .1\nprintf '%s\\n' '{}' > '{}'\nprintf '%s\\n' '{}'\nsleep .1\nprintf '%s\\n' '{}' > '{}'\nprintf '%s\\n' '{}'\nprintf '{{\"partial\":'\nsleep 30\n",
                arguments.display(),
                first,
                backing.display(),
                first,
                second,
                backing.display(),
                second,
            ),
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();

        let logs = DecisionLogs::start(&inputs(command, config), &directory.0).unwrap();
        let expected = [(1, 1, 1), (2, 1, 2)].into_iter().collect();
        let report = logs.finish(&expected).unwrap();

        assert_eq!(report["count"], 2);
        assert_eq!(fs::read_to_string(backing).unwrap(), format!("{second}\n"));
        let retained = fs::read_to_string(directory.0.join("decision-logs.jsonl")).unwrap();
        assert!(retained.contains(&first.to_string()));
        assert!(retained.contains(&second.to_string()));
        assert!(retained
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok()));
        let arguments = fs::read_to_string(arguments).unwrap();
        assert!(arguments.contains("--num-lines\n1\n--follow\n"));
    }

    #[test]
    fn measurement_window_survives_later_capture_failure() {
        let directory = Directory::new();
        let result = evidence::retain(&directory.0, |_archives| {
            retain_measurement_window(&directory.0, 42, 60)?;
            bail!("later aggregation failed")
        });

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("later aggregation failed"));
        let window: Value =
            serde_json::from_slice(&fs::read(directory.0.join("measurement-window.json")).unwrap())
                .unwrap();
        assert_eq!(window["start_micros"], 42);
        assert_eq!(window["end_micros"], 60_000_042);
        assert_eq!(window["duration_micros"], 60_000_000);
        assert_eq!(window["end_boundary"], "exclusive");
    }
}

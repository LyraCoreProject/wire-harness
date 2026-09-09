mod analysis;
mod counters;
mod evidence;
mod movement;
mod stream;
mod timing;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
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

fn decision_timings(
    inputs: &Inputs,
    directory: &Path,
    expected: &BTreeSet<timing::Decision>,
) -> Result<Value> {
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
        if fs::metadata(&path)?.len() > MAX_LOG_BYTES {
            bail!("decision log evidence exceeds the stream limit");
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                bail!("decision log collection failed: {status}");
            }
            break;
        }
        if started.elapsed() > Duration::from_secs(20) {
            bail!("decision log collection exceeded twenty seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    timing::report(&fs::read_to_string(path)?, expected)
}

fn collect(
    inputs: &Inputs,
    directory: &Path,
    expected: &BTreeSet<u64>,
    archives: &mut evidence::Archives,
) -> Result<Value> {
    let origin = Instant::now();
    let (mut subscription, receiver) = subscribe(inputs, directory, origin)?;
    let mut stream = stream::Stream::default();
    let mut measurement = analysis::Measurement::new(expected);
    let mut warmed = BTreeSet::new();
    let mut stream_bytes = 0;
    let mut before = None;
    let mut before_window = Value::Null;
    let mut start_micros = None;
    let mut measured_passes = 0_u64;
    let mut measured_decisions = BTreeSet::new();
    loop {
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
            start_micros = Some(origin.elapsed().as_micros() as u64);
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
    let timing_report = decision_timings(inputs, directory, &measured_decisions)?;
    Ok(
        serde_json::json!({"status":"captured", "measured_passes":measured_passes,
        "stream_bytes":stream_bytes,"warmup_micros":start_micros,
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

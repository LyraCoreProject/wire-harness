use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Serialize;
use serde_json::{json, Value};

const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_GZIP_BYTES: u64 = 256 * 1024 * 1024;

struct LimitedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.written) {
            return Err(io::Error::other(
                "compressed evidence exceeds its byte limit",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) struct JsonlGzip {
    name: &'static str,
    encoder: GzEncoder<LimitedWriter<File>>,
    json_bytes: u64,
    json_limit: u64,
}

impl JsonlGzip {
    fn new(name: &'static str, file: File, json_limit: u64, gzip_limit: u64) -> Self {
        Self {
            name,
            encoder: GzEncoder::new(
                LimitedWriter {
                    inner: file,
                    written: 0,
                    limit: gzip_limit,
                },
                Compression::fast(),
            ),
            json_bytes: 0,
            json_limit,
        }
    }

    pub(super) fn append(&mut self, record: &impl Serialize) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        if line.len() as u64 > self.json_limit.saturating_sub(self.json_bytes) {
            bail!("{} exceeds its uncompressed byte limit", self.name);
        }
        self.encoder.write_all(&line)?;
        self.json_bytes += line.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> Value {
        let mut errors = Vec::new();
        if let Err(error) = self.encoder.try_finish() {
            errors.push(format!("gzip finalization: {error}"));
        }
        let writer = self.encoder.get_ref();
        if let Err(error) = writer.inner.sync_all() {
            errors.push(format!("file synchronization: {error}"));
        }
        json!({
            "path": self.name,
            "codec": "gzip",
            "compression_level": 1,
            "status": if errors.is_empty() { "finalized" } else { "failed" },
            "complete_record_bytes": self.json_bytes,
            "compressed_bytes": writer.written,
            "uncompressed_limit_bytes": self.json_limit,
            "compressed_limit_bytes": writer.limit,
            "errors": errors,
        })
    }
}

pub(super) struct Archives {
    pub(super) transactions: JsonlGzip,
    pub(super) passes: JsonlGzip,
}

/// Finalize both archives after success or a capture error, and retain every failure.
pub(super) fn retain(
    directory: &Path,
    collect: impl FnOnce(&mut Archives) -> Result<Value>,
) -> Result<Value> {
    let transactions = File::create(directory.join("transactions.jsonl.gz"))?;
    let passes = File::create(directory.join("passes.jsonl.gz"))?;
    let mut archives = Archives {
        transactions: JsonlGzip::new(
            "transactions.jsonl.gz",
            transactions,
            MAX_JSON_BYTES,
            MAX_GZIP_BYTES,
        ),
        passes: JsonlGzip::new("passes.jsonl.gz", passes, MAX_JSON_BYTES, MAX_GZIP_BYTES),
    };
    let result = collect(&mut archives);
    let retained = json!([archives.transactions.finish(), archives.passes.finish(),]);
    let mut errors = Vec::new();
    if let Err(error) = &result {
        errors.push(format!("capture: {error:#}"));
    }
    for archive in retained.as_array().into_iter().flatten() {
        if archive["status"] != "finalized" {
            errors.push(format!("archive finalization: {archive}"));
        }
    }
    if let Err(error) = fs::write(
        directory.join("retention.json"),
        serde_json::to_vec_pretty(&retained)?,
    ) {
        errors.push(format!("retention report: {error}"));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    let mut report = result?;
    report["evidence_archives"] = retained;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "playerbots-evidence-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn decoded(&self, name: &str) -> String {
            let mut text = String::new();
            GzDecoder::new(File::open(self.0.join(name)).unwrap())
                .read_to_string(&mut text)
                .unwrap();
            text
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn capture_failure_keeps_decodable_transaction_and_pass_archives() {
        let directory = Directory::new();
        let transaction: Value =
            serde_json::from_str(include_str!("../fixtures/initial-transaction.json")).unwrap();
        let pass = json!({"received_micros": 42, "measured": false, "pass": {"runners": []}});
        let error = retain(&directory.0, |archives| {
            archives.transactions.append(&transaction)?;
            archives.passes.append(&pass)?;
            bail!("fixture disconnected")
        })
        .unwrap_err();
        assert!(error.to_string().contains("fixture disconnected"));
        assert_eq!(
            serde_json::from_str::<Value>(&directory.decoded("transactions.jsonl.gz")).unwrap(),
            transaction
        );
        assert_eq!(
            serde_json::from_str::<Value>(&directory.decoded("passes.jsonl.gz")).unwrap(),
            pass
        );
        let retained: Value =
            serde_json::from_slice(&fs::read(directory.0.join("retention.json")).unwrap()).unwrap();
        assert!(retained
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "finalized"));
    }

    #[test]
    fn uncompressed_limit_refuses_the_next_whole_record() {
        let directory = Directory::new();
        let mut archive = JsonlGzip::new(
            "bounded.gz",
            File::create(directory.0.join("bounded.gz")).unwrap(),
            8,
            1024,
        );
        archive.append(&json!({"a": 1})).unwrap();
        assert!(archive.append(&json!({"b": 2})).is_err());
        let report = archive.finish();
        assert_eq!(report["status"], "finalized");
        assert_eq!(report["complete_record_bytes"], 8);
        assert_eq!(directory.decoded("bounded.gz"), "{\"a\":1}\n");
    }

    #[test]
    fn compressed_limit_reports_an_unfinished_archive() {
        let directory = Directory::new();
        let archive = JsonlGzip::new(
            "bounded.gz",
            File::create(directory.0.join("bounded.gz")).unwrap(),
            1024,
            1,
        );
        let report = archive.finish();
        assert_eq!(report["status"], "failed");
        assert!(report["errors"][0].as_str().unwrap().contains("byte limit"));
        assert!(fs::metadata(directory.0.join("bounded.gz")).unwrap().len() <= 1);
    }

    #[test]
    fn success_reports_exact_archive_sizes() {
        let directory = Directory::new();
        let report = retain(&directory.0, |archives| {
            archives.transactions.append(&json!({"a": 1}))?;
            Ok(json!({"status": "captured"}))
        })
        .unwrap();
        assert_eq!(report["status"], "captured");
        assert_eq!(report["evidence_archives"][0]["complete_record_bytes"], 8);
        assert_eq!(
            report["evidence_archives"][0]["compressed_bytes"],
            fs::metadata(directory.0.join("transactions.jsonl.gz"))
                .unwrap()
                .len()
        );
        assert_eq!(directory.decoded("passes.jsonl.gz"), "");
    }
}

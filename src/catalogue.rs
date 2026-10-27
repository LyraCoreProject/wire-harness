use anyhow::{Context, Result};

use crate::trace::{GoldenTrace, Trace};

pub struct Entry {
    pub id: &'static str,
    pub scenario: &'static str,
    pub trace: &'static str,
}

pub const ENTRIES: &[Entry] = &[Entry {
    id: "connection_identity",
    scenario: include_str!("../scenarios/connection_identity.toml"),
    trace: include_str!("../traces/connection_identity.json"),
}];

pub fn get(id: &str) -> Result<GoldenTrace> {
    let entry = ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .context("unknown scenario id")?;
    GoldenTrace::new(entry.scenario, Trace::parse(entry.trace.as_bytes())?)
}

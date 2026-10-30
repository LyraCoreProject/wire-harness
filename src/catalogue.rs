use anyhow::{Context, Result};

use crate::trace::{GoldenTrace, Trace};

pub struct Entry {
    pub id: &'static str,
    pub scenario: &'static str,
    pub trace: &'static str,
}

pub const ENTRIES: &[Entry] = &[
    Entry {
        id: "bank_slot_purchase",
        scenario: include_str!("../scenarios/bank_slot_purchase.toml"),
        trace: include_str!("../traces/bank_slot_purchase.json"),
    },
    Entry {
        id: "bank_slot_purchase__observer",
        scenario: include_str!("../scenarios/generated/bank_slot_purchase__observer.toml"),
        trace: include_str!("../traces/bank_slot_purchase__observer.json"),
    },
    Entry {
        id: "bank_slot_purchase__spawn_beside_viewer",
        scenario: include_str!(
            "../scenarios/generated/bank_slot_purchase__spawn_beside_viewer.toml"
        ),
        trace: include_str!("../traces/bank_slot_purchase__spawn_beside_viewer.json"),
    },
    Entry {
        id: "connection_identity",
        scenario: include_str!("../scenarios/connection_identity.toml"),
        trace: include_str!("../traces/connection_identity.json"),
    },
    Entry {
        id: "taxi_single_hop",
        scenario: include_str!("../scenarios/taxi_single_hop.toml"),
        trace: include_str!("../traces/taxi_single_hop.json"),
    },
    Entry {
        id: "taxi_single_hop__late_observer",
        scenario: include_str!("../scenarios/generated/taxi_single_hop__late_observer.toml"),
        trace: include_str!("../traces/taxi_single_hop__late_observer.json"),
    },
    Entry {
        id: "taxi_single_hop__reconnect",
        scenario: include_str!("../scenarios/generated/taxi_single_hop__reconnect.toml"),
        trace: include_str!("../traces/taxi_single_hop__reconnect.json"),
    },
    Entry {
        id: "two_character_visibility",
        scenario: include_str!("../scenarios/two_character_visibility.toml"),
        trace: include_str!("../traces/two_character_visibility.json"),
    },
    Entry {
        id: "two_character_visibility__opposite_faction",
        scenario: include_str!(
            "../scenarios/generated/two_character_visibility__opposite_faction.toml"
        ),
        trace: include_str!("../traces/two_character_visibility__opposite_faction.json"),
    },
];

pub fn get(id: &str) -> Result<GoldenTrace> {
    let entry = ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .context("unknown scenario id")?;
    GoldenTrace::new(entry.scenario, Trace::parse(entry.trace.as_bytes())?)
}

use anyhow::{bail, ensure, Result};
use wire_client::compare::{compare, DeliberateDeviation};
use wire_client::trace::{GoldenTrace, Trace};

pub fn strictness_command() -> Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("strictness-gate") {
        ensure!(args.len() == 1, "strictness-gate takes no arguments");
        let reports = wire_client::catalogue::ENTRIES
            .iter()
            .map(|entry| wire_client::strictness::analyze(&wire_client::catalogue::get(entry.id)?))
            .collect::<Result<Vec<_>>>()?;
        for report in &reports {
            println!(
                "{}: {} mutations, {} detected, {} excused, {} unexplained",
                report.scenario,
                report.mutations,
                report.detected,
                report.excused_by_rule.values().sum::<usize>(),
                report.unexplained_survivors.len()
            );
            for broad in &report.broad_rules {
                println!(
                    "  broad rule {}: {}% of mutations",
                    broad.name, broad.share_percent
                );
            }
        }
        wire_client::strictness::check_allowlist(
            &reports,
            include_str!("../strictness_allowlist.json"),
        )?;
        return Ok(true);
    }
    if args.first().map(String::as_str) != Some("strictness") {
        return Ok(false);
    }
    ensure!(
        (2..=3).contains(&args.len()) && (args.len() == 2 || args[2] == "--json"),
        "strictness needs SCENARIO [--json]"
    );
    let golden = wire_client::catalogue::get(&args[1])?;
    let report = wire_client::strictness::analyze(&golden)?;
    if args.len() == 3 {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}: {} mutations, {} detected",
            report.scenario, report.mutations, report.detected
        );
        for (name, count) in &report.excused_by_rule {
            println!("  excused by {name}: {count}");
        }
        for survivor in &report.unexplained_survivors {
            println!("  unexplained {}: {}", survivor.id, survivor.description);
        }
        for broad in &report.broad_rules {
            println!(
                "  broad rule {}: {} of {} mutations ({}%)",
                broad.name, broad.excused, report.mutations, broad.share_percent
            );
        }
        println!(
            "  unexplained survivors: {}",
            report.unexplained_survivors.len()
        );
    }
    Ok(true)
}

pub fn compare_command() -> Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("compare") {
        return Ok(false);
    }
    ensure!(
        (4..=5).contains(&args.len()),
        "compare needs SCENARIO GOLDEN CANDIDATE [DEVIATIONS_JSON]"
    );
    let expected = GoldenTrace::new(
        &std::fs::read_to_string(&args[1])?,
        Trace::parse(&std::fs::read(&args[2])?)?,
    )?;
    let actual = Trace::parse(&std::fs::read(&args[3])?)?;
    let deviations: Vec<DeliberateDeviation> = if let Some(path) = args.get(4) {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        Vec::new()
    };
    let comparison = compare(&expected, &actual, &deviations)?;
    println!("{}", serde_json::to_string_pretty(&comparison)?);
    if !comparison.is_match() {
        bail!(
            "trace has {} rule differences and {} data differences, including {} Deliberate Deviations",
            comparison.rule_difference_count(),
            comparison.data_difference_count(),
            comparison.deviation_count()
        );
    }
    Ok(true)
}

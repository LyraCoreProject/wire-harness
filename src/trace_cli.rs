use anyhow::{bail, ensure, Result};
use wire_client::compare::{compare, DeliberateDeviation};
use wire_client::trace::{GoldenTrace, Trace};

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

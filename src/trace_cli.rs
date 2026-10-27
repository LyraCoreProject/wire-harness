use anyhow::{bail, ensure, Result};
use wire_client::compare::{compare, EquivalenceRules};
use wire_client::trace::Trace;

pub fn compare_command() -> Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("compare") {
        return Ok(false);
    }
    ensure!(
        (3..=4).contains(&args.len()),
        "compare needs GOLDEN ACTUAL [RULES_JSON]"
    );
    let expected = Trace::parse(&std::fs::read(&args[1])?)?;
    let actual = Trace::parse(&std::fs::read(&args[2])?)?;
    let rules: EquivalenceRules = if let Some(path) = args.get(3) {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        EquivalenceRules::default()
    };
    let comparison = compare(&expected, &actual, &rules)?;
    println!("{}", serde_json::to_string_pretty(&comparison)?);
    if !comparison.is_match() {
        bail!(
            "trace differs at {} fields or packet groups",
            comparison.differences.len()
        );
    }
    Ok(true)
}

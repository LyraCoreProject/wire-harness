use std::path::Path;

use anyhow::{ensure, Context, Result};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.is_empty() || args == ["--check"],
        "usage: generate_variants [--check]"
    );
    let check = !args.is_empty();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut declarations: Vec<_> = std::fs::read_dir(root.join("variants"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    declarations.sort();
    let mut generated = std::collections::BTreeSet::new();
    for path in declarations {
        if path
            .extension()
            .map_or(true, |extension| extension != "toml")
        {
            continue;
        }
        let base = path
            .file_stem()
            .and_then(|name| name.to_str())
            .context("variant filename")?;
        let scenario = std::fs::read_to_string(root.join(format!("scenarios/{base}.toml")))?;
        let declarations = std::fs::read_to_string(root.join(format!("variants/{base}.toml")))?;
        for (id, text) in wire_client::variants::derive(&scenario, &declarations)? {
            let path = root.join(format!("scenarios/generated/{id}.toml"));
            generated.insert(path.clone());
            if check {
                ensure!(
                    std::fs::read_to_string(&path)? == text,
                    "stale generated variant {id}"
                );
            } else {
                std::fs::write(&path, text)?;
            }
        }
    }
    for entry in std::fs::read_dir(root.join("scenarios/generated"))? {
        let path = entry?.path();
        ensure!(
            path.extension()
                .map_or(true, |extension| extension != "toml")
                || generated.contains(&path),
            "undeclared generated variant {}",
            path.display()
        );
    }
    Ok(())
}

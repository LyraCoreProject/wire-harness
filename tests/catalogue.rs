use std::collections::BTreeSet;
use std::path::Path;

use wire_client::catalogue::{get, ENTRIES};
use wire_client::trace::Origin;

#[test]
fn catalogue_embeds_every_scenario_and_matching_golden_trace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ids: BTreeSet<_> = ENTRIES.iter().map(|entry| entry.id.to_owned()).collect();
    assert_eq!(ids.len(), ENTRIES.len());
    for (directory, extension) in [("scenarios", "toml"), ("traces", "json")] {
        let files: BTreeSet<_> = std::fs::read_dir(root.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == extension))
            .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, files);
    }
    for entry in ENTRIES {
        assert_eq!(
            entry.scenario,
            std::fs::read_to_string(root.join("scenarios").join(format!("{}.toml", entry.id)))
                .unwrap()
        );
        assert_eq!(
            entry.trace,
            std::fs::read_to_string(root.join("traces").join(format!("{}.json", entry.id)))
                .unwrap()
        );
        let golden = get(entry.id).unwrap();
        assert_eq!(golden.scenario().id, entry.id);
        assert_eq!(golden.trace().header.scenario, entry.id);
        assert!(
            matches!(&golden.trace().header.origin, Origin::Golden { provenance } if provenance.server == "vmangos")
        );
        assert!(wire_client::compare::compare(&golden, golden.trace(), &[])
            .unwrap()
            .is_match());
    }
}

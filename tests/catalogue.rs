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
        if directory == "scenarios" {
            let mut files = files;
            for entry in std::fs::read_dir(root.join("scenarios/generated")).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == "toml") {
                    files.insert(path.file_stem().unwrap().to_str().unwrap().to_owned());
                }
            }
            assert_eq!(ids, files);
        } else {
            assert_eq!(ids, files);
        }
    }
    for entry in ENTRIES {
        let scenario_path = if entry.id.contains("__") {
            root.join("scenarios/generated")
        } else {
            root.join("scenarios")
        };
        assert_eq!(
            entry.scenario,
            std::fs::read_to_string(scenario_path.join(format!("{}.toml", entry.id))).unwrap()
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

#[test]
fn generated_variants_match_their_base_and_declarations() {
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_generate_variants"))
            .arg("--check")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn variant_declarations_reject_unused_fields() {
    let base = include_str!("../scenarios/bank_slot_purchase.toml");
    let declaration = include_str!("../variants/bank_slot_purchase.toml");
    assert!(wire_client::variants::derive(
        base,
        &declaration.replace(
            "axis = \"observer\"",
            "axis = \"observer\"\nstep = \"open\""
        )
    )
    .is_err());
}

#[test]
fn opposite_faction_and_spawn_axes_derive_from_a_base() {
    let connection = include_str!("../scenarios/connection_identity.toml");
    let opposite = wire_client::variants::derive(
        connection,
        "[[variants]]\naxis = 'opposite_faction'\nrace = 'orc'\n",
    )
    .unwrap();
    assert_eq!(opposite[0].0, "connection_identity__opposite_faction");
    let scenario = wire_client::scenario::Scenario::parse(&opposite[0].1).unwrap();
    assert_eq!(scenario.schema_version, 3);

    let bank = include_str!("../scenarios/bank_slot_purchase.toml");
    let variants =
        wire_client::variants::derive(bank, include_str!("../variants/bank_slot_purchase.toml"))
            .unwrap();
    let spawn = variants
        .iter()
        .find(|(id, _)| id == "bank_slot_purchase__spawn_beside_viewer")
        .unwrap();
    let scenario = wire_client::scenario::Scenario::parse(&spawn.1).unwrap();
    assert_eq!(scenario.characters[0].id, "viewer");
    assert_eq!(scenario.characters[1].id, "actor");
    assert_eq!(scenario.opening.fixture_creates.len(), 1);
    assert_eq!(scenario.opening.fixture_creates[0].fixture, "banker");

    let visibility = wire_client::variants::derive(
        include_str!("../scenarios/two_character_visibility.toml"),
        include_str!("../variants/two_character_visibility.toml"),
    )
    .unwrap();
    assert_eq!(
        visibility[0].0,
        "two_character_visibility__opposite_faction"
    );
    let scenario = wire_client::scenario::Scenario::parse(&visibility[0].1).unwrap();
    assert_eq!(scenario.characters[1].id, "arriving");
    assert!(matches!(
        &scenario.characters[1].fixtures[0],
        wire_client::scenario::FixtureVerb::Character { race, .. } if race == "orc"
    ));
}

#[test]
fn variants_require_valid_axes_and_event_positions() {
    let base = include_str!("../scenarios/taxi_single_hop.toml");
    for declarations in [
        "[[variants]]\naxis = 'reconnect'\nstep = 'flight'\nafter_ms = 80000\n",
        "[[variants]]\naxis = 'reconnect'\nstep = 'missing'\nafter_ms = 10\n",
        "[[variants]]\naxis = 'reconnect'\nstep = 'flight'\nafter_ms = 0\n",
        "[[variants]]\naxis = 'opposite_faction'\nrace = 'dwarf'\n",
        "[[variants]]\naxis = 'opposite_faction'\nrace = 'night-elf'\n",
        "[[variants]]\naxis = 'opposite_faction'\nrace = 'HUMAN'\n",
        "[[variants]]\naxis = 'unknown'\n",
        "[[variants]]\naxis = 'opposite_faction'\nrace = 'orc'\n[[variants]]\naxis = 'opposite_faction'\nrace = 'troll'\n",
    ] {
        assert!(wire_client::variants::derive(base, declarations).is_err(), "{declarations}");
    }
}

#[test]
fn variant_hash_covers_base_bytes_and_event_time() {
    let base = include_str!("../scenarios/taxi_single_hop.toml");
    let declarations = "[[variants]]\naxis = 'reconnect'\nstep = 'flight'\nafter_ms = 40000\n";
    let derive = |base: &str, declarations: &str| {
        let variants = wire_client::variants::derive(base, declarations).unwrap();
        wire_client::trace::Sha256Digest::of(variants[0].1.as_bytes())
    };
    let hash = derive(base, declarations);
    assert_ne!(hash, derive(&format!("{base}\n"), declarations));
    assert_ne!(hash, derive(base, &declarations.replace("40000", "41000")));
    assert_ne!(hash, wire_client::trace::Sha256Digest::of(base.as_bytes()));
}

#[test]
fn spawn_variant_requires_a_viewer_connected_before_fixture_setup() {
    let variants = wire_client::variants::derive(
        include_str!("../scenarios/bank_slot_purchase.toml"),
        include_str!("../variants/bank_slot_purchase.toml"),
    )
    .unwrap();
    let (_, source) = variants
        .iter()
        .find(|(id, _)| id.ends_with("__spawn_beside_viewer"))
        .unwrap();
    let mut scenario = wire_client::scenario::Scenario::parse(source).unwrap();
    scenario.characters.swap(0, 1);
    assert!(scenario.validate().is_err());
}

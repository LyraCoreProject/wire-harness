use wire_client::compare::compare;
use wire_client::trace::{RecordedField as Field, Trace};

fn word_mut<'a>(trace: &'a mut Trace, checkpoint: &str, name: &str) -> &'a mut Field {
    trace
        .packets
        .iter_mut()
        .filter(|packet| packet.checkpoint == checkpoint)
        .find_map(|packet| {
            let Field::List(objects) = packet.fields.get_mut("objects")? else {
                return None;
            };
            objects.iter_mut().find_map(|object| {
                let Field::Object(object) = object else {
                    return None;
                };
                let Field::Object(values) = object.get_mut("values")? else {
                    return None;
                };
                values.get_mut(name)
            })
        })
        .unwrap()
}

fn remap_item_guids(field: &mut Field) {
    match field {
        Field::Guid(guid) if *guid >> 48 == 0x4000 => *guid |= 1 << 47,
        Field::List(fields) => fields.iter_mut().for_each(remap_item_guids),
        Field::Object(fields) => fields.values_mut().for_each(remap_item_guids),
        _ => (),
    }
}

#[test]
fn inventory_links_use_complete_guids_and_preserve_identity_between_slots() {
    let golden = wire_client::catalogue::get("vendor_transactions").unwrap();
    let mut candidate = golden.trace().clone();
    for (checkpoint, high) in [
        ("buy", "field_539"),
        ("sell", "field_625"),
        ("buyback", "field_537"),
        ("sell_again", "field_627"),
        ("buy_in_slot", "field_547"),
    ] {
        *word_mut(&mut candidate, checkpoint, high) = Field::Unsigned(0x4000_8000);
    }
    for packet in &mut candidate.packets {
        packet.fields.values_mut().for_each(remap_item_guids);
    }
    let comparison = compare(&golden, &candidate, &[]).unwrap();
    assert!(comparison.is_match(), "{comparison:#?}");
    *word_mut(&mut candidate, "sell_again", "field_626") = Field::Unsigned(46);
    assert!(compare(&golden, &candidate, &[])
        .unwrap()
        .differences
        .iter()
        .any(|difference| difference.rule == "guid_bijection"));
}

#[test]
fn item_link_type_zero_shape_and_high_word_consistency_remain_strict() {
    let golden = wire_client::catalogue::get("vendor_transactions").unwrap();
    for (checkpoint, name, value) in [
        ("sell", "field_536", Field::Unsigned(45)),
        ("sell_again", "field_627", Field::Unsigned(0x4000_8000)),
        ("buy", "field_539", Field::Unsigned(0xf130_0000)),
        ("buy", "field_539", Field::Unsigned(1u64 << 32)),
        ("buy", "field_539", Field::Signed(0x4000_0000)),
    ] {
        let mut candidate = golden.trace().clone();
        *word_mut(&mut candidate, checkpoint, name) = value;
        assert!(
            !compare(&golden, &candidate, &[]).unwrap().is_match(),
            "{checkpoint} {name}"
        );
    }
}

#[test]
fn strictness_detects_a_wrong_item_link_in_a_later_slot() {
    let golden = wire_client::catalogue::get("vendor_transactions").unwrap();
    let report = wire_client::strictness::analyze(&golden).unwrap();
    assert!(report
        .detected_mutations
        .iter()
        .any(|id| { id.ends_with("/field_objects.0.values.field_626/plus_one") }));
    assert!(report.unexplained_survivors.is_empty());
}

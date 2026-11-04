use std::collections::BTreeMap;
use wire_client::trace::{RecordedField, Sha256Digest};

fn field_mut<'a>(
    fields: &'a mut BTreeMap<String, RecordedField>,
    path: &[String],
) -> &'a mut RecordedField {
    let mut value = fields.get_mut(&path[0]).unwrap();
    for part in &path[1..] {
        value = match value {
            RecordedField::Object(fields) => fields.get_mut(part).unwrap(),
            RecordedField::List(items) => &mut items[part.parse::<usize>().unwrap()],
            _ => panic!("world-text path must reach a field"),
        };
    }
    value
}

#[test]
fn authored_world_text_has_only_data_differences() {
    let golden = wire_client::catalogue::get("quest_offers").unwrap();
    assert_eq!(golden.scenario().rules.data_fields.len(), 11);
    for rule in &golden.scenario().rules.data_fields {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.opcode == rule.opcode && packet.checkpoint != "opening")
            .unwrap();
        *field_mut(&mut packet.fields, &rule.path) =
            RecordedField::TextSha256(Sha256Digest::of(b"Locally authored fixture text"));
        let result = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
        assert_eq!(result.rule_difference_count(), 0, "{}", rule.name);
        assert_eq!(result.data_difference_count(), 1, "{}", rule.name);
    }
}

#[test]
fn text_presence_type_and_giver_identity_remain_rules() {
    let golden = wire_client::catalogue::get("quest_offers").unwrap();
    for opcode in [93, 392] {
        for replacement in [None, Some(RecordedField::Unsigned(0))] {
            let mut candidate = golden.trace().clone();
            let packet = candidate
                .packets
                .iter_mut()
                .find(|packet| packet.opcode == opcode && packet.checkpoint != "opening")
                .unwrap();
            if let Some(value) = replacement {
                packet.fields.insert("title".into(), value);
            } else {
                packet.fields.remove("title");
            }
            assert!(
                wire_client::compare::compare(&golden, &candidate, &[])
                    .unwrap()
                    .rule_difference_count()
                    > 0
            );
        }
    }
    let mut candidate = golden.trace().clone();
    let packet = candidate
        .packets
        .iter_mut()
        .find(|packet| packet.opcode == 392 && packet.checkpoint != "opening")
        .unwrap();
    packet
        .fields
        .insert("guid".into(), RecordedField::Guid(0xf13000000000ffff));
    assert!(
        wire_client::compare::compare(&golden, &candidate, &[])
            .unwrap()
            .rule_difference_count()
            > 0
    );
}

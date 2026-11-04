use std::collections::BTreeMap;
use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Scenario, Target};

#[test]
fn spell_requests_use_unit_target_mask_and_packed_guid() {
    let request = ClientPacket::CastSpell {
        target: Some(Target::Character),
        spell_id: 5504,
    };
    assert_eq!(
        request.encode(0x010203, &BTreeMap::new()).unwrap(),
        (302, vec![128, 21, 0, 0, 2, 0, 7, 3, 2, 1])
    );
    assert_eq!(
        ClientPacket::CancelCast { spell_id: 5504 }
            .encode(7, &BTreeMap::new())
            .unwrap(),
        (303, vec![128, 21, 0, 0])
    );
}

#[test]
fn spell_fixture_refuses_zero_overflow_and_duplicate_ids() {
    let text = include_str!("../scenarios/cast_and_interrupt.toml");
    assert!(Scenario::parse(text).is_ok());
    for id in [0, 65536] {
        assert!(
            Scenario::parse(&text.replacen("spell_id = 5504", &format!("spell_id = {id}"), 1))
                .is_err()
        );
    }
    let duplicate = text.replace(
        "[[steps]]\nid = \"begin\"",
        "[[fixtures]]\nverb = \"spell\"\nspell_id = 5504\n\n[[steps]]\nid = \"begin\"",
    );
    assert!(Scenario::parse(&duplicate).is_err());
}

#[test]
fn initial_spell_list_checks_spell_slots_and_cooldown_layouts() {
    let body = [0, 1, 0, 128, 21, 0, 0, 0, 0];
    let packet = decode(298, &body).unwrap();
    assert_eq!(
        packet.fields["spells"],
        Field::List(vec![Field::Object(BTreeMap::from([
            ("id".into(), Field::Unsigned(5504)),
            ("slot".into(), Field::Signed(0))
        ]))])
    );
    for end in 0..body.len() {
        assert!(decode(298, &body[..end]).is_err());
    }
}

#[test]
fn spell_failures_keep_result_reason_and_interrupted_caster() {
    let body = [128, 21, 0, 0, 2, 35];
    let packet = decode(304, &body).unwrap();
    assert_eq!(packet.fields["spell"], Field::Unsigned(5504));
    assert_eq!(packet.fields["reason"], Field::Unsigned(35));
    for end in 0..body.len() {
        assert!(decode(304, &body[..end]).is_err());
    }
    let mut body = 7u64.to_le_bytes().to_vec();
    body.extend(5504u32.to_le_bytes());
    assert_eq!(decode(678, &body).unwrap().fields["guid"], Field::Guid(7));
    for end in 0..body.len() {
        assert!(decode(678, &body[..end]).is_err());
    }
}

#[test]
fn self_effect_cast_omits_explicit_unit_target() {
    assert_eq!(
        ClientPacket::CastSpell {
            target: None,
            spell_id: 5504
        }
        .encode(7, &BTreeMap::new())
        .unwrap(),
        (302, vec![128, 21, 0, 0, 0, 0])
    );
}

#[test]
fn create_item_execute_log_checks_effect_and_item_entries() {
    let mut body = vec![1, 7];
    for word in [5504u32, 1, 24, 1, 5350] {
        body.extend(word.to_le_bytes());
    }
    let packet = decode(588, &body).unwrap();
    assert_eq!(packet.fields["caster"], Field::Guid(7));
    assert_eq!(packet.fields["spell"], Field::Unsigned(5504));
    for end in 0..body.len() {
        assert!(decode(588, &body[..end]).is_err());
    }
    body[10] = 2;
    assert!(decode(588, &body).is_err());
}

#[test]
fn interrupted_and_completed_casts_keep_actor_item_and_refusal_fields_strict() {
    use wire_client::trace::RecordedField;
    let golden = wire_client::catalogue::get("cast_and_interrupt").unwrap();
    assert!(wire_client::compare::compare(&golden, golden.trace(), &[])
        .unwrap()
        .is_match());
    for (checkpoint, opcode, field, value) in [
        ("interrupt", 304, "reason", RecordedField::Unsigned(10)),
        ("finish", 358, "count", RecordedField::Unsigned(3)),
        ("bad_target", 304, "reason", RecordedField::Unsigned(35)),
        ("finish", 305, "caster", RecordedField::Guid(999)),
    ] {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.checkpoint == checkpoint && packet.opcode == opcode)
            .unwrap();
        packet.fields.insert(field.into(), value);
        assert!(!wire_client::compare::compare(&golden, &candidate, &[])
            .unwrap()
            .is_match());
    }
}

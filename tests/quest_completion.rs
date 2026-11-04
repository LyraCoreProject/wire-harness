use std::collections::BTreeMap;
use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Target};

#[test]
fn completion_requests_encode_full_giver_and_quest() {
    let fixtures = BTreeMap::from([("innkeeper".into(), 9)]);
    for (request, opcode) in [
        (
            ClientPacket::QuestgiverComplete {
                target: Target::Fixture("innkeeper".into()),
                entry: 2158,
            },
            394,
        ),
        (
            ClientPacket::QuestgiverRequestReward {
                target: Target::Fixture("innkeeper".into()),
                entry: 2158,
            },
            396,
        ),
    ] {
        assert_eq!(
            request.encode(5, &fixtures).unwrap(),
            (opcode, vec![9, 0, 0, 0, 0, 0, 0, 0, 110, 8, 0, 0])
        );
    }
}

#[test]
fn completion_packets_decode_rewards_and_completion_flags() {
    for (opcode, words, field, expected) in [
        (
            397,
            vec![1u32, 0, 1, 4656, 5, 6402, 0, 0, 8, 0],
            "choices",
            Field::List(vec![Field::Object(BTreeMap::from([
                ("entry".into(), Field::Unsigned(4656)),
                ("count".into(), Field::Unsigned(5)),
                ("display".into(), Field::Unsigned(6402)),
            ]))]),
        ),
        (
            395,
            vec![0u32, 0, 0, 0, 0, 2, 0, 4, 8],
            "completion_2",
            Field::Unsigned(0),
        ),
    ] {
        let mut body = 9u64.to_le_bytes().to_vec();
        body.extend(2158u32.to_le_bytes());
        body.extend(b"Title\0Text\0");
        for word in words {
            body.extend(word.to_le_bytes());
        }
        assert_eq!(decode(opcode, &body).unwrap().fields[field], expected);
        for end in 0..body.len() {
            assert!(decode(opcode, &body[..end]).is_err());
        }
        body.push(0);
        assert!(decode(opcode, &body).is_err());
    }
}

#[test]
fn complete_and_incomplete_replies_bind_their_giver() {
    let golden = wire_client::catalogue::get("quest_completion").unwrap();
    assert!(wire_client::compare::compare(&golden, golden.trace(), &[])
        .unwrap()
        .is_match());
    let mut candidate = golden.trace().clone();
    for packet in candidate
        .packets
        .iter_mut()
        .filter(|packet| matches!(packet.opcode, 395 | 397))
    {
        packet.fields.insert(
            "guid".into(),
            wire_client::trace::RecordedField::Guid(0xf13000000000ffff),
        );
    }
    assert!(wire_client::compare::compare(&golden, &candidate, &[])
        .unwrap()
        .differences
        .iter()
        .any(|difference| difference.rule == "fixture_guid"));
}

#[test]
fn world_text_is_data_but_presence_and_type_stay_rules() {
    use wire_client::trace::{RecordedField, Sha256Digest};
    let golden = wire_client::catalogue::get("quest_completion").unwrap();
    for opcode in [395, 397] {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.opcode == opcode)
            .unwrap();
        packet.fields.insert(
            "title".into(),
            RecordedField::TextSha256(Sha256Digest::of(b"Authored fixture title")),
        );
        let result = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
        assert_eq!(result.rule_difference_count(), 0);
        assert_eq!(result.data_difference_count(), 1);
        for replacement in [None, Some(RecordedField::Unsigned(0))] {
            let mut candidate = golden.trace().clone();
            let packet = candidate
                .packets
                .iter_mut()
                .find(|packet| packet.opcode == opcode)
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
}

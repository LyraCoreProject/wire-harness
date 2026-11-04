use std::collections::BTreeMap;
use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Scenario, Target};

#[test]
fn reward_requests_encode_giver_quest_and_zero_based_choice() {
    let fixtures = BTreeMap::from([("innkeeper".into(), 9)]);
    for (request, opcode, suffix) in [
        (
            ClientPacket::QuestgiverAccept {
                target: Target::Fixture("innkeeper".into()),
                entry: 2158,
            },
            393,
            vec![],
        ),
        (
            ClientPacket::QuestgiverRequestReward {
                target: Target::Fixture("innkeeper".into()),
                entry: 2158,
            },
            396,
            vec![],
        ),
        (
            ClientPacket::QuestgiverChooseReward {
                target: Target::Fixture("innkeeper".into()),
                entry: 2158,
                choice: 1,
            },
            398,
            vec![1, 0, 0, 0],
        ),
    ] {
        let mut body = vec![9, 0, 0, 0, 0, 0, 0, 0, 110, 8, 0, 0];
        body.extend(suffix);
        assert_eq!(request.encode(5, &fixtures).unwrap(), (opcode, body));
        assert!(request.encode(5, &BTreeMap::new()).is_err());
    }
    assert!(Scenario::parse(include_str!("../scenarios/quest_reward.toml")).is_ok());
}

#[test]
fn quest_reward_packet_has_quest_xp_money_and_fixed_rewards() {
    let mut body = Vec::new();
    for word in [2158u32, 3, 110, 0, 1, 4656, 5] {
        body.extend(word.to_le_bytes());
    }
    let decoded = decode(401, &body).unwrap();
    assert_eq!(decoded.fields["entry"], Field::Unsigned(2158));
    assert_eq!(decoded.fields["xp"], Field::Unsigned(110));
    assert_eq!(
        decoded.fields["rewards"],
        Field::List(vec![Field::Object(BTreeMap::from([
            ("entry".into(), Field::Unsigned(4656)),
            ("count".into(), Field::Unsigned(5))
        ]))])
    );
    for end in 0..body.len() {
        assert!(decode(401, &body[..end]).is_err());
    }
    body.push(0);
    assert!(decode(401, &body).is_err());
}

#[test]
fn reward_reputation_and_xp_packets_keep_signed_standing_and_gain_kind() {
    let mut body = 1u32.to_le_bytes().to_vec();
    body.extend(19u32.to_le_bytes());
    body.extend((-25i32).to_le_bytes());
    let decoded = decode(292, &body).unwrap();
    assert_eq!(
        decoded.fields["standings"],
        Field::List(vec![Field::Object(BTreeMap::from([
            ("index".into(), Field::Unsigned(19)),
            ("standing".into(), Field::Signed(-25))
        ]))])
    );
    for end in 0..body.len() {
        assert!(decode(292, &body[..end]).is_err());
    }
    let mut body = 0u64.to_le_bytes().to_vec();
    body.extend(110u32.to_le_bytes());
    body.push(1);
    assert_eq!(
        decode(464, &body).unwrap().fields["xp"],
        Field::Unsigned(110)
    );
    for end in 0..body.len() {
        assert!(decode(464, &body[..end]).is_err());
    }
}

#[test]
fn reward_grants_and_refusal_state_remain_strict() {
    use wire_client::trace::RecordedField;
    let golden = wire_client::catalogue::get("quest_reward").unwrap();
    assert!(wire_client::compare::compare(&golden, golden.trace(), &[])
        .unwrap()
        .is_match());
    for (opcode, field) in [(358, "count"), (401, "xp")] {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.checkpoint == "turn_in" && packet.opcode == opcode)
            .unwrap();
        packet
            .fields
            .insert(field.into(), RecordedField::Unsigned(999));
        assert!(!wire_client::compare::compare(&golden, &candidate, &[])
            .unwrap()
            .is_match());
    }
    let mut candidate = golden.trace().clone();
    let mut update = candidate
        .packets
        .iter()
        .find(|packet| packet.checkpoint == "turn_in" && packet.opcode == 169
                && matches!(packet.fields.get("objects"), Some(RecordedField::List(objects)) if objects.iter().any(|object| matches!(object, RecordedField::Object(fields) if fields.get("guid") == Some(&RecordedField::Guid(candidate.header.character_guid))))))
        .unwrap()
        .clone();
    update.checkpoint = "invalid_choice".into();
    update.stream = wire_client::scenario::Stream::Broadcast("values".into());
    candidate.packets.push(update);
    assert!(!wire_client::compare::compare(&golden, &candidate, &[])
        .unwrap()
        .is_match());
}

#[test]
fn world_text_is_data_but_presence_and_type_stay_rules() {
    use wire_client::trace::{RecordedField, Sha256Digest};
    let golden = wire_client::catalogue::get("quest_reward").unwrap();
    let opcode = 397;
    {
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

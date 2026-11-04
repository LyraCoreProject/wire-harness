use std::collections::BTreeMap;

use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Target};

#[test]
fn quest_queries_encode_the_giver_and_entry() {
    let fixtures = BTreeMap::from([("giver".into(), 9)]);
    let request = ClientPacket::QuestgiverQuery {
        target: Target::Fixture("giver".into()),
        entry: 433,
    };
    assert_eq!(
        request.encode(7, &fixtures).unwrap(),
        (0x0186, vec![9, 0, 0, 0, 0, 0, 0, 0, 177, 1, 0, 0])
    );
    assert!(request.encode(7, &BTreeMap::new()).is_err());
    assert_eq!(
        ClientPacket::QuestQuery { entry: 433 }
            .encode(7, &fixtures)
            .unwrap(),
        (0x005c, vec![177, 1, 0, 0])
    );
}

#[test]
fn quest_details_decode_rewards_and_require_complete_bodies() {
    let mut body = 9u64.to_le_bytes().to_vec();
    body.extend(433u32.to_le_bytes());
    body.extend(b"Title\0Details\0Objectives\0");
    for word in [1u32, 1, 100, 2, 300, 0, 5, 0, 1, 6, 7] {
        body.extend(word.to_le_bytes());
    }
    let decoded = decode(0x0188, &body).unwrap();
    assert_eq!(decoded.fields["guid"], Field::Guid(9));
    assert_eq!(decoded.fields["entry"], Field::Unsigned(433));
    assert_eq!(decoded.fields["title"], Field::Text("Title".into()));
    assert_eq!(
        decoded.fields["choices"],
        Field::List(vec![Field::Object(BTreeMap::from([
            ("entry".into(), Field::Unsigned(100)),
            ("count".into(), Field::Unsigned(2)),
            ("display".into(), Field::Unsigned(300)),
        ]))])
    );
    for end in 0..body.len() {
        assert!(decode(0x0188, &body[..end]).is_err());
    }
    body.push(0);
    assert!(decode(0x0188, &body).is_err());
}

#[test]
fn quest_status_and_gossip_close_have_fixed_layouts() {
    let body = [9, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0];
    let decoded = decode(0x0183, &body).unwrap();
    assert_eq!(decoded.fields["guid"], Field::Guid(9));
    assert_eq!(decoded.fields["status"], Field::Unsigned(5));
    assert!(decode(0x0183, &body[..11]).is_err());
    assert!(decode(0x017e, &[]).is_ok());
    assert!(decode(0x017e, &[0]).is_err());
}

#[test]
fn quest_replies_name_the_giver_seen_during_opening() {
    let golden = wire_client::catalogue::get("quest_offers").unwrap();
    let mut candidate = golden.trace().clone();
    for packet in candidate
        .packets
        .iter_mut()
        .filter(|packet| packet.checkpoint != "opening" && matches!(packet.opcode, 387 | 392))
    {
        packet.fields.insert(
            "guid".into(),
            wire_client::trace::RecordedField::Guid(0xf130_0000_0000_0001),
        );
    }
    let result = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
    assert!(result
        .differences
        .iter()
        .any(|difference| difference.rule == "fixture_guid"));
}

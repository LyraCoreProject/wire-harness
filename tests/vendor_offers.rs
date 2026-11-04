use std::collections::BTreeMap;

use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Target};

#[test]
fn vendor_offer_requests_encode_guid_entry_and_count() {
    let fixtures = BTreeMap::from([("barkeep".into(), 9)]);
    assert_eq!(
        ClientPacket::ListInventory {
            target: Target::Fixture("barkeep".into())
        }
        .encode(5, &fixtures)
        .unwrap(),
        (415 - 1, 9u64.to_le_bytes().to_vec())
    );
    assert_eq!(
        ClientPacket::BuyItem {
            target: Target::Fixture("barkeep".into()),
            entry: 11325,
            count: 10
        }
        .encode(5, &fixtures)
        .unwrap(),
        (418, vec![9, 0, 0, 0, 0, 0, 0, 0, 61, 44, 0, 0, 10, 0])
    );
}

#[test]
fn vendor_list_decodes_stock_price_and_purchase_quantity() {
    let mut body = 9u64.to_le_bytes().to_vec();
    body.push(1);
    for word in [1u32, 2320, 4752, u32::MAX, 10, 0, 1] {
        body.extend(word.to_le_bytes());
    }
    let packet = decode(415, &body).unwrap();
    let Field::List(items) = &packet.fields["items"] else {
        panic!()
    };
    let Field::Object(item) = &items[0] else {
        panic!()
    };
    assert_eq!(item["stock"], Field::Unsigned(u64::from(u32::MAX)));
    assert_eq!(item["price"], Field::Unsigned(10));
    assert_eq!(item["buy_count"], Field::Unsigned(1));
    for end in 0..body.len() {
        assert!(decode(415, &body[..end]).is_err());
    }
    body.push(0);
    assert!(decode(415, &body).is_err());
    assert!(decode(415, &[9, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_ok());
}

#[test]
fn vendor_refusals_and_item_push_have_exact_layouts() {
    for (opcode, suffix, field, value) in [
        (421, vec![16, 9, 0, 0, 2], "reason", 2),
        (
            420,
            vec![1, 0, 0, 0, 255, 255, 255, 255, 1, 0, 0, 0],
            "count",
            1,
        ),
    ] {
        let mut body = 9u64.to_le_bytes().to_vec();
        body.extend(suffix);
        assert_eq!(
            decode(opcode, &body).unwrap().fields[field],
            Field::Unsigned(value)
        );
        for end in 0..body.len() {
            assert!(decode(opcode, &body[..end]).is_err());
        }
        body.push(0);
        assert!(decode(opcode, &body).is_err());
    }
    let mut body = 5u64.to_le_bytes().to_vec();
    for word in [1u32, 0, 1] {
        body.extend(word.to_le_bytes());
    }
    body.push(255);
    for word in [30u32, 2320, 0, 0, 1] {
        body.extend(word.to_le_bytes());
    }
    assert_eq!(
        decode(358, &body).unwrap().fields["slot"],
        Field::Unsigned(30)
    );
    for end in 0..body.len() {
        assert!(decode(358, &body[..end]).is_err());
    }
}

#[test]
fn creature_chat_keeps_sender_identity_and_checks_text_lengths() {
    let mut body = vec![0x0b, 0, 0, 0, 0];
    body.extend(0xf130000000000007u64.to_le_bytes());
    body.extend(5u32.to_le_bytes());
    body.extend(b"Name\0");
    body.extend(0u64.to_le_bytes());
    body.extend(6u32.to_le_bytes());
    body.extend(b"Hello\0");
    body.push(0);
    let packet = decode(150, &body).unwrap();
    assert_eq!(packet.fields["guid"], Field::Guid(0xf130000000000007));
    assert_eq!(packet.fields["text"], Field::Text("Hello".into()));
    for end in 0..body.len() {
        assert!(decode(150, &body[..end]).is_err());
    }
    body[13] = 4;
    assert!(decode(150, &body).is_err());
}

#[test]
fn offer_price_stock_and_condition_membership_remain_strict() {
    use wire_client::trace::RecordedField;
    let golden = wire_client::catalogue::get("vendor_offers").unwrap();
    for (checkpoint, index, field) in [
        ("condition_list", 0, "entry"),
        ("stock_list", 1, "price"),
        ("restocked_list", 1, "stock"),
    ] {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.checkpoint == checkpoint && packet.opcode == 415)
            .unwrap();
        let RecordedField::List(items) = packet.fields.get_mut("items").unwrap() else {
            panic!()
        };
        let RecordedField::Object(item) = &mut items[index] else {
            panic!()
        };
        item.insert(field.into(), RecordedField::Unsigned(999));
        assert!(!wire_client::compare::compare(&golden, &candidate, &[])
            .unwrap()
            .is_match());
    }
}

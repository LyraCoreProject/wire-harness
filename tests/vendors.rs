use std::collections::BTreeMap;

use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Scenario, Target};

#[test]
fn vendor_requests_keep_full_guids_and_byte_counts() {
    let fixtures = BTreeMap::from([("vendor".into(), 9), ("linen".into(), 0x4000000000000007)]);
    let vendor = || Target::Fixture("vendor".into());
    let requests = [
        (
            ClientPacket::BuyItem {
                target: vendor(),
                entry: 2320,
                count: 2,
            },
            0x01a2,
            vec![16, 9, 0, 0, 2, 0],
        ),
        (
            ClientPacket::BuyItemInSlot {
                target: vendor(),
                entry: 2320,
                slot: 30,
                count: 1,
            },
            0x01a3,
            vec![16, 9, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 30, 1],
        ),
        (
            ClientPacket::SellItem {
                target: vendor(),
                item: "linen".into(),
                count: 0,
            },
            0x01a0,
            vec![7, 0, 0, 0, 0, 0, 0, 64, 0],
        ),
        (
            ClientPacket::BuybackItem {
                target: vendor(),
                slot: 74,
            },
            0x0290,
            vec![74, 0, 0, 0],
        ),
    ];
    for (request, opcode, suffix) in requests {
        let mut expected = 9u64.to_le_bytes().to_vec();
        expected.extend(suffix);
        assert_eq!(request.encode(5, &fixtures).unwrap(), (opcode, expected));
        assert!(request.encode(5, &BTreeMap::new()).is_err());
    }
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
        (417, vec![7, 0, 0, 0, 0, 0, 0, 64, 2], "reason", 2),
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
fn item_fixtures_refuse_ambiguous_bindings_and_invalid_counts() {
    let text = include_str!("../scenarios/vendor_transactions.toml");
    assert!(Scenario::parse(text).is_ok());
    for invalid in [
        text.replace("count = 2", "count = 0"),
        text.replace("count = 2", "count = 256"),
        text.replace("id = \"linen\"", "id = \"vendor\""),
        text.replace("item = \"linen\"", "item = \"missing\""),
    ] {
        assert!(Scenario::parse(&invalid).is_err());
    }
}

#[test]
fn vendor_and_sold_item_replies_use_their_opening_fixtures() {
    let golden = wire_client::catalogue::get("vendor_transactions").unwrap();
    for field in ["guid", "item"] {
        let mut candidate = golden.trace().clone();
        let packet = candidate
            .packets
            .iter_mut()
            .find(|packet| packet.opcode == 417)
            .unwrap();
        packet.fields.insert(
            field.into(),
            wire_client::trace::RecordedField::Guid(if field == "item" {
                0x400000000000ffff
            } else {
                0xf13000000000ffff
            }),
        );
        let result = wire_client::compare::compare(&golden, &candidate, &[]).unwrap();
        assert!(result
            .differences
            .iter()
            .any(|difference| difference.rule == "fixture_guid"));
    }
}

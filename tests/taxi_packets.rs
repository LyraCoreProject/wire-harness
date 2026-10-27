use std::collections::BTreeMap;

use wire_client::decode::{decode, Field};
use wire_client::scenario::{ClientPacket, Target};

fn packed_guid(value: u64) -> Vec<u8> {
    let bytes = value.to_le_bytes();
    let mut result = vec![0];
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != 0 {
            result[0] |= 1 << i;
            result.push(*byte);
        }
    }
    result
}

fn movement(kind: u8) -> Vec<u8> {
    let mut bytes = packed_guid(9);
    for coordinate in [1.0f32, 2.0, 3.0] {
        bytes.extend(coordinate.to_le_bytes());
    }
    bytes.extend(5u32.to_le_bytes());
    bytes.push(kind);
    if kind != 1 {
        bytes.extend(0x300u32.to_le_bytes());
        bytes.extend(2000u32.to_le_bytes());
        bytes.extend(2u32.to_le_bytes());
        for coordinate in [4.0f32, 5.0, 6.0, 7.0, 8.0, 9.0] {
            bytes.extend(coordinate.to_le_bytes());
        }
    }
    bytes
}

#[test]
fn taxi_requests_use_full_guids_and_little_endian_node_ids() {
    let target = Target::Fixture("master".into());
    let fixtures = BTreeMap::from([("master".into(), 0xf130_0001_6000_0017)]);
    let guid = 0xf130_0001_6000_0017u64.to_le_bytes().to_vec();
    assert_eq!(
        ClientPacket::TaxiNodeStatusQuery {
            target: target.clone()
        }
        .encode(7, &fixtures)
        .unwrap(),
        (0x1aa, guid.clone())
    );
    assert_eq!(
        ClientPacket::TaxiQueryAvailableNodes {
            target: target.clone()
        }
        .encode(7, &fixtures)
        .unwrap(),
        (0x1ac, guid.clone())
    );
    let mut activation = guid;
    activation.extend([2, 0, 0, 0, 4, 0, 0, 0]);
    let packet = ClientPacket::ActivateTaxi {
        target,
        source: 2,
        destination: 4,
    };
    assert_eq!(packet.encode(7, &fixtures).unwrap(), (0x1ad, activation));
    assert!(packet.encode(7, &BTreeMap::new()).is_err());
}

#[test]
fn taxi_status_menu_and_refusal_keep_exact_values() {
    let mut status = 17u64.to_le_bytes().to_vec();
    status.push(1);
    let decoded = decode(0x1ab, &status).unwrap();
    assert_eq!(decoded.fields["guid"], Field::Guid(17));
    assert_eq!(decoded.fields["known"], Field::Unsigned(1));
    let mut menu = 1u32.to_le_bytes().to_vec();
    menu.extend(17u64.to_le_bytes());
    menu.extend(2u32.to_le_bytes());
    menu.extend(10u32.to_le_bytes());
    menu.extend([0; 28]);
    let decoded = decode(0x1a9, &menu).unwrap();
    assert_eq!(decoded.fields["node"], Field::Unsigned(2));
    let Field::List(mask) = &decoded.fields["known_nodes"] else {
        panic!()
    };
    assert_eq!(mask.len(), 8);
    assert_eq!(mask[0], Field::Unsigned(10));
    assert_eq!(
        decode(0x1ae, &[3, 0, 0, 0]).unwrap().fields["result"],
        Field::Unsigned(3)
    );
    for (opcode, bytes) in [
        (0x1ab, status),
        (0x1a9, menu),
        (0xdd, movement(0)),
        (0xdd, movement(1)),
    ] {
        for cut in 0..bytes.len() {
            assert!(
                decode(opcode, &bytes[..cut]).is_err(),
                "opcode {opcode} cut {cut}"
            );
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(decode(opcode, &extra).is_err());
    }
}

#[test]
fn flight_spline_keeps_duration_vertices_flags_and_server_local_id() {
    let packet = decode(0xdd, &movement(0)).unwrap();
    assert_eq!(packet.fields["guid"], Field::Guid(9));
    assert_eq!(packet.fields["spline_id"], Field::Unsigned(5));
    assert_eq!(packet.fields["duration"], Field::TimeMs(2000));
    assert_eq!(packet.fields["flags"], Field::Unsigned(0x300));
    let Field::List(points) = &packet.fields["points"] else {
        panic!()
    };
    assert_eq!(points.len(), 2);
    assert_eq!(
        points[1],
        Field::Object(BTreeMap::from([
            ("x".into(), Field::Float(7.0)),
            ("y".into(), Field::Float(8.0)),
            ("z".into(), Field::Float(9.0))
        ]))
    );
    let landing = decode(0xdd, &movement(1)).unwrap();
    assert_eq!(landing.fields["kind"], Field::Unsigned(1));
    assert!(!landing.fields.contains_key("duration"));
    let mut invalid = movement(0);
    invalid[2..6].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(decode(0xdd, &invalid).is_err());
}

#[test]
fn complete_values_decode_rejects_a_truncated_tail() {
    let mut bytes = vec![1, 0, 0, 0, 0, 0, 1, 9, 2];
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((1u32 << 14).to_le_bytes());
    bytes.extend(0x100004u32.to_le_bytes());
    let packet = decode(0xa9, &bytes).unwrap();
    let Field::List(objects) = &packet.fields["objects"] else {
        panic!()
    };
    let Field::Object(object) = &objects[0] else {
        panic!()
    };
    assert_eq!(object["guid"], Field::Guid(9));
    assert_eq!(
        object["values"],
        Field::Object(BTreeMap::from([(
            "field_46".into(),
            Field::Unsigned(0x100004)
        )]))
    );
    for cut in 0..bytes.len() {
        assert!(decode(0xa9, &bytes[..cut]).is_err());
    }
    use std::io::Write;
    let mut compressed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    compressed.write_all(&bytes).unwrap();
    let mut envelope = (bytes.len() as u32).to_le_bytes().to_vec();
    envelope.extend(compressed.finish().unwrap());
    assert_eq!(decode(0x1f6, &envelope).unwrap(), packet);
    envelope.push(0);
    assert!(decode(0x1f6, &envelope).is_err());
}

#[test]
fn landing_and_flight_controls_reject_incomplete_or_extra_fields() {
    let mut landing = packed_guid(9);
    landing.extend(0u32.to_le_bytes());
    landing.extend(0u32.to_le_bytes());
    landing.extend(u32::MAX.to_le_bytes());
    for coordinate in [-10.0f32, 20.0, 30.0, 1.5] {
        landing.extend(coordinate.to_le_bytes());
    }
    landing.extend(0u32.to_le_bytes());
    let decoded = decode(199, &landing).unwrap();
    assert_eq!(decoded.fields["guid"], Field::Guid(9));
    assert_eq!(decoded.fields["counter"], Field::Unsigned(0));
    assert_eq!(
        decoded.fields["timestamp"],
        Field::Unsigned(u32::MAX.into())
    );
    assert_eq!(decoded.fields["orientation"], Field::Float(1.5));
    assert_eq!(decoded.fields["fall_time"], Field::TimeMs(0));
    for (opcode, body) in [
        (199, landing),
        (366, vec![0; 4]),
        (367, vec![0; 4]),
        (334, vec![]),
    ] {
        assert!(decode(opcode, &body).is_ok());
        for cut in 0..body.len() {
            assert!(decode(opcode, &body[..cut]).is_err());
        }
        let mut extra = body;
        extra.push(0);
        assert!(decode(opcode, &extra).is_err());
    }
}

#[test]
fn nearby_spell_targets_retain_passenger_identity_and_scrubbable_text() {
    let mut body = vec![1, 9, 1, 9];
    body.extend(123u32.to_le_bytes());
    body.extend(0u16.to_le_bytes());
    body.extend(1000u32.to_le_bytes());
    body.extend(0x2002u16.to_le_bytes());
    body.extend(packed_guid(7));
    body.extend(b"private target text\0");
    let packet = decode(305, &body).unwrap();
    assert_eq!(packet.fields["caster"], Field::Guid(9));
    let Field::Object(targets) = &packet.fields["targets"] else {
        panic!()
    };
    assert_eq!(targets["unit"], Field::Guid(7));
    assert_eq!(targets["text"], Field::Text("private target text".into()));
    for cut in 0..body.len() {
        assert!(decode(305, &body[..cut]).is_err());
    }
}

#[test]
fn combat_stop_and_despawn_keep_their_subject_guids() {
    let speed = decode(769, &[1, 9, 0, 0, 32, 64]).unwrap();
    assert_eq!(speed.fields["guid"], Field::Guid(9));
    assert_eq!(speed.fields["speed"], Field::Float(2.5));
    let stop = decode(324, &[1, 9, 1, 7, 1, 0, 0, 0]).unwrap();
    assert_eq!(stop.fields["attacker"], Field::Guid(9));
    assert_eq!(stop.fields["victim"], Field::Guid(7));
    assert_eq!(stop.fields["dead"], Field::Unsigned(1));
    let despawn = decode(533, &9u64.to_le_bytes()).unwrap();
    assert_eq!(despawn.fields["guid"], Field::Guid(9));
    assert!(decode(324, &[1, 9, 1, 7, 1]).is_err());
    assert!(decode(533, &[1, 9]).is_err());
}

#[test]
fn zone_world_states_decode_map_zone_and_signed_state_values() {
    let body = [
        0, 0, 0, 0, 12, 0, 0, 0, 1, 0, 42, 0, 0, 0, 255, 255, 255, 255,
    ];
    let packet = decode(706, &body).unwrap();
    assert_eq!(packet.fields["map"], Field::Unsigned(0));
    assert_eq!(packet.fields["zone"], Field::Unsigned(12));
    assert_eq!(
        packet.fields["states"],
        Field::List(vec![Field::Object(BTreeMap::from([
            ("id".into(), Field::Unsigned(42)),
            ("value".into(), Field::Signed(-1)),
        ]))])
    );
    for end in 0..body.len() {
        assert!(decode(706, &body[..end]).is_err());
    }
    assert!(decode(706, &[body.as_slice(), &[0]].concat()).is_err());
}

#[test]
fn zone_weather_decodes_values_and_rejects_malformed_fields() {
    let body = [1, 0, 0, 0, 0, 0, 0, 63, 7, 0, 0, 0, 1];
    let packet = decode(756, &body).unwrap();
    assert_eq!(
        packet.fields,
        BTreeMap::from([
            ("weather_type".into(), Field::Unsigned(1)),
            ("grade".into(), Field::Float(0.5)),
            ("sound".into(), Field::Unsigned(7)),
            ("instant".into(), Field::Unsigned(1)),
        ])
    );
    for end in 0..body.len() {
        assert!(decode(756, &body[..end]).is_err());
    }
    assert!(decode(756, &[body.as_slice(), &[0]].concat()).is_err());
    let mut invalid = body;
    invalid[12] = 2;
    assert!(decode(756, &invalid).is_err());
    invalid = body;
    invalid[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(decode(756, &invalid).is_err());
}

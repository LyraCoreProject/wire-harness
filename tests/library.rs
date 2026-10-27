use wire_client::decode::{decode, Field};
use wire_client::Endpoints;

#[test]
fn independent_endpoints_do_not_use_process_configuration() {
    let first = Endpoints::new("127.0.0.1", 3724, Some(8085));
    let second = Endpoints::new("127.0.0.2", 13724, None);
    assert_eq!(first.world_addr("ignored:1234"), "127.0.0.1:8085");
    assert_eq!(second.world_addr("advertised:18085"), "advertised:18085");
}

#[test]
fn pong_decodes_at_the_plaintext_boundary() {
    let packet = decode(0x01dd, &[0x78, 0x56, 0x34, 0x12]).unwrap();
    assert_eq!(packet.fields["sequence"], Field::Unsigned(0x12345678));
}

#[test]
fn name_response_keeps_text_separate_from_guids_and_numbers() {
    let packet = decode(
        0x0051,
        &[
            7, 0, 0, 0, 0, 0, 0, 0, b'A', 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
        ],
    )
    .unwrap();
    assert_eq!(packet.fields["guid"], Field::Guid(7));
    assert_eq!(packet.fields["name"], Field::Text("A".into()));
    assert_eq!(packet.fields["realm"], Field::Text(String::new()));
}

#[test]
fn malformed_and_unknown_packets_fail_without_raw_bytes_in_errors() {
    for (opcode, bytes) in [
        (0xffff, &b"private text"[..]),
        (0x01dd, &[1, 2][..]),
        (0x01dd, &[0, 0, 0, 0, 1][..]),
        (0x0063, &[51, 0, 0, 0, 51, 0, 0, 0][..]),
    ] {
        let error = decode(opcode, bytes).unwrap_err().to_string();
        assert!(!error.contains("private text"));
    }
}

#[test]
fn timestamps_use_milliseconds_and_rolls_keep_their_guid() {
    assert_eq!(
        decode(0x01cf, &[2, 0, 0, 0]).unwrap().fields["time"],
        Field::TimeMs(2000)
    );
    let packet = decode(
        0x01fb,
        &[1, 0, 0, 0, 6, 0, 0, 0, 4, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap();
    assert_eq!(packet.fields["result"], Field::Unsigned(4));
    assert_eq!(packet.fields["guid"], Field::Guid(9));
}

#[test]
fn positions_keep_their_values_and_refuse_nan() {
    let mut body = vec![0; 20];
    body[4..8].copy_from_slice(&1.5f32.to_le_bytes());
    assert_eq!(
        decode(0x0236, &body).unwrap().fields["x"],
        Field::Float(1.5)
    );
    body[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(decode(0x0236, &body).is_err());
}

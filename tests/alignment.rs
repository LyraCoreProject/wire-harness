use std::collections::BTreeMap;
use std::time::Instant;

use wire_client::compare::compare;
use wire_client::scenario::Stream;
use wire_client::trace::{
    GoldenTrace, Header, Provenance, RecordedField as Field, RecordedPacket, Sha256Digest, Trace,
};

fn measure(distinct_subjects: bool) {
    let text = include_str!("../scenarios/connection_identity.toml")
        .split("[[steps]]")
        .next()
        .unwrap();
    let text = format!("{text}\n[[steps]]\nid='query'\nsend={{opcode='CMSG_PING',sequence=7,latency_ms=0}}\ncheckpoint={{window_ms=1,replies=[477],broadcasts=[{{opcode=507,stream='updates'}}]}}\n");
    let header = Header::golden(
        &text,
        Provenance {
            server: "reference".into(),
            binary_release: "test".into(),
            binary_sha256: Sha256Digest::of(b"test"),
            source_commit: "a".repeat(40),
            database_commit: "b".repeat(40),
        },
        1000,
    )
    .unwrap();
    let make = |offset| {
        let mut packets = vec![RecordedPacket {
            character: None,
            received_after_ms: Some(0),
            checkpoint: "query".into(),
            stream: Stream::Reply("query".into()),
            opcode: 477,
            fields: BTreeMap::from([(
                "subjects".into(),
                Field::List((1..=1000).map(|id| Field::Guid(id + offset)).collect()),
            )]),
        }];
        packets.extend((0..1000).map(|index| {
            let mut fields: BTreeMap<_, _> = (0..32)
                .map(|field| {
                    (
                        format!("field_{field}"),
                        Field::Unsigned(index * 32 + field),
                    )
                })
                .collect();
            fields.insert(
                "guid".into(),
                Field::Guid(offset + if distinct_subjects { index + 1 } else { 1 }),
            );
            RecordedPacket {
                character: None,
                received_after_ms: Some(index),
                checkpoint: "query".into(),
                stream: Stream::Broadcast("updates".into()),
                opcode: 507,
                fields,
            }
        }));
        Trace {
            header: header.clone(),
            packets,
        }
    };
    let expected = GoldenTrace::new(&text, make(0)).unwrap();
    let mut actual = make(1000);
    actual.packets[801]
        .fields
        .insert("field_0".into(), Field::Unsigned(99999));
    actual.packets.remove(501);
    let runs = if cfg!(debug_assertions) { 1 } else { 5 };
    for _ in 0..runs {
        let start = Instant::now();
        let report = compare(&expected, &actual, &[]).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(report.differences.len(), 2);
        assert_eq!(report.differences[0].rule, "missing_packet");
        assert_eq!(report.differences[0].packet_index, Some(500));
        assert_eq!(report.differences[1].rule, "exact_field");
        assert_eq!(report.differences[1].packet_index, Some(800));
        assert_eq!(report.differences[1].field_path, ["field_0"]);
        eprintln!(
            "alignment_1000_packets distinct_subjects={distinct_subjects}: {:.3} ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }
}

#[test]
fn alignment_1000_packets_with_mapped_subjects() {
    measure(true);
}

#[test]
fn alignment_1000_packets_with_a_repeated_key() {
    measure(false);
}

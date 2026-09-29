//! Logical-message stream ownership: fragmentation must not change envelope
//! boundaries, and malformed continuations must fail rather than allocate/reorder.
use snap_transport::{Command, Invocation, Response, binary, json};

#[tokio::test]
async fn segmented_envelopes_preserve_order_and_reject_invalid_continuations() {
    let response = Response::Notification {
        operation: "large".into(),
        input: json!({"text":"x".repeat(200000),"exact":u64::MAX}),
    };
    let encoded = binary::response(&response, false, None).unwrap();
    let mut offset = 0;
    let mut starts = Vec::new();
    while offset < encoded.len() {
        starts.push(offset);
        let (kind, size) =
            binary::header(encoded[offset..offset + 12].try_into().unwrap()).unwrap();
        assert_eq!(kind, binary::SEGMENT);
        assert!(size <= 65536);
        offset += 12 + size;
    }
    assert!(starts.len() > 2);
    let mut stream = encoded.as_slice();
    assert_eq!(
        snap_transport_native::read_response(&mut stream)
            .await
            .unwrap(),
        (response, None)
    );
    assert!(stream.is_empty());
    let command = Command::Invoke(Invocation {
        id: 7,
        operation: "large.input".into(),
        input: json!("y".repeat(200000)),
    });
    let mut commands = binary::command(&command).unwrap();
    commands.extend(binary::command(&Command::Close).unwrap());
    let mut stream = commands.as_slice();
    let Some(Command::Invoke(received)) = snap_transport_native::read_command(&mut stream)
        .await
        .unwrap()
    else {
        panic!("Missing invocation");
    };
    assert_eq!(received.id, 7);
    assert_eq!(received.operation, "large.input");
    assert_eq!(received.input, json!("y".repeat(200000)));
    assert!(matches!(
        snap_transport_native::read_command(&mut stream)
            .await
            .unwrap(),
        Some(Command::Close)
    ));
    assert!(
        snap_transport_native::read_command(&mut stream)
            .await
            .unwrap()
            .is_none()
    );

    for defect in 0..6 {
        let mut bad = encoded.clone();
        match defect {
            0 => bad[16..20].copy_from_slice(&1u32.to_be_bytes()), // first offset
            1 => bad[12..16]
                .copy_from_slice(&((binary::LOGICAL_MESSAGE_LIMIT + 1) as u32).to_be_bytes()),
            2 => bad[starts[1] + 16..starts[1] + 20].copy_from_slice(&0u32.to_be_bytes()), // repeated bytes
            3 => bad[starts[1] + 5] = binary::MESSAGE, // interleaving is forbidden
            4 => bad[starts[1] + 12..starts[1] + 16].copy_from_slice(&200001u32.to_be_bytes()), // inconsistent total
            _ => {
                bad.pop();
            }                                       // truncated final payload
        }
        assert!(
            snap_transport_native::read_response(&mut bad.as_slice())
                .await
                .is_err(),
            "defect {defect}"
        );
    }
    let short = b"SNAP\x01\x03\x00\x00\x00\x00\x00\x08\x00\x02\x00\x00\x00\x00\x00\x00";
    assert!(
        snap_transport_native::read_response(&mut short.as_slice())
            .await
            .is_err()
    );
}

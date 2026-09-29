use snap_transport::{Command, Response, binary, json};

#[test]
fn v1_independent_frames_and_bounds() {
    // Independent CBOR map, not a value produced by the encoder under test.
    let body = b"\xa2\x66bearer\x61x\x69client_id\x61y";
    let mut frame = b"SNAP\x01\x01\x00\x00\x00\x00\x00\x16".to_vec();
    frame.extend_from_slice(body);
    let (kind, length) = binary::header(frame[..12].try_into().unwrap()).unwrap();
    assert_eq!(length, body.len());
    assert!(
        matches!(binary::read_command(kind,body).unwrap(), Command::Connect { bearer, client_id } if bearer == "x" && client_id == "y")
    );
    let command = Command::Connect {
        bearer: "x".into(),
        client_id: "y".into(),
    };
    assert_eq!(binary::command(&command).unwrap(), frame);
    for (index, value) in [(0, b'X'), (4, 2), (5, 3), (6, 1)] {
        let mut header: [u8; 12] = frame[..12].try_into().unwrap();
        header[index] = value;
        assert!(binary::header(&header).is_err());
    }
    let mut header: [u8; 12] = frame[..12].try_into().unwrap();
    header[8..].copy_from_slice(&4097u32.to_be_bytes());
    assert!(binary::header(&header).is_err());
    header[5] = 2;
    header[8..].copy_from_slice(&65537u32.to_be_bytes());
    assert!(binary::header(&header).is_err());
    let mut trailing = body.to_vec();
    trailing.push(0xf6);
    assert!(binary::read_command(1, &trailing).is_err());
    assert!(binary::read_command(2, &serde_cbor_connect()).is_err());
}
fn serde_cbor_connect() -> Vec<u8> {
    // A CONNECT wrapped in a MESSAGE must never authenticate.
    b"\xa1\x67Connect\xa2\x66bearer\x61x\x69client_id\x61y".to_vec()
}

#[test]
fn exact_numbers_and_connect_reply_metadata() {
    let response = Response::Notification {
        operation: "probe".into(),
        input: json!({"signed":i64::MIN,"unsigned":u64::MAX,"bytes":"x".repeat(4096)}),
    };
    let frame = binary::response(&response, false, 0).unwrap();
    assert_eq!(binary::read_response(2, &frame[12..]).unwrap().0, response);
    let frame = binary::response(&Response::Attached { resumed: true }, true, 1800000).unwrap();
    assert_eq!(
        binary::read_response(1, &frame[12..]).unwrap(),
        (Response::Attached { resumed: true }, Some(1800000))
    );
    assert!(binary::read_response(2, &frame[12..]).is_err());
}

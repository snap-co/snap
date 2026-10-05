use sha2::{Digest, Sha256};
use snap_store::*;

fn catalog() -> Catalog {
    Catalog::new(vec![Table {
        name: "typed.records".into(),
        columns: vec![
            Column {
                name: "id".into(),
                kind: Kind::Integer,
            },
            Column {
                name: "name".into(),
                kind: Kind::Text,
            },
            Column {
                name: "payload".into(),
                kind: Kind::Bytes,
            },
        ],
        primary: vec!["id".into()],
        indexes: vec![],
        foreign: vec![],
    }])
    .unwrap()
}

// Independently authored version-1 wire fixture. The schema JSON is a literal,
// not serialized by the code under test; instruction operands are literal bytes.
fn wire(frames: &[&[u8]]) -> Vec<u8> {
    let schema = br#"{"tables":[{"name":"typed.records","columns":[{"name":"id","kind":"integer"},{"name":"name","kind":"text"},{"name":"payload","kind":"bytes"}],"primary":["id"],"indexes":[],"foreign":[]}]}"#;
    let mut bytes = b"SNAPMUT\0\x01\x00".to_vec();
    bytes.extend_from_slice(&Sha256::digest(schema));
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    for frame in frames {
        bytes.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        bytes.extend_from_slice(frame);
    }
    let length = bytes.len() as u32;
    bytes[42..46].copy_from_slice(&length.to_le_bytes());
    bytes
}

fn fixture() -> Vec<u8> {
    wire(&[
        b"\x01\x0d\0\0\0typed.records\x03\0\0\0\x02\0\0\0id\x02\0\0\0\0\0\0\0\x80\x04\0\0\0name\x01\x04\0\0\0a\0\xc3\xa9\x07\0\0\0payload\x03\x02\0\0\0\0\xff".as_slice(),
        b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x01\0\0\0\x04\0\0\0name\x01\x06\0\0\0secret".as_slice(),
        b"\x03\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80".as_slice(),
    ])
}

#[test]
fn authoring_and_backend_receive_the_defined_binary_program_with_partial_updates() {
    struct Sink;
    impl Backend for Sink {
        fn load(&mut self, _: &Table) -> Result<Rows, Error> {
            Ok(vec![])
        }
        fn commit(&mut self, program: &Program) -> Result<(), CommitError> {
            assert_eq!(program.as_bytes(), fixture());
            Ok(())
        }
    }
    let mut store = Store::new(catalog(), Sink).unwrap();
    let committed = store
        .run("typed mutations", |tx| {
            tx.insert(
                "typed.records",
                Row::from([
                    ("id".into(), i64::MIN.into()),
                    ("name".into(), "a\0é".into()),
                    ("payload".into(), Value::Bytes(vec![0, 255])),
                ]),
            )?;
            tx.update(
                "typed.records",
                &[i64::MIN.into()],
                Row::from([("name".into(), "secret".into())]),
            )?;
            assert_eq!(
                tx.get("typed.records", &[i64::MIN.into()])?,
                Some(Row::from([
                    ("id".into(), i64::MIN.into()),
                    ("name".into(), "secret".into()),
                    ("payload".into(), Value::Bytes(vec![0, 255])),
                ]))
            );
            tx.delete("typed.records", &[i64::MIN.into()])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(committed.program.as_bytes(), fixture());
    assert!(committed.changes.is_empty());
    let decoded = Program::from_bytes(&catalog(), &fixture()).unwrap();
    assert_eq!(
        decoded.instructions().collect::<Vec<_>>(),
        vec![
            Instruction::Insert {
                table: "typed.records".into(),
                row: Row::from([
                    ("id".into(), i64::MIN.into()),
                    ("name".into(), "a\0é".into()),
                    ("payload".into(), Value::Bytes(vec![0, 255])),
                ])
            },
            Instruction::Update {
                table: "typed.records".into(),
                key: vec![i64::MIN.into()],
                changes: Row::from([("name".into(), "secret".into())])
            },
            Instruction::Delete {
                table: "typed.records".into(),
                key: vec![i64::MIN.into()]
            },
        ]
    );
    assert!(!format!("{decoded:?}").contains("secret"));
}

#[test]
fn invalid_programs_are_rejected_before_execution() {
    let valid = fixture();
    for end in 0..valid.len() {
        assert!(
            matches!(
                Program::from_bytes(&catalog(), &valid[..end]),
                Err(Error::Invalid)
            ),
            "prefix {end}"
        );
    }
    for (label, offset, value) in [
        ("magic", 0, b'X'),
        ("version", 8, 2),
        ("schema", 10, 0),
        ("envelope length", 42, 0),
        ("instruction count", 46, 0),
        ("frame length", 50, 0),
        ("opcode", 54, 255),
        ("table", 59, b'X'),
        ("field type", 82, 255),
        ("field name", 80, b'X'),
    ] {
        let mut bytes = valid.clone();
        assert_ne!(bytes[offset], value, "{label} must change input");
        bytes[offset] = value;
        assert!(
            matches!(Program::from_bytes(&catalog(), &bytes), Err(Error::Invalid)),
            "{label}"
        );
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    let length = trailing.len() as u32;
    trailing[42..46].copy_from_slice(&length.to_le_bytes());
    assert!(Program::from_bytes(&catalog(), &trailing).is_err());
    let mut schema = catalog();
    schema.tables[0].columns.push(Column {
        name: "other".into(),
        kind: Kind::Integer,
    });
    assert!(Program::from_bytes(&schema, &valid).is_err());
    for (label, frame) in [
        ("primary key update", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x01\0\0\0\x02\0\0\0id\x02\0\0\0\0\0\0\0\0".as_slice()),
        ("wrong key type", b"\x03\x0d\0\0\0typed.records\x01\0\0\0\x01\0\0\0\0".as_slice()),
        ("missing key", b"\x03\x0d\0\0\0typed.records\0\0\0\0".as_slice()),
        ("empty update", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\0\0\0\0".as_slice()),
        ("wrong field type", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x01\0\0\0\x04\0\0\0name\x02\0\0\0\0\0\0\0\0".as_slice()),
        ("unknown field", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x01\0\0\0\x04\0\0\0nope\x01\0\0\0\0".as_slice()),
        ("duplicate field", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x02\0\0\0\x04\0\0\0name\x01\0\0\0\0\x04\0\0\0name\x01\0\0\0\0".as_slice()),
        ("invalid UTF-8", b"\x02\x0d\0\0\0typed.records\x01\0\0\0\x02\0\0\0\0\0\0\0\x80\x01\0\0\0\x04\0\0\0name\x01\x01\0\0\0\xff".as_slice()),
        ("operand length overflow", b"\x03\xff\xff\xff\xff".as_slice()),
    ] {
        assert!(matches!(Program::from_bytes(&catalog(), &wire(&[frame])), Err(Error::Invalid)), "{label}");
    }
    assert!(matches!(
        Program::from_bytes(&catalog(), &vec![0; 16 * 1024 * 1024 + 1]),
        Err(Error::Invalid)
    ));
}

#[test]
fn encoding_failure_poisoning_discards_earlier_local_changes() {
    struct NoCommit;
    impl Backend for NoCommit {
        fn load(&mut self, _: &Table) -> Result<Rows, Error> {
            Ok(vec![])
        }
        fn commit(&mut self, _: &Program) -> Result<(), CommitError> {
            panic!("an invalid program must never reach the backend")
        }
    }
    let mut store = Store::new(catalog(), NoCommit).unwrap();
    store.load("typed.records").unwrap();
    let result = store.run("oversized", |tx| {
        tx.insert(
            "typed.records",
            Row::from([
                ("id".into(), 1.into()),
                ("name".into(), "ok".into()),
                ("payload".into(), Value::Bytes(vec![])),
            ]),
        )?;
        let failed = tx.insert(
            "typed.records",
            Row::from([
                ("id".into(), 2.into()),
                ("name".into(), "too large".into()),
                ("payload".into(), Value::Bytes(vec![0; 16 * 1024 * 1024])),
            ]),
        );
        assert_eq!(failed, Err(Error::Invalid));
        Ok(())
    });
    assert!(matches!(result, Err(Error::Invalid)));
    assert!(
        store
            .inspect("discarded", |tx| tx.find("typed.records", "primary", &[]))
            .unwrap()
            .is_empty()
    );
}

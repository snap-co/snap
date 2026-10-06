use snap_store::{
    replica::{Publication, Replica},
    *,
};

fn catalog() -> Catalog {
    Catalog::new(vec![Table {
        name: "example.rows".into(),
        columns: vec![
            Column {
                name: "id".into(),
                kind: Kind::Integer,
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
fn row(id: i64, bytes: &[u8]) -> Row {
    Row::from([
        ("id".into(), id.into()),
        ("payload".into(), Value::Bytes(bytes.into())),
    ])
}
fn publication(sequence: u64, reset: bool, instructions: Vec<Instruction>) -> Publication {
    Publication {
        sequence,
        reset,
        program: Program::from_instructions(&catalog(), instructions)
            .unwrap()
            .as_bytes()
            .to_vec(),
    }
}

#[test]
fn replica_requires_a_baseline_and_order_and_preserves_state_on_failed_programs() {
    let mut replica = Replica::new(catalog()).unwrap();
    let assign = |id: i64, bytes: &[u8]| Instruction::Update {
        table: "example.rows".into(),
        key: vec![id.into()],
        changes: Row::from([("payload".into(), Value::Bytes(bytes.into()))]),
    };
    assert_eq!(
        replica.apply(&publication(1, false, vec![assign(1, &[2])])),
        Err(Error::Invalid)
    );
    let initial = publication(
        5,
        true,
        vec![Instruction::Insert {
            table: "example.rows".into(),
            row: row(1, &[0, 255]),
        }],
    );
    assert_eq!(replica.apply(&initial), Ok(true));
    assert_eq!(
        replica.get("example.rows", &[1.into()]).unwrap(),
        Some(row(1, &[0, 255]))
    );
    assert_eq!(
        replica.apply(&publication(7, false, vec![assign(1, &[3])])),
        Err(Error::Invalid)
    );
    assert_eq!(
        replica.apply(&publication(
            6,
            false,
            vec![assign(1, &[2]), assign(99, &[3])]
        )),
        Err(Error::NotFound)
    );
    assert_eq!(replica.sequence(), Some(5));
    assert_eq!(
        replica.get("example.rows", &[1.into()]).unwrap(),
        Some(row(1, &[0, 255]))
    );
    let next = publication(6, false, vec![assign(1, &[2])]);
    assert_eq!(replica.apply(&next), Ok(true));
    assert_eq!(replica.apply(&initial), Ok(false));
    assert_eq!(replica.apply(&next), Ok(false));
    assert_eq!(
        replica.get("example.rows", &[1.into()]).unwrap(),
        Some(row(1, &[2]))
    );
    let mut invalid = publication(7, true, vec![]);
    invalid.program[0] = b'X';
    assert_eq!(replica.apply(&invalid), Err(Error::Invalid));
    assert_eq!(replica.sequence(), Some(6));
    assert_eq!(
        replica.get("example.rows", &[1.into()]).unwrap(),
        Some(row(1, &[2]))
    );
    assert_eq!(replica.apply(&publication(20, true, vec![])), Ok(true));
    assert_eq!(replica.get("example.rows", &[1.into()]).unwrap(), None);
}

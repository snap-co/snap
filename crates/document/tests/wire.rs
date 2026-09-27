use snap_document::{ClientMessage, Completion, Intent, ServerMessage, wire::Wire};
use snap_transport::{Command, Error, Event, Response, json};

fn intent(id: u64) -> Intent {
    Intent {
        id,
        document: "doc".into(),
        version: "1".into(),
        mutation: "add".into(),
        args: json!(1),
    }
}

#[test]
fn physical_correlation_is_distinct_from_recovered_mutation_ids() {
    let mut wire = Wire::default();
    let Command::Invoke(first) = wire.submit(ClientMessage::Mutate(intent(42))).unwrap() else {
        panic!()
    };
    assert_eq!(first.id, 1);
    assert_eq!(
        wire.receive(Response::Events(vec![Event::Accepted { id: 1 }]))
            .unwrap(),
        vec![ServerMessage::Accepted { id: 42 }]
    );
    let Command::Invoke(second) = wire.submit(ClientMessage::Mutate(intent(43))).unwrap() else {
        panic!()
    };
    assert_eq!(second.id, 2);
    let completion = ServerMessage::Completed(Completion {
        id: 42,
        document: "doc".into(),
        result: Ok(None),
    });
    assert_eq!(
        wire.receive(Response::Events(vec![Event::Completed {
            id: 1,
            outcome: Ok(serde_json::to_value(&completion).unwrap())
        }]))
        .unwrap(),
        vec![completion]
    );
}

#[test]
fn an_unknown_commit_outcome_is_not_converted_to_a_definite_rejection() {
    let mut wire = Wire::default();
    wire.submit(ClientMessage::Mutate(intent(9))).unwrap();
    wire.receive(Response::Events(vec![Event::Accepted { id: 1 }]))
        .unwrap();
    assert_eq!(
        wire.receive(Response::Events(vec![Event::Completed {
            id: 1,
            outcome: Err(Error::Unavailable)
        }])),
        Err(Error::Unavailable)
    );
}

#[test]
fn completion_cannot_claim_a_different_intent_or_skip_acceptance() {
    for accepted in [false, true] {
        let mut wire = Wire::default();
        wire.submit(ClientMessage::Mutate(intent(9))).unwrap();
        if accepted {
            wire.receive(Response::Events(vec![Event::Accepted { id: 1 }]))
                .unwrap();
        }
        let completion = ServerMessage::Completed(Completion {
            id: if accepted { 10 } else { 9 },
            document: "doc".into(),
            result: Ok(None),
        });
        assert_eq!(
            wire.receive(Response::Events(vec![Event::Completed {
                id: 1,
                outcome: Ok(serde_json::to_value(completion).unwrap())
            }])),
            Err(Error::Protocol)
        );
    }
}

#[test]
fn independent_wire_examples_distinguish_pushes_from_completions() {
    let mut wire = Wire::default();
    let push: Response = serde_json::from_str(
        r#"{"Notification":{"operation":"document","input":{"Removed":["doc"]}}}"#,
    )
    .unwrap();
    assert_eq!(
        wire.receive(push).unwrap(),
        vec![ServerMessage::Removed(vec!["doc".into()])]
    );
    let forged: Response = serde_json::from_str(r#"{"Notification":{"operation":"document","input":{"Completed":{"id":1,"document":"doc","result":{"Ok":null}}}}}"#).unwrap();
    assert_eq!(wire.receive(forged), Err(Error::Protocol));
}

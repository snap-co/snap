use snap_document::{ClientMessage, Completion, Intent, Manifest, ServerMessage, wire::Wire};
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
    let mut ids = snap_transport::client::InvocationIds::default();
    let Command::Invoke(first) = wire
        .submit(&mut ids, ClientMessage::Mutate(intent(42)))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(first.id, 1);
    assert_eq!(
        wire.receive(Response::Event(Event::Accepted { id: 1 }))
            .unwrap(),
        Some(ServerMessage::Accepted { id: 42 })
    );
    let Command::Invoke(second) = wire
        .submit(&mut ids, ClientMessage::Mutate(intent(43)))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(second.id, 2);
    let completion = ServerMessage::Completed(Completion {
        id: 42,
        document: "doc".into(),
        result: Ok(None),
    });
    assert_eq!(
        wire.receive(Response::Event(Event::Completed {
            id: 1,
            outcome: Ok(serde_json::to_value(&completion).unwrap())
        }))
        .unwrap(),
        Some(completion)
    );
}

/// Transport carries one event per frame, so acceptance and completion arrive as
/// separate frames. Acceptance produces no message of its own: it only advances
/// pacing, so a caller reading it sees that the journal moved forward.
#[test]
fn acceptance_is_its_own_frame_and_produces_no_manifest_message() {
    let mut wire = Wire::default();
    let mut ids = snap_transport::client::InvocationIds::default();
    wire.submit(
        &mut ids,
        ClientMessage::Manifest(Manifest {
            holdings: Vec::new(),
            pending: Vec::new(),
        }),
    )
    .unwrap();
    assert_eq!(
        wire.receive(Response::Event(Event::Accepted { id: 1 }))
            .unwrap(),
        None
    );
}

/// Progress is reported against the intent id, not the transport invocation id,
/// so a caller tracking its own journal does not have to translate.
#[test]
fn progress_is_reported_against_the_intent_id() {
    let mut wire = Wire::default();
    let mut ids = snap_transport::client::InvocationIds::default();
    wire.submit(&mut ids, ClientMessage::Mutate(intent(77)))
        .unwrap();
    wire.receive(Response::Event(Event::Accepted { id: 1 }))
        .unwrap();
    let mut seen = Vec::new();
    assert_eq!(
        wire.receive_with_progress(
            Response::Event(Event::Progress {
                id: 1,
                value: json!("working")
            }),
            |id, value| seen.push((id, value)),
        )
        .unwrap(),
        None
    );
    assert_eq!(seen, vec![(77, json!("working"))]);
}

#[test]
fn an_unknown_commit_outcome_is_not_converted_to_a_definite_rejection() {
    let mut wire = Wire::default();
    let mut ids = snap_transport::client::InvocationIds::default();
    wire.submit(&mut ids, ClientMessage::Mutate(intent(9)))
        .unwrap();
    wire.receive(Response::Event(Event::Accepted { id: 1 }))
        .unwrap();
    assert_eq!(
        wire.receive(Response::Event(Event::Completed {
            id: 1,
            outcome: Err(Error::Unavailable)
        })),
        Err(Error::Unavailable)
    );
}

#[test]
fn completion_cannot_claim_a_different_intent_or_skip_acceptance() {
    for accepted in [false, true] {
        let mut wire = Wire::default();
        let mut ids = snap_transport::client::InvocationIds::default();
        wire.submit(&mut ids, ClientMessage::Mutate(intent(9)))
            .unwrap();
        if accepted {
            wire.receive(Response::Event(Event::Accepted { id: 1 }))
                .unwrap();
        }
        let completion = ServerMessage::Completed(Completion {
            id: if accepted { 10 } else { 9 },
            document: "doc".into(),
            result: Ok(None),
        });
        assert_eq!(
            wire.receive(Response::Event(Event::Completed {
                id: 1,
                outcome: Ok(serde_json::to_value(completion).unwrap())
            })),
            Err(Error::Protocol)
        );
    }
}

/// Replication is uncorrelated: it arrives on the global path by topic kind,
/// never on an invocation's channel, so no submission is required to receive it.
#[test]
fn independent_wire_examples_distinguish_pushes_from_completions() {
    let mut wire = Wire::default();
    let push: Response = serde_json::from_str(&format!(
        r#"{{"Global":{{"kind":"{}","input":{{"Removed":["doc"]}}}}}}"#,
        snap_document::wire::KIND
    ))
    .unwrap();
    assert_eq!(
        wire.receive(push).unwrap(),
        Some(ServerMessage::Removed(vec!["doc".into()]))
    );
    let forged: Response = serde_json::from_str(&format!(
        r#"{{"Global":{{"kind":"{}","input":{{"Completed":{{"id":1,"document":"doc","result":{{"Ok":null}}}}}}}}}}"#,
        snap_document::wire::KIND
    ))
    .unwrap();
    assert_eq!(wire.receive(forged), Err(Error::Protocol));
}

/// A push for a topic this capability does not own is refused rather than
/// silently ignored, so a misconfigured host cannot feed the journal garbage.
#[test]
fn a_push_for_another_topic_is_refused() {
    let mut wire = Wire::default();
    let other: Response =
        serde_json::from_str(r#"{"Global":{"kind":"something.else","input":{"Reset":null}}}"#)
            .unwrap();
    assert_eq!(wire.receive(other), Err(Error::Protocol));
}

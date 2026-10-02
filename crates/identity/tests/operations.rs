mod support;
use snap_identity::{
    Identity,
    operation::{self, Enrollment},
};
use snap_transport::{
    Error, Invocation, Operation,
    bearer::{Change, Token},
    json,
    operation::{Context, Runtime},
};

#[test]
fn failed_account_initialization_rolls_back_credentials_and_never_releases_a_token() {
    let mut store = support::store(true);
    let definitions = operation::definitions(
        Identity::default(),
        support::Fake::default,
        Some(Enrollment {
            data: snap_store::Data::default(),
            initialize: Box::new(|_, _, _| Err(snap_store::Error::Unavailable)),
        }),
    );
    let mut runtime = Runtime::default();
    for definition in definitions.preconnection {
        runtime.register(definition).unwrap();
    }
    let selection = runtime
        .definitions()
        .resolve(operation::Enroll::NAME)
        .unwrap();
    runtime
        .enqueue(
            (),
            Invocation {
                id: 1,
                operation: operation::Enroll::NAME.into(),
                input: json!({"email":"a@b", "password":"password1"}),
            },
            selection,
        )
        .unwrap();
    let (work, call, selection) = runtime.acquire().unwrap();
    runtime
        .accept(
            &mut store,
            work,
            call,
            selection,
            Context {
                inputs: [("clock".into(), json!(1))].into_iter().collect(),
                ..Context::default()
            },
        )
        .unwrap_or_else(|_| panic!("admission"));
    let completed = runtime.execute(&mut store).unwrap();
    assert_eq!(completed.outcome, Err(Error::Unavailable));
    assert!(completed.context.bearer_change.is_none());
    assert!(completed.changes.is_empty());
    assert!(
        store
            .inspect("absence", |tx| snap_identity::Credential::find(tx, "a@b"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn accepted_release_uses_captured_authority_and_issues_clear_only_after_commit() {
    let identity = Identity::new(10).unwrap();
    let mut crypto = support::Fake::default();
    let mut store = support::store(true);
    let issued = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut crypto, "a@b", "password1", 0)
        })
        .unwrap()
        .value;
    let principal = store
        .inspect("admission", |tx| {
            identity.resolve(tx, &crypto, &issued.bearer, 9)
        })
        .unwrap();
    let mut runtime = Runtime::default();
    for definition in operation::definitions(identity, support::Fake::default, None).preconnection {
        runtime.register(definition).unwrap();
    }
    let selection = runtime
        .definitions()
        .resolve(operation::Release::NAME)
        .unwrap();
    runtime
        .enqueue(
            (),
            Invocation {
                id: 1,
                operation: operation::Release::NAME.into(),
                input: json!({"scope":"current"}),
            },
            selection,
        )
        .unwrap();
    let (work, call, selection) = runtime.acquire().unwrap();
    runtime
        .accept(
            &mut store,
            work,
            call,
            selection,
            Context {
                actor: Some(principal.identity.clone()),
                principal: Some(principal),
                bearer: Some(issued.bearer.clone()),
                ..Context::default()
            },
        )
        .unwrap_or_else(|_| panic!("admission"));
    assert!(matches!(
        store.inspect("expired", |tx| identity.resolve(
            tx,
            &crypto,
            &issued.bearer,
            10
        )),
        Err(snap_store::Error::NotFound)
    ));
    let completed = runtime.execute(&mut store).unwrap();
    assert_eq!(completed.outcome, Ok(json!(null)));
    assert_eq!(completed.context.bearer_change, Some(Change::Clear));
    assert!(matches!(
        store.inspect("revoked", |tx| identity.resolve(
            tx,
            &crypto,
            &issued.bearer,
            9
        )),
        Err(snap_store::Error::NotFound)
    ));
    assert!(
        !format!("{:?}", Change::Set(Token::new(issued.bearer.clone()))).contains(&issued.bearer)
    );
}

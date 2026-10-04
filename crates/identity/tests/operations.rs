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

#[test]
fn host_authentication_captures_principal_and_only_optional_identity_can_fall_back() {
    use snap_identity::authentication::Authentication;
    use snap_store::Error as StoreError;
    use snap_transport::bearer::{Authority, Principal};
    use std::sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    };

    let identity = Identity::new(10).unwrap();
    let mut store = support::store(true);
    let issued = store
        .run("enroll", |tx| {
            identity.enroll(tx, &mut support::Fake::default(), "a@b", "password1", 100)
        })
        .unwrap()
        .value;
    let clock = Arc::new(AtomicI64::new(100));
    let time = clock.clone();
    let authentication = Authentication::new(
        Arc::new(identity.provider(support::Fake::default())),
        Arc::new(move || time.load(Ordering::Relaxed)),
    );
    for (now, bearer, required, expected) in [
        (109, Some(issued.bearer.as_str()), true, Ok(true)),
        (109, Some(issued.bearer.as_str()), false, Ok(true)),
        (109, Some("unknown"), true, Err(StoreError::NotFound)),
        (109, Some("unknown"), false, Ok(false)),
        (
            110,
            Some(issued.bearer.as_str()),
            true,
            Err(StoreError::NotFound),
        ),
        (110, Some(issued.bearer.as_str()), false, Ok(false)),
        (109, None, false, Ok(false)),
    ] {
        clock.store(now, Ordering::Relaxed);
        let result = store.inspect("host admission", |tx| {
            authentication.resolve(tx, bearer, required)
        });
        match expected {
            Ok(identified) => {
                let resolved = result.unwrap();
                assert_eq!(
                    resolved.actor,
                    identified.then(|| issued.principal.identity.clone())
                );
                assert_eq!(
                    resolved.principal,
                    identified.then(|| Principal {
                        identity: issued.principal.identity.clone(),
                        authenticated_at: 100,
                    })
                );
            }
            Err(error) => assert!(matches!(result, Err(actual) if actual == error)),
        }
    }
    // Unknown residency is not proof of an invalid credential. Optional identity
    // must fail rather than allowing the caller through as anonymous.
    store
        .retain_keys("identity.sessions", &Default::default())
        .unwrap();
    assert!(matches!(
        store.inspect("cold credential", |tx| {
            authentication.resolve(tx, Some(&issued.bearer), false)
        }),
        Err(StoreError::Miss(_))
    ));
}

#[path = "../../../crates/identity/tests/support/mod.rs"]
mod support;
use futures::{FutureExt, executor::block_on, task::noop_waker};
use snap_transport::{Channel, Command, Error, Invocation, Response, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use testy_server::identity::{Sessions, platform};
use testy_server::memory::Memory;

/// Send one command and return its terminal frame. The channel is a stream, so
/// acceptance may arrive first; these cases expect it not to.
async fn round_trip<C: Channel>(channel: &mut C, command: Command) -> Response {
    channel.send(command).await.unwrap();
    loop {
        if let Some(
            response @ (Response::Event(snap_transport::Event::Completed { .. })
            | Response::Failed(_)),
        ) = channel.receive().await.unwrap()
        {
            return response;
        }
    }
}

#[test]
fn session_logout_waits_for_accepted_work_and_then_retires_its_connections() {
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::new(10).unwrap(),
        move || clock.load(Ordering::SeqCst),
    );
    let memory = Memory::new(platform(sessions));
    let mut first = testy::Client::new(memory.channel());
    assert_eq!(
        block_on(first.start("anonymous")),
        Err(Error::IdentityRequired)
    );
    let token = block_on(first.authenticate(true, "a@b", "password1")).unwrap();
    assert!(
        memory.trace().is_empty(),
        "issuance results must not enter retained traces"
    );
    block_on(first.start("one")).unwrap();
    block_on(first.add(42)).unwrap();
    let mut second = testy::Client::new(memory.channel());
    block_on(second.authenticate(false, "a@b", "password1")).unwrap();
    block_on(second.start("two")).unwrap();
    assert_eq!(block_on(second.inspect()).unwrap().accumulator, 0);
    let mut sibling = testy::Client::new(memory.channel());
    sibling.use_session(&token).unwrap();
    block_on(sibling.start("same-session-other-tab")).unwrap();
    assert_eq!(block_on(sibling.inspect()).unwrap().accumulator, 0);
    block_on(first.disconnect()).unwrap();
    block_on(first.start("one")).unwrap();
    assert_eq!(block_on(first.inspect()).unwrap().accumulator, 0);

    let mut pending = Box::pin(first.add_checked(5));
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    assert!(pending.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = memory.pending_read().unwrap();
    let mut logout = Box::pin(sibling.logout());
    assert!(logout.poll_unpin(&mut cx).is_pending());
    assert!(logout.poll_unpin(&mut cx).is_pending());
    memory.supply(ticket, &key, Ok(json!(100))).unwrap();
    assert_eq!(block_on(pending), Ok(5));
    block_on(logout).unwrap();
    assert_eq!(memory.residents(), 1);
    assert!(block_on(first.start("revoked")).is_err());
    assert_eq!(block_on(second.add(3)).unwrap(), 3);
    now.store(10, Ordering::SeqCst);
    memory.advance(1);
    assert_eq!(memory.residents(), 0);
    assert!(block_on(second.add(1)).is_err());
}

#[test]
fn transport_preserves_miss_instead_of_misreporting_bad_credentials() {
    let sessions = Sessions::new(
        support::store(false),
        support::Fake::default(),
        snap_identity::Identity::default(),
        || 0,
    );
    let memory = Memory::new(platform(sessions));
    let mut client = testy::Client::new(memory.channel());
    assert_eq!(
        block_on(client.authenticate(true, "a@b", "password1")),
        Err(Error::Application(json!({"code": "StoreMiss"})))
    );
}

#[test]
fn expiry_during_a_held_accepted_operation_drains_before_logical_release() {
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::new(10).unwrap(),
        move || clock.load(Ordering::SeqCst),
    );
    let memory = Memory::new(platform(sessions));
    let mut client = testy::Client::new(memory.channel());
    block_on(client.authenticate(true, "a@b", "password1")).unwrap();
    block_on(client.start("expiry")).unwrap();
    let mut pending = Box::pin(client.add_checked(7));
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    assert!(pending.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = memory.pending_read().unwrap();
    now.store(10, Ordering::SeqCst);
    memory.advance(1);
    assert_eq!(memory.residents(), 1);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    memory.supply(ticket, &key, Ok(json!(100))).unwrap();
    assert_eq!(block_on(pending), Ok(7));
    assert_eq!(memory.residents(), 0);
}

#[test]
fn protected_identity_requests_validate_authority_before_ack() {
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::default(),
        || 0,
    );
    let memory = Memory::new(platform(sessions));
    let mut channel = memory.channel();
    let reply = block_on(round_trip(
        &mut channel,
        Command::Request {
            bearer: Some("invalid".into()),
            invocation: Invocation {
                id: 1,
                operation: "identity.release".into(),
                input: json!({"scope":"current"}),
            },
        },
    ));
    assert!(matches!(
        reply,
        Response::Event(snap_transport::Event::Completed {
            outcome: Err(Error::InvalidBearer),
            ..
        })
    ));
    assert!(memory.trace().is_empty());
}

#[test]
fn identity_inputs_on_wrong_command_kind_never_enter_execution_diagnostics() {
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::default(),
        || 0,
    );
    let memory = Memory::new(platform(sessions));
    let mut channel = memory.channel();
    assert_eq!(
        block_on(round_trip(
            &mut channel,
            Command::Invoke(Invocation {
                id: 1,
                operation: "identity.acquire".into(),
                input: json!({"email": "a@b", "password": "never-log-this"}),
            })
        )),
        Response::Failed(Error::Protocol)
    );
    assert!(memory.trace().is_empty());
}

#[test]
fn permission_changes_share_the_fifo_with_calculator_operations() {
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::default(),
        || 0,
    );
    let memory = Memory::new(platform(sessions));
    let mut first = testy::Client::new(memory.channel());
    block_on(first.authenticate(true, "a@b", "password1")).unwrap();
    block_on(first.start("one")).unwrap();
    let mut second = testy::Client::new(memory.channel());
    let token = block_on(second.authenticate(false, "a@b", "password1")).unwrap();
    block_on(second.start("two")).unwrap();
    let mut logout = testy::Client::new(memory.channel());
    logout.use_session(&token).unwrap();
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    let mut held = Box::pin(first.add_checked(5));
    assert!(held.poll_unpin(&mut cx).is_pending());
    assert!(held.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = memory.pending_read().unwrap();
    let mut queued = Box::pin(second.add(3));
    assert!(queued.poll_unpin(&mut cx).is_pending());
    assert!(queued.poll_unpin(&mut cx).is_pending());
    let mut revoked = Box::pin(logout.logout());
    assert!(revoked.poll_unpin(&mut cx).is_pending());
    assert!(revoked.poll_unpin(&mut cx).is_pending());
    assert!(held.poll_unpin(&mut cx).is_pending());
    assert_eq!(memory.residents(), 2);
    memory.supply(ticket, &key, Ok(json!(100))).unwrap();
    assert_eq!(block_on(held), Ok(5));
    assert_eq!(block_on(queued), Ok(3));
    block_on(revoked).unwrap();
    assert_eq!(memory.residents(), 1);
}

#[test]
fn shared_identity_sdk_owns_acquisition_management_and_release_without_tracing_secrets() {
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::default(),
        || 10,
    );
    let memory = Memory::new(platform(sessions));
    let mut transport = snap_transport::client::Client::new(memory.channel());
    assert_eq!(
        block_on(snap_identity::client::Client::new(&mut transport).fetch()),
        Ok(None)
    );
    let principal = block_on(
        snap_identity::client::Client::new(&mut transport).enroll(" UI@Example.test ", "password1"),
    )
    .unwrap();
    assert_eq!(principal.authenticated_at, 10);
    let bearer = transport.bearer().unwrap().to_owned();
    assert_eq!(
        block_on(snap_identity::client::Client::new(&mut transport).fetch()),
        Ok(Some(principal))
    );
    block_on(transport.connect(&bearer, "identity-sdk")).unwrap();
    let summaries =
        block_on(snap_identity::client::Client::new(&mut transport).sessions()).unwrap();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].current);
    assert_ne!(summaries[0].id, bearer);
    let credentials =
        block_on(snap_identity::client::Client::new(&mut transport).credentials()).unwrap();
    assert_eq!(credentials[0].label, "ui@example.test");
    block_on(
        snap_identity::client::Client::new(&mut transport)
            .release(snap_identity::ReleaseScope::Current),
    )
    .unwrap();
    assert_eq!(transport.bearer(), None);
    assert_eq!(
        block_on(snap_identity::client::Client::new(&mut transport).fetch()),
        Ok(None)
    );
    transport.use_bearer(&bearer);
    assert_eq!(
        block_on(snap_identity::client::Client::new(&mut transport).fetch()),
        Ok(None)
    );
    assert_eq!(
        transport.bearer(),
        None,
        "invalid credentials are cleared by the provider's fetch contract"
    );
    assert!(memory.trace().is_empty());
}

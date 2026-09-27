#[path = "../../../crates/identity/tests/support/mod.rs"]
mod support;
use futures::{FutureExt, executor::block_on, task::noop_waker};
use snap_platform_local::memory::Memory;
use snap_transport::{Channel, Command, Error, Invocation, Response, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use testy_local::identity::{Sessions, platform};

#[test]
fn sessions_own_connections_and_retirement_discards_even_held_work() {
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
    // Identity's synchronous Store dispatcher remains available while Calc is held.
    block_on(sibling.logout()).unwrap();
    assert_eq!(block_on(pending), Err(Error::IdentityRequired));
    assert!(memory.supply(ticket, &key, Ok(json!(100))).is_err());
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
        block_on(channel.exchange(Command::Invoke(Invocation {
            id: 1,
            operation: "identity.login".into(),
            input: json!({"email": "a@b", "password": "never-log-this"}),
        })))
        .unwrap(),
        Response::Failed(Error::Protocol)
    );
    assert!(memory.trace().is_empty());
}

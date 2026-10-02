use futures::executor::block_on;
use snap_transport::{
    Channel, Command, Error, Event, Invocation, Response, json,
    server::{Config, Server},
};
use testy_server::memory::Memory;

fn platform() -> Memory<testy::App, testy::TestAuthority> {
    Memory::new(snap_transport::execution::Runtime::new(
        Server::new(
            testy::TestAuthority,
            Config {
                reconnect_ms: 100,
                capacity: 8,
            },
        ),
        snap_transport::execution::Executor::new(testy::App::default(), 16).unwrap(),
    ))
}
fn fixture_client<C: Channel>(channel: C) -> testy::Client<C> {
    let mut client = testy::Client::new(channel);
    client.use_session(testy::BEARER).unwrap();
    client
}

#[test]
fn sdk_program_and_independent_tabs() {
    block_on(async {
        let platform = platform();
        let mut first = fixture_client(platform.channel());
        let mut second = fixture_client(platform.channel());
        let result = testy::journey(&mut first, "tab-a").await.unwrap();
        second.start("tab-b").await.unwrap();
        second.add(90).await.unwrap();
        assert_eq!(result.accumulator, 6);
        assert_eq!(result.history.len(), 4);
        assert_eq!(first.inspect().await.unwrap(), result);
        assert_eq!(second.inspect().await.unwrap().accumulator, 90);
    });
}

#[test]
fn reconnect_retains_state_but_close_and_expiry_release_it() {
    block_on(async {
        let platform = platform();
        let mut client = fixture_client(platform.channel());
        client.start("tab").await.unwrap();
        client.add(21).await.unwrap();
        // Replacement drops the physical channel, not logical state.
        client.replace_channel(platform.channel());
        platform.advance(99);
        assert!(client.reconnect("tab").await.unwrap());
        assert_eq!(client.inspect().await.unwrap().accumulator, 21);
        client.close().await.unwrap();
        assert_eq!(platform.residents(), 0);
        assert!(!client.reconnect("tab").await.unwrap());
        assert_eq!(
            client.inspect().await.unwrap_err(),
            Error::Application(json!("NotStarted"))
        );
        client.close().await.unwrap();
        client.start("tab").await.unwrap();
        client.disconnect().await.unwrap();
        platform.advance(100);
        assert_eq!(platform.residents(), 0);
        assert!(!client.reconnect("tab").await.unwrap());
        assert_eq!(
            client.inspect().await.unwrap_err(),
            Error::Application(json!("NotStarted"))
        );
    });
}

#[test]
fn duplicate_attachment_cannot_displace_owner() {
    block_on(async {
        let platform = platform();
        let mut owner = fixture_client(platform.channel());
        let mut contender = fixture_client(platform.channel());
        owner.start("tab").await.unwrap();
        assert_eq!(contender.reconnect("tab").await, Err(Error::Occupied));
        owner.add(7).await.unwrap();
        owner.disconnect().await.unwrap();
        assert!(contender.reconnect("tab").await.unwrap());
        assert_eq!(contender.inspect().await.unwrap().accumulator, 7);
    });
}

#[test]
fn schemas_identity_and_application_failures_have_distinct_admission() {
    block_on(async {
        let platform = platform();
        let mut channel = platform.channel();
        let call = |id, input| Command::Request {
            bearer: None,
            invocation: Invocation {
                id,
                operation: "calc.add".into(),
                input,
            },
        };
        assert_eq!(
            channel.exchange(call(1, json!("bad"))).await.unwrap(),
            Response::Events(vec![Event::Completed {
                id: 1,
                outcome: Err(Error::InvalidInput)
            }])
        );
        assert_eq!(
            channel.exchange(call(2, json!(2))).await.unwrap(),
            Response::Events(vec![Event::Completed {
                id: 2,
                outcome: Err(Error::IdentityRequired)
            }])
        );
        let mut client = fixture_client(channel);
        client.start("tab").await.unwrap();
        client.add(8).await.unwrap();
        let before = client.inspect().await.unwrap();
        assert_eq!(
            client.div(0).await.unwrap_err(),
            Error::Application(json!("DivisionByZero"))
        );
        let trace = platform.trace();
        assert!(matches!(
            &trace[trace.len() - 2..],
            [
                Event::Accepted { .. },
                Event::Completed {
                    outcome: Err(Error::Application(_)),
                    ..
                }
            ]
        ));
        assert_eq!(client.inspect().await.unwrap(), before);
    });
}

#[test]
fn memory_delivery_is_async_and_cancellable_before_admission() {
    use futures::{FutureExt, task::noop_waker};
    let platform = platform();
    let mut channel = platform.channel();
    let mut pending = Box::pin(channel.exchange(Command::Connect {
        bearer: testy::BEARER.into(),
        client_id: "cancelled".into(),
    }));
    let waker = noop_waker();
    assert!(
        pending
            .poll_unpin(&mut core::task::Context::from_waker(&waker))
            .is_pending()
    );
    drop(pending);
    assert_eq!(platform.residents(), 0);
}

#[test]
fn sdk_waits_behind_the_application_gate_and_retries_without_duplicate_history() {
    use futures::{FutureExt, task::noop_waker};
    let platform = platform();
    let mut first = fixture_client(platform.channel());
    let mut second = fixture_client(platform.channel());
    block_on(first.start("first")).unwrap();
    block_on(second.start("second")).unwrap();
    block_on(first.add(42)).unwrap();
    let baseline = platform.trace().len();
    let mut pending = Box::pin(first.add_checked(10));
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    assert!(pending.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = platform.pending_read().unwrap();
    assert_eq!(key, testy::CEILING);
    let mut queued = Box::pin(second.add(20));
    assert!(queued.poll_unpin(&mut cx).is_pending());
    assert!(queued.poll_unpin(&mut cx).is_pending());
    assert!(matches!(
        &platform.trace()[baseline..],
        [Event::Accepted { .. }]
    ));
    assert_eq!(
        platform.replace(testy::App::default()),
        Err(snap_transport::execution::Error::Unavailable)
    );
    platform.supply(ticket, &key, Ok(json!(100))).unwrap();
    assert_eq!(block_on(pending).unwrap(), 52);
    assert_eq!(block_on(queued).unwrap(), 20);
    let calculator = block_on(first.inspect()).unwrap();
    assert_eq!(calculator.history.len(), 2);
    assert_eq!(calculator.history[1].before, 42);
    assert_eq!(calculator.history[1].after, 52);
    platform
        .replace(testy::App::with_add(|a, b| {
            a.checked_add(b.checked_mul(2)?)
        }))
        .unwrap();
    assert_eq!(block_on(first.add(5)).unwrap(), 62);
    assert_eq!(block_on(first.inspect()).unwrap().history.len(), 3);
}

#[test]
fn cancelled_client_keeps_owned_work_and_expiry_waits_for_it() {
    use futures::{FutureExt, task::noop_waker};
    let platform = platform();
    let mut client = fixture_client(platform.channel());
    block_on(client.start("lost")).unwrap();
    let mut pending = Box::pin(client.add_checked(10));
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    assert!(pending.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = platform.pending_read().unwrap();
    drop(pending);
    drop(client);
    platform.advance(100);
    assert_eq!(
        platform.residents(),
        1,
        "accepted work retains the draining logical connection"
    );
    // Data remains pinned until the accepted operation has finished. Supplying
    // the read cannot panic or revive the retired connection.
    platform.supply(ticket, &key, Ok(json!(100))).unwrap();
    assert_eq!(platform.residents(), 0);
    assert!(
        matches!(platform.trace().last(), Some(Event::Completed { outcome: Ok(value), .. }) if value == &json!(10))
    );
    let mut client = fixture_client(platform.channel());
    block_on(client.start("lost")).unwrap();
    assert_eq!(block_on(client.inspect()).unwrap().accumulator, 0);
}

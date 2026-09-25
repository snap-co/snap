use futures::executor::block_on;
use snap_platform_local::memory::Memory;
use snap_transport::{
    Channel, Command, Error, Event, Invocation, Response, json,
    server::{Config, Server},
};

fn platform() -> Memory<testy::App, testy::TestAuthority> {
    Memory::new(
        Server::new(
            testy::App::default(),
            testy::TestAuthority,
            Config {
                reconnect_ms: 100,
                capacity: 8,
            },
        )
        .unwrap(),
    )
}

#[test]
fn sdk_program_and_independent_tabs() {
    block_on(async {
        let platform = platform();
        let mut first = testy::Client::new(platform.channel());
        let mut second = testy::Client::new(platform.channel());
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
        let mut client = testy::Client::new(platform.channel());
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
        let mut owner = testy::Client::new(platform.channel());
        let mut contender = testy::Client::new(platform.channel());
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
        let mut client = testy::Client::new(channel);
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

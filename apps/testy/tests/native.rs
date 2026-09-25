use snap_platform_local::{Platform, native};
use snap_transport::{Error, server::Server};

#[test]
fn sdk_program_over_real_native_socket_and_reconnect() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = Server::new(testy::TestAuthority, Default::default());
            let execution = snap_execution::Executor::new(testy::App::default(), 16).unwrap();
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            let host = tokio::task::spawn_local(native::serve(
                listener,
                Platform::new(server, execution),
                receiver,
                |_, key| {
                    if key == testy::CEILING {
                        Ok(snap_execution::json!(100))
                    } else {
                        Err(snap_execution::Error::Unavailable)
                    }
                },
            ));
            let mut first = testy::Client::new(native::Connection::open(address).await.unwrap());
            let result = testy::journey(&mut first, "native-tab").await.unwrap();
            assert_eq!(result.accumulator, 6);
            assert_eq!(result.history.len(), 4);
            let mut second = testy::Client::new(native::Connection::open(address).await.unwrap());
            assert_eq!(second.reconnect("native-tab").await, Err(Error::Occupied));
            // Explicit detach synchronizes the native socket lifecycle; expiry uses
            // virtual time in the memory contract instead of wall-clock sleeps here.
            first.disconnect().await.unwrap();
            assert!(second.reconnect("native-tab").await.unwrap());
            assert_eq!(second.inspect().await.unwrap(), result);
            // Lose the actual TCP channel. The server must observe EOF before a
            // replacement can claim it; retry only attachment, never calculations.
            second.replace_channel(native::Connection::open(address).await.unwrap());
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match second.reconnect("native-tab").await {
                        Err(Error::Occupied) => tokio::task::yield_now().await,
                        Ok(resumed) => {
                            assert!(resumed);
                            break;
                        }
                        Err(error) => panic!("unexpected reconnect error: {error:?}"),
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(second.inspect().await.unwrap(), result);
            second.close().await.unwrap();
            second.start("checked").await.unwrap();
            assert_eq!(second.add_checked(12).await.unwrap(), 12);
            assert_eq!(
                second.add_checked(100).await,
                Err(Error::Application(snap_execution::json!("AboveCeiling")))
            );
            assert_eq!(second.inspect().await.unwrap().accumulator, 12);
            second.close().await.unwrap();
            shutdown.send(true).unwrap();
            host.await.unwrap().unwrap();
        }));
}

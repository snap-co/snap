use testy_native as native;
use testy_server::identity::{Sessions, platform};

#[test]
fn authenticated_sdk_over_real_tcp_discards_connection_state() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let migration = toml::from_str(snap_identity::MIGRATION).unwrap();
            let time = toml::from_str(snap_identity::SESSION_TIME_MIGRATION).unwrap();
            let kind = toml::from_str(snap_identity::CREDENTIAL_KIND_MIGRATION).unwrap();
            let flows = toml::from_str(snap_identity::FLOW_MIGRATION).unwrap();
            let mut store = snap_store_sqlite::Sqlite::memory(&[migration, time, kind, flows]).unwrap();
            for table in snap_identity::TABLES {
                store.load(table).unwrap();
            }
            let sessions = Sessions::new(
                store,
                snap_crypto::Native,
                snap_identity::Identity::default(),
                || 0,
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            let host = tokio::task::spawn_local(native::serve(
                listener,
                platform(sessions),
                receiver,
                |_, _| Ok(snap_transport::json!(100)),
            ));
            let mut first = testy::Client::new(native::Connection::open(address).await.unwrap());
            let token = first.authenticate(true, "a@b", "password1").await.unwrap();
            let result = testy::journey(&mut first, "first").await.unwrap();
            assert_eq!(result.accumulator, 6);
            let mut second = testy::Client::new(native::Connection::open(address).await.unwrap());
            second
                .authenticate(false, "a@b", "password1")
                .await
                .unwrap();
            second.start("second").await.unwrap();
            assert_eq!(second.inspect().await.unwrap().accumulator, 0);
            first.disconnect().await.unwrap();
            first.start("first").await.unwrap();
            assert_eq!(first.inspect().await.unwrap().accumulator, 0);
            assert_eq!(first.add_checked(12).await.unwrap(), 12);
            first.logout().await.unwrap();
            let mut revoked = testy::Client::new(native::Connection::open(address).await.unwrap());
            revoked.use_session(&token).unwrap();
            assert_eq!(
                revoked.start("revoked").await,
                Err(snap_transport::Error::InvalidBearer)
            );
            assert_eq!(second.add(7).await.unwrap(), 7);
            shutdown.send(true).unwrap();
            host.await.unwrap().unwrap();
        }));
}

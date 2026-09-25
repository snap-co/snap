fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let address = std::env::var("TESTY_ADDR").unwrap_or_else(|_| "127.0.0.1:3847".into());
            let listener = tokio::net::TcpListener::bind(address).await?;
            println!("Testy listening on {}", listener.local_addr()?);
            let server =
                snap_transport::server::Server::new(testy::TestAuthority, Default::default());
            let execution = snap_execution::Executor::new(testy::App::default(), 1024).unwrap();
            let (_shutdown, receiver) = tokio::sync::watch::channel(false);
            snap_platform_local::native::serve(
                listener,
                snap_platform_local::Platform::new(server, execution),
                receiver,
                |_, key| {
                    if key == testy::CEILING {
                        Ok(snap_execution::json!(1000))
                    } else {
                        Err(snap_execution::Error::Unavailable)
                    }
                },
            )
            .await
        }))
}

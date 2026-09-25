fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let address = std::env::var("TESTY_ADDR").unwrap_or_else(|_| "127.0.0.1:3847".into());
            let listener = tokio::net::TcpListener::bind(address).await?;
            println!("Testy listening on {}", listener.local_addr()?);
            let server = snap_transport::server::Server::new(
                testy::App::default(),
                testy::TestAuthority,
                Default::default(),
            )
            .unwrap();
            let (_shutdown, receiver) = tokio::sync::watch::channel(false);
            snap_platform_local::native::serve(
                listener,
                snap_platform_local::Platform::new(server),
                receiver,
            )
            .await
        }))
}

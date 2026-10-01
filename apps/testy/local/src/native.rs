fn main() -> std::io::Result<()> {
    let options = snap_config::Options::parse().map_err(std::io::Error::other)?;
    let config = snap_config::Config::<testy_local::config::Settings>::read(&options.config)
        .map_err(std::io::Error::other)?;
    if options.action == snap_config::Action::Check {
        return Ok(());
    }
    if options.action != snap_config::Action::Serve {
        return Err(std::io::Error::other("Migrate using the packaged server"));
    }
    config.load_secrets().map_err(std::io::Error::other)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::task::LocalSet::new().run_until(async {
            let address = config.host.listen;
            let listener = tokio::net::TcpListener::bind(address).await?;
            println!("Testy listening on {}", listener.local_addr()?);
            let sessions = testy_local::identity::open(&config.database())
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let (_shutdown, receiver) = tokio::sync::watch::channel(false);
            snap_runtime_local::native::serve(
                listener,
                testy_local::identity::platform(sessions),
                receiver,
                |_, key| {
                    if key == testy::CEILING {
                        Ok(snap_transport::json!(1000))
                    } else {
                        Err(snap_transport::execution::Error::Unavailable)
                    }
                },
            )
            .await
        }))
}

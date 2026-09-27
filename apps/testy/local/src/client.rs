fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let address = std::env::var("TESTY_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:3847".into())
                .parse()
                .map_err(std::io::Error::other)?;
            let channel = snap_platform_local::native::Connection::open(address).await?;
            let mut client = testy::Client::new(channel);
            let email = std::env::var("TESTY_EMAIL").map_err(std::io::Error::other)?;
            let password = std::env::var("TESTY_PASSWORD").map_err(std::io::Error::other)?;
            client
                .authenticate(
                    std::env::var_os("TESTY_ENROLL").is_some(),
                    &email,
                    &password,
                )
                .await
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            let result = testy::journey(&mut client, "native-program")
                .await
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            assert_eq!(result.accumulator, 6);
            assert_eq!(result.history.len(), 4);
            client
                .close()
                .await
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            println!(
                "Testy native: {} after {} operations",
                result.accumulator,
                result.history.len()
            );
            Ok(())
        })
}

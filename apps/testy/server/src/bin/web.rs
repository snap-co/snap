use snap_transport::json;
use testy_server::{development::Development, web};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let options = snap_config::Options::parse().map_err(std::io::Error::other)?;
    let config = snap_config::Config::<testy_server::config::Settings>::read(&options.config)
        .map_err(std::io::Error::other)?;
    if options.action == snap_config::Action::Check {
        return Ok(());
    }
    if options.action == snap_config::Action::Migrate {
        let database = config.database();
        std::fs::create_dir_all(database.parent().unwrap())?;
        snap_store_sqlite::migrate(
            &database,
            &[
                toml::from_str(snap_identity::MIGRATION).map_err(std::io::Error::other)?,
                toml::from_str(snap_transport::native::web::Cookies::MIGRATION)
                    .map_err(std::io::Error::other)?,
            ],
        )
        .map_err(std::io::Error::other)?;
        return Ok(());
    }
    config.load_secrets().map_err(std::io::Error::other)?;
    let sessions = testy_server::identity::open(&config.database())
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let (cookies, operations) = sessions
        .browser(
            config
                .host
                .public_origin(config.host.listen)
                .starts_with("https:"),
        )
        .map_err(std::io::Error::other)?;
    let platform = testy_server::identity::platform(sessions);
    let host = Development::new(
        platform,
        |_, key| {
            if key == testy::CEILING {
                Ok(json!(1000))
            } else {
                Err(snap_transport::execution::Error::Unavailable)
            }
        },
        |name| match name {
            "standard" => Some(testy::App::default()),
            "double-add" => Some(testy::App::with_add(|a, b| {
                a.checked_add(b.checked_mul(2)?)
            })),
            _ => None,
        },
    );
    let assets = config.assets().to_string_lossy().into_owned();
    let development = config.host.mode == snap_config::Mode::Development;
    // Validate the serving policy before bind makes TCP connections possible.
    if development && !config.host.listen.ip().is_loopback() {
        return Err(std::io::Error::other(
            "development controls require a loopback listener",
        ));
    }
    let listener = tokio::net::TcpListener::bind(config.host.listen).await?;
    let origin = config.host.public_origin(listener.local_addr()?);
    println!("Testy http://{}", listener.local_addr()?);
    web::serve_configured(
        listener,
        host,
        assets,
        origin,
        development,
        cookies.reader(),
        operations,
    )
    .await
}

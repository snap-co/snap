use snap_platform_local::{development::Development, web};
use snap_transport::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let sessions =
        testy_local::identity::open().map_err(|error| std::io::Error::other(error.to_string()))?;
    let platform = testy_local::identity::platform(sessions);
    let host = Development::new(
        platform,
        |_, key| {
            if key == testy::CEILING {
                Ok(json!(1000))
            } else {
                Err(snap_execution::Error::Unavailable)
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
    let address = std::env::var("TESTY_WEB_ADDR").unwrap_or_else(|_| "127.0.0.1:3848".into());
    let assets = std::env::var("TESTY_WEB_DIR").unwrap_or_else(|_| {
        let sibling = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("web");
        if sibling.is_dir() {
            sibling.to_string_lossy().into_owned()
        } else {
            "apps/testy/.snap/web".into()
        }
    });
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Testy http://{}", listener.local_addr()?);
    web::serve(listener, host, assets).await
}

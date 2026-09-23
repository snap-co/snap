//! Native platform entrypoint for scripted clients. The journey contains no host setup.
#[path = "../../../tests/journeys/healthy.rs"]
mod journey;

#[tokio::main]
async fn main() {
    let url = std::env::var("SNAP_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:3846".into());
    let http = snap_native::client::Http::new(&url, "healthy-journey")
        .expect("valid client configuration");
    let mut client = snap_native::client::start(healthy::client::application(), http);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        journey::run(&mut client),
    )
    .await;
    client.close().await;
    match result {
        Ok(Ok(())) => println!("PASS: native Healthy journey completed and closed"),
        other => {
            eprintln!("Healthy journey failed: {other:?}");
            std::process::exit(1);
        }
    }
}

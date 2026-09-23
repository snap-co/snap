//! Test construction adapter: expose the native SDK to the shared TypeScript contract.
use std::io::{self, BufRead, Write};
#[tokio::main]
async fn main() {
    let base = std::env::var("SNAP_BASE_URL").expect("SNAP_BASE_URL");
    let client = snap_native::client::Client::new(
        snap_native::client::Http::new(&base, "bridge").expect("valid URL"),
    );
    for line in io::stdin().lock().lines() {
        match line.expect("stdin").as_str() {
            "health.up" => {
                let result = match client.health_up().await {
                    Ok(report) => serde_json::json!({"ok": report}),
                    Err(error) => serde_json::json!({"error": error}),
                };
                println!("{result}");
                io::stdout().flush().expect("stdout");
            }
            "close" => break,
            _ => panic!("Unknown bridge operation"),
        }
    }
    client.close();
}

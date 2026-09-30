//! Pipe-protocol dependency double for the owned disposable OpenCode service.
//! Receives the production adapter's stdin and forwards actual fixture traffic.
use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<()> {
    let mut input = Vec::new();
    tokio::io::stdin().read_to_end(&mut input).await?;
    let value: Value = serde_json::from_slice(&input)?;
    let base = std::env::var("FACTORIO_FIXTURE_API").context("fixture API")?;
    let client = reqwest::Client::new();
    let mut response = if let Some(id) = value["watch"].as_str() {
        client
            .get(format!("{base}/opencode/watch/{id}"))
            .send()
            .await?
    } else {
        client
            .post(format!("{base}/opencode/request"))
            .json(&value)
            .send()
            .await?
    }
    .error_for_status()?;
    let mut stdout = tokio::io::stdout();
    while let Some(chunk) = response.chunk().await? {
        stdout.write_all(&chunk).await?;
        stdout.flush().await?;
    }
    Ok(())
}

//! Test adapter: line-delimited SDK calls and observations, without assertions.
use serde_json::{Value, json};
use snap_native::client::{Http, identity::Client};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() {
    let base = std::env::var("SNAP_BASE_URL").expect("SNAP_BASE_URL");
    let build = std::env::var("SNAP_BUILD").expect("SNAP_BUILD");
    let client = Arc::new(Client::new(
        Http::new(&base, &build).unwrap(),
        authy::client(),
    ));
    let mut observations = client.observe();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut calls = tokio::task::JoinSet::new();
    let mut watching = true;
    println!("{}", json!({"snapshot":client.snapshot()}));
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line.unwrap() else { break };
                let value: Value = serde_json::from_str(&line).unwrap();
                let client = client.clone();
                calls.spawn(async move {
                    let id = &value["id"];
                    let key = value["key"].as_str().unwrap();
                    if key == "close" { client.close().await; println!("{}", json!({"id":id,"ok":true,"value":null})); return; }
                    match client.command(key, value.get("payload").cloned()).await {
                        Ok(value) => println!("{}", json!({"id":id,"ok":true,"value":value})),
                        Err(error) => println!("{}", json!({"id":id,"ok":false,"error":error})),
                    }
                });
            }
            result = observations.changed(), if watching => {
                if result.is_err() { watching = false; }
                else { println!("{}", json!({"snapshot":observations.borrow_and_update().clone()})); }
            }
            _ = calls.join_next(), if !calls.is_empty() => {},
        }
    }
    client.close().await;
}

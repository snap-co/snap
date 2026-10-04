//! App-owned OpenCode executable contract fixture, launched by the real CLI.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{env, fs, path::PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--session") {
        println!("{}", json!({"resumed": args.get(1).context("session id")?}));
        return Ok(());
    }
    ensure!(
        args.first().map(String::as_str) == Some("api"),
        "expected api"
    );
    let method = args.get(1).context("method")?;
    let path = args.get(2).context("path")?;
    let file = PathBuf::from(env::var_os("FACTORIO_FIXTURE").context("fixture directory")?)
        .join("conversations.json");
    let mut sessions: Value = fs::read(&file)
        .ok()
        .map(|data| serde_json::from_slice(&data))
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let body: Value = if args.get(3).map(String::as_str) == Some("--data") {
        serde_json::from_str(args.get(4).context("body")?)?
    } else {
        json!({})
    };
    let id = path.split('/').nth(3).unwrap_or("");
    if method == "get" {
        ensure!(!sessions[id].is_null(), "unknown session");
        println!("{}", json!({"data": sessions[id]}));
    } else if path == "/api/session" {
        let id = body["id"].as_str().context("session id")?;
        let directory = body["location"]["directory"]
            .as_str()
            .context("session directory")?;
        sessions[id] = body.clone();
        sessions[id]["directory"] = json!(directory);
        println!("{}", json!({"data": sessions[id]}));
    } else {
        ensure!(!sessions[id].is_null(), "unknown session");
        if path.ends_with("/move") {
            sessions[id]["directory"] = json!(body["directory"].as_str().context("directory")?);
        } else if path.ends_with("/model") {
            ensure!(
                body["model"]["providerID"].is_string() && body["model"]["id"].is_string(),
                "model"
            );
            sessions[id]["model"] = body["model"].clone();
        } else if path.ends_with("/prompt") {
            ensure!(body["text"].is_string(), "prompt");
            println!("{}", json!({"data": {"id": body["id"]}}));
        } else {
            anyhow::bail!("unknown fixture endpoint {path}");
        }
    }
    fs::write(file, sessions.to_string())?;
    Ok(())
}

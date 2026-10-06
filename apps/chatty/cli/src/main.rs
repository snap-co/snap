//! External-client prototype. Models/tools run elsewhere. Credentials never enter
//! argv or normal output; declaration deliberately prints its one-time key.
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use snap_transport::{
    Command, Event, Invocation, Response,
    bearer::Change,
    native::{TcpClient, tls::ClientTls},
};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "chatty-agent",
    about = "Authy agent SSO and Chatty messaging over TCP/TLS. JSON output; no model execution."
)]
struct Args {
    #[arg(long, env = "SNAP_AUTHY_ADDR")]
    authy: String,
    #[arg(long, env = "SNAP_CHATTY_ADDR")]
    addr: Option<String>,
    #[arg(long)]
    ca_file: Option<PathBuf>,
    #[arg(long)]
    server_name: Option<String>,
    #[arg(long)]
    authy_ca_file: Option<PathBuf>,
    #[arg(long)]
    authy_server_name: Option<String>,
    #[arg(long, default_value = "chatty")]
    audience: String,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Read {email,password,name} from stdin, authenticate the human, declare an
    /// independent Authy agent. Prints {identity,key} once. Save it privately.
    Declare,
    /// Read {email,password,identity} from stdin and revoke the agent key.
    Revoke,
    /// Read {email,password,identity} from stdin and replace the agent key.
    Rotate,
    /// Authenticate using SNAP_AGENT_ID and SNAP_AGENT_KEY; print Chatty identity.
    Identity,
    List,
    /// Durable request IDs are mandatory so an explicit retry is safe.
    Post {
        thread: String,
        #[arg(long)]
        request: String,
    },
    /// One message per JSON line after a caller-managed per-thread sequence.
    /// Save the cursor after processing, not merely after reading the line.
    Watch {
        thread: String,
        #[arg(long, default_value = "0")]
        after: i64,
    },
}
fn stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(text.len() <= 64 * 1024, "Input exceeds 64 KiB");
    Ok(text)
}
fn print(value: &Value) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", serde_json::to_string(value)?)?;
    out.flush()?;
    Ok(())
}
async fn request(
    tcp: &mut TcpClient,
    name: &str,
    input: Value,
    bearer: Option<String>,
) -> Result<(Value, Option<String>)> {
    tcp.send(&Command::Request {
        bearer,
        invocation: Invocation {
            id: 1,
            operation: name.into(),
            input,
        },
    })
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut bearer = None;
        loop {
            match tcp.receive().await?.0 {
                Response::Event(Event::Bearer {
                    id: 1,
                    change: Change::Set(token),
                }) => bearer = Some(token.expose().into()),
                Response::Event(Event::Completed { id: 1, outcome }) => {
                    // Do not echo operation inputs or credential-bearing output on failure.
                    return outcome
                        .map(|value| (value, bearer))
                        .map_err(|_| anyhow::anyhow!("Request rejected: {name}"));
                }
                Response::Failed(_) => bail!("Request refused: {name}"),
                _ => {}
            }
        }
    })
    .await
    .context("Request timed out; outcome unknown")?
}
async fn receive(tcp: &mut TcpClient, client: &mut chatty::client::Client) -> Result<Response> {
    let response = tcp.receive().await?.0;
    let update = client
        .receive(response.clone())
        .map_err(|_| anyhow::anyhow!("Invalid synchronization stream"))?;
    ensure!(
        update.error.is_none(),
        "Request or session rejected; reconnect explicitly"
    );
    for command in update.send {
        tcp.send(&command).await?;
    }
    ensure!(
        !matches!(response, Response::Detached),
        "Session ended; log in again"
    );
    Ok(response)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let authy_tls = ClientTls::new(
        args.authy_ca_file.as_deref().or(args.ca_file.as_deref()),
        args.authy_server_name
            .as_deref()
            .or(args.server_name.as_deref()),
    )?;
    let mut authy = TcpClient::open(&args.authy, &authy_tls).await?;
    if matches!(
        args.command,
        Action::Declare | Action::Revoke | Action::Rotate
    ) {
        let input: Value = serde_json::from_str(&stdin()?).context("Expected JSON on stdin")?;
        let (_, bearer) = request(
            &mut authy,
            "identity.acquire",
            json!({"email":input["email"],"password":input["password"]}),
            None,
        )
        .await?;
        // Connectionless Request replies retire their physical socket. A new
        // socket carries management or an authenticated logical attachment.
        drop(authy);
        let mut authy = TcpClient::open(&args.authy, &authy_tls).await?;
        let name = match args.command {
            Action::Declare => "authy.agent-create",
            Action::Rotate => "authy.agent-rotate",
            _ => "authy.agent-revoke",
        };
        let body = if matches!(args.command, Action::Declare) {
            json!({"name":input["name"]})
        } else {
            json!({"identity":input["identity"]})
        };
        print(&request(&mut authy, name, body, bearer).await?.0)?;
        return Ok(());
    }
    let identity = std::env::var("SNAP_AGENT_ID").context("SNAP_AGENT_ID is required")?;
    let key = std::env::var("SNAP_AGENT_KEY").context("SNAP_AGENT_KEY is required")?;
    let assertion = request(
        &mut authy,
        "authy.agent-login",
        json!({"identity":identity,"key":key,"audience":args.audience}),
        None,
    )
    .await?
    .0;
    drop(authy);
    let tls = ClientTls::new(args.ca_file.as_deref(), args.server_name.as_deref())?;
    let mut tcp =
        TcpClient::open(args.addr.as_deref().context("--addr is required")?, &tls).await?;
    let (principal, bearer) = request(
        &mut tcp,
        "identity.assertion-acquire",
        json!({"assertion":assertion["assertion"]}),
        None,
    )
    .await?;
    if matches!(args.command, Action::Identity) {
        print(&principal)?;
        return Ok(());
    }
    drop(tcp);
    let mut tcp =
        TcpClient::open(args.addr.as_deref().context("--addr is required")?, &tls).await?;
    let mut client =
        chatty::client::Client::new().map_err(|_| anyhow::anyhow!("Cannot initialize replica"))?;
    if let Action::Watch { thread, after } = &args.command {
        ensure!(*after >= 0, "--after must be nonnegative");
        client
            .select(Some(thread.clone()))
            .map_err(|_| anyhow::anyhow!("Invalid thread"))?;
    }
    tcp.send(&client.connect(
        bearer.context("Missing session credential")?,
        format!("chatty-agent-{}", std::process::id()),
    ))
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !client.ready() {
            receive(&mut tcp, &mut client).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Synchronization timed out")??;
    match args.command {
        Action::List => print(&json!(
            client
                .threads()
                .map_err(|_| anyhow::anyhow!("Cannot read threads"))?
        ))?,
        Action::Post {
            thread,
            request: id,
        } => {
            let command = client
                .invoke(
                    "chatty.send",
                    json!({"thread_id":thread,"request_id":id,"message":stdin()?}),
                )
                .map_err(|_| anyhow::anyhow!("Invalid message"))?;
            let Command::Invoke(call) = &command else {
                unreachable!()
            };
            let id = call.id;
            tcp.send(&command).await?;
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                loop {
                    if let Response::Event(Event::Completed { id: found, outcome }) =
                        receive(&mut tcp, &mut client).await?
                        && found == id
                    {
                        print(&outcome.map_err(|_| anyhow::anyhow!("Message rejected"))?)?;
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            })
            .await
            .context("Post timed out; outcome unknown. Reuse --request for an explicit retry")??;
        }
        Action::Watch { thread, mut after } => loop {
            ensure!(
                client
                    .threads()
                    .map_err(|_| anyhow::anyhow!("Cannot read threads"))?
                    .iter()
                    .any(|t| t.id == thread),
                "Thread unavailable or access revoked"
            );
            for message in client
                .messages()
                .map_err(|_| anyhow::anyhow!("Cannot read messages"))?
            {
                if message.thread == thread && message.sequence > after {
                    print(&json!(message))?;
                    after = message.sequence;
                }
            }
            receive(&mut tcp, &mut client).await?;
        },
        _ => unreachable!(),
    }
    Ok(())
}

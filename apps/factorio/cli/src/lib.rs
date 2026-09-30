//! Client commands and server-option parsing for the native Factorio executable.
//! Ordinary commands detach TCP, never retire the saved logical lifetime.
mod client;
mod credentials;
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use client::Client;
use credentials::Locked;
use factorio::Command;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "factorio",
    about = "Factorio client and server. Human candidate approval remains in the browser."
)]
struct Args {
    /// TLS endpoint as host:port. A new login uses checkout defaults, then 127.0.0.1:1024.
    #[arg(long, global = true, env = "FACTORIO_ADDR")]
    addr: Option<String>,
    /// PEM CA bundle instead of standard public roots. Saved with credentials.
    #[arg(long, global = true, env = "FACTORIO_CA_FILE")]
    ca_file: Option<PathBuf>,
    /// Verify this certificate name instead of the endpoint host, for tunnels.
    #[arg(long, global = true, env = "FACTORIO_SERVER_NAME")]
    server_name: Option<String>,
    #[arg(long, global = true)]
    credentials: Option<PathBuf>,
    #[arg(long, global = true, env = "FACTORIO_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[arg(long, global = true, env = "FACTORIO_WORKSPACE")]
    workspace: Option<String>,
    #[command(subcommand)]
    command: Mode,
}
#[derive(Subcommand)]
enum Mode {
    /// Run the HTTP/browser and verified TLS TCP server.
    Serve {
        /// Config override. Defaults to packaged config or the checkout's development profile.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Validate configuration without decrypting secrets or starting listeners.
        #[arg(long, conflicts_with = "migrate")]
        check_config: bool,
        /// Apply database migrations without starting listeners.
        #[arg(long)]
        migrate: bool,
    },
    /// Save login for up to 30 days via browser approval or an agent token.
    Login,
    /// Revoke this CLI credential and retire its logical connection.
    Logout,
    /// Recover the exact interrupted invocation, only on its retained lifetime.
    Retry,
    Status,
    Repositories,
    Workspaces,
    Onboard {
        #[arg(long, default_value = "configured")]
        repository: String,
    },
    Ticket {
        file: PathBuf,
    },
    DeleteTicket {
        id: String,
    },
    Start {
        #[arg(long)]
        id: Option<String>,
        #[arg(long, value_delimiter = ',', required = true)]
        modules: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        tickets: Vec<String>,
        #[arg(long)]
        conversation: Option<String>,
        #[arg(last = true, required = true)]
        intent: Vec<String>,
    },
    Publish {
        id: String,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        findings: Option<PathBuf>,
    },
    Expand {
        id: String,
        #[arg(long, value_delimiter = ',', required = true)]
        modules: Vec<String>,
    },
    Accept {
        id: String,
    },
    Recover {
        id: String,
    },
    Cleanup {
        id: String,
    },
    Abandon {
        id: String,
    },
    Path {
        id: String,
    },
    Intake {
        #[arg(long)]
        resume: Option<String>,
        /// Create/read the intake without contacting or launching OpenCode.
        #[arg(long)]
        no_open: bool,
        #[arg(last = true)]
        description: Vec<String>,
    },
    IntakeRead {
        #[arg(long)]
        intake: String,
    },
    IntakeSave {
        file: PathBuf,
        #[arg(long)]
        intake: String,
    },
    IntakeReady {
        #[arg(long)]
        intake: String,
        #[arg(long)]
        revision: u32,
    },
    IntakeDelete {
        #[arg(long)]
        intake: String,
    },
    /// Invoke any registered operation with JSON input. Files and stdin use @path or @-.
    Invoke {
        operation: String,
        #[arg(default_value = "{}")]
        input: String,
    },
    /// Stream Document SDK snapshots as JSON lines until Ctrl-C.
    Watch,
}
fn read_json(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn print(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

struct DevelopmentEndpoint {
    addr: String,
    ca_file: Option<PathBuf>,
    server_name: Option<String>,
}
impl DevelopmentEndpoint {
    fn read() -> Result<Option<Self>> {
        #[derive(serde::Deserialize)]
        struct Application {
            tcp: Tcp,
        }
        #[derive(serde::Deserialize)]
        struct Tcp {
            #[serde(default = "listen")]
            listen: std::net::SocketAddr,
            ca_file: Option<PathBuf>,
            server_name: Option<String>,
        }
        fn listen() -> std::net::SocketAddr {
            std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 1024))
        }
        let Some(path) = snap_config::application_development_config("factorio")? else {
            return Ok(None);
        };
        let config = snap_config::Config::<Application>::read(&path)?;
        ensure!(
            config.host.mode == snap_config::Mode::Development,
            "Checkout login defaults require development configuration"
        );
        let tcp = &config.app.tcp;
        let mut address = tcp.listen;
        if address.ip().is_unspecified() {
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        ensure!(
            address.port() != 0,
            "Set a fixed development TCP port before using factorio login"
        );
        Ok(Some(Self {
            addr: address.to_string(),
            ca_file: tcp.ca_file.as_ref().map(|path| config.path(path)),
            server_name: tcp.server_name.clone(),
        }))
    }
}

async fn opencode_api(exe: &str, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let mut command = tokio::process::Command::new(exe);
    command
        .args(["api", method, path])
        .env_remove("SNAP_MASTER_KEY")
        .kill_on_drop(true);
    if let Some(body) = body {
        command.args(["--data", &body.to_string()]);
    }
    let output =
        tokio::time::timeout(std::time::Duration::from_secs(40), command.output()).await??;
    ensure!(
        output.status.success(),
        "OpenCode API failed. The intake is saved; use --resume after configuring OpenCode"
    );
    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
async fn open_intake(
    item: factorio::intake::Intake,
    workspace: factorio::Workspace,
    path: &Path,
    workspace_id: &str,
) -> Result<()> {
    let exe = std::env::var("FACTORIO_OPENCODE").unwrap_or_else(|_| "opencode".into());
    let route = format!("/api/session/{}", item.conversation);
    let existing = match opencode_api(&exe,"get",&route,None).await {
        Ok(value) => value,
        Err(_) => opencode_api(&exe, "post", "/api/session", Some(json!({"id":item.conversation,"title":format!("Intake: {}",item.description),"location":{"directory":workspace.config.repository},"metadata":{"factorio_intake":item.id,"factorio_workspace":workspace_id},"permissions":[{"action":"edit","resource":"*","effect":"deny"}]}))).await?,
    };
    ensure!(
        existing["data"]["location"]["directory"] == workspace.config.repository
            && existing["data"]["metadata"]["factorio_intake"] == item.id
            && existing["data"]["metadata"]["factorio_workspace"] == workspace_id,
        "OpenCode session ownership or directory changed"
    );
    let executable = std::env::current_exe()?;
    let credentials = std::fs::canonicalize(path)?;
    let guide = include_str!("../../INTAKE.md")
        .replace(
            "factory intake-read",
            &format!(
                "{} intake-read --credentials {} --intake {}",
                quote(&executable),
                quote(&credentials),
                item.id
            ),
        )
        .replace(
            "factory intake-save -",
            &format!(
                "{} intake-save - --credentials {} --intake {}",
                quote(&executable),
                quote(&credentials),
                item.id
            ),
        );
    let text = format!(
        "{guide}\n\nUser request:\n{}\nWorkspace: {workspace_id}. Modules: {}.",
        item.description,
        serde_json::to_string(&workspace.config.modules)?
    );
    opencode_api(&exe,"post",&format!("{route}/prompt"),Some(json!({"id":format!("msg_{}_initial",item.id),"text":text,"metadata":{"factorio_initial":true}}))).await?;
    let status = tokio::process::Command::new(exe)
        .args(["--session", &item.conversation])
        .env_remove("SNAP_MASTER_KEY")
        .status()
        .await?;
    ensure!(status.success(), "OpenCode exited with {status}");
    Ok(())
}

async fn run(mut args: Args) -> Result<()> {
    args.token = args.token.filter(|token| !token.is_empty());
    let default_addr = args.addr.clone().unwrap_or_else(|| "127.0.0.1:1024".into());
    let login = matches!(args.command, Mode::Login);
    // Explicit files carry their own endpoint. Environment tokens use a private
    // per-token state file to keep counters stable across independent processes.
    let path = match args.credentials.clone() {
        Some(path) => path,
        None => credentials::default_path(
            &default_addr,
            if login { None } else { args.token.as_deref() },
        )?,
    };
    let seed = if login {
        Some("")
    } else if args.credentials.is_none() {
        args.token.as_deref()
    } else {
        None
    };
    let mut credentials = Locked::open(&path, seed)?;
    // Infer checkout trust only for a fresh default login. Explicit endpoints and
    // saved logins, including uncertain outcomes, keep their own authority and trust.
    let development = if login && args.addr.is_none() && credentials.value.addr.is_none() {
        DevelopmentEndpoint::read()?
    } else {
        None
    };
    let addr = args
        .addr
        .or(credentials.value.addr.clone())
        .or_else(|| development.as_ref().map(|endpoint| endpoint.addr.clone()))
        .unwrap_or(default_addr);
    let development_ca = if args.ca_file.is_none() && credentials.value.ca_file.is_none() {
        development
            .as_ref()
            .and_then(|endpoint| endpoint.ca_file.clone())
            .map(std::fs::canonicalize)
            .transpose()?
    } else {
        None
    };
    let ca_file = args
        .ca_file
        .map(std::fs::canonicalize)
        .transpose()?
        .or(credentials.value.ca_file.clone())
        .or(development_ca);
    let server_name = args
        .server_name
        .or(credentials.value.server_name.clone())
        .or_else(|| {
            development
                .as_ref()
                .and_then(|endpoint| endpoint.server_name.clone())
        });
    if !login && credentials.value.pending.is_some() {
        ensure!(
            credentials
                .value
                .addr
                .as_deref()
                .is_none_or(|old| old == addr)
                && credentials.value.ca_file == ca_file
                && credentials.value.server_name == server_name,
            "Cannot change endpoint while an invocation has an unknown outcome. Retry at its original endpoint, or login to start a fresh lifetime"
        );
    }
    let tls =
        snap_transport_native::tls::ClientTls::new(ca_file.as_deref(), server_name.as_deref())?;
    if login {
        // Proposed login settings remain local until issuance succeeds. A failed
        // login must leave the original pending invocation and its trust intact.
        async fn exchange(
            addr: &str,
            tls: &snap_transport_native::tls::ClientTls,
            operation: &str,
            input: Value,
            bearer: Option<String>,
        ) -> Result<Value> {
            let mut tcp = snap_transport_native::Client::open(addr, tls).await?;
            tcp.send(&snap_transport::Command::Request {
                bearer,
                invocation: snap_transport::Invocation {
                    id: 1,
                    operation: operation.into(),
                    input,
                },
            })
            .await?;
            client::outcome(&mut tcp, 1).await
        }
        let issued = if let Some(token) = args.token {
            exchange(&addr, &tls, "factorio.login", Value::Null, Some(token)).await?
        } else {
            let start = exchange(&addr, &tls, "factorio.login-start", json!({}), None).await?;
            eprintln!(
                "Open {}\nRequest code: {}\nApprove only this request. Waiting for browser approval...",
                start["url"].as_str().context("Missing login URL")?,
                start["code"].as_str().context("Missing request code")?
            );
            let began = std::time::Instant::now();
            loop {
                ensure!(
                    began.elapsed() < std::time::Duration::from_secs(300),
                    "Login approval expired. Run factorio login again"
                );
                let issued = exchange(
                    &addr,
                    &tls,
                    "factorio.login-finish",
                    json!({"code":start["code"],"proof":start["proof"]}),
                    None,
                )
                .await?;
                if !issued.is_null() {
                    break issued;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        };
        credentials.value.bearer = issued["bearer"]
            .as_str()
            .context("Missing credential")?
            .into();
        credentials.value.addr = Some(addr);
        credentials.value.ca_file = ca_file;
        credentials.value.server_name = server_name;
        if let Some(workspace) = args.workspace {
            credentials.value.workspace = workspace;
        }
        credentials.value.expires = issued["expires"].as_i64();
        credentials.value.client_id = uuid::Uuid::new_v4().to_string();
        credentials.value.next_id = 1;
        credentials.value.pending = None;
        credentials.value.pending_replayable = true;
        credentials.value.lifetime = None;
        credentials.save()?;
        return print(
            &json!({"logged_in":true,"owner":issued["owner"],"expires":issued["expires"],"credentials":path,"note":"Login lasts up to 30 days, bounded by your Authy session. Factorio refreshes OAuth automatically. CLI credentials cannot approve candidates. Run login again after expiry or revocation."}),
        );
    }
    credentials.value.addr = Some(addr.clone());
    credentials.value.ca_file = ca_file;
    credentials.value.server_name = server_name;
    if let Some(workspace) = args.workspace {
        credentials.value.workspace = workspace;
    }
    credentials.save()?;
    let credentials = if matches!(args.command, Mode::Watch) {
        credentials.isolated()
    } else {
        credentials
    };
    let mut client = Client::connect(&addr, &tls, credentials).await?;
    let result = match args.command {
        Mode::Login | Mode::Serve { .. } => unreachable!(),
        Mode::Retry => client.retry().await?,
        Mode::Logout => {
            let value = client.invoke("factorio.logout", json!({})).await?;
            let _ = client.tcp.send(&snap_transport::Command::Close).await;
            client.credentials.value.bearer.clear();
            client.credentials.save()?;
            value
        }
        Mode::Status => serde_json::to_value(client.workspace().await?)?,
        Mode::Repositories => client.invoke("factorio.repositories", json!({})).await?,
        Mode::Workspaces => client.invoke("factorio.workspaces", json!({})).await?,
        Mode::Onboard { repository } => {
            let value = client
                .invoke("factorio.onboard", json!({"repository":repository}))
                .await?;
            client.credentials.value.workspace =
                value["id"].as_str().context("Missing workspace")?.into();
            client.credentials.save()?;
            serde_json::to_value(client.workspace().await?)?
        }
        Mode::Invoke { operation, input } => {
            client
                .invoke(
                    &operation,
                    if let Some(file) = input.strip_prefix('@') {
                        read_json(Path::new(file))?
                    } else {
                        serde_json::from_str(&input)?
                    },
                )
                .await?
        }
        Mode::Watch => {
            let identity = client.invoke("factorio.identity", json!({})).await?;
            let mut documents = snap_document::client::Client::new(
                identity["owner"].as_str().context("Missing owner")?.into(),
            );
            let registry = factorio::documents::registry();
            let response = client
                .invoke(
                    "document.manifest",
                    serde_json::to_value(documents.manifest())?,
                )
                .await?;
            documents
                .handle(&registry, serde_json::from_value(response)?)
                .map_err(|e| anyhow::anyhow!("Document: {e:?}"))?;
            println!("{}", serde_json::to_string(&documents.view())?);
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {let _=client.tcp.send(&snap_transport::Command::Close).await;return Ok(());},
                    response = client.tcp.receive() => {
                        match response?.0 {
                            snap_transport::Response::Notification { operation, input } if operation == "document" => {
                                let outcome = documents.handle(&registry,serde_json::from_value(input)?).map_err(|e| anyhow::anyhow!("Document: {e:?}"))?;
                                if matches!(outcome, snap_document::client::Outcome::NeedManifest { .. }) {
                                    let response = client.invoke("document.manifest",serde_json::to_value(documents.manifest())?).await?;
                                    documents.handle(&registry,serde_json::from_value(response)?).map_err(|e| anyhow::anyhow!("Document: {e:?}"))?;
                                }
                                println!("{}",serde_json::to_string(&documents.view())?);
                            },
                            other => bail!("Watch ended: {other:?}"),
                        }
                    }
                }
            }
        }
        Mode::Intake {
            resume,
            no_open,
            description,
        } => {
            let workspace_id = client.workspace_id().await?;
            let id = resume
                .clone()
                .unwrap_or_else(|| format!("intake-{}", uuid::Uuid::new_v4().simple()));
            let item: factorio::intake::Intake = if resume.is_some() {
                client
                    .workspace()
                    .await?
                    .intakes
                    .remove(&id)
                    .context("Intake not found")?
            } else {
                ensure!(
                    !description.is_empty(),
                    "Supply -- <description>, or --resume <id>"
                );
                serde_json::from_value(client.invoke("factorio.intake-create",json!({"workspace":workspace_id,"id":id,"description":description.join(" ")})).await?)?
            };
            eprintln!("Intake {id}. Resume with factorio intake --resume {id}");
            if no_open {
                serde_json::to_value(item)?
            } else {
                let workspace = client.workspace().await?;
                drop(client);
                return open_intake(item, workspace, &path, &workspace_id).await;
            }
        }
        Mode::IntakeRead { intake } => {
            let workspace = client.workspace_id().await?;
            client
                .invoke(
                    "factorio.intake-read",
                    json!({"workspace":workspace,"id":intake}),
                )
                .await?
        }
        Mode::IntakeSave { intake, file } => {
            let workspace = client.workspace_id().await?;
            client
                .invoke(
                    "factorio.intake-drafts",
                    json!({"workspace":workspace,"id":intake,"drafts":read_json(&file)?}),
                )
                .await?
        }
        Mode::IntakeReady { intake, revision } => {
            let workspace = client.workspace_id().await?;
            client
                .invoke(
                    "factorio.intake-ready",
                    json!({"workspace":workspace,"id":intake,"revision":revision}),
                )
                .await?
        }
        Mode::IntakeDelete { intake } => {
            let workspace = client.workspace_id().await?;
            client
                .invoke(
                    "factorio.intake-delete",
                    json!({"workspace":workspace,"id":intake}),
                )
                .await?
        }
        Mode::Path { id } => {
            let workspace = client.workspace().await?;
            println!(
                "{}",
                workspace
                    .sessions
                    .get(&id)
                    .context("Session not found")?
                    .worktree
            );
            return Ok(());
        }
        command => {
            let mut started = None;
            let command = match command {
                Mode::Ticket { file } => Command::Ticket {
                    ticket: serde_json::from_value(read_json(&file)?)?,
                },
                Mode::DeleteTicket { id } => Command::DeleteTicket { id },
                Mode::Start {
                    id,
                    modules,
                    tickets,
                    conversation,
                    intent,
                } => {
                    let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    started = Some(id.clone());
                    let workspace = client.workspace_id().await?;
                    let conversation = conversation.unwrap_or_else(|| {
                        format!(
                            "ses_{}",
                            factorio::documents::child_id(
                                &workspace,
                                factorio::documents::SESSION_KIND,
                                &id
                            )
                        )
                    });
                    Command::Start {
                        id,
                        modules,
                        tickets,
                        conversation,
                        prompt: intent.join(" "),
                        base: String::new(),
                    }
                }
                Mode::Publish {
                    id,
                    evidence,
                    findings,
                } => Command::Publish {
                    id,
                    evidence: std::fs::read_to_string(evidence)?,
                    findings: findings
                        .map(|p| read_json(&p).and_then(|v| Ok(serde_json::from_value(v)?)))
                        .transpose()?
                        .unwrap_or_default(),
                },
                Mode::Expand { id, modules } => Command::Expand { id, modules },
                Mode::Accept { id } => Command::Accept { id },
                Mode::Recover { id } | Mode::Cleanup { id } => Command::Recover { id },
                Mode::Abandon { id } => Command::Abandon { id },
                _ => unreachable!(),
            };
            let workspace = client.command(command).await?;
            if let Some(id) = started {
                json!({"session":workspace.sessions.get(&id),"next":"Move your OpenCode session to the worktree. Publish evidence, then obtain human approval in the browser before accept."})
            } else {
                serde_json::to_value(workspace)?
            }
        }
    };
    print(&result)
}
/// Execute a client command, or return server startup options to the host.
/// Server dispatch happens before any client credential file or connection is opened.
pub async fn dispatch() -> Result<Option<snap_config::Options>> {
    let args = Args::parse();
    if let Mode::Serve {
        config,
        check_config,
        migrate,
    } = args.command
    {
        return Ok(Some(snap_config::Options {
            config: match config {
                Some(config) => config,
                None => snap_config::application_config("factorio")?,
            },
            action: if check_config {
                snap_config::Action::Check
            } else if migrate {
                snap_config::Action::Migrate
            } else {
                snap_config::Action::Serve
            },
        }));
    }
    run(args).await?;
    Ok(None)
}

use crate::credentials::Locked;
use anyhow::{Result, bail, ensure};
use snap_transport::{Command, Event, Invocation, Response, Value};
use snap_transport_tcp::Client as Tcp;
use std::time::Duration;

pub struct Client {
    pub tcp: Tcp,
    pub credentials: Locked,
    pub resumed: bool,
}
#[derive(Debug)]
pub struct OperationError(pub snap_transport::Error);
impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Operation rejected: {}",
            serde_json::to_string(&self.0).unwrap_or_default()
        )
    }
}
impl std::error::Error for OperationError {}
impl Client {
    pub async fn connect(
        addr: &str,
        tls: &snap_transport_tcp::tls::ClientTls,
        mut credentials: Locked,
    ) -> Result<Self> {
        let start = std::time::Instant::now();
        loop {
            let mut tcp = Tcp::open(addr, tls).await?;
            tcp.send(&Command::Connect {
                bearer: credentials.value.bearer.clone(),
                client_id: credentials.value.client_id.clone(),
            })
            .await?;
            let (reply, info) =
                tokio::time::timeout(Duration::from_secs(65), tcp.receive()).await??;
            match reply {
                Response::Attached { resumed } => {
                    let info =
                        info.ok_or_else(|| anyhow::anyhow!("Missing logical lifetime identity"))?;
                    ensure!(!info.lifetime.is_empty(), "Empty logical lifetime identity");
                    if credentials.value.pending.is_some() {
                        // Persist the fence: a second attempt must not mistake
                        // this newly-created lifetime for the original one.
                        if !resumed
                            || credentials.value.lifetime.as_deref() != Some(info.lifetime.as_str())
                        {
                            credentials.value.pending_replayable = false;
                        }
                    } else {
                        credentials.value.lifetime = Some(info.lifetime);
                    }
                    credentials.save()?;
                    return Ok(Self {
                        tcp,
                        credentials,
                        resumed,
                    });
                }
                // A previous CLI dropped TCP before releasing the file lock, but
                // the server may not have consumed its EOF yet. No invocation or
                // credential issuance is replayed by retrying this handshake.
                Response::Failed(snap_transport::Error::Occupied)
                    if start.elapsed() < Duration::from_secs(1) =>
                {
                    drop(tcp);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                _ => bail!("Attachment rejected: {reply:?}. Run factorio login again"),
            }
        }
    }
    pub async fn invoke(&mut self, name: &str, input: Value) -> Result<Value> {
        ensure!(
            self.credentials.value.pending.is_none(),
            "An earlier invocation has an unknown outcome. Run factorio retry, or factorio login to start a new lifetime without replay"
        );
        let id = self.credentials.reserve()?;
        let invocation = Invocation {
            id,
            operation: name.into(),
            input,
        };
        snap_transport::binary::command(&Command::Invoke(invocation.clone()))
            .map_err(|e| anyhow::anyhow!("Cannot encode invocation: {e:?}"))?;
        self.credentials.value.pending = Some(invocation.clone());
        self.credentials.value.pending_replayable = true;
        self.credentials.save()?;
        self.send_invocation(invocation).await
    }
    pub async fn retry(&mut self) -> Result<Value> {
        ensure!(
            self.resumed && self.credentials.value.pending_replayable,
            "Logical lifetime ended. The old outcome is unknown; login starts a fresh lifetime without replay"
        );
        let invocation = self
            .credentials
            .value
            .pending
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No interrupted invocation to retry"))?;
        self.send_invocation(invocation).await
    }
    async fn send_invocation(&mut self, invocation: Invocation) -> Result<Value> {
        let id = invocation.id;
        self.tcp.send(&Command::Invoke(invocation)).await?;
        let result = receive_outcome(&mut self.tcp, id).await?;
        self.credentials.value.pending = None;
        self.credentials.save()?;
        result.map_err(|e| OperationError(e).into())
    }
    pub async fn workspace_id(&mut self) -> Result<String> {
        if self.credentials.value.workspace.is_empty() {
            let list: Vec<factorio::client::WorkspaceSummary> = serde_json::from_value(
                self.invoke("factorio.workspaces", serde_json::json!({}))
                    .await?,
            )?;
            ensure!(
                list.len() == 1,
                "Select --workspace <id>, or run factorio onboard to create your first workspace"
            );
            self.credentials.value.workspace = list[0].id.clone();
            self.credentials.save()?;
        }
        Ok(self.credentials.value.workspace.clone())
    }
    pub async fn workspace(&mut self) -> Result<factorio::Workspace> {
        let workspace = self.workspace_id().await?;
        Ok(serde_json::from_value(
            self.invoke(
                "factorio.workspace",
                serde_json::to_value(factorio::client::WorkspaceInput {
                    workspace: &workspace,
                })?,
            )
            .await?,
        )?)
    }
    pub async fn command(&mut self, command: factorio::Command) -> Result<factorio::Workspace> {
        let workspace = self.workspace_id().await?;
        let id = match &command {
            factorio::Command::Ticket { ticket } => ticket.id.clone(),
            factorio::Command::Start { id, .. }
            | factorio::Command::Publish { id, .. }
            | factorio::Command::DeleteTicket { id }
            | factorio::Command::Expand { id, .. }
            | factorio::Command::Approve { id, .. }
            | factorio::Command::Recover { id }
            | factorio::Command::Accept { id }
            | factorio::Command::Abandon { id } => id.clone(),
        };
        let result = self
            .invoke(
                "factorio.command",
                serde_json::to_value(factorio::client::CommandInput {
                    workspace: &workspace,
                    command,
                })?,
            )
            .await;
        if let Err(error) = result {
            if matches!(error.downcast_ref::<OperationError>(),Some(OperationError(snap_transport::Error::Application(value))) if value["code"]=="Blocked" && value["committed"]==true)
            {
                let state = self.workspace().await?;
                bail!(
                    "{}",
                    state
                        .sessions
                        .get(&id)
                        .map(|s| s.error.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Controller blocked; inspect the session and recover")
                );
            }
            return Err(error);
        }
        self.workspace().await
    }
}

pub async fn outcome(tcp: &mut Tcp, id: u64) -> Result<Value> {
    receive_outcome(tcp, id)
        .await?
        .map_err(|e| OperationError(e).into())
}
async fn receive_outcome(tcp: &mut Tcp, id: u64) -> Result<snap_transport::Outcome> {
    let mut accepted = false;
    loop {
        // Controller work can be long-running. Timeout only when no observations
        // arrive for ten minutes; never replay an unknown result.
        let (response, _) = tokio::time::timeout(Duration::from_secs(600), tcp.receive()).await??;
        match response {
            // One event per frame. Acceptance and progress arrive as their own
            // frames while the operation is still running, which is what keeps a
            // long controller from looking hung.
            Response::Event(Event::Accepted { id: received }) => {
                ensure!(received == id, "Unexpected invocation");
                accepted = true;
            }
            Response::Event(Event::Progress {
                id: received,
                value,
            }) => {
                ensure!(received == id && accepted, "Unaccepted progress");
                eprintln!("{value}");
            }
            Response::Event(Event::Bearer { .. }) => {
                bail!("Unexpected bearer update on an attached operation")
            }
            Response::Event(Event::Completed {
                id: received,
                outcome,
            }) => {
                ensure!(received == id, "Unexpected completion");
                return Ok(outcome);
            }
            // Uncorrelated push. This CLI drives one operation at a time and
            // has no handler registered, so it is dropped rather than fatal.
            Response::Global { .. } => {}
            Response::Failed(error) => bail!("Transport rejected: {error:?}"),
            _ => bail!("Logical connection ended; outstanding outcome is unknown"),
        }
    }
}

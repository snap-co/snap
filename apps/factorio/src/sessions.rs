//! Session intent, candidate approval, repository claims and resource observations.
use crate::*;

pub fn definition() -> snap_document::Definition {
    snap_document::Definition {
        kind: workspaces::SESSION_KIND.into(),
        version: "1".into(),
        validate: |v| serde_json::from_value::<workspaces::Child<Session>>(v.clone()).is_ok(),
        mutations: vec![],
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Active,
    Published,
    Integrating,
    Cleanup,
    Complete,
    Abandoning,
    Abandoned,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub commit: String,
    pub target: String,
    pub evidence: String,
    pub findings: Vec<Finding>,
    pub approval: Option<Approval>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub text: String,
    pub disposition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub human: String,
    pub at: i64,
    pub commit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    pub owner: String,
    pub prompt: String,
    pub tickets: Vec<String>,
    pub modules: Vec<String>,
    pub phase: Phase,
    pub base: String,
    pub branch: String,
    pub worktree: String,
    pub data: String,
    pub port: u16,
    pub conversation: String,
    pub candidate: Option<Candidate>,
    pub publications: Vec<Candidate>,
    /// Persisted before mainline changes; recovery checks ancestry of this exact OID.
    pub integration: Option<String>,
    pub error: String,
    #[serde(default)]
    pub desired: Desired,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Desired {
    #[default]
    Active,
    Published {
        evidence: String,
        findings: Vec<Finding>,
    },
    Integrated,
    Abandoned,
}
impl Session {
    pub fn claims(&self) -> bool {
        !matches!(self.phase, Phase::Complete | Phase::Abandoned)
    }
}
fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|m| m == "*" || b.iter().any(|n| n == "*" || m == n))
}
fn oid(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Pure desired-state transition, shared by admission guards and transactional
/// handlers. No credentials, Store writes or host effects run here.
pub fn transition(
    mut w: Workspace,
    owner: &str,
    human: bool,
    now: i64,
    cmd: Command,
) -> Result<Workspace, Error> {
    match cmd {
        Command::Recover { id } => {
            w.sessions
                .get_mut(&id)
                .ok_or(Error::NotFound)?
                .error
                .clear();
        }
        Command::Publish {
            id,
            evidence,
            findings,
        } => {
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if !matches!(s.phase, Phase::Active | Phase::Published)
                || evidence.trim().is_empty()
                || evidence.len() > 32768
                || findings.len() > 128
                || findings
                    .iter()
                    .any(|f| f.text.len() + f.disposition.len() > 8192)
            {
                return Err(Error::Constraint);
            }
            s.desired = Desired::Published { evidence, findings };
            if let Some(previous) = s.candidate.take() {
                s.publications.push(previous);
            }
            s.phase = Phase::Active;
        }
        Command::Accept { id } => {
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if s.phase != Phase::Published
                || s.candidate
                    .as_ref()
                    .is_none_or(|c| c.approval.as_ref().is_none_or(|a| a.commit != c.commit))
            {
                return Err(Error::Constraint);
            }
            s.desired = Desired::Integrated;
        }
        Command::Ticket { ticket } => tickets::edit(&mut w, ticket, now)?,
        Command::DeleteTicket { id } => tickets::delete(&mut w, &id)?,
        Command::Start {
            id,
            prompt,
            tickets,
            mut modules,
            base,
            conversation,
        } => {
            if !valid_id(&id)
                || (!base.is_empty() && !oid(&base))
                || prompt.trim().is_empty()
                || prompt.len() > 32768
                || !conversation.starts_with("ses")
                || !valid_id(&conversation)
            {
                return Err(Error::Invalid);
            }
            if w.sessions.contains_key(&id) {
                return Err(Error::Constraint);
            }
            for id in &tickets {
                let t = w.tickets.get(id).ok_or(Error::NotFound)?;
                if !actionable(&w, t) {
                    return Err(Error::Constraint);
                }
                for m in &t.modules {
                    if !modules.contains(m) {
                        modules.push(m.clone());
                    }
                }
            }
            scope(&w, &modules)?;
            if w.sessions
                .values()
                .any(|s| s.claims() && overlaps(&s.modules, &modules))
            {
                return Err(Error::Constraint);
            }
            let port = u16::try_from(w.next_port).map_err(|_| Error::Constraint)?;
            w.next_port += 1;
            w.sessions.insert(
                id.clone(),
                Session {
                    id: id.clone(),
                    created_at: Some(now),
                    owner: owner.into(),
                    prompt,
                    tickets,
                    modules,
                    phase: Phase::Starting,
                    base,
                    branch: alloc::format!("factorio/{id}"),
                    worktree: alloc::format!("{}/worktrees/{id}", w.config.resources),
                    data: alloc::format!("{}/data/{id}", w.config.resources),
                    port,
                    conversation,
                    candidate: None,
                    publications: vec![],
                    integration: None,
                    error: String::new(),
                    desired: Desired::Active,
                },
            );
        }
        Command::Expand { id, modules } => {
            scope(&w, &modules)?;
            if w.sessions
                .values()
                .any(|s| s.id != id && s.claims() && overlaps(&s.modules, &modules))
            {
                return Err(Error::Constraint);
            }
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if !matches!(s.phase, Phase::Active | Phase::Published) {
                return Err(Error::Constraint);
            }
            if let Some(previous) = s.candidate.take() {
                s.publications.push(previous);
            }
            s.phase = Phase::Active;
            s.desired = Desired::Active;
            for m in modules {
                if !s.modules.contains(&m) {
                    s.modules.push(m);
                }
            }
        }
        Command::Approve { id, commit } => {
            if !human {
                return Err(Error::Constraint);
            }
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if s.phase != Phase::Published {
                return Err(Error::Constraint);
            }
            let c = s.candidate.as_mut().ok_or(Error::Constraint)?;
            if c.commit != commit || c.findings.iter().any(|f| f.disposition.trim().is_empty()) {
                return Err(Error::Constraint);
            }
            c.approval = Some(Approval {
                human: owner.into(),
                at: now,
                commit,
            });
        }
        Command::Abandon { id } => {
            let s = w.sessions.get_mut(&id).ok_or(Error::NotFound)?;
            if !matches!(
                s.phase,
                Phase::Starting | Phase::Active | Phase::Published | Phase::Abandoning
            ) {
                return Err(Error::Constraint);
            }
            s.phase = Phase::Abandoning;
            s.desired = Desired::Abandoned;
        }
    }
    Ok(w)
}

/// Host-only observations of committed effect intent. Never deserialize this enum
/// from a public request. Claims persist through failures and cleanup retries.
pub enum Effect {
    Based(String),
    Started,
    Published {
        commit: String,
        target: String,
        evidence: String,
        findings: Vec<Finding>,
    },
    Integrating {
        commit: String,
    },
    Integrated,
    Cleaned,
    Failed(String),
}
/// Pure controller observation. Hosts persist these only after the corresponding
/// resource check/effect; failures leave desired state and resource claims intact.
pub fn observe(mut w: Workspace, id: &str, effect: Effect) -> Result<Workspace, Error> {
    if matches!(effect, Effect::Integrating { .. })
        && w.sessions
            .values()
            .any(|s| s.id != id && s.phase == Phase::Integrating)
    {
        return Err(Error::Constraint);
    }
    let s = w.sessions.get_mut(id).ok_or(Error::NotFound)?;
    match effect {
        Effect::Based(commit) => {
            if s.phase != Phase::Starting || !s.base.is_empty() || !oid(&commit) {
                return Err(Error::Constraint);
            }
            s.base = commit;
        }
        Effect::Started => {
            if s.phase != Phase::Starting {
                return Err(Error::Constraint);
            }
            s.phase = Phase::Active;
        }
        Effect::Published {
            commit,
            target,
            evidence,
            findings,
        } => {
            if !matches!(s.phase, Phase::Active | Phase::Published)
                || !oid(&commit)
                || !oid(&target)
                || evidence.trim().is_empty()
                || evidence.len() > 32768
            {
                return Err(Error::Constraint);
            }
            if let Some(previous) = s.candidate.take() {
                s.publications.push(previous);
            }
            s.candidate = Some(Candidate {
                commit,
                target,
                evidence,
                findings,
                approval: None,
            });
            s.phase = Phase::Published;
        }
        Effect::Integrating { commit } => {
            if s.phase != Phase::Published
                || !oid(&commit)
                || s.candidate
                    .as_ref()
                    .is_none_or(|c| c.approval.as_ref().is_none_or(|a| a.commit != c.commit))
            {
                return Err(Error::Constraint);
            }
            s.integration = Some(commit);
            s.phase = Phase::Integrating;
        }
        Effect::Integrated => {
            if s.phase != Phase::Integrating || s.integration.is_none() {
                return Err(Error::Constraint);
            }
            // Merge completion and ticket completion cannot be observed separately.
            for id in &s.tickets {
                w.tickets.get_mut(id).ok_or(Error::NotFound)?.status = Status::Done;
            }
            s.phase = Phase::Cleanup;
        }
        Effect::Cleaned => {
            s.phase = match s.phase {
                Phase::Cleanup => Phase::Complete,
                Phase::Abandoning => Phase::Abandoned,
                _ => return Err(Error::Constraint),
            };
        }
        Effect::Failed(message) => {
            s.error = message;
            return Ok(w);
        }
    }
    s.error.clear();
    Ok(w)
}

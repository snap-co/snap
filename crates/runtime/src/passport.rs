//! Identity provider. Async workflows own domain policy; Store owns persistence.
//! Continuations contain no IO handles or executor dependency. The host keeps
//! admitted invocations alive after their observer leaves; mutations are not replayed.
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::future::Future;
use snap_identity::{Release, Session};
use snap_protocol::{Error, Invocation, Operation, Outcome, Provider, Value, json};
use snap_store::{
    Cache, Guard, Index, Kind, NoCache, Predicate as P, Query, Row, Schema, Statement as S, Store,
    Table, Transaction,
};

pub const SESSION_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const CREDENTIALS: Table = Table {
    namespace: "snap_identity",
    name: "credentials",
};
pub const SESSIONS: Table = Table {
    namespace: "snap_identity",
    name: "sessions",
};
pub const SETTINGS: Table = Table {
    namespace: "snap_identity",
    name: "settings",
};

/// Schema and legacy names belong to Passport. Store executes their registration
/// atomically. These shapes preserve the original Authy rows and signing key.
pub fn schemas() -> [Schema; 3] {
    [
        Schema {
            table: CREDENTIALS,
            columns: &[
                ("id", Kind::Text),
                ("identity_id", Kind::Text),
                ("kind", Kind::Text),
                ("email", Kind::Text),
                ("hash", Kind::Text),
                ("created", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[
                Index {
                    columns: &["kind", "email"],
                    unique: true,
                },
                Index {
                    columns: &["identity_id", "created", "id"],
                    unique: false,
                },
            ],
            foreign: &[],
            legacy_name: Some("credentials"),
        },
        Schema {
            table: SESSIONS,
            columns: &[
                ("id", Kind::Text),
                ("identity_id", Kind::Text),
                ("credential_id", Kind::Text),
                ("digest", Kind::Text),
                ("created", Kind::Integer),
                ("expires", Kind::Integer),
            ],
            primary: &["id"],
            indexes: &[
                Index {
                    columns: &["digest"],
                    unique: true,
                },
                Index {
                    columns: &["identity_id", "created", "id"],
                    unique: false,
                },
            ],
            foreign: &[snap_store::ForeignKey {
                columns: &["credential_id"],
                target: CREDENTIALS,
                references: &["id"],
            }],
            legacy_name: Some("sessions"),
        },
        Schema {
            table: SETTINGS,
            columns: &[("key", Kind::Text), ("value", Kind::Text)],
            primary: &["key"],
            indexes: &[],
            foreign: &[],
            legacy_name: Some("settings"),
        },
    ]
}

#[derive(Default)]
pub struct Context {
    pub token: Option<String>,
    pub now: u64,
}

/// Host crypto work is separate from Store and its transactional lifetime.
pub trait Crypto: Clone + 'static {
    fn hash(&self, password: String) -> impl Future<Output = Result<String, Error>>;
    fn verify(&self, password: String, hash: String) -> impl Future<Output = Result<bool, Error>>;
    fn generate(&self) -> impl Future<Output = Result<Material, Error>>;
    fn digest(&self, token: &str) -> String;
}
pub struct Material {
    pub identity: String,
    pub credential: String,
    pub session: String,
    pub token: String,
    pub digest: String,
}
pub struct Description {
    pub session: Session,
    pub email: String,
    pub authenticated_at: u64,
}
pub type Enrollment = fn(&Material, &str, u64) -> Vec<S>;

/// Decoded before acceptance. Handlers never parse an untrusted invocation.
enum Input {
    Fetch,
    Password {
        enrollment: bool,
        email: String,
        password: String,
        kind: Option<String>,
    },
    Collection(Table),
    Release(Release),
}
impl Input {
    fn decode(invocation: Invocation, register: &str) -> Result<Self, Error> {
        let key = invocation.key.as_str();
        if key == register || key == "identity.password.acquire" {
            let value = invocation.payload.ok_or_else(invalid)?;
            return Ok(Self::Password {
                enrollment: key == register,
                email: value
                    .get("email")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?
                    .trim()
                    .to_lowercase(),
                password: value
                    .get("password")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?
                    .to_string(),
                kind: value.get("kind").and_then(Value::as_str).map(String::from),
            });
        }
        match key {
            "identity.fetch" => Ok(Self::Fetch),
            "identity.credentials" => Ok(Self::Collection(CREDENTIALS)),
            "identity.sessions" => Ok(Self::Collection(SESSIONS)),
            "identity.release" => serde_json::from_value(invocation.payload.ok_or_else(invalid)?)
                .map(Self::Release)
                .map_err(|_| invalid()),
            _ => Err(Error::ContractViolationError {
                message: format!("Unknown key: {key}"),
            }),
        }
    }
}

pub struct Response {
    pub outcome: Outcome,
    pub empty: bool,
    pub session: Option<Session>,
    pub token: Option<Option<String>>,
    pub revoked: Vec<String>,
}
impl snap_protocol::Rejection for Response {
    fn rejected(error: Error) -> Self {
        Self {
            outcome: Err(error),
            empty: false,
            session: None,
            token: None,
            revoked: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct Passport<S, C, K = NoCache> {
    kind: &'static str,
    register: &'static str,
    store: S,
    crypto: C,
    cache: K,
    enrollment: Option<Enrollment>,
}
impl<D: Store, C: Crypto, K: Cache> Passport<D, C, K> {
    pub fn new(kind: &'static str, register: &'static str, store: D, crypto: C, cache: K) -> Self {
        Self {
            kind,
            register,
            store,
            crypto,
            cache,
            enrollment: None,
        }
    }

    /// Trusted composition hook. App-owned initial records join the credential and
    /// first-session transaction. The callback must only construct statements for
    /// registered app schemas; it performs no IO and cannot commit independently.
    pub fn with_enrollment(mut self, enrollment: Enrollment) -> Self {
        self.enrollment = Some(enrollment);
        self
    }
    /// Passport owns the shape of session authority used by dependent modules.
    pub fn authority(subject: &str, session: &str, now: u64) -> Guard {
        Guard {
            query: Query::new(SESSIONS)
                .matching(vec![
                    P::eq("id", session),
                    P::eq("identity_id", subject),
                    P::gt("expires", now as i64),
                ])
                .limit(1),
            exists: true,
        }
    }
    /// Trusted server projection, not a published operation. A caller must obtain
    /// the identifiers from a verified bearer or a previously granted authority.
    pub async fn describe(
        &self,
        subject: &str,
        id: &str,
        now: u64,
    ) -> Result<Option<Description>, Error> {
        let authority = Self::authority(subject, id, now);
        let Some(row) = self.read(authority.query.clone()).await?.pop() else {
            return Ok(None);
        };
        let credentials = self
            .transaction(
                vec![authority],
                vec![S::Select(
                    Query::new(CREDENTIALS)
                        .matching(vec![
                            P::eq("id", text(&row, "credential_id")?),
                            P::eq("identity_id", subject),
                        ])
                        .limit(1),
                )],
            )
            .await?;
        let credential = credentials[0].first().ok_or_else(unavailable)?;
        Ok(Some(Description {
            session: session(&row)?,
            email: text(credential, "email")?,
            authenticated_at: integer(&row, "created")? as u64,
        }))
    }
    /// Trusted issuer logout. The application must bind this to the authenticated
    /// session and confirm the browser's logout request before invoking it.
    pub async fn end_session(&self, subject: &str, id: &str) -> Result<(), Error> {
        self.transaction(
            vec![],
            vec![S::Delete {
                table: SESSIONS,
                filter: vec![P::eq("id", id), P::eq("identity_id", subject)],
            }],
        )
        .await?;
        Ok(())
    }

    async fn transaction(
        &self,
        guards: Vec<Guard>,
        statements: Vec<S>,
    ) -> Result<Vec<snap_store::Rows>, Error> {
        self.store
            .transaction(Transaction { guards, statements })
            .await
            .map_err(storage_error)
    }
    async fn read(&self, query: Query) -> Result<snap_store::Rows, Error> {
        Ok(self
            .transaction(vec![], vec![S::Select(query)])
            .await?
            .remove(0))
    }

    /// Trusted server interface. The token has been obtained by the host; its
    /// digest is still checked against live Store authority on every resolution.
    pub async fn resolve(&self, token: &str, now: u64) -> Result<Option<Session>, Error> {
        let rows = self
            .read(
                Query::new(SESSIONS)
                    .matching(vec![
                        P::eq("digest", self.crypto.digest(token)),
                        P::gt("expires", now as i64),
                    ])
                    .limit(1),
            )
            .await?;
        rows.first().map(session).transpose()
    }

    async fn dispatch(
        &self,
        input: Input,
        context: Context,
        reply: &mut Response,
        resolved: Option<Session>,
    ) -> Outcome {
        reply.session = resolved.clone();
        if context.token.is_some() && resolved.is_none() {
            reply.token = Some(None);
        }
        if matches!(input, Input::Fetch) {
            return Ok(json!({"identityId":resolved.map(|s| s.identity_id)}));
        }
        if let Input::Password {
            enrollment,
            email,
            password,
            kind,
        } = input
        {
            if enrollment {
                let hash = self.crypto.hash(password).await?;
                let fresh = self.crypto.generate().await?;
                let credential = row(&[
                    ("id", fresh.credential.clone().into()),
                    ("identity_id", fresh.identity.clone().into()),
                    ("kind", self.kind.into()),
                    ("email", email.clone().into()),
                    ("hash", hash.into()),
                    ("created", (context.now as i64).into()),
                ]);
                let mut statements = vec![
                    S::Insert {
                        table: CREDENTIALS,
                        row: credential,
                    },
                    expired(context.now),
                    S::Insert {
                        table: SESSIONS,
                        row: session_row(&fresh, &fresh.credential, &fresh.identity, context.now),
                    },
                ];
                if let Some(enrollment) = self.enrollment {
                    statements.extend(enrollment(&fresh, &email, context.now));
                }
                self.store
                    .transaction(Transaction {
                        guards: vec![],
                        statements,
                    })
                    .await
                    .map_err(|e| {
                        if e == snap_store::Error::Constraint {
                            domain("EnrollFailedError", "Duplicate credential")
                        } else {
                            storage_error(e)
                        }
                    })?;
                reply.token = Some(Some(fresh.token));
                return Ok(Value::Null);
            }
            if kind.as_deref() != Some(self.kind) {
                return Err(bad_credential());
            }
            let query = Query::new(CREDENTIALS)
                .matching(vec![P::eq("kind", self.kind), P::eq("email", email)])
                .limit(1);
            let mut rows = snap_store::snapshot(&self.store, &self.cache, query.clone())
                .await
                .map_err(storage_error)?;
            // Cached absence is advisory too: enrollment may have happened since
            // the lookup. Reload authority before rejecting a missing credential.
            let mut credential = match rows.pop() {
                Some(credential) => credential,
                None => self
                    .read(query.clone())
                    .await?
                    .pop()
                    .ok_or_else(bad_credential)?,
            };
            // A stale cached hash must not reject a password that is valid now.
            if !self
                .crypto
                .verify(password.clone(), text(&credential, "hash")?)
                .await?
            {
                credential = self.read(query).await?.pop().ok_or_else(bad_credential)?;
                if !self
                    .crypto
                    .verify(password, text(&credential, "hash")?)
                    .await?
                {
                    return Err(bad_credential());
                }
            }
            let id = text(&credential, "id")?;
            let identity = text(&credential, "identity_id")?;
            let fresh = self.crypto.generate().await?;
            let guard = Guard {
                query: Query::new(CREDENTIALS)
                    .matching(vec![
                        P::eq("id", id.clone()),
                        P::eq("identity_id", identity.clone()),
                        P::eq("hash", text(&credential, "hash")?),
                    ])
                    .limit(1),
                exists: true,
            };
            self.transaction(
                vec![guard],
                vec![
                    expired(context.now),
                    S::Insert {
                        table: SESSIONS,
                        row: session_row(&fresh, &id, &identity, context.now),
                    },
                ],
            )
            .await?;
            reply.token = Some(Some(fresh.token));
            return Ok(json!({"_tag":"Approved"}));
        }
        let session = resolved.ok_or_else(required)?;
        let authority = Guard {
            query: Query::new(SESSIONS)
                .matching(vec![
                    P::eq("id", session.session_id.clone()),
                    P::eq("identity_id", session.identity_id.clone()),
                    P::gt("expires", context.now as i64),
                ])
                .limit(1),
            exists: true,
        };
        match input {
            Input::Collection(table) => {
                let mut filter = vec![P::eq("identity_id", session.identity_id.clone())];
                if table == SESSIONS {
                    filter.push(P::gt("expires", context.now as i64));
                }
                let mut results = self
                    .transaction(
                        vec![authority],
                        vec![S::Select(
                            Query::new(table)
                                .matching(filter)
                                .ordered(&["created", "id"])
                                .limit(10001),
                        )],
                    )
                    .await?;
                let rows = results.remove(0);
                if rows.len() > 10000 {
                    return Err(unavailable());
                }
                let values = rows.iter().map(|r| if table == CREDENTIALS {
                    Ok(json!({"credentialId":text(r,"id")?,"method":"password","label":text(r,"email")?,"createdAt":timestamp(integer(r,"created")?),"removable":false}))
                } else {
                    let id = text(r,"id")?;
                    Ok(json!({"current":id==session.session_id,"sessionId":id,"createdAt":timestamp(integer(r,"created")?),"expiresAt":timestamp(integer(r,"expires")?)}))
                }).collect::<Result<Vec<Value>,Error>>()?;
                Ok(if table == CREDENTIALS {
                    json!({"credentials":values})
                } else {
                    json!({"sessions":values})
                })
            }
            Input::Release(scope) => {
                let mut filter = vec![P::eq("identity_id", session.identity_id.clone())];
                let clear = match scope {
                    Release::Current => {
                        filter.push(P::eq("id", session.session_id.clone()));
                        true
                    }
                    Release::All => true,
                    Release::Others => {
                        filter.push(snap_store::Predicate {
                            column: "id",
                            compare: snap_store::Compare::Ne,
                            value: session.session_id.clone().into(),
                        });
                        false
                    }
                    Release::Session { session_id } => {
                        let clear = session_id == session.session_id;
                        filter.push(P::eq("id", session_id));
                        clear
                    }
                };
                // The returned IDs and deletion share the authority-check transaction.
                let mut results = self
                    .transaction(
                        vec![authority],
                        vec![
                            S::Select(
                                Query::new(SESSIONS)
                                    .matching(filter.clone())
                                    .limit(u32::MAX),
                            ),
                            S::Delete {
                                table: SESSIONS,
                                filter,
                            },
                        ],
                    )
                    .await?;
                reply.revoked = results
                    .remove(0)
                    .iter()
                    .map(|r| text(r, "id"))
                    .collect::<Result<_, _>>()?;
                if clear {
                    reply.token = Some(None);
                }
                Ok(Value::Null)
            }
            Input::Fetch | Input::Password { .. } => unreachable!("handled above"),
        }
    }
}
impl<S: Store, C: Crypto, K: Cache> Provider for Passport<S, C, K> {
    type Context = Context;
    type Output = Response;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        core::iter::once(Operation::new(
            self.register,
            snap_identity::enrollment,
            snap_protocol::IdentityPolicy::Optional,
        ))
        .chain(snap_identity::OPERATIONS)
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        context: Context,
    ) -> impl Future<Output = Result<snap_protocol::Accepted<Response>, Error>> + 'static {
        let provider = self.clone();
        let policy = self
            .operations()
            .find(|op| op.key == invocation.key)
            .map(|op| op.identity);
        async move {
            let policy = policy.ok_or_else(|| Error::ContractViolationError {
                message: "Unknown operation".into(),
            })?;
            let input = Input::decode(invocation, provider.register)?;
            let resolved = if let Some(token) = &context.token {
                provider.resolve(token, context.now).await?
            } else {
                None
            };
            policy.check(resolved.is_some())?;
            Ok(snap_protocol::Accepted::new(move || async move {
                let mut reply = Response {
                    outcome: Ok(Value::Null),
                    empty: false,
                    session: None,
                    token: None,
                    revoked: Vec::new(),
                };
                reply.outcome = provider
                    .dispatch(input, context, &mut reply, resolved)
                    .await;
                reply.empty = matches!(reply.outcome, Ok(Value::Null));
                reply
            }))
        }
    }
}

fn row(fields: &[(&str, snap_store::Value)]) -> Row {
    fields
        .iter()
        .map(|(k, v)| ((*k).into(), v.clone()))
        .collect()
}
fn text(row: &Row, key: &str) -> Result<String, Error> {
    match row.get(key) {
        Some(snap_store::Value::Text(s)) => Ok(s.clone()),
        _ => Err(unavailable()),
    }
}
fn integer(row: &Row, key: &str) -> Result<i64, Error> {
    match row.get(key) {
        Some(snap_store::Value::Integer(n)) => Ok(*n),
        _ => Err(unavailable()),
    }
}
fn session(row: &Row) -> Result<Session, Error> {
    Ok(Session {
        session_id: text(row, "id")?,
        identity_id: text(row, "identity_id")?,
        expires_at: integer(row, "expires")? as u64,
    })
}
fn session_row(fresh: &Material, credential: &str, identity: &str, now: u64) -> Row {
    row(&[
        ("id", fresh.session.clone().into()),
        ("identity_id", identity.into()),
        ("credential_id", credential.into()),
        ("digest", fresh.digest.clone().into()),
        ("created", (now as i64).into()),
        ("expires", ((now + SESSION_SECONDS * 1000) as i64).into()),
    ])
}
fn expired(now: u64) -> S {
    S::Delete {
        table: SESSIONS,
        filter: vec![P::le("expires", now as i64)],
    }
}
fn invalid() -> Error {
    Error::InvalidInputError {
        message: "Invalid input".into(),
    }
}
fn required() -> Error {
    Error::IdentityRequiredError {
        message: "Session ended".into(),
    }
}
fn bad_credential() -> Error {
    domain("InvalidCredentialError", "Invalid credential")
}
pub fn unavailable() -> Error {
    domain("IdentityUnavailable", "Identity is temporarily unavailable")
}
fn storage_error(error: snap_store::Error) -> Error {
    if error == snap_store::Error::Conflict {
        required()
    } else {
        unavailable()
    }
}
pub fn domain(tag: &str, message: &str) -> Error {
    Error::OperationError {
        failure: json!({"_tag":tag,"message":message}),
    }
}

// Gregorian civil date from Unix days. Millisecond precision matches the selected
// TypeScript wire contract without asking a SQL dialect to format domain output.
fn timestamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let time = ms.rem_euclid(86_400_000);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

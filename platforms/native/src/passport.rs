//! SQLite/password/cookie adapter for the portable Passport workflow.
//!
//! One SQLite database is the authority for accounts and sessions. Unique claims,
//! session insertion, and revocation use transactions. Raw session tokens are
//! carried only in signed cookies; the database stores SHA-256 digests. The local
//! signing key is created once in that database unless explicitly configured.
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use snap_protocol::{
    Error,
    identity::{Credential, Release, Session},
    json,
};
use snap_runtime::passport::{Result as Completed, SESSION_SECONDS, Work, domain};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Passport {
    database: Arc<Mutex<Connection>>,
    key: Vec<u8>,
    pub origin: String,
    name: String,
    secure: bool,
}

impl Passport {
    pub fn open(path: &Path, origin: &str, name: &str) -> std::io::Result<Self> {
        let origin = reqwest::Url::parse(origin).map_err(std::io::Error::other)?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(std::io::Error::other(
                "SNAP_ORIGIN must be an HTTP(S) origin",
            ));
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(std::io::Error::other("Invalid session cookie name"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
        }
        let mut db = Connection::open(path).map_err(std::io::Error::other)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(std::io::Error::other)?;
        db.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS credentials (id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, kind TEXT NOT NULL, email TEXT NOT NULL, hash TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE(kind,email));
            CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, credential_id TEXT NOT NULL REFERENCES credentials(id), digest TEXT NOT NULL UNIQUE, created INTEGER NOT NULL, expires INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS session_identity ON sessions(identity_id);") .map_err(std::io::Error::other)?;
        let tx = db.transaction().map_err(std::io::Error::other)?;
        tx.execute(
            "INSERT OR IGNORE INTO settings VALUES ('signing-key', ?1)",
            [random()],
        )
        .map_err(std::io::Error::other)?;
        let local: String = tx
            .query_row(
                "SELECT value FROM settings WHERE key='signing-key'",
                [],
                |r| r.get(0),
            )
            .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)?;
        let key = std::env::var("SNAP_SESSION_KEY").unwrap_or(local);
        if key.len() < 32 {
            return Err(std::io::Error::other(
                "SNAP_SESSION_KEY must contain at least 32 bytes",
            ));
        }
        let secure = origin.scheme() == "https";
        Ok(Self {
            database: Arc::new(Mutex::new(db)),
            key: key.into_bytes(),
            origin: origin.origin().ascii_serialization(),
            name: format!("{}{name}_session", if secure { "__Host-" } else { "" }),
            secure,
        })
    }

    pub fn read_cookie(&self, headers: &axum::http::HeaderMap) -> Result<Option<String>, Error> {
        let mut found = None;
        for header in headers.get_all("cookie") {
            for part in header.to_str().unwrap_or_default().split(';') {
                if let Some((name, value)) = part.trim().split_once('=') {
                    if name != self.name {
                        continue;
                    }
                    if found.is_some() {
                        return Err(Error::InvalidInputError {
                            message: "invalid session cookie".into(),
                        });
                    }
                    found = Some(value);
                }
            }
        }
        let Some((token, signature)) = found.and_then(|v| v.rsplit_once('.')) else {
            return Ok(None);
        };
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
            return Ok(None);
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
        mac.update(token.as_bytes());
        Ok(mac.verify_slice(&signature).is_ok().then(|| token.into()))
    }

    pub fn cookie(&self, token: Option<&str>) -> String {
        let value = token
            .map(|token| {
                let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC key");
                mac.update(token.as_bytes());
                format!(
                    "{token}.{}",
                    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
                )
            })
            .unwrap_or_default();
        format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age={}",
            self.name,
            if self.secure { "; Secure" } else { "" },
            if token.is_some() { SESSION_SECONDS } else { 0 }
        )
    }

    pub fn execute(&self, work: Work) -> Result<Completed, Error> {
        // CPU-heavy password work never runs under the database lock or on an IO task.
        match work {
            Work::Hash { password } => {
                let salt = SaltString::generate(&mut rand::rngs::OsRng);
                return Argon2::default()
                    .hash_password(password.as_bytes(), &salt)
                    .map(|h| Completed::Hash(h.to_string()))
                    .map_err(|_| unavailable());
            }
            Work::Verify { password, hash } => {
                let hash = PasswordHash::new(&hash).map_err(|_| unavailable())?;
                return Ok(Completed::Verified(
                    Argon2::default()
                        .verify_password(password.as_bytes(), &hash)
                        .is_ok(),
                ));
            }
            _ => {}
        }
        let mut db = self.database.lock().map_err(|_| unavailable())?;
        let tx = db.transaction().map_err(|_| unavailable())?;
        let result = (|| -> rusqlite::Result<Completed> {
            match work {
                Work::Resolve { token, now } => {
                    let session = tx.query_row("SELECT id, identity_id, expires FROM sessions WHERE digest=?1 AND expires>?2", params![digest(&token), now as i64], |r| Ok(Session { session_id: r.get(0)?, identity_id: r.get(1)?, expires_at: r.get::<_,i64>(2)? as u64 })).optional()?;
                    Ok(Completed::Session(session))
                }
                Work::Credential { kind, email } => {
                    let credential = tx.query_row("SELECT id, identity_id, hash FROM credentials WHERE kind=?1 AND email=?2", params![kind,email], |r| Ok(Credential { id: r.get(0)?, identity_id: r.get(1)?, hash: r.get(2)? })).optional()?;
                    Ok(Completed::Credential(credential))
                }
                Work::Enroll {
                    kind,
                    email,
                    hash,
                    now,
                } => {
                    let identity = uuid::Uuid::now_v7().to_string();
                    let id = uuid::Uuid::now_v7().to_string();
                    tx.execute(
                        "INSERT INTO credentials VALUES (?1,?2,?3,?4,?5,?6)",
                        params![id, identity, kind, email, hash, now as i64],
                    )?;
                    create_session(&tx, &id, &identity, now)
                }
                Work::CreateSession { credential, now } => {
                    // Fence against a credential being replaced during password verification.
                    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=?1 AND identity_id=?2 AND hash=?3)", params![credential.id,credential.identity_id,credential.hash], |r| r.get(0))?;
                    if !valid {
                        return Err(rusqlite::Error::QueryReturnedNoRows);
                    }
                    create_session(&tx, &credential.id, &credential.identity_id, now)
                }
                Work::Credentials { session, now } => {
                    require_session(&tx, &session, now)?;
                    let mut stmt = tx.prepare("SELECT id,email,strftime('%Y-%m-%dT%H:%M:%fZ',created/1000.0,'unixepoch') FROM credentials WHERE identity_id=?1 ORDER BY created,id")?;
                    let values = stmt.query_map([session.identity_id], |r| Ok(json!({"credentialId":r.get::<_,String>(0)?,"method":"password","label":r.get::<_,String>(1)?,"createdAt":r.get::<_,String>(2)?,"removable":false})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(Completed::Data(json!({"credentials":values})))
                }
                Work::Sessions { session, now } => {
                    require_session(&tx, &session, now)?;
                    let mut stmt = tx.prepare("SELECT id,strftime('%Y-%m-%dT%H:%M:%fZ',created/1000.0,'unixepoch'),strftime('%Y-%m-%dT%H:%M:%fZ',expires/1000.0,'unixepoch') FROM sessions WHERE identity_id=?1 AND expires>?2 ORDER BY created,id")?;
                    let values = stmt.query_map(params![session.identity_id,now as i64], |r| { let id: String = r.get(0)?; Ok(json!({"current":id==session.session_id,"sessionId":id,"createdAt":r.get::<_,String>(1)?,"expiresAt":r.get::<_,String>(2)?})) })?.collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(Completed::Data(json!({"sessions":values})))
                }
                Work::Revoke {
                    session,
                    scope,
                    now,
                } => {
                    require_session(&tx, &session, now)?;
                    let mut stmt = tx.prepare("SELECT id FROM sessions WHERE identity_id=?1")?;
                    let sessions = stmt
                        .query_map([&session.identity_id], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let revoked: Vec<_> = sessions
                        .into_iter()
                        .filter(|id| match &scope {
                            Release::Current => *id == session.session_id,
                            Release::Others => *id != session.session_id,
                            Release::All => true,
                            Release::Session { session_id } => id == session_id,
                        })
                        .collect();
                    for id in &revoked {
                        tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
                    }
                    Ok(Completed::Revoked(revoked))
                }
                Work::Hash { .. } | Work::Verify { .. } => unreachable!(),
            }
        })();
        match result {
            Ok(result) => {
                tx.commit().map_err(|_| unavailable())?;
                Ok(result)
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(domain("EnrollFailedError", "Duplicate credential"))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(Error::IdentityRequiredError {
                message: "Session ended".into(),
            }),
            Err(_) => Err(unavailable()),
        }
    }
}

fn create_session(
    tx: &rusqlite::Transaction<'_>,
    credential: &str,
    identity: &str,
    now: u64,
) -> rusqlite::Result<Completed> {
    let token = random();
    tx.execute("DELETE FROM sessions WHERE expires<=?1", [now as i64])?;
    tx.execute(
        "INSERT INTO sessions VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            uuid::Uuid::now_v7().to_string(),
            identity,
            credential,
            digest(&token),
            now as i64,
            (now + SESSION_SECONDS * 1000) as i64
        ],
    )?;
    Ok(Completed::Created { token })
}
fn require_session(
    tx: &rusqlite::Transaction<'_>,
    session: &Session,
    now: u64,
) -> rusqlite::Result<()> {
    tx.query_row(
        "SELECT id FROM sessions WHERE id=?1 AND identity_id=?2 AND expires>?3",
        params![session.session_id, session.identity_id, now as i64],
        |_| Ok(()),
    )
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn digest(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}
fn unavailable() -> Error {
    domain("IdentityUnavailable", "Identity is temporarily unavailable")
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

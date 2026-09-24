//! Passport's native crypto executor and startup composition helpers. No SQL.
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};
use snap_protocol::Error;
use snap_runtime::passport::{Material, SETTINGS, unavailable};
use snap_store::{Predicate, Query, Statement, Transaction, Value};

#[derive(Clone)]
pub struct Crypto;
impl snap_runtime::passport::Crypto for Crypto {
    async fn hash(&self, password: String) -> Result<String, Error> {
        tokio::task::spawn_blocking(move || {
            let salt = SaltString::generate(&mut rand::rngs::OsRng);
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map(|h| h.to_string())
                .map_err(|_| unavailable())
        })
        .await
        .map_err(|_| unavailable())?
    }
    async fn verify(&self, password: String, hash: String) -> Result<bool, Error> {
        tokio::task::spawn_blocking(move || {
            let hash = PasswordHash::new(&hash).map_err(|_| unavailable())?;
            Ok(Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok())
        })
        .await
        .map_err(|_| unavailable())?
    }
    async fn generate(&self) -> Result<Material, Error> {
        tokio::task::spawn_blocking(|| {
            let token = random();
            Material {
                identity: uuid::Uuid::now_v7().to_string(),
                credential: uuid::Uuid::now_v7().to_string(),
                session: uuid::Uuid::now_v7().to_string(),
                digest: URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes())),
                token,
            }
        })
        .await
        .map_err(|_| unavailable())
    }
    fn digest(&self, token: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
    }
}

/// Startup only. The persisted setting is shared with pre-refactor Authy installs.
pub fn signing_key(store: &crate::store::Store) -> std::io::Result<Vec<u8>> {
    let query = Query::new(SETTINGS)
        .matching(vec![Predicate::eq("key", "signing-key")])
        .limit(1);
    let read = || {
        store.execute(Transaction {
            guards: vec![],
            statements: vec![Statement::Select(query.clone())],
        })
    };
    let error = |_| std::io::Error::other("Cannot load session signing key");
    if read().map_err(error)?[0].is_empty() {
        let result = store.execute(Transaction {
            guards: vec![],
            statements: vec![Statement::Insert {
                table: SETTINGS,
                row: [
                    ("key".into(), Value::from("signing-key")),
                    ("value".into(), Value::from(random())),
                ]
                .into(),
            }],
        });
        if !matches!(result, Ok(_) | Err(snap_store::Error::Constraint)) {
            return Err(error(snap_store::Error::Unavailable));
        }
    }
    let rows = read().map_err(error)?;
    let Some(Value::Text(local)) = rows[0].first().and_then(|r| r.get("value")) else {
        return Err(error(snap_store::Error::Unavailable));
    };
    let key = std::env::var("SNAP_SESSION_KEY").unwrap_or_else(|_| local.clone());
    if key.len() < 32 {
        return Err(std::io::Error::other(
            "SNAP_SESSION_KEY must contain at least 32 bytes",
        ));
    }
    Ok(key.into_bytes())
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

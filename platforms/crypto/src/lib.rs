//! Native Identity cryptography. Argon2id's encoded hashes carry their parameters
//! and salt. Bearers contain 256 random bits; storage only receives SHA-256 digests.
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use sha2::{Digest, Sha256};
use snap_identity::Crypto;
use snap_store::Error;

#[derive(Default)]
pub struct Native;
impl Crypto for Native {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        let salt = SaltString::encode_b64(&self.random()?[..16]).map_err(|_| Error::Unavailable)?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| Error::Unavailable)
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        let hash = PasswordHash::new(hash).map_err(|_| Error::Unavailable)?;
        match Argon2::default().verify_password(password.as_bytes(), &hash) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        Sha256::digest(secret.as_bytes()).to_vec()
    }
}

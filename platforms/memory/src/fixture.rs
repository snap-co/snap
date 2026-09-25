//! Deliberately insecure, deterministic crypto for isolated test compositions only.
//! Never use this provider with real credentials or exposed application hosts.
use snap_protocol::Error;
use snap_runtime::passport::{Crypto, Material};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Default)]
pub struct TestCrypto(Rc<Cell<u64>>);
impl Crypto for TestCrypto {
    async fn hash(&self, password: String) -> Result<String, Error> {
        Ok(format!("test-only:{password}"))
    }
    async fn verify(&self, password: String, hash: String) -> Result<bool, Error> {
        Ok(hash == format!("test-only:{password}"))
    }
    async fn generate(&self) -> Result<Material, Error> {
        let n = self.0.get().checked_add(1).expect("fixture IDs exhausted");
        self.0.set(n);
        let token = format!("test-token-{n}");
        Ok(Material {
            identity: format!("identity-{n}"),
            credential: format!("credential-{n}"),
            session: format!("session-{n}"),
            digest: self.digest(&token),
            token,
        })
    }
    fn digest(&self, token: &str) -> String {
        format!("test-digest:{token}")
    }
}

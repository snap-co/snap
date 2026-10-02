use snap_identity::Crypto;
use snap_store::{Error, Store};

// Insecure and deliberately transparent. Only for controlled behavior tests.
#[derive(Default)]
pub struct Fake(pub u64);
impl Crypto for Fake {
    fn random(&mut self) -> Result<[u8; 32], Error> {
        self.0 += 1;
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&self.0.to_be_bytes());
        Ok(bytes)
    }
    fn hash_password(&mut self, password: &str) -> Result<String, Error> {
        Ok(format!("fake:{password}"))
    }
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool, Error> {
        Ok(hash == format!("fake:{password}"))
    }
    fn digest(&self, secret: &str) -> Vec<u8> {
        secret.as_bytes().to_vec()
    }
}
pub fn migrations() -> Vec<snap_store::migration::Migration> {
    [
        snap_identity::MIGRATION,
        snap_identity::SESSION_TIME_MIGRATION,
    ]
    .into_iter()
    .map(|source| toml::from_str(source).unwrap())
    .collect()
}
pub fn store(loaded: bool) -> Store<snap_store_sqlite::Sqlite> {
    let mut store = snap_store_sqlite::Sqlite::memory(&migrations()).unwrap();
    if loaded {
        for table in snap_identity::TABLES {
            store.load(table).unwrap();
        }
    }
    store
}

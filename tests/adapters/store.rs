use snap_native::store::Store;
use snap_store::Schema;

pub struct Fixture {
    pub store: Store,
    pub peer: Store,
    sqlite: bool,
    directory: std::path::PathBuf,
}
impl Fixture {
    pub fn new(sqlite: bool, schemas: &[Schema]) -> Self {
        let directory = std::path::PathBuf::from(format!(
            "/tmp/opencode/store-contract-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("store.sqlite");
        let store = if sqlite {
            Store::sqlite(&path, schemas)
        } else {
            Store::memory(schemas)
        }
        .unwrap();
        let peer = if sqlite {
            Store::sqlite(&path, schemas).unwrap()
        } else {
            store.clone()
        };
        Self {
            store,
            peer,
            sqlite,
            directory,
        }
    }
    pub fn register(&self, schemas: &[Schema]) -> Result<Store, snap_store::Error> {
        if self.sqlite {
            Store::sqlite(&self.directory.join("store.sqlite"), schemas)
        } else {
            Store::memory(schemas)
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

//! Same application with advisory credential caching selected by its launcher.
fn main() -> std::io::Result<()> {
    authy_native::run(snap_native::store::MemoryCache::new(8))
}

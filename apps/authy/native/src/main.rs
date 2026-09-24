fn main() -> std::io::Result<()> {
    let config = snap_native::Config::from_env("authy")?;
    let origin =
        std::env::var("SNAP_ORIGIN").unwrap_or_else(|_| format!("http://{}", config.address));
    let database = std::env::var_os("SNAP_DATABASE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ".snap/authy.sqlite".into());
    let passport = snap_native::passport::Passport::open(&database, &origin, "authy")?;
    snap_native::run_with_passport(authy::server(), config, passport)
}

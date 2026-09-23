//! The composition root belongs to the host, not the application library.

fn main() -> std::io::Result<()> {
    let application =
        healthy::application().map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    snap_native::run(application, snap_native::Config::from_env("healthy")?)
}

//! The same local-only provider is exercised by native and Workers host contracts.
#[path = "../../../tests/adapters/carrier.rs"]
mod carrier;
fn main() -> std::io::Result<()> {
    let config = snap_native::Config::from_env("carrier-contract")?;
    let origin = format!("http://{}", config.address);
    let cookie = snap_native::cookie::Cookie::new(vec![7; 32], "fixture", false, 60)?;
    snap_native::run_application(
        carrier::App::new(snap_native::now),
        config,
        snap_native::Web {
            bindings: carrier::bindings(),
            session: Some(snap_native::SessionCarrier {
                origin,
                cookie,
                identify: "lease.resolve",
            }),
        },
    )
}

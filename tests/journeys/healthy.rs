//! A headless user journey. The runner provides a live SDK; the scenario uses observations.
use healthy::client::{Snapshot, Status};
use snap_native::client::Running;

pub async fn run(client: &mut Running<Snapshot>) -> Result<(), String> {
    println!("Healthy journey: client started; waiting for a health observation");
    loop {
        let snapshot = client
            .changed()
            .await
            .map_err(|error| format!("{error:?}"))?;
        match snapshot.status {
            Status::Loading => continue,
            Status::Error => {
                return Err("Healthy journey received a failed health observation".into());
            }
            Status::Ok => {
                println!(
                    "Healthy journey: observed OK with {} history sample(s)",
                    snapshot.samples.len()
                );
                return Ok(());
            }
        }
    }
}

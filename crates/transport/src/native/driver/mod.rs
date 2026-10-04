//! Native execution of a portable Transport loop. Socket drivers only hold queue
//! handles; blocking application execution never runs on their async tasks.
mod dispatch;
use crate::runtime::Loop;
#[cfg(feature = "native-legacy")]
pub use dispatch::Endpoint;
pub use dispatch::{Dispatcher, Prepare};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub struct Shared<L: Loop> {
    pub host: Mutex<L>,
    clock: Instant,
}

impl<L: Loop> Shared<L> {
    pub fn new(runtime: L) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(runtime),
            clock: Instant::now(),
        })
    }
    pub(crate) fn now(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }
}

/// Drive the FIFO independently of socket tasks. A controlled host may instead
/// call the portable loop's tick and step methods explicitly.
pub async fn dispatch<L: Loop + Send + 'static>(shared: Arc<Shared<L>>) {
    let mut interval = tokio::time::interval(Duration::from_millis(5));
    loop {
        interval.tick().await;
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || {
            let mut runtime = shared.host.lock().unwrap();
            runtime.tick(shared.now());
            runtime.step();
        })
        .await
        .expect("transport execution panicked");
    }
}

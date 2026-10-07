//! Real TCP plus the production dispatcher. Carrier-only retirement cases supply
//! output at the Loop dependency seam; authentication cases mount the cartridge.
use crate::native::{TcpChannel, tls_support};
use snap_transport::native::driver::{Dispatcher, Shared};
use snap_transport::{
    Command, Error, Response, bearer,
    carrier::Frame,
    runtime::{CarrierControl, Loop, Output},
};
use std::sync::{Arc, Mutex};

pub struct Tcp<L: Loop> {
    shared: Arc<Shared<L>>,
    serving: tokio::task::JoinHandle<std::io::Result<()>>,
    dispatch: tokio::task::JoinHandle<()>,
    address: String,
    tls: snap_transport::native::tls::ClientTls,
    _pki: tempfile::TempDir,
}
impl<L: Loop + Send + 'static> Tcp<L> {
    pub async fn start(host: L) -> Self {
        let pki = tempfile::tempdir().unwrap();
        let (server_tls, tls) = tls_support::pki(pki.path(), false);
        let shared = Shared::new(host);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let serving = tokio::spawn(snap_transport::native::tcp::serve(
            listener,
            Dispatcher::tcp(shared.clone(), None),
            server_tls,
        ));
        let dispatch = tokio::spawn(snap_transport::native::driver::dispatch(shared.clone()));
        Self {
            shared,
            serving,
            dispatch,
            address,
            tls,
            _pki: pki,
        }
    }
    pub async fn channel(&self) -> TcpChannel {
        TcpChannel::open(&self.address, &self.tls).await.unwrap()
    }
    pub async fn stop(mut self) {
        self.serving.abort();
        self.dispatch.abort();
        let _ = (&mut self.serving).await;
        let _ = (&mut self.dispatch).await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while Arc::strong_count(&self.shared) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
impl<L: Loop> Drop for Tcp<L> {
    fn drop(&mut self) {
        self.serving.abort();
        self.dispatch.abort();
    }
}

/// No admission or transaction claims. This fixture supplies the output and
/// retirement inputs whose carriage is under test, identically to both drivers.
pub struct Retirement {
    output: Output,
    control: CarrierControl,
    retired: bool,
    host_retirement: bool,
    frames: Vec<Frame>,
    pub submissions: Arc<Mutex<usize>>,
}
impl Retirement {
    pub fn new(final_output: bool, terminal: bool) -> Self {
        let frames = if final_output {
            vec![
                Frame {
                    response: Response::Event(snap_transport::Event::Progress {
                        id: 9,
                        value: snap_transport::json!("waiting"),
                    }),
                    handshake: false,
                    attachment: None,
                    terminal: false,
                },
                Frame {
                    response: Response::Event(snap_transport::Event::Completed {
                        id: 9,
                        outcome: Ok(snap_transport::json!("committed")),
                    }),
                    handshake: false,
                    attachment: None,
                    terminal,
                },
            ]
        } else {
            vec![]
        };
        Self {
            output: Default::default(),
            control: Default::default(),
            retired: false,
            host_retirement: !terminal,
            frames,
            submissions: Default::default(),
        }
    }
}
impl Loop for Retirement {
    fn open(&mut self) -> Result<u64, Error> {
        Ok(1)
    }
    fn output(&self, _: u64) -> Result<Output, Error> {
        Ok(self.output.clone())
    }
    fn carrier_control(&self, _: u64) -> Result<CarrierControl, Error> {
        Ok(self.control.clone())
    }
    fn tick(&mut self, _: u64) {
        if self.control.take().is_some() {
            self.retired = true;
        }
    }
    fn step(&mut self) -> bool {
        false
    }
    fn submit(&mut self, _: u64, _: Command, _: u64) -> Result<(), Error> {
        *self.submissions.lock().unwrap() += 1;
        for frame in self.frames.drain(..) {
            if frame.terminal {
                self.output.seal(Some(frame));
            } else {
                self.output.push_frame(frame);
            }
        }
        self.retired = self.host_retirement;
        Ok(())
    }
    fn retired(&self, _: u64) -> bool {
        self.retired
    }
    fn authorize_upgrade(&self, _: &str) -> Result<(), Error> {
        Ok(())
    }
    fn is_preconnection_request(&self, _: &str) -> bool {
        false
    }
    fn preconnection_reply(
        &mut self,
        _: snap_transport::Invocation,
        _: Option<String>,
    ) -> bearer::Reply {
        Err(Error::Unavailable).into()
    }
}

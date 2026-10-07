//! Channel adaptation shared by native cartridge and benchmark hosts. Production
//! driver owns framing and verified TLS. Physical failure never retries a call.
use snap_transport::{Channel, Command, Error, Response};

pub(crate) struct TcpChannel {
    driver: snap_transport::native::TcpClient,
    usable: bool,
}
impl TcpChannel {
    pub(crate) async fn open(
        address: &str,
        tls: &snap_transport::native::tls::ClientTls,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            driver: snap_transport::native::TcpClient::open(address, tls).await?,
            usable: true,
        })
    }
}
impl Channel for TcpChannel {
    async fn send(&mut self, command: Command) -> Result<(), Error> {
        if !self.usable {
            return Err(Error::Unavailable);
        }
        self.driver.send(&command).await.map_err(|_| {
            self.usable = false;
            Error::Unavailable
        })
    }
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        if !self.usable {
            return Err(Error::Unavailable);
        }
        self.driver
            .receive()
            .await
            .map(|(response, _)| Some(response))
            .map_err(|_| {
                self.usable = false;
                Error::Unavailable
            })
    }
}

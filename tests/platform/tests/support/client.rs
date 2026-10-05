//! The implementation under test is TCP Client, not the server carrier. A raw
//! TLS peer supplies wire inputs and observes commands without operation dispatch.
use super::server::tls_support;
use snap_platform_tests::transport::{Client, Duplex};
use snap_transport::{
    Command, Response,
    carrier::{AttachmentInfo, Frame},
};
use std::time::Duration;

pub struct Setup {
    client: snap_transport::native::TcpClient,
    peer: Option<snap_transport::native::tls::ServerStream>,
}

pub async fn start() -> Setup {
    let directory = tempfile::tempdir().unwrap();
    let (server_tls, client_tls) = tls_support::pki(directory.path(), false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (client, peer) = tokio::join!(
        snap_transport::native::TcpClient::open(&address, &client_tls),
        async { server_tls.accept(listener.accept().await.unwrap().0).await }
    );
    Setup {
        client: client.unwrap(),
        peer: Some(peer.unwrap()),
    }
}

impl Duplex for Setup {
    async fn send(&mut self, command: &Command) {
        self.client.send(command).await.unwrap();
    }
    async fn incoming(&mut self) -> Command {
        tokio::time::timeout(
            Duration::from_secs(2),
            snap_transport::native::tcp::read_command(self.peer.as_mut().unwrap()),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    }
    async fn publish(&mut self, frame: Frame) {
        snap_transport::native::tcp::write_response(
            self.peer.as_mut().unwrap(),
            &frame.response,
            frame.handshake,
            frame.attachment.as_ref(),
        )
        .await
        .unwrap();
    }
    async fn receive(&mut self) -> Result<(Response, Option<AttachmentInfo>), String> {
        tokio::time::timeout(Duration::from_secs(2), self.client.receive())
            .await
            .expect("client read deadline")
            .map_err(|e| e.to_string())
    }
}
impl Client for Setup {
    fn lose_peer(&mut self) {
        self.peer.take();
    }
}

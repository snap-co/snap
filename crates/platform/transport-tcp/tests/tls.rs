//! Real TLS owns trust/name/expiry and plaintext rejection. Application suites
//! separately prove dispatch and retained lifetimes through this carrier.
mod support;
use snap_transport::{Command, Response};
use snap_transport_tcp::{Client, tls::ClientTls};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};
use tokio_rustls::rustls::{CertificateError, Error};

#[tokio::test]
#[ignore = "real TLS sockets"]
async fn verified_tls_carries_snap_and_rejects_untrusted_wrong_name_expired_and_plaintext() {
    let temp = tempfile::tempdir().unwrap();
    let (server, trusted) = support::pki(temp.path(), false);
    let ipv6_server = server.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let serving = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            if let Ok(mut socket) = server.accept(socket).await {
                assert!(matches!(
                    snap_transport_tcp::read_command(&mut socket).await.unwrap(),
                    Some(Command::Close)
                ));
                snap_transport_tcp::write_response(
                    &mut socket,
                    &Response::Global {
                        kind: "probe".into(),
                        input: snap_transport::Value::Null,
                    },
                    false,
                    None,
                )
                .await
                .unwrap();
                socket.shutdown().await.unwrap();
            }
        }
    });
    // Standard roots don't trust our independent private CA.
    let public = ClientTls::new(None, None).unwrap();
    let error = Client::open(&addr, &public).await.err().unwrap();
    assert!(
        matches!(
            error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
            Some(Error::InvalidCertificate(CertificateError::UnknownIssuer))
        ),
        "{error}"
    );
    let wrong_name =
        ClientTls::new(Some(&temp.path().join("ca.pem")), Some("wrong.example")).unwrap();
    let error = Client::open(&addr, &wrong_name).await.err().unwrap();
    assert!(
        matches!(
            error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
            Some(Error::InvalidCertificate(
                CertificateError::NotValidForNameContext { .. }
            ))
        ),
        "{error}"
    );
    let mut plain = TcpStream::connect(&addr).await.unwrap();
    plain
        .write_all(&snap_transport::binary::command(&Command::Close).unwrap())
        .await
        .unwrap();
    plain.shutdown().await.unwrap();
    let mut byte = [0u8; 1];
    use tokio::io::AsyncReadExt;
    // An alert or EOF is not a SNAP response, and the listener still serves TLS.
    if let Ok(n) = plain.read(&mut byte).await {
        assert!(n == 0 || byte[0] != b'S');
    }
    for address in [
        &addr,
        &format!("localhost:{}", addr.rsplit_once(':').unwrap().1),
    ] {
        let mut client = Client::open(address, &trusted).await.unwrap();
        client.send(&Command::Close).await.unwrap();
        assert_eq!(
            client.receive().await.unwrap().0,
            Response::Global {
                kind: "probe".into(),
                input: snap_transport::Value::Null
            }
        );
    }
    serving.abort();
    let _ = serving.await;
    let ipv6_listener = TcpListener::bind("[::1]:0").await.unwrap();
    let ipv6_addr = ipv6_listener.local_addr().unwrap().to_string();
    let serving = tokio::spawn(async move {
        let (socket, _) = ipv6_listener.accept().await.unwrap();
        let mut socket = ipv6_server.accept(socket).await.unwrap();
        assert!(matches!(
            snap_transport_tcp::read_command(&mut socket).await.unwrap(),
            Some(Command::Close)
        ));
    });
    let mut client = Client::open(&ipv6_addr, &trusted).await.unwrap();
    client.send(&Command::Close).await.unwrap();
    serving.await.unwrap();
    let expired_dir = tempfile::tempdir().unwrap();
    let (expired, trusted) = support::pki(expired_dir.path(), true);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let serving = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let _ = expired.accept(socket).await;
    });
    let error = Client::open(&addr, &trusted).await.err().unwrap();
    assert!(
        matches!(
            error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
            Some(Error::InvalidCertificate(
                CertificateError::ExpiredContext { .. }
            ))
        ),
        "{error}"
    );
    serving.await.unwrap();
}

#[tokio::test]
#[ignore = "real TLS sockets with controlled timer"]
async fn stalled_handshake_expires_without_opening_a_protocol_stream() {
    let temp = tempfile::tempdir().unwrap();
    let (server, _) = support::pki(temp.path(), false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _stalled = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (socket, _) = listener.accept().await.unwrap();
    tokio::time::pause();
    let result = server.accept(socket).await;
    assert_eq!(result.err().unwrap().kind(), std::io::ErrorKind::TimedOut);
}

use snap_http::client::{Body, Client, Outgoing};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn redirects_uncertainty_and_body_limits_never_replay_requests() {
    for scenario in ["redirect", "disconnect", "oversize", "slow-body", "success"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (finished, mut done) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let byte = stream.read_u8().await.unwrap();
                request.push(byte);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("POST /exchange HTTP/1.1\r\n"));
            match scenario {
                "redirect" => stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: /stolen\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await
                    .unwrap(),
                "disconnect" => {}
                _ => {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc")
                        .await
                        .unwrap();
                    if scenario == "slow-body" {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                    let _ = stream.write_all(b"def").await;
                }
            }
            drop(stream);
            // Keep the listener available until the entire client call has
            // settled, including any backoff. Dropping it early could conceal a retry.
            tokio::select! {
                _ = &mut done => {},
                _ = listener.accept() => panic!("request was redirected or retried"),
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("HTTP call did not settle"),
            }
        });
        let client = snap_http::native::Client::new().unwrap();
        let result = client
            .send(Outgoing {
                method: "POST",
                url: format!("http://{address}/exchange"),
                headers: vec![],
                body: vec![],
                max_bytes: if scenario == "oversize" { 5 } else { 6 },
                timeout_ms: if scenario == "slow-body" { 150 } else { 3000 },
            })
            .await;
        match scenario {
            "disconnect" => assert!(result.is_err()),
            "redirect" => assert_eq!(result.unwrap().status, 302),
            _ => {
                let mut response = result.unwrap();
                let result = snap_http::client::collect(&mut response.body, 100).await;
                if scenario == "success" {
                    assert_eq!(result.unwrap(), b"abcdef");
                } else {
                    assert!(result.is_err(), "{scenario}");
                    assert!(
                        response.body.chunk().await.is_err(),
                        "failed stream cannot resume"
                    );
                }
            }
        }
        let _ = finished.send(());
        peer.await.unwrap();
    }
}

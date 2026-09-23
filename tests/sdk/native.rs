//! Consumer contracts, using a real HTTP peer to supply failures the Healthy server never emits.
#[path = "../adapters/native.rs"]
mod adapter;
use snap_native::client::{Client, Http};
use snap_protocol::Error;
use std::time::Duration;

#[tokio::test]
async fn rejects_invalid_completions_and_preserves_remote_errors() {
    let peer = adapter::Peer::start(2).await;
    let client = Client::new(Http::new(&peer.url, "test").unwrap());
    assert!(matches!(
        client.health_up().await,
        Err(Error::ContractViolationError { .. })
    ));
    peer.mode(4);
    assert!(matches!(
        client.health_up().await,
        Err(Error::ContractViolationError { .. })
    ));
    peer.mode(1);
    assert!(
        matches!(client.health_up().await, Err(Error::UnavailableError { message }) if message == "Service unavailable")
    );
    peer.mode(0);
    let (first, second) = tokio::join!(client.health_up(), client.health_up());
    assert_eq!(first.unwrap().status, "OK");
    assert_eq!(second.unwrap().status, "OK");
    client.close();
}

#[tokio::test]
async fn close_cancels_an_outstanding_request_and_rejects_new_work() {
    let peer = adapter::Peer::start(3).await;
    let client = Client::new(Http::new(&peer.url, "test").unwrap());
    let work = async {
        let (result, ()) = tokio::join!(client.health_up(), async {
            peer.arrived.notified().await;
            client.close();
        });
        assert!(matches!(result, Err(Error::UnavailableError { .. })));
        assert!(matches!(
            client.health_up().await,
            Err(Error::UnavailableError { .. })
        ));
        client.close();
    };
    tokio::time::timeout(Duration::from_secs(2), work)
        .await
        .expect("close must release a hanging query");
}

#[tokio::test]
async fn observations_record_failure_and_recovery_without_a_renderer() {
    let peer = adapter::Peer::start(1).await;
    let mut client = snap_native::client::start(
        healthy::client::application(),
        Http::new(&peer.url, "test").unwrap(),
    );
    assert_eq!(client.snapshot().status, healthy::client::Status::Loading);
    let work = async {
        loop {
            if client.changed().await.unwrap().status == healthy::client::Status::Error {
                break;
            }
        }
        peer.mode(0);
        loop {
            let snapshot = client.changed().await.unwrap();
            if snapshot.status == healthy::client::Status::Ok {
                assert!(!snapshot.samples.first().unwrap().ok);
                assert!(snapshot.samples.last().unwrap().ok);
                break;
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(7), work).await;
    client.close().await;
    client.close().await;
    result.expect("client should recover after the peer recovers");
}

#[tokio::test]
async fn query_deadline_releases_the_caller_and_allows_recovery() {
    let peer = adapter::Peer::start(3).await;
    let client = Client::new(Http::new(&peer.url, "test").unwrap());
    let result = tokio::time::timeout(Duration::from_secs(7), client.health_up())
        .await
        .expect("SDK deadline must bound a silent peer");
    assert!(matches!(result, Err(Error::UnavailableError { .. })));
    peer.mode(0);
    assert_eq!(client.health_up().await.unwrap().status, "OK");
    client.close();
}

//! Carrier conformance below application dispatch and logical connection policy.
//! A host adapter must exercise the selected production carrier, not implement
//! its codec or return the expected response. Published frames are dependency
//! inputs here; these cases do not prove operation acceptance or execution.
use alloc::string::String;
use snap_transport::{
    Command, Event, Invocation, Response,
    carrier::{AttachmentInfo, Frame},
    json,
};

/// Client/server IO plus the opposite dependency seam. Hosts own deadlines.
/// `incoming` observes decoded commands; `publish` supplies host-owned output.
/// TCP carries handshake metadata; JSON WebSocket responses do not.
pub trait Duplex {
    fn send(&mut self, command: &Command) -> impl core::future::Future<Output = ()>;
    fn incoming(&mut self) -> impl core::future::Future<Output = Command>;
    fn publish(&mut self, frame: Frame) -> impl core::future::Future<Output = ()>;
    fn receive(
        &mut self,
    ) -> impl core::future::Future<Output = Result<(Response, Option<AttachmentInfo>), String>>;
}

/// Server-side carrier controls, independent of the selected client peer.
pub trait Server: Duplex {
    fn malformed(&mut self) -> impl core::future::Future<Output = ()>;
    fn teardown(&mut self) -> impl core::future::Future<Output = bool>;
    fn pending_command(&mut self) -> bool;
    /// Arm the legal interleaving: receive observes empty, then the host publishes
    /// its final frame and retires before the carrier's next observation.
    fn retire_after_empty_receive(&mut self, frame: Frame);
}

/// Client-side carrier controls. Dropping the peer must produce an IO error,
/// never a successful empty reply. Logical reconnect is not a carrier retry.
pub trait Client: Duplex {
    fn lose_peer(&mut self);
}

pub async fn commands_and_observations<T: Duplex>(transport: &mut T, metadata_on_wire: bool) {
    let connect = Command::Connect {
        bearer: "private".into(),
        client_id: "client".into(),
    };
    transport.send(&connect).await;
    assert_eq!(
        serde_json::to_value(transport.incoming().await).unwrap(),
        serde_json::to_value(connect).unwrap()
    );
    let attachment = AttachmentInfo {
        retention_ms: 123,
        lifetime: "boot:connection".into(),
    };
    transport
        .publish(Frame {
            response: Response::Attached { resumed: false },
            handshake: true,
            attachment: Some(attachment.clone()),
            terminal: false,
        })
        .await;
    let (response, info) = transport.receive().await.expect("handshake delivery");
    assert_eq!(response, Response::Attached { resumed: false });
    if metadata_on_wire {
        assert_eq!(info, Some(attachment));
    }

    let invoke = Command::Invoke(Invocation {
        id: 7,
        operation: "probe.run".into(),
        input: json!({"value":3}),
    });
    transport.send(&invoke).await;
    assert_eq!(
        serde_json::to_value(transport.incoming().await).unwrap(),
        serde_json::to_value(invoke).unwrap()
    );
    // No handler has completed. Output must still cross an otherwise idle socket.
    // One event per frame: a carrier that wants to batch does so below this layer
    // and reassembles here, so a slow operation can publish progress as it goes.
    let progress = Response::Event(Event::Progress {
        id: 7,
        value: json!("waiting"),
    });
    transport
        .publish(Frame {
            response: progress.clone(),
            handshake: false,
            attachment: None,
            terminal: false,
        })
        .await;
    assert_eq!(
        transport.receive().await.expect("idle output delivery").0,
        progress
    );
}

pub async fn close_reaches_host<T: Server>(transport: &mut T) {
    transport.send(&Command::Close).await;
    assert!(
        transport.teardown().await,
        "logical Close must reach the host"
    );
    assert!(
        !transport.teardown().await,
        "physical disconnect follows Close"
    );
}

pub async fn malformed_input_never_reaches_dispatch<T: Server>(transport: &mut T) {
    transport.malformed().await;
    assert!(!transport.teardown().await);
    assert!(
        !transport.pending_command(),
        "malformed input reached dispatch"
    );
}

pub async fn final_reply_at_retirement<T: Server>(transport: &mut T) {
    let response = Response::Event(Event::Completed {
        id: 9,
        outcome: Ok(json!("committed")),
    });
    transport.retire_after_empty_receive(Frame {
        response: response.clone(),
        handshake: false,
        attachment: None,
        terminal: true,
    });
    assert_eq!(
        transport.receive().await.expect("final output delivery").0,
        response
    );
}

pub async fn physical_loss_is_an_error<T: Client>(transport: &mut T) {
    transport.lose_peer();
    assert!(
        transport.receive().await.is_err(),
        "EOF is not a successful exchange"
    );
}

/// TCP setup policy, not a requirement on every carrier. Operation failures are
/// correlated Completed events and do not themselves retire the attachment.
pub async fn refusal_ends_physical_connection<C: snap_transport::Channel>(channel: C) {
    let mut client = snap_transport::client::Client::new(channel);
    assert_eq!(
        client.connect("invalid", "refusal").await,
        Err(snap_transport::Error::InvalidBearer)
    );
    assert_eq!(
        client.connect("alice", "refusal").await,
        Err(snap_transport::Error::Unavailable)
    );
}

/// Carrier-only dependency outputs, not a substitute for operation execution.
/// The setup supplies these frames and retirement at the host/carrier seam.
pub async fn retirement_drains_output_before_loss<C: snap_transport::Channel>(
    mut channel: C,
    final_output: bool,
) {
    channel
        .send(Command::Connect {
            bearer: "unused".into(),
            client_id: "retirement".into(),
        })
        .await
        .unwrap();
    if final_output {
        assert_eq!(
            channel.receive().await.unwrap(),
            Some(Response::Event(Event::Progress {
                id: 9,
                value: json!("waiting")
            }))
        );
        assert_eq!(
            channel.receive().await.unwrap(),
            Some(Response::Event(Event::Completed {
                id: 9,
                outcome: Ok(json!("committed"))
            }))
        );
    }
    let after = channel.receive().await;
    assert!(
        after.is_err(),
        "retired stream must lose the physical connection after draining output, got {after:?}"
    );
    assert!(
        channel
            .send(Command::Connect {
                bearer: "unused".into(),
                client_id: "reuse".into()
            })
            .await
            .is_err(),
        "observed physical loss must fence reuse"
    );
}

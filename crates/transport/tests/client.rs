use snap_transport::{Channel, Command, Error, Event, Response, client::Client, json};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
struct Reply(Response);
impl Channel for Reply {
    async fn exchange(&mut self, _: Command) -> Result<Response, Error> {
        Ok(self.0.clone())
    }
}
fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("unexpected IO"),
    }
}
#[test]
fn client_rejects_uncorrelated_and_out_of_order_completion() {
    for events in [
        vec![Event::Completed {
            id: 1,
            outcome: Ok(json!(1)),
        }],
        vec![
            Event::Accepted { id: 2 },
            Event::Completed {
                id: 2,
                outcome: Ok(json!(1)),
            },
        ],
        vec![
            Event::Completed {
                id: 1,
                outcome: Ok(json!(1)),
            },
            Event::Accepted { id: 1 },
        ],
        vec![
            Event::Progress {
                id: 1,
                value: json!("too early"),
            },
            Event::Accepted { id: 1 },
            Event::Completed {
                id: 1,
                outcome: Ok(json!(1)),
            },
        ],
    ] {
        let mut client = Client::new(Reply(Response::Events(events)));
        assert_eq!(
            ready(client.invoke("operation", json!(null))),
            Err(Error::Protocol)
        );
    }
}

#[test]
fn repeated_ack_stops_retry_and_progress_closes_with_completion() {
    use snap_transport::client::{Observation, Trace};
    let mut trace = Trace::new(7, 100);
    assert!(!trace.retry_due(99));
    assert!(trace.retry_due(100));
    trace.retried(200);
    assert!(!trace.retry_due(100));
    for _ in 0..2 {
        assert_eq!(
            trace.receive(Event::Accepted { id: 7 }),
            Ok(Observation::Accepted)
        );
    }
    assert!(!trace.retry_due(u64::MAX));
    assert_eq!(
        trace.receive(Event::Progress {
            id: 7,
            value: json!("working")
        }),
        Ok(Observation::Progress(json!("working")))
    );
    assert_eq!(
        trace.receive(Event::Completed {
            id: 7,
            outcome: Ok(json!(42))
        }),
        Ok(Observation::Completed(Ok(json!(42))))
    );
    assert_eq!(
        trace.receive(Event::Progress {
            id: 7,
            value: json!("late")
        }),
        Err(Error::Protocol)
    );
}

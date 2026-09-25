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
            Event::Accepted { id: 1 },
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

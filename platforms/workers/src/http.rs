//! Standards HTTP adapter inside a Durable Object. Accepted calls are bounded and
//! retained with State::wait_until. Runtime termination still interrupts work.
use futures_channel::oneshot;
use futures_util::{
    StreamExt,
    future::{Either, select},
};
use snap_http::Service;
use std::{cell::Cell, rc::Rc, time::Duration};
use worker::{Delay, Request, Response, Result, State};
pub struct Host<T> {
    service: T,
    state: Rc<State>,
    admitted: Rc<Cell<usize>>,
}
struct Permit(Rc<Cell<usize>>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
impl<T: Service> Host<T> {
    pub fn new(service: T, state: Rc<State>) -> Self {
        Self {
            service,
            state,
            admitted: Rc::new(Cell::new(0)),
        }
    }
    pub async fn fetch(&self, mut request: Request) -> Result<Response> {
        if !self.service.routes().contains(&request.path().as_str()) {
            return Response::error("Not found", 404);
        }
        if self.admitted.get() >= 64 {
            return Response::error("At capacity", 503);
        }
        let mut body = Vec::new();
        if let Ok(mut stream) = request.stream() {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if body.len() + chunk.len() > 64 * 1024 {
                    return Response::error("Body too large", 413);
                }
                body.extend(chunk);
            }
        }
        // Capacity is checked again after the request body suspension.
        if self.admitted.get() >= 64 {
            return Response::error("At capacity", 503);
        }
        self.admitted.set(self.admitted.get() + 1);
        let permit = Permit(self.admitted.clone());
        let req = snap_http::Request {
            method: request.method().to_string(),
            path: request.path(),
            query: request.url()?.query().unwrap_or("").into(),
            headers: request.headers().entries().collect(),
            body,
            now: crate::crypto::now(),
        };
        let future = self.service.call(req);
        let (send, receive) = oneshot::channel();
        self.state.wait_until(async move {
            let _permit = permit;
            let _ = send.send(future.await);
        });
        match select(receive, Box::pin(Delay::from(Duration::from_secs(180)))).await {
            Either::Left((Ok(reply), _)) => {
                let mut response = Response::from_bytes(reply.body)?.with_status(reply.status);
                for (name, value) in reply.headers {
                    response.headers_mut().append(&name, &value)?;
                }
                Ok(response)
            }
            _ => Response::error("Response deadline exceeded", 504),
        }
    }
}

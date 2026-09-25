use crate::{Peer, Platform};
use snap_transport::{
    Channel, Command, Error, Event, Response,
    server::{Application, Authority, Server},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub struct Memory<A: Application, R: Authority> {
    platform: Rc<RefCell<Platform<A, R>>>,
    now: Rc<Cell<u64>>,
    trace: Rc<RefCell<Vec<Event>>>,
}
impl<A: Application, R: Authority> Memory<A, R> {
    pub fn new(server: Server<A, R>) -> Self {
        Self {
            platform: Rc::new(RefCell::new(Platform::new(server))),
            now: Rc::default(),
            trace: Rc::default(),
        }
    }
    pub fn channel(&self) -> Connection<A, R> {
        Connection {
            platform: self.platform.clone(),
            now: self.now.clone(),
            trace: self.trace.clone(),
            peer: Peer::default(),
        }
    }
    pub fn advance(&self, milliseconds: u64) {
        self.now.set(
            self.now
                .get()
                .checked_add(milliseconds)
                .expect("clock exhausted"),
        );
        self.platform.borrow_mut().transport.tick(self.now.get());
    }
    pub fn residents(&self) -> usize {
        self.platform.borrow().transport.resident_count()
    }
    pub fn trace(&self) -> Vec<Event> {
        self.trace.borrow().clone()
    }
}
pub struct Connection<A: Application, R: Authority> {
    platform: Rc<RefCell<Platform<A, R>>>,
    now: Rc<Cell<u64>>,
    trace: Rc<RefCell<Vec<Event>>>,
    peer: Peer,
}
impl<A: Application, R: Authority> Channel for Connection<A, R> {
    async fn exchange(&mut self, command: Command) -> Result<Response, Error> {
        // A real scheduling boundary, without a byte-serialization round trip.
        // Dropping this future before delivery prevents admission.
        let mut yielded = false;
        core::future::poll_fn(|cx| {
            if yielded {
                core::task::Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                core::task::Poll::Pending
            }
        })
        .await;
        Ok(self
            .platform
            .borrow_mut()
            .receive(&mut self.peer, command, self.now.get(), |event| {
                self.trace.borrow_mut().push(event)
            }))
    }
}
impl<A: Application, R: Authority> Drop for Connection<A, R> {
    fn drop(&mut self) {
        self.platform
            .borrow_mut()
            .lost(&mut self.peer, self.now.get());
    }
}

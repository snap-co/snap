use crate::{Observation, Peer, Platform, Submission};
use snap_execution::{Program, Ticket};
use snap_transport::{Channel, Command, Error, Event, Response, server::Authority};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    task::{Poll, Waker},
};

struct Mailbox {
    events: Vec<Event>,
    done: bool,
    waker: Option<Waker>,
}
struct Host<P: Program, R: Authority> {
    platform: Platform<P, R>,
    now: u64,
    trace: Vec<Event>,
    mailboxes: BTreeMap<Ticket, Mailbox>,
    waiting: Option<(Ticket, String)>,
}
impl<P: Program, R: Authority> Host<P, R> {
    fn drive(&mut self) {
        while let Some(observation) = self.platform.step() {
            match observation {
                Observation::Need { ticket, key } => {
                    self.waiting = Some((ticket, key));
                }
                Observation::Event { ticket, event } => {
                    self.trace.push(event.clone());
                    if let Some(mailbox) = self.mailboxes.get_mut(&ticket) {
                        mailbox.done = matches!(event, Event::Completed { .. });
                        mailbox.events.push(event);
                        if let Some(waker) = mailbox.waker.take() {
                            waker.wake();
                        }
                    }
                }
            }
        }
    }
}

/// Async value delivery with explicit, virtual dependency completion. `supply`
/// resumes host-owned work even if its client future was dropped after submission.
pub struct Memory<P: Program, R: Authority> {
    host: Rc<RefCell<Host<P, R>>>,
}
impl<P: Program, R: Authority> Memory<P, R> {
    pub fn new(platform: Platform<P, R>) -> Self {
        Self {
            host: Rc::new(RefCell::new(Host {
                platform,
                now: 0,
                trace: Vec::new(),
                mailboxes: BTreeMap::new(),
                waiting: None,
            })),
        }
    }
    pub fn channel(&self) -> Connection<P, R> {
        Connection {
            host: self.host.clone(),
            peer: Peer::default(),
        }
    }
    pub fn advance(&self, milliseconds: u64) {
        let mut host = self.host.borrow_mut();
        host.now = host.now.checked_add(milliseconds).expect("clock exhausted");
        let now = host.now;
        host.platform.tick(now);
        host.drive();
    }
    pub fn residents(&self) -> usize {
        self.host.borrow().platform.transport.resident_count()
    }
    pub fn trace(&self) -> Vec<Event> {
        self.host.borrow().trace.clone()
    }
    pub fn pending_read(&self) -> Option<(Ticket, String)> {
        self.host.borrow().waiting.clone()
    }
    pub fn supply(
        &self,
        ticket: Ticket,
        key: &str,
        result: snap_execution::Outcome,
    ) -> Result<(), snap_execution::Error> {
        let mut host = self.host.borrow_mut();
        host.platform.execution.supply(ticket, key, result)?;
        host.waiting = None;
        host.drive();
        Ok(())
    }
    /// Host administration only. Replacement is allowed at a drained execution
    /// gate; no transport attachment or calculator data is reconstructed.
    pub fn replace(&self, program: P) -> Result<(), snap_execution::Error> {
        let mut host = self.host.borrow_mut();
        host.platform.execution.pause();
        let result = host.platform.execution.replace(program);
        host.platform.execution.resume();
        result
    }
}
pub struct Connection<P: Program, R: Authority> {
    host: Rc<RefCell<Host<P, R>>>,
    peer: Peer,
}
// Own only observation interest. Dropping a caller never cancels submitted work.
struct Interest<P: Program, R: Authority> {
    host: Rc<RefCell<Host<P, R>>>,
    ticket: Ticket,
}
impl<P: Program, R: Authority> Drop for Interest<P, R> {
    fn drop(&mut self) {
        self.host.borrow_mut().mailboxes.remove(&self.ticket);
    }
}
impl<P: Program, R: Authority> Channel for Connection<P, R> {
    async fn exchange(&mut self, command: Command) -> Result<Response, Error> {
        // Cancellation before delivery prevents submission entirely.
        let mut yielded = false;
        core::future::poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        let ticket = {
            let mut host = self.host.borrow_mut();
            let now = host.now;
            match host.platform.submit(&mut self.peer, command, now) {
                Submission::Ready(response) => {
                    if let Response::Events(events) = &response {
                        host.trace.extend(events.clone());
                    }
                    host.drive();
                    return Ok(response);
                }
                Submission::Pending(ticket) => {
                    host.mailboxes.insert(
                        ticket,
                        Mailbox {
                            events: Vec::new(),
                            done: false,
                            waker: None,
                        },
                    );
                    ticket
                }
            }
        };
        let _interest = Interest {
            host: self.host.clone(),
            ticket,
        };
        core::future::poll_fn(|cx| {
            let mut host = self.host.borrow_mut();
            host.drive();
            let mailbox = host.mailboxes.get_mut(&ticket).unwrap();
            if mailbox.done {
                Poll::Ready(Ok(Response::Events(core::mem::take(&mut mailbox.events))))
            } else {
                mailbox.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
impl<P: Program, R: Authority> Drop for Connection<P, R> {
    fn drop(&mut self) {
        let mut host = self.host.borrow_mut();
        let now = host.now;
        host.platform.lost(&mut self.peer, now);
        host.drive();
    }
}

//! Testy's controlled in-memory execution harness over portable Transport.
use snap_transport::execution;
use snap_transport::execution::{Observation, Peer, Runtime, Submission};
use snap_transport::execution::{Program, Ticket};
use snap_transport::{Channel, Command, Error, Event, Response, server::Authority};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    task::{Poll, Waker},
};

struct Mailbox {
    events: VecDeque<Event>,
    done: bool,
    waker: Option<Waker>,
}
struct Host<P: Program, R: Authority> {
    platform: Runtime<P, R>,
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
                Observation::Event {
                    ticket,
                    event,
                    private,
                    bearer,
                } => {
                    if matches!(event, Event::Completed { .. })
                        && self
                            .waiting
                            .as_ref()
                            .is_some_and(|(waiting, _)| *waiting == ticket)
                    {
                        self.waiting = None;
                    }
                    if !private {
                        self.trace.push(event.clone());
                    }
                    if let Some(mailbox) = self.mailboxes.get_mut(&ticket) {
                        mailbox.done = matches!(event, Event::Completed { .. });
                        if let Some(change) = bearer
                            && let Event::Completed { id, .. } = &event
                        {
                            mailbox.events.push_back(Event::Bearer { id: *id, change });
                        }
                        mailbox.events.push_back(event);
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
    pub fn new(platform: Runtime<P, R>) -> Self {
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
            tickets: VecDeque::new(),
            ready: VecDeque::new(),
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
        self.host.borrow().platform.residents()
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
        result: execution::Outcome,
    ) -> Result<(), execution::Error> {
        let mut host = self.host.borrow_mut();
        host.platform.supply(ticket, key, result)?;
        host.waiting = None;
        host.drive();
        Ok(())
    }
    /// Host administration only. Replacement is allowed at a drained execution
    /// gate; no transport attachment or calculator data is reconstructed.
    pub fn replace(&self, program: P) -> Result<(), execution::Error> {
        let mut host = self.host.borrow_mut();
        host.platform.pause();
        let result = host.platform.replace(program);
        host.platform.resume();
        result
    }
}
pub struct Connection<P: Program, R: Authority> {
    host: Rc<RefCell<Host<P, R>>>,
    peer: Peer,
    /// Invocations this logical connection has outstanding, in submission order.
    /// A channel is a stream, so more than one may be running at once.
    tickets: VecDeque<Ticket>,
    /// Frames the host answered without queueing: attachment, detachment, refusal,
    /// or a preconnection credential issue. These are observations like any
    /// other, so they are queued for `receive` rather than returned from `send`.
    ready: VecDeque<Response>,
}
impl<P: Program, R: Authority> Channel for Connection<P, R> {
    /// Submits and returns. Acceptance and progress reach the caller as later
    /// frames, which is what lets a slow operation publish before it finishes.
    async fn send(&mut self, command: Command) -> Result<(), Error> {
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
        let mut host = self.host.borrow_mut();
        let now = host.now;
        match host.platform.submit(&mut self.peer, command, now) {
            Submission::Ready(response) => {
                // Immediate host replies can contain issued credentials. The
                // execution trace only retains queued application observations.
                host.drive();
                self.ready.push_back(response);
            }
            Submission::Pending(ticket) => {
                host.mailboxes.insert(
                    ticket,
                    Mailbox {
                        events: VecDeque::new(),
                        done: false,
                        waker: None,
                    },
                );
                self.tickets.push_back(ticket);
            }
        }
        Ok(())
    }

    /// Yields the next observation, oldest invocation first. `None` only when no
    /// invocation is outstanding, because then nothing further can arrive.
    async fn receive(&mut self) -> Result<Option<Response>, Error> {
        core::future::poll_fn(|cx| {
            let mut host = self.host.borrow_mut();
            host.drive();
            if let Some(response) = self.ready.pop_front() {
                return Poll::Ready(Ok(Some(response)));
            }
            let mut index = 0;
            while index < self.tickets.len() {
                let ticket = self.tickets[index];
                let mailbox = host.mailboxes.get_mut(&ticket).unwrap();
                if let Some(event) = mailbox.events.pop_front() {
                    // A completed mailbox retires once its last frame is handed
                    // out. Submission order is preserved for the rest.
                    if mailbox.events.is_empty() && mailbox.done {
                        host.mailboxes.remove(&ticket);
                        self.tickets.remove(index);
                    }
                    return Poll::Ready(Ok(Some(Response::Event(event))));
                }
                index += 1;
            }
            if self.tickets.is_empty() {
                return Poll::Ready(Ok(None));
            }
            for ticket in self.tickets.clone() {
                if let Some(mailbox) = host.mailboxes.get_mut(&ticket) {
                    mailbox.waker = Some(cx.waker().clone());
                }
            }
            Poll::Pending
        })
        .await
    }
}
impl<P: Program, R: Authority> Drop for Connection<P, R> {
    fn drop(&mut self) {
        let mut host = self.host.borrow_mut();
        let now = host.now;
        // Submitted work is not cancelled, but this channel's observation
        // interest dies with it, so its frames have nowhere to go.
        for ticket in self.tickets.drain(..) {
            host.mailboxes.remove(&ticket);
        }
        host.platform.lost(&mut self.peer, now);
        host.drive();
    }
}

//! THROWAWAY: host-driven futures and a cached, conditional record store.
//! No IO, executor, wire format, or database dependencies. See README.md.
extern crate alloc;

use alloc::{collections::BTreeMap, rc::Rc, string::String, vec, vec::Vec};
use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub version: u64,
    pub bytes: Vec<u8>,
}

/// The executor checks every expected version, then applies every write atomically.
/// None means the key must be absent. This experiment has no deletes or scans.
#[derive(Clone, Debug)]
pub struct Batch {
    pub expected: Vec<(String, Option<u64>)>,
    pub writes: Vec<(String, Vec<u8>)>,
}

pub trait Store {
    fn read(&self, key: &str) -> impl Future<Output = Option<Record>>;
    fn commit(&self, batch: Batch) -> impl Future<Output = bool>;
}

pub trait Cache {
    fn get(&self, key: &str) -> Option<Record>;
    fn put(&mut self, key: String, record: Record);
    fn clear(&mut self);
    fn resident(&self) -> usize;
}

#[derive(Default)]
pub struct MemoryCache(BTreeMap<String, Record>);
impl Cache for MemoryCache {
    fn get(&self, key: &str) -> Option<Record> {
        self.0.get(key).cloned()
    }
    fn put(&mut self, key: String, record: Record) {
        self.0.insert(key, record);
    }
    fn clear(&mut self) {
        self.0.clear();
    }
    fn resident(&self) -> usize {
        self.0.len()
    }
}

pub struct NoCache;
impl Cache for NoCache {
    fn get(&self, _: &str) -> Option<Record> {
        None
    }
    fn put(&mut self, _: String, _: Record) {}
    fn clear(&mut self) {}
    fn resident(&self) -> usize {
        0
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    Read(String),
    Commit(Batch),
}
#[derive(Debug)]
pub enum Response {
    Read(Option<Record>),
    Committed(bool),
}

pub struct Job {
    id: u64,
    epoch: u64,
    pub request: Request,
}
struct Slot {
    request: Option<Request>,
    response: Option<Response>,
    waker: Option<Waker>,
}
struct State<C> {
    cache: C,
    epoch: u64,
    next: u64,
    slots: BTreeMap<u64, Slot>,
}

pub struct Port<C>(Rc<RefCell<State<C>>>);
pub struct Driver<C>(Rc<RefCell<State<C>>>);

pub fn channel<C: Cache>(cache: C) -> (Port<C>, Driver<C>) {
    let state = Rc::new(RefCell::new(State {
        cache,
        epoch: 0,
        next: 0,
        slots: BTreeMap::new(),
    }));
    (Port(state.clone()), Driver(state))
}

impl<C: Cache> Store for Port<C> {
    async fn read(&self, key: &str) -> Option<Record> {
        // The borrow ends before any suspension. Callers get an owned snapshot.
        let resident = self.0.borrow().cache.get(key);
        if resident.is_some() {
            return resident;
        }
        match self.request(Request::Read(key.into())).await {
            Response::Read(record) => record,
            _ => panic!("prototype executor returned the wrong response"),
        }
    }
    async fn commit(&self, batch: Batch) -> bool {
        match self.request(Request::Commit(batch)).await {
            Response::Committed(committed) => committed,
            _ => panic!("prototype executor returned the wrong response"),
        }
    }
}

impl<C> Port<C> {
    fn request(&self, request: Request) -> Waiting<C> {
        let mut state = self.0.borrow_mut();
        state.next += 1;
        let id = state.next;
        state.slots.insert(
            id,
            Slot {
                request: Some(request),
                response: None,
                waker: None,
            },
        );
        Waiting {
            state: self.0.clone(),
            id,
        }
    }
}

struct Waiting<C> {
    state: Rc<RefCell<State<C>>>,
    id: u64,
}
impl<C> Future for Waiting<C> {
    type Output = Response;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Response> {
        let mut state = self.state.borrow_mut();
        let slot = state.slots.get_mut(&self.id).expect("live request");
        if let Some(response) = slot.response.take() {
            return Poll::Ready(response);
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
impl<C> Drop for Waiting<C> {
    fn drop(&mut self) {
        self.state.borrow_mut().slots.remove(&self.id);
    }
}

impl<C: Cache> Driver<C> {
    /// Taking a job is the experiment's acceptance point. Dropping its caller
    /// after this point removes the waiter but does not undo accepted work.
    pub fn take(&self) -> Option<Job> {
        let mut state = self.0.borrow_mut();
        let epoch = state.epoch;
        state.slots.iter_mut().find_map(|(&id, slot)| {
            slot.request
                .take()
                .map(|request| Job { id, epoch, request })
        })
    }

    /// Complete on the owner thread. A production IO worker would send this
    /// result back to that thread. Wake outside the borrow to permit re-entry.
    pub fn complete(&self, job: Job, response: Response) -> bool {
        let waker = {
            let mut state = self.0.borrow_mut();
            match (&job.request, &response) {
                (Request::Read(key), Response::Read(Some(record))) if job.epoch == state.epoch => {
                    state.cache.put(key.clone(), record.clone());
                }
                (Request::Commit(_), Response::Committed(_)) => {
                    // Conservative for the experiment: even a rejected commit
                    // invalidates stale snapshots. This also happens after cancellation.
                    state.epoch += 1;
                    state.cache.clear();
                }
                _ => {}
            }
            let Some(slot) = state.slots.get_mut(&job.id) else {
                return false;
            };
            slot.response = Some(response);
            slot.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    // Inspection only for the demo, not a proposed production interface.
    pub fn inspect(&self) -> (usize, usize, usize) {
        let state = self.0.borrow();
        (
            state.cache.resident(),
            state.slots.len(),
            state.slots.values().filter(|s| s.request.is_some()).count(),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Missing,
    Conflict,
    Created { credential_version: u64 },
}

/// Session-shaped workflow only. Password checking is deliberately omitted;
/// the fixture record contains an identity, not a password or a real verifier.
pub async fn session_workflow(store: &impl Store, locator: &str, session: &str) -> Outcome {
    let Some(credential) = store.read(locator).await else {
        return Outcome::Missing;
    };
    let version = credential.version;
    let committed = store
        .commit(Batch {
            expected: vec![(locator.into(), Some(version)), (session.into(), None)],
            writes: vec![(session.into(), credential.bytes)],
        })
        .await;
    if committed {
        Outcome::Created {
            credential_version: version,
        }
    } else {
        Outcome::Conflict
    }
}

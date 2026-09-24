//! THROWAWAY executable. Run the companion run.py for the narrated trace and HTML.
#[path = "../../../crates/runtime/prototypes/store-continuation/core.rs"]
mod portable;

use portable::{
    Batch, Cache, Driver, MemoryCache, NoCache, Outcome, Record, Request, Response, Store,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

const CREDENTIAL: &str = "credentials/demo@example.test";
const SESSION: &str = "sessions/demo-session";

// The host's two concrete executors understand records and atomic conditions,
// with no Passport-, Identity-, credential-, or session-specific methods.
trait Backend {
    fn execute(&mut self, request: &Request) -> Response;
}
#[derive(Default)]
struct Memory(BTreeMap<String, Record>);
impl Backend for Memory {
    fn execute(&mut self, request: &Request) -> Response {
        match request {
            Request::Read(key) => Response::Read(self.0.get(key).cloned()),
            Request::Commit(batch) => {
                if batch
                    .expected
                    .iter()
                    .any(|(key, version)| self.0.get(key).map(|r| r.version) != *version)
                {
                    return Response::Committed(false);
                }
                for (key, bytes) in &batch.writes {
                    let version = self.0.get(key).map_or(1, |r| r.version + 1);
                    self.0.insert(
                        key.clone(),
                        Record {
                            version,
                            bytes: bytes.clone(),
                        },
                    );
                }
                Response::Committed(true)
            }
        }
    }
}

struct Sqlite {
    db: Option<Connection>,
    path: std::path::PathBuf,
}
impl Sqlite {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::PathBuf::from(format!(
            "/tmp/opencode/PROTOTYPE-wipe-me-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE records (key TEXT PRIMARY KEY, version INTEGER NOT NULL, bytes BLOB NOT NULL)").unwrap();
        Self { db: Some(db), path }
    }
}
impl Drop for Sqlite {
    fn drop(&mut self) {
        drop(self.db.take());
        std::fs::remove_file(&self.path).unwrap();
    }
}
impl Backend for Sqlite {
    fn execute(&mut self, request: &Request) -> Response {
        let db = self.db.as_mut().unwrap();
        match request {
            Request::Read(key) => Response::Read(
                db.query_row(
                    "SELECT version, bytes FROM records WHERE key = ?1",
                    [key],
                    |row| {
                        Ok(Record {
                            version: row.get::<_, i64>(0)?.try_into().unwrap(),
                            bytes: row.get(1)?,
                        })
                    },
                )
                .optional()
                .unwrap(),
            ),
            Request::Commit(batch) => {
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .unwrap();
                for (key, expected) in &batch.expected {
                    let version: Option<i64> = tx
                        .query_row("SELECT version FROM records WHERE key = ?1", [key], |row| {
                            row.get(0)
                        })
                        .optional()
                        .unwrap();
                    if version.map(|v| u64::try_from(v).unwrap()) != *expected {
                        return Response::Committed(false);
                    }
                }
                for (key, bytes) in &batch.writes {
                    tx.execute("INSERT INTO records VALUES (?1, 1, ?2) ON CONFLICT(key) DO UPDATE SET version = version + 1, bytes = excluded.bytes", params![key, bytes]).unwrap();
                }
                tx.commit().unwrap();
                Response::Committed(true)
            }
        }
    }
}

#[derive(Default)]
struct Signal(AtomicBool);
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Task<F: Future> {
    future: Pin<Box<F>>,
    signal: Arc<Signal>,
}
impl<F: Future> Task<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
            signal: Arc::new(Signal(AtomicBool::new(true))),
        }
    }
    fn turn(&mut self) -> Poll<F::Output> {
        // No busy polling: a suspended task must have been woken by completion.
        assert!(
            self.signal.0.swap(false, Ordering::SeqCst),
            "task was not woken"
        );
        let waker = Waker::from(self.signal.clone());
        self.future.as_mut().poll(&mut Context::from_waker(&waker))
    }
}

fn note<C: Cache>(steps: &mut Vec<Value>, driver: &Driver<C>, title: &str, detail: &str) {
    let (resident, waiters, queued) = driver.inspect();
    steps.push(json!({ "title": title, "detail": detail, "resident": resident, "waiters": waiters, "queued": queued }));
}
fn deliver<C: Cache>(driver: &Driver<C>, backend: &mut dyn Backend, steps: &mut Vec<Value>) {
    let job = driver.take().expect("one pending host job");
    let detail = format!("Host executes {:?}", job.request);
    let response = backend.execute(&job.request);
    assert!(driver.complete(job, response));
    note(
        steps,
        driver,
        "Host returned a result and woke the caller",
        &detail,
    );
}
fn seed(backend: &mut dyn Backend, version: Option<u64>) {
    let response = backend.execute(&Request::Commit(Batch {
        expected: vec![(CREDENTIAL.into(), version)],
        writes: vec![(CREDENTIAL.into(), b"identity:demo".to_vec())],
    }));
    assert!(matches!(response, Response::Committed(true)));
}

fn scenario<C: Cache>(backend: &mut dyn Backend, cache: C, mode: &str) -> Vec<Value> {
    seed(backend, None);
    let (store, driver) = portable::channel(cache);
    let mut steps = Vec::new();
    note(
        &mut steps,
        &driver,
        "Ready",
        "Credential exists only in the authoritative store. No invocation is running.",
    );

    if mode == "prefetch" || mode == "stale" || mode == "no-cache" {
        let mut prefetch = Task::new(store.read(CREDENTIAL));
        assert!(prefetch.turn().is_pending());
        note(
            &mut steps,
            &driver,
            "Prefetch suspended",
            "The ordinary read interface starts the load before the session workflow arrives.",
        );
        deliver(&driver, backend, &mut steps);
        assert!(matches!(prefetch.turn(), Poll::Ready(Some(_))));
        note(
            &mut steps,
            &driver,
            "Prefetch finished",
            "MemoryCache retains a snapshot; NoCache discards it after returning the value.",
        );
        if mode == "stale" {
            seed(backend, Some(1));
            note(
                &mut steps,
                &driver,
                "Another writer changed the credential",
                "The authoritative record is now version 2. The resident snapshot is still version 1.",
            );
        }
    }

    let mut task = Task::new(portable::session_workflow(&store, CREDENTIAL, SESSION));
    assert!(task.turn().is_pending());
    let warm = mode == "prefetch" || mode == "stale";
    if warm {
        note(
            &mut steps,
            &driver,
            "Read completed without suspension; commit suspended",
            "One poll passed through the read await using a resident snapshot and reached the durable-write await.",
        );
    } else {
        note(
            &mut steps,
            &driver,
            "Workflow suspended on the missing record",
            "The compiler retains the invocation's continuation. No database call ran while polling it.",
        );
        let mut other = Task::new(async { "unrelated invocation completed" });
        assert!(other.turn().is_ready());
        note(
            &mut steps,
            &driver,
            "Host ran another invocation",
            "The first invocation is still suspended. The host chooses when to service its IO request.",
        );
        if mode == "cancel-queued" {
            drop(task);
            assert!(driver.take().is_none());
            note(
                &mut steps,
                &driver,
                "Caller cancelled before host acceptance",
                "Dropping the future removed its queued read. There is no job or waiter left.",
            );
            return steps;
        }
        deliver(&driver, backend, &mut steps);
        assert!(task.turn().is_pending());
        note(
            &mut steps,
            &driver,
            "Workflow resumed, then suspended on commit",
            "Local credential data survived across await. The commit checks its version and session-key absence atomically.",
        );
    }

    if mode == "cancel-accepted" {
        let job = driver.take().unwrap();
        assert!(matches!(job.request, Request::Commit(_)));
        drop(task);
        note(
            &mut steps,
            &driver,
            "Caller cancelled after host acceptance",
            "The host owns the accepted write. The caller's continuation and waiter are gone.",
        );
        let response = backend.execute(&job.request);
        assert!(matches!(response, Response::Committed(true)));
        assert!(!driver.complete(job, response));
        assert!(matches!(
            backend.execute(&Request::Read(SESSION.into())),
            Response::Read(Some(_))
        ));
        note(
            &mut steps,
            &driver,
            "Accepted write completed without a caller",
            "The session exists. Completion invalidated the cache, but woke nobody. Cancellation did not roll back the write.",
        );
        return steps;
    }

    deliver(&driver, backend, &mut steps);
    let expected = if mode == "stale" {
        Outcome::Conflict
    } else {
        Outcome::Created {
            credential_version: 1,
        }
    };
    assert_eq!(task.turn(), Poll::Ready(expected));
    let stored = backend.execute(&Request::Read(SESSION.into()));
    if mode == "stale" {
        assert!(matches!(stored, Response::Read(None)));
        note(
            &mut steps,
            &driver,
            "Stale authority rejected",
            "The atomic version check failed. No session was created, and the stale cache was cleared. No automatic retry.",
        );
    } else {
        assert!(matches!(stored, Response::Read(Some(_))));
        note(
            &mut steps,
            &driver,
            "Session created",
            "The same portable function completed with both storage implementations. A read hit saved a suspension; the write still awaited authority.",
        );
    }
    steps
}

fn main() {
    let mut traces = Vec::new();
    for backend_name in ["memory", "sqlite"] {
        for mode in [
            "cold",
            "prefetch",
            "no-cache",
            "stale",
            "cancel-queued",
            "cancel-accepted",
        ] {
            let mut backend: Box<dyn Backend> = match backend_name {
                "memory" => Box::<Memory>::default(),
                _ => Box::new(Sqlite::new()),
            };
            let steps = if mode == "no-cache" {
                scenario(&mut *backend, NoCache, mode)
            } else {
                scenario(&mut *backend, MemoryCache::default(), mode)
            };
            eprintln!("PASS {backend_name}/{mode}: {} narrated steps", steps.len());
            traces.push(json!({ "backend": backend_name, "mode": mode, "steps": steps }));
        }
    }
    println!("{}", serde_json::to_string_pretty(&traces).unwrap());
}

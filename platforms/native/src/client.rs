//! Native client host. Consumers use this SDK directly; no language binding is needed.
use snap_client::{
    HealthReport,
    application::{Application, Input, Step},
};
use snap_protocol::{Error, Invocation};
use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::watch, task::JoinHandle};

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    base: reqwest::Url,
    build: String,
}

impl Http {
    pub fn new(base: &str, build: &str) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(unavailable)?,
            base: reqwest::Url::parse(base).map_err(unavailable)?,
            build: build.into(),
        })
    }

    pub async fn query(&self, invocation: &Invocation) -> Result<String, Error> {
        let mut url = self.base.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.path_segments_mut()
            .map_err(|_| unavailable("Base URL cannot carry paths"))?
            .clear()
            .extend(invocation.key.split('.'));
        self.client
            .get(url)
            .header("x-snap-operation-id", &invocation.operation_id)
            .header("x-snap-build", &self.build)
            .send()
            .await
            .map_err(unavailable)?
            .text()
            .await
            .map_err(unavailable)
    }
}

pub struct Client {
    core: Mutex<Option<snap_client::Client>>,
    http: Http,
    closed: watch::Sender<bool>,
}

impl Client {
    pub fn new(http: Http) -> Self {
        Self {
            core: Mutex::new(Some(snap_client::Client::default())),
            http,
            closed: watch::channel(false).0,
        }
    }

    pub async fn health_up(&self) -> Result<HealthReport, Error> {
        let mut closed = self.closed.subscribe();
        let query = self
            .core
            .lock()
            .expect("client lock poisoned")
            .as_mut()
            .ok_or_else(|| unavailable("Client is closed"))?
            .health_up()?;
        let wire = tokio::select! {
            biased;
            _ = closed.changed() => return Err(unavailable("Client is closed")),
            result = self.http.query(query.invocation()) => result?,
        };
        query.complete(&wire)
    }

    pub fn close(&self) {
        self.core.lock().expect("client lock poisoned").take();
        self.closed.send_replace(true);
    }
}

pub struct Running<S: Clone> {
    state: watch::Receiver<S>,
    task: Option<JoinHandle<()>>,
}

impl<S: Clone> Running<S> {
    pub fn snapshot(&self) -> S {
        self.state.borrow().clone()
    }
    pub async fn changed(&mut self) -> Result<S, Error> {
        self.state.changed().await.map_err(unavailable)?;
        Ok(self.state.borrow_and_update().clone())
    }
    pub async fn close(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl<S: Clone> Drop for Running<S> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub fn start<A>(mut app: A, http: Http) -> Running<A::Snapshot>
where
    A: Application + Send + 'static,
    A::Snapshot: Send + Sync + 'static,
{
    let (state, receiver) = watch::channel(app.snapshot());
    let task = tokio::spawn(async move {
        let mut input = Input::Start;
        loop {
            let step = app.update(input);
            state.send_replace(app.snapshot());
            input = match step {
                Step::Query(invocation) => Input::Completed {
                    result: http.query(&invocation).await,
                    at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                },
                Step::Wait { milliseconds } => {
                    tokio::time::sleep(Duration::from_millis(milliseconds.into())).await;
                    Input::Wake
                }
                Step::Stop => break,
            };
        }
    });
    Running {
        state: receiver,
        task: Some(task),
    }
}

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::UnavailableError {
        message: error.to_string(),
    }
}

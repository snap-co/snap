//! Benchmark host adapters. Only assembly, clocks, execution and teardown live
//! here; both adapters consume the same cartridge plan and portable measurement.
use super::{TcpChannel, tls_support};

use serde::{Deserialize, Serialize};
use snap_platform_tests::{
    benchmark::{self, Clock, Report},
    cartridge::benchmark::{self as cartridge, Plan, Profile},
    runner::Random,
    simulation::{Schedule, Simulation, Store as SimStore, Timeline},
};
use snap_store::{Backend, Catalog, Row, Store, memory::Memory};
use snap_store_sqlite::Sqlite;
use snap_transport::{
    client::Client,
    host::Blocking,
    native::driver::{Dispatcher, Shared},
};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    Simulation,
    TcpMemory,
    TcpSqlite,
}
impl Host {
    pub fn name(self) -> &'static str {
        match self {
            Self::Simulation => "simulation",
            Self::TcpMemory => "tcp-memory",
            Self::TcpSqlite => "tcp-sqlite",
        }
    }
    pub fn response_clock(self) -> &'static str {
        if self == Self::Simulation {
            "virtual"
        } else {
            "wall"
        }
    }
    pub fn settings(self) -> serde_json::Value {
        let schedule = simulation_schedule(0);
        match self {
            Self::Simulation => serde_json::json!({
                "carrier": "structured-fifo", "store": "volatile-memory",
                "command_ms": schedule.command_ms, "admission_ms": schedule.admission_ms,
                "response_ms": schedule.response_ms, "execution_ms": schedule.execution_ms,
                "load_ms": schedule.load_ms, "commit_ms": schedule.commit_ms,
                "jitter_ms": schedule.jitter_ms, "trace_capacity": schedule.trace_capacity,
                "event_fingerprinting": true, "faults": false,
            }),
            _ => serde_json::json!({
                "carrier": "tcp-tls-loopback", "store": if self == Self::TcpMemory { "volatile-memory" } else { "sqlite-file" },
                "client_executor": "tokio-current-thread", "faults": false,
            }),
        }
    }
    /// Implementation facts are evidence, not comparison keys: changing server
    /// execution is precisely what this benchmark should compare across revisions.
    /// Update these descriptions when the mounted production implementation changes.
    pub fn implementation(self) -> serde_json::Value {
        if self == Self::Simulation {
            serde_json::json!({"single_actor": true, "executor": "event-scheduler"})
        } else {
            serde_json::json!({"single_actor": true, "driver": "production-dispatch", "dispatch_tick_ms": 5})
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Config {
    pub seed: u64,
    pub profile: Profile,
    pub clients: usize,
    pub warmup: u64,
    pub operations: u64,
}
impl Config {
    pub fn plan(&self) -> Plan {
        Plan::new(
            self.seed,
            self.profile,
            self.clients,
            self.warmup,
            self.operations,
        )
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=127).contains(&self.clients)
            || !(1..=1_000_000).contains(&self.operations)
            || self.warmup > 1_000_000
        {
            return Err(io::Error::other(
                "clients must be 1..127; ops 1..1000000; warmup 0..1000000",
            )
            .into());
        }
        if self.operations < self.clients as u64 {
            return Err(io::Error::other(
                "ops must give every selected client at least one invocation",
            )
            .into());
        }
        Ok(())
    }
}

pub struct WallClock(Instant);
impl WallClock {
    pub fn new() -> Self {
        Self(Instant::now())
    }
}
impl Clock for WallClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).expect("wall clock overflow")
    }
}
struct VirtualClock(Timeline);
impl Clock for VirtualClock {
    fn now_ns(&self) -> u64 {
        self.0
            .now()
            .checked_mul(1_000_000)
            .expect("virtual clock overflow")
    }
}
fn catalog() -> Result<Catalog> {
    Ok(cartridge::migration().apply(&Catalog::default())?)
}
fn mount<B: Backend>(mut store: Store<B>, plan: &Plan) -> Result<Blocking<B>> {
    let keys = plan
        .keys
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    store.run("bench.seed", |tx| {
        for key in &keys {
            for table in cartridge::TABLES {
                tx.insert(
                    table,
                    Row::from([("id".into(), (*key).into()), ("value".into(), 0.into())]),
                )?;
            }
        }
        Ok(())
    })?;
    let mut registry = snap_transport::operation::Registry::default();
    for definition in cartridge::definitions() {
        registry = registry.with_request(definition);
    }
    Ok(Blocking::new(
        store,
        (),
        registry,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |_, bearer| {
                if bearer == "alice" {
                    Ok("alice".into())
                } else {
                    Err(snap_store::Error::NotFound)
                }
            },
        ))),
        Default::default(),
        "benchmark-boot".into(),
    ))
}

pub fn simulated(config: &Config) -> Result<Report> {
    config.validate()?;
    let plan = config.plan();
    let timeline = Timeline::new(simulation_schedule(config.seed));
    let catalog = catalog()?;
    let backend = SimStore::new(catalog.clone(), timeline.clone())?;
    let mut simulation = Simulation::new(
        mount(Store::new(catalog, backend)?, &plan)?,
        timeline.clone(),
    );
    let mut clients = Vec::new();
    for actor in 0..config.clients {
        let mut client = Client::new(
            simulation
                .open()
                .map_err(|e| io::Error::other(format!("open: {e:?}")))?,
        );
        if simulation
            .run(client.connect("alice", &format!("benchmark-{actor}")))
            .map_err(|e| io::Error::other(format!("setup: {e:?}")))?
            .map_err(|e| io::Error::other(format!("connect: {e:?}")))?
        {
            return Err(io::Error::other("fresh client unexpectedly resumed").into());
        }
        clients.push(client);
    }
    let mut actors = plan.actors(clients);
    let response_clock = VirtualClock(timeline);
    let wall_clock = WallClock::new();
    let drive = |error| io::Error::other(format!("simulation: {error:?}"));
    simulation
        .run(benchmark::run(
            &mut actors,
            config.warmup,
            &response_clock,
            &wall_clock,
        ))
        .map_err(drive)?
        .map_err(io::Error::other)?;
    let report = simulation
        .run(benchmark::run(
            &mut actors,
            config.operations,
            &response_clock,
            &wall_clock,
        ))
        .map_err(drive)?
        .map_err(io::Error::other)?;
    simulation
        .run(cartridge::verify(&mut actors))
        .map_err(drive)?
        .map_err(io::Error::other)?;
    drop(actors);
    simulation.finish().map_err(drive)?;
    Ok(report)
}

fn simulation_schedule(seed: u64) -> Schedule {
    Schedule {
        seed: Random::stream(seed, "benchmark.schedule").next_u64(),
        max_events: 100_000_000,
        max_polls: 100_000_000,
        max_time_ms: u64::MAX,
        trace_capacity: 0,
        ..Default::default()
    }
}

struct Server<B: Backend> {
    shared: Arc<Shared<Blocking<B>>>,
    serving: tokio::task::JoinHandle<io::Result<()>>,
    dispatch: tokio::task::JoinHandle<()>,
    address: String,
    tls: snap_transport::native::tls::ClientTls,
    _pki: tempfile::TempDir,
}
impl<B: Backend> Drop for Server<B> {
    fn drop(&mut self) {
        self.serving.abort();
        self.dispatch.abort();
    }
}
impl<B: Backend + Send + 'static> Server<B> {
    async fn start(host: Blocking<B>) -> Result<Self> {
        let pki = tempfile::tempdir()?;
        let (server_tls, tls) = tls_support::pki(pki.path(), false);
        let shared = Shared::new(host);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?.to_string();
        let serving = tokio::spawn(snap_transport::native::tcp::serve(
            listener,
            Dispatcher::tcp(shared.clone(), None),
            server_tls,
        ));
        let dispatch = tokio::spawn(snap_transport::native::driver::dispatch(shared.clone()));
        Ok(Self {
            shared,
            serving,
            dispatch,
            address,
            tls,
            _pki: pki,
        })
    }
    async fn stop(mut self) -> Result<()> {
        self.serving.abort();
        self.dispatch.abort();
        let serving = (&mut self.serving).await;
        let dispatch = (&mut self.dispatch).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&self.shared) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        match serving {
            Ok(result) => result?,
            Err(e) if !e.is_cancelled() => return Err(e.into()),
            _ => {}
        }
        if let Err(error) = dispatch
            && !error.is_cancelled()
        {
            return Err(error.into());
        }
        Ok(())
    }
}

async fn native_store<B: Backend + Send + 'static>(
    store: Store<B>,
    config: &Config,
    timeout: Duration,
) -> Result<Report> {
    let plan = config.plan();
    let server = Server::start(mount(store, &plan)?).await?;
    let execution = tokio::time::timeout(timeout, async {
        let mut clients = Vec::new();
        for actor in 0..config.clients {
            let mut client = Client::new(TcpChannel::open(&server.address, &server.tls).await?);
            if client
                .connect("alice", &format!("benchmark-{actor}"))
                .await
                .map_err(|e| io::Error::other(format!("connect: {e:?}")))?
            {
                return Err(io::Error::other("fresh client unexpectedly resumed").into());
            }
            clients.push(client);
        }
        let mut actors = plan.actors(clients);
        let clock = WallClock::new();
        benchmark::run(&mut actors, config.warmup, &clock, &clock)
            .await
            .map_err(io::Error::other)?;
        let report = benchmark::run(&mut actors, config.operations, &clock, &clock)
            .await
            .map_err(io::Error::other)?;
        cartridge::verify(&mut actors)
            .await
            .map_err(io::Error::other)?;
        Ok::<_, Box<dyn std::error::Error>>(report)
    })
    .await;
    // The timed-out client futures drop before host ownership is released. No
    // retries or success report for unknown outcomes, including during teardown.
    server.stop().await?;
    execution?
}

pub async fn sample(host: Host, config: &Config, timeout: Duration) -> Result<Report> {
    config.validate()?;
    match host {
        Host::Simulation => simulated(config),
        Host::TcpMemory => {
            let catalog = catalog()?;
            native_store(
                Store::new(catalog.clone(), Memory::new(catalog)?)?,
                config,
                timeout,
            )
            .await
        }
        Host::TcpSqlite => {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("benchmark.sqlite");
            snap_store_sqlite::migrate(&path, &[cartridge::migration()])?;
            native_store(Sqlite::open(&path)?, config, timeout).await
        }
    }
}

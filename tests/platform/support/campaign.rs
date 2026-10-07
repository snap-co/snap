//! Native assembly only. Workload policy and the contention oracle stay portable;
//! the runner knows nothing about this cartridge or these fault schedules.
use super::setup;
use snap_platform_tests::{
    cartridge::Read,
    runner::{self, Recording, Transcript},
    simulation::{Channel, Clock, Schedule, Simulation, Store, Timeline},
    workload::{
        Probe, World,
        concurrent::{self, Rejections},
    },
};
use snap_transport::{
    Command, Event, Invocation, Operation, Response, client::Client, host::Blocking, json,
};
use std::{cell::Cell, io, rc::Rc};

pub type Actor = concurrent::Probe<Recording<Channel>, Clock>;
pub struct Campaign {
    pub simulation: Simulation<Blocking<Store>>,
    pub timeline: Timeline,
    pub actors: Vec<Actor>,
    pub transcripts: Vec<Transcript>,
    pub server_checks: Rc<Cell<u64>>,
    pub fault_injections: Rc<Cell<u64>>,
}
pub fn assemble(
    schedule: Schedule,
    world: &World,
    clients: usize,
    faults_enabled: bool,
) -> io::Result<Campaign> {
    // Production Blocking supports 128 physical peers. Reserve one for the
    // scheduled server request; this is a setup bound, not a runner restriction.
    if !(1..=127).contains(&clients) {
        return Err(io::Error::other("--clients must be 1..=127"));
    }
    let (mut simulation, timeline, faults) = setup::setup(schedule);
    let transcripts: Vec<_> = (0..clients).map(|_| Transcript::default()).collect();
    let mut sdks = Vec::new();
    for transcript in &transcripts {
        let channel = simulation
            .open()
            .map_err(|error| io::Error::other(format!("open: {error:?}")))?;
        sdks.push(Client::new(transcript.channel(channel)));
    }
    simulation
        .run(runner::join(
            sdks.iter_mut()
                .enumerate()
                .map(|(actor, client)| {
                    Box::pin(async move {
                        assert!(
                            !client
                                .connect("alice", &format!("campaign-client-{actor}"))
                                .await
                                .expect("campaign connect failed")
                        );
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>
                })
                .collect(),
        ))
        .map_err(|error| io::Error::other(format!("connect schedule: {error:?}")))?;
    let mut initializer = Probe::new(sdks.remove(0));
    simulation
        .run(initializer.initialize(world))
        .map_err(|error| io::Error::other(format!("world setup: {error:?}")))?;
    sdks.insert(0, initializer.into_client());
    let rejections = Rejections::default();
    let actors = concurrent::Probe::actors(sdks, simulation.clock(), world, rejections.clone());
    let server_checks = Rc::new(Cell::new(0u64));
    let checks = server_checks.clone();
    let mut peer = None;
    let mut sequence = 0;
    let mut pending = false;
    // A server-owned periodic consistency check runs real Read operations through
    // production dispatch. It has no physical client and cannot synthesize outputs.
    simulation.schedule_task(
        timeline.now() + 1_000,
        Some(1_000),
        "paired-row-check",
        move |host, now| {
            let peer = *peer.get_or_insert_with(|| host.open().expect("scheduled check peer"));
            while let Some(response) = host.output(peer).unwrap().pop_front() {
                match response {
                    Response::Event(Event::Accepted { id }) => assert_eq!(id, sequence),
                    Response::Event(Event::Completed { id, outcome }) => {
                        assert_eq!(id, sequence);
                        let rows: [i64; 2] =
                            serde_json::from_value(outcome.expect("scheduled Read failed"))
                                .unwrap();
                        assert_eq!(rows[0], rows[1], "scheduled check found torn paired rows");
                        checks.set(checks.get() + 1);
                        pending = false;
                    }
                    other => panic!("unexpected scheduled check output: {other:?}"),
                }
            }
            if !pending {
                sequence += 1;
                host.submit(
                    peer,
                    Command::Request {
                        bearer: Some("alice".into()),
                        invocation: Invocation {
                            id: sequence,
                            operation: Read::NAME.into(),
                            input: json!(null),
                        },
                    },
                    now,
                )
                .expect("scheduled Read submission failed");
                pending = true;
            }
        },
    );
    let fault_injections = Rc::new(Cell::new(0u64));
    if faults_enabled {
        let injections = fault_injections.clone();
        simulation.schedule_task(
            timeline.now() + 97,
            Some(97),
            "reject-next-commit",
            move |_, _| {
                rejections.arm();
                faults.reject_next();
                injections.set(injections.get() + 1);
            },
        );
    }
    Ok(Campaign {
        simulation,
        timeline,
        actors,
        transcripts,
        server_checks,
        fault_injections,
    })
}

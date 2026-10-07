//! Real SDK and production execution at loss boundaries. Unlike carrier scripts,
//! these tests must reach actual admission/commit and retain accepted work while
//! replacing physical peers. The bounded model must preserve unknown outcomes,
//! then reject drift outside its input-derived possibilities.
use crate::{campaign_setup, setup};
use snap_platform_tests::{
    cartridge::{Change, Edit, Read, Stop},
    runner::{self, Budget, Config, Drive, Host, Workload},
    simulation::{Action, LossBoundary, NetworkFaults, Schedule},
    workload::{Action as InputAction, World, concurrent::Input},
};
use snap_transport::{
    Error, Operation,
    client::{Client, Pump},
    json,
};
use std::{future::Future, pin::Pin};

const BOUNDARIES: [LossBoundary; 3] = [
    LossBoundary::BeforeAdmission,
    LossBoundary::AfterAcceptance,
    LossBoundary::BeforeCompletionDelivery,
];
fn policy(boundary: Option<LossBoundary>, one_in: u64) -> NetworkFaults {
    NetworkFaults {
        seed: 42,
        operation: Change::NAME.into(),
        one_in,
        boundary,
    }
}
fn schedule() -> Schedule {
    Schedule {
        seed: 19,
        execution_ms: 100,
        response_ms: 20,
        jitter_ms: 0,
        max_events: 100_000,
        max_polls: 100_000,
        ..Default::default()
    }
}

#[test]
fn loss_boundaries_preserve_effects_without_replaying_or_redirecting_old_results() {
    for boundary in BOUNDARIES {
        let (mut simulation, timeline, _) = setup::setup(schedule());
        let channel = simulation.open_for(0).unwrap();
        let old_peer = channel.peer();
        let mut client = Client::new(channel);
        let healthy_channel = simulation.open_for(1).unwrap();
        let healthy_peer = healthy_channel.peer();
        let mut healthy = Client::new(healthy_channel);
        simulation
            .run(async {
                client.connect("alice", "losing").await.unwrap();
                healthy.connect("alice", "healthy").await.unwrap();
            })
            .unwrap();
        let trace_start = timeline.trace().len();
        simulation.network_faults(policy(Some(boundary), 1));
        let connector = simulation.connector(0);
        let clock = simulation.clock();
        simulation
            .run(runner::join(vec![
                Box::pin(async {
                    let id = client
                        .begin(
                            Change::NAME,
                            serde_json::to_value(Edit {
                                expected: 0,
                                amount: 5,
                                stop: Stop::Commit,
                            })
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    loop {
                        match client.pump().await {
                            Ok(Pump::Accepted { id: actual }) => assert_eq!(actual, id),
                            Err(Error::Unavailable) => break,
                            other => {
                                panic!("expected unknown outcome at {boundary:?}, got {other:?}")
                            }
                        }
                    }
                    assert!(client.abandon(id));
                    let channel = connector.open().await.unwrap();
                    assert_ne!(channel.peer(), old_peer);
                    client.replace_channel(channel);
                    assert!(
                        client.connect("alice", "losing").await.unwrap(),
                        "logical connection is retained"
                    );
                    let fresh = client.begin(Read::NAME, json!(null)).await.unwrap();
                    assert_eq!(fresh, id + 1, "ID allocator must survive physical recovery");
                    assert_eq!(client.pump().await.unwrap(), Pump::Accepted { id: fresh });
                    let value = if boundary == LossBoundary::BeforeAdmission {
                        0
                    } else {
                        5
                    };
                    assert_eq!(
                        client.pump().await.unwrap(),
                        Pump::Completed {
                            id: fresh,
                            outcome: Ok(json!([value, value]))
                        },
                        "fresh peer must receive only fresh results"
                    );
                    assert_eq!(client.outstanding(), 0);
                }) as Pin<Box<dyn Future<Output = ()>>>,
                Box::pin(async {
                    clock.sleep(1).await;
                    let rows = healthy.invoke(Read::NAME, json!(null)).await.unwrap();
                    let rows: [i64; 2] = serde_json::from_value(rows).unwrap();
                    assert_eq!(rows[0], rows[1]);
                    assert!([0, 5].contains(&rows[0]));
                }),
            ]))
            .unwrap();
        let full_trace = timeline.trace();
        let trace = &full_trace[trace_start..];
        let loss = trace.iter().position(|record| matches!(record.action, Action::NetworkLoss { boundary: actual, .. } if actual == boundary)).unwrap();
        assert_eq!(trace.iter().filter(|record| matches!(record.action, Action::CommandQueued { peer, kind: "invoke", id: Some(1) } if peer == old_peer)).count(), 1, "no mutation retry");
        assert!(!trace.iter().any(|record| matches!(record.action, Action::ResponseDelivered { peer, kind: "completed", .. } if peer == old_peer)));
        match boundary {
            LossBoundary::BeforeAdmission => assert!(!trace.iter().any(|record| matches!(record.action, Action::CommandSubmitted { peer, kind: "invoke", .. } if peer == old_peer))),
            LossBoundary::AfterAcceptance => {
                assert!(!trace[..loss].iter().any(|record| matches!(record.action, Action::StoreCommit { .. })), "loss must precede execution of the accepted mutation");
                assert!(trace[loss + 1..].iter().any(|record| matches!(record.action, Action::StoreCommit { rejected: false })), "accepted work must commit after loss");
                assert!(trace[..loss].iter().any(|record| matches!(record.action, Action::ResponsePublished { peer, kind: "accepted", .. } if peer == old_peer)));
            }
            LossBoundary::BeforeCompletionDelivery => {
                assert!(trace[..loss].iter().any(|record| matches!(record.action, Action::StoreCommit { rejected: false })));
                assert!(trace[..loss].iter().any(|record| matches!(record.action, Action::ResponsePublished { peer, kind: "completed", .. } if peer == old_peer)));
            }
        }
        assert!(trace[loss + 1..].iter().any(|record| matches!(record.action, Action::ResponseDelivered { kind: "completed", peer, .. } if peer == healthy_peer)), "the unaffected client must keep progressing during recovery");
        let healthy_started = trace.iter().position(|record| matches!(record.action, Action::CommandQueued { peer, kind: "invoke", .. } if peer == healthy_peer)).unwrap();
        let reconnect_started = trace.iter().position(|record| matches!(record.action, Action::CommandQueued { peer, kind: "connect", .. } if peer != healthy_peer && peer != old_peer)).unwrap();
        let healthy_finished = trace.iter().position(|record| matches!(record.action, Action::ResponseDelivered { peer, kind: "completed", .. } if peer == healthy_peer)).unwrap();
        assert!(
            healthy_started < reconnect_started && reconnect_started < healthy_finished,
            "reconnect must overlap another client's active call"
        );
        assert_eq!(trace.iter().filter(|record| matches!(record.action, Action::ResponseDelivered { peer, kind: "completed", .. } if peer == healthy_peer)).count(), 1, "old completion must not be redirected to another active peer with the same local ID");
        assert_eq!(
            trace
                .iter()
                .filter(|record| matches!(
                    record.action,
                    Action::CommandQueued { kind: "invoke", .. }
                ))
                .count(),
            3,
            "exactly one mutation, one healthy read and one fresh recovery read"
        );
    }
}

#[test]
fn recovery_model_keeps_lost_mutations_unknown_until_fresh_reads_constrain_them() {
    for boundary in BOUNDARIES {
        let world = World::generate(42);
        let mut campaign = campaign_setup::assemble_network(
            schedule(),
            &world,
            1,
            false,
            Some(policy(Some(boundary), 1)),
        )
        .unwrap();
        let change = Input {
            delay_ms: 0,
            action: InputAction::Change(Edit {
                expected: world.value,
                amount: 5,
                stop: Stop::Commit,
            }),
        };
        campaign
            .simulation
            .drive(campaign.actors[0].execute(change), None)
            .unwrap();
        campaign_setup::Actor::check_round(&mut campaign.actors);
        assert_eq!(campaign.actors[0].unknown_calls(), 1);
        assert_eq!(
            campaign.actors[0].possible_values(),
            [world.value, world.value + 5],
            "the oracle cannot learn the effect from host loss instrumentation"
        );
        campaign
            .simulation
            .drive(
                campaign.actors[0].execute(Input {
                    delay_ms: 0,
                    action: InputAction::Read,
                }),
                None,
            )
            .unwrap();
        campaign_setup::Actor::check_round(&mut campaign.actors);
        let expected = if boundary == LossBoundary::BeforeAdmission {
            world.value
        } else {
            world.value + 5
        };
        assert_eq!(campaign.actors[0].possible_values(), [expected]);
        assert_eq!(campaign.actors[0].expected_value(), expected);
    }
}

#[test]
fn unknown_outcome_oracle_does_not_adopt_an_unissued_mutation() {
    let world = World::generate(42);
    let mut campaign = campaign_setup::assemble_network(
        schedule(),
        &world,
        1,
        false,
        Some(policy(Some(LossBoundary::BeforeAdmission), 1)),
    )
    .unwrap();
    campaign
        .simulation
        .drive(
            campaign.actors[0].execute(Input {
                delay_ms: 0,
                action: InputAction::Change(Edit {
                    expected: world.value,
                    amount: 5,
                    stop: Stop::Commit,
                }),
            }),
            None,
        )
        .unwrap();
    campaign_setup::Actor::check_round(&mut campaign.actors);
    // Drift through a real operation on a separate, non-targeted request path.
    let mut outside = Client::new(campaign.simulation.open().unwrap());
    assert_eq!(
        campaign
            .simulation
            .drive(
                outside.request(
                    Some("alice"),
                    Change::NAME,
                    serde_json::to_value(Edit {
                        expected: world.value,
                        amount: 9,
                        stop: Stop::Commit
                    })
                    .unwrap()
                ),
                None
            )
            .unwrap(),
        Drive::Complete(Ok(json!([world.value + 9, world.value + 9])))
    );
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        campaign
            .simulation
            .drive(
                campaign.actors[0].execute(Input {
                    delay_ms: 0,
                    action: InputAction::Read,
                }),
                None,
            )
            .unwrap();
    }))
    .unwrap_err();
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(message.contains("input-derived possible states"));
    assert_eq!(
        campaign.actors[0].possible_values(),
        [world.value, world.value + 5]
    );
}

#[test]
fn seeded_loss_campaign_replays_under_operation_and_time_budgets() {
    for clients in [1, 2, 7] {
        for budget in [Budget::Operations(301), Budget::TimeMs(10_000)] {
            let run = || {
                let mut campaign = campaign_setup::assemble_network(
                    Schedule {
                        jitter_ms: 20,
                        trace_capacity: 0,
                        ..schedule()
                    },
                    &World::generate(42),
                    clients,
                    true,
                    Some(policy(None, 4)),
                )
                .unwrap();
                let report = runner::run_many(
                    &mut campaign.simulation,
                    &mut campaign.actors,
                    Config { seed: 42, budget },
                )
                .unwrap();
                let losses = campaign.simulation.network_losses();
                assert!(
                    losses.into_iter().all(|count| count > 0),
                    "exercise every loss boundary"
                );
                let unknown = campaign
                    .actors
                    .iter()
                    .map(|actor| actor.unknown_calls())
                    .sum::<u64>();
                assert!(unknown > 0);
                assert!(campaign.timeline.trace().is_empty());
                let last_losses = campaign.simulation.last_losses();
                assert!(
                    !last_losses.is_empty() && last_losses.len() <= clients,
                    "bounded fault diagnostics must survive trace eviction and old-peer release"
                );
                let streams: Vec<_> = campaign
                    .transcripts
                    .iter()
                    .map(|stream| stream.summary())
                    .collect();
                if let Budget::Operations(count) = budget {
                    assert_eq!(report.campaign.completed, count);
                    assert_eq!(
                        streams.iter().map(|stream| stream.sent).sum::<u64>(),
                        count + clients as u64 + 2 + unknown,
                        "only one extra Connect per lost action; no mutation resends"
                    );
                    assert_eq!(unknown, losses.iter().sum::<u64>());
                } else {
                    assert!(report.campaign.end_ms >= report.campaign.deadline_ms.unwrap());
                }
                (
                    report,
                    streams,
                    campaign.timeline.events_sha256(),
                    losses,
                    unknown,
                    campaign.actors[0].possible_values(),
                    last_losses,
                )
            };
            assert_eq!(run(), run(), "clients={clients} budget={budget:?}");
        }
    }
}

#[test]
fn pending_carrier_open_is_canceled_at_a_time_horizon() {
    for allocated in [false, true] {
        let (mut simulation, _, _) = setup::setup(schedule());
        let connector = simulation.connector(0);
        let now = simulation.now();
        let opening = connector.open();
        if allocated {
            simulation.advance_by(0).unwrap();
        }
        assert!(matches!(
            simulation.drive(opening, Some(now)).unwrap(),
            Drive::Deadline
        ));
        // Cancellation must release both queued and allocated/unclaimed opens.
        // All 128 production peer reservations must remain available.
        simulation.finish().unwrap();
        let channels: Vec<_> = (0..128).map(|_| simulation.open().unwrap()).collect();
        assert!(simulation.open().is_err());
        drop(channels);
        simulation.finish().unwrap();
        assert!(simulation.open().is_ok());
    }
}

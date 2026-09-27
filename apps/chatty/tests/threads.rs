use chatty::{Accepted, Job, Outcome, Progress, Send};
use serde_json::json;
use snap_oidc::relying_party as rp;
use snap_store::{Error, Store};

type Database = Store<snap_sqlite::Sqlite>;
fn fixture() -> Database {
    let mut migrations: Vec<snap_store::migration::Migration> = [
        snap_access::MIGRATION,
        snap_document::server::MIGRATION,
        rp::MIGRATION,
        chatty::MIGRATION,
    ]
    .into_iter()
    .map(|s| toml::from_str(s).unwrap())
    .collect();
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut store = snap_sqlite::Sqlite::memory(&migrations).unwrap();
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
        .chain(rp::TABLES.iter())
        .chain(chatty::TABLES.iter())
    {
        store.load(table).unwrap();
    }
    for person in ["alice", "bob"] {
        let state = format!("fixture-state-32-characters-long-{person}");
        store
            .run("login", |tx| {
                rp::start(
                    tx,
                    &state,
                    &rp::Attempt {
                        binding: rp::digest(person),
                        nonce: "nonce".into(),
                        verifier: "verifier".into(),
                        redirect: "https://chatty.test/auth/callback".into(),
                        issuer: "https://authy.test".into(),
                        old_session: None,
                        logout: false,
                        expires: 400,
                        processing: false,
                    },
                )?;
                rp::consume(tx, &state, person, false, 100)?;
                rp::issue(
                    tx,
                    &state,
                    &rp::Session {
                        id: rp::digest(person),
                        owner: owner(person),
                        subject: person.into(),
                        issuer: "https://authy.test".into(),
                        csrf: "csrf".into(),
                        nonce: "nonce".into(),
                        profile: json!({}),
                        tokens: rp::Tokens {
                            access: "access".into(),
                            refresh: "refresh".into(),
                            id_token: "id".into(),
                            access_expires: 700,
                            auth_time: Some(100),
                        },
                        expires: 1000,
                        refreshing: false,
                        version: 1,
                    },
                    100,
                )
            })
            .unwrap();
    }
    store
}
fn owner(person: &str) -> String {
    rp::owner("https://authy.test", person)
}
fn id(n: u32) -> String {
    format!("018f3c4b-6d2a-7000-8000-{n:012x}")
}
fn create(store: &mut Database, n: u32) {
    store
        .run("create", |tx| {
            chatty::create(
                tx,
                &rp::digest("alice"),
                &id(n),
                "New thread",
                "medium",
                100,
            )
        })
        .unwrap();
}
fn send(store: &mut Database, n: u32, request: &str) -> Result<Accepted, Error> {
    store
        .run("send", |tx| {
            chatty::send(
                tx,
                Send {
                    session: &rp::digest("alice"),
                    thread: &id(n),
                    turn: &format!("turn-{n}-{request}"),
                    request,
                    message: "Hello",
                    now: 101,
                },
            )
        })
        .map(|c| c.value)
}
fn view(store: &mut Database, n: u32) -> serde_json::Value {
    store
        .run("view", |tx| {
            chatty::document().read(tx, &id(n), Some(&owner("alice")))
        })
        .unwrap()
        .value
        .value
}
fn progress(
    store: &mut Database,
    job: &Job,
    p: &Progress,
    outcome: Outcome<'_>,
) -> Result<(), Error> {
    store
        .run("progress", |tx| chatty::progress(tx, job, p, outcome, 102))
        .map(|_| ())
}

#[test]
fn acceptance_is_atomic_deduplicated_and_isolated() {
    let mut store = fixture();
    create(&mut store, 1);
    let aborted = store.run("failed acceptance", |tx| {
        chatty::send(
            tx,
            Send {
                session: &rp::digest("alice"),
                thread: &id(1),
                turn: "discarded",
                request: "r",
                message: "Hello",
                now: 101,
            },
        )?;
        Err::<(), _>(Error::Constraint)
    });
    assert!(matches!(aborted, Err(Error::Constraint)));
    assert_eq!(view(&mut store, 1)["turns"], json!([]));
    let first = send(&mut store, 1, "r").unwrap();
    assert!(first.job.is_some());
    let repeated = send(&mut store, 1, "r").unwrap();
    assert!(repeated.job.is_none());
    assert_eq!(first.turn, repeated.turn);
    assert!(matches!(
        store.run("changed duplicate", |tx| chatty::send(
            tx,
            Send {
                session: &rp::digest("alice"),
                thread: &id(1),
                turn: "another",
                request: "r",
                message: "Changed",
                now: 101
            }
        )),
        Err(Error::Constraint)
    ));
    assert!(matches!(
        store.run("other owner", |tx| chatty::send(
            tx,
            Send {
                session: &rp::digest("bob"),
                thread: &id(1),
                turn: "stolen",
                request: "stolen",
                message: "Hello",
                now: 101
            }
        )),
        Err(Error::NotFound)
    ));
    assert!(
        store
            .run("other document reader", |tx| chatty::document().read(
                tx,
                &id(1),
                Some(&owner("bob"))
            ))
            .is_err()
    );
    assert_eq!(view(&mut store, 1)["turns"].as_array().unwrap().len(), 1);
}

#[test]
fn opaque_provider_context_is_private_and_retained_in_order() {
    let mut store = fixture();
    create(&mut store, 1);
    let job = send(&mut store, 1, "one").unwrap().job.unwrap();
    let output = vec![
        json!({"type":"reasoning","encrypted_content":"opaque-provider-secret"}),
        json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Answer"}]}),
    ];
    let p = Progress {
        text: "Answer".into(),
        summary: "Provider summary".into(),
        output: output.clone(),
        usage: json!({"output_tokens":4}),
        ..Default::default()
    };
    progress(&mut store, &job, &p, Outcome::Complete).unwrap();
    let public = view(&mut store, 1);
    assert_eq!(public["turns"][0]["text"], "Answer");
    assert!(!public.to_string().contains("opaque-provider-secret"));
    assert!(!public.to_string().contains("refresh"));
    let next = send(&mut store, 1, "two").unwrap().job.unwrap();
    assert_eq!(next.input[1]["encrypted_content"], "opaque-provider-secret");
    assert_eq!(next.input[1]["summary"], json!([]));
    assert_eq!(next.input[2], output[1]);
    assert_eq!(next.input[3]["role"], "user");
}

#[test]
fn cancel_delete_and_logout_fence_late_progress() {
    let mut store = fixture();
    create(&mut store, 1);
    let job = send(&mut store, 1, "one").unwrap().job.unwrap();
    let p = Progress {
        text: "Late answer".into(),
        ..Default::default()
    };
    store
        .run("cancel", |tx| {
            chatty::cancel(tx, &rp::digest("alice"), &id(1), &job.turn, 102)
        })
        .unwrap();
    assert!(progress(&mut store, &job, &p, Outcome::Complete).is_err());
    assert_eq!(view(&mut store, 1)["turns"][0]["status"], "cancelled");
    let job = send(&mut store, 1, "two").unwrap().job.unwrap();
    store
        .run("delete", |tx| {
            chatty::remove(tx, &rp::digest("alice"), &id(1), 102)
        })
        .unwrap();
    assert!(progress(&mut store, &job, &p, Outcome::Complete).is_err());
    create(&mut store, 2);
    let job = send(&mut store, 2, "one").unwrap().job.unwrap();
    store.run("logout", |tx| rp::revoke(tx, "alice")).unwrap();
    assert!(progress(&mut store, &job, &p, Outcome::Complete).is_err());
    store
        .run("abandon", |tx| chatty::abandon(tx, &job, 102))
        .unwrap();
    let value = view(&mut store, 2);
    assert_eq!(value["turns"][0]["status"], "interrupted");
    assert_eq!(value["turns"][0]["text"], "");
}

#[test]
fn startup_records_interruption_without_replaying_acceptance() {
    let mut store = fixture();
    create(&mut store, 1);
    let job = send(&mut store, 1, "one").unwrap().job.unwrap();
    store.run("startup", |tx| chatty::recover(tx, 102)).unwrap();
    assert_eq!(view(&mut store, 1)["turns"][0]["status"], "interrupted");
    assert!(send(&mut store, 1, "one").unwrap().job.is_none());
    assert!(progress(&mut store, &job, &Progress::default(), Outcome::Complete).is_err());
    let next = send(&mut store, 1, "two").unwrap().job.unwrap();
    assert_eq!(next.omitted, 1);
    assert_eq!(next.input.len(), 1);
}

#[test]
fn four_active_turns_bound_acceptance_and_cancellation_releases_capacity() {
    let mut store = fixture();
    for n in 1..=5 {
        create(&mut store, n);
    }
    let mut jobs = Vec::new();
    for n in 1..=4 {
        jobs.push(send(&mut store, n, "one").unwrap().job.unwrap());
    }
    assert!(matches!(send(&mut store, 5, "one"), Err(Error::Constraint)));
    assert!(send(&mut store, 1, "one").unwrap().job.is_none());
    store
        .run("cancel", |tx| {
            chatty::cancel(tx, &rp::digest("alice"), &id(1), &jobs[0].turn, 102)
        })
        .unwrap();
    assert!(send(&mut store, 5, "one").unwrap().job.is_some());
}

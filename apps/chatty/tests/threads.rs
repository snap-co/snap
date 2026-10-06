//! Client-level shared collections, authorization, durable requests and restart.
use chatty::client::Client;
use serde_json::{Value, json};
use snap_store::{Error, Program};
use snap_transport::{
    Command, Event, Response,
    host::{Blocking, Controllers},
    replication::Replications,
};
use std::sync::Arc;
const ID: &str = "018f3c4b-6d2a-7000-8000-000000000001";
type Host =
    Blocking<snap_store_sqlite::Sqlite, Controllers<snap_store_sqlite::Sqlite, Replications>>;
fn migrations() -> Vec<snap_store::migration::Migration> {
    let mut migrations: Vec<_> = [
        snap_access::MIGRATION,
        snap_store::resource::MIGRATION,
        chatty::MIGRATION,
    ]
    .iter()
    .map(|m| toml::from_str(m).unwrap())
    .collect();
    migrations.sort_by(|a: &snap_store::migration::Migration, b| a.id.cmp(&b.id));
    migrations
}
fn host(store: snap_store::Store<snap_store_sqlite::Sqlite>) -> Host {
    let replication = chatty::replication();
    let mut operations =
        snap_transport::operation::Registry::default().with_request(replication.operation());
    for definition in chatty::operations::declarations() {
        operations = operations.with_request(definition);
    }
    let mut host = Host::new(
        store,
        Controllers::around(Replications::new(replication)),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |_, token| match token {
                "alice-token" => Ok("alice".into()),
                "agent-token" => Ok("agent".into()),
                _ => Err(Error::NotFound),
            },
        ))),
        snap_transport::server::Config::default(),
        "chatty-test".into(),
    )
    .with_inputs(|key| {
        if key == "clock" {
            Ok(json!(1000))
        } else {
            Err(snap_transport::Error::Unavailable)
        }
    });
    host.recover().unwrap();
    host
}
fn pump(host: &mut Host, peer: u64, client: &mut Client) -> Vec<Response> {
    let mut seen = vec![];
    for _ in 0..20 {
        while host.step() {}
        let frames = host.drain(peer).unwrap();
        if frames.is_empty() {
            return seen;
        }
        for frame in frames {
            let update = client.receive(frame.clone()).unwrap();
            seen.push(frame);
            for command in update.send {
                host.submit(peer, command, 0).unwrap();
            }
        }
    }
    panic!("exchange did not settle")
}
fn attach(host: &mut Host, token: &str, id: &str, client: &mut Client) -> u64 {
    let peer = host.open().unwrap();
    host.submit(peer, client.connect(token.into(), id.into()), 0)
        .unwrap();
    pump(host, peer, client);
    assert!(client.ready());
    peer
}
fn invoke(
    host: &mut Host,
    peer: u64,
    client: &mut Client,
    name: &str,
    input: Value,
) -> snap_transport::Outcome {
    let command = client.invoke(name, input).unwrap();
    let Command::Invoke(call) = &command else {
        unreachable!()
    };
    let id = call.id;
    host.submit(peer, command, 0).unwrap();
    pump(host, peer, client)
        .into_iter()
        .find_map(|response| match response {
            Response::Event(Event::Completed { id: found, outcome }) if id == found => {
                Some(outcome)
            }
            Response::Failed(error) => Some(Err(error)),
            _ => None,
        })
        .expect("missing outcome")
}
#[test]
fn shared_collections_discover_future_rows_and_redact_revoked_backlogs() {
    let mut host = host(snap_store_sqlite::Sqlite::memory(&migrations()).unwrap());
    let mut human = Client::new().unwrap();
    let mut agent = Client::new().unwrap();
    let hp = attach(&mut host, "alice-token", "human", &mut human);
    let ap = attach(&mut host, "agent-token", "agent", &mut agent);
    invoke(
        &mut host,
        hp,
        &mut human,
        "chatty.create",
        json!({"id":ID,"title":"Shared work"}),
    )
    .unwrap();
    pump(&mut host, ap, &mut agent);
    assert!(agent.threads().unwrap().is_empty());
    assert!(
        invoke(
            &mut host,
            ap,
            &mut agent,
            "chatty.send",
            json!({"thread_id":ID,"request_id":"denied","message":"secret"})
        )
        .is_err()
    );
    invoke(
        &mut host,
        hp,
        &mut human,
        "chatty.member",
        json!({"thread_id":ID,"identity":"agent","role":"editor"}),
    )
    .unwrap();
    pump(&mut host, ap, &mut agent);
    assert_eq!(agent.threads().unwrap()[0].title, "Shared work");
    for (peer, client) in [(hp, &mut human), (ap, &mut agent)] {
        let command = client.select(Some(ID.into())).unwrap().unwrap();
        host.submit(peer, command, 0).unwrap();
        pump(&mut host, peer, client);
    }
    let message = json!({"thread_id":ID,"request_id":"first","message":"Hello from human"});
    let saved = invoke(&mut host, hp, &mut human, "chatty.send", message.clone()).unwrap();
    assert_eq!(saved["sequence"], 1);
    assert_eq!(saved["sender"], "alice");
    pump(&mut host, ap, &mut agent);
    assert_eq!(agent.messages().unwrap()[0].body, "Hello from human");
    assert_eq!(
        invoke(&mut host, hp, &mut human, "chatty.send", message).unwrap(),
        saved
    );
    assert!(
        invoke(
            &mut host,
            hp,
            &mut human,
            "chatty.send",
            json!({"thread_id":ID,"request_id":"first","message":"changed"})
        )
        .is_err()
    );
    let reply = invoke(
        &mut host,
        ap,
        &mut agent,
        "chatty.send",
        json!({"thread_id":ID,"request_id":"reply","message":"Hello from agent"}),
    )
    .unwrap();
    assert_eq!(reply["sequence"], 2);
    assert_eq!(reply["sender"], "agent");
    pump(&mut host, hp, &mut human);
    assert_eq!(human.messages().unwrap().len(), 2);
    assert!(
        invoke(
            &mut host,
            ap,
            &mut agent,
            "chatty.rename",
            json!({"thread_id":ID,"title":"Hijacked"})
        )
        .is_err()
    );
    // Queue a new payload without draining the agent's independent carrier queue.
    invoke(
        &mut host,
        hp,
        &mut human,
        "chatty.send",
        json!({"thread_id":ID,"request_id":"queued","message":"queued confidential"}),
    )
    .unwrap();
    invoke(
        &mut host,
        hp,
        &mut human,
        "chatty.member",
        json!({"thread_id":ID,"identity":"agent","role":null}),
    )
    .unwrap();
    let output = host.output(ap).unwrap();
    while let Some(response) = output.pop_front() {
        if let Response::Global { input, .. } = &response {
            let publication: snap_store::replica::Publication =
                serde_json::from_value(input.clone()).unwrap();
            assert!(publication.reset);
            assert!(
                Program::from_bytes(&chatty::catalog(), &publication.program)
                    .unwrap()
                    .is_empty()
            );
        }
        agent.receive(response).unwrap();
    }
    assert!(agent.threads().unwrap().is_empty());
    assert!(agent.messages().unwrap().is_empty());
    assert!(
        invoke(
            &mut host,
            ap,
            &mut agent,
            "chatty.send",
            json!({"thread_id":ID,"request_id":"after","message":"revoked"})
        )
        .is_err()
    );
    invoke(
        &mut host,
        hp,
        &mut human,
        "chatty.delete",
        json!({"thread_id":ID}),
    )
    .unwrap();
    assert!(human.threads().unwrap().is_empty());
    assert!(human.messages().unwrap().is_empty());
}
#[test]
fn restart_recovers_history_and_message_idempotency_without_handlers_or_documents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chatty.sqlite");
    snap_store_sqlite::migrate(&path, &migrations()).unwrap();
    let mut running = host(snap_store_sqlite::Sqlite::open(&path).unwrap());
    let mut client = Client::new().unwrap();
    let peer = attach(&mut running, "alice-token", "one", &mut client);
    invoke(
        &mut running,
        peer,
        &mut client,
        "chatty.create",
        json!({"id":ID,"title":"History"}),
    )
    .unwrap();
    let message = json!({"thread_id":ID,"request_id":"stable","message":"Durable"});
    invoke(
        &mut running,
        peer,
        &mut client,
        "chatty.send",
        message.clone(),
    )
    .unwrap();
    drop(running);
    let mut restarted = host(snap_store_sqlite::Sqlite::open(&path).unwrap());
    let mut client = Client::new().unwrap();
    client.select(Some(ID.into())).unwrap();
    let peer = attach(&mut restarted, "alice-token", "two", &mut client);
    assert_eq!(client.messages().unwrap()[0].body, "Durable");
    let result = invoke(&mut restarted, peer, &mut client, "chatty.send", message).unwrap();
    assert_eq!(result["sequence"], 1);
    assert_eq!(client.messages().unwrap().len(), 1);
}

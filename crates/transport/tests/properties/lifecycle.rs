//! A lease/ownership model driven through Server's public interface. Retain old
//! handles deliberately: a successful reconnect must revoke all earlier owners.
use hegel::{TestCase, generators as gs};
use snap_transport::{Error, Invocation, Value, json, server::*};
use std::{cell::Cell, collections::BTreeSet, rc::Rc};

#[derive(Clone)]
struct Credentials {
    rotated: Rc<Cell<[bool; 2]>>,
    lookups: Rc<Cell<usize>>,
}

impl Authority for Credentials {
    fn identify(&self, bearer: &str) -> Result<String, Error> {
        self.lookups.set(self.lookups.get() + 1);
        match bearer {
            "alice-old" if !self.rotated.get()[0] => Some("alice".into()),
            "alice-new" if self.rotated.get()[0] => Some("alice".into()),
            "bob-old" if !self.rotated.get()[1] => Some("bob".into()),
            "bob-new" if self.rotated.get()[1] => Some("bob".into()),
            "empty-identity" => Some(String::new()),
            _ => None,
        }
        .ok_or(Error::InvalidBearer)
    }
}

struct Lease {
    identity: usize,
    client: String,
    connection: ConnectionId,
    owner: Option<usize>,
    deadline: u128,
    last_invocation: u64,
}

struct Model {
    server: Server<Credentials>,
    credentials: Credentials,
    capacity: usize,
    retention: u64,
    now: u64,
    leases: Vec<Lease>,
    handles: Vec<Attachment>,
    retired: BTreeSet<u64>,
    retired_once: BTreeSet<u64>,
    allocated: BTreeSet<u64>,
}

impl Model {
    fn new(capacity: usize, retention: u64, now: u64) -> Self {
        let credentials = Credentials {
            rotated: Rc::new(Cell::new([false; 2])),
            lookups: Rc::default(),
        };
        Self {
            server: Server::new(
                credentials.clone(),
                Config {
                    capacity,
                    reconnect_ms: retention,
                },
            ),
            credentials,
            capacity,
            retention,
            now,
            leases: Vec::new(),
            handles: Vec::new(),
            retired: BTreeSet::new(),
            retired_once: BTreeSet::new(),
            allocated: BTreeSet::new(),
        }
    }

    fn expire(&mut self, now: u64) {
        self.now = self.now.max(now);
        self.leases.retain(|lease| {
            if lease.owner.is_none() && lease.deadline <= u128::from(self.now) {
                assert!(self.retired_once.insert(lease.connection.0));
                self.retired.insert(lease.connection.0);
                false
            } else {
                true
            }
        });
    }

    fn token(&self, tc: &TestCase) -> (String, Option<usize>) {
        let choice = tc.draw(gs::integers::<usize>().max_value(5));
        if choice >= 4 {
            return (["invalid", "empty-identity"][choice - 4].into(), None);
        }
        let identity = choice % 2;
        let current = self.credentials.rotated.get()[identity];
        let use_new = if choice < 2 { current } else { !current };
        let token = format!(
            "{}-{}",
            ["alice", "bob"][identity],
            if use_new { "new" } else { "old" }
        );
        (token, (choice < 2).then_some(identity))
    }

    fn handle(&self, tc: &TestCase) -> Option<usize> {
        if self.handles.is_empty() {
            None
        } else {
            Some(tc.draw(gs::integers::<usize>().max_value(self.handles.len() - 1)))
        }
    }

    fn connect_as(&mut self, token: &str, identity: Option<usize>, client: &str) {
        self.expire(self.now);
        let existing = identity.and_then(|identity| {
            self.leases
                .iter()
                .position(|l| l.identity == identity && l.client == client)
        });
        let error = if identity.is_none() {
            Some(Error::InvalidBearer)
        } else if client.is_empty() || client.len() > 128 {
            Some(Error::InvalidInput)
        } else if existing.is_some_and(|i| self.leases[i].owner.is_some()) {
            Some(Error::Occupied)
        } else if existing.is_none() && self.leases.len() == self.capacity {
            Some(Error::Capacity)
        } else {
            None
        };
        let lookups = self.credentials.lookups.get();
        let result = self.server.connect(token, client, self.now);
        assert_eq!(self.credentials.lookups.get(), lookups + 1);
        if let Some(error) = error {
            assert_eq!(result.err(), Some(error));
            return;
        }
        let (attachment, resumed) = result.unwrap();
        assert_eq!(resumed, existing.is_some());
        if let Some(i) = existing {
            assert_eq!(attachment.connection(), self.leases[i].connection);
            self.leases[i].owner = Some(self.handles.len());
            self.leases[i].last_invocation = 0;
        } else {
            assert!(
                self.allocated.insert(attachment.connection().0),
                "fresh logical connection reused an old ID"
            );
            self.leases.push(Lease {
                identity: identity.unwrap(),
                client: client.into(),
                connection: attachment.connection(),
                owner: Some(self.handles.len()),
                deadline: 0,
                last_invocation: 0,
            });
        }
        self.handles.push(attachment);
    }

    fn invoke_as(&mut self, index: usize, id: u64) {
        let owner = self.leases.iter().position(|l| l.owner == Some(index));
        let expected = match owner {
            None => Err(Error::StaleConnection),
            Some(i) if id <= self.leases[i].last_invocation => Err(Error::Protocol),
            Some(i) => {
                self.leases[i].last_invocation = id;
                Ok((self.leases[i].identity, self.leases[i].connection))
            }
        };
        let lookups = self.credentials.lookups.get();
        let call = Invocation {
            id,
            operation: "opaque.op".into(),
            input: json!({"id": id}),
        };
        let actual = self.server.invoke(&self.handles[index], call);
        assert_eq!(
            self.credentials.lookups.get(),
            lookups,
            "attached identity is not re-resolved"
        );
        match expected {
            Err(error) => assert_eq!(actual.err(), Some(error)),
            Ok((identity, connection)) => {
                let dispatch = actual.unwrap();
                assert_eq!(
                    dispatch.identity.as_deref(),
                    Some(["alice", "bob"][identity])
                );
                assert_eq!(dispatch.connection, Some(connection));
                assert_eq!(dispatch.invocation.id, id);
                assert_eq!(dispatch.invocation.operation, "opaque.op");
                assert_eq!(dispatch.invocation.input, json!({"id": id}));
            }
        }
    }

    fn disconnect_as(&mut self, index: usize) {
        self.expire(self.now);
        let owner = self.leases.iter().position(|l| l.owner == Some(index));
        let expected = owner.map_or(Err(Error::StaleConnection), |i| {
            self.leases[i].owner = None;
            self.leases[i].deadline =
                (u128::from(self.now) + u128::from(self.retention)).min(u128::from(u64::MAX));
            Ok(())
        });
        assert_eq!(
            self.server.disconnect(&self.handles[index], self.now),
            expected
        );
        self.expire(self.now);
    }
}

#[hegel::state_machine]
impl Model {
    #[rule]
    fn connect(&mut self, tc: TestCase) {
        let (token, identity) = self.token(&tc);
        let clients = [
            "tab-a".into(),
            "tab-b".into(),
            String::new(),
            "é".repeat(64),
            "é".repeat(65),
        ];
        let client = &clients[tc.draw(gs::integers::<usize>().max_value(clients.len() - 1))];
        tc.note(&format!("connect {token:?} {client:?}"));
        let handles = self.handles.len();
        let allocated = self.allocated.len();
        self.connect_as(&token, identity, client);
        tc.event(if self.handles.len() == handles {
            "connect rejected"
        } else if self.allocated.len() == allocated {
            "resumed retained connection"
        } else {
            "fresh connection"
        });
    }

    #[rule]
    fn invoke(&mut self, tc: TestCase) {
        let Some(index) = self.handle(&tc) else {
            return;
        };
        let last = self
            .leases
            .iter()
            .find(|l| l.owner == Some(index))
            .map_or(0, |l| l.last_invocation);
        let choices = [0, 1, last, last.saturating_add(1), u64::MAX];
        let id = choices[tc.draw(gs::integers::<usize>().max_value(choices.len() - 1))];
        tc.note(&format!("invoke handle={index} id={id}"));
        tc.event(
            if self.leases.iter().any(|lease| lease.owner == Some(index)) {
                "invoke current attachment"
            } else {
                "invoke stale attachment"
            },
        );
        self.invoke_as(index, id);
    }

    #[rule]
    fn disconnect(&mut self, tc: TestCase) {
        if let Some(index) = self.handle(&tc) {
            tc.note(&format!("disconnect handle={index}"));
            self.disconnect_as(index);
        }
    }

    #[rule]
    fn close(&mut self, tc: TestCase) {
        let Some(index) = self.handle(&tc) else {
            return;
        };
        let owner = self.leases.iter().position(|l| l.owner == Some(index));
        let expected = owner.map_or(Err(Error::StaleConnection), |i| {
            let lease = self.leases.remove(i);
            assert!(self.retired_once.insert(lease.connection.0));
            self.retired.insert(lease.connection.0);
            Ok(())
        });
        tc.note(&format!("close handle={index}"));
        assert_eq!(self.server.close(&self.handles[index]), expected);
    }

    #[rule]
    fn time(&mut self, tc: TestCase) {
        let delta = tc.draw(gs::integers::<u64>().max_value(101));
        let now = if tc.draw(gs::booleans()) {
            self.now.saturating_add(delta)
        } else {
            self.now.saturating_sub(delta)
        };
        tc.note(&format!("tick {now}"));
        self.expire(now);
        self.server.tick(now);
    }

    #[rule]
    fn rotate(&mut self, tc: TestCase) {
        tc.event("rotate credentials");
        let identity = tc.draw(gs::integers::<usize>().max_value(1));
        let mut tokens = self.credentials.rotated.get();
        tokens[identity] = !tokens[identity];
        self.credentials.rotated.set(tokens);
    }

    #[rule]
    fn request(&mut self, tc: TestCase) {
        let (token, identity) = self.token(&tc);
        let anonymous = tc.draw(gs::booleans());
        let before = self.credentials.lookups.get();
        let result = self.server.request(
            (!anonymous).then_some(token.as_str()),
            Invocation {
                id: 0,
                operation: "request".into(),
                input: Value::Null,
            },
        );
        assert_eq!(
            self.credentials.lookups.get(),
            before + usize::from(!anonymous)
        );
        if anonymous || identity.is_some() {
            let dispatch = result.unwrap();
            assert_eq!(dispatch.connection, None);
            assert_eq!(
                dispatch.identity,
                if anonymous {
                    None
                } else {
                    identity.map(|i| ["alice", "bob"][i].into())
                }
            );
        } else {
            assert_eq!(result.err(), Some(Error::InvalidBearer));
        }
    }

    #[invariant(always_run)]
    fn residents_and_retirement(&mut self, tc: TestCase) {
        if !self.retired.is_empty() {
            tc.event("retired connection");
        }
        assert_eq!(self.server.resident_count(), self.leases.len());
        assert!(self.leases.len() <= self.capacity);
        let retired = self.server.take_retired();
        assert_eq!(
            retired.len(),
            self.retired.len(),
            "retirement must occur exactly once"
        );
        assert_eq!(
            retired.into_iter().map(|id| id.0).collect::<BTreeSet<_>>(),
            std::mem::take(&mut self.retired)
        );
        assert!(self.server.take_retired().is_empty());
    }
}

#[hegel::test]
fn connection_histories_match_lease_model(tc: TestCase) {
    let capacity = tc.draw(gs::integers::<usize>().max_value(5));
    let retention = tc.draw(gs::integers::<u64>().max_value(100));
    let now = if tc.draw(gs::booleans()) {
        0
    } else {
        u64::MAX - 200
    };
    hegel::stateful::machine(Model::new(capacity, retention, now))
        .steps(150)
        .run(tc);
}

#[hegel::test]
fn reconnect_fences_every_old_attachment(tc: TestCase) {
    let mut model = Model::new(1, 100, 0);
    let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    model.connect_as("alice-old", Some(0), "tab");
    for owner in 0..rounds {
        model.invoke_as(owner, u64::MAX);
        model.disconnect_as(owner);
        model.connect_as("alice-old", Some(0), "tab");
        for old in 0..=owner {
            model.invoke_as(old, 1);
            assert_eq!(
                model.server.close(&model.handles[old]),
                Err(Error::StaleConnection)
            );
            assert_eq!(
                model.server.disconnect(&model.handles[old], 0),
                Err(Error::StaleConnection)
            );
        }
        model.invoke_as(owner + 1, 1);
    }
}

#[hegel::test]
fn expiry_boundary_is_exact_and_attached_connections_do_not_expire(tc: TestCase) {
    let retention = tc.draw(gs::integers::<u64>().min_value(1).max_value(10000));
    let start = tc.draw(gs::integers::<u64>().max_value(u64::MAX - retention));
    for offset in [retention - 1, retention, retention + 1] {
        let mut model = Model::new(2, retention, start);
        model.connect_as("alice-old", Some(0), "tab");
        model.connect_as("bob-old", Some(1), "tab");
        model.disconnect_as(0);
        let now = start.saturating_add(offset);
        model.expire(now);
        model.server.tick(now);
        model.residents_and_retirement(tc.clone());
        model.connect_as("alice-old", Some(0), "tab");
        model.invoke_as(1, 1);
        assert_eq!(
            model.handles[0].connection() == model.handles[2].connection(),
            offset < retention
        );
    }
}

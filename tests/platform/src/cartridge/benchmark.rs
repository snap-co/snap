//! Benchmark application policy. Independent paired counters or one shared pair,
//! guarded positive writes alternating with reads. Hosts never predict results.
use crate::{
    benchmark::{self, Outcome, Workload},
    runner::{Fingerprint, Random},
};
use alloc::{format, string::String, vec, vec::Vec};
use serde::{Deserialize, Serialize};
use snap_store::{
    Column, Data, Error as StoreError, Kind, Row, Table, Transaction,
    migration::{Change as Ddl, Migration},
};
use snap_transport::{
    Channel, Error, Operation, Value,
    client::{Client, Operations},
    json,
    operation::{Definition, Guard},
};

pub const VERSION: u32 = 1;
pub const TABLES: [&str; 2] = ["bench.left", "bench.right"];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Independent,
    Shared,
}

pub fn migration() -> Migration {
    Migration {
        id: "9999_benchmark".into(),
        changes: TABLES
            .into_iter()
            .map(|name| Ddl::CreateTable {
                table: Table {
                    name: name.into(),
                    columns: vec![
                        Column {
                            name: "id".into(),
                            kind: Kind::Integer,
                        },
                        Column {
                            name: "value".into(),
                            kind: Kind::Integer,
                        },
                    ],
                    primary: vec!["id".into()],
                    indexes: vec![],
                    foreign: vec![],
                },
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    pub key: i64,
    pub expected: i64,
    pub amount: i64,
}
pub struct Change;
impl Operation for Change {
    const NAME: &'static str = "bench.change";
    type Input = Edit;
    type Output = [i64; 2];
    type Error = String;
    type Progress = Value;
}
pub struct Read;
impl Operation for Read {
    const NAME: &'static str = "bench.read";
    type Input = i64;
    type Output = [i64; 2];
    type Error = String;
    type Progress = Value;
}
fn read(tx: &mut Transaction<'_>, key: i64) -> Result<[i64; 2], StoreError> {
    let mut result = [0; 2];
    for (index, table) in TABLES.into_iter().enumerate() {
        let row = tx.get(table, &[key.into()])?.ok_or(StoreError::NotFound)?;
        let Some(snap_store::Value::Integer(value)) = row.get("value") else {
            return Err(StoreError::Invalid);
        };
        result[index] = *value;
    }
    Ok(result)
}
pub fn definitions() -> Vec<Definition> {
    vec![
        Definition::typed::<Change>(
            true,
            vec![Guard::new(|tx, invocation, _| {
                let edit: Edit = serde_json::from_value(invocation.input.clone())
                    .map_err(|_| Error::InvalidInput)?;
                if edit.amount <= 0 {
                    return Err(Error::InvalidInput.into());
                }
                if read(tx, edit.key)?[0] != edit.expected {
                    return Err(Error::Application(json!("stale")).into());
                }
                Ok(())
            })],
            Data::new(&TABLES),
            &[],
            |tx, edit, _| {
                for (table, before) in TABLES.into_iter().zip(read(tx, edit.key)?) {
                    let after = before.checked_add(edit.amount).ok_or(StoreError::Invalid)?;
                    tx.update(
                        table,
                        &[edit.key.into()],
                        Row::from([("value".into(), after.into())]),
                    )?;
                }
                Ok(read(tx, edit.key)?)
            },
        ),
        Definition::typed::<Read>(true, vec![], Data::new(&TABLES), &[], |tx, key, _| {
            Ok(read(tx, key)?)
        }),
    ]
}

#[derive(Clone, Debug, Serialize)]
enum Action {
    Read,
    Change(i64),
}
pub struct Plan {
    pub profile: Profile,
    pub keys: Vec<i64>,
    actions: Vec<Vec<Action>>,
    pub sha256: [u8; 32],
}
impl Plan {
    /// Only action kinds/amounts and keys are fixed. CAS expectations follow the
    /// actor's last observation; conflicts and completion order depend on host.
    pub fn new(seed: u64, profile: Profile, clients: usize, warmup: u64, operations: u64) -> Self {
        let mut fingerprint = Fingerprint::new(b"snap-benchmark-inputs-v1");
        fingerprint.record(&(VERSION, seed, profile, clients, warmup, operations));
        let keys = (0..clients)
            .map(|actor| {
                if profile == Profile::Shared {
                    1
                } else {
                    actor as i64 + 1
                }
            })
            .collect::<Vec<_>>();
        let actions = (0..clients)
            .map(|actor| {
                let mut random = Random::stream(seed, &format!("benchmark.actor.{actor}"));
                let mut actions = Vec::new();
                for (phase, count) in [warmup, operations].into_iter().enumerate() {
                    for local in 0..benchmark::actor_operations(count, clients, actor) {
                        let action = if actions.len() % 2 == 0 {
                            Action::Read
                        } else {
                            Action::Change(1 + random.below(7) as i64)
                        };
                        fingerprint.record(&(phase, actor, local, keys[actor], &action));
                        actions.push(action);
                    }
                }
                actions
            })
            .collect();
        Self {
            profile,
            keys,
            actions,
            sha256: fingerprint.finish(),
        }
    }
    pub fn actors<C: Channel>(&self, clients: Vec<Client<C>>) -> Vec<Actor<C>> {
        assert_eq!(clients.len(), self.actions.len());
        clients
            .into_iter()
            .enumerate()
            .map(|(index, client)| Actor {
                client,
                key: self.keys[index],
                profile: self.profile,
                actions: self.actions[index].clone(),
                cursor: 0,
                observed: 0,
                committed_amount: 0,
            })
            .collect()
    }
}

pub struct Actor<C> {
    client: Client<C>,
    key: i64,
    profile: Profile,
    actions: Vec<Action>,
    cursor: usize,
    observed: i64,
    committed_amount: i64,
}
impl<C: Channel> Workload for Actor<C> {
    async fn execute(&mut self) -> Result<Outcome, String> {
        let action = self
            .actions
            .get(self.cursor)
            .ok_or("benchmark input plan exhausted")?
            .clone();
        self.cursor += 1;
        match action {
            Action::Read => {
                let rows = Operations::call::<Read>(&mut self.client, &self.key)
                    .await
                    .map_err(|e| format!("read: {e:?}"))?;
                if rows[0] != rows[1]
                    || rows[0] < self.observed
                    || (self.profile == Profile::Independent && rows[0] != self.committed_amount)
                {
                    return Err(format!("invalid paired read {rows:?} for key {}", self.key));
                }
                self.observed = rows[0];
                Ok(Outcome::Read)
            }
            Action::Change(amount) => {
                let expected = self.observed;
                match Operations::call::<Change>(
                    &mut self.client,
                    &Edit {
                        key: self.key,
                        expected,
                        amount,
                    },
                )
                .await
                {
                    Ok(rows) if rows == [expected + amount; 2] => {
                        self.observed = expected + amount;
                        self.committed_amount += amount;
                        Ok(Outcome::Commit)
                    }
                    Err(Error::Application(value))
                        if value == json!("stale") && self.profile == Profile::Shared =>
                    {
                        Ok(Outcome::Conflict)
                    }
                    other => Err(format!(
                        "invalid change outcome for key {}: {other:?}",
                        self.key
                    )),
                }
            }
        }
    }
}

/// Quiescent verification via every SDK, outside timing. Expected totals derive
/// from issued amounts with confirmed commits, not the returned final rows.
pub async fn verify<C: Channel>(actors: &mut [Actor<C>]) -> Result<(), String> {
    let total: i64 = actors.iter().map(|actor| actor.committed_amount).sum();
    for actor in actors {
        let expected = if actor.profile == Profile::Shared {
            total
        } else {
            actor.committed_amount
        };
        let actual = Operations::call::<Read>(&mut actor.client, &actor.key)
            .await
            .map_err(|e| format!("verify: {e:?}"))?;
        if actual != [expected; 2] {
            return Err(format!(
                "final key {}: {actual:?}, expected {expected}",
                actor.key
            ));
        }
    }
    Ok(())
}

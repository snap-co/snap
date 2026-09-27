#[path = "../support/mod.rs"]
mod support;
use hegel::{TestCase, generators as gs};
use snap_identity::Identity;
use snap_store::Error;

#[hegel::test]
fn session_histories_match_authority_model(tc: TestCase) {
    let actions = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(80));
    let identity = Identity::new(10).unwrap();
    let mut store = support::store(true);
    let mut crypto = support::Fake::default();
    let mut enrolled = [false; 3];
    let mut sessions: Vec<(String, usize, i64, bool)> = vec![];
    let mut owners = [String::new(), String::new(), String::new()];
    let mut now = 0;
    for action in actions {
        let owner = (action as usize / 7) % 3;
        let email = format!("user{owner}@example.com");
        match action % 7 {
            0 => {
                let result = store.run("enroll", |tx| {
                    identity.enroll(tx, &mut crypto, &email, "password1", now)
                });
                if enrolled[owner] {
                    assert!(matches!(result, Err(Error::Constraint)));
                } else {
                    let issued = result.unwrap().value;
                    owners[owner] = issued.session.identity;
                    sessions.push((issued.bearer, owner, now + 10, false));
                    enrolled[owner] = true;
                }
            }
            1 | 2 => {
                let correct = action % 7 == 1;
                let result = store.run("login", |tx| {
                    identity.login(
                        tx,
                        &mut crypto,
                        &email,
                        if correct { "password1" } else { "wrongpass" },
                        now,
                    )
                });
                if enrolled[owner] && correct {
                    let issued = result.unwrap().value;
                    assert_eq!(issued.session.identity, owners[owner]);
                    sessions.push((issued.bearer, owner, now + 10, false));
                } else {
                    assert!(matches!(result, Err(Error::NotFound)));
                }
            }
            3 => now += i64::from(action / 7 % 11),
            4 if !sessions.is_empty() => {
                let index = action as usize % sessions.len();
                let (bearer, _, expires, revoked) = &mut sessions[index];
                let result = store.run("revoke", |tx| identity.revoke(tx, &crypto, bearer, now));
                if *revoked || now >= *expires {
                    assert!(matches!(result, Err(Error::NotFound)));
                } else {
                    result.unwrap();
                    *revoked = true;
                }
            }
            5 => {
                let result = store.run("abort-login", |tx| {
                    identity.login(tx, &mut crypto, &email, "password1", now)?;
                    Err::<(), _>(Error::Unavailable)
                });
                assert!(result.is_err());
            }
            _ => {}
        }
        for (bearer, owner, expires, revoked) in &sessions {
            let result = store.run("resolve", |tx| identity.resolve(tx, &crypto, bearer, now));
            if *revoked || now >= *expires {
                assert!(matches!(result, Err(Error::NotFound)));
            } else {
                assert_eq!(result.unwrap().value.identity, owners[*owner]);
            }
        }
        let count = store
            .run("count", |tx| {
                tx.find(snap_identity::TABLES[2], "primary", &[])
            })
            .unwrap()
            .value
            .len();
        assert_eq!(count, sessions.iter().filter(|session| !session.3).count());
    }
}

#[hegel::test]
fn failed_issuance_never_returns_a_credential_or_partial_authority(tc: TestCase) {
    use snap_store::{Backend, Catalog, CommitError, Row, Store, Table, Write};
    use std::sync::{Arc, Mutex};
    struct Disk {
        writes: Arc<Mutex<Vec<Write>>>,
        fault: u8,
    }
    impl Backend for Disk {
        fn load(&mut self, table: &Table) -> Result<Vec<Row>, Error> {
            Ok(self
                .writes
                .lock()
                .unwrap()
                .iter()
                .filter_map(|write| match write {
                    Write::Insert { table: name, row } if *name == table.name => Some(row.clone()),
                    _ => None,
                })
                .collect())
        }
        fn commit(&mut self, writes: &[Write]) -> Result<(), CommitError> {
            if self.fault == 1 {
                return Err(CommitError::Rejected(Error::Unavailable));
            }
            self.writes.lock().unwrap().extend_from_slice(writes);
            if self.fault == 2 {
                Err(CommitError::Indeterminate)
            } else {
                Ok(())
            }
        }
    }
    let fault = tc.draw(gs::integers::<u8>().max_value(2));
    let abort = tc.draw(gs::booleans());
    let writes = Arc::new(Mutex::new(vec![]));
    let catalog = support::migration().apply(&Catalog::default()).unwrap();
    let mut store = Store::new(
        catalog,
        Disk {
            writes: writes.clone(),
            fault,
        },
    )
    .unwrap();
    for table in snap_identity::TABLES {
        store.load(table).unwrap();
    }
    let identity = Identity::default();
    let mut crypto = support::Fake::default();
    let result = store.run("enroll", |tx| {
        let issued = identity.enroll(tx, &mut crypto, "a@b", "password1", 0)?;
        if abort {
            Err(Error::Unavailable)
        } else {
            Ok(issued)
        }
    });
    if abort || fault == 1 {
        assert!(matches!(result, Err(Error::Unavailable)));
        assert!(writes.lock().unwrap().is_empty());
        assert!(matches!(
            store.run("resolve", |tx| identity.login(
                tx,
                &mut crypto,
                "a@b",
                "password1",
                1
            )),
            Err(Error::NotFound)
        ));
    } else if fault == 2 {
        assert!(matches!(result, Err(Error::Indeterminate)));
        assert_eq!(writes.lock().unwrap().len(), 3);
        assert!(matches!(
            store.run("fenced", |tx| identity.login(
                tx,
                &mut crypto,
                "a@b",
                "password1",
                1
            )),
            Err(Error::Indeterminate)
        ));
    } else {
        let issued = result.unwrap().value;
        assert_eq!(writes.lock().unwrap().len(), 3);
        store
            .run("resolve", |tx| {
                identity.resolve(tx, &crypto, &issued.bearer, 1)
            })
            .unwrap();
    }
}

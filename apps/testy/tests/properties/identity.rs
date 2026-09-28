#[path = "../../../../crates/identity/tests/support/mod.rs"]
mod support;
use futures::{FutureExt, executor::block_on, task::noop_waker};
use hegel::{TestCase, generators as gs};
use snap_platform_local::memory::Memory;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use testy_local::identity::{Sessions, platform};

#[hegel::test]
fn accepted_work_drains_before_queued_logout_across_expiry_and_dependency_failure(tc: TestCase) {
    let amount = tc.draw(gs::integers::<i64>().min_value(1).max_value(100));
    let expire = tc.draw(gs::booleans());
    let fail = tc.draw(gs::booleans());
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::new(10).unwrap(),
        move || clock.load(Ordering::SeqCst),
    );
    let memory = Memory::new(platform(sessions));
    let mut client = testy::Client::new(memory.channel());
    let bearer = block_on(client.authenticate(true, "a@b", "password1")).unwrap();
    block_on(client.start("held")).unwrap();
    let mut sibling = testy::Client::new(memory.channel());
    sibling.use_session(&bearer).unwrap();
    let mut pending = Box::pin(client.add_checked(amount));
    let waker = noop_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(pending.poll_unpin(&mut cx).is_pending());
    assert!(pending.poll_unpin(&mut cx).is_pending());
    let (ticket, key) = memory.pending_read().unwrap();
    let mut logout = Box::pin(sibling.logout());
    assert!(logout.poll_unpin(&mut cx).is_pending());
    assert!(logout.poll_unpin(&mut cx).is_pending());
    if expire {
        now.store(10, Ordering::SeqCst);
        memory.advance(1);
    }
    assert_eq!(memory.residents(), 1);
    assert!(logout.poll_unpin(&mut cx).is_pending());
    memory
        .supply(
            ticket,
            &key,
            if fail {
                Err(snap_execution::Error::Unavailable)
            } else {
                Ok(snap_transport::json!(1000))
            },
        )
        .unwrap();
    assert_eq!(
        block_on(pending),
        if fail {
            Err(snap_transport::Error::Unavailable)
        } else {
            Ok(amount)
        }
    );
    assert_eq!(
        block_on(logout),
        if expire {
            Err(snap_transport::Error::InvalidBearer)
        } else {
            Ok(())
        }
    );
    assert_eq!(memory.residents(), 0);
}

#[hegel::test]
fn authenticated_calculators_follow_connection_lifetimes(tc: TestCase) {
    let actions = tc.draw(gs::vecs(gs::integers::<u8>()).min_size(1).max_size(60));
    let now = Arc::new(AtomicI64::new(0));
    let clock = now.clone();
    let sessions = Sessions::new(
        support::store(true),
        support::Fake::default(),
        snap_identity::Identity::new(10).unwrap(),
        move || clock.load(Ordering::SeqCst),
    );
    let memory = Memory::new(platform(sessions));
    let mut clients = [
        testy::Client::new(memory.channel()),
        testy::Client::new(memory.channel()),
    ];
    block_on(clients[0].authenticate(true, "a@b", "password1")).unwrap();
    block_on(clients[1].authenticate(false, "a@b", "password1")).unwrap();
    let mut live = [true; 2];
    let mut expires = [10; 2];
    let mut connected = [false; 2];
    let mut values = [0i64; 2];
    for action in actions {
        let i = (action / 6 % 2) as usize;
        let time = now.load(Ordering::SeqCst);
        let valid = live[i] && time < expires[i];
        match action % 6 {
            0 => {
                let result = block_on(clients[i].start(&format!("tab{i}")));
                if valid && !connected[i] {
                    result.unwrap();
                    connected[i] = true;
                    values[i] = 0;
                } else {
                    assert!(result.is_err());
                }
            }
            1 => {
                let result = block_on(clients[i].add(1));
                if valid && connected[i] {
                    values[i] += 1;
                    assert_eq!(result.unwrap(), values[i]);
                } else {
                    assert!(result.is_err());
                }
            }
            2 => {
                clients[i].replace_channel(memory.channel());
                connected[i] = false;
                values[i] = 0;
            }
            3 => {
                let result = block_on(clients[i].logout());
                if valid {
                    result.unwrap();
                    live[i] = false;
                    connected[i] = false;
                } else {
                    assert!(result.is_err());
                }
                clients[i].replace_channel(memory.channel());
                connected[i] = false;
            }
            4 => {
                now.fetch_add(3, Ordering::SeqCst);
                memory.advance(1);
                for j in 0..2 {
                    if now.load(Ordering::SeqCst) >= expires[j] {
                        connected[j] = false;
                    }
                }
                // A revoked attachment is a closed physical lifetime; replace it.
                for j in 0..2 {
                    if !connected[j] {
                        clients[j].replace_channel(memory.channel());
                    }
                }
            }
            _ => {
                clients[i].replace_channel(memory.channel());
                block_on(clients[i].authenticate(false, "a@b", "password1")).unwrap();
                expires[i] = now.load(Ordering::SeqCst) + 10;
                live[i] = true;
                connected[i] = false;
            }
        }
        for j in 0..2 {
            if connected[j] {
                assert_eq!(
                    block_on(clients[j].inspect()).unwrap().accumulator,
                    values[j]
                );
            }
        }
        assert_eq!(
            memory.residents(),
            connected.iter().filter(|value| **value).count()
        );
    }
}

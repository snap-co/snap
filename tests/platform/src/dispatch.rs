//! Shared server-role properties at the command/result boundary. The reference
//! model is a scalar and an ordered list of outcomes, not another dispatcher or
//! transaction engine. Drivers never supply expected acceptance or completion.
use crate::cartridge::{Change, Edit, Read, Stop};
use alloc::{vec, vec::Vec};
use snap_transport::{Error, Event, Invocation, Operation, Outcome, json};

#[derive(Clone, Copy, Debug)]
pub enum Loss {
    Disconnect,
    Close,
    Expire,
}

/// The production host owns submission, admission, loading and execution.
/// Setup owns driving and observer loss; it must not synthesize result events.
pub trait Platform {
    fn call(&mut self, invocation: Invocation) -> Result<(), Error>;
    fn events(&mut self) -> Vec<Event>;
    fn finish(&mut self);
    fn lose(&mut self, loss: Loss);
    fn connect(&mut self) -> bool;
}

/// Only setups with a controlled Backend fault implement this capability.
pub trait CommitFault: Platform {
    fn reject_next_commit(&mut self);
}

pub fn invocation(id: u64, edit: &Edit) -> Invocation {
    Invocation {
        id,
        operation: Change::NAME.into(),
        input: serde_json::to_value(edit).unwrap(),
    }
}

/// Cartridge specification: a stale compare rejects admission; a successful
/// change moves both rows by `amount`. Every other outcome preserves both rows.
/// The error literals below are independent expectations, not `storage_error`.
pub(crate) fn predict(value: &mut i64, edit: &Edit) -> (bool, Outcome) {
    if edit.expected != *value {
        return (false, Err(Error::Application(json!("stale"))));
    }
    let outcome = match edit.stop {
        Stop::Commit => {
            *value += edit.amount;
            Ok(json!([*value, *value]))
        }
        Stop::Application => Err(Error::Application(json!("declined"))),
        Stop::InvalidOutput => Err(Error::InvalidOutput),
        Stop::CaughtMiss => Err(Error::Application(json!({"code":"StoreMiss"}))),
    };
    (true, outcome)
}
fn completed(id: u64, outcome: Outcome) -> Event {
    Event::Completed { id, outcome }
}
fn trace(id: u64, accepted: bool, outcome: Outcome) -> Vec<Event> {
    let mut events = Vec::new();
    if accepted {
        events.push(Event::Accepted { id });
    }
    events.push(completed(id, outcome));
    events
}

/// One persistent model per generated history. Only the input specification
/// changes `value`; observed results never become the expected state.
#[derive(Default)]
pub struct History {
    value: i64,
    sequence: u64,
}
impl History {
    pub fn value(&self) -> i64 {
        self.value
    }
    fn next_id(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// FIFO is observable when later compare guards depend on earlier commits.
    /// A successful result may appear only after the host drives execution.
    pub fn batch<P: Platform>(&mut self, platform: &mut P, edits: &[Edit]) {
        assert!(
            !edits.is_empty(),
            "a generated batch must exercise an operation"
        );
        let mut expected = Vec::new();
        for edit in edits {
            let id = self.next_id();
            let (accepted, outcome) = predict(&mut self.value, edit);
            expected.extend(trace(id, accepted, outcome));
            platform.call(invocation(id, edit)).unwrap();
        }
        let mut observed = platform.events();
        assert!(
            !observed
                .iter()
                .any(|event| matches!(event, Event::Completed { outcome: Ok(_), .. })),
            "success was published before execution was driven"
        );
        platform.finish();
        observed.extend(platform.events());
        assert_eq!(
            observed, expected,
            "admission/completion must match FIFO committed state"
        );
        self.verify_loaded_rows(platform);
    }

    /// Read goes through dispatch and declares its data again. The setup must
    /// exercise backend loading, not inspect a resident cache or model sidecar.
    pub fn verify_loaded_rows<P: Platform>(&mut self, platform: &mut P) {
        let id = self.next_id();
        platform
            .call(Invocation {
                id,
                operation: Read::NAME.into(),
                input: json!(null),
            })
            .unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        platform.finish();
        assert_eq!(
            platform.events(),
            vec![completed(id, Ok(json!([self.value, self.value])))],
            "backend-loaded rows disagree with the independent model"
        );
    }

    /// Repeated IDs still enter the real guards and handlers. Pending submissions
    /// and changed payloads are not collapsed, and lower IDs are not rejected.
    pub fn repeated_ids<P: Platform>(&mut self, platform: &mut P, edit: Edit, reconnect: bool) {
        let id = self.next_id();
        let command = invocation(id, &edit);
        let mut expected = Vec::new();
        for _ in 0..2 {
            let (accepted, outcome) = predict(&mut self.value, &edit);
            expected.extend(trace(id, accepted, outcome));
            platform.call(command.clone()).unwrap();
        }
        let mut observed = platform.events();
        platform.finish();
        observed.extend(platform.events());
        assert_eq!(
            observed, expected,
            "every submission must run its own admission"
        );
        self.verify_loaded_rows(platform);
        if reconnect {
            platform.lose(Loss::Disconnect);
            assert!(platform.connect(), "a retained lifetime should resume");
            assert!(
                platform.events().is_empty(),
                "reconnect must not push an unsolicited old completion"
            );
        }
        // Reuse the original ID with a different operation after a higher ID.
        platform
            .call(Invocation {
                id,
                operation: Read::NAME.into(),
                input: json!(null),
            })
            .unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        platform.finish();
        assert_eq!(
            platform.events(),
            vec![completed(id, Ok(json!([self.value, self.value])))]
        );
        let changed = Edit {
            expected: self.value,
            amount: edit.amount + 1,
            stop: Stop::Commit,
        };
        let (_, outcome) = predict(&mut self.value, &changed);
        platform.call(invocation(id, &changed)).unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        platform.finish();
        assert_eq!(platform.events(), vec![completed(id, outcome)]);
        self.verify_loaded_rows(platform);
    }

    /// Observer loss does not revoke accepted work. Reconnection never redirects
    /// its completion, even when the replacement peer exists before it finishes.
    pub fn draining<P: Platform>(&mut self, platform: &mut P, amount: i64, loss: Loss) {
        assert!(amount > 0);
        let id = self.next_id();
        let edit = Edit {
            expected: self.value,
            amount,
            stop: Stop::Commit,
        };
        let command = invocation(id, &edit);
        let _ = predict(&mut self.value, &edit);
        platform.call(command.clone()).unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        platform.lose(loss);
        if matches!(loss, Loss::Disconnect) {
            assert!(platform.connect());
            assert!(platform.events().is_empty());
        }
        platform.finish();
        if !matches!(loss, Loss::Disconnect) {
            assert!(!platform.connect());
        }
        assert!(
            platform.events().is_empty(),
            "old output reached the replacement peer"
        );
        self.verify_loaded_rows(platform);
        platform.call(command).unwrap();
        let expected = vec![completed(id, Err(Error::Application(json!("stale"))))];
        assert_eq!(
            platform.events(),
            expected,
            "a new submission must inspect the current committed state"
        );
        platform.finish();
        assert!(platform.events().is_empty());
        self.verify_loaded_rows(platform);
    }

    /// Handler success is insufficient: confirmed commit rejection must surface
    /// as failure, preserve backend rows, and release the dispatch lane without
    /// automatically executing it again.
    pub fn rejected_commit<P: CommitFault>(&mut self, platform: &mut P, amount: i64) {
        let id = self.next_id();
        let edit = Edit {
            expected: self.value,
            amount,
            stop: Stop::Commit,
        };
        let command = invocation(id, &edit);
        platform.call(command.clone()).unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        platform.reject_next_commit();
        platform.finish();
        assert_eq!(
            platform.events(),
            vec![completed(id, Err(Error::Unavailable))]
        );
        self.verify_loaded_rows(platform);
        platform.finish();
        assert!(
            platform.events().is_empty(),
            "a failed commit was retried implicitly"
        );
        // An explicit new submission with the same ID is not a cached failure.
        platform.call(command).unwrap();
        assert_eq!(platform.events(), vec![Event::Accepted { id }]);
        let (_, outcome) = predict(&mut self.value, &edit);
        platform.finish();
        assert_eq!(platform.events(), vec![completed(id, outcome)]);
        self.verify_loaded_rows(platform);
    }
}

//! Generated inputs for shared Transport-to-Store properties. Hegel explores and
//! shrinks histories; `History` states the rules and supplies independent results.
//! Fixed examples and this consumer use exactly the same real-host adapters.
use hegel::{TestCase, generators as gs};
use snap_platform_tests::{
    cartridge::{Edit, Stop},
    dispatch::{History, Loss},
};
#[path = "../support/dispatch.rs"]
mod support;

fn edit(tc: &TestCase, value: i64) -> Edit {
    Edit {
        expected: value + i64::from(tc.draw(gs::booleans())),
        amount: tc.draw(gs::integers::<i64>().min_value(-8).max_value(8)),
        stop: [
            Stop::Commit,
            Stop::Application,
            Stop::InvalidOutput,
            Stop::CaughtMiss,
        ][tc.draw(gs::integers::<usize>().max_value(3))],
    }
}

fn batch(tc: &TestCase, value: i64) -> Vec<Edit> {
    let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
    let mut baseline = value;
    (0..count)
        .map(|_| {
            let edit = edit(tc, baseline);
            // Generate both satisfied and stale compares, including compares against
            // earlier writes in this batch. This is input construction, not observed
            // state: only the independent model checks production results.
            if edit.expected == baseline && edit.stop == Stop::Commit {
                baseline += edit.amount;
            }
            edit
        })
        .collect()
}

macro_rules! properties {
    ($setup:ident) => {
        mod $setup {
            use super::*;

            /// Later guards see earlier commits; failed attempts leave both rows
            /// unchanged; no successful completion precedes execution.
            #[hegel::test]
            fn fifo_admission_commit_and_rollback_match_model(tc: TestCase) {
                let mut platform = support::$setup();
                let mut model = History::default();
                let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));
                for round in 0..rounds {
                    let edits = batch(&tc, model.value());
                    tc.note(&format!("round={round} batch={edits:?}"));
                    for edit in &edits {
                        tc.event(&format!("stop={:?}", edit.stop));
                    }
                    model.batch(&mut platform, &edits);
                }
            }

            /// Pending/completed retries replay, conflicting payloads fail, and
            /// physical reconnect never pushes an unsolicited old result.
            #[hegel::test]
            fn retained_retries_preserve_results_without_reexecution(tc: TestCase) {
                let mut platform = support::$setup();
                let mut model = History::default();
                let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(10));
                for round in 0..rounds {
                    let edit = edit(&tc, model.value());
                    let reconnect = tc.draw(gs::booleans());
                    tc.note(&format!(
                        "round={round} retry={edit:?} reconnect={reconnect}"
                    ));
                    tc.event(if reconnect {
                        "retained reconnect"
                    } else {
                        "same observer"
                    });
                    model.replay(&mut platform, edit, reconnect);
                }
            }

            /// Accepted work commits after observer loss. Only retained logical
            /// lifetimes keep its replay result; Close and expiry end that scope.
            #[hegel::test]
            fn accepted_work_drains_and_replay_respects_lifetime(tc: TestCase) {
                let mut platform = support::$setup();
                let mut model = History::default();
                let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(10));
                for round in 0..rounds {
                    let amount = tc.draw(gs::integers::<i64>().min_value(1).max_value(8));
                    let loss = [Loss::Disconnect, Loss::Close, Loss::Expire]
                        [tc.draw(gs::integers::<usize>().max_value(2))];
                    tc.note(&format!("round={round} amount={amount} loss={loss:?}"));
                    tc.event(&format!("loss={loss:?}"));
                    model.draining(&mut platform, amount, loss);
                }
            }
        }
    };
}

properties!(memory);
properties!(sqlite_memory);
properties!(sqlite_file);

/// Confirmed commit rejection is an explicit dependency capability. This setup
/// wraps real Memory; it does not pretend to inject faults into SQLite's private
/// connection or claim to simulate unknown outcomes.
#[hegel::test]
fn rejected_commit_never_reports_success_or_retries_itself(tc: TestCase) {
    let mut platform = support::rejecting_memory();
    let mut model = History::default();
    let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(20));
    for round in 0..rounds {
        let amount = tc.draw(gs::integers::<i64>().min_value(1).max_value(8));
        tc.note(&format!("round={round} reject commit amount={amount}"));
        model.rejected_commit(&mut platform, amount);
    }
}

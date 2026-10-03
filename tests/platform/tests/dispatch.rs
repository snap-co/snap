//! Fixed cartridge example; generated histories use the same production setup.
#[path = "support/dispatch.rs"]
mod support;
use snap_platform_tests::cartridge::{self, Edit, Stop};
use snap_platform_tests::dispatch::{History, Loss};

#[test]
fn rejected_commit_is_not_success_and_a_fresh_call_can_proceed() {
    History::default().rejected_commit(&mut support::rejecting_memory(), 3);
}

macro_rules! examples {
    ($setup:ident) => {
        mod $setup {
            use super::*;
            #[test]
            fn fifo_admission_and_rollback() {
                let mut platform = support::$setup();
                let mut model = History::default();
                model.batch(&mut platform, &cartridge::example());
                assert_eq!(model.value(), 5);
            }
            #[test]
            fn retained_retry_and_conflicting_input() {
                History::default().replay(
                    &mut support::$setup(),
                    Edit {
                        expected: 0,
                        amount: 3,
                        stop: Stop::Commit,
                    },
                    true,
                );
            }
            #[test]
            fn accepted_work_survives_observer_loss() {
                for loss in [Loss::Disconnect, Loss::Close, Loss::Expire] {
                    History::default().draining(&mut support::$setup(), 3, loss);
                }
            }
        }
    };
}
examples!(memory);
examples!(sqlite_memory);
examples!(sqlite_file);

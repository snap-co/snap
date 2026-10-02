use snap_platform_tests::{memory::Memory, store, transport};
use snap_store::Store;
use snap_store_sqlite::Sqlite;

mod support;

struct Storage<B> {
    store: Store<B>,
    // Drop Store before removing its database directory, including unwinding.
    _directory: Option<tempfile::TempDir>,
}

fn memory() -> Storage<Memory> {
    Storage {
        store: Store::new(store::catalog(), Memory::new(store::catalog()).unwrap()).unwrap(),
        _directory: None,
    }
}
fn sqlite_memory() -> Storage<Sqlite> {
    Storage {
        store: Sqlite::memory(&store::migrations()).unwrap(),
        _directory: None,
    }
}
fn sqlite_file() -> Storage<Sqlite> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store.sqlite");
    snap_store_sqlite::migrate(&path, &store::migrations()).unwrap();
    Storage {
        store: Sqlite::open(&path).unwrap(),
        _directory: Some(directory),
    }
}

// Only routing is repeated. Each contract body is shared across all backends.
macro_rules! storage_cases {
    ($setup:ident, $($case:ident),+ $(,)?) => {
        mod $setup {
            use super::*;
            $(#[test] fn $case() { store::$case(&mut super::$setup().store); })+
        }
    }
}
macro_rules! all_storage_cases {
    ($setup:ident) => {
        storage_cases!(
            $setup,
            secondary_indexes_and_negative_results_change_with_the_commit,
            unique_index_failure_discards_earlier_statements_and_allows_the_next_operation,
            cross_module_constraints_roll_back_every_write_including_memory,
            caught_miss_discards_writes_without_loading_or_retrying,
            cold_insert_does_not_claim_other_keys_or_a_complete_index,
            committed_changes_coalesce_and_publish_only_the_net_state,
            releasing_residency_does_not_delete_rows_or_claim_complete_indexes,
        );
    };
}
all_storage_cases!(memory);
all_storage_cases!(sqlite_memory);
all_storage_cases!(sqlite_file);

macro_rules! server_cases {
    ($name:ident, $driver:ident, $metadata:literal) => {
        mod $name {
            use super::*;
            async fn setup() -> support::server::Setup {
                support::server::start(support::server::Driver::$driver).await
            }
            #[tokio::test]
            async fn commands_and_observations() {
                let mut carrier = setup().await;
                transport::commands_and_observations(&mut carrier, $metadata).await;
                transport::close_reaches_host(&mut carrier).await;
            }
            #[tokio::test]
            async fn malformed_input_never_reaches_dispatch() {
                transport::malformed_input_never_reaches_dispatch(&mut setup().await).await;
            }
            #[tokio::test]
            async fn final_reply_at_retirement() {
                transport::final_reply_at_retirement(&mut setup().await).await;
            }
        }
    };
}
server_cases!(server_tcp, Tcp, true);
server_cases!(server_websocket, WebSocket, false);

mod client_tcp {
    use super::*;
    #[tokio::test]
    async fn commands_and_observations() {
        transport::commands_and_observations(&mut support::client::start().await, true).await;
    }
    #[tokio::test]
    async fn physical_loss_is_an_error() {
        transport::physical_loss_is_an_error(&mut support::client::start().await).await;
    }
}

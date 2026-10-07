//! Same client/server contract, native Store × carrier Cartesian product.
#[path = "support/matrix.rs"]
mod setup;
use snap_platform_tests::configuration::{Carrier, Storage};

macro_rules! carriers {
    ($module:ident, $storage:ident) => {
        mod $module {
            use super::*;
            macro_rules! contract {
                ($carrier_module:ident, $carrier:ident) => {
                    mod $carrier_module {
                        use super::*;
                        #[tokio::test]
                        async fn client_server_contract() {
                            setup::run(Storage::$storage, Carrier::$carrier).await;
                        }
                    }
                };
            }
            contract!(controlled, Controlled);
            contract!(tcp, Tcp);
            contract!(websocket, WebSocket);
        }
    };
}
carriers!(memory, Memory);
carriers!(sqlite_memory, SqliteMemory);
carriers!(sqlite_file, SqliteFile);

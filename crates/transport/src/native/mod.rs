//! Native composition. Applications own listeners, HTTP routes and shutdown;
//! Transport owns queue handoff and execution pumping. TCP never replays a call.
#[cfg(any(feature = "native-server", feature = "native-legacy"))]
mod driver;
mod tcp;
#[cfg(any(feature = "native-server", feature = "native-legacy"))]
mod web;

#[cfg(any(feature = "native-server", feature = "native-legacy"))]
pub use driver::Prepare;
pub use tcp::{Client as TcpClient, tls};
#[cfg(any(feature = "native-server", feature = "native-legacy"))]
pub use web::ReadCookie;

#[cfg(any(feature = "native-server", feature = "native-legacy"))]
mod server;
#[cfg(any(feature = "native-server", feature = "native-legacy"))]
pub use server::{Server, Transactions, WebSocket};

/// Temporary exports for hosts not yet migrated to `Server`. These are not the
/// supported composition API and will disappear with their remaining consumers.
#[cfg(feature = "native-legacy")]
#[doc(hidden)]
pub mod legacy {
    pub use super::driver::{Dispatcher, Endpoint, Prepare, Shared, dispatch};
    pub mod tcp {
        pub use super::super::tcp::*;
    }
    pub mod web {
        pub use super::super::web::*;
    }
}

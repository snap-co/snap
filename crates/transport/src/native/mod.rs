//! Native composition. Applications own listeners, HTTP routes and shutdown;
//! Transport owns queue handoff and execution pumping. TCP never replays a call.
/// Queue adapter and execution pump for hosts that explicitly drive a portable
/// loop. Ordinary applications use `Server`; controlled platforms own stepping.
#[cfg(feature = "native-server")]
pub mod driver;
pub mod tcp;
/// JSON carriers for a host-selected `carrier::Dispatch`, including custom
/// development hosts. Ordinary applications mount these through `Server`.
#[cfg(feature = "native-server")]
pub mod web;

#[cfg(feature = "native-server")]
pub use driver::Prepare;
pub use tcp::{Client as TcpClient, tls};
#[cfg(feature = "native-server")]
pub use web::ReadCookie;

#[cfg(feature = "native-server")]
mod server;
#[cfg(feature = "native-server")]
pub use server::{Server, Transactions, WebSocket};

//! Native Identity capabilities. App hosts select and mount the flows they use.
#[cfg(feature = "oauth")]
pub mod assertion;
#[cfg(feature = "oauth")]
pub mod oauth;
#[cfg(feature = "passkey")]
pub mod passkey;

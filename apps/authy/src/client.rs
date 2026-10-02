//! Authy's account SDK, composed after Identity resolves public authentication.
use snap_transport::{Channel, Error, Operation};
pub async fn account<C: Channel>(
    transport: &mut snap_transport::client::Client<C>,
) -> Result<crate::Account, Error> {
    let bearer = transport.bearer().map(alloc::string::String::from);
    let value = transport
        .request(
            bearer.as_deref(),
            crate::operations::FetchAccount::NAME,
            snap_transport::Value::Null,
        )
        .await?;
    snap_transport::client::decode(value)
}

//! Private durable continuations. Correlation secrets do not confer authority.
use alloc::string::String;
use serde::{Serialize, de::DeserializeOwned};
use snap_store::{Data, Error, Transaction};

pub(crate) const TABLE: &str = "identity.attempts";
pub(crate) fn data() -> Data {
    Data::new(&[TABLE])
}
pub(crate) fn read<T: DeserializeOwned>(
    tx: &mut Transaction<'_>,
    table: &str,
    id: &str,
) -> Result<Option<T>, Error> {
    tx.get(table, &[id.into()])?
        .map(|r| serde_json::from_str(crate::text(&r, "data")?).map_err(|_| Error::Invalid))
        .transpose()
}
pub(crate) fn write<T: Serialize>(
    tx: &mut Transaction<'_>,
    table: &str,
    id: &str,
    value: &T,
    insert: bool,
) -> Result<(), Error> {
    let data: String = serde_json::to_string(value).map_err(|_| Error::Invalid)?;
    if insert {
        tx.insert(
            table,
            crate::row([("id", id.into()), ("data", data.into())]),
        )
    } else {
        tx.update(table, &[id.into()], crate::row([("data", data.into())]))
    }
}

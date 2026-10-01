//! Platform assembly supplies physical inputs to portable Authy declarations.
use serde_json::json;
use snap_document_host::Host;

pub fn register(mut host: Host<snap_store_sqlite::Sqlite>) -> Host<snap_store_sqlite::Sqlite> {
    host = host.with_inputs(|key| match key {
        "clock" => Ok(json!(crate::now())),
        _ => Err(snap_transport::Error::Unavailable),
    });
    let app = authy::operations::declarations(|| snap_crypto::Native);
    for definition in app.preconnection {
        host = host.with_preconnection_request(definition);
    }
    for definition in app.requests {
        host = host.with_request(definition);
    }
    host
}

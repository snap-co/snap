//! Assembly selects Identity's provider and operations, then Authy's account view.
use serde_json::json;
use snap_document_host::Host;

pub fn register(mut host: Host<snap_store_sqlite::Sqlite>) -> Host<snap_store_sqlite::Sqlite> {
    host = host.with_inputs(|key| match key {
        "clock" => Ok(json!(crate::now())),
        _ => Err(snap_transport::Error::Unavailable),
    });
    let identity = snap_identity::operation::definitions(
        snap_identity::Identity::default(),
        || snap_crypto::Native,
        Some(snap_identity::operation::Enrollment {
            data: authy::enrollment_data(),
            initialize: Box::new(|tx, principal, email| {
                authy::initialize_account(tx, &principal.identity, email)
            }),
        }),
    );
    for definition in identity.preconnection {
        host = host.with_preconnection_request(definition);
    }
    for definition in identity.requests {
        host = host.with_request(definition);
    }
    for definition in authy::operations::declarations() {
        host = host.with_preconnection_request(definition);
    }
    host
}

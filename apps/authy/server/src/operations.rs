//! Assembly selects Identity's provider and operations, then Authy's account view.
use snap_transport::operation::Registry;

pub fn register(
    mut operations: Registry,
    webauthn: Option<snap_identity_native::passkey::Native>,
) -> Registry {
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
        operations = operations.with_preconnection_request(definition);
    }
    for definition in identity.requests {
        operations = operations.with_request(definition);
    }
    if let Some(webauthn) = webauthn {
        for definition in snap_identity::operation::passkey_definitions(
            snap_identity::Identity::default(),
            || snap_crypto::Native,
            webauthn,
            Some(snap_identity::operation::Enrollment {
                data: authy::enrollment_data(),
                initialize: Box::new(|tx, principal, label| {
                    // A registration label is metadata, never a linking proof.
                    // Authy retains its email-shaped enrollment metadata policy.
                    let email = snap_identity::Credential::canonical_email(label)?;
                    authy::initialize_account(tx, &principal.identity, &email)
                }),
            }),
            Some(snap_identity::operation::PasskeyLookup {
                data: snap_store::Data::new(&authy::TABLES),
                lookup: Box::new(|tx, name| {
                    let email = snap_identity::Credential::canonical_email(name)?;
                    let mut identities = Vec::new();
                    for row in tx.find(authy::ACCOUNTS, "primary", &[])? {
                        if row.get("email") == Some(&snap_store::Value::Text(email.clone())) {
                            let Some(snap_store::Value::Text(identity)) = row.get("identity")
                            else {
                                return Err(snap_store::Error::Invalid);
                            };
                            identities.push(identity.clone());
                        }
                    }
                    Ok(identities)
                }),
            }),
        ) {
            operations = operations.with_preconnection_request(definition);
        }
    }
    for definition in authy::operations::declarations() {
        operations = operations.with_preconnection_request(definition);
    }
    operations
}

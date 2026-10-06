//! Account operations over Authy's metadata interface. Authentication facts come
//! from Transport's provider; this app does not resolve credentials or sessions.
use alloc::{vec, vec::Vec};
use snap_transport::{
    Operation,
    operation::{Definition, Guard, TypedFailure},
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub profile: alloc::string::String,
    pub first_name: alloc::string::String,
    pub last_name: alloc::string::String,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EditError {
    Denied,
    Invalid,
    Conflict,
}
pub struct EditProfile;
impl Operation for EditProfile {
    const NAME: &'static str = "authy.profile.edit";
    type Input = EditInput;
    type Output = ();
    type Error = EditError;
    type Progress = ();
}

pub fn edit_profile() -> Definition {
    Definition::typed::<EditProfile>(
        true,
        vec![Guard::new(|tx, call, context| {
            let input: EditInput = serde_json::from_value(call.input.clone())
                .map_err(|_| snap_transport::Error::InvalidInput)?;
            let actor = context
                .actor
                .as_deref()
                .ok_or(snap_store::Error::NotFound)?;
            let role = crate::access().role(
                tx,
                &snap_access::Resource::new(crate::PROFILE_KIND, &input.profile)?,
                Some(actor),
                false,
            )?;
            let reject = |error| snap_transport::Error::Application(serde_json::json!(error));
            if !snap_access::allows(role, snap_access::Role::Owner) {
                return Err(reject(EditError::Denied).into());
            }
            let profile = crate::Profile::read(tx, &input.profile)?;
            if profile.revision != input.revision {
                return Err(reject(EditError::Conflict).into());
            }
            crate::normalize_names(&input.first_name, &input.last_name).map_err(reject)?;
            Ok(())
        })],
        crate::profile_data(),
        &[],
        |tx, input, _| {
            let (first, last) = crate::normalize_names(&input.first_name, &input.last_name)
                .map_err(TypedFailure::Application)?;
            let revision = input
                .revision
                .checked_add(1)
                .ok_or(snap_store::Error::Invalid)?;
            tx.update(
                crate::PROFILES,
                &[input.profile.into()],
                snap_store::Row::from([
                    ("first_name".into(), first.into()),
                    ("last_name".into(), last.into()),
                    ("revision".into(), revision.into()),
                ]),
            )?;
            Ok(())
        },
    )
}

pub struct FetchAccount;
impl Operation for FetchAccount {
    const NAME: &'static str = "authy.account";
    type Input = ();
    type Output = crate::Account;
    type Error = ();
    type Progress = ();
}
pub fn declarations() -> Vec<Definition> {
    vec![Definition::typed::<FetchAccount>(
        true,
        vec![],
        crate::Account::data(),
        &[],
        |tx, _, context| {
            let principal = context
                .principal
                .as_ref()
                .ok_or(snap_store::Error::NotFound)?;
            Ok(crate::account_by_identity(
                tx,
                &principal.identity,
                principal.authenticated_at,
            )?)
        },
    )]
}

pub fn http_routes() -> Vec<snap_transport::carrier::HttpRoute> {
    vec![snap_transport::carrier::HttpRoute {
        operation: FetchAccount::NAME,
        method: snap_transport::carrier::HttpMethod::Get,
        read_bearer: true,
    }]
}

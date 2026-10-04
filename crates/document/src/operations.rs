//! Document protocol declarations consumed by the shared Transport dispatcher.
use crate::{
    Completion, DocumentAccessGuard, Manifest, ServerMessage,
    server::{AdmittedMutation, Document},
};
use alloc::{collections::BTreeSet, string::String, sync::Arc, vec, vec::Vec};
use snap_store::{Error, Transaction};
use snap_transport::{
    Operation, Value,
    operation::{Context, Definition, Failure, Guard},
};

/// Shared SDK/server contracts. The host may publish application progress while
/// finishing post-commit work; Document does not interpret those payloads.
pub struct Mutate;
impl Operation for Mutate {
    const NAME: &'static str = "document.mutate";
    type Input = crate::Intent;
    type Output = ServerMessage;
    type Error = crate::Error;
    type Progress = Value;
}
pub struct Synchronize;
impl Operation for Synchronize {
    const NAME: &'static str = "document.manifest";
    type Input = Manifest;
    type Output = ServerMessage;
    type Error = crate::Error;
    type Progress = Value;
}

#[derive(serde::Serialize, serde::Deserialize)]
enum Prepared {
    Mutation(AdmittedMutation),
    Replay(Completion),
    Manifest(Manifest, BTreeSet<String>),
}

pub fn definitions(document: Arc<Document>) -> Vec<Definition> {
    let mutation_policy = admission(document.clone(), true);
    let sync_policy = admission(document.clone(), false);
    let behavior = document.clone();
    vec![
        Definition::typed::<Mutate>(
            true,
            vec![mutation_policy],
            document.metadata(),
            &[],
            move |tx, _, context| execute(&behavior, tx, context).map_err(Into::into),
        ),
        Definition::typed::<Synchronize>(
            true,
            vec![sync_policy],
            document.metadata(),
            &[],
            move |tx, _, context| execute(&document, tx, context).map_err(Into::into),
        ),
    ]
}

fn admission(document: Arc<Document>, mutation: bool) -> Guard {
    Guard::new(move |tx, call, context| {
        let actor = context.actor.as_deref().ok_or(Error::Invalid)?;
        let lifetime = context
            .lifetime
            .as_deref()
            .ok_or(snap_transport::Error::IdentityRequired)?;
        let policy = DocumentAccessGuard::new(&document);
        let prepared = if mutation {
            let intent = serde_json::from_value(call.input.clone()).map_err(|_| Error::Invalid)?;
            if let Some(mut completion) = document.recover(tx, lifetime, actor, &intent)? {
                // A new recovery observation uses current visibility, unlike the
                // terminal result of an already accepted original invocation.
                if let Ok(Some(_)) = &completion.result
                    && !policy.extent(tx, actor)?.contains(&intent.document)
                {
                    completion.result = Ok(None);
                }
                Prepared::Replay(completion)
            } else {
                Prepared::Mutation(policy.admit(tx, actor, &intent)?.map_err(|error| {
                    Failure::Rejected(snap_transport::Error::Application(
                        serde_json::to_value(error).expect("document error"),
                    ))
                })?)
            }
        } else {
            let manifest =
                serde_json::from_value(call.input.clone()).map_err(|_| Error::Invalid)?;
            Prepared::Manifest(manifest, policy.extent(tx, actor)?)
        };
        context.prepared = serde_json::to_value(prepared).map_err(|_| Error::Invalid)?;
        Ok(())
    })
}

fn execute(
    document: &Document,
    tx: &mut Transaction<'_>,
    context: &mut Context,
) -> Result<ServerMessage, Error> {
    let lifetime = context.lifetime.as_deref().ok_or(Error::Invalid)?;
    let prepared: Prepared = serde_json::from_value(core::mem::take(&mut context.prepared))
        .map_err(|_| Error::Invalid)?;
    Ok(match prepared {
        Prepared::Replay(completion) => {
            context.publication = snap_transport::json!({crate::wire::KIND: crate::sync::Publication {
                completion: completion.clone(), replication: None,
            }});
            ServerMessage::Completed(completion)
        }
        Prepared::Mutation(admitted) => {
            let result = document.execute_recorded(tx, lifetime, admitted)?;
            context.publication = snap_transport::json!({crate::wire::KIND: crate::sync::Publication {
                completion: result.completion.clone(), replication: result.replication,
            }});
            ServerMessage::Completed(result.completion)
        }
        Prepared::Manifest(manifest, extent) => ServerMessage::Manifest(document.manifest(
            tx,
            lifetime,
            context.actor.as_deref().ok_or(Error::Invalid)?,
            &manifest,
            &extent,
        )?),
    })
}

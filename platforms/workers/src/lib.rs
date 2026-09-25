//! Local execution on Cloudflare Workers. No threaded runtime or unsafe Send shims.
pub mod crypto;
pub mod host;
pub mod store;
use snap_protocol::{Error, Invocation, Outcome, Provider};
use snap_web::{Binding, Completion, Method};
use worker::{Request, Response, Result};

/// Dispatch an already normalized invocation on the caller's event loop.
pub async fn invoke(
    provider: &mut impl Provider<Context = (), Output = Outcome>,
    invocation: Invocation,
) -> Result<Response> {
    let target = invocation.operation_id.clone();
    let outcome = provider.invoke(invocation, ()).await;
    let status = match &outcome {
        Ok(_) => 200,
        Err(Error::UnavailableError { .. }) => 503,
        Err(Error::IdentityRequiredError { .. }) => 401,
        Err(Error::IdentityForbiddenError { .. }) => 403,
        _ => 400,
    };
    Response::from_json(&Completion::new(target, outcome)).map(|r| r.with_status(status))
}

/// Stateless, read-only HTTP dispatch. Composition explicitly selects bindings;
/// this entry point has no session or detached-mutation lifetime semantics.
pub async fn query(
    request: Request,
    provider: &mut impl Provider<Context = (), Output = Outcome>,
    bindings: &[Binding],
) -> Result<Response> {
    let binding = bindings.iter().find(|b| {
        b.http == Some(Method::Get) && request.path() == format!("/{}", b.key.replace('.', "/"))
    });
    let Some(binding) = binding else {
        return Response::error("Not found", 404);
    };
    if request.method() != worker::Method::Get {
        return Response::error("Method not allowed", 405);
    }
    let fields = request
        .url()?
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), serde_json::Value::String(v.into_owned())))
        .collect::<serde_json::Map<_, _>>();
    invoke(
        provider,
        Invocation {
            operation_id: request
                .headers()
                .get("x-snap-operation-id")?
                .unwrap_or_else(|| "query".into()),
            key: binding.key.into(),
            payload: (!fields.is_empty()).then_some(fields.into()),
            traceparent: request.headers().get("traceparent")?,
        },
    )
    .await
}

use worker::*;

#[event(fetch)]
pub async fn fetch(request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    if request.path() == "/__snap/build" {
        return Response::from_json(
            &serde_json::json!({"contract":1,"application":"healthy","build":"healthy-workers"}),
        );
    }
    let mut provider = healthy::application().map_err(|e| Error::RustError(format!("{e:?}")))?;
    snap_workers::query(
        request,
        &mut provider,
        &[snap_web::Binding::get("health.up")],
    )
    .await
}

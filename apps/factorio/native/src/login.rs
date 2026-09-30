//! Browser confirmation for a native TCP credential exchange. The URL contains
//! a public challenge, never the secret proof required to collect the credential.
use super::*;
use axum::{Form, extract::Path, response::Html};

/// Refresh native authority before admission, and during idle/long-running
/// attachments. Selection never accepts an expired local credential. The shared
/// dispatcher still revalidates authority when admitting each protected operation.
/// Renewal may wait behind an accepted controller; waiting does not block Tokio
/// or shorten the login/recovery lifetime. Network IO never holds the host lock.
pub async fn prepare(oauth: &Arc<OAuth>, command: snap_transport::Command) -> Result<(), Error> {
    let id = oauth
        .run_async("cli.refresh.select", move |tx| match command {
            snap_transport::Command::Connect { bearer, .. }
            | snap_transport::Command::Request {
                bearer: Some(bearer),
                ..
            } => operations::session_id(tx, &bearer).map(|(id, _)| Some(id)),
            snap_transport::Command::Request { invocation, .. }
                if invocation.operation == "factorio.login-finish" =>
            {
                let code = invocation.input["code"].as_str().ok_or(Error::Invalid)?;
                let proof = invocation.input["proof"].as_str().ok_or(Error::Invalid)?;
                let row = tx
                    .get("factorio.cli_login", &[code.into()])?
                    .ok_or(Error::NotFound)?;
                let (
                    Some(snap_store::Value::Text(saved)),
                    Some(snap_store::Value::Integer(expires)),
                    Some(snap_store::Value::Text(id)),
                ) = (row.get("proof"), row.get("expires"), row.get("session"))
                else {
                    return Err(Error::Invalid);
                };
                if *expires <= now() || !rp::same_secret(saved, &rp::digest(proof)) {
                    return Err(Error::NotFound);
                }
                Ok((!id.is_empty()).then(|| id.clone()))
            }
            _ => Ok(None),
        })
        .await?;
    if let Some(id) = id {
        oauth.session_id(&id).await?;
    }
    Ok(())
}

fn safe(code: &str) -> bool {
    code.len() == 43
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn page_response(content: String) -> Response {
    // no-referrer makes Chromium form POST send Origin:null. same-origin
    // preserves CSRF origin validation without leaking the challenge elsewhere.
    (
        [("cache-control","no-store"),("referrer-policy","same-origin"),("x-frame-options","DENY"),("content-security-policy","default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'")],
        Html(format!(r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Connect factory | Factorio</title><style>body{{margin:0;background:#f6f7f4;color:#23332d;font:16px/1.5 system-ui,sans-serif}}main{{max-width:42rem;margin:8vh auto;padding:24px}}h1{{font-size:30px;line-height:1.2;letter-spacing:-.025em}}code{{display:block;overflow-wrap:anywhere;background:#e2eee5;padding:16px;font:14px/1.6 ui-monospace,monospace}}button,a{{min-height:44px;box-sizing:border-box}}button{{font:550 16px system-ui;background:#286044;color:white;border:0;border-radius:7px;padding:12px 18px;cursor:pointer}}button:hover{{background:#1f4d36}}a{{color:#286044;display:inline-block;padding:10px 0}}:focus-visible{{outline:3px solid #286044;outline-offset:3px}}::selection{{background:#e2eee5}}p{{max-width:65ch}}form{{margin-top:24px}}</style><main>{content}</main></html>"#)),
    ).into_response()
}
pub async fn page(
    State(app): State<Arc<App>>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !safe(&code) {
        return failure(Error::Invalid);
    }
    let s = match app.oauth.session(&headers).await {
        Ok(s) => s,
        Err(_) => return page_response("<h1>Sign in to connect Factorio CLI</h1><p>Sign in with Authy, then reopen the link printed by factorio login. This request expires after five minutes.</p><a href=\"/auth/login\">Sign in with Authy</a>".into()),
    };
    match app.oauth.run("cli.login.inspect", |tx| {
        let row = tx
            .get("factorio.cli_login", &[code.clone().into()])?
            .ok_or(Error::NotFound)?;
        if !matches!(row.get("expires"),Some(snap_store::Value::Integer(t)) if *t>now()) {
            return Err(Error::NotFound);
        }
        Ok(())
    }) {
        Ok(()) => {}
        Err(e) => return failure(e),
    }
    // Generated random csrf and code contain no HTML metacharacters.
    if !s
        .csrf
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return failure(Error::Invalid);
    }
    page_response(format!(
        "<h1>Connect Factorio CLI</h1><p>Only continue if you started factorio login yourself. Compare this request code with your terminal.</p><code>{code}</code><p>The CLI can read and change your workspaces, tickets and sessions. It cannot approve candidates. Access lasts up to 30 days with automatic renewal of short-lived access tokens, and ends if your sign-in expires or is revoked.</p><form method=\"post\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button type=\"submit\">Allow CLI access</button></form><a href=\"/\">Cancel and return to Factorio</a>",
        s.csrf
    ))
}
#[derive(serde::Deserialize)]
pub struct Approval {
    csrf: String,
}
pub async fn approve(
    State(app): State<Arc<App>>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Form(input): Form<Approval>,
) -> Response {
    if !safe(&code) {
        return failure(Error::Invalid);
    }
    let s = match app.oauth.session(&headers).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(&app.oauth.config.origin)
        || !rp::same_secret(&input.csrf, &s.csrf)
    {
        return failure(Error::Invalid);
    }
    match app.oauth.run("cli.login.approve", |tx| {
        rp::lease(tx,&s.id,now())?;
        let row=tx.get("factorio.cli_login", &[code.clone().into()])?.ok_or(Error::NotFound)?;
        if !matches!(row.get("expires"),Some(snap_store::Value::Integer(t)) if *t>now()) || !matches!(row.get("session"),Some(snap_store::Value::Text(s)) if s.is_empty()) {return Err(Error::NotFound);}
        tx.update("factorio.cli_login", &[code.into()],[("session".into(),s.id.into())].into_iter().collect())
    }) {Ok(())=>page_response("<h1>CLI access approved</h1><p>Return to your terminal. Factory will save its credential and finish signing in.</p><a href=\"/\">Return to Factorio</a>".into()),Err(e)=>failure(e)}
}

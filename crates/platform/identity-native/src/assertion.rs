//! Host bootstrap for Authy's redirect-free login. Public keys are fetched once;
//! key rotation requires restart in this prototype. Clients do no HTTP login IO.
use serde_json::Value;
use snap_store::Error;
pub async fn authy_keys(issuer: &str) -> Result<Value, Error> {
    if super::oauth::origin(issuer)? != issuer {
        return Err(Error::Invalid);
    }
    let url = url::Url::parse(issuer).map_err(|_| Error::Invalid)?;
    if url.scheme() != "https"
        && !url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    {
        return Err(Error::Invalid);
    }
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| Error::Unavailable)?;
    let mut response = http
        .get(format!("{issuer}/oauth/jwks"))
        .send()
        .await
        .map_err(|_| Error::Unavailable)?;
    if !response.status().is_success() {
        return Err(Error::Unavailable);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(Error::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
}

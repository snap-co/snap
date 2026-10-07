//! App bootstrap for Authy's redirect-free agent login. Rotation needs restart.
use snap_store::Error;
pub async fn authy_keys(issuer: &str) -> Result<serde_json::Value, Error> {
    use snap_http::client::{Client, Outgoing, collect};
    if crate::identity_host::origin(issuer)? != issuer {
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
    let client = snap_http::native::Client::new().map_err(|_| Error::Unavailable)?;
    let mut response = client
        .send(Outgoing {
            method: "GET",
            url: format!("{issuer}/oauth/jwks"),
            headers: vec![],
            body: vec![],
            max_bytes: 64 * 1024,
            timeout_ms: 10_000,
        })
        .await
        .map_err(|_| Error::Unavailable)?;
    if response.status != 200 {
        return Err(Error::Unavailable);
    }
    let bytes = collect(&mut response.body, 64 * 1024)
        .await
        .map_err(|_| Error::Unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
}

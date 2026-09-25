//! Native HTTP header adapter for the shared signed-cookie carrier.
use axum::http::HeaderMap;
use snap_protocol::Error;

#[derive(Clone)]
pub struct Cookie(snap_web::cookie::Cookie);
impl snap_http::Cookie for Cookie {
    fn read(&self, header: Option<&str>) -> Result<Option<String>, snap_http::Response> {
        snap_http::Cookie::read(&self.0, header)
    }
    fn encode(&self, token: Option<&str>) -> String {
        self.0.encode(token)
    }
}
impl Cookie {
    pub fn new(key: Vec<u8>, name: &str, secure: bool, max_age: u64) -> std::io::Result<Self> {
        snap_web::cookie::Cookie::new(key, name, secure, max_age)
            .map(Self)
            .map_err(|_| std::io::Error::other("Invalid cookie configuration"))
    }
    pub fn read(&self, headers: &HeaderMap) -> Result<Option<String>, Error> {
        self.0.read(
            headers
                .get_all("cookie")
                .iter()
                .map(|h| h.to_str().unwrap_or_default()),
        )
    }
    pub fn encode(&self, token: Option<&str>) -> String {
        self.0.encode(token)
    }
}

//! TLS is native IO, not part of SNAP framing or application authority. There is
//! no plaintext fallback or early data. Verification finishes before callers can
//! send credentials. Configured private roots replace, rather than extend, public
//! roots. Certificates are loaded at startup; rotation requires a host restart.
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsAcceptor, TlsConnector,
    rustls::{self, pki_types::ServerName},
};

pub type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;
pub type ServerStream = tokio_rustls::server::TlsStream<TcpStream>;

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}
fn certificates(path: &Path) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let bytes = std::fs::read(path)?;
    let certs = rustls_pemfile::certs(&mut bytes.as_slice()).collect::<io::Result<Vec<_>>>()?;
    if certs.is_empty() {
        return Err(invalid("PEM file contains no certificates"));
    }
    Ok(certs)
}
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Clone)]
pub struct ClientTls {
    connector: TlsConnector,
    server_name: Option<ServerName<'static>>,
}
impl ClientTls {
    /// An explicit CA bundle is the entire trust set. An override names the
    /// certificate to verify when connecting through a tunnel or local proxy.
    pub fn new(ca_file: Option<&Path>, server_name: Option<&str>) -> io::Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        if let Some(path) = ca_file {
            for cert in certificates(path)? {
                roots.add(cert).map_err(invalid)?;
            }
        } else {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        let config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            connector: TlsConnector::from(Arc::new(config)),
            server_name: server_name
                .map(|s| ServerName::try_from(s.to_owned()).map_err(invalid))
                .transpose()?,
        })
    }
    /// The ten-second deadline includes DNS, TCP connect and verified TLS.
    pub async fn connect(&self, addr: &str) -> io::Result<ClientStream> {
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| invalid("Expected host:port or [IPv6]:port"))?;
        port.parse::<u16>().map_err(invalid)?;
        let host = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host);
        let name = match &self.server_name {
            Some(name) => name.clone(),
            None => ServerName::try_from(host.to_owned()).map_err(invalid)?,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let socket = TcpStream::connect(addr).await?;
            socket.set_nodelay(true)?;
            self.connector.connect(name, socket).await
        })
        .await?
    }
}

#[derive(Clone)]
pub struct ServerTls {
    acceptor: TlsAcceptor,
}
impl ServerTls {
    pub fn new(cert_file: &Path, key_file: &Path) -> io::Result<Self> {
        let certs = certificates(cert_file)?;
        let bytes = std::fs::read(key_file)?;
        let key = rustls_pemfile::private_key(&mut bytes.as_slice())?
            .ok_or_else(|| invalid("PEM file contains no private key"))?;
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(invalid)?;
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }
    /// Bound unauthenticated TLS work before the host allocates a peer. The
    /// Document listener separately caps simultaneous handshakes/connections.
    pub async fn accept(&self, socket: TcpStream) -> io::Result<ServerStream> {
        socket.set_nodelay(true)?;
        tokio::time::timeout(Duration::from_secs(10), self.acceptor.accept(socket)).await?
    }
}

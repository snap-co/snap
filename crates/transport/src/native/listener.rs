use std::{io, net::SocketAddr};
use tokio::net::{TcpListener, TcpSocket};

/// A bound address that cannot accept connections yet, including when port zero
/// selects an ephemeral port. Hosts can derive their origin and finish fallible
/// initialization, including controller recovery, before calling `listen`.
/// Dropping a pending listener closes the socket without exposing the service.
/// Activation is per socket, not an atomic commit across multiple listeners,
/// and does not promise the running service will remain healthy.
pub struct PendingListener(TcpSocket);

impl PendingListener {
    pub fn reserve(address: SocketAddr) -> io::Result<Self> {
        let socket = if address.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        // Match Tokio's listener restart behavior, without Windows' permissive
        // SO_REUSEADDR semantics that allow hijacking another listener.
        #[cfg(unix)]
        socket.set_reuseaddr(true)?;
        socket.bind(address)?;
        Ok(Self(socket))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    /// Enable connections only once the host has completed bootstrap. The host
    /// must drive its serving futures immediately after activation.
    pub fn listen(self) -> io::Result<TcpListener> {
        self.0.listen(1024)
    }
}

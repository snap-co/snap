//! Document host composition. TLS/binary socket handling lives in transport-tcp.
pub use crate::carrier::Prepare;
use crate::{carrier::Dispatcher, web::Shared};
use snap_store::Backend;
use std::sync::Arc;

pub async fn serve<B: Backend + Send + 'static>(
    listener: tokio::net::TcpListener,
    shared: Arc<Shared<B>>,
    tls: snap_transport_tcp::tls::ServerTls,
) -> std::io::Result<()> {
    snap_transport_tcp::serve(listener, Dispatcher::tcp(shared, None), tls).await
}
pub async fn serve_prepared<B: Backend + Send + 'static>(
    listener: tokio::net::TcpListener,
    shared: Arc<Shared<B>>,
    tls: snap_transport_tcp::tls::ServerTls,
    prepare: Prepare,
) -> std::io::Result<()> {
    snap_transport_tcp::serve(listener, Dispatcher::tcp(shared, Some(prepare)), tls).await
}

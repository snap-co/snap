//! Application-owned local platform. Mount transport; select memory or native IO.
//! No application, identity, persistence or cache provider is installed implicitly.
pub mod memory;
#[cfg(feature = "native")]
pub mod native;

use snap_transport::{
    Command, Error, Event, Response,
    server::{Application, Attachment, Authority, Server},
};

#[derive(Default)]
pub struct Peer {
    attachment: Option<Attachment>,
}
pub struct Platform<A: Application, R: Authority> {
    pub transport: Server<A, R>,
}
impl<A: Application, R: Authority> Platform<A, R> {
    pub fn new(transport: Server<A, R>) -> Self {
        Self { transport }
    }
    /// The platform retains physical handles, never the client. Emission is local
    /// queueing before handler entry; external delivery is not an acceptance gate.
    pub fn receive(
        &mut self,
        peer: &mut Peer,
        command: Command,
        now: u64,
        mut observe: impl FnMut(Event),
    ) -> Response {
        self.transport.tick(now);
        let mut events = Vec::new();
        let mut emit = |event: Event| {
            observe(event.clone());
            events.push(event);
        };
        match command {
            Command::Connect { bearer, client_id } => {
                if peer.attachment.is_some() {
                    return Response::Failed(Error::Occupied);
                }
                match self.transport.connect(&bearer, &client_id, now) {
                    Ok((attachment, resumed)) => {
                        peer.attachment = Some(attachment);
                        Response::Attached { resumed }
                    }
                    Err(error) => Response::Failed(error),
                }
            }
            Command::Request { bearer, invocation } => {
                self.transport
                    .request(bearer.as_deref(), invocation, &mut emit);
                Response::Events(events)
            }
            Command::Invoke(invocation) => {
                match &peer.attachment {
                    Some(attachment) => self.transport.invoke(attachment, invocation, &mut emit),
                    None => emit(Event::Completed {
                        id: invocation.id,
                        outcome: Err(Error::IdentityRequired),
                    }),
                }
                Response::Events(events)
            }
            Command::Disconnect | Command::Close => {
                let Some(attachment) = peer.attachment.take() else {
                    return Response::Failed(Error::StaleConnection);
                };
                let result = if matches!(command, Command::Close) {
                    self.transport.close(&attachment)
                } else {
                    self.transport.disconnect(&attachment, now)
                };
                match result {
                    Ok(()) => Response::Detached,
                    Err(error) => Response::Failed(error),
                }
            }
        }
    }
    pub fn lost(&mut self, peer: &mut Peer, now: u64) {
        if let Some(attachment) = peer.attachment.take() {
            let _ = self.transport.disconnect(&attachment, now);
        }
    }
}

//! A small host consumer connecting verified transport requests to Store's gate.
//! Store misses are terminal responses, never execution::Need or hidden retries.
use snap_store::{Backend, Store};
use snap_transport::{
    Command, Error, Event, Response, json,
    server::{Config, Server},
};

pub struct Host<B> {
    pub store: Store<B>,
    transport: Server<testy::TestAuthority>,
}
impl<B: Backend> Host<B> {
    pub fn new(store: Store<B>) -> Self {
        Self {
            store,
            transport: Server::new(testy::TestAuthority, Config::default()),
        }
    }
    pub fn exchange(&mut self, command: Command) -> Response {
        let Command::Request { bearer, invocation } = command else {
            return Response::Failed(Error::Protocol);
        };
        let id = invocation.id;
        let result = self.transport.request(bearer.as_deref(), invocation);
        let dispatch = match result {
            Ok(dispatch) => dispatch,
            Err(error) => return Response::Failed(error),
        };
        if dispatch.identity.is_none() {
            return Response::Failed(Error::IdentityRequired);
        }
        if dispatch.invocation.operation != "accounts.create" {
            return Response::Failed(Error::UnknownOperation);
        }
        let input = dispatch.invocation.input;
        let (Some(account), Some(email)) = (input["id"].as_i64(), input["email"].as_str()) else {
            return Response::Failed(Error::InvalidInput);
        };
        let outcome = self
            .store
            .run("accounts.create", |tx| {
                testy::store::create(tx, account, email)
            })
            .map(|_| json!({"created": account}))
            .map_err(|error| match error {
                snap_store::Error::Miss(lookup) => Error::Application(
                    json!({"code": "StoreMiss", "table": lookup.table, "index": lookup.index}),
                ),
                other => Error::Application(json!({"code": format!("{other:?}")})),
            });
        Response::Events(vec![
            Event::Accepted { id },
            Event::Completed { id, outcome },
        ])
    }
}

pub fn migration() -> snap_store::migration::Migration {
    toml::from_str(include_str!("../../migrations/0001_signup.toml"))
        .expect("Testy migration declaration")
}

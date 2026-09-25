use snap_protocol::{Invocation, Operation, Provider};

struct Authy<K>(
    snap_runtime::passport::Passport<snap_native::store::Store, snap_native::passport::Crypto, K>,
);
impl<K: snap_store::Cache> Provider for Authy<K> {
    type Context = Option<String>;
    type Output = snap_native::Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        self.0.operations()
    }
    fn prepare(
        &mut self,
        invocation: Invocation,
        token: Option<String>,
    ) -> impl core::future::Future<Output = snap_protocol::Admission<Self::Output>> + 'static {
        let future = snap_protocol::dispatch(
            &mut self.0,
            invocation,
            snap_runtime::passport::Context {
                token,
                now: snap_native::now(),
            },
        );
        async move {
            snap_protocol::project(future.await, |response| snap_native::Reply {
                outcome: response.outcome,
                empty: response.empty,
                lease: response.session.map(|s| snap_native::Lease {
                    id: s.session_id,
                    expires_at: s.expires_at,
                }),
                cookie: response.token,
                terminate: response.revoked,
            })
        }
    }
}
/// Native composition shared by the packaged app and cache-policy contract adapter.
pub fn run(cache: impl snap_store::Cache) -> std::io::Result<()> {
    let config = snap_native::Config::from_env("authy")?;
    let origin =
        std::env::var("SNAP_ORIGIN").unwrap_or_else(|_| format!("http://{}", config.address));
    let origin = snap_native::parse_origin(&origin)?;
    let database = std::env::var_os("SNAP_DATABASE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ".snap/authy.sqlite".into());
    let store = snap_native::store::Store::sqlite(&database, &authy::schemas())
        .map_err(|e| std::io::Error::other(format!("Store registration failed: {e:?}")))?;
    let key = snap_native::passport::signing_key(&store)?;
    let cookie = snap_native::cookie::Cookie::new(
        key,
        "authy",
        origin.scheme() == "https",
        snap_runtime::passport::SESSION_SECONDS,
    )?;
    let signer = snap_native::oidc::Crypto::load(&store)?;
    let passport = authy::server(store.clone(), snap_native::passport::Crypto, cache);
    let chatty_origin =
        std::env::var("CHATTY_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:3850".into());
    let chatty_origin = snap_native::parse_origin(&chatty_origin)?
        .origin()
        .ascii_serialization();
    let http = authy::http::Web {
        issuer: snap_oidc::Issuer {
            origin: origin.origin().ascii_serialization(),
            clients: vec![snap_oidc::Client {
                id: "chatty".into(),
                name: "Chatty".into(),
                redirect_uri: format!("{chatty_origin}/auth/callback"),
                post_logout_redirect_uri: format!("{chatty_origin}/auth/logged-out"),
                secret_digest: std::env::var("CHATTY_CLIENT_SECRET")
                    .ok()
                    .map(|s| snap_oidc::digest(&s)),
            }],
            store: store.clone(),
            crypto: signer,
            accounts: authy::account::Accounts {
                store,
                passport: passport.clone(),
            },
        },
        cookie: cookie.clone(),
    };
    let application = Authy(passport);
    snap_native::run_application_with_http(
        application,
        config,
        snap_native::Web {
            bindings: snap_native::web::identity("account.create"),
            session: Some(snap_native::SessionCarrier {
                origin: origin.origin().ascii_serialization(),
                cookie,
                identify: "identity.fetch",
            }),
        },
        http,
    )
}

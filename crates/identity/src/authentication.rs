//! Identity's credential policy for host dispatch. Hosts supply the clock and
//! prepare the provider's declared data before resolving resident credentials.
use alloc::sync::Arc;
use snap_store::{Error, Transaction};
use snap_transport::bearer::{Authority, Provider, Resolved};

pub struct Authentication {
    provider: Arc<dyn Provider>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}
impl Authentication {
    pub fn new(provider: Arc<dyn Provider>, clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self { provider, clock }
    }
}
impl Authority for Authentication {
    fn identify(
        &self,
        tx: &mut Transaction<'_>,
        bearer: &str,
    ) -> Result<alloc::string::String, Error> {
        self.provider
            .identify(tx, bearer, (self.clock)())
            .map(|principal| principal.identity)
    }

    fn resolve(
        &self,
        tx: &mut Transaction<'_>,
        bearer: Option<&str>,
        identity_required: bool,
    ) -> Result<Resolved, Error> {
        let principal = match bearer {
            Some(bearer) => match self.provider.identify(tx, bearer, (self.clock)()) {
                Ok(principal) => Some(principal),
                // Optional-identity requests may observe an absent or expired
                // credential as anonymous. Store failures never grant that fallback.
                Err(Error::NotFound) if !identity_required => None,
                Err(error) => return Err(error),
            },
            None => None,
        };
        Ok(Resolved {
            actor: principal
                .as_ref()
                .map(|principal| principal.identity.clone()),
            principal,
        })
    }
}

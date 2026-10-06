use serde::Deserialize;
use snap_config::{SecretRef, Secrets};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub tcp: Option<snap_config::Tcp>,
    #[serde(default)]
    pub app_domain: Option<String>,
    #[serde(default)]
    pub auto_approve_domain: String,
    pub clients: Vec<Client>,
    pub cookie_key_ref: Option<SecretRef>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub client_secret_ref: Option<SecretRef>,
}
impl Settings {
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut ids = std::collections::BTreeSet::new();
        for client in &self.clients {
            if client.id.is_empty() || client.name.is_empty() || !ids.insert(&client.id) {
                return Err("Invalid or duplicate OAuth client".into());
            }
            snap_config::validate_origin(&client.origin)?;
            self.client_origin(client)?;
        }
        if !self.auto_approve_domain.is_empty() {
            super::oidc_http::domain_app_origin(&self.auto_approve_domain, "validate")?;
        }
        Ok(())
    }
    pub fn client_origin(&self, client: &Client) -> Result<String, Box<dyn std::error::Error>> {
        match &self.app_domain {
            Some(domain) => super::oidc_http::domain_app_origin(domain, &client.id),
            None => Ok(client.origin.clone()),
        }
    }
    pub fn cookie_key(
        &self,
        secrets: &Secrets,
    ) -> Result<Option<snap_config::Secret>, Box<dyn std::error::Error>> {
        self.cookie_key_ref
            .as_ref()
            .map(|r| secrets.resolve(r).cloned())
            .transpose()
            .map_err(Into::into)
    }
}

//! Password/session vocabulary shared by application consumers and carriers.
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub identity_id: String,
    pub expires_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub identity_id: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum Release {
    Current,
    Others,
    All,
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

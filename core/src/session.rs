//! Host-qualified session identity.
//!
//! Session operations take an explicit key instead of reading an ambient Host
//! scope, so one resolved identity travels with the request and cannot be
//! replaced by a concurrently changed selection. Wire conversion stays at the
//! protocol boundary.

use crate::api::{CoreError, CoreResult};

/// A session identity qualified by the authorized Host that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionKey {
    pub host_id: String,
    pub session_id: String,
}

impl SessionKey {
    /// Build a validated key. Scope authorization is checked by the operation
    /// that uses the key, never inferred from the key itself.
    pub fn new(host_id: impl Into<String>, session_id: impl Into<String>) -> CoreResult<Self> {
        let host_id = host_id.into();
        let session_id = session_id.into();
        if host_id.trim().is_empty() {
            return Err(CoreError::InvalidInput("Host identity missing".into()));
        }
        if session_id.is_empty() {
            return Err(CoreError::InvalidInput("session identity missing".into()));
        }
        Ok(Self {
            host_id,
            session_id,
        })
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl From<whalerein_proto::SessionKey> for SessionKey {
    fn from(value: whalerein_proto::SessionKey) -> Self {
        Self {
            host_id: value.host_id,
            session_id: value.session_id,
        }
    }
}

impl From<&SessionKey> for whalerein_proto::SessionKey {
    fn from(value: &SessionKey) -> Self {
        Self {
            host_id: value.host_id.clone(),
            session_id: value.session_id.clone(),
        }
    }
}

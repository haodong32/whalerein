use super::{Core, CoreOptions};
use crate::api::{MemorySecretStore, SessionKey};
use std::sync::Arc;

/// Build an explicit Host-qualified session key for a fake Access Host.
pub(super) fn key(host: &str, session: &str) -> SessionKey {
    SessionKey::new(host, session).unwrap()
}

pub(super) fn core(dir: &std::path::Path) -> (Core, Arc<MemorySecretStore>) {
    let secrets = Arc::new(MemorySecretStore::new());
    let core = Core::new(CoreOptions {
        data_dir: dir.to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    (core, secrets)
}

use super::{Core, CoreOptions};
use crate::api::MemorySecretStore;
use std::sync::Arc;

pub(super) fn core(dir: &std::path::Path) -> (Core, Arc<MemorySecretStore>) {
    let secrets = Arc::new(MemorySecretStore::new());
    let core = Core::new(CoreOptions {
        data_dir: dir.to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    (core, secrets)
}

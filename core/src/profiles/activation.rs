//! Cross-process activation ownership for profile switching.
//!
//! Every open `Core` holds a shared activation lock for its lifetime. A profile
//! switch takes the exclusive form, so it fails closed while any client process
//! can still send authenticated traffic. OS file locks are released on crash;
//! no daemon or stale-pid cleanup is needed.

use std::fs::File;
use std::path::Path;

use crate::api::{CoreError, CoreResult};

pub(crate) struct ActivationGuard {
    _file: File,
}

impl ActivationGuard {
    pub(crate) fn shared(root: &Path) -> CoreResult<Self> {
        Self::acquire(root, false)
    }

    pub(crate) fn exclusive(root: &Path) -> CoreResult<Self> {
        Self::acquire(root, true)
    }

    fn acquire(root: &Path, exclusive: bool) -> CoreResult<Self> {
        let file = open_lock(root)?;
        let result = if exclusive {
            fs4::FileExt::try_lock(&file)
        } else {
            fs4::FileExt::try_lock_shared(&file)
        };
        match result {
            Ok(()) => Ok(Self { _file: file }),
            Err(fs4::TryLockError::WouldBlock) if exclusive => Err(CoreError::Conflict(
                "another client process is active; stop it before switching profiles".into(),
            )),
            Err(fs4::TryLockError::WouldBlock) => Err(CoreError::Conflict(
                "profile activation is in progress; retry".into(),
            )),
            Err(fs4::TryLockError::Error(_)) => Err(CoreError::Config(
                "profile activation lock could not be acquired".into(),
            )),
        }
    }
}

fn open_lock(root: &Path) -> CoreResult<File> {
    super::ensure_private_dir(root)?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("activation.lock"))
        .map_err(|_| CoreError::Config("profile activation lock could not be opened".into()))
}

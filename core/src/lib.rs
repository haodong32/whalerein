//! WhaleRein client core.
//!
//! The client interface lives in [`api`] and [`conversation`]; other modules
//! are internal implementation details. `SecretStore` is the only injected
//! capability.

mod access;
mod account;
pub mod api;
mod cache;
mod commands;
mod diagnostics;
mod drafts;
mod pairing;
mod profiles;
mod runtime;
mod security;
mod session;
mod sync;

pub(crate) mod util;

pub use api::{
    Core, CoreError, CoreOptions, FileSecretStore, MemorySecretStore, Profiles, SecretError,
    SecretStore,
};

pub mod conversation;

/// Low-level protocol drivers for repository tests; not a client interface.
#[cfg(feature = "protocol-testing")]
pub mod testing {
    pub use crate::access::{DeviceMqttClient, MqttConnectionEvent, MqttSettings};
    pub use crate::security::channel::{
        derive_channel_keys, open_with_key, seal_with_key, ChannelKeys,
    };
    pub use crate::security::{fingerprint, NoiseSession, StaticIdentity};
}

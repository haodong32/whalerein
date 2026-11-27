//! WhaleRein client core.
//!
//! The client interface lives in [`api`], [`conversation`] and [`history`]; other modules
//! are internal implementation details. Clients provide a `SecretStore` and
//! can receive redacted events through a `DiagnosticSink`.

mod access;
mod account;
pub mod api;
mod cache;
mod commands;
mod configuration;
mod connection;
mod diagnostics;
mod drafts;
mod observation;
mod pairing;
#[cfg(test)]
mod profiles;
pub mod route;
mod runtime;
mod security;
mod session;
mod sync;

pub(crate) mod util;

pub use api::{Core, CoreError, MemorySecretStore, SecretError, SecretStore};

#[cfg(test)]
pub use api::{CoreOptions, Profiles};

pub mod attachments;
pub mod conversation;
pub mod history;

/// Low-level protocol drivers for repository tests; not a client interface.
#[cfg(feature = "protocol-testing")]
pub mod testing {
    pub use crate::access::{DeviceMqttClient, MqttConnectionEvent, MqttSettings};
    pub use crate::security::channel::{
        derive_channel_keys, open_with_key, seal_with_key, ChannelKeys,
    };
    pub use crate::security::{fingerprint, NoiseSession, StaticIdentity};
}

//! Runtime lifecycle and in-memory connection state.
//!
//! Connection state and the active secure channel live only here. Nothing in
//! this module writes SQLite.

pub mod channel;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use whalerein_proto::CapabilitySet;

use crate::access::transport::TransportBudget;
use crate::api::{CoreError, CoreResult};
use crate::diagnostics::Metrics;
use crate::security::SecureChannel;
use crate::sync::SyncState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Pairing,
    Connecting,
    Handshaking,
    Syncing,
    Connected,
    Reconnecting,
    Offline,
    Revoked,
    Failed,
}

impl ConnectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Pairing => "pairing",
            Self::Connecting => "connecting",
            Self::Handshaking => "handshaking",
            Self::Syncing => "syncing",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Offline => "offline",
            Self::Revoked => "revoked",
            Self::Failed => "failed",
        }
    }

    pub fn can_transition(self, next: Self) -> bool {
        if self == Self::Revoked {
            return false;
        }
        if self == next {
            return true;
        }
        if next == Self::Revoked {
            return true;
        }
        if matches!(next, Self::Offline | Self::Failed) {
            return true;
        }
        matches!(
            (self, next),
            (Self::Disconnected, Self::Pairing)
                | (Self::Disconnected, Self::Connecting)
                | (Self::Pairing, Self::Handshaking)
                | (Self::Pairing, Self::Disconnected)
                | (Self::Connecting, Self::Handshaking)
                | (Self::Handshaking, Self::Syncing)
                | (Self::Syncing, Self::Connected)
                | (Self::Connected, Self::Reconnecting)
                | (Self::Connected, Self::Disconnected)
                | (Self::Reconnecting, Self::Connecting)
                | (Self::Reconnecting, Self::Connected)
                | (Self::Offline, Self::Connecting)
                | (Self::Offline, Self::Reconnecting)
                | (Self::Failed, Self::Connecting)
                | (Self::Failed, Self::Reconnecting)
                | (Self::Failed, Self::Disconnected)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakePhase {
    Idle,
    Msg1Sent,
    #[cfg(test)]
    Msg2Received,

    Complete,
    Failed,
}

#[derive(Debug)]
struct HostRuntime {
    state: ConnectionState,
    channel: Option<SecureChannel>,
    /// Capabilities advertised by this channel's `ChannelReady`. `None` means
    /// no channel negotiation has completed for the Host.
    capabilities: Option<CapabilitySet>,
    sync: SyncState,
    reconnect_count: u32,
    handshake_phase: HandshakePhase,
    /// Receive limits advertised by this channel's `ChannelReady`.
    receive: Option<TransportBudget>,
    receive_generation: Option<u64>,
}

impl Default for HostRuntime {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            channel: None,
            capabilities: None,
            sync: SyncState::Idle,
            reconnect_count: 0,
            handshake_phase: HandshakePhase::Idle,
            receive: None,
            receive_generation: None,
        }
    }
}

struct RuntimeInner {
    hosts: HashMap<String, HostRuntime>,
    revoked: bool,
    cloud_session: Option<String>,
    recent_errors: VecDeque<String>,
    metrics: Metrics,
    transport: TransportBudget,
    receive_generation: u64,
}

pub struct Runtime {
    inner: Mutex<RuntimeInner>,
    channel_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RuntimeInner {
                hosts: HashMap::new(),
                revoked: false,
                cloud_session: None,
                recent_errors: VecDeque::new(),
                metrics: Metrics::new(),
                transport: TransportBudget::default(),
                receive_generation: 0,
            }),
            channel_locks: Mutex::new(HashMap::new()),
        }
    }

    fn channel_lock(&self, host_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.channel_locks
            .lock()
            .expect("channel locks mutex")
            .entry(host_id.to_owned())
            .or_default()
            .clone()
    }

    pub fn metrics(&self) -> Vec<crate::diagnostics::MetricSample> {
        self.inner.lock().expect("runtime mutex").metrics.snapshot()
    }

    pub fn is_revoked(&self) -> bool {
        self.inner.lock().expect("runtime mutex").revoked
    }

    pub fn state(&self, host_id: &str) -> ConnectionState {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .map(|host| host.state)
            .unwrap_or(ConnectionState::Disconnected)
    }

    pub fn set_state(&self, host_id: &str, next: ConnectionState) -> CoreResult<()> {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.revoked && next != ConnectionState::Revoked {
            return Err(CoreError::Revoked);
        }
        let host = inner.hosts.entry(host_id.to_owned()).or_default();
        if !host.state.can_transition(next) {
            return Err(CoreError::Internal(format!(
                "invalid connection transition {} -> {}",
                host.state.as_str(),
                next.as_str()
            )));
        }
        host.state = next;
        if next == ConnectionState::Revoked {
            host.channel = None;
            host.capabilities = None;
            host.receive = None;
            host.receive_generation = None;
        }
        let metrics = &inner.metrics;
        metrics.record_connection(next.as_str());
        Ok(())
    }

    pub fn channel(&self, host_id: &str) -> Option<SecureChannel> {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .and_then(|host| host.channel.clone())
    }

    pub fn set_channel(&self, host_id: &str, channel: SecureChannel) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.revoked {
            return;
        }
        inner.hosts.entry(host_id.to_owned()).or_default().channel = Some(channel);
    }

    pub fn clear_channel(&self, host_id: &str) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if let Some(host) = inner.hosts.get_mut(host_id) {
            host.channel = None;
            // Capabilities and receive budgets belong to this handshake.
            host.capabilities = None;
            host.receive = None;
            host.receive_generation = None;
        }
    }

    /// Capabilities advertised by the current channel's `ChannelReady`.
    pub fn capabilities(&self, host_id: &str) -> Option<CapabilitySet> {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .and_then(|host| host.capabilities.clone())
    }

    pub fn set_capabilities(&self, host_id: &str, capabilities: CapabilitySet) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.revoked {
            return;
        }
        inner
            .hosts
            .entry(host_id.to_owned())
            .or_default()
            .capabilities = Some(capabilities);
    }

    /// Install the profile resolved from the client configuration. It is
    /// fixed for this process; a configuration change takes effect on restart.
    pub(crate) fn set_transport_budget(&self, budget: TransportBudget) {
        self.inner.lock().expect("runtime mutex").transport = budget;
    }

    pub(crate) fn transport_budget(&self) -> TransportBudget {
        self.inner.lock().expect("runtime mutex").transport
    }

    /// Monotonic receive-advertisement generation. A new handshake never
    /// reuses the previous generation, so a delayed advertisement is fenced.
    pub(crate) fn next_receive_generation(&self) -> u64 {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner.receive_generation = inner.receive_generation.saturating_add(1).max(1);
        inner.receive_generation
    }

    pub(crate) fn set_channel_receive(
        &self,
        host_id: &str,
        budget: TransportBudget,
        generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.revoked {
            return;
        }
        let host = inner.hosts.entry(host_id.to_owned()).or_default();
        host.receive = Some(budget);
        host.receive_generation = Some(generation);
    }

    #[cfg(test)]
    pub(crate) fn host_receive_generation(&self, host_id: &str) -> Option<u64> {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .and_then(|host| host.receive_generation)
    }

    /// The peer's current authenticated receive budget, if a channel has
    /// completed. Senders must not exceed this directional limit.
    pub(crate) fn host_receive(&self, host_id: &str) -> Option<TransportBudget> {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .and_then(|host| host.receive)
    }

    #[cfg(test)]
    pub fn handshake_phase(&self, host_id: &str) -> HandshakePhase {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .map(|host| host.handshake_phase)
            .unwrap_or(HandshakePhase::Idle)
    }

    pub fn set_handshake_phase(&self, host_id: &str, phase: HandshakePhase) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if !inner.revoked {
            inner
                .hosts
                .entry(host_id.to_owned())
                .or_default()
                .handshake_phase = phase;
        }
    }

    #[cfg(test)]
    pub fn sync_state(&self, host_id: &str) -> SyncState {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .map(|host| host.sync)
            .unwrap_or(SyncState::Idle)
    }

    pub fn set_sync_state(&self, host_id: &str, state: SyncState) {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .entry(host_id.to_owned())
            .or_default()
            .sync = state;
    }

    #[cfg(test)]
    pub fn reconnect_count(&self, host_id: &str) -> u32 {
        self.inner
            .lock()
            .expect("runtime mutex")
            .hosts
            .get(host_id)
            .map(|host| host.reconnect_count)
            .unwrap_or(0)
    }

    pub fn reset_reconnect(&self, host_id: &str) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner
            .hosts
            .entry(host_id.to_owned())
            .or_default()
            .reconnect_count = 0;
    }

    /// Schedule the next reconnect attempt and return its backoff delay.
    pub fn next_reconnect_delay(&self, host_id: &str) -> Duration {
        let mut inner = self.inner.lock().expect("runtime mutex");
        let host = inner.hosts.entry(host_id.to_owned()).or_default();
        let attempt = host.reconnect_count;
        host.reconnect_count = host.reconnect_count.saturating_add(1);
        inner.metrics.record_reconnect();
        let jitter: f64 = rand::random();
        crate::access::backoff_delay(attempt, jitter)
    }

    pub fn record_error(&self, code: &str) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner.metrics.record_error_code(code);
        if !inner.recent_errors.iter().any(|existing| existing == code) {
            inner.recent_errors.push_back(code.to_owned());
            while inner.recent_errors.len() > 8 {
                inner.recent_errors.pop_front();
            }
        }
    }

    #[cfg(test)]
    pub fn recent_errors(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("runtime mutex")
            .recent_errors
            .iter()
            .cloned()
            .collect()
    }

    pub(crate) fn bind_cloud_session(&self, session: &str) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.cloud_session.as_deref() != Some(session) {
            inner.hosts.clear();
            inner.cloud_session = Some(session.to_owned());
        }
    }

    pub(crate) fn disconnect_cloud_session_if(&self, expected: Option<&str>) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        if inner.cloud_session.as_deref() == expected {
            inner.cloud_session = None;
            inner.hosts.clear();
        }
    }

    pub(crate) fn disconnect_cloud_session(&self) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner.cloud_session = None;
        inner.hosts.clear();
    }

    /// Allow a newly stored pairing to open channels after a revocation.
    pub fn accept_pairing(&self) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner.revoked = false;
        inner.hosts.clear();
    }

    /// Stop all reconnects and discard every channel.
    pub fn revoke(&self) {
        let mut inner = self.inner.lock().expect("runtime mutex");
        inner.revoked = true;
        for host in inner.hosts.values_mut() {
            host.channel = None;
            host.capabilities = None;
            host.state = ConnectionState::Revoked;
            host.handshake_phase = HandshakePhase::Failed;
        }
        inner
            .metrics
            .record_connection(ConnectionState::Revoked.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecureChannel;

    fn channel() -> SecureChannel {
        SecureChannel::from_handshake(&[1u8; 32], "p", "chan", "h-1", "d-1").unwrap()
    }

    #[test]
    fn allows_only_defined_connection_state_transitions() {
        use ConnectionState::*;
        assert!(Disconnected.can_transition(Pairing));
        assert!(Disconnected.can_transition(Connecting));
        assert!(Connecting.can_transition(Handshaking));
        assert!(Handshaking.can_transition(Syncing));
        assert!(Syncing.can_transition(Connected));
        assert!(Connected.can_transition(Reconnecting));
        assert!(Reconnecting.can_transition(Connecting));
        assert!(Connected.can_transition(Offline));
        assert!(Connected.can_transition(Revoked));
        assert!(!Revoked.can_transition(Connecting));
        assert!(!Disconnected.can_transition(Connected));
        assert!(!Syncing.can_transition(Handshaking));
    }

    #[test]
    fn rejects_connection_state_transitions_outside_the_allowed_graph() {
        let runtime = Runtime::new();
        assert_eq!(runtime.state("h"), ConnectionState::Disconnected);
        runtime.set_state("h", ConnectionState::Connecting).unwrap();
        assert!(matches!(
            runtime.set_state("h", ConnectionState::Connected),
            Err(CoreError::Internal(_))
        ));
        runtime
            .set_state("h", ConnectionState::Handshaking)
            .unwrap();
        runtime.set_state("h", ConnectionState::Syncing).unwrap();
        runtime.set_state("h", ConnectionState::Connected).unwrap();
        assert_eq!(runtime.state("h"), ConnectionState::Connected);
    }

    #[test]
    fn rejects_all_state_transitions_after_revocation() {
        let runtime = Runtime::new();
        runtime.set_state("h", ConnectionState::Connecting).unwrap();
        runtime.revoke();
        assert!(runtime.is_revoked());
        assert_eq!(runtime.state("h"), ConnectionState::Revoked);
        assert!(matches!(
            runtime.set_state("h", ConnectionState::Connecting),
            Err(CoreError::Revoked)
        ));
    }

    #[test]
    fn replaces_channels_on_reconnect_and_clears_them_on_disconnect() {
        let runtime = Runtime::new();
        assert!(runtime.channel("h").is_none());
        let first = channel();
        runtime.set_channel("h", first.clone());
        assert_eq!(
            runtime.channel("h").unwrap().channel_id(),
            first.channel_id()
        );
        runtime.clear_channel("h");
        assert!(runtime.channel("h").is_none());
        runtime.set_channel("h", channel());
        runtime.revoke();
        assert!(runtime.channel("h").is_none());
        // A handshake which finishes after revocation cannot reinstall a key.
        runtime.set_channel("h", channel());
        assert!(runtime.channel("h").is_none());
    }

    #[test]
    fn capabilities_belong_to_the_current_channel() {
        let runtime = Runtime::new();
        assert!(runtime.capabilities("h").is_none());
        let advertised = crate::pairing::default_capabilities();
        runtime.set_capabilities("h", advertised.clone());
        assert_eq!(runtime.capabilities("h"), Some(advertised.clone()));
        runtime.clear_channel("h");
        assert!(runtime.capabilities("h").is_none());

        runtime.set_capabilities("h", advertised.clone());
        runtime.revoke();
        assert!(runtime.capabilities("h").is_none());
        // A handshake finishing after revocation cannot advertise capabilities.
        runtime.set_capabilities("h", advertised);
        assert!(runtime.capabilities("h").is_none());
    }

    #[test]
    fn reconnect_count_and_backoff_are_bounded() {
        let runtime = Runtime::new();
        let first = runtime.next_reconnect_delay("h");
        let second = runtime.next_reconnect_delay("h");
        assert!(first <= crate::access::BACKOFF_CAP);
        assert!(second <= crate::access::BACKOFF_CAP);
        assert_eq!(runtime.reconnect_count("h"), 2);
        runtime.reset_reconnect("h");
        assert_eq!(runtime.reconnect_count("h"), 0);
        assert_eq!(runtime.reconnect_count("missing"), 0);
    }

    #[test]
    fn recent_errors_are_deduplicated_and_bounded() {
        let runtime = Runtime::new();
        for index in 0..12 {
            runtime.record_error(&format!("code_{index}"));
        }
        runtime.record_error("code_11");
        let errors = runtime.recent_errors();
        assert_eq!(errors.len(), 8);
        assert_eq!(errors.last().unwrap(), "code_11");
    }

    #[test]
    fn stores_and_loads_sync_state_and_cursor() {
        let runtime = Runtime::new();
        assert_eq!(runtime.sync_state("h"), SyncState::Idle);
        runtime.set_sync_state("h", SyncState::Synced);
        assert_eq!(runtime.sync_state("h"), SyncState::Synced);
        assert_eq!(runtime.handshake_phase("h"), HandshakePhase::Idle);
        runtime.set_handshake_phase("h", HandshakePhase::Msg2Received);
        assert_eq!(runtime.handshake_phase("h"), HandshakePhase::Msg2Received);
    }

    #[test]
    fn metrics_are_recorded_without_ids() {
        let runtime = Runtime::new();
        runtime.set_state("h", ConnectionState::Connecting).unwrap();
        runtime.record_error("unauthorized");
        runtime.next_reconnect_delay("h");
        let samples = runtime.metrics();
        assert!(!samples.is_empty());
        let json = serde_json::to_string(&samples).unwrap();
        assert!(!json.contains("\"h\""));
    }
}

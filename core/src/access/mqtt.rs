//! MQTT 5 device connection.
//!
//! The device subscribes to its own control event and notice topics at QoS 1
//! and its live preview topic at QoS 0, and publishes its receive advertisement
//! after every successful connection. Connection state lives in `runtime`
//! memory; this module never writes it to disk. A revoked device stops
//! reconnecting when the caller's stop predicate becomes true.

use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use rumqttc::v5::mqttbytes::v5::{Packet, PublishProperties, SubscribeReasonCode};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, ConnectionError, Event, MqttOptions, StateError};
use rumqttc::{TlsConfiguration, Transport};
#[cfg(test)]
use rustls::pki_types::CertificateDer;
use rustls::ClientConfig;
use url::Url;
use whalerein_proto::{EventEnvelope, HostRequest, HostResult, RequestStateNotice};

use super::transport::{self, TransportBudget};
use super::AccessError;
use crate::diagnostics::journal::{DiagnosticKind, Diagnostics};

pub const EVENT_TOPIC_PREFIX: &str = "whalerein/v1/h";
pub const DEFAULT_MQTT_PORT: u16 = 8883;
/// A hard-disconnected device must be observable promptly through the Broker
/// lifecycle notifications; epoch 5 has no Device Will fallback.
pub const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(10);
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttSettings {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub ca_pem: Option<Vec<u8>>,
    /// Platform anchors supplied as data when Rust cannot enumerate the
    /// platform store itself. Ignored in custom mode.
    pub platform_roots: Option<Vec<u8>>,
    pub alpn: Vec<String>,
    pub keep_alive: Duration,
    pub(crate) budget: TransportBudget,
}

impl MqttSettings {
    /// Derive the broker endpoint from the Access URL.
    ///
    /// The official TLS entry shares port 443 and splits HTTPS and MQTT by
    /// ALPN, so `https` reuses the Access host and port with ALPN `mqtt`.
    /// Plain `http` self-hosted deployments default to MQTT on port 1883.
    pub fn from_access_url(access: &Url) -> Result<Self, AccessError> {
        let host = access
            .host_str()
            .ok_or_else(|| AccessError::protocol("Access URL has no host"))?
            .to_owned();
        let tls = access.scheme() == "https";
        Ok(Self {
            host,
            port: if tls {
                access.port_or_known_default().unwrap_or(443)
            } else {
                1883
            },
            tls,
            ca_pem: None,
            platform_roots: None,
            alpn: if tls {
                vec!["mqtt".to_owned()]
            } else {
                Vec::new()
            },
            keep_alive: DEFAULT_KEEP_ALIVE,
            budget: TransportBudget::default(),
        })
    }

    /// Parse `mqtts://host[:port]`, `mqtt://host[:port]`, `ssl://`, or `tcp://`.
    pub fn parse(value: &str) -> Result<Self, AccessError> {
        let url = Url::parse(value).map_err(|_| AccessError::protocol("invalid MQTT URL"))?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(AccessError::protocol("invalid MQTT URL"));
        }
        let tls = match url.scheme() {
            "mqtts" | "ssl" | "tls" => true,
            "mqtt" | "tcp" => false,
            _ => return Err(AccessError::protocol("unsupported MQTT scheme")),
        };
        let host = url
            .host_str()
            .ok_or_else(|| AccessError::protocol("MQTT URL has no host"))?
            .to_owned();
        let port = url
            .port()
            .unwrap_or(if tls { DEFAULT_MQTT_PORT } else { 1883 });
        Ok(Self {
            host,
            port,
            tls,
            ca_pem: None,
            platform_roots: None,
            alpn: Vec::new(),
            keep_alive: DEFAULT_KEEP_ALIVE,
            budget: TransportBudget::default(),
        })
    }

    pub fn with_ca(mut self, ca_pem: Option<Vec<u8>>) -> Self {
        self.ca_pem = ca_pem;
        self
    }

    pub fn with_platform_roots(mut self, platform_roots: Option<Vec<u8>>) -> Self {
        self.platform_roots = platform_roots;
        self
    }

    pub(crate) fn with_transport_budget(mut self, budget: TransportBudget) -> Self {
        self.budget = budget;
        self
    }

    #[cfg(test)]
    pub fn with_alpn(mut self, alpn: Vec<String>) -> Self {
        self.alpn = alpn;
        self
    }

    fn tls_config(&self) -> Result<Arc<ClientConfig>, AccessError> {
        // HTTPS and MQTT share one trust decision: platform roots by default,
        // or exactly the configured CA bundle.
        super::tls::client_config(
            self.ca_pem.as_deref(),
            self.platform_roots.as_deref(),
            &self.alpn,
        )
    }

    #[cfg(test)]
    fn options(&self, device_id: &str, token: &str) -> Result<MqttOptions, AccessError> {
        self.options_with_client(device_id, token, &device_client_id(device_id), None)
    }

    fn options_with_client(
        &self,
        device_id: &str,
        token: &str,
        client_id: &str,
        session_id: Option<&str>,
    ) -> Result<MqttOptions, AccessError> {
        let mut options = MqttOptions::new(client_id, self.host.clone(), self.port);
        let username =
            session_id.map_or_else(|| device_username(device_id), |id| format!("session:{id}"));
        // Epoch 5 removes the presence Will. Access derives disconnect from
        // the Broker-stamped lifecycle notification instead.
        options
            .set_credentials(username, token)
            .set_keep_alive(self.keep_alive)
            .set_clean_start(true)
            .set_max_packet_size(Some(self.budget.packet_bytes));
        if self.tls {
            options.set_transport(Transport::Tls(TlsConfiguration::Rustls(self.tls_config()?)));
        } else {
            options.set_transport(Transport::Tcp);
        }
        Ok(options)
    }
}

pub fn device_client_id(device_id: &str) -> String {
    format!("whalerein-device-{device_id}")
}

pub fn device_username(device_id: &str) -> String {
    format!("device:{device_id}")
}

pub fn device_event_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/event")
}

/// Packet-sized result notice topic for this Device. Notices carry metadata
/// only; complete results are read on demand over HTTPS.
pub fn device_notice_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/notice")
}

/// The device live preview topic. Previews are QoS 0 derived state and never
/// carry control intent.
pub fn device_live_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/live")
}

/// The Device receive-advertisement topic. A Device publishes only here; Access
/// registers the advertisement against the authenticated connection.
pub fn device_ready_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/ready")
}

/// Epoch-5 query ingress. A Device publishes one complete `HostRequest` with
/// QoS 1 and MQTT 5 request/reply properties; Access admits it transiently.
pub fn device_query_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/query")
}

/// Epoch-5 query reply topic. Access publishes one `HostResult` here with the
/// original Correlation Data and the target connection stamp.
pub fn device_reply_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/reply")
}

/// Reserved MQTT 5 user property carrying the Broker-stamped target connection.
pub const TARGET_PROPERTY: &str = "whalerein-target";

/// Exponential backoff with a bounded jitter fraction.
pub fn backoff_delay(attempt: u32, jitter_fraction: f64) -> Duration {
    let exponent = attempt.min(16);
    let base_ms = BACKOFF_BASE.as_millis() as u64;
    let raw = base_ms.saturating_mul(1u64 << exponent);
    let capped = raw.min(BACKOFF_CAP.as_millis() as u64);
    let fraction = jitter_fraction.clamp(0.0, 1.0);
    let jitter = (capped as f64 * 0.2 * fraction) as u64;
    Duration::from_millis(capped.saturating_sub(jitter))
}

/// Logical delivery class of one received envelope. The class is selected by
/// topic; both classes share one authenticated connection and the same
/// encrypted envelope type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryClass {
    /// Ordered, acknowledged, durable control carried at QoS 1.
    Control,
    /// Fire-and-forget derived preview carried at QoS 0.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttConnectionEvent {
    /// A broker session exists. The connection is not usable yet.
    Connected { generation: u64 },
    /// Every subscription is acknowledged; queries may be published.
    Subscribed { generation: u64 },
    /// The broker session ended. Waiters bound to the old generation fail.
    Disconnected,
}

/// One reply received on the device reply topic. Transport identity and MQTT
/// properties stay together so the waiter registry can validate both before a
/// payload is accepted.
#[derive(Debug, Clone)]
pub struct QueryReply {
    pub result: HostResult,
    pub connection_generation: u64,
    /// Broker/Access target stamp, when the received PUBLISH carries one.
    pub target: Option<String>,
}

/// Mutable slot the long-lived connection fills with its publish handle.
///
/// The event loop owns the socket; the connection owner publishes through a
/// clone of the same MQTT client after the subscription is acknowledged.
#[derive(Default)]
pub struct PublisherSlot {
    inner: std::sync::Mutex<Option<Arc<QueryPublisher>>>,
}

impl PublisherSlot {
    fn set(&self, publisher: Arc<QueryPublisher>) {
        *self.inner.lock().expect("mqtt publisher slot") = Some(publisher);
    }

    pub(crate) fn get(&self) -> Option<Arc<QueryPublisher>> {
        self.inner.lock().expect("mqtt publisher slot").clone()
    }
}

/// The one MQTT query publisher for a device connection.
///
/// A query is a complete `HostRequest` published at QoS 1 with MQTT 5
/// Correlation Data, Response Topic and Message Expiry Interval. The publisher
/// never waits for a reply itself: the connection owner registers a waiter
/// before calling it.
pub struct QueryPublisher {
    client: AsyncClient,
    query_topic: String,
    reply_topic: String,
    packet_bytes: u32,
}

impl QueryPublisher {
    /// Positive whole-second Message Expiry Interval for a deadline. `None`
    /// means the deadline already elapsed and the publication must be skipped.
    pub fn remaining_expiry_seconds(expires_at_unix_ms: i64, now_unix_ms: i64) -> Option<u32> {
        let remaining_ms = expires_at_unix_ms.saturating_sub(now_unix_ms);
        if remaining_ms <= 0 {
            return None;
        }
        let seconds = (remaining_ms as u64).div_ceil(1_000);
        Some(u32::try_from(seconds).unwrap_or(u32::MAX))
    }

    /// Publish one query. The caller owns waiter registration, identity checks
    /// and the application deadline; this only encodes the wire properties.
    pub(crate) async fn publish(&self, request: &HostRequest) -> Result<u32, AccessError> {
        let Some(expiry) =
            Self::remaining_expiry_seconds(request.expires_at_unix_ms, crate::util::now_unix_ms())
        else {
            return Err(AccessError::timeout());
        };
        let payload = request.encode_to_vec();
        let properties = PublishProperties {
            response_topic: Some(self.reply_topic.clone()),
            correlation_data: Some(bytes::Bytes::from(request.request_id.clone().into_bytes())),
            message_expiry_interval: Some(expiry),
            ..Default::default()
        };
        let encoded = transport::encoded_publish_bytes(&self.query_topic, 0, payload.len() as u64)
            .saturating_add(32);
        if encoded > u64::from(self.packet_bytes) {
            return Err(AccessError::protocol(
                "query exceeds the MQTT packet budget",
            ));
        }
        self.client
            .publish_with_properties(
                self.query_topic.clone(),
                QoS::AtLeastOnce,
                false,
                payload,
                properties,
            )
            .await
            .map_err(|_| AccessError::transport("MQTT query publish failed"))?;
        Ok(expiry)
    }
}

/// Live device event connection. `run` reconnects with backoff until the stop
/// predicate returns true or the event handler asks to stop.
pub struct DeviceMqttClient {
    settings: MqttSettings,
    host_id: String,
    device_id: String,
    token: String,
    diagnostics: Arc<Diagnostics>,
    /// Per-connection MQTT identity. Two processes for the same device must not
    /// replace each other's broker session; the device id still scopes topics,
    /// credentials and presence.
    client_id: String,
    session_id: Option<String>,
}

impl DeviceMqttClient {
    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn new(
        settings: MqttSettings,
        host_id: impl Into<String>,
        device_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self::with_diagnostics(settings, host_id, device_id, token, Arc::default())
    }

    pub(crate) fn with_diagnostics(
        settings: MqttSettings,
        host_id: impl Into<String>,
        device_id: impl Into<String>,
        token: impl Into<String>,
        diagnostics: Arc<Diagnostics>,
    ) -> Self {
        let device_id = device_id.into();
        Self {
            settings,
            host_id: host_id.into(),
            client_id: format!(
                "{}-{}",
                device_client_id(&device_id),
                crate::util::random_id(6)
            ),
            device_id,
            token: token.into(),
            session_id: None,
            diagnostics,
        }
    }

    pub(crate) fn with_cloud_session(mut self, id: String) -> Self {
        self.client_id = format!("whalerein-session-{id}-{}", crate::util::random_id(6));
        self.session_id = Some(id);
        self
    }

    pub fn topic(&self) -> String {
        device_event_topic(&self.host_id, &self.device_id)
    }

    pub fn notice_topic(&self) -> String {
        device_notice_topic(&self.host_id, &self.device_id)
    }

    pub fn live_topic(&self) -> String {
        device_live_topic(&self.host_id, &self.device_id)
    }

    pub fn ready_topic(&self) -> String {
        device_ready_topic(&self.host_id, &self.device_id)
    }

    pub fn query_topic(&self) -> String {
        device_query_topic(&self.host_id, &self.device_id)
    }

    pub fn reply_topic(&self) -> String {
        device_reply_topic(&self.host_id, &self.device_id)
    }

    /// Run the Device connection until stopped.
    ///
    /// After every successful `CONNACK` the Device publishes its receive
    /// advertisement with the coordinated relay epoch, then subscribes to its
    /// control event and notice topics at QoS 1, its reply topic at QoS 1 and
    /// its live preview topic at QoS 0. `Subscribed` is emitted only after all
    /// four acknowledgements, so a query is never published before its reply
    /// subscription exists. An envelope, notice or reply with another epoch is
    /// a hard protocol mismatch: there is no unframed or dual-class fallback.
    #[allow(clippy::too_many_arguments)]
    pub async fn run<S, E, L, N, R, G>(
        &self,
        mut should_stop: S,
        mut on_event: E,
        mut on_live: L,
        mut on_notice: N,
        mut on_reply: R,
        mut on_state: G,
        publisher: Option<&Arc<PublisherSlot>>,
    ) -> Result<(), AccessError>
    where
        S: FnMut() -> bool,
        E: FnMut(&EventEnvelope) -> bool,
        L: FnMut(&EventEnvelope) -> bool,
        N: FnMut(&RequestStateNotice) -> bool,
        R: FnMut(QueryReply) -> bool,
        G: FnMut(MqttConnectionEvent),
    {
        let options = self.settings.options_with_client(
            &self.device_id,
            &self.token,
            &self.client_id,
            self.session_id.as_deref(),
        )?;
        let (client, mut eventloop) = AsyncClient::new(options, 32);
        let event_topic = self.topic();
        let live_topic = self.live_topic();
        let notice_topic = self.notice_topic();
        let ready_topic = self.ready_topic();
        let reply_topic = self.reply_topic();
        if let Some(publisher) = publisher {
            publisher.set(Arc::new(QueryPublisher {
                client: client.clone(),
                query_topic: self.query_topic(),
                reply_topic: reply_topic.clone(),
                packet_bytes: self.settings.budget.packet_bytes,
            }));
        }
        let mut attempt = 0u32;
        let mut subscribed = false;
        let mut pending_subscriptions = 0usize;
        let mut generation = 0u64;

        loop {
            if should_stop() {
                let _ = client.disconnect().await;
                return Ok(());
            }
            let polled = eventloop.poll().await;
            if should_stop() {
                let _ = client.disconnect().await;
                return Ok(());
            }
            match polled {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    attempt = 0;
                    generation = generation.saturating_add(1);
                    on_state(MqttConnectionEvent::Connected { generation });
                    // Advertise this connection's receive limits before any
                    // Access packet can be routed back to the new session.
                    let advertisement = self
                        .settings
                        .budget
                        .advertisement(generation)
                        .map_err(|_| AccessError::protocol("invalid receive advertisement"))?
                        .encode_to_vec();
                    client
                        .publish(ready_topic.clone(), QoS::AtLeastOnce, false, advertisement)
                        .await
                        .map_err(|_| AccessError::transport("MQTT ready publish failed"))?;
                    if !subscribed {
                        for (topic, qos) in [
                            (event_topic.clone(), QoS::AtLeastOnce),
                            (notice_topic.clone(), QoS::AtLeastOnce),
                            (reply_topic.clone(), QoS::AtLeastOnce),
                            (live_topic.clone(), QoS::AtMostOnce),
                        ] {
                            client
                                .subscribe(topic, qos)
                                .await
                                .map_err(|_| AccessError::transport("MQTT subscribe failed"))?;
                            pending_subscriptions += 1;
                        }
                        subscribed = true;
                    }
                }
                Ok(Event::Incoming(Packet::SubAck(ack))) => {
                    if ack.return_codes.is_empty()
                        || ack
                            .return_codes
                            .iter()
                            .any(|code| !matches!(code, SubscribeReasonCode::Success(_)))
                    {
                        return Err(AccessError::protocol("MQTT subscription rejected"));
                    }
                    // Ready only after control, notice, reply and live
                    // subscriptions are all acknowledged; before that a query
                    // could be published without a reply subscription.
                    if pending_subscriptions > 0 {
                        pending_subscriptions -= 1;
                        if pending_subscriptions == 0 {
                            self.diagnostics.record(DiagnosticKind::MqttSubscribed);
                            on_state(MqttConnectionEvent::Subscribed { generation });
                        }
                    }
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let topic = publish.topic.as_ref();
                    if topic == event_topic.as_bytes() {
                        let envelope = EventEnvelope::decode(publish.payload.as_ref())
                            .map_err(|_| AccessError::protocol("invalid MQTT event envelope"))?;
                        transport::validate_event_representation(&envelope).map_err(|error| {
                            if error.code
                                == Some(whalerein_proto::ErrorCode::ProtocolVersionIncompatible)
                            {
                                transport::protocol_incompatible(
                                    "incoming event does not advertise relay epoch 5",
                                )
                            } else {
                                AccessError::protocol("invalid MQTT event envelope")
                            }
                        })?;
                        if !on_event(&envelope) {
                            let _ = client.disconnect().await;
                            return Ok(());
                        }
                    } else if topic == live_topic.as_bytes() {
                        let envelope = EventEnvelope::decode(publish.payload.as_ref())
                            .map_err(|_| AccessError::protocol("invalid MQTT live envelope"))?;
                        // Live is ephemeral derived state. It must be inline
                        // ciphertext at the coordinated epoch; a reference, a
                        // missing nonce or a foreign epoch is rejected here
                        // instead of entering the control representation path.
                        transport::validate_live_event_representation(&envelope).map_err(
                            |error| {
                                if error.code
                                    == Some(whalerein_proto::ErrorCode::ProtocolVersionIncompatible)
                                {
                                    transport::protocol_incompatible(
                                        "incoming live event does not advertise relay epoch 5",
                                    )
                                } else {
                                    AccessError::protocol("invalid MQTT live envelope")
                                }
                            },
                        )?;
                        if !on_live(&envelope) {
                            let _ = client.disconnect().await;
                            return Ok(());
                        }
                    } else if topic == notice_topic.as_bytes() {
                        let notice = RequestStateNotice::decode(publish.payload.as_ref())
                            .map_err(|_| AccessError::protocol("invalid MQTT result notice"))?;
                        transport::require_relay_epoch(notice.relay_version).map_err(|_| {
                            transport::protocol_incompatible(
                                "incoming result notice does not advertise relay epoch 5",
                            )
                        })?;
                        if notice.request_id.is_empty() {
                            return Err(AccessError::protocol("invalid MQTT result notice"));
                        }
                        if !on_notice(&notice) {
                            let _ = client.disconnect().await;
                            return Ok(());
                        }
                    } else if topic == reply_topic.as_bytes() {
                        let result = HostResult::decode(publish.payload.as_ref())
                            .map_err(|_| AccessError::protocol("invalid MQTT query reply"))?;
                        transport::validate_host_result_representation(&result)?;
                        let properties = publish.properties.unwrap_or_default();
                        let correlation_data = properties
                            .correlation_data
                            .map(|value| value.to_vec())
                            .unwrap_or_default();
                        // Correlation Data must be the request identity. A
                        // reply that fails this is a protocol defect, not a
                        // near-miss for a different request.
                        if correlation_data.is_empty()
                            || correlation_data.as_slice() != result.request_id.as_bytes()
                        {
                            return Err(AccessError::protocol(
                                "MQTT query reply correlation does not match its request",
                            ));
                        }
                        if properties.response_topic.is_some() {
                            return Err(AccessError::protocol(
                                "MQTT query reply must not carry a response topic",
                            ));
                        }
                        if properties.message_expiry_interval == Some(0) {
                            // The broker delivered an already expired copy; it
                            // can never complete a waiter.
                            continue;
                        }
                        let target = properties.user_properties.iter().find_map(|(key, value)| {
                            (key == TARGET_PROPERTY).then(|| value.clone())
                        });
                        let reply = QueryReply {
                            result,
                            connection_generation: generation,
                            target,
                        };
                        if !on_reply(reply) {
                            let _ = client.disconnect().await;
                            return Ok(());
                        }
                    }
                    // A packet on any other topic is a broker/ACL defect, not a
                    // message for this receiver; it is never printed or read.
                }
                Ok(_) => {}
                Err(error) => {
                    let (reason, reason_code, bytes, limit) = connection_failure(&error);
                    subscribed = false;
                    pending_subscriptions = 0;
                    on_state(MqttConnectionEvent::Disconnected);
                    if matches!(error, ConnectionError::ConnectionRefused(
                        rumqttc::v5::mqttbytes::v5::ConnectReturnCode::NotAuthorized
                        | rumqttc::v5::mqttbytes::v5::ConnectReturnCode::BadUserNamePassword
                    )) {
                        return Err(AccessError {
                            code: Some(whalerein_proto::ErrorCode::Unauthorized),
                            ..AccessError::protocol("broker rejected the Device credential")
                        });
                    }
                    let delay = if should_stop() {
                        None
                    } else {
                        Some(backoff_delay(attempt, rand::random()))
                    };
                    self.diagnostics.record(DiagnosticKind::MqttDisconnected {
                        reason,
                        reason_code,
                        bytes,
                        limit,
                        retry_in_ms: delay.map(|delay| delay.as_millis() as u64),
                    });
                    let Some(delay) = delay else {
                        return Ok(());
                    };
                    tokio::time::sleep(delay).await;
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }
}

// Never format ConnectionError: it can contain packets or broker free text.
fn connection_failure(error: &ConnectionError) -> (&'static str, i32, u64, u64) {
    match error {
        ConnectionError::MqttState(StateError::ServerDisconnect { reason_code, .. }) => {
            ("broker_disconnect", *reason_code as i32, 0, 0)
        }
        ConnectionError::MqttState(StateError::OutgoingPacketTooLarge { pkt_size, max }) => {
            ("packet_too_large", 149, *pkt_size as u64, *max as u64)
        }
        ConnectionError::MqttState(StateError::IncomingPacketTooLarge { pkt_size, max }) => {
            ("packet_too_large", 149, *pkt_size as u64, *max as u64)
        }
        ConnectionError::ConnectionRefused(code) => {
            use rumqttc::v5::mqttbytes::v5::ConnectReturnCode;
            // ConnectReturnCode has no wire-valued repr; never cast its ordinal.
            let wire_code = match code {
                ConnectReturnCode::NotAuthorized => 135,
                ConnectReturnCode::BadUserNamePassword => 134,
                ConnectReturnCode::PacketTooLarge => 149,
                ConnectReturnCode::ServerUnavailable | ConnectReturnCode::ServiceUnavailable => 136,
                _ => -1,
            };
            ("connection_refused", wire_code, 0, 0)
        }
        ConnectionError::Io(_) => ("io_error", -1, 0, 0),
        _ => ("transport_closed", -1, 0, 0),
    }
}

/// Parse DER certificates for tests and for private CA loading.
#[cfg(test)]
pub fn parse_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, AccessError> {
    let certs = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AccessError::protocol("invalid certificate bundle"))?;
    if certs.is_empty() {
        return Err(AccessError::protocol("invalid certificate bundle"));
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_device_connections_use_distinct_client_ids() {
        let settings = MqttSettings::parse("mqtt://broker.test:1883").unwrap();
        let first = DeviceMqttClient::new(settings.clone(), "host-1", "device-1", "token");
        let second = DeviceMqttClient::new(settings, "host-1", "device-1", "token");
        assert_ne!(first.client_id, second.client_id);
        assert!(first.client_id.starts_with("whalerein-device-device-1-"));
        // Topics and the device identity are unchanged; only the broker
        // session identity differs so two processes cannot replace each other.
        assert_eq!(first.device_id, second.device_id);
        assert_eq!(first.topic(), second.topic());
        assert_eq!(first.live_topic(), second.live_topic());
    }

    #[test]
    fn connection_diagnostics_keep_numeric_reasons_without_broker_text() {
        use rumqttc::v5::mqttbytes::v5::{ConnectReturnCode, DisconnectReasonCode};
        let broker = ConnectionError::MqttState(StateError::ServerDisconnect {
            reason_code: DisconnectReasonCode::PacketTooLarge,
            reason_string: Some("private broker detail".into()),
        });
        assert_eq!(
            connection_failure(&broker),
            ("broker_disconnect", 149, 0, 0)
        );
        let outgoing = ConnectionError::MqttState(StateError::OutgoingPacketTooLarge {
            pkt_size: 200,
            max: 100,
        });
        assert_eq!(
            connection_failure(&outgoing),
            ("packet_too_large", 149, 200, 100)
        );
        let incoming = ConnectionError::MqttState(StateError::IncomingPacketTooLarge {
            pkt_size: 200,
            max: 100,
        });
        assert_eq!(
            connection_failure(&incoming),
            ("packet_too_large", 149, 200, 100)
        );
        let refused = ConnectionError::ConnectionRefused(ConnectReturnCode::NotAuthorized);
        assert_eq!(
            connection_failure(&refused),
            ("connection_refused", 135, 0, 0)
        );
        for (code, expected) in [
            (ConnectReturnCode::BadUserNamePassword, 134),
            (ConnectReturnCode::PacketTooLarge, 149),
            (ConnectReturnCode::ServerUnavailable, 136),
            (ConnectReturnCode::ServiceUnavailable, 136),
            (ConnectReturnCode::UnspecifiedError, -1),
        ] {
            assert_eq!(
                connection_failure(&ConnectionError::ConnectionRefused(code)).1,
                expected
            );
        }
        assert_eq!(
            connection_failure(&ConnectionError::Io(std::io::Error::from(
                std::io::ErrorKind::ConnectionReset
            )))
            .0,
            "io_error"
        );
        assert_eq!(
            connection_failure(&ConnectionError::RequestsDone).0,
            "transport_closed"
        );
    }

    #[test]
    fn builds_host_topics_and_credentials_from_identity() {
        assert_eq!(
            device_event_topic("h-1", "d-1"),
            "whalerein/v1/h/h-1/d/d-1/event"
        );
        assert_eq!(
            device_live_topic("h-1", "d-1"),
            "whalerein/v1/h/h-1/d/d-1/live"
        );
        assert_eq!(device_client_id("d-1"), "whalerein-device-d-1");
        assert_eq!(device_username("d-1"), "device:d-1");
    }

    #[test]
    fn accepts_supported_mqtt_url_schemes_and_rejects_invalid_urls() {
        let tls = MqttSettings::parse("mqtts://broker.test:8884").unwrap();
        assert_eq!(tls.host, "broker.test");
        assert_eq!(tls.port, 8884);
        assert!(tls.tls);
        let plain = MqttSettings::parse("mqtt://broker.test").unwrap();
        assert_eq!(plain.port, 1883);
        assert!(!plain.tls);
        assert!(MqttSettings::parse("http://broker.test").is_err());
        assert!(MqttSettings::parse("mqtts://user:pw@broker.test").is_err());
    }

    #[test]
    fn derives_default_mqtt_settings_from_access_url() {
        let settings =
            MqttSettings::from_access_url(&Url::parse("https://access.test").unwrap()).unwrap();
        assert_eq!(settings.host, "access.test");
        assert_eq!(settings.port, 443);
        assert!(settings.tls);
        assert_eq!(settings.alpn, vec!["mqtt".to_owned()]);
        let plain =
            MqttSettings::from_access_url(&Url::parse("http://access.test").unwrap()).unwrap();
        assert_eq!(plain.port, 1883);
        assert!(!plain.tls);
        assert!(MqttSettings::from_access_url(&Url::parse("mailto:x@y").unwrap()).is_err());
    }

    #[test]
    fn backoff_is_exponential_capped_and_jittered() {
        let base = backoff_delay(0, 0.0);
        let second = backoff_delay(1, 0.0);
        let capped = backoff_delay(30, 0.0);
        assert_eq!(base, BACKOFF_BASE);
        assert_eq!(second, BACKOFF_BASE * 2);
        assert_eq!(capped, BACKOFF_CAP);
        let jittered = backoff_delay(0, 1.0);
        assert!(jittered <= base);
        assert!(jittered >= base.mul_f64(0.79));
        assert_eq!(backoff_delay(0, 2.0), backoff_delay(0, 1.0));
        assert_eq!(backoff_delay(0, -1.0), base);
    }

    #[test]
    fn invalid_ca_is_rejected() {
        let settings = MqttSettings::parse("mqtts://broker.test").unwrap();
        let error = settings.options("d-1", "token").err();
        // No CA is configured, so options construction only fails on TLS
        // initialization; system mode falls back to portable roots when the
        // platform exposes no store, so it does not depend on a local bundle.
        assert!(error.is_none());
        let settings = settings.with_ca(Some(b"not a pem".to_vec()));
        assert!(settings.options("d-1", "token").is_err());
    }

    #[test]
    fn configures_plain_mqtt_options_and_client_metadata() {
        let settings = MqttSettings::parse("mqtt://broker.test:1884")
            .unwrap()
            .with_alpn(vec!["mqtt".into()]);
        assert!(!settings.tls);
        assert_eq!(settings.alpn, vec!["mqtt".to_owned()]);
        let options = settings.options("d-1", "token").unwrap();
        assert_eq!(options.client_id(), device_client_id("d-1"));
        assert_eq!(
            options.max_packet_size(),
            Some(TransportBudget::default().packet_bytes)
        );
        assert_eq!(options.keep_alive(), DEFAULT_KEEP_ALIVE);
        let login = options.credentials().unwrap();
        assert_eq!(login.username, device_username("d-1"));
        assert_eq!(login.password, "token");
        let client = DeviceMqttClient::new(settings, "h-1", "d-1", "token");
        assert_eq!(client.topic(), "whalerein/v1/h/h-1/d/d-1/event");
    }

    #[test]
    fn configures_a_short_keepalive_and_no_device_will() {
        let settings = MqttSettings::parse("mqtt://broker.test:1884").unwrap();
        assert_eq!(DEFAULT_KEEP_ALIVE, Duration::from_secs(10));
        let options = settings.options("d-1", "secret-token").unwrap();
        assert_eq!(options.keep_alive(), Duration::from_secs(10));
        assert!(options.last_will().is_none(), "epoch 5 has no Device Will");
    }

    #[test]
    fn rejects_empty_certificate_bundle() {
        assert!(parse_certificates(b"not a certificate").is_err());
    }

    #[tokio::test]
    async fn run_stops_after_a_connection_failure() {
        let settings = MqttSettings::parse("mqtt://127.0.0.1:1").unwrap();
        let client = DeviceMqttClient::new(settings, "h-1", "d-1", "token");
        let disconnected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = disconnected.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.run(
                move || flag.load(std::sync::atomic::Ordering::SeqCst),
                |_| true,
                |_| true,
                |_| true,
                |_| true,
                move |event| {
                    if event == MqttConnectionEvent::Disconnected {
                        disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                },
                None,
            ),
        )
        .await
        .expect("run should stop");
        assert!(result.is_ok());
    }

    #[test]
    fn fixture_ca_is_always_parsed_and_builds_tls_options() {
        // Self-signed test certificate. The signing key was discarded.
        let pem = b"-----BEGIN CERTIFICATE-----
MIIBazCCARKgAwIBAgIUCYEJf6AWvrWHzQnqALYWDNrwYfgwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRV2hhbGVSZWluIFRlc3QgQ0EwHhcNMjYxMTE0MDQzMDA2WhcN
MzYxMTExMDQzMDA2WjAcMRowGAYDVQQDDBFXaGFsZVJlaW4gVGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABIDzEtmehsRaR0Vx65NWD4y1kflMTDFblTA8
64kOENsLgnPx+O1z8BV4JzL5+wqRd/gcxvR5WULQOuxjCoDpm4ejMjAwMB0GA1Ud
DgQWBBQ0nEJTr/TR1J9+vLqmx9euDviHNzAPBgNVHRMBAf8EBTADAQH/MAoGCCqG
SM49BAMCA0cAMEQCIEyFy7p0NegnWrzl41pLP89cCp9itTXSeJ8I13iKpm28AiAR
kg3kmwrRgJT2uxIVgusk+cBIKzWjxMwkC7uvy8RsrQ==
-----END CERTIFICATE-----
"
        .to_vec();
        let certs = parse_certificates(&pem).unwrap();
        assert_eq!(certs.len(), 1);
        let settings = MqttSettings::parse("mqtts://localhost:8883")
            .unwrap()
            .with_ca(Some(pem));
        assert!(settings.options("d-1", "token").is_ok());
    }
}

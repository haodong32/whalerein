//! MQTT 5 device connection.
//!
//! The device subscribes to its own event and notice topics and publishes its
//! receive advertisement after every successful connection. Connection state
//! lives in `runtime` memory; this module never writes it to disk. A revoked
//! device stops reconnecting when the caller's stop predicate becomes true.

use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use rumqttc::v5::mqttbytes::v5::{LastWill, LastWillProperties, Packet, SubscribeReasonCode};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, ConnectionError, Event, MqttOptions, StateError};
use rumqttc::{TlsConfiguration, Transport};
#[cfg(test)]
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use url::Url;
use whalerein_proto::{EventEnvelope, RequestStateNotice};

use super::transport::{self, TransportBudget};
use super::AccessError;
use crate::diagnostics::journal::{DiagnosticKind, Diagnostics};

pub const EVENT_TOPIC_PREFIX: &str = "whalerein/v1/h";
pub const PRESENCE_TOPIC_PREFIX: &str = "whalerein/v1/presence/device";
pub const DEFAULT_MQTT_PORT: u16 = 8883;
/// A hard-disconnected device must be observable promptly. Access treats the
/// broker last will as a fallback for missing `$SYS` lifecycle events.
pub const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(10);
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttSettings {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub ca_pem: Option<Vec<u8>>,
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
            alpn: Vec::new(),
            keep_alive: DEFAULT_KEEP_ALIVE,
            budget: TransportBudget::default(),
        })
    }

    pub fn with_ca(mut self, ca_pem: Option<Vec<u8>>) -> Self {
        self.ca_pem = ca_pem;
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
        // Keep MQTT on the same ring provider as HTTPS. A host application
        // may already have installed its own process-wide provider.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let mut roots = RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for certificate in native.certs {
            let _ = roots.add(certificate);
        }
        if let Some(ca) = &self.ca_pem {
            let mut loaded = 0usize;
            for certificate in rustls_pemfile::certs(&mut ca.as_slice()) {
                let certificate = certificate
                    .map_err(|_| AccessError::protocol("invalid MQTT CA certificate"))?;
                roots
                    .add(certificate)
                    .map_err(|_| AccessError::protocol("invalid MQTT CA certificate"))?;
                loaded += 1;
            }
            if loaded == 0 {
                return Err(AccessError::protocol("invalid MQTT CA certificate"));
            }
        }
        let mut config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = self
            .alpn
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
        Ok(Arc::new(config))
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
        if session_id.is_none() {
            options.set_last_will(device_last_will(device_id));
        }
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

/// The Device receive-advertisement topic. A Device publishes only here; Access
/// registers the advertisement against the authenticated connection.
pub fn device_ready_topic(host_id: &str, device_id: &str) -> String {
    format!("{EVENT_TOPIC_PREFIX}/{host_id}/d/{device_id}/ready")
}

/// Device-scoped last-will topic. It carries no host id because one device may
/// be authorized by several hosts; Access fans the event out to each grant.
pub fn device_presence_topic(device_id: &str) -> String {
    format!("{PRESENCE_TOPIC_PREFIX}/{device_id}")
}

#[derive(serde::Serialize)]
struct DeviceWill<'a> {
    event: &'static str,
    device_id: &'a str,
}

/// Compact, content-free last-will payload. It contains only the fixed event
/// name and the device identity; never a token, host id or message content.
fn device_will_payload(device_id: &str) -> Vec<u8> {
    let will = DeviceWill {
        event: "device_offline",
        device_id,
    };
    serde_json::to_vec(&will).unwrap_or_else(|_| Vec::new())
}

/// Non-retained QoS 1 last will. A clean DISCONNECT does not publish it.
fn device_last_will(device_id: &str) -> LastWill {
    LastWill::new(
        device_presence_topic(device_id),
        device_will_payload(device_id),
        QoS::AtLeastOnce,
        false,
        Some(LastWillProperties {
            delay_interval: Some(3),
            payload_format_indicator: None,
            message_expiry_interval: None,
            content_type: None,
            response_topic: None,
            correlation_data: None,
            user_properties: Vec::new(),
        }),
    )
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttConnectionEvent {
    Connected,
    Subscribed,
    Disconnected,
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

    pub fn ready_topic(&self) -> String {
        device_ready_topic(&self.host_id, &self.device_id)
    }

    /// Run the Device connection until stopped. After every successful
    /// `CONNACK` the Device publishes its receive advertisement with the
    /// coordinated relay epoch, then subscribes to its own event and notice
    /// topics. An envelope or notice with another epoch is a hard protocol
    /// mismatch: there is no unframed or fragment fallback.
    pub async fn run<S, E, N, G>(
        &self,
        mut should_stop: S,
        mut on_event: E,
        mut on_notice: N,
        mut on_state: G,
    ) -> Result<(), AccessError>
    where
        S: FnMut() -> bool,
        E: FnMut(&EventEnvelope) -> bool,
        N: FnMut(&RequestStateNotice) -> bool,
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
        let notice_topic = self.notice_topic();
        let ready_topic = self.ready_topic();
        let mut attempt = 0u32;
        let mut subscribed = false;
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
                    on_state(MqttConnectionEvent::Connected);
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
                        for topic in [event_topic.clone(), notice_topic.clone()] {
                            client
                                .subscribe(topic, QoS::AtLeastOnce)
                                .await
                                .map_err(|_| AccessError::transport("MQTT subscribe failed"))?;
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
                    self.diagnostics.record(DiagnosticKind::MqttSubscribed);
                    on_state(MqttConnectionEvent::Subscribed);
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
                                    "incoming event does not advertise relay epoch 3",
                                )
                            } else {
                                AccessError::protocol("invalid MQTT event envelope")
                            }
                        })?;
                        if !on_event(&envelope) {
                            let _ = client.disconnect().await;
                            return Ok(());
                        }
                    } else if topic == notice_topic.as_bytes() {
                        let notice = RequestStateNotice::decode(publish.payload.as_ref())
                            .map_err(|_| AccessError::protocol("invalid MQTT result notice"))?;
                        transport::require_relay_epoch(notice.relay_version).map_err(|_| {
                            transport::protocol_incompatible(
                                "incoming result notice does not advertise relay epoch 3",
                            )
                        })?;
                        if notice.request_id.is_empty() {
                            return Err(AccessError::protocol("invalid MQTT result notice"));
                        }
                        if !on_notice(&notice) {
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
                    on_state(MqttConnectionEvent::Disconnected);
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
            device_presence_topic("d-1"),
            "whalerein/v1/presence/device/d-1"
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
        // initialization; a valid platform trust store yields Ok.
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
    fn configures_a_short_keepalive_and_content_free_device_scoped_will() {
        let settings = MqttSettings::parse("mqtt://broker.test:1884").unwrap();
        assert_eq!(DEFAULT_KEEP_ALIVE, Duration::from_secs(10));
        let options = settings.options("d-1", "secret-token").unwrap();
        assert_eq!(options.keep_alive(), Duration::from_secs(10));

        let will = options.last_will().expect("last will is configured");
        assert_eq!(
            will.topic.as_ref(),
            b"whalerein/v1/presence/device/d-1".as_slice()
        );
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(!will.retain);

        let payload = String::from_utf8(will.message.to_vec()).unwrap();
        assert_eq!(payload, r#"{"event":"device_offline","device_id":"d-1"}"#);
        assert!(!payload.contains("secret-token"));
        assert!(!payload.contains("h-1"));
        assert!(!payload.contains("password"));
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
                move |event| {
                    if event == MqttConnectionEvent::Disconnected {
                        disconnected.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                },
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

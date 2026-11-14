//! MQTT 5 device connection.
//!
//! The device subscribes only to its own event topic. Connection state lives in
//! `runtime` memory; this module never writes it to disk. A revoked device
//! stops reconnecting when the caller's stop predicate becomes true.

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
use whalerein_proto::EventEnvelope;

use super::AccessError;

pub const EVENT_TOPIC_PREFIX: &str = "whalerein/v1/h";
pub const PRESENCE_TOPIC_PREFIX: &str = "whalerein/v1/presence/device";
pub const DEFAULT_MQTT_PORT: u16 = 8883;
/// Matches the deployed broker packet budget, including MQTT framing.
pub const MAX_MQTT_PACKET_BYTES: u32 = super::transport::PACKET_BYTES as u32;
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
        })
    }

    pub fn with_ca(mut self, ca_pem: Option<Vec<u8>>) -> Self {
        self.ca_pem = ca_pem;
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

    fn options(&self, device_id: &str, token: &str) -> Result<MqttOptions, AccessError> {
        let mut options =
            MqttOptions::new(device_client_id(device_id), self.host.clone(), self.port);
        options
            .set_credentials(device_username(device_id), token)
            .set_keep_alive(self.keep_alive)
            .set_clean_start(true)
            .set_max_packet_size(Some(MAX_MQTT_PACKET_BYTES))
            .set_last_will(device_last_will(device_id));
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
    trace_id: String,
}

impl DeviceMqttClient {
    pub fn new(
        settings: MqttSettings,
        host_id: impl Into<String>,
        device_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            settings,
            host_id: host_id.into(),
            device_id: device_id.into(),
            token: token.into(),
            trace_id: crate::util::random_id(16),
        }
    }

    pub fn with_trace_id(mut self, trace_id: &str) -> Self {
        self.trace_id = trace_id.to_owned();
        self
    }

    pub fn topic(&self) -> String {
        device_event_topic(&self.host_id, &self.device_id)
    }

    pub async fn run<S, F, G>(
        &self,
        mut should_stop: S,
        mut on_event: F,
        mut on_state: G,
    ) -> Result<(), AccessError>
    where
        S: FnMut() -> bool,
        F: FnMut(&EventEnvelope) -> bool,
        G: FnMut(MqttConnectionEvent),
    {
        let options = self.settings.options(&self.device_id, &self.token)?;
        let (client, mut eventloop) = AsyncClient::new(options, 32);
        let topic = self.topic();
        let mut attempt = 0u32;
        let mut subscribed = false;
        let mut receiver = super::transport::Receiver::default();
        let mut queued_receipts = 0usize;

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
                    on_state(MqttConnectionEvent::Connected);
                    if !subscribed {
                        client
                            .subscribe(topic.clone(), QoS::AtLeastOnce)
                            .await
                            .map_err(|_| AccessError::transport("MQTT subscribe failed"))?;
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
                    tracing::info!(event = "mqtt_subscribed",trace_id = %self.trace_id,layer = "mqtt");
                    on_state(MqttConnectionEvent::Subscribed);
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    if publish.topic.as_ref() != topic.as_bytes() {
                        continue;
                    }
                    let mut receipt_pending = false;
                    let bytes = if let Some(receipt) = receiver.receive(&topic, &publish.payload)? {
                        let ack_topic = format!(
                            "{EVENT_TOPIC_PREFIX}/{}/d/{}/ack",
                            self.host_id, self.device_id
                        );
                        client
                            .publish(ack_topic, QoS::AtMostOnce, false, receipt.ack)
                            .await
                            .map_err(|_| AccessError::transport("transport receipt failed"))?;
                        queued_receipts += 1;
                        match receipt.logical {
                            Some(bytes) => {
                                receipt_pending = true;
                                bytes
                            }
                            None => continue,
                        }
                    } else {
                        publish.payload.to_vec()
                    };
                    let envelope = EventEnvelope::decode(bytes.as_slice())
                        .map_err(|_| AccessError::protocol("invalid MQTT event envelope"))?;
                    if !on_event(&envelope) {
                        // Drive the queued receipt onto the socket before a
                        // once-only consumer closes its MQTT connection.
                        if receipt_pending {
                            while queued_receipts > 0 {
                                match eventloop.poll().await {
                                    Ok(Event::Outgoing(rumqttc::Outgoing::Publish(_))) => {
                                        queued_receipts = queued_receipts.saturating_sub(1)
                                    }
                                    Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                        let _ = client.disconnect().await;
                        return Ok(());
                    }
                }
                Ok(Event::Outgoing(rumqttc::Outgoing::Publish(_))) => {
                    queued_receipts = queued_receipts.saturating_sub(1);
                }
                Ok(_) => {}
                Err(error) => {
                    let (reason, reason_code, bytes, limit) = connection_failure(&error);
                    tracing::warn!(event = "mqtt_disconnected", trace_id = %self.trace_id, layer = "mqtt", reason, reason_code, bytes, limit);
                    subscribed = false;
                    receiver.clear();
                    queued_receipts = 0;
                    on_state(MqttConnectionEvent::Disconnected);
                    if should_stop() {
                        return Ok(());
                    }
                    let jitter: f64 = rand::random();
                    tokio::time::sleep(backoff_delay(attempt, jitter)).await;
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
        assert_eq!(options.max_packet_size(), Some(MAX_MQTT_PACKET_BYTES));
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

//! Internal Access transports: HTTPS and MQTT 5.

pub mod http;
pub mod mqtt;
pub(crate) mod tls;
pub(crate) mod transport;

pub use http::{AccessClient, AccessError, AccessErrorKind, HostDirectoryPreview};
pub use mqtt::{
    backoff_delay, DeliveryClass, DeviceMqttClient, MqttConnectionEvent, MqttSettings,
    PublisherSlot, QueryReply,
};

#[cfg(test)]
pub use mqtt::BACKOFF_CAP;
#[cfg(any(test, feature = "test-support"))]
pub use mqtt::{
    device_event_topic, device_live_topic, device_notice_topic, device_reply_topic, TARGET_PROPERTY,
};

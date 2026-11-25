//! Device receiver driven by the container-owned Go/JS integration case.
//!
//! The relay carries one complete message per PUBLISH: either the inline
//! ciphertext or an opaque reference that the caller resolves over HTTPS. This
//! driver only asserts that the received event is exactly one valid
//! representation of the coordinated relay epoch.
use std::time::Duration;
use whalerein_core::testing::{DeviceMqttClient, MqttConnectionEvent, MqttSettings};

#[tokio::main]
async fn main() {
    let url = std::env::var("WHALEREIN_TEST_MQTT_URL").unwrap();
    let client = DeviceMqttClient::new(
        MqttSettings::parse(&url).unwrap(),
        "h-e2e",
        "transport-device",
        "device-pass",
    );
    let mut received = 0;
    tokio::time::timeout(
        Duration::from_secs(20),
        client.run(
            || false,
            |event| {
                assert_eq!(event.relay_version, 5);
                let inline = !event.ciphertext.is_empty();
                let referenced = event.payload_reference.is_some();
                assert!(inline != referenced);
                received += 1;
                false
            },
            // The driver observes the control class only; a live preview is
            // derived state and never the committed message it waits for.
            |_| true,
            |_| true,
            |_| true,
            |state| {
                if matches!(state, MqttConnectionEvent::Subscribed { .. }) {
                    println!("transport-device-ready");
                }
            },
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received, 1);
}

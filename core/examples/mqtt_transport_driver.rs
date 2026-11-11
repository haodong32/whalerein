//! Device receiver driven by the container-owned Go/JS integration case.
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
                assert_eq!(event.ciphertext.len(), 2 * 1024 * 1024);
                received += 1;
                false
            },
            |state| {
                if state == MqttConnectionEvent::Subscribed {
                    println!("transport-device-ready");
                }
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(received, 1);
}

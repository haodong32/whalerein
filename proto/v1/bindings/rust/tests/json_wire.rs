use prost::Message;

#[test]
fn preserves_message_fields_across_proto_json_and_binary_encoding(
) -> Result<(), Box<dyn std::error::Error>> {
    let json = include_str!("../../../fixtures/host_request.json");
    let request: whalerein_proto::HostRequest = serde_json::from_str(json)?;
    let binary = request.encode_to_vec();
    assert_eq!(binary, include_bytes!("../../../fixtures/host_request.bin"));
    assert_eq!(request.request_id, "req-1");
    assert_eq!(request.kind, whalerein_proto::RequestKind::Query as i32);
    assert_eq!(request.expires_at_unix_ms, 1_893_456_000_000);
    assert_eq!(request.body, b"hello");
    let roundtrip = serde_json::to_string(&request)?;
    let parsed: whalerein_proto::HostRequest = serde_json::from_str(&roundtrip)?;
    assert_eq!(parsed.encode_to_vec(), binary);
    Ok(())
}

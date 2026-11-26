//! Contract tests for history observation/trace helpers.

use super::*;

#[test]
fn page_request_builders_keep_bounded_defaults() {
    let session = SessionKey::new("host-1", "session-1").unwrap();
    let request = HistoryPageRequest::new(session.clone());
    assert_eq!(request.mode, HistoryMode::Source);
    assert_eq!(request.max_messages, DEFAULT_MAX_MESSAGES);
    assert_eq!(request.max_bytes, DEFAULT_MAX_BYTES);
    assert!(request.continuation.is_none());
    let cached = request.clone().cache();
    assert_eq!(cached.mode, HistoryMode::Cache);
    let token: HistoryContinuation = "abc".parse().unwrap();
    assert_eq!(token.to_string(), "abc");
    assert!(format!("{token:?}").contains("HistoryContinuation"));
    let continued = request.continuation(token.clone());
    assert_eq!(continued.continuation, Some(token));
}

#[test]
fn continuation_parsing_rejects_empty_and_oversized_tokens() {
    assert!("".parse::<HistoryContinuation>().is_err());
    let oversized = "x".repeat(MAX_TOKEN_CHARS + 1);
    assert!(oversized.parse::<HistoryContinuation>().is_err());
    let encoded = serde_json::to_string(&"token".parse::<HistoryContinuation>().unwrap()).unwrap();
    assert_eq!(encoded, "\"token\"");
    let decoded: HistoryContinuation = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.to_string(), "token");
    assert!(serde_json::from_str::<HistoryContinuation>("\"\"").is_err());
}

#[test]
fn message_references_bound_every_field() {
    let session = SessionKey::new("host-1", "session-1").unwrap();
    let reference = MessageReference::new(session.clone(), "m-1", "locator", "projection").unwrap();
    assert_eq!(reference.session(), &session);
    assert_eq!(reference.message_id(), "m-1");
    assert_eq!(reference.source_locator(), "locator");
    assert_eq!(reference.projection_id(), "projection");
    assert!(format!("{reference:?}").contains("MessageReference"));
    assert!(MessageReference::new(session.clone(), "", "locator", "projection").is_err());
    assert!(MessageReference::new(session.clone(), "m", "", "projection").is_err());
    assert!(MessageReference::new(session.clone(), "m", "locator", "").is_err());

    let token = reference.to_token();
    assert!(reference.to_string().starts_with("whr1."));
    let parsed: MessageReference = token.parse().unwrap();
    assert_eq!(parsed, reference);
    assert!(reference.to_string().parse::<MessageReference>().is_ok());
    let encoded = serde_json::to_string(&reference).unwrap();
    let decoded: MessageReference = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, reference);
    assert!(serde_json::from_str::<MessageReference>("\"whr1.bad\"").is_err());
    let committed = CommittedMessageRef {
        session: Some((&session).into()),
        message_id: "m-1".into(),
        source_locator: "locator".into(),
        projection_id: "projection".into(),
    };
    assert_eq!(
        MessageReference::from_committed(&session, &committed).unwrap(),
        reference
    );
    let mut missing = committed.clone();
    missing.session = None;
    assert!(MessageReference::from_committed(&session, &missing).is_err());
    let mut wrong_session = committed.clone();
    wrong_session.session = Some((&SessionKey::new("host-2", "session-2").unwrap()).into());
    assert!(MessageReference::from_committed(&session, &wrong_session).is_err());
    let wired = reference.wire();
    assert_eq!(wired.message_id, "m-1");
    assert_eq!(wired.projection_id, "projection");
    assert_eq!(
        MessageReference::from_committed(&session, &wired).unwrap(),
        reference
    );
    let mut foreign = committed;
    foreign.message_id.clear();
    assert!(MessageReference::from_committed(&session, &foreign).is_err());
}

#[test]
fn continuation_tokens_round_trip_and_reject_tampering() {
    let payload = serde_json::json!({"v": 1, "next": "cursor"});
    let token = encode_token("test-domain", &payload);
    let decoded: serde_json::Value = decode_token("test-domain", &token).unwrap();
    assert_eq!(decoded, payload);
    assert!(decode_token::<serde_json::Value>("other-domain", &token).is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "not-a-token").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr2.e30.00").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr1").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr1.e30").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr1.e30.00.extra").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr1.@@@.00").is_err());
    assert!(decode_token::<serde_json::Value>("test-domain", "whr1.e30.AAAA").is_err());
    assert!(decode_token::<serde_json::Value>(
        "test-domain",
        &format!("whr1.{}.{}", "A".repeat(6000), "00"),
    )
    .is_err());
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not-json");
    let digest = hex::encode(crate::util::sha256(
        [b"test-domain".as_slice(), b"\0", b"not-json".as_slice()]
            .concat()
            .as_slice(),
    ));
    assert!(
        decode_token::<serde_json::Value>("test-domain", &format!("whr1.{body}.{digest}")).is_err()
    );
}

#[test]
fn history_sources_round_trip_through_the_wire_enum() {
    for source in [HistorySource::Online, HistorySource::Cache] {
        assert_eq!(HistorySource::from_wire(source.as_wire()), Some(source));
    }
    assert_eq!(HistorySource::from_wire(0), None);
    assert_eq!(HistorySource::from_wire(99), None);
}

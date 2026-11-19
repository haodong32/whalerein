//! Drift and relation checks for the generated transport profile artifact.

use whalerein_proto::{
    DEFAULT_TRANSPORT_PROFILE, TRANSPORT_PROFILES, TRANSPORT_PROFILES_SCHEMA,
    default_transport_profile_values, transport_profile_values, validate_transport_profile,
    validate_transport_profiles,
};

#[test]
fn generated_profiles_cover_the_packaged_vocabulary() {
    assert_eq!(TRANSPORT_PROFILES_SCHEMA, "whalerein/transport-profiles/v1");
    assert_eq!(DEFAULT_TRANSPORT_PROFILE, "default");
    assert_eq!(TRANSPORT_PROFILES.len(), 3);
    for name in ["default", "compact", "roomy"] {
        assert!(transport_profile_values(name).is_some(), "{name} missing");
    }
    assert!(transport_profile_values("TRANSPORT_PROFILE_DEFAULT").is_none());
}

#[test]
fn default_profile_retains_the_measured_geometry() {
    let default = default_transport_profile_values();
    assert_eq!(default.name, "default");
    assert_eq!(default.packet_bytes, 65_536);
    assert_eq!(default.fragment_payload_bytes, 61_440);
    assert_eq!(default.envelope_bytes, 16 * 1024 * 1024);
}

#[test]
fn every_generated_profile_satisfies_its_relations() {
    validate_transport_profiles().expect("generated profiles are valid");
    for profile in &TRANSPORT_PROFILES {
        validate_transport_profile(profile).expect("profile is valid");
    }
}

#[test]
fn invalid_relations_are_rejected() {
    for profile in TRANSPORT_PROFILES {
        let mut invalid = profile;
        invalid.fragment_count = 1;
        assert!(validate_transport_profile(&invalid).is_err());

        let mut invalid = profile;
        invalid.fragment_payload_bytes = invalid.packet_bytes;
        assert!(validate_transport_profile(&invalid).is_err());

        let mut invalid = profile;
        invalid.reserved_bytes = invalid.envelope_bytes - 1;
        assert!(validate_transport_profile(&invalid).is_err());

        let mut invalid = profile;
        invalid.packet_overhead_bytes = invalid.packet_bytes;
        assert!(validate_transport_profile(&invalid).is_err());
    }
}

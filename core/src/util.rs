//! Small internal helpers shared by the client modules.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time in Unix milliseconds.
pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Cryptographically random bytes from the operating system.
pub(crate) fn random_bytes(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("operating system randomness is available");
    buf
}

/// Random opaque identifier rendered as lowercase hex.
pub(crate) fn random_id(bytes: usize) -> String {
    hex::encode(random_bytes(bytes))
}

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256(data))
}

/// Truncate a string to a byte budget without splitting a UTF-8 scalar.
pub(crate) fn truncate_utf8(value: &mut String, max_bytes: usize) {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

/// Normalize a user-supplied fingerprint: lowercase hex with separators removed.
pub(crate) fn normalize_fingerprint(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Group a lowercase hex fingerprint into 4-character blocks for display.
pub(crate) fn format_fingerprint(hex_value: &str) -> String {
    hex_value
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_are_hex_and_distinct() {
        let a = random_id(16);
        let b = random_id(16);
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn normalizes_fingerprint_hex_and_groups_for_display() {
        assert_eq!(normalize_fingerprint("AB CD:ef-01"), "abcdef01");
        assert_eq!(
            format_fingerprint("abcdef0123456789"),
            "abcd ef01 2345 6789"
        );
        assert_eq!(format_fingerprint("abcd"), "abcd");
    }

    #[test]
    fn unicode_byte_limits_preserve_complete_scalars_at_each_boundary() {
        for (budget, expected) in [
            (0, ""),
            (1, "a"),
            (2, "a"),
            (3, "a"),
            (4, "a"),
            (5, "a🦀"),
            (6, "a🦀z"),
            (99, "a🦀z"),
        ] {
            let mut value = "a🦀z".to_owned();
            truncate_utf8(&mut value, budget);
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn sha256_matches_known_answers_for_empty_short_and_multiblock_inputs() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

//! Encrypted, Host-scoped conversation drafts.
//!
//! Drafts are device-local but still qualified by `(host_id, session_id)`.
//! Text is sealed with the profile cache key and AEAD associated data that
//! binds the draft schema plus both identities, so a row copied to another
//! Host or session scope cannot be opened. Plaintext is never written to
//! SQLite or `SecretStore`, and it is never logged.

use crate::api::{CoreError, CoreResult};
use crate::cache::{Cache, CACHE_NONCE_LEN};
use crate::security::channel::{open_with_key, seal_with_key};

/// Maximum accepted draft size in UTF-8 bytes.
pub(crate) const MAX_DRAFT_BYTES: usize = 256 * 1024;

fn validate_scope(host_id: &str, session_id: &str) -> CoreResult<()> {
    if host_id.trim().is_empty() {
        return Err(CoreError::InvalidInput("Host identity missing".into()));
    }
    if session_id.trim().is_empty() {
        return Err(CoreError::InvalidInput("Session identity missing".into()));
    }
    Ok(())
}

fn draft_aad(host_id: &str, session_id: &str) -> Vec<u8> {
    serde_json::to_vec(&["whalerein/v1/draft", host_id, session_id]).expect("string tuple")
}

fn corrupt() -> CoreError {
    CoreError::Cache("draft payload is corrupt".into())
}

pub(crate) fn save(
    cache: &Cache,
    key: &[u8; 32],
    host_id: &str,
    session_id: &str,
    text: &str,
) -> CoreResult<()> {
    validate_scope(host_id, session_id)?;
    if text.is_empty() {
        cache.delete_draft(host_id, session_id)?;
        return Ok(());
    }
    if text.len() > MAX_DRAFT_BYTES {
        return Err(CoreError::InvalidInput(format!(
            "draft exceeds {MAX_DRAFT_BYTES} bytes"
        )));
    }
    let (nonce, ciphertext) = seal_with_key(key, &draft_aad(host_id, session_id), text.as_bytes())
        .map_err(|_| CoreError::Internal("draft encryption failed".into()))?;
    let mut sealed = Vec::with_capacity(nonce.len() + ciphertext.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    cache.put_draft(host_id, session_id, &sealed)?;
    Ok(())
}

pub(crate) fn load(
    cache: &Cache,
    key: &[u8; 32],
    host_id: &str,
    session_id: &str,
) -> CoreResult<Option<String>> {
    validate_scope(host_id, session_id)?;
    let Some(sealed) = cache.get_draft(host_id, session_id)? else {
        return Ok(None);
    };
    if sealed.len() <= CACHE_NONCE_LEN {
        return Err(corrupt());
    }
    let (nonce, ciphertext) = sealed.split_at(CACHE_NONCE_LEN);
    let plaintext = open_with_key(key, &draft_aad(host_id, session_id), nonce, ciphertext)
        .map_err(|_| corrupt())?;
    String::from_utf8(plaintext)
        .map(Some)
        .map_err(|_| corrupt())
}

pub(crate) fn clear(cache: &Cache, host_id: &str, session_id: &str) -> CoreResult<()> {
    validate_scope(host_id, session_id)?;
    cache.delete_draft(host_id, session_id)?;
    Ok(())
}

pub(crate) fn clear_host(cache: &Cache, host_id: &str) -> CoreResult<()> {
    if host_id.trim().is_empty() {
        return Err(CoreError::InvalidInput("Host identity missing".into()));
    }
    cache.delete_host_drafts(host_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_identities_and_oversized_text_before_storage() {
        let cache = Cache::open_in_memory([1u8; 32]).unwrap();
        let key = [1u8; 32];
        assert!(matches!(
            save(&cache, &key, " ", "s", "text"),
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            save(&cache, &key, "h", "", "text"),
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            load(&cache, &key, "", "s"),
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            clear(&cache, "h", " "),
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            clear_host(&cache, ""),
            Err(CoreError::InvalidInput(_))
        ));
        let oversized = "x".repeat(MAX_DRAFT_BYTES + 1);
        assert!(matches!(
            save(&cache, &key, "h", "s", &oversized),
            Err(CoreError::InvalidInput(_))
        ));
        assert!(cache.get_draft("h", "s").unwrap().is_none());
    }

    #[test]
    fn wrong_key_and_wrong_scope_cannot_open_a_sealed_draft() {
        let cache = Cache::open_in_memory([1u8; 32]).unwrap();
        let key = [2u8; 32];
        save(&cache, &key, "h", "s", "marker").unwrap();
        assert!(load(&cache, &[3u8; 32], "h", "s").is_err());
        assert_eq!(load(&cache, &key, "other", "s").unwrap(), None);
        assert_eq!(load(&cache, &key, "h", "other").unwrap(), None);
        // Moving the sealed row to another scope must fail AEAD authentication.
        let sealed = cache.get_draft("h", "s").unwrap().unwrap();
        cache.put_draft("h", "other", &sealed).unwrap();
        assert!(load(&cache, &key, "h", "other").is_err());
        cache.put_draft("other", "s", &sealed).unwrap();
        assert!(load(&cache, &key, "other", "s").is_err());
        cache.put_draft("h", "short", b"x").unwrap();
        assert!(load(&cache, &key, "h", "short").is_err());
        assert_eq!(
            load(&cache, &key, "h", "s").unwrap().as_deref(),
            Some("marker")
        );
    }
}

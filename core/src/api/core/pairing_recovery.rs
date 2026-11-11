//! The only retained FINISH request lives in SecretStore, never config or logs.
use super::*;
use serde::{Deserialize, Serialize};
use whalerein_proto::{PairingClaimRequest, PairingPhase};

#[derive(Serialize, Deserialize)]
pub(super) struct PendingPairing {
    pub config: PersistedConfig,
    pub finish: PairingClaimRequest,
    pub device_public_key: Vec<u8>,
}

impl Core {
    pub(super) fn load_pending_pairing(
        &self,
        device_id: &str,
    ) -> CoreResult<Option<PendingPairing>> {
        let Some(bytes) = self
            .options
            .secrets
            .load(&keys::pending_pairing(device_id))?
        else {
            return Ok(None);
        };
        let pending: PendingPairing = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Security("invalid pending pairing".into()))?;
        pending.config.validate()?;
        if pending.config.device_id != device_id
            || pending.finish.device_id != device_id
            || pending.finish.phase != PairingPhase::Finish as i32
            || !pending.finish.confirmed
            || pending.finish.device_token.is_empty()
            || pending.device_public_key.len() != 32
        {
            return Err(CoreError::Security(
                "invalid pending pairing binding".into(),
            ));
        }
        Ok(Some(pending))
    }

    pub(super) fn save_pending_pairing(&self, pending: &PendingPairing) -> CoreResult<()> {
        let bytes = serde_json::to_vec(pending)
            .map_err(|_| CoreError::Internal("could not encode pending pairing".into()))?;
        self.options
            .secrets
            .save(&keys::pending_pairing(&pending.config.device_id), &bytes)?;
        Ok(())
    }

    /// Resume a previously confirmed pairing after a lost response or restart.
    /// This reuses the original credential and FINISH, never a new device grant.
    pub async fn resume_pairing(&self) -> CoreResult<PairingOutcome> {
        let guard = self.identity_lock()?;
        let identity = self.device_identity_locked(&guard)?;
        let pending = self
            .load_pending_pairing(&identity.device_id)?
            .ok_or_else(|| CoreError::NotFound("no pending pairing".into()))?;
        self.resume_pending_pairing(pending, &identity).await
    }

    pub(super) async fn resume_pending_pairing(
        &self,
        pending: PendingPairing,
        identity: &crate::security::device::DeviceIdentity,
    ) -> CoreResult<PairingOutcome> {
        if pending.device_public_key != identity.key.public_key() {
            return Err(CoreError::Security(
                "pending pairing belongs to another key".into(),
            ));
        }
        if let Some(current) = self.config.read().expect("config lock").as_ref() {
            if current != &pending.config {
                return Err(CoreError::Conflict(
                    "pending pairing differs from current configuration".into(),
                ));
            }
        }
        let ca = config::ca_pem_from_env();
        let access = AccessClient::with_ca(
            Url::parse(&pending.config.access_url)
                .map_err(|_| CoreError::Config("invalid pending endpoint".into()))?,
            None,
            ca.as_deref(),
        )?
        .with_trace_id(self.diagnostic_id());
        let reply = access.pair_claim(&pending.finish).await?;
        if reply.claim_id != pending.finish.claim_id
            || reply.host_id != pending.config.host_id
            || reply.device_id != identity.device_id
            || reply.device_token != pending.finish.device_token
            || reply.phase != PairingPhase::Finish as i32
            || reply.protocol_version != whalerein_proto::PROTOCOL_VERSION
            || reply.device_public_key != pending.device_public_key
        {
            return Err(CoreError::Security(
                "pairing recovery response mismatch".into(),
            ));
        }
        let outcome = PairingOutcome {
            host_id: pending.config.host_id.clone(),
            device_id: identity.device_id.clone(),
            fingerprint: pending
                .config
                .host_fingerprint
                .clone()
                .ok_or_else(|| CoreError::Security("pending Host pin missing".into()))?,
            access_url: pending.config.access_url.clone(),
        };
        self.finish_pairing_config(pending.config, &pending.finish.device_token)?;
        Ok(outcome)
    }

    pub(super) fn finish_pairing_config(
        &self,
        persisted: PersistedConfig,
        token: &str,
    ) -> CoreResult<()> {
        let device_id = &persisted.device_id;
        load_or_create_cache_key(&self.options.secrets, device_id)?;
        crate::security::store_device_token(&self.options.secrets, device_id, token)?;
        self.config_store().save(&persisted)?;
        *self.config.write().expect("config lock") = Some(persisted.clone());
        self.runtime.accept_pairing();
        let _ = self
            .runtime
            .set_state(&persisted.host_id, ConnectionState::Disconnected);
        // A failed cleanup leaves an idempotent receipt, not an unpaired client.
        let _ = self
            .options
            .secrets
            .delete(&keys::pending_pairing(device_id));
        tracing::info!(trace_id=%self.diagnostic_id(),"pairing completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{FakeHost, MockResponse, MockServer};
    use prost::Message;

    async fn interrupted_pairing() -> (tempfile::TempDir, Core, MockServer, String) {
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = super::super::test_support::core(dir.path());
        let host = Arc::new(Mutex::new(FakeHost::new()));
        let receipt = Arc::new(Mutex::new(None::<(Vec<u8>, Vec<u8>)>));
        let server = MockServer::start(Arc::new(move |request| {
            let finish = serde_json::from_slice::<PairingClaimRequest>(&request.body)
                .ok()
                .filter(|claim| claim.phase == PairingPhase::Finish as i32);
            if let Some(claim) = finish {
                let mut receipt = receipt.lock().unwrap();
                if let Some((proof, body)) = receipt.as_ref() {
                    assert!(
                        proof == &claim.encode_to_vec(),
                        "recovery changed the original FINISH"
                    );
                    return MockResponse {
                        status: 200,
                        body: body.clone(),
                    };
                }
                let reply = host.lock().unwrap().handle(request);
                assert_eq!(reply.status, 200);
                *receipt = Some((claim.encode_to_vec(), reply.body));
                return MockResponse::empty(503);
            }
            host.lock().unwrap().handle(request)
        }))
        .await;
        let encoded: String =
            url::form_urlencoded::byte_serialize(server.url().as_str().as_bytes()).collect();
        let uri = format!("whalerein://pair?v=1&access={encoded}&host=host-1&token=one-time-token");
        assert!(core
            .pair(&uri, |_| PairingDecision::approve())
            .await
            .is_err());
        assert!(!core.is_paired());
        (dir, core, server, uri)
    }

    #[tokio::test]
    async fn restart_resumes_confirmed_finish_and_keeps_secrets_out_of_config() {
        let (dir, core, _server, _uri) = interrupted_pairing().await;
        let restarted = Core::new(core.options.clone()).unwrap();
        let outcome = restarted.resume_pairing().await.unwrap();
        assert!(restarted.is_paired());
        let token = load_device_token(&core.options.secrets, &outcome.device_id)
            .unwrap()
            .unwrap();
        let config = std::fs::read(dir.path().join("config.json")).unwrap();
        assert!(!config
            .windows(token.len())
            .any(|part| part == token.as_bytes()));
        assert!(restarted
            .load_pending_pairing(&outcome.device_id)
            .unwrap()
            .is_none());
        assert!(matches!(
            restarted.resume_pairing().await,
            Err(CoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn another_endpoint_cannot_replace_uncertain_pairing() {
        let (_dir, core, _server, uri) = interrupted_pairing().await;
        let changed = uri.replace("host=host-1", "host=host-2");
        assert!(matches!(
            core.pair(&changed, |_| PairingDecision::approve()).await,
            Err(CoreError::Conflict(_))
        ));
        assert!(matches!(
            core.pair(&uri, |_| PairingDecision::reject()).await,
            Err(CoreError::PairingCancelled)
        ));
        assert!(matches!(
            core.pair(&uri, |_| PairingDecision::expect("ff".repeat(32)))
                .await,
            Err(CoreError::FingerprintMismatch { .. })
        ));
        core.pair(&uri, |_| PairingDecision::approve())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pending_pairing_rejects_changed_device_key_and_corrupt_secret() {
        let (_dir, core, _server, _uri) = interrupted_pairing().await;
        let guard = core.identity_lock().unwrap();
        let identity = core.device_identity_locked(&guard).unwrap();
        let mut pending = core
            .load_pending_pairing(&identity.device_id)
            .unwrap()
            .unwrap();
        pending.device_public_key = vec![0; 32];
        assert!(matches!(
            core.resume_pending_pairing(pending, &identity).await,
            Err(CoreError::Security(_))
        ));
        core.options
            .secrets
            .save(&keys::pending_pairing(&identity.device_id), b"invalid")
            .unwrap();
        assert!(matches!(
            core.load_pending_pairing(&identity.device_id),
            Err(CoreError::Security(_))
        ));
    }
}

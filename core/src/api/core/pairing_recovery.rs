//! The only retained FINISH request lives in SecretStore, never config or logs.
use super::*;
use serde::{Deserialize, Serialize};
use whalerein_proto::{PairingClaimRequest, PairingPhase};

#[derive(Serialize, Deserialize)]
pub(super) struct PendingPairing {
    pub config: PersistedConfig,
    pub finish: PairingClaimRequest,
    pub device_public_key: Vec<u8>,
    #[serde(default)]
    pub admission_marker: Option<String>,
    #[serde(default)]
    pub admission_epoch: Option<String>,
}

pub(super) fn pairing_admission_error(error: std::io::Error) -> CoreError {
    if crate::security::device::retryable_admission_error(&error) {
        CoreError::Unavailable("profile admission lock is busy; retry".into())
    } else {
        CoreError::Config("profile admission lock unavailable".into())
    }
}

impl Core {
    /// Return minimal durable records of pairings that committed but can no longer be recovered.
    pub fn pairing_history(&self) -> CoreResult<Vec<crate::api::PairingHistoryRecord>> {
        let Some(device_id) = crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        )?
        else {
            return Ok(Vec::new());
        };
        let Some(bytes) = self
            .options
            .secrets
            .load(&keys::pairing_history(&device_id))?
        else {
            return Ok(Vec::new());
        };
        serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Security("invalid pairing history".into()))
    }

    pub(super) fn settle_pairing_terminal(
        &self,
        pending: &PendingPairing,
        error: &CoreError,
    ) -> CoreResult<()> {
        if matches!(
            error,
            CoreError::PairingCommittedUnavailable | CoreError::PairingCommittedUnconfirmed
        ) {
            let mut records = self.pairing_history()?;
            let record = crate::api::PairingHistoryRecord {
                claim_id: pending.finish.claim_id.clone(),
                host_id: pending.config.host_id.clone(),
                fingerprint: pending
                    .config
                    .host_fingerprint
                    .clone()
                    .ok_or_else(|| CoreError::Security("pending Host pin missing".into()))?,
                status: if matches!(error, CoreError::PairingCommittedUnavailable) {
                    "committed_unavailable"
                } else {
                    "committed_host_confirmation_unknown"
                }
                .into(),
            };
            if let Some(existing) = records
                .iter_mut()
                .find(|item| item.claim_id == record.claim_id)
            {
                if existing.host_id != record.host_id || existing.fingerprint != record.fingerprint
                {
                    return Err(CoreError::Security(
                        "pairing history identity conflict".into(),
                    ));
                }
                match (existing.status.as_str(), record.status.as_str()) {
                    (
                        "committed_host_confirmation_unknown" | "unconfirmed_revoked",
                        "committed_unavailable",
                    ) => existing.status = record.status,
                    (
                        "committed_host_confirmation_unknown",
                        "committed_host_confirmation_unknown",
                    )
                    | ("committed_unavailable", "committed_unavailable")
                    | ("committed_unavailable", "committed_host_confirmation_unknown")
                    | ("unconfirmed_revoked", "committed_host_confirmation_unknown") => {}
                    _ => return Err(CoreError::Security("invalid pairing history state".into())),
                }
            } else {
                records.push(record);
            }
            let encoded = serde_json::to_vec(&records)
                .map_err(|_| CoreError::Internal("could not encode pairing history".into()))?;
            self.options
                .secrets
                .save(&keys::pairing_history(&pending.config.device_id), &encoded)?;
        }
        if error.pairing_closed() {
            self.options
                .secrets
                .delete(&keys::pending_pairing(&pending.config.device_id))?;
        }
        Ok(())
    }

    fn record_pairing_confirmation_if_observed(&self, pending: &PendingPairing) -> CoreResult<()> {
        let mut records = self.pairing_history()?;
        if let Some(record) = records
            .iter_mut()
            .find(|item| item.claim_id == pending.finish.claim_id)
        {
            if record.host_id != pending.config.host_id
                || pending.config.host_fingerprint.as_ref() != Some(&record.fingerprint)
            {
                return Err(CoreError::Security(
                    "pairing history identity conflict".into(),
                ));
            }
            if record.status == "committed_host_confirmation_unknown" {
                record.status = "confirmed".into();
                let encoded = serde_json::to_vec(&records)
                    .map_err(|_| CoreError::Internal("could not encode pairing history".into()))?;
                self.options
                    .secrets
                    .save(&keys::pairing_history(&pending.config.device_id), &encoded)?;
            }
        }
        Ok(())
    }

    /// Preserve only an opaque identity after confirmed global revocation, then
    /// remove the retained FINISH (which contains a transport credential).
    pub(super) fn redact_pending_pairing_after_revocation(
        &self,
        device_id: &str,
        directory: &std::fs::File,
    ) -> CoreResult<()> {
        // A pairing explicitly started after completed revocation belongs to
        // this same generation; startup cleanup must not erase its FINISH.
        if let (Ok(Some(mut pending)), Ok(marker), Ok(epoch)) = (
            self.load_pending_pairing(device_id),
            super::revoked_commands::marker_in(directory),
            super::revoked_commands::pairing_epoch_in(directory),
        ) {
            if marker
                .as_deref()
                .and_then(super::revoked_commands::confirmed_phase)
                .is_some_and(|phase| matches!(phase, "saved" | "lost"))
                && pending.admission_marker == marker
            {
                if pending.admission_epoch == epoch {
                    return Ok(());
                }
                // An older pending record that already names this exact
                // completed marker was admitted after that revocation. Bind
                // the newly introduced epoch without discarding its FINISH.
                if pending.admission_epoch.is_none() && epoch.is_some() {
                    pending.admission_epoch = epoch;
                    let bytes = serde_json::to_vec(&pending).map_err(|_| {
                        CoreError::Internal("could not encode pending pairing".into())
                    })?;
                    self.options
                        .secrets
                        .save(&keys::pending_pairing(device_id), &bytes)?;
                    return Ok(());
                }
            }
        }
        let retained = (|| -> CoreResult<()> {
            let Some(pending) = self.load_pending_pairing(device_id)? else {
                return Ok(());
            };
            let claim = &pending.finish.claim_id;
            if claim.len() != 32
                || !claim
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err(CoreError::Security(
                    "invalid pending pairing identity".into(),
                ));
            }
            let record = crate::api::PairingHistoryRecord {
                claim_id: claim.clone(),
                host_id: pending.config.host_id.clone(),
                fingerprint: pending
                    .config
                    .host_fingerprint
                    .clone()
                    .ok_or_else(|| CoreError::Security("pending Host pin missing".into()))?,
                status: "unconfirmed_revoked".into(),
            };
            let mut history = self.pairing_history()?;
            if let Some(existing) = history.iter().find(|item| item.claim_id == record.claim_id) {
                if existing.host_id != record.host_id || existing.fingerprint != record.fingerprint
                {
                    return Err(CoreError::Security(
                        "pairing history identity conflict".into(),
                    ));
                }
            } else {
                history.push(record);
            }
            let encoded = serde_json::to_vec(&history)
                .map_err(|_| CoreError::Internal("could not encode pairing history".into()))?;
            self.options
                .secrets
                .save(&keys::pairing_history(device_id), &encoded)?;
            Ok(())
        })();
        // Attempt deletion even if the redacted snapshot cannot be written.
        let deleted = self
            .options
            .secrets
            .delete(&keys::pending_pairing(device_id))
            .map_err(CoreError::from);
        retained.and(deleted)
    }

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

    pub(super) fn save_pending_pairing(
        &self,
        pending: &PendingPairing,
        expected_marker: Option<&str>,
    ) -> CoreResult<()> {
        let guard =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(pairing_admission_error)?;
        let marker = super::revoked_commands::marker_in(guard.directory())?;
        if marker.as_deref() != expected_marker
            || super::revoked_commands::pairing_epoch_in(guard.directory())?
                != pending.admission_epoch
        {
            return Err(CoreError::Revoked);
        }
        if let Some(value) = marker.as_deref() {
            if !matches!(
                super::revoked_commands::confirmed_phase(value),
                Some("saved" | "lost")
            ) || self.config_store().load()?.is_some()
            {
                return Err(CoreError::Revoked);
            }
        }
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
            if current != &pending.config
                && (current.device_id != pending.config.device_id
                    || current.access_url != pending.config.access_url)
            {
                return Err(CoreError::Conflict(
                    "pending pairing differs from current configuration".into(),
                ));
            }
        }
        if let Some(current) = self.current_config() {
            let existing = if current.host_id == pending.config.host_id {
                current.host_fingerprint.as_ref()
            } else {
                current.host_fingerprints.get(&pending.config.host_id)
            };
            if let Some(existing) = existing {
                if pending.config.host_fingerprint.as_ref() != Some(existing) {
                    return Err(CoreError::Security(
                        "pending Host fingerprint changed".into(),
                    ));
                }
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
        let reply = match access.pair_claim(&pending.finish).await {
            Ok(reply) => reply,
            Err(error) => {
                let error = CoreError::from_pairing_access(error);
                self.settle_pairing_terminal(&pending, &error)?;
                return Err(error);
            }
        };
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
        self.record_pairing_confirmation_if_observed(&pending)?;
        self.finish_pairing_config(
            pending.config,
            &pending.finish.device_token,
            pending.admission_marker.as_deref(),
            pending.admission_epoch.as_deref(),
        )?;
        Ok(outcome)
    }

    pub(super) fn finish_pairing_config(
        &self,
        persisted: PersistedConfig,
        token: &str,
        expected_marker: Option<&str>,
        expected_epoch: Option<&str>,
    ) -> CoreResult<()> {
        let _admission =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(pairing_admission_error)?;
        if revoked_commands::marker_in(_admission.directory())?.as_deref() != expected_marker
            || revoked_commands::pairing_epoch_in(_admission.directory())?.as_deref()
                != expected_epoch
        {
            return Err(CoreError::Revoked);
        }
        let original_device_id = persisted.device_id.clone();
        let device_id = &original_device_id;
        if let Some(state) = super::revoked_commands::marker_in(_admission.directory())? {
            if super::revoked_commands::confirmed_phase(&state) == Some("conflict") {
                return Err(CoreError::Unavailable(
                    "revocation cleanup requires manual inspection before pairing".into(),
                ));
            }
            if !state.starts_with("confirmed") || self.config_store().load()?.is_some() {
                return Err(CoreError::Unavailable(
                    "authorization verification or cleanup must finish before pairing".into(),
                ));
            }
            // Finish any interrupted old cleanup before beginning a new grant.
            self.clear_local_pairing_credentials(device_id, _admission.directory(), None)?;
            if super::revoked_commands::confirmed_phase(&state) != Some("saved") {
                let warning = self.options.data_dir.join("revocation-retention-warning");
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                match options.open(warning) {
                    Ok(file) => file.sync_all().map_err(|_| {
                        CoreError::Internal("retention warning could not be saved".into())
                    })?,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        return Err(CoreError::Internal(
                            "retention warning could not be saved".into(),
                        ))
                    }
                }
            }
            super::revoked_commands::remove_marker_in(_admission.directory())?;
        }
        let persisted = if let Some(mut current) = self.config_store().load()? {
            if current == persisted {
                current
            } else {
                if current.device_id != persisted.device_id
                    || current.access_url != persisted.access_url
                {
                    return Err(CoreError::Conflict(
                        "pairing profile changed during admission".into(),
                    ));
                }
                let pin = persisted
                    .host_fingerprint
                    .clone()
                    .ok_or_else(|| CoreError::Security("pending Host pin missing".into()))?;
                if current.host_id == persisted.host_id {
                    if current
                        .host_fingerprint
                        .as_ref()
                        .is_some_and(|existing| existing != &pin)
                    {
                        return Err(CoreError::Security("Host fingerprint changed".into()));
                    }
                    current.host_fingerprint = Some(pin);
                } else {
                    if current
                        .host_fingerprints
                        .get(&persisted.host_id)
                        .is_some_and(|existing| existing != &pin)
                    {
                        return Err(CoreError::Security("Host fingerprint changed".into()));
                    }
                    current
                        .host_fingerprints
                        .insert(persisted.host_id.clone(), pin);
                }
                current
            }
        } else {
            persisted
        };
        load_or_create_cache_key(&self.options.secrets, device_id)?;
        crate::security::store_device_token(&self.options.secrets, device_id, token)?;
        self.config_store().save(&persisted)?;
        #[cfg(feature = "protocol-testing")]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_PAIR_COMMITTED_BARRIER") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
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
                        delay_ms: 0,
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

#[cfg(test)]
mod multi_host_tests {
    use super::*;
    use crate::api::core::test_support::core;
    use crate::api::test_support::FakeAccess;

    #[tokio::test]
    async fn startup_preserves_new_generation_pending_pairing_after_revocation() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        core.revoke().await.unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        assert!(pending.admission_epoch.is_some());
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        assert_eq!(
            reopened
                .load_pending_pairing(&primary.device_id)
                .unwrap()
                .unwrap()
                .finish
                .claim_id,
            pending.finish.claim_id
        );
        access.host.lock().unwrap().drop_pair_finish_once = false;
        assert_eq!(reopened.resume_pairing().await.unwrap().host_id, "host-2");
    }

    #[tokio::test]
    async fn legacy_pending_without_epoch_recovers_when_no_revocation_occurred() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let key = keys::pending_pairing(&primary.device_id);
        let mut value: serde_json::Value =
            serde_json::from_slice(&secrets.load(&key).unwrap().unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("admission_epoch");
        secrets
            .save(&key, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        assert!(!dir.path().join("pairing-epoch").exists());
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        access.host.lock().unwrap().drop_pair_finish_once = false;
        assert_eq!(reopened.resume_pairing().await.unwrap().host_id, "host-2");
    }

    #[tokio::test]
    async fn legacy_pending_bound_to_completed_marker_is_upgraded_without_losing_finish() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        core.revoke().await.unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let mut pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        assert!(pending.admission_marker.is_some());
        pending.admission_epoch = None;
        secrets
            .save(
                &keys::pending_pairing(&primary.device_id),
                &serde_json::to_vec(&pending).unwrap(),
            )
            .unwrap();
        std::fs::remove_file(dir.path().join("pairing-epoch")).unwrap();
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        let migrated = reopened
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        assert_eq!(migrated.finish.claim_id, pending.finish.claim_id);
        assert!(migrated.admission_epoch.is_some());
        access.host.lock().unwrap().drop_pair_finish_once = false;
        assert_eq!(reopened.resume_pairing().await.unwrap().host_id, "host-2");
    }

    #[tokio::test]
    async fn epoch_write_failure_blocks_repair_and_still_erases_credentials() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        std::fs::create_dir(dir.path().join("pairing-epoch")).unwrap();
        assert!(core.revoke().await.is_err());
        assert!(secrets
            .load(&keys::pending_pairing(&primary.device_id))
            .unwrap()
            .is_none());
        assert!(secrets
            .load(&keys::device_token(&primary.device_id))
            .unwrap()
            .is_none());
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn missing_local_token_does_not_discard_an_unproven_pending_pairing() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let claim = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap()
            .finish
            .claim_id;
        secrets
            .delete(&keys::device_token(&primary.device_id))
            .unwrap();
        assert!(matches!(
            core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await,
            Err(CoreError::NotPaired)
        ));
        assert_eq!(
            core.load_pending_pairing(&primary.device_id)
                .unwrap()
                .unwrap()
                .finish
                .claim_id,
            claim
        );
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        assert_eq!(
            reopened
                .load_pending_pairing(&primary.device_id)
                .unwrap()
                .unwrap()
                .finish
                .claim_id,
            claim
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_violation_is_retryable() {
        assert_eq!(
            pairing_admission_error(std::io::Error::from_raw_os_error(32)).code(),
            "unavailable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_epipe_is_not_a_windows_sharing_violation() {
        let error = pairing_admission_error(std::io::Error::from_raw_os_error(32));
        assert_eq!(error.code(), "invalid_configuration");
        let busy = pairing_admission_error(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        assert_eq!(busy.code(), "unavailable");
    }

    #[tokio::test]
    async fn contended_pairing_commit_is_retryable_unavailable_and_preserves_credentials() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        let token = secrets
            .load(&keys::device_token(&primary.device_id))
            .unwrap();
        let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
        let error = core
            .finish_pairing_config(
                pending.config,
                &pending.finish.device_token,
                pending.admission_marker.as_deref(),
                pending.admission_epoch.as_deref(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "unavailable");
        assert_eq!(
            secrets
                .load(&keys::device_token(&primary.device_id))
                .unwrap(),
            token
        );
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_some());
        drop(guard);
    }

    #[tokio::test]
    async fn old_finish_cannot_overwrite_fresh_pair_after_revocation_marker_is_cleared() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let stale = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        core.revoke().await.unwrap();
        access.host.lock().unwrap().drop_pair_finish_once = false;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let current = secrets
            .load(&keys::device_token(&primary.device_id))
            .unwrap()
            .unwrap();
        assert!(core
            .finish_pairing_config(
                stale.config,
                &stale.finish.device_token,
                stale.admission_marker.as_deref(),
                stale.admission_epoch.as_deref(),
            )
            .is_err());
        assert_eq!(
            secrets
                .load(&keys::device_token(&primary.device_id))
                .unwrap()
                .unwrap(),
            current
        );
    }

    #[tokio::test]
    async fn finish_received_before_global_revoke_cannot_restore_credentials_afterward() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        core.revoke().await.unwrap();
        let result = core.finish_pairing_config(
            pending.config,
            &pending.finish.device_token,
            pending.admission_marker.as_deref(),
            pending.admission_epoch.as_deref(),
        );
        assert!(result.is_err());
        assert!(secrets
            .load(&keys::device_token(&primary.device_id))
            .unwrap()
            .is_none());
        assert!(!core.is_paired());
    }

    #[tokio::test]
    async fn in_flight_hello_cannot_write_pending_after_global_revocation() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let first = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let before;
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.pair_hello_delay_ms = 800;
            before = host.pair_hello_calls;
        }
        let scoped = core.clone();
        let uri = access.pairing_uri();
        let in_flight =
            tokio::spawn(async move { scoped.pair(&uri, |_| PairingDecision::approve()).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while access.host.lock().unwrap().pair_hello_calls == before {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        core.revoke().await.unwrap();
        assert!(matches!(in_flight.await.unwrap(), Err(CoreError::Revoked)));
        assert!(secrets
            .load(&keys::pending_pairing(&first.device_id))
            .unwrap()
            .is_none());
        assert!(secrets
            .load(&keys::device_token(&first.device_id))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn global_revocation_redaction_failure_still_erases_token_and_reports_unverified() {
        use crate::security::{MemorySecretStore, SecretError, SecretStore};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct FaultStore {
            inner: MemorySecretStore,
            fail: AtomicBool,
        }
        impl SecretStore for FaultStore {
            fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
                self.inner.load(key)
            }
            fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
                if key.starts_with("whalerein:v1:pairing-history:")
                    && self.fail.load(Ordering::SeqCst)
                {
                    return Err(SecretError::Failed);
                }
                self.inner.save(key, value)
            }
            fn delete(&self, key: &str) -> Result<(), SecretError> {
                self.inner.delete(key)
            }
        }
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(FaultStore {
            inner: MemorySecretStore::new(),
            fail: AtomicBool::new(false),
        });
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        core.sessions("host-1").await.unwrap();
        // Descriptor-anchored SQLite provenance is a Unix-only precondition;
        // the credential-erasure and unverified-retention assertions run on all OSes.
        #[cfg(unix)]
        {
            let guard =
                crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
            let cache = core.cache().unwrap();
            assert!(cache
                .pending_commands_from_existing(guard.directory(), dir.path())
                .is_ok());
        }
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_some());
        secrets.fail.store(true, Ordering::SeqCst);
        let outcome = core.revoke().await.unwrap();
        assert_eq!(
            outcome.retention,
            crate::api::RevocationRetention::Unverified
        );
        assert!(secrets
            .load(&keys::pending_pairing(&primary.device_id))
            .unwrap()
            .is_none());
        assert!(secrets
            .load(&keys::device_token(&primary.device_id))
            .unwrap()
            .is_none());
        drop(core);
        secrets.fail.store(false, Ordering::SeqCst);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        assert!(reopened.revoked_identity_retention_unverified().unwrap());
        assert!(secrets
            .load(&keys::pending_pairing(&primary.device_id))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn global_revocation_removes_pending_pairing_credentials_but_retains_redacted_identity() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        let claim = pending.finish.claim_id.clone();
        let token = pending.finish.device_token.clone();
        core.revoke().await.unwrap();
        assert!(matches!(
            core.save_pending_pairing(&pending, None),
            Err(CoreError::Revoked)
        ));
        assert!(secrets
            .load(&keys::pending_pairing(&primary.device_id))
            .unwrap()
            .is_none());
        let history = core.pairing_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].claim_id, claim);
        assert_eq!(history[0].status, "unconfirmed_revoked");
        assert!(!serde_json::to_string(&history).unwrap().contains(&token));
        core.settle_pairing_terminal(&pending, &CoreError::PairingCommittedUnavailable)
            .unwrap();
        assert_eq!(
            core.pairing_history().unwrap()[0].status,
            "committed_unavailable"
        );
        assert!(secrets
            .load(&keys::pending_pairing(&primary.device_id))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn revoked_primary_changed_pin_is_rejected_before_prompt_or_finish() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        let original = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.identity = crate::security::StaticIdentity::generate();
            host.hide_primary_from_hosts = true;
            host.extra_host = true;
        }
        let before = access.host.lock().unwrap().pair_finish_calls;
        let prompted = std::sync::atomic::AtomicBool::new(false);
        let result = core
            .pair(&access.pairing_uri(), |_| {
                prompted.store(true, std::sync::atomic::Ordering::SeqCst);
                PairingDecision::approve()
            })
            .await;
        assert!(matches!(result, Err(CoreError::Security(_))));
        assert!(!prompted.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(access.host.lock().unwrap().pair_finish_calls, before);
        assert!(core
            .load_pending_pairing(&original.device_id)
            .unwrap()
            .is_none());
        assert_eq!(
            core.confirmed_host_fingerprint("host-1").unwrap(),
            original.fingerprint
        );
    }

    #[tokio::test]
    async fn revoked_primary_repair_preserves_other_host_and_recovers_lost_finish() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let first_key = {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            std::mem::replace(
                &mut host.identity,
                crate::security::StaticIdentity::generate(),
            )
        };
        let second = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-1".into();
            host.identity = first_key;
            host.hide_primary_from_hosts = true;
            host.extra_host = true;
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_some());
        access.host.lock().unwrap().drop_pair_finish_once = false;
        drop(core);
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        let repaired = core.resume_pairing().await.unwrap();
        assert_eq!(repaired.host_id, "host-1");
        assert_eq!(
            core.confirmed_host_fingerprint("host-1").unwrap(),
            primary.fingerprint
        );
        assert_eq!(
            core.confirmed_host_fingerprint("host-2").unwrap(),
            second.fingerprint
        );
    }

    #[tokio::test]
    async fn additional_host_pin_mismatch_is_rejected_before_confirmation() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
        }
        let second = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        access.host.lock().unwrap().identity = crate::security::StaticIdentity::generate();
        let prompted = std::sync::atomic::AtomicBool::new(false);
        let result = core
            .pair(&access.pairing_uri(), |_| {
                prompted.store(true, std::sync::atomic::Ordering::SeqCst);
                PairingDecision::approve()
            })
            .await;
        assert!(matches!(result, Err(CoreError::Security(_))));
        assert!(!prompted.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            core.confirmed_host_fingerprint("host-2").unwrap(),
            second.fingerprint
        );
    }

    #[tokio::test]
    async fn unexpected_http_status_with_fenced_code_keeps_pending_finish() {
        for status in [500, 401] {
            let access = FakeAccess::start().await;
            let dir = tempfile::tempdir().unwrap();
            let (core, _) = core(dir.path());
            let first = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap();
            {
                let mut host = access.host.lock().unwrap();
                host.host_id = "host-2".into();
                host.identity = crate::security::StaticIdentity::generate();
                host.fence_pair_finish_once = true;
                host.fence_pair_finish_status = status;
            }
            let error = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap_err();
            assert_ne!(error.code(), "pairing_fenced");
            assert!(core
                .load_pending_pairing(&first.device_id)
                .unwrap()
                .is_some());
        }
    }

    #[tokio::test]
    async fn committed_pairing_history_save_failure_keeps_pending_and_retry_is_idempotent() {
        use crate::security::{MemorySecretStore, SecretError, SecretStore};
        use std::sync::atomic::{AtomicBool, Ordering};
        struct FailingHistory {
            inner: MemorySecretStore,
            fail: AtomicBool,
        }
        impl SecretStore for FailingHistory {
            fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
                self.inner.load(key)
            }
            fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
                if key.starts_with("whalerein:v1:pairing-history:")
                    && self.fail.load(Ordering::SeqCst)
                {
                    return Err(SecretError::Failed);
                }
                self.inner.save(key, value)
            }
            fn delete(&self, key: &str) -> Result<(), SecretError> {
                self.inner.delete(key)
            }
        }
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(FailingHistory {
            inner: MemorySecretStore::new(),
            fail: AtomicBool::new(false),
        });
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        secrets.fail.store(true, Ordering::SeqCst);
        assert!(core
            .settle_pairing_terminal(&pending, &CoreError::PairingCommittedUnavailable)
            .is_err());
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_some());
        secrets.fail.store(false, Ordering::SeqCst);
        core.settle_pairing_terminal(&pending, &CoreError::PairingCommittedUnavailable)
            .unwrap();
        core.settle_pairing_terminal(&pending, &CoreError::PairingCommittedUnavailable)
            .unwrap();
        assert_eq!(core.pairing_history().unwrap().len(), 1);
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn committed_unavailable_is_recorded_without_secret_before_new_invitation() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let pending = core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .unwrap();
        let claim = pending.finish.claim_id.clone();
        let credential = pending.finish.device_token.clone();
        drop(core);
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.drop_pair_finish_once = false;
            host.committed_pair_finish_status = Some(409);
            host.token = "fresh-invitation".into();
        }
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        assert_eq!(paired.host_id, "host-2");
        let history = core.pairing_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].claim_id, claim);
        assert_eq!(history[0].host_id, "host-2");
        assert_eq!(history[0].status, "committed_unavailable");
        assert!(!serde_json::to_string(&history)
            .unwrap()
            .contains(&credential));
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_none());
        assert_eq!(
            core.confirmed_host_fingerprint("host-1").unwrap(),
            primary.fingerprint
        );
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        assert_eq!(reopened.pairing_history().unwrap(), history);
    }

    #[tokio::test]
    async fn hello_only_exposes_valid_fenced_status_and_never_completed_proof() {
        for (code, status, expected) in [
            (
                whalerein_proto::ErrorCode::PairingFenced,
                409,
                "pairing_fenced",
            ),
            (
                whalerein_proto::ErrorCode::PairingFenced,
                500,
                "security_error",
            ),
            (
                whalerein_proto::ErrorCode::PairingCommittedUnavailable,
                409,
                "security_error",
            ),
            (
                whalerein_proto::ErrorCode::PairingCommittedUnavailable,
                500,
                "security_error",
            ),
            (
                whalerein_proto::ErrorCode::PairingCommittedUnconfirmed,
                409,
                "security_error",
            ),
            (
                whalerein_proto::ErrorCode::PairingCommittedUnconfirmed,
                500,
                "security_error",
            ),
        ] {
            let access = FakeAccess::start().await;
            let dir = tempfile::tempdir().unwrap();
            let (core, _) = core(dir.path());
            let first = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap();
            {
                let mut host = access.host.lock().unwrap();
                host.host_id = "host-2".into();
                host.identity = crate::security::StaticIdentity::generate();
                host.hello_terminal_response = Some((code, status));
            }
            let error = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap_err();
            assert_eq!(error.code(), expected);
            assert!(core
                .load_pending_pairing(&first.device_id)
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn committed_active_without_host_confirmation_records_uncertainty() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.unconfirmed_pair_finish_status = Some(409);
        }
        let error = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap_err();
        assert_eq!(error.code(), "pairing_committed_unconfirmed");
        let history = core.pairing_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "committed_host_confirmation_unknown");
        assert!(core
            .load_pending_pairing(&core.current_config().unwrap().device_id)
            .unwrap()
            .is_some());
        access.host.lock().unwrap().unconfirmed_pair_finish_status = Some(409);
        assert_eq!(
            core.resume_pairing().await.unwrap_err().code(),
            "pairing_committed_unconfirmed"
        );
        assert_eq!(core.pairing_history().unwrap().len(), 1);
        access.host.lock().unwrap().committed_pair_finish_status = Some(409);
        assert_eq!(
            core.resume_pairing().await.unwrap_err().code(),
            "pairing_committed_unavailable"
        );
        assert_eq!(
            core.pairing_history().unwrap()[0].status,
            "committed_unavailable"
        );
        assert!(core
            .load_pending_pairing(&core.current_config().unwrap().device_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unconfirmed_history_advances_after_original_pairing_is_confirmed() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        let primary = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.unconfirmed_pair_finish_status = Some(409);
        }
        assert_eq!(
            core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap_err()
                .code(),
            "pairing_committed_unconfirmed"
        );
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_some());
        let recovered = core.resume_pairing().await.unwrap();
        assert_eq!(recovered.host_id, "host-2");
        assert_eq!(core.pairing_history().unwrap()[0].status, "confirmed");
        assert!(core
            .load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn committed_unavailable_wrong_http_status_retains_pending() {
        for (unconfirmed, status) in [(false, 401), (false, 500), (true, 401), (true, 500)] {
            let access = FakeAccess::start().await;
            let dir = tempfile::tempdir().unwrap();
            let (core, _) = core(dir.path());
            let primary = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap();
            {
                let mut host = access.host.lock().unwrap();
                host.host_id = "host-2".into();
                host.identity = crate::security::StaticIdentity::generate();
                if unconfirmed {
                    host.unconfirmed_pair_finish_status = Some(status);
                } else {
                    host.committed_pair_finish_status = Some(status);
                }
            }
            let error = core
                .pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap_err();
            assert_ne!(error.code(), "pairing_committed_unavailable");
            assert_ne!(error.code(), "pairing_committed_unconfirmed");
            assert!(core
                .load_pending_pairing(&primary.device_id)
                .unwrap()
                .is_some());
            assert!(core.pairing_history().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn new_invitation_keeps_unproven_pending_finish_after_transport_failure() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let first = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        let old = core
            .load_pending_pairing(&first.device_id)
            .unwrap()
            .unwrap()
            .finish
            .claim_id;
        drop(core);
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        access.host.lock().unwrap().token = "new-invitation".into();
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        assert_eq!(
            core.load_pending_pairing(&first.device_id)
                .unwrap()
                .unwrap()
                .finish
                .claim_id,
            old
        );
        access.host.lock().unwrap().drop_pair_finish_once = false;
        assert_eq!(core.resume_pairing().await.unwrap().host_id, "host-2");
    }

    #[tokio::test]
    async fn new_invitation_clears_only_proven_fenced_pending_finish_after_restart() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        drop(core);
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.drop_pair_finish_once = false;
            host.fence_pair_finish_once = true;
            host.token = "new-invitation".into();
        }
        let result = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        assert_eq!(result.host_id, "host-2");
        assert!(core
            .load_pending_pairing(&result.device_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn additional_host_lost_finish_recovers_without_overwriting_primary_pin() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let first = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
            host.drop_pair_finish_once = true;
        }
        assert!(core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .is_err());
        assert_eq!(core.current_config().unwrap().host_id, "host-1");
        access.host.lock().unwrap().drop_pair_finish_once = false;
        drop(core);
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        let second = core.resume_pairing().await.unwrap();
        assert_eq!(second.host_id, "host-2");
        assert_eq!(
            core.confirmed_host_fingerprint("host-1").unwrap(),
            first.fingerprint
        );
        assert_eq!(
            core.confirmed_host_fingerprint("host-2").unwrap(),
            second.fingerprint
        );
    }

    #[tokio::test]
    async fn additional_host_pairing_preserves_primary_and_confirmed_pins_after_restart() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let first = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.host_id = "host-2".into();
            host.identity = crate::security::StaticIdentity::generate();
        }
        let second = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        let config = core.current_config().unwrap();
        assert_eq!(config.host_id, "host-1");
        assert_eq!(
            config.host_fingerprint.as_deref(),
            Some(first.fingerprint.as_str())
        );
        assert_eq!(
            config.host_fingerprints.get("host-2"),
            Some(&second.fingerprint)
        );
        drop(core);
        let reopened = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets,
        })
        .unwrap();
        assert_eq!(
            reopened.confirmed_host_fingerprint("host-1").unwrap(),
            first.fingerprint
        );
        assert_eq!(
            reopened.confirmed_host_fingerprint("host-2").unwrap(),
            second.fingerprint
        );
    }
}

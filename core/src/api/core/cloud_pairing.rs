//! Explicit Cloud pairing with a recoverable operation and authenticated peer.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use whalerein_proto::{ExternalAuthorization, PairingClaimRequest, PairingPhase};

use super::*;
use crate::{
    account::native::AccountClient,
    pairing::{PairingReplay, PairingUri},
    security::device::DeviceIdentity,
};

const ACTIVE_PAIRING: &str = "cloud-pairing-active";

#[derive(Serialize, Deserialize)]
struct CloudPairing {
    id: String,
    host: String,
    device: String,
    device_public_key: [u8; 32],
    ephemeral: [u8; 32],
    device_nonce: String,
    device_token: String,
    marker: Option<String>,
    epoch: Option<String>,
    route: Option<PairingRoute>,
    finish: Option<PairingClaimRequest>,
}

#[derive(Serialize, Deserialize)]
struct PairingRoute {
    access: Url,
    prologue_hash: String,
    host_fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizePairing<'a> {
    operation_id: &'a str,
    device_id: &'a str,
    device_noise_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizedPairing {
    operation_id: String,
    authorization: ExternalAuthorization,
    route: AuthorizedRoute,
    host_noise_public_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizedRoute {
    region: String,
    access_origin: Url,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRevocationState {
    RevocationPending,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct CloudRevocationOutcome {
    pub operation_id: String,
    pub state: CloudRevocationState,
}

impl Core {
    pub fn pending_cloud_pairing(&self) -> CoreResult<Option<(String, String)>> {
        Ok(self
            .load_cloud_pairing::<CloudPairing>(ACTIVE_PAIRING)?
            .map(|operation| (operation.id, operation.host)))
    }

    pub fn has_cloud_pairing_operation(&self, operation_id: &str) -> CoreResult<bool> {
        if !valid_operation_id(operation_id)
            || self
                .instance
                .as_ref()
                .is_none_or(|instance| instance.cloud.is_none())
        {
            return Ok(false);
        }
        let pending = self
            .pending_cloud_pairing()?
            .is_some_and(|(id, _)| id == operation_id);
        Ok(pending
            || self
                .options
                .secrets
                .load(&pairing_receipt(operation_id))?
                .is_some())
    }

    /// Connect an explicitly selected Cloud Host using the verified account.
    pub async fn connect_cloud(&self, host_id: &str) -> CoreResult<PairingOutcome> {
        if host_id.is_empty() {
            return Err(CoreError::InvalidInput("Host identity missing".into()));
        }
        self.require_cloud_account()?;
        self.ensure_instance_state()?;
        let guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        let identity = self.device_identity_locked(&guard)?;
        let client = self.account_client(false).await?;
        if let Some(operation) = self.load_cloud_pairing::<CloudPairing>(ACTIVE_PAIRING)? {
            if operation.host != host_id {
                return Err(CoreError::Conflict(
                    "recover the pending Host connection before connecting another Host".into(),
                ));
            }
            return self.run_cloud_pairing(&client, &identity, operation).await;
        }

        if let Some(outcome) = self.reuse_cloud_grant(host_id).await? {
            self.set_default_host(host_id)?;
            return Ok(outcome);
        }

        let admission =
            crate::security::device::ProfileLock::acquire_admission_waiting(self.data_dir())
                .map_err(pairing_recovery::pairing_admission_error)?;
        let marker = revoked_commands::marker_in(admission.directory())?;
        let epoch = revoked_commands::ensure_pairing_epoch_in(admission.directory())?;
        drop(admission);
        if let Some(state) = marker.as_deref() {
            if !matches!(
                revoked_commands::confirmed_phase(state),
                Some("saved" | "lost")
            ) || self.config_store().load()?.is_some()
            {
                return Err(CoreError::Revoked);
            }
        }
        let existing_token = load_device_token(&self.options.secrets, &identity.device_id)?;
        let operation = CloudPairing {
            id: Self::new_command_id(),
            host: host_id.to_owned(),
            device: identity.device_id.clone(),
            device_public_key: *identity.key.public_key(),
            ephemeral: crate::util::random_bytes(32)
                .try_into()
                .expect("32 random bytes"),
            device_nonce: crate::util::random_id(16),
            device_token: existing_token
                .unwrap_or_else(|| URL_SAFE_NO_PAD.encode(crate::util::random_bytes(32))),
            marker,
            epoch,
            route: None,
            finish: None,
        };
        self.save_cloud_pairing(ACTIVE_PAIRING, &operation)?;
        self.run_cloud_pairing(&client, &identity, operation).await
    }

    /// Resume only the original authorized operation.
    pub async fn recover_cloud_pairing(&self, operation_id: &str) -> CoreResult<PairingOutcome> {
        if !valid_operation_id(operation_id) {
            return Err(CoreError::InvalidInput(
                "invalid pairing operation identity".into(),
            ));
        }
        if !self.has_cloud_pairing_operation(operation_id)? {
            return Err(CoreError::NotFound("pairing operation".into()));
        }
        self.require_cloud_account()?;
        let guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        let client = self.account_client(false).await?;
        if let Some(outcome) =
            self.load_cloud_pairing::<PairingOutcome>(&pairing_receipt(operation_id))?
        {
            self.verify_cloud_receipt(&outcome).await?;
            if self
                .pending_cloud_pairing()?
                .is_some_and(|(id, _)| id == operation_id)
            {
                self.finish_cloud_receipt(&outcome)?;
            }
            return Ok(outcome);
        }
        let operation: CloudPairing = self
            .load_cloud_pairing(ACTIVE_PAIRING)?
            .filter(|operation: &CloudPairing| operation.id == operation_id)
            .ok_or_else(|| CoreError::NotFound("pairing operation".into()))?;
        let identity = self.device_identity_locked(&guard)?;
        self.run_cloud_pairing(&client, &identity, operation).await
    }

    /// Submit or recover the original Cloud revocation intent for this Host and Device.
    pub async fn revoke_cloud(&self, host_id: &str) -> CoreResult<CloudRevocationOutcome> {
        self.require_cloud_account()?;
        let _guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        self.ensure_authorized_host(host_id)?;
        let config = self.require_config()?;
        let client = self.account_client(false).await?;
        let scope = format!("{}\n{host_id}", config.device_id);
        let key = format!("cloud-revoke:{}", crate::util::sha256_hex(scope.as_bytes()));
        let operation_id: String = match self.load_cloud_pairing(&key)? {
            Some(id) => id,
            None => {
                let id = Self::new_command_id();
                self.save_cloud_pairing(&key, &id)?;
                id
            }
        };
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Revoke<'a> {
            operation_id: &'a str,
        }
        let outcome: CloudRevocationOutcome = client
            .post_scoped(
                &["hosts", host_id, "devices", &config.device_id, "revoke"],
                &Revoke {
                    operation_id: &operation_id,
                },
            )
            .await?;
        if outcome.operation_id != operation_id {
            return Err(CoreError::Security(
                "Cloud revocation scope mismatch".into(),
            ));
        }
        // Access is the enforcement authority. Completion does not discard
        // unrelated Host pins or the local Device and operation history.
        if outcome.state == CloudRevocationState::Revoked {
            let _ = self
                .runtime
                .set_state(host_id, ConnectionState::Disconnected);
        }
        self.save_cloud_pairing(&format!("{key}:state"), &outcome.state)?;
        Ok(outcome)
    }

    pub(super) fn local_cloud_revocation_state(&self) -> CoreResult<Option<&'static str>> {
        if self
            .instance
            .as_ref()
            .is_none_or(|instance| instance.cloud.is_none())
        {
            return Ok(None);
        }
        let (Some(config), Some(host_id)) = (self.current_config(), self.default_host()?) else {
            return Ok(None);
        };
        Ok(
            match self.cloud_host_revocation(&config.device_id, &host_id)? {
                Some(CloudRevocationState::Revoked) => Some("revoked"),
                Some(CloudRevocationState::RevocationPending) => Some("revocation_pending"),
                None => None,
            },
        )
    }

    fn cloud_host_revocation(
        &self,
        device_id: &str,
        host_id: &str,
    ) -> CoreResult<Option<CloudRevocationState>> {
        let scope = format!("{device_id}\n{host_id}");
        let key = format!(
            "cloud-revoke:{}:state",
            crate::util::sha256_hex(scope.as_bytes())
        );
        self.load_cloud_pairing(&key)
    }

    pub(super) fn cloud_grants_locally_revoked(&self) -> CoreResult<bool> {
        if self
            .instance
            .as_ref()
            .is_none_or(|instance| instance.cloud.is_none())
        {
            return Ok(false);
        }
        let Some(config) = self.current_config() else {
            return Ok(false);
        };
        for host in std::iter::once(&config.host_id).chain(config.host_fingerprints.keys()) {
            if self.cloud_host_revocation(&config.device_id, host)?
                != Some(CloudRevocationState::Revoked)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn clear_cloud_pairing_material(&self) -> CoreResult<()> {
        if self
            .instance
            .as_ref()
            .is_some_and(|instance| instance.cloud.is_some())
        {
            self.options.secrets.delete(ACTIVE_PAIRING)?;
        }
        Ok(())
    }

    fn require_cloud_account(&self) -> CoreResult<()> {
        if self
            .instance
            .as_ref()
            .is_none_or(|instance| instance.cloud.is_none())
        {
            return Err(CoreError::InvalidInput(
                "Cloud pairing requires the whalerein_cloud service".into(),
            ));
        }
        if self.account_status()?.is_none() {
            return Err(CoreError::Unauthorized);
        }
        Ok(())
    }

    async fn reuse_cloud_grant(&self, host_id: &str) -> CoreResult<Option<PairingOutcome>> {
        let Some(config) = self.current_config() else {
            return Ok(None);
        };
        let pin = if config.host_id == host_id {
            config.host_fingerprint.as_ref()
        } else {
            config.host_fingerprints.get(host_id)
        };
        let Some(pin) = pin else { return Ok(None) };
        let access = self.access_client()?;
        let hosts = self
            .settle(access.list_hosts().await.map_err(CoreError::from))
            .await?;
        let host = hosts
            .into_iter()
            .find(|host| host.host_id == host_id)
            .ok_or_else(|| {
                CoreError::Unavailable("the existing Host grant is unconfirmed".into())
            })?;
        if host.fingerprint != *pin {
            return Err(CoreError::Security("Host fingerprint changed".into()));
        }
        Ok(Some(PairingOutcome {
            operation_id: None,
            host_id: host_id.into(),
            device_id: config.device_id,
            fingerprint: pin.clone(),
            access_url: config.access_url,
        }))
    }

    async fn run_cloud_pairing(
        &self,
        client: &AccountClient,
        identity: &DeviceIdentity,
        mut operation: CloudPairing,
    ) -> CoreResult<PairingOutcome> {
        if identity.device_id != operation.device
            || identity.key.public_key() != &operation.device_public_key
        {
            return Err(CoreError::Conflict(
                "pairing operation belongs to a different Device".into(),
            ));
        }
        if let Some(outcome) =
            self.load_cloud_pairing::<PairingOutcome>(&pairing_receipt(&operation.id))?
        {
            self.verify_cloud_receipt(&outcome).await?;
            self.finish_cloud_receipt(&outcome)?;
            return Ok(outcome);
        }

        let authorization: AuthorizedPairing = match client
            .post_scoped(
                &["hosts", &operation.host, "pairing"],
                &AuthorizePairing {
                    operation_id: &operation.id,
                    device_id: &operation.device,
                    device_noise_key: URL_SAFE_NO_PAD.encode(operation.device_public_key),
                },
            )
            .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                if operation.route.is_none()
                    && matches!(
                        error,
                        CoreError::Unauthorized
                            | CoreError::AuthenticationRequired
                            | CoreError::Revoked
                            | CoreError::NotFound(_)
                            | CoreError::InvalidInput(_)
                            | CoreError::Conflict(_)
                    )
                {
                    // An explicit denial before any regional request cannot have
                    // created a grant. It must not block an unrelated selection.
                    self.options.secrets.delete(ACTIVE_PAIRING)?;
                }
                return Err(error);
            }
        };
        let route = self.validate_cloud_pairing(&operation, &authorization)?;
        let access = AccessClient::with_ca(route.access.clone(), None, self.ca_pem().as_deref())?
            .with_trace_id(self.diagnostic_id());
        let expected_fingerprint = route.host_fingerprint.clone();
        let access_url = route.access.to_string();

        if operation.finish.is_none() {
            if operation.route.is_none() {
                operation.route = Some(route);
                self.save_cloud_pairing(ACTIVE_PAIRING, &operation)?;
            }
            let route = operation.route.as_ref().expect("validated pairing route");
            let uri = PairingUri {
                access_url: route.access.clone(),
                mqtt_url: None,
                host_id: operation.host.clone(),
                token: String::new(),
                token_hash_hex: route.prologue_hash.clone(),
            };
            let (mut session, hello) = PairingInitiator::start_replayable(
                &uri,
                &identity.device_id,
                &identity.key,
                PairingReplay {
                    claim_id: &operation.id,
                    ephemeral: &operation.ephemeral,
                    device_nonce: &operation.device_nonce,
                    device_token: &operation.device_token,
                },
            )?;
            let hello_reply = access
                .pair_cloud_claim(&authorization.authorization, &hello)
                .await
                .map_err(CoreError::from_pairing_hello_access)?;
            let confirmation = session.accept_hello(&hello_reply)?;
            if confirmation.fingerprint != expected_fingerprint {
                return Err(CoreError::Security(
                    "Cloud pairing Host identity mismatch".into(),
                ));
            }
            session.confirm(&PairingDecision {
                approve: true,
                expected_fingerprint: Some(expected_fingerprint.clone()),
            })?;
            operation.finish = Some(session.write_finish()?);
            self.save_cloud_pairing(ACTIVE_PAIRING, &operation)?;
        }

        let finish = operation.finish.as_ref().expect("retained pairing FINISH");
        let reply = access
            .pair_cloud_claim(&authorization.authorization, finish)
            .await
            .map_err(CoreError::from_pairing_access)?;
        if reply.claim_id != operation.id
            || reply.host_id != operation.host
            || reply.device_id != operation.device
            || reply.device_token != finish.device_token
            || reply.phase != PairingPhase::Finish as i32
            || reply.protocol_version != whalerein_proto::PROTOCOL_VERSION
            || reply.device_public_key != operation.device_public_key
        {
            return Err(CoreError::Security(
                "Cloud pairing completion scope mismatch".into(),
            ));
        }
        let mut persisted = PersistedConfig::new(
            access_url.clone(),
            None,
            operation.host.clone(),
            operation.device.clone(),
        );
        persisted.host_fingerprint = Some(expected_fingerprint.clone());
        self.finish_pairing_config(
            persisted,
            &finish.device_token,
            operation.marker.as_deref(),
            operation.epoch.as_deref(),
        )?;
        let outcome = PairingOutcome {
            operation_id: Some(operation.id.clone()),
            host_id: operation.host,
            device_id: operation.device,
            fingerprint: expected_fingerprint,
            access_url,
        };
        self.finish_cloud_receipt(&outcome)?;
        Ok(outcome)
    }

    async fn verify_cloud_receipt(&self, receipt: &PairingOutcome) -> CoreResult<()> {
        let current = self.reuse_cloud_grant(&receipt.host_id).await?;
        if current.is_none_or(|current| {
            current.device_id != receipt.device_id
                || current.fingerprint != receipt.fingerprint
                || current.access_url != receipt.access_url
        }) {
            return Err(CoreError::Revoked);
        }
        Ok(())
    }

    fn finish_cloud_receipt(&self, outcome: &PairingOutcome) -> CoreResult<()> {
        let id = outcome
            .operation_id
            .as_deref()
            .ok_or_else(|| CoreError::Internal("pairing operation ID is unavailable".into()))?;
        self.save_cloud_pairing(&pairing_receipt(id), outcome)?;
        self.set_default_host(&outcome.host_id)?;
        self.options.secrets.delete(ACTIVE_PAIRING)?;
        Ok(())
    }

    fn validate_cloud_pairing(
        &self,
        operation: &CloudPairing,
        response: &AuthorizedPairing,
    ) -> CoreResult<PairingRoute> {
        let invalid = || CoreError::Security("Cloud pairing authorization scope mismatch".into());
        let auth = &response.authorization;
        let instance = self.instance.as_ref().ok_or_else(invalid)?;
        let cloud = instance.cloud.as_ref().ok_or_else(invalid)?;
        let scope = instance
            .manifest()?
            .and_then(|manifest| manifest.account)
            .ok_or(CoreError::Unauthorized)?;
        let host_key = URL_SAFE_NO_PAD
            .decode(&response.host_noise_public_key)
            .map_err(|_| invalid())?;
        let url = &response.route.access_origin;
        if response.operation_id != operation.id
            || auth.operation_id != operation.id
            || auth.host_id != operation.host
            || auth.device_id != operation.device
            || auth.workspace_id != scope.workspace
            || auth.region_id != response.route.region
            || auth.auth_version != "whalerein/v1/ext-auth-v1"
            || auth.purpose != "device_pairing"
            || Url::parse(&auth.issuer).ok()
                != cloud
                    .definition
                    .api_origin
                    .as_deref()
                    .and_then(|value| Url::parse(value).ok())
            || Url::parse(&auth.audience).ok().as_ref() != Some(url)
            || auth.device_public_keys.as_slice() != [operation.device_public_key.to_vec()]
            || host_key.len() != 32
            || !auth.host_public_keys.contains(&host_key)
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(invalid());
        }
        if self
            .current_config()
            .is_some_and(|config| config.access_url != url.as_str())
        {
            return Err(CoreError::Conflict(
                "Host is assigned to another regional authority".into(),
            ));
        }
        let host_key_hex = hex::encode(&host_key);
        let device_key_hex = hex::encode(operation.device_public_key);
        let fields = [
            auth.issuer.as_str(),
            &auth.audience,
            &auth.workspace_id,
            &auth.region_id,
            &auth.enrollment_revision,
            &auth.host_id,
            &auth.device_id,
            &auth.operation_id,
            &host_key_hex,
            &device_key_hex,
        ];
        if fields
            .iter()
            .any(|field| field.is_empty() || field.contains(['\r', '\n']))
        {
            return Err(invalid());
        }
        let route = PairingRoute {
            access: url.clone(),
            prologue_hash: crate::util::sha256_hex(
                format!("whalerein/v1/cloud-pairing\n{}", fields.join("\n")).as_bytes(),
            ),
            host_fingerprint: crate::security::fingerprint(&host_key),
        };
        if operation
            .route
            .as_ref()
            .is_some_and(|retained| retained.prologue_hash != route.prologue_hash)
        {
            return Err(invalid());
        }
        Ok(route)
    }

    fn load_cloud_pairing<T: DeserializeOwned>(&self, key: &str) -> CoreResult<Option<T>> {
        self.options
            .secrets
            .load(key)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| {
                    CoreError::Security("stored Cloud pairing operation is invalid".into())
                })
            })
            .transpose()
    }

    fn save_cloud_pairing<T: Serialize>(&self, key: &str, value: &T) -> CoreResult<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| {
            CoreError::Internal("could not encode the Cloud pairing operation".into())
        })?;
        self.options.secrets.save(key, &bytes)?;
        Ok(())
    }
}

fn pairing_receipt(operation_id: &str) -> String {
    format!("cloud-pairing-receipt:{operation_id}")
}

pub(super) fn valid_operation_id(operation_id: &str) -> bool {
    operation_id.len() == 32 && operation_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::instance::AccountScope;

    #[tokio::test]
    async fn cloud_ready_grant_reuse_checks_scope_without_new_pairing() {
        use crate::api::{test_support::FakeAccess, MemorySecretStore};

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "fixture"),
        )
        .unwrap();
        let core = Core::open(&config, Arc::new(MemorySecretStore::new())).unwrap();
        let access = FakeAccess::start().await;
        let original = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        assert!(original.operation_id.is_some());

        let before = access.host.lock().unwrap().pair_hello_calls;
        let ready = core
            .reuse_cloud_grant(&original.host_id)
            .await
            .unwrap()
            .unwrap();
        assert!(ready.operation_id.is_none());
        assert_eq!(ready.device_id, original.device_id);
        assert_eq!(ready.host_id, original.host_id);
        core.verify_cloud_receipt(&original).await.unwrap();
        let mut wrong_scope = original;
        wrong_scope.device_id = "unrelated-fixture-device".into();
        assert!(matches!(
            core.verify_cloud_receipt(&wrong_scope).await,
            Err(CoreError::Revoked)
        ));
        assert_eq!(access.host.lock().unwrap().pair_hello_calls, before);
        assert_eq!(core.default_host().unwrap().as_deref(), Some("host-1"));
    }

    #[tokio::test]
    async fn cloud_pairing_inspection_and_denial_keep_a_fresh_instance_read_only() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("cloud.toml");
        Core::write_config(
            &file,
            &ClientConfig::new(ClientService::WhaleReinCloud, "instance"),
        )
        .unwrap();
        let backend = Arc::new(crate::api::MemorySecretStore::new());
        let core = Core::open(&file, backend.clone()).unwrap();
        assert!(matches!(
            core.connect_cloud("").await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            core.connect_cloud("host-a").await,
            Err(CoreError::Unauthorized)
        ));
        assert!(core.pending_cloud_pairing().unwrap().is_none());
        assert!(!core.has_cloud_pairing_operation("bad-id").unwrap());
        let unknown = Core::new_command_id();
        assert!(!core.has_cloud_pairing_operation(&unknown).unwrap());
        assert!(matches!(
            core.recover_cloud_pairing(&unknown).await,
            Err(CoreError::NotFound(_))
        ));
        assert!(matches!(
            core.recover_cloud_pairing("bad-id").await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(core.local_revocation_state().unwrap().is_none());
        assert!(core.default_host().unwrap().is_none());
        assert!(!core.configured_data_dir().exists());
        assert!(backend.is_empty());
    }

    #[tokio::test]
    async fn cloud_pending_pairing_survives_restart_and_refuses_recovery_without_account() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::WhaleReinCloud, "instance"),
        )
        .unwrap();
        let backend = Arc::new(crate::api::MemorySecretStore::new());
        let core = Core::open(&path, backend.clone()).unwrap();
        core.ensure_instance_state().unwrap();
        core.set_default_host("previous-fixture-host").unwrap();
        let instance = core.instance.as_ref().unwrap();
        instance
            .bind_account(&AccountScope {
                issuer: instance
                    .cloud
                    .as_ref()
                    .unwrap()
                    .definition
                    .issuer
                    .clone()
                    .unwrap(),
                subject: "fixture-account".into(),
                workspace: "fixture-workspace".into(),
            })
            .unwrap();
        let operation = {
            let lock = core.identity_lock().unwrap();
            let identity = core.device_identity_locked(&lock).unwrap();
            CloudPairing {
                id: Core::new_command_id(),
                host: "pending-fixture-host".into(),
                device: identity.device_id.clone(),
                device_public_key: *identity.key.public_key(),
                ephemeral: crate::util::random_bytes(32).try_into().unwrap(),
                device_nonce: crate::util::random_id(16),
                device_token: URL_SAFE_NO_PAD.encode(crate::util::random_bytes(32)),
                marker: None,
                epoch: None,
                route: None,
                finish: None,
            }
        };
        core.save_cloud_pairing(ACTIVE_PAIRING, &operation).unwrap();
        drop(core);

        let core = Core::open(&path, backend).unwrap();
        assert!(matches!(
            core.recover_cloud_pairing(&Core::new_command_id()).await,
            Err(CoreError::NotFound(_))
        ));
        assert!(matches!(
            core.recover_cloud_pairing(&operation.id).await,
            Err(CoreError::Unauthorized)
        ));
        assert_eq!(
            core.pending_cloud_pairing().unwrap(),
            Some((operation.id, operation.host))
        );
        assert_eq!(
            core.default_host().unwrap().as_deref(),
            Some("previous-fixture-host")
        );
        assert!(load_device_token(&core.options.secrets, &operation.device)
            .unwrap()
            .is_none());
    }

    #[test]
    fn cloud_pairing_scope_and_replay_check_the_exact_operation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cloud.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::WhaleReinCloud, "cloud"),
        )
        .unwrap();
        let secrets = Arc::new(crate::api::MemorySecretStore::new());
        let core = Core::open(&path, secrets.clone()).unwrap();
        core.ensure_instance_state().unwrap();
        let instance = core.instance.as_ref().unwrap();
        let definition = &instance.cloud.as_ref().unwrap().definition;
        instance
            .bind_account(&AccountScope {
                issuer: definition.issuer.clone().unwrap(),
                subject: "subject-fixture".into(),
                workspace: "workspace-fixture".into(),
            })
            .unwrap();
        let grant = core.identity_lock().unwrap();
        let identity = core.device_identity_locked(&grant).unwrap();
        let operation = CloudPairing {
            id: Core::new_command_id(),
            host: "host-fixture".into(),
            device: identity.device_id.clone(),
            device_public_key: *identity.key.public_key(),
            ephemeral: crate::util::random_bytes(32).try_into().unwrap(),
            device_nonce: crate::util::random_id(16),
            device_token: URL_SAFE_NO_PAD.encode(crate::util::random_bytes(32)),
            marker: None,
            epoch: None,
            route: None,
            finish: None,
        };
        let host_key = crate::security::StaticIdentity::generate();
        let authorized = AuthorizedPairing {
            operation_id: operation.id.clone(),
            authorization: ExternalAuthorization {
                auth_version: "whalerein/v1/ext-auth-v1".into(),
                issuer: definition.api_origin.clone().unwrap(),
                audience: "https://access.fixture.test".into(),
                purpose: "device_pairing".into(),
                workspace_id: "workspace-fixture".into(),
                region_id: "region-fixture".into(),
                operation_id: operation.id.clone(),
                enrollment_revision: "revision-fixture".into(),
                host_id: operation.host.clone(),
                host_public_keys: vec![host_key.public_key().to_vec(), vec![1_u8; 32]],
                device_id: operation.device.clone(),
                device_public_keys: vec![operation.device_public_key.to_vec()],
                ..ExternalAuthorization::default()
            },
            route: AuthorizedRoute {
                region: "region-fixture".into(),
                access_origin: Url::parse("https://access.fixture.test").unwrap(),
            },
            host_noise_public_key: URL_SAFE_NO_PAD.encode(host_key.public_key()),
        };
        let route = core
            .validate_cloud_pairing(&operation, &authorized)
            .unwrap();
        assert_eq!(
            route.host_fingerprint,
            crate::security::fingerprint(host_key.public_key())
        );
        let uri = PairingUri {
            access_url: route.access,
            mqtt_url: None,
            host_id: operation.host.clone(),
            token: String::new(),
            token_hash_hex: route.prologue_hash,
        };
        let start = || {
            PairingInitiator::start_replayable(
                &uri,
                &identity.device_id,
                &identity.key,
                PairingReplay {
                    claim_id: &operation.id,
                    ephemeral: &operation.ephemeral,
                    device_nonce: &operation.device_nonce,
                    device_token: &operation.device_token,
                },
            )
            .unwrap()
            .1
        };
        let first = start();
        let retry = start();
        assert!(first == retry, "recovery must reproduce the exact HELLO");
        let mut invalid = authorized;
        invalid.authorization.workspace_id = "different-workspace".into();
        assert!(matches!(
            core.validate_cloud_pairing(&operation, &invalid),
            Err(CoreError::Security(_))
        ));
    }
}

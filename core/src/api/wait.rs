//! Read-only native wait slices with an overall monotonic deadline.
use super::*;
use crate::api::types::{WaitInput, WaitObservation, WaitReason, WaitState};
use tokio::time::{timeout, timeout_at, Instant};
use whalerein_proto::{
    request_payload, response_payload, CancelSessionWait, InputKind, ReadWaitState,
    SessionWaitState, WaitReason as WireReason, WaitSession,
};

fn wait_state(source: SessionWaitState) -> CoreResult<WaitState> {
    Ok(WaitState {
        active: source.active,
        running: source.running,
        queue_available: source.queue_available,
        pending_input: source
            .pending_input
            .into_iter()
            .map(|kind| match InputKind::try_from(kind) {
                Ok(InputKind::InputApproval) => Ok(WaitInput::Approval),
                Ok(InputKind::InputQuestion) => Ok(WaitInput::Question),
                Ok(InputKind::InputOther) => Ok(WaitInput::Other),
                _ => Err(CoreError::Internal("invalid input kind".into())),
            })
            .collect::<CoreResult<Vec<_>>>()?,
    })
}

// A lost transient waiter cannot signal a Host or Access restart. Bound one
// read so a new channel and read can still fit the user's original deadline.
const MAX_WAIT_SLICE: Duration = Duration::from_secs(5);

/// Cancellation uses the same Noise channel as the native wait. It cannot
/// create a replacement channel or cancel a sibling observation during recovery.
struct RemoteWait {
    commands: Arc<CommandService>,
    channel: crate::security::SecureChannel,
    session_id: String,
    wait_id: String,
    active: bool,
}

impl RemoteWait {
    async fn observe(&mut self, deadline: Instant) -> CoreResult<ResponsePayload> {
        let payload = RequestPayload {
            payload: Some(request_payload::Payload::WaitSession(WaitSession {
                session_id: self.session_id.clone(),
                wait_id: self.wait_id.clone(),
                ..Default::default()
            })),
        };
        let response = self
            .commands
            .query_wait(
                self.channel.host_id(),
                self.channel.device_id(),
                &self.channel,
                payload,
                deadline,
            )
            .await;
        // Dropping the in-flight future still triggers cancellation. A terminal
        // reply, channel replacement or transport expiry owns its own cleanup.
        self.active = false;
        response
    }
}

impl Drop for RemoteWait {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let commands = self.commands.clone();
        let channel = self.channel.clone();
        let payload = RequestPayload {
            payload: Some(request_payload::Payload::CancelSessionWait(
                CancelSessionWait {
                    session_id: self.session_id.clone(),
                    wait_id: self.wait_id.clone(),
                },
            )),
        };
        runtime.spawn(async move {
            let cancel = commands.query(channel.host_id(), channel.device_id(), &channel, payload);
            let _ = timeout(Duration::from_millis(500), cancel).await;
        });
    }
}

impl Core {
    /// Read source state without activating a cold Agent.
    pub async fn read_wait_state(&self, key: &SessionKey) -> CoreResult<WaitState> {
        self.authorized_session(key)?;
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                key.host_id.as_str(),
                RequestPayload {
                    payload: Some(request_payload::Payload::ReadWaitState(ReadWaitState {
                        session_id: session_id.into(),
                    })),
                },
            )
            .await?;
        let Some(response_payload::Payload::SessionWaitStateResult(result)) = response.payload
        else {
            return Err(CoreError::Internal("invalid wait state response".into()));
        };
        if result.session_id != session_id {
            return Err(CoreError::ChannelMismatch);
        }
        wait_state(
            result
                .state
                .ok_or_else(|| CoreError::Internal("missing wait state".into()))?,
        )
    }

    /// Observe the native stopping condition without starting or cancelling Agent work.
    pub async fn wait_session(
        &self,
        key: &SessionKey,
        timeout_ms: u64,
    ) -> CoreResult<WaitObservation> {
        if timeout_ms == 0 || timeout_ms > 2_147_483_647 {
            return Err(CoreError::InvalidInput(
                "wait timeout is outside the supported timer range".into(),
            ));
        }
        self.authorized_session(key)?;
        let session_id = key.session_id.as_str();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let return_allowance = Duration::from_millis((timeout_ms / 5).min(500));
        let mut stale_channel_id: Option<String> = None;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(CoreError::Timeout);
            }
            let slice_deadline = deadline.min(now + MAX_WAIT_SLICE);
            let wait_id = crate::util::random_id(16);
            let mut attempted_channel = None;
            let slice = async {
                let expected_session = self.cloud_session_id()?;
                let channel = self
                    .ensure_channel_inner(
                        &key.host_id,
                        stale_channel_id.is_some(),
                        stale_channel_id.as_deref(),
                    )
                    .await?;
                attempted_channel = Some(channel.channel_id().to_owned());
                let commands = self
                    .command_service_for_session(expected_session.as_deref())
                    .await?;
                RemoteWait {
                    commands: Arc::new(commands),
                    channel,
                    session_id: session_id.into(),
                    wait_id: wait_id.clone(),
                    active: true,
                }
                .observe(slice_deadline)
                .await
            };
            let response = match timeout_at(slice_deadline, slice).await {
                Ok(Ok(result)) => {
                    stale_channel_id = None;
                    result
                }
                Ok(Err(error))
                    if error.host_channel_replaced()
                        || (stale_channel_id.is_none()
                            && attempted_channel.is_some()
                            && (matches!(error, CoreError::ChannelMismatch)
                                || error.device_authorization_lost())) =>
                {
                    if error.device_authorization_lost() {
                        let checked: CoreResult<()> = timeout_at(deadline, self.settle(Err(error)))
                            .await
                            .unwrap_or(Err(CoreError::Timeout));
                        if matches!(
                            checked,
                            Err(CoreError::Revoked | CoreError::RevokedRetentionUnverified)
                        ) {
                            return Err(checked.expect_err("revocation result"));
                        }
                        if matches!(checked, Err(CoreError::Timeout)) {
                            return Err(CoreError::Timeout);
                        }
                    }
                    if stale_channel_id.is_some() {
                        let delay = self.runtime.next_reconnect_delay(&key.host_id);
                        if timeout_at(deadline, tokio::time::sleep(delay))
                            .await
                            .is_err()
                        {
                            return Err(CoreError::Timeout);
                        }
                    }
                    stale_channel_id = attempted_channel;
                    continue;
                }
                Ok(Err(CoreError::Timeout)) | Err(_) if slice_deadline < deadline => {
                    if attempted_channel.is_some() {
                        stale_channel_id = attempted_channel;
                    }
                    continue;
                }
                Ok(Err(error)) => {
                    return timeout_at(deadline, self.settle(Err(error)))
                        .await
                        .unwrap_or(Err(CoreError::Timeout));
                }
                Err(_) => return Err(CoreError::Timeout),
            };
            let Some(response_payload::Payload::SessionWaitResult(result)) = response.payload
            else {
                return Err(CoreError::Internal("invalid wait response".into()));
            };
            if result.session_id != session_id || result.wait_id != wait_id {
                return Err(CoreError::ChannelMismatch);
            }
            let source = result
                .state
                .ok_or_else(|| CoreError::Internal("missing wait state".into()))?;
            let state = wait_state(source)?;
            match WireReason::try_from(result.reason) {
                Ok(WireReason::WaitIdle) => {
                    return Ok(WaitObservation {
                        session_id: session_id.into(),
                        reason: WaitReason::Idle,
                        state,
                    });
                }
                Ok(WireReason::WaitInputRequired) => {
                    return Ok(WaitObservation {
                        session_id: session_id.into(),
                        reason: WaitReason::InputRequired,
                        state,
                    });
                }
                Ok(WireReason::WaitTimeout) => {
                    if deadline.saturating_duration_since(Instant::now()) <= return_allowance {
                        tokio::time::sleep_until(deadline).await;
                        return Ok(WaitObservation {
                            session_id: session_id.into(),
                            reason: WaitReason::Timeout,
                            state,
                        });
                    }
                }
                _ => return Err(CoreError::Internal("invalid wait reason".into())),
            }
        }
    }
}

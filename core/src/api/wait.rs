//! Read-only native wait slices with an overall monotonic deadline.
use super::*;
use crate::access::http::TransportObservation;
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

struct ObservationGuard {
    core: Core,
    host: String,
    session: String,
    wait_id: String,
    armed: bool,
}
impl Drop for ObservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let core = self.core.clone();
        let host = self.host.clone();
        let session_id = self.session.clone();
        let wait_id = self.wait_id.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // The Host expiry remains the cleanup fallback after runtime exit.
            return;
        };
        runtime.spawn(async move {
            let cleanup = core.query(
                &host,
                RequestPayload {
                    payload: Some(request_payload::Payload::CancelSessionWait(
                        CancelSessionWait {
                            session_id,
                            wait_id,
                        },
                    )),
                },
            );
            let _ = timeout(Duration::from_millis(500), cleanup).await;
        });
    }
}

impl Core {
    async fn wait_slice_query(
        &self,
        host_id: &str,
        payload: RequestPayload,
        fresh: bool,
        stale_channel_id: Option<&str>,
        deadline: Instant,
        transport: Arc<TransportObservation>,
    ) -> Result<ResponsePayload, (CoreError, Option<String>)> {
        let config = self.require_config().map_err(|error| (error, None))?;
        let channel = self
            .ensure_wait_channel(host_id, fresh, stale_channel_id, transport.clone())
            .await
            .map_err(|error| (error, None))?;
        let channel_id = channel.channel_id().to_owned();
        self.command_service()
            .map_err(|error| (error, Some(channel_id.clone())))?
            .query_wait(
                host_id,
                &config.device_id,
                &channel,
                payload,
                deadline,
                &transport,
            )
            .await
            .map_err(|error| (error, Some(channel_id)))
    }

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
        let mut last = None;
        let mut fresh = false;
        let mut stale_channel_id = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return last
                    .map(|state| WaitObservation {
                        session_id: session_id.into(),
                        reason: WaitReason::Timeout,
                        state,
                    })
                    .ok_or(CoreError::Timeout);
            }
            let wait_id = crate::util::random_id(16);
            let transport = Arc::new(TransportObservation::default());
            let mut guard = ObservationGuard {
                core: self.clone(),
                host: key.host_id.clone(),
                session: session_id.into(),
                wait_id: wait_id.clone(),
                armed: true,
            };
            let payload = RequestPayload {
                payload: Some(request_payload::Payload::WaitSession(WaitSession {
                    session_id: session_id.into(),
                    wait_id: wait_id.clone(),
                    // The command service derives this immediately before
                    // sending, after channel acquisition and handshake.
                    timeout_ms: 1,
                })),
            };
            let response = match timeout_at(
                deadline,
                self.wait_slice_query(
                    &key.host_id,
                    payload,
                    fresh,
                    stale_channel_id.as_deref(),
                    deadline,
                    transport.clone(),
                ),
            )
            .await
            {
                Ok(Ok(result)) => {
                    fresh = false;
                    stale_channel_id = None;
                    result
                }
                Ok(Err((error, failed_channel)))
                    if error.host_channel_replaced()
                        || (!fresh
                            && (matches!(error, CoreError::ChannelMismatch)
                                || error.device_authorization_lost())) =>
                {
                    // The old channel is no longer usable for a scoped cancel.
                    // Its Host-side channel cleanup or expiry owns the old wait;
                    // opening a second recovery handshake here can race the retry.
                    guard.armed = false;
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
                    if fresh {
                        let delay = self.runtime.next_reconnect_delay(&key.host_id);
                        if timeout_at(deadline, tokio::time::sleep(delay))
                            .await
                            .is_err()
                        {
                            return last
                                .map(|state| WaitObservation {
                                    session_id: session_id.into(),
                                    reason: WaitReason::Timeout,
                                    state,
                                })
                                .ok_or(CoreError::Timeout);
                        }
                    }
                    fresh = true;
                    stale_channel_id = failed_channel;
                    continue;
                }
                Ok(Err((error, _))) => {
                    return timeout_at(deadline, self.settle(Err(error)))
                        .await
                        .unwrap_or(Err(CoreError::Timeout));
                }
                Err(_) => {
                    if let Some(error) = transport.latest_error() {
                        return Err(error.into());
                    }
                    return last
                        .map(|state| WaitObservation {
                            session_id: session_id.into(),
                            reason: WaitReason::Timeout,
                            state,
                        })
                        .ok_or(CoreError::Timeout);
                }
            };
            guard.armed = false;
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
                Ok(WireReason::WaitTimeout) => last = Some(state),
                _ => return Err(CoreError::Internal("invalid wait reason".into())),
            }
        }
    }
}

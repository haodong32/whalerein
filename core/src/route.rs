//! Host route recovery decisions.
//!
//! A Device follows the single Host route Central advertises. These types keep
//! the distinction between a routing outcome and a policy denial explicit:
//! only a policy denial is terminal, while a stale or unavailable route may be
//! retried with bounded backoff. They carry no conversation content.

use serde::{Deserialize, Serialize};

/// Central's current route as returned in the account host directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRoute {
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub route_generation: u64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub last_observed_at: String,
    #[serde(default)]
    pub https_url: String,
    #[serde(default)]
    pub mqtt_url: String,
    #[serde(default)]
    pub reason: String,
}

impl HostRoute {
    /// An open route with a current Central directory answer.
    pub fn usable(&self) -> bool {
        self.state == "open" && self.directory == "fresh" && !self.https_url.is_empty()
    }

    /// A stale route may still be used under residual authority, but a new
    /// connection must refresh the directory first.
    pub fn stale(&self) -> bool {
        self.state == "open" && self.directory != "fresh"
    }

    pub fn denied(&self) -> bool {
        self.reason == "policy_denied" || self.state == "denied"
    }
}

/// User-facing route status derived from Central's directory answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteStatus {
    /// Open and current; a new connection may use it.
    Current,
    /// Open but the node heartbeat is stale; residual authority only.
    Stale,
    /// No usable route is known.
    Unavailable,
    /// Current policy denies this scope. Never retry elsewhere.
    Denied,
    /// No route row exists for this Host.
    None,
}

impl HostRoute {
    /// Classify the directory row for status output and recovery decisions.
    pub fn status(&self) -> RouteStatus {
        if self.denied() {
            return RouteStatus::Denied;
        }
        if self.state == "none" || (self.state.is_empty() && self.node_id.is_empty()) {
            return RouteStatus::None;
        }
        if self.usable() {
            return RouteStatus::Current;
        }
        if self.stale() {
            return RouteStatus::Stale;
        }
        RouteStatus::Unavailable
    }
}

/// One authenticated directory answer for a recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryAnswer {
    /// Central advertises a different generation and node.
    Newer(HostRoute),
    /// Central still advertises the route the client already knows.
    Unchanged(HostRoute),
    /// Central could not answer; residual authority alone applies.
    Unavailable,
    /// Current policy denies the scope. This is terminal.
    Denied(String),
}

/// The next action for one coalesced recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// Reconnect to the advertised node and recover the original command ID.
    Reconnect(HostRoute),
    /// Keep the existing channel or retry the same route.
    Keep(HostRoute),
    /// Wait with bounded exponential backoff before another lookup.
    Backoff,
    /// Stop this scope and do not retry elsewhere.
    Stop(String),
}

/// Decide one recovery action. A stale hint never grants access by itself; a
/// policy denial always wins over a concurrent route hint.
pub fn decide(answer: DirectoryAnswer, known_generation: u64) -> RecoveryDecision {
    match answer {
        DirectoryAnswer::Denied(reason) => RecoveryDecision::Stop(reason),
        DirectoryAnswer::Unavailable => RecoveryDecision::Backoff,
        DirectoryAnswer::Newer(route) => {
            if route.denied() {
                return RecoveryDecision::Stop(route.reason);
            }
            if route.route_generation > known_generation && route.usable() {
                RecoveryDecision::Reconnect(route)
            } else {
                RecoveryDecision::Backoff
            }
        }
        DirectoryAnswer::Unchanged(route) => {
            if route.denied() {
                RecoveryDecision::Stop(route.reason)
            } else if route.usable() {
                RecoveryDecision::Keep(route)
            } else {
                RecoveryDecision::Backoff
            }
        }
    }
}

/// Bounded exponential backoff with jitter supplied by the caller. The caller
/// owns randomness so tests and platforms can be deterministic.
pub fn backoff_ms(attempt: u32, jitter_ms: u64) -> u64 {
    let exponent = attempt.min(6);
    let base = 250u64.saturating_mul(1u64 << exponent).min(30_000);
    base.saturating_add(jitter_ms.min(base))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(generation: u64, state: &str, directory: &str) -> HostRoute {
        HostRoute {
            node_id: "node-b".into(),
            route_generation: generation,
            state: state.into(),
            directory: directory.into(),
            last_observed_at: "2026-11-26T00:00:00Z".into(),
            https_url: "https://b".into(),
            mqtt_url: "mqtts://b".into(),
            reason: String::new(),
        }
    }

    #[test]
    fn newer_open_route_reconnects_and_keeps_the_original_identity_contract() {
        let decision = decide(DirectoryAnswer::Newer(route(9, "open", "fresh")), 8);
        assert!(
            matches!(decision, RecoveryDecision::Reconnect(route) if route.route_generation == 9)
        );
    }

    #[test]
    fn unchanged_route_does_not_move() {
        assert!(matches!(
            decide(DirectoryAnswer::Unchanged(route(8, "open", "fresh")), 8),
            RecoveryDecision::Keep(_)
        ));
    }

    #[test]
    fn stale_route_requires_a_refresh_and_unavailable_backs_off() {
        assert_eq!(
            decide(DirectoryAnswer::Newer(route(9, "open", "stale")), 8),
            RecoveryDecision::Backoff
        );
        assert_eq!(
            decide(DirectoryAnswer::Unavailable, 8),
            RecoveryDecision::Backoff
        );
    }

    #[test]
    fn policy_denial_is_terminal_even_beside_a_newer_route() {
        let mut denied = route(9, "open", "fresh");
        denied.reason = "policy_denied".into();
        assert!(matches!(
            decide(DirectoryAnswer::Newer(denied), 8),
            RecoveryDecision::Stop(reason) if reason == "policy_denied"
        ));
        assert!(matches!(
            decide(DirectoryAnswer::Denied("session_revoked".into()), 8),
            RecoveryDecision::Stop(reason) if reason == "session_revoked"
        ));
    }

    #[test]
    fn status_maps_route_rows_to_user_facing_states() {
        assert_eq!(route(8, "open", "fresh").status(), RouteStatus::Current);
        assert_eq!(route(8, "open", "stale").status(), RouteStatus::Stale);
        assert_eq!(
            route(8, "closed", "fresh").status(),
            RouteStatus::Unavailable
        );
        assert_eq!(HostRoute::default().status(), RouteStatus::None);
        let mut denied = route(8, "open", "fresh");
        denied.reason = "policy_denied".into();
        assert_eq!(denied.status(), RouteStatus::Denied);
    }

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(backoff_ms(0, 0), 250);
        assert_eq!(backoff_ms(3, 0), 2000);
        assert!(backoff_ms(30, 1000) <= 31_000);
    }
}

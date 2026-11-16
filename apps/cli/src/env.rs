//! Startup validation of environment-provided configuration.
//!
//! Every variable that steers transport, TLS or logging is checked before a
//! command runs. A bad value is a local configuration problem, not an internal
//! failure: it is reported as `invalid_configuration` (exit 2) with the
//! offending variable named. Values are never echoed back because an MQTT URL
//! may embed credentials.

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

/// One rejected environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvIssue {
    pub variable: &'static str,
    pub detail: String,
}

impl EnvIssue {
    fn new(variable: &'static str, detail: impl Into<String>) -> Self {
        Self {
            variable,
            detail: detail.into(),
        }
    }

    /// Actionable single-line message; never includes the full variable value.
    pub fn message(&self) -> String {
        format!("{} is invalid: {}", self.variable, self.detail)
    }
}

/// Validate every supported environment variable before dispatch.
pub fn validate() -> Result<(), EnvIssue> {
    mqtt_url()?;
    ca_file()?;
    log_filter()?;
    Ok(())
}

/// `WHALEREIN_MQTT_URL`, when present, must be an absolute MQTT endpoint.
pub fn mqtt_url() -> Result<Option<String>, EnvIssue> {
    let value = match std::env::var("WHALEREIN_MQTT_URL") {
        Ok(value) if !value.is_empty() => value,
        _ => return Ok(None),
    };
    check_mqtt_url(&value)?;
    Ok(Some(value))
}

fn check_mqtt_url(value: &str) -> Result<(), EnvIssue> {
    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            "expected mqtt://, mqtts://, ssl://, tls:// or tcp://",
        ));
    };
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "mqtt" | "mqtts" | "ssl" | "tls" | "tcp") {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            format!(
                "unsupported scheme {scheme:?}; expected mqtt://, mqtts://, ssl://, tls:// or tcp://"
            ),
        ));
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned();
    if authority.is_empty() {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            "the broker host is missing",
        ));
    }
    if authority.contains('@') {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            "credentials are not allowed in the broker URL",
        ));
    }
    if rest.contains('#') {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            "a fragment is not allowed in the broker URL",
        ));
    }
    let host = authority_host(&authority);
    if host.is_empty() {
        return Err(EnvIssue::new(
            "WHALEREIN_MQTT_URL",
            "the broker host is missing",
        ));
    }
    Ok(())
}

/// Extract the host from `host`, `host:port` or `[ipv6]:port`.
fn authority_host(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, _)) => host,
            None => "",
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => host,
        _ => authority,
    }
}

/// `WHALEREIN_CA_FILE`, when present, must name a readable non-empty PEM file.
pub fn ca_file() -> Result<Option<PathBuf>, EnvIssue> {
    let value = match std::env::var("WHALEREIN_CA_FILE") {
        Ok(value) if !value.is_empty() => value,
        _ => return Ok(None),
    };
    let path = PathBuf::from(value);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => Err(EnvIssue::new(
            "WHALEREIN_CA_FILE",
            format!("{} is empty", path.display()),
        )),
        Ok(_) => Ok(Some(path)),
        Err(_) => Err(EnvIssue::new(
            "WHALEREIN_CA_FILE",
            format!("{} could not be read", path.display()),
        )),
    }
}

/// The log filter that `init_tracing` installs. An unparseable filter is a
/// configuration error instead of a silent fallback to the default.
pub fn log_filter() -> Result<String, EnvIssue> {
    let (variable, value) = match std::env::var("WHALEREIN_LOG") {
        Ok(value) if !value.is_empty() => ("WHALEREIN_LOG", value),
        _ => match std::env::var("RUST_LOG") {
            Ok(value) if !value.is_empty() => ("RUST_LOG", value),
            _ => return Ok("warn".to_owned()),
        },
    };
    EnvFilter::try_new(&value).map_err(|_| {
        EnvIssue::new(
            variable,
            "not a valid log filter directive (expected e.g. warn, info or whalerein_core=debug)",
        )
    })?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment variables are process-global; mutate them serially.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn with_env<T>(name: &str, value: Option<&str>, run: impl FnOnce() -> T) -> T {
        let previous = std::env::var(name).ok();
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
        let result = run();
        match previous {
            Some(previous) => std::env::set_var(name, previous),
            None => std::env::remove_var(name),
        }
        result
    }

    #[test]
    fn accepts_supported_broker_schemes_and_hosts() {
        let _guard = serial();
        for value in [
            "mqtt://127.0.0.1:1883",
            "mqtts://broker.test:8883",
            "ssl://broker.test",
            "tls://broker.test",
            "tcp://broker.test:1883",
            "MQTT://broker.test",
            "mqtt://[::1]:1883",
        ] {
            assert!(check_mqtt_url(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn rejects_bad_broker_urls_without_echoing_the_value() {
        let _guard = serial();
        for (value, expected) in [
            ("http://broker.test", "unsupported scheme"),
            ("broker.test:1883", "expected mqtt://"),
            ("mqtt://", "host is missing"),
            (
                "mqtt://user:secret@broker.test",
                "credentials are not allowed",
            ),
            ("mqtt://broker.test/#fragment", "fragment"),
        ] {
            let issue = check_mqtt_url(value).unwrap_err();
            assert_eq!(issue.variable, "WHALEREIN_MQTT_URL");
            assert!(issue.detail.contains(expected), "{value}: {issue:?}");
            assert!(!issue.message().contains("user:secret"));
            assert!(!issue.message().contains("http://broker.test"));
        }
    }

    #[test]
    fn broker_url_is_optional_and_empty_means_unset() {
        let _guard = serial();
        assert!(with_env("WHALEREIN_MQTT_URL", None, mqtt_url)
            .unwrap()
            .is_none());
        assert!(with_env("WHALEREIN_MQTT_URL", Some(""), mqtt_url)
            .unwrap()
            .is_none());
        assert_eq!(
            with_env("WHALEREIN_MQTT_URL", Some("mqtt://broker.test"), mqtt_url)
                .unwrap()
                .as_deref(),
            Some("mqtt://broker.test")
        );
        assert!(with_env("WHALEREIN_MQTT_URL", Some("ftp://x"), mqtt_url).is_err());
    }

    #[test]
    fn ca_file_must_exist_be_readable_and_non_empty() {
        let _guard = serial();
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("ca.pem");
        std::fs::write(&pem, b"-----BEGIN CERTIFICATE-----").unwrap();
        assert_eq!(
            with_env("WHALEREIN_CA_FILE", Some(pem.to_str().unwrap()), ca_file).unwrap(),
            Some(pem.clone())
        );
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, b"").unwrap();
        let issue =
            with_env("WHALEREIN_CA_FILE", Some(empty.to_str().unwrap()), ca_file).unwrap_err();
        assert_eq!(issue.variable, "WHALEREIN_CA_FILE");
        assert!(issue.detail.contains("empty"));
        let missing = dir.path().join("missing.pem");
        let issue = with_env(
            "WHALEREIN_CA_FILE",
            Some(missing.to_str().unwrap()),
            ca_file,
        )
        .unwrap_err();
        assert!(issue.detail.contains("could not be read"));
        assert!(with_env("WHALEREIN_CA_FILE", None, ca_file)
            .unwrap()
            .is_none());
        assert!(with_env("WHALEREIN_CA_FILE", Some(""), ca_file)
            .unwrap()
            .is_none());
    }

    #[test]
    fn log_filter_is_validated_before_tracing_starts() {
        let _guard = serial();
        assert_eq!(with_env("WHALEREIN_LOG", None, log_filter).unwrap(), "warn");
        assert_eq!(
            with_env("WHALEREIN_LOG", Some("info"), log_filter).unwrap(),
            "info"
        );
        assert_eq!(
            with_env(
                "WHALEREIN_LOG",
                Some("whalerein_core=debug,warn"),
                log_filter
            )
            .unwrap(),
            "whalerein_core=debug,warn"
        );
        let issue = with_env("WHALEREIN_LOG", Some("bad[["), log_filter).unwrap_err();
        assert_eq!(issue.variable, "WHALEREIN_LOG");
        assert!(issue.message().contains("valid log filter"));
        let issue = with_env("WHALEREIN_LOG", Some("warn"), || {
            with_env("WHALEREIN_LOG", None, || {
                with_env("RUST_LOG", Some("bad[["), log_filter)
            })
        })
        .unwrap_err();
        assert_eq!(issue.variable, "RUST_LOG");
    }

    #[test]
    fn valid_environment_passes_aggregate_validation() {
        let _guard = serial();
        with_env("WHALEREIN_MQTT_URL", Some("mqtt://broker.test"), || {
            with_env("WHALEREIN_CA_FILE", None, || {
                with_env("WHALEREIN_LOG", Some("warn"), || {
                    with_env("RUST_LOG", None, validate)
                })
            })
        })
        .unwrap();
        let issue = with_env("WHALEREIN_MQTT_URL", Some("http://x"), || {
            with_env("WHALEREIN_LOG", None, validate)
        })
        .unwrap_err();
        assert_eq!(issue.variable, "WHALEREIN_MQTT_URL");
    }
}

//! Account-service descriptors and account context.
//!
//! This module owns service authority, profile binding, and native account sessions.

use super::Environment;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::api::{CoreError, CoreResult};
use crate::configuration::{ConfigurationError, EnvironmentDefinition, EnvironmentMode};

/// How a profile obtains identity and Host authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileMode {
    /// Invitation pairing only; no account service is contacted.
    Standalone,
    /// Account-backed discovery and authorization.
    Account,
}

impl ProfileMode {
    /// Map a resolved environment mode to the profile mode it requires.
    pub fn from_environment_mode(mode: EnvironmentMode) -> Self {
        match mode {
            EnvironmentMode::Account => Self::Account,
            EnvironmentMode::Standalone => Self::Standalone,
        }
    }
}

/// The selected account service family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceKind {
    /// Official production service.
    #[default]
    OfficialProduction,
    /// Official staging service, available only through explicit testing setup.
    OfficialStaging,
    /// Official local development service.
    OfficialLocal,
    /// Operator-provided compatible service.
    Custom,
}

impl ServiceKind {
    pub fn environment(self) -> Environment {
        match self {
            Self::OfficialProduction => Environment::Production,
            Self::OfficialStaging => Environment::Staging,
            Self::OfficialLocal => Environment::Local,
            Self::Custom => Environment::Custom,
        }
    }

    pub fn is_testing(self) -> bool {
        matches!(self, Self::OfficialStaging | Self::OfficialLocal)
    }

    pub fn is_official(self) -> bool {
        !matches!(self, Self::Custom)
    }
}

/// A validated account-service selection.
///
/// Official presets currently carry their stable service identity only:
/// public endpoints and the native registration are frozen by the protocol
/// feasibility task. A custom service must supply every endpoint explicitly;
/// nothing is inferred from a hostname.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescriptor {
    pub kind: ServiceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_url: Option<String>,
}

/// Explicit custom-service configuration supplied by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomServiceInput {
    pub api_origin: String,
    pub issuer: String,
    pub native_client_id: String,
    pub access_url: Option<String>,
}

impl ServiceDescriptor {
    pub fn official_production() -> Self {
        Self {
            kind: ServiceKind::OfficialProduction,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            access_url: None,
        }
    }

    pub fn official_staging() -> Self {
        Self {
            kind: ServiceKind::OfficialStaging,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            access_url: None,
        }
    }

    pub fn official_local() -> Self {
        Self {
            kind: ServiceKind::OfficialLocal,
            ..Self::official_production()
        }
    }

    /// Build the account-service descriptor for one resolved environment.
    ///
    /// A standalone environment has no account descriptor. Built-in identities
    /// keep their stable [`ServiceKind`] and never repeat catalog endpoints; a
    /// custom account environment carries its explicit authority. Managed
    /// routes are never part of this descriptor.
    pub(crate) fn from_environment_definition(
        definition: &EnvironmentDefinition,
        builtin: bool,
    ) -> CoreResult<Option<Self>> {
        let mode = definition.mode.ok_or_else(|| {
            CoreError::from(ConfigurationError::InvalidMode {
                environment: definition.id.clone(),
            })
        })?;
        if mode == EnvironmentMode::Standalone {
            return Ok(None);
        }
        if builtin {
            let descriptor = match definition.id.as_str() {
                "official-production" => Self::official_production(),
                "official-staging" => Self::official_staging(),
                "official-local" => Self::official_local(),
                _ => {
                    return Err(CoreError::from(ConfigurationError::InvalidMode {
                        environment: definition.id.clone(),
                    }))
                }
            };
            descriptor.validate()?;
            return Ok(Some(descriptor));
        }
        let api_origin = definition.api_origin.clone().ok_or_else(|| {
            CoreError::from(ConfigurationError::InvalidModeFields {
                environment: definition.id.clone(),
            })
        })?;
        let issuer = definition.issuer.clone().ok_or_else(|| {
            CoreError::from(ConfigurationError::InvalidModeFields {
                environment: definition.id.clone(),
            })
        })?;
        let native_client_id = definition.native_client_id.clone().ok_or_else(|| {
            CoreError::from(ConfigurationError::InvalidModeFields {
                environment: definition.id.clone(),
            })
        })?;
        Ok(Some(Self::custom(CustomServiceInput {
            api_origin,
            issuer,
            native_client_id,
            access_url: None,
        })?))
    }

    pub fn custom(input: CustomServiceInput) -> CoreResult<Self> {
        let descriptor = Self {
            kind: ServiceKind::Custom,
            api_origin: Some(input.api_origin),
            issuer: Some(input.issuer),
            native_client_id: Some(input.native_client_id),
            access_url: input.access_url,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    /// Validate every configured value before it can be selected or used.
    pub fn validate(&self) -> CoreResult<()> {
        let api = match &self.api_origin {
            Some(value) => Some(parse_service_url(value, "account API origin")?),
            None => None,
        };
        let issuer = match &self.issuer {
            Some(value) => Some(parse_service_url(value, "account issuer")?),
            None => None,
        };
        let access = match &self.access_url {
            Some(value) => Some(parse_service_url(value, "Access URL")?),
            None => None,
        };

        match self.kind {
            ServiceKind::Custom => {
                let api = api.as_ref().ok_or_else(|| {
                    CoreError::Config("custom service requires an account API origin".into())
                })?;
                let issuer = issuer.as_ref().ok_or_else(|| {
                    CoreError::Config("custom service requires an OIDC issuer".into())
                })?;
                let client = self.native_client_id.as_ref().ok_or_else(|| {
                    CoreError::Config("custom service requires a native client registration".into())
                })?;
                if client.is_empty()
                    || client.len() > 128
                    || client
                        .chars()
                        .any(|value| value.is_control() || value.is_whitespace())
                {
                    return Err(CoreError::Config(
                        "invalid native client registration".into(),
                    ));
                }
                if api.origin_str() == issuer.origin_str() {
                    return Err(CoreError::Config(
                        "account API origin and issuer must be distinct".into(),
                    ));
                }
                if let Some(access) = &access {
                    if access.origin_str() == api.origin_str()
                        || access.origin_str() == issuer.origin_str()
                    {
                        return Err(CoreError::Config(
                            "Access URL must be distinct from account service origins".into(),
                        ));
                    }
                }
            }
            ServiceKind::OfficialProduction
            | ServiceKind::OfficialStaging
            | ServiceKind::OfficialLocal => {
                // Official presets are built in. An edited registry must not
                // smuggle a different authority under an official label.
                if self.api_origin.is_some()
                    || self.issuer.is_some()
                    || self.native_client_id.is_some()
                    || self.access_url.is_some()
                {
                    return Err(CoreError::Config(
                        "official service descriptors cannot override endpoints".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn environment(&self) -> Environment {
        self.kind.environment()
    }

    pub fn is_testing(&self) -> bool {
        self.kind.is_testing()
    }

    /// Human-safe service label used by every client surface.
    pub fn label(&self) -> &'static str {
        match self.kind {
            ServiceKind::OfficialProduction => "WhaleRein official production",
            ServiceKind::OfficialStaging => "WhaleRein official staging (testing only)",
            ServiceKind::OfficialLocal => "WhaleRein official local (testing only)",
            ServiceKind::Custom => "Custom account service",
        }
    }

    /// Short value used in CLI text and structured output.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            ServiceKind::OfficialProduction => "official-production",
            ServiceKind::OfficialStaging => "official-staging",
            ServiceKind::OfficialLocal => "official-local",
            ServiceKind::Custom => "custom",
        }
    }

    /// Resolve bootstrap configuration without persisting endpoint copies.
    pub fn bootstrap_api_origin(&self) -> CoreResult<String> {
        self.validate()?;
        match self.kind {
            ServiceKind::OfficialLocal => configuration_catalog_origin("local"),
            ServiceKind::Custom => self
                .api_origin
                .clone()
                .ok_or_else(|| CoreError::Config("custom API origin missing".into())),
            ServiceKind::OfficialProduction | ServiceKind::OfficialStaging => {
                let key = if self.kind == ServiceKind::OfficialProduction {
                    "production"
                } else {
                    "staging"
                };
                official_catalog()[key]["api_origin"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        CoreError::Config("official environment catalog is invalid".into())
                    })
            }
        }
    }

    pub fn issuer_display(&self) -> &str {
        self.issuer.as_deref().unwrap_or("not configured")
    }
}

/// Public built-in environment catalog shared with the Node Host package.
///
/// The canonical client-environment catalog is embedded by Core, so the binary
/// never depends on a repository-relative JSON file or a prototype descriptor.
fn official_catalog() -> serde_json::Value {
    let catalog: serde_json::Value =
        serde_json::from_str(crate::configuration::CONFIG_CATALOG_JSON)
            .expect("built-in official catalog must be valid JSON");
    catalog["environments"].clone()
}

/// Read the canonical bootstrap origin for a built-in environment. The shared
/// client-environment catalog is the sole authority; no prototype descriptor
/// file is consulted.
fn configuration_catalog_origin(environment: &str) -> CoreResult<String> {
    let catalog: serde_json::Value =
        serde_json::from_str(crate::configuration::CONFIG_CATALOG_JSON)
            .map_err(|_| CoreError::Config("client environment catalog is invalid".into()))?;
    catalog["environments"][environment]["api_origin"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| CoreError::Config("official environment catalog is invalid".into()))
}

/// Stable authority fingerprint for one resolved environment definition.
///
/// Labels are deliberately absent: renaming a display label never re-binds a
/// profile. The fingerprint covers the environment identity, mode and account
/// authority fields, so a changed origin/issuer/client is a different authority.
pub(crate) fn environment_authority_fingerprint(definition: &EnvironmentDefinition) -> String {
    let mode = definition
        .mode
        .map(EnvironmentMode::as_str)
        .unwrap_or("unresolved");
    let material = format!(
        "whalerein/v1/profile-authority\nid={}\nmode={}\napi_origin={}\nissuer={}\nnative_client_id={}",
        definition.id,
        mode,
        definition.api_origin.as_deref().unwrap_or(""),
        definition.issuer.as_deref().unwrap_or(""),
        definition.native_client_id.as_deref().unwrap_or("")
    );
    crate::util::sha256_hex(material.as_bytes())
}

/// Opaque binding recorded after an account session is established.
///
/// An account-bound profile must not be rebound to a different issuer, subject
/// or workspace in place; that requires a separate profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBinding {
    pub subject: String,
    pub workspace: String,
    pub issuer: String,
    #[serde(default)]
    pub bound_unix_ms: u64,
}

impl AccountBinding {
    pub fn validate(&self, service: Option<&ServiceDescriptor>) -> CoreResult<()> {
        if self.subject.is_empty() || self.workspace.is_empty() || self.issuer.is_empty() {
            return Err(CoreError::Config(
                "account binding is missing identity fields".into(),
            ));
        }
        let service = service.ok_or_else(|| {
            CoreError::Config("account binding requires a service descriptor".into())
        })?;
        if let Some(issuer) = &service.issuer {
            if issuer != &self.issuer {
                return Err(CoreError::Config(
                    "account binding issuer does not match the selected service".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ParsedServiceUrl {
    origin: String,
}

impl ParsedServiceUrl {
    fn origin_str(&self) -> &str {
        &self.origin
    }
}

fn parse_service_url(value: &str, field: &str) -> CoreResult<ParsedServiceUrl> {
    if value.is_empty() || value.len() > 2048 {
        return Err(CoreError::Config(format!("invalid {field}")));
    }
    let url = Url::parse(value).map_err(|_| CoreError::Config(format!("invalid {field}")))?;
    let scheme = url.scheme();
    let host = url
        .host_str()
        .ok_or_else(|| CoreError::Config(format!("invalid {field}")))?;
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1";
    if !(scheme == "https" || (scheme == "http" && loopback)) {
        return Err(CoreError::Config(format!("{field} must use https")));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CoreError::Config(format!(
            "{field} must not carry credentials, a query or a fragment"
        )));
    }
    let port = url
        .port()
        .map(|value| format!(":{value}"))
        .unwrap_or_default();
    Ok(ParsedServiceUrl {
        origin: format!("{scheme}://{host}{port}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(api: &str, issuer: &str) -> ServiceDescriptor {
        ServiceDescriptor::custom(CustomServiceInput {
            api_origin: api.into(),
            issuer: issuer.into(),
            native_client_id: "whalerein-cli".into(),
            access_url: None,
        })
        .unwrap()
    }

    #[test]
    fn local_descriptor_is_catalog_driven() {
        assert_eq!(
            official_catalog()["production"]["id"],
            "official-production"
        );
        assert_eq!(
            official_catalog()["local"]["api_origin"],
            "https://app.whalerein.test"
        );
        let local = ServiceDescriptor::official_local();
        local.validate().unwrap();
        assert_eq!(local.environment(), Environment::Local);
        assert!(local.is_testing());
        assert!(local.label().contains("local"));
        assert_eq!(
            local.bootstrap_api_origin().unwrap(),
            "https://app.whalerein.test"
        );
    }

    #[test]
    fn official_presets_are_valid_and_labeled() {
        let production = ServiceDescriptor::official_production();
        let staging = ServiceDescriptor::official_staging();
        production.validate().unwrap();
        assert_eq!(
            production.bootstrap_api_origin().unwrap(),
            "https://app.whalerein.com"
        );
        staging.validate().unwrap();
        assert_eq!(production.environment(), Environment::Production);
        assert_eq!(staging.environment(), Environment::Staging);
        assert!(staging.is_testing());
        assert!(!production.is_testing());
        assert_eq!(production.issuer_display(), "not configured");
        assert!(staging.label().contains("testing only"));
    }

    #[test]
    fn official_descriptor_cannot_override_endpoints() {
        let mut descriptor = ServiceDescriptor::official_production();
        descriptor.api_origin = Some("https://api.example".into());
        assert!(matches!(descriptor.validate(), Err(CoreError::Config(_))));
    }

    #[test]
    fn custom_service_requires_distinct_explicit_endpoints() {
        let descriptor = custom("https://api.corp.example", "https://id.corp.example");
        descriptor.validate().unwrap();
        assert_eq!(descriptor.environment(), Environment::Custom);

        for (api, issuer) in [
            ("http://api.corp.example", "https://id.corp.example"),
            ("https://user@api.corp.example", "https://id.corp.example"),
            ("https://api.corp.example?x=1", "https://id.corp.example"),
            ("https://api.corp.example", "https://api.corp.example"),
        ] {
            let input = CustomServiceInput {
                api_origin: api.into(),
                issuer: issuer.into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            };
            assert!(ServiceDescriptor::custom(input).is_err(), "{api} {issuer}");
        }
    }

    #[test]
    fn loopback_http_is_allowed_for_local_fixtures() {
        let descriptor = custom("http://127.0.0.1:8080", "http://localhost:3001/oidc");
        descriptor.validate().unwrap();
    }

    #[test]
    fn custom_service_requires_a_native_registration() {
        let input = CustomServiceInput {
            api_origin: "https://api.corp.example".into(),
            issuer: "https://id.corp.example".into(),
            native_client_id: " ".into(),
            access_url: None,
        };
        assert!(ServiceDescriptor::custom(input).is_err());
    }

    #[test]
    fn access_url_must_not_reuse_account_origins() {
        let input = CustomServiceInput {
            api_origin: "https://api.corp.example".into(),
            issuer: "https://id.corp.example".into(),
            native_client_id: "whalerein-cli".into(),
            access_url: Some("https://api.corp.example".into()),
        };
        assert!(ServiceDescriptor::custom(input).is_err());
    }

    #[test]
    fn unknown_service_fields_are_rejected() {
        let json = r#"{"kind":"official-production","unexpected":"authority"}"#;
        assert!(serde_json::from_str::<ServiceDescriptor>(json).is_err());
    }

    #[test]
    fn config_env_ac_07_definition_maps_mode_service_and_authority() {
        use crate::configuration::{EnvironmentMode, TlsConfig, TlsProvider};

        let custom = EnvironmentDefinition {
            id: "company".into(),
            mode: Some(EnvironmentMode::Account),
            label: Some("Company Cloud".into()),
            api_origin: Some("https://api.corp.example".into()),
            issuer: Some("https://id.corp.example".into()),
            native_client_id: Some("whalerein-cli".into()),
            environment: None,
            tls: Some(TlsConfig {
                ca_provider: TlsProvider::System,
                ca_file: None,
            }),
        };
        assert_eq!(
            ProfileMode::from_environment_mode(EnvironmentMode::Account),
            ProfileMode::Account
        );
        let descriptor = ServiceDescriptor::from_environment_definition(&custom, false).unwrap();
        let descriptor = descriptor.unwrap();
        assert_eq!(descriptor.kind, ServiceKind::Custom);
        assert_eq!(
            descriptor.issuer.as_deref(),
            Some("https://id.corp.example")
        );

        let mut labelled = custom.clone();
        labelled.label = Some("Renamed".into());
        assert_eq!(
            environment_authority_fingerprint(&custom),
            environment_authority_fingerprint(&labelled),
            "labels must not participate in authority binding"
        );
        let mut changed = custom.clone();
        changed.api_origin = Some("https://api.other.example".into());
        assert_ne!(
            environment_authority_fingerprint(&custom),
            environment_authority_fingerprint(&changed)
        );

        let standalone = EnvironmentDefinition {
            id: "lab".into(),
            mode: Some(EnvironmentMode::Standalone),
            label: None,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            environment: None,
            tls: None,
        };
        assert_eq!(
            ServiceDescriptor::from_environment_definition(&standalone, false).unwrap(),
            None
        );
        assert_eq!(
            ProfileMode::from_environment_mode(EnvironmentMode::Standalone),
            ProfileMode::Standalone
        );

        let builtin_local = EnvironmentDefinition {
            id: "official-local".into(),
            mode: Some(EnvironmentMode::Account),
            label: None,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            environment: Some("local".into()),
            tls: None,
        };
        let local = ServiceDescriptor::from_environment_definition(&builtin_local, true)
            .unwrap()
            .unwrap();
        assert_eq!(local.kind, ServiceKind::OfficialLocal);
        assert_eq!(local.environment(), Environment::Local);
        assert_eq!(
            local.bootstrap_api_origin().unwrap(),
            "https://app.whalerein.test"
        );
        let mut missing_mode = custom;
        missing_mode.mode = None;
        assert!(ServiceDescriptor::from_environment_definition(&missing_mode, false).is_err());
    }

    #[test]
    fn binding_must_match_the_service_issuer() {
        let service = custom("https://api.corp.example", "https://id.corp.example");
        let mut binding = AccountBinding {
            subject: "subject-1".into(),
            workspace: "workspace-1".into(),
            issuer: "https://id.corp.example".into(),
            bound_unix_ms: 0,
        };
        binding.validate(Some(&service)).unwrap();
        binding.issuer = "https://other.example".into();
        assert!(binding.validate(Some(&service)).is_err());
        binding.issuer = "https://id.corp.example".into();
        assert!(binding.validate(None).is_err());
    }

    #[test]
    fn binding_requires_opaque_identity_fields() {
        let service = custom("https://api.corp.example", "https://id.corp.example");
        let binding = AccountBinding {
            subject: String::new(),
            workspace: "w".into(),
            issuer: "https://id.corp.example".into(),
            bound_unix_ms: 0,
        };
        assert!(binding.validate(Some(&service)).is_err());
    }
}

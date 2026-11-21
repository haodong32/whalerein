//! The iOS app's authoritative client-configuration document.
//!
//! One versioned TOML owns the App selection and the typed Core instance
//! entries. Core validates every candidate through the same rules as
//! `Core::open`; this module only owns document shape, relative-path anchoring
//! and the commit order. All document writes are atomic through Core's shared
//! configuration writer.
//!
//! Commit rules:
//!
//! - the materialized per-instance TOML is written before the app document;
//! - the app document's `[app] active_instance` is the startup commit point;
//! - a new CA is staged under the instance directory and replaces `ca.pem`
//!   only after the app document commits, so the previous valid CA stays
//!   available until then.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{value, DocumentMut, Item, Table};
use whalerein_core::api::{ClientConfig, ClientService, Core, Environment, TransportSettings};

use crate::error::MobileError;

const APP_DOCUMENT_VERSION: u32 = 1;
const INSTANCE_VERSION: u32 = 1;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PEM_BYTES: usize = 1024 * 1024;
const ACTIVE_CLOUD_ID: &str = "cloud-production";
const STANDALONE_ID: &str = "standalone";
const LOCAL_ENVIRONMENT: &str = "local";

/// App-level settings exposed to Swift.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppSettingsView {
    pub active_instance_id: String,
    pub reasoning_visible: bool,
}

/// One typed instance entry with app-document-relative paths resolved.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InstanceView {
    pub id: String,
    pub service: String,
    pub environment: Option<String>,
    /// Absolute data directory resolved against the app document.
    pub data_dir: String,
    /// Absolute materialized Core TOML path for this instance.
    pub config_path: String,
    pub has_standalone_ca: bool,
    /// Absolute CA bundle path when the instance references one.
    pub standalone_ca_path: Option<String>,
    /// True when the local Cloud instance pins an imported root CA.
    pub has_cloud_ca: bool,
    /// Absolute local Cloud CA bundle path when one is referenced.
    pub cloud_ca_path: Option<String>,
}

/// The whole authoritative document as Swift sees it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppConfigView {
    pub path: String,
    pub version: u32,
    pub app: AppSettingsView,
    pub instances: Vec<InstanceView>,
}

/// Editable instance fields. `data_dir` is relative to the app document.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct InstanceInput {
    pub id: String,
    pub service: String,
    pub environment: Option<String>,
    pub data_dir: String,
    pub standalone_ca_pem: Option<String>,
    /// Imported mkcert root bundle for the local Cloud instance.
    pub cloud_ca_pem: Option<String>,
}

#[derive(Debug, Clone)]
struct InstanceRecord {
    service: String,
    data_dir: String,
    cloud_environment: Option<String>,
    standalone_ca_certificate: Option<String>,
    cloud_ca_certificate: Option<String>,
    /// Preserved verbatim so editing another field never drops a transport
    /// profile or a future supported field.
    transport: Option<Item>,
}

#[derive(Debug, Clone)]
struct ParsedDocument {
    path: PathBuf,
    doc: DocumentMut,
    active_instance: String,
    reasoning_visible: bool,
    instances: BTreeMap<String, InstanceRecord>,
}

impl AppConfigView {
    #[cfg(test)]
    fn active_instance(&self) -> Option<&InstanceView> {
        self.instances
            .iter()
            .find(|instance| instance.id == self.app.active_instance_id)
    }
}

impl ParsedDocument {
    fn view(&self) -> AppConfigView {
        let base = self.base();
        AppConfigView {
            path: self.path.to_string_lossy().into_owned(),
            version: APP_DOCUMENT_VERSION,
            app: AppSettingsView {
                active_instance_id: self.active_instance.clone(),
                reasoning_visible: self.reasoning_visible,
            },
            instances: self
                .instances
                .iter()
                .map(|(id, record)| instance_view(&base, id, record))
                .collect(),
        }
    }

    fn base(&self) -> PathBuf {
        app_base(&self.path)
    }
}

// ---------------------------------------------------------------------------
// Exported surface
// ---------------------------------------------------------------------------

/// Create the document on first launch and materialize its active instance.
///
/// An existing document is never overwritten. Legacy `preferences.json` and
/// `core/` paths are never read, migrated or deleted.
#[uniffi::export]
pub fn app_config_bootstrap(path: String) -> Result<AppConfigView, MobileError> {
    bootstrap(Path::new(&path))
}

/// Read the authoritative document without writing anything.
#[uniffi::export]
pub fn app_config_read(path: String) -> Result<AppConfigView, MobileError> {
    let parsed = read_document(Path::new(&path))?;
    Ok(parsed.view())
}

/// Validate a candidate instance without changing the document.
#[uniffi::export]
pub fn app_config_validate_instance(
    path: String,
    input: InstanceInput,
) -> Result<AppConfigView, MobileError> {
    let parsed = read_document(Path::new(&path))?;
    let candidate = candidate_record(&parsed, &input)?;
    validate_candidate(
        &parsed,
        &input.id,
        &candidate,
        input.standalone_ca_pem.as_deref(),
        input.cloud_ca_pem.as_deref(),
    )?;
    Ok(parsed.view())
}

/// Save one instance and materialize it before committing the document.
#[uniffi::export]
pub fn app_config_save_instance(
    path: String,
    input: InstanceInput,
) -> Result<AppConfigView, MobileError> {
    save_instance(Path::new(&path), input)
}

/// Make one existing instance the startup selection.
///
/// The destination is materialized and validated before the app document
/// commits. A failure leaves the previous document and selection untouched.
#[uniffi::export]
pub fn app_config_set_active(path: String, id: String) -> Result<AppConfigView, MobileError> {
    set_active(Path::new(&path), &id)
}

/// Remove one non-active instance entry. Instance data is left on disk.
#[uniffi::export]
pub fn app_config_remove_instance(path: String, id: String) -> Result<AppConfigView, MobileError> {
    let path = Path::new(&path);
    let mut parsed = read_document(path)?;
    validate_id(&id)?;
    if parsed.active_instance == id {
        return Err(MobileError::conflict());
    }
    if parsed.instances.remove(&id).is_none() {
        return Err(MobileError::not_found());
    }
    remove_instance_table(&mut parsed.doc, &id);
    commit_document(&parsed, &write_document)?;
    read_document(path).map(|parsed| parsed.view())
}

/// Remove the local Cloud CA reference and delete its instance bundle.
#[uniffi::export]
pub fn app_config_remove_cloud_ca(path: String, id: String) -> Result<AppConfigView, MobileError> {
    let path = Path::new(&path);
    let mut parsed = read_document(path)?;
    validate_id(&id)?;
    let Some(record) = parsed.instances.get(&id).cloned() else {
        return Err(MobileError::not_found());
    };
    let Some(ca_reference) = record.cloud_ca_certificate.clone() else {
        return Ok(parsed.view());
    };
    let mut next = record;
    next.cloud_ca_certificate = None;
    write_instance_table(&mut parsed.doc, &id, &next);
    parsed.instances.insert(id.clone(), next);
    materialize(&parsed, &id)?;
    commit_document(&parsed, &write_document)?;
    remove_owned_bundle(&parsed.base(), &ca_reference)?;
    read_document(path).map(|parsed| parsed.view())
}

fn remove_owned_bundle(base: &Path, reference: &str) -> Result<(), MobileError> {
    match fs::remove_file(resolve_reference(base, reference)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(MobileError::internal()),
    }
}

/// Persist only the App-section reasoning visibility flag.
#[uniffi::export]
pub fn app_config_set_reasoning_visible(
    path: String,
    visible: bool,
) -> Result<AppConfigView, MobileError> {
    let path = Path::new(&path);
    let mut parsed = read_document(path)?;
    parsed.reasoning_visible = visible;
    app_table_mut(&mut parsed.doc)?["reasoning_visible"] = value(visible);
    commit_document(&parsed, &write_document)?;
    read_document(path).map(|parsed| parsed.view())
}

/// Remove the Standalone CA reference and delete the instance-owned bundle.
#[uniffi::export]
pub fn app_config_remove_ca(path: String, id: String) -> Result<AppConfigView, MobileError> {
    let path = Path::new(&path);
    let mut parsed = read_document(path)?;
    validate_id(&id)?;
    let Some(record) = parsed.instances.get(&id).cloned() else {
        return Err(MobileError::not_found());
    };
    let Some(ca_reference) = record.standalone_ca_certificate.clone() else {
        return Ok(parsed.view());
    };
    let mut next = record;
    next.standalone_ca_certificate = None;
    write_instance_table(&mut parsed.doc, &id, &next);
    parsed.instances.insert(id.clone(), next);
    materialize(&parsed, &id)?;
    commit_document(&parsed, &write_document)?;
    let ca_path = resolve_reference(&parsed.base(), &ca_reference);
    match fs::remove_file(&ca_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(MobileError::internal()),
    }
    read_document(path).map(|parsed| parsed.view())
}

// ---------------------------------------------------------------------------
// Document lifecycle
// ---------------------------------------------------------------------------

type DocumentWriter<'a> = &'a dyn Fn(&Path, &[u8]) -> Result<(), MobileError>;

fn write_document(path: &Path, bytes: &[u8]) -> Result<(), MobileError> {
    Core::write_config_atomically(path, bytes).map_err(|_| MobileError::internal())
}

fn bootstrap(path: &Path) -> Result<AppConfigView, MobileError> {
    if validate_path(path).is_err() {
        return Err(MobileError::invalid_input());
    }
    let path = absolute(path)?;
    if !path.exists() {
        let document = default_document();
        write_document(&path, document.to_string().as_bytes())?;
    }
    let parsed = read_document(&path)?;
    materialize(&parsed, &parsed.active_instance)?;
    read_document(&path).map(|parsed| parsed.view())
}

fn read_document(path: &Path) -> Result<ParsedDocument, MobileError> {
    validate_path(path)?;
    let path = absolute(path)?;
    let text = fs::read_to_string(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MobileError::not_found()
        } else {
            MobileError::internal()
        }
    })?;
    parse_document(path, &text)
}

fn parse_document(path: PathBuf, text: &str) -> Result<ParsedDocument, MobileError> {
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|_| MobileError::invalid_input())?;
    let version = doc
        .get("version")
        .and_then(Item::as_integer)
        .ok_or_else(MobileError::invalid_input)?;
    if version != i64::from(APP_DOCUMENT_VERSION) {
        return Err(MobileError::invalid_input());
    }
    let app = doc
        .get("app")
        .and_then(Item::as_table)
        .ok_or_else(MobileError::invalid_input)?;
    let active_instance = app
        .get("active_instance")
        .and_then(Item::as_str)
        .ok_or_else(MobileError::invalid_input)?
        .to_owned();
    validate_id(&active_instance)?;
    let reasoning_visible = match app.get("reasoning_visible") {
        None => false,
        Some(item) => item.as_bool().ok_or_else(MobileError::invalid_input)?,
    };
    let instances_table = doc
        .get("instances")
        .and_then(Item::as_table)
        .ok_or_else(MobileError::invalid_input)?;
    let mut instances = BTreeMap::new();
    for (id, item) in instances_table.iter() {
        validate_id(id)?;
        let table = item.as_table().ok_or_else(MobileError::invalid_input)?;
        instances.insert(id.to_owned(), parse_instance(table)?);
    }
    if !instances.contains_key(&active_instance) {
        return Err(MobileError::invalid_input());
    }
    Ok(ParsedDocument {
        path,
        doc,
        active_instance,
        reasoning_visible,
        instances,
    })
}

fn parse_instance(table: &Table) -> Result<InstanceRecord, MobileError> {
    let version = table
        .get("version")
        .and_then(Item::as_integer)
        .ok_or_else(MobileError::invalid_input)?;
    if version != i64::from(INSTANCE_VERSION) {
        return Err(MobileError::invalid_input());
    }
    let service = table
        .get("service")
        .and_then(Item::as_str)
        .ok_or_else(MobileError::invalid_input)?
        .to_owned();
    parse_service(&service)?;
    let data_dir = table
        .get("data_dir")
        .and_then(Item::as_str)
        .ok_or_else(MobileError::invalid_input)?
        .to_owned();
    if data_dir.is_empty() {
        return Err(MobileError::invalid_input());
    }
    let cloud_environment = match table.get("cloud_environment") {
        None => None,
        Some(item) => Some(
            item.as_str()
                .ok_or_else(MobileError::invalid_input)?
                .to_owned(),
        ),
    };
    if let Some(environment) = &cloud_environment {
        parse_environment(environment)?;
    }
    let standalone_ca_certificate = match table.get("standalone_ca_certificate") {
        None => None,
        Some(item) => Some(
            item.as_str()
                .ok_or_else(MobileError::invalid_input)?
                .to_owned(),
        ),
    };
    if let Some(path) = &standalone_ca_certificate {
        if path.is_empty() {
            return Err(MobileError::invalid_input());
        }
    }
    let cloud_ca_certificate = match table.get("cloud_ca_certificate") {
        None => None,
        Some(item) => Some(
            item.as_str()
                .ok_or_else(MobileError::invalid_input)?
                .to_owned(),
        ),
    };
    if let Some(path) = &cloud_ca_certificate {
        if path.is_empty() {
            return Err(MobileError::invalid_input());
        }
    }
    Ok(InstanceRecord {
        service,
        data_dir,
        cloud_environment,
        standalone_ca_certificate,
        cloud_ca_certificate,
        transport: table.get("transport").cloned(),
    })
}

fn default_document() -> DocumentMut {
    let mut doc = DocumentMut::new();
    doc["version"] = value(i64::from(APP_DOCUMENT_VERSION));
    let mut app = Table::new();
    app["active_instance"] = value(ACTIVE_CLOUD_ID);
    app["reasoning_visible"] = value(false);
    doc["app"] = Item::Table(app);

    let mut instances = Table::new();
    instances[ACTIVE_CLOUD_ID] = Item::Table(instance_table(
        "whalerein_cloud",
        "instances/cloud-production/data",
        Some("production"),
        None,
    ));
    instances[STANDALONE_ID] = Item::Table(instance_table(
        "standalone",
        "instances/standalone/data",
        None,
        None,
    ));
    doc["instances"] = Item::Table(instances);
    doc
}

fn instance_table(
    service: &str,
    data_dir: &str,
    environment: Option<&str>,
    ca: Option<&str>,
) -> Table {
    let mut table = Table::new();
    table["version"] = value(i64::from(INSTANCE_VERSION));
    table["service"] = value(service);
    table["data_dir"] = value(data_dir);
    if let Some(environment) = environment {
        table["cloud_environment"] = value(environment);
    }
    if let Some(ca) = ca {
        table["standalone_ca_certificate"] = value(ca);
    }
    table
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

fn save_instance(path: &Path, input: InstanceInput) -> Result<AppConfigView, MobileError> {
    let mut parsed = read_document(path)?;
    let mut record = candidate_record(&parsed, &input)?;
    validate_candidate(
        &parsed,
        &input.id,
        &record,
        input.standalone_ca_pem.as_deref(),
        input.cloud_ca_pem.as_deref(),
    )?;

    let base = parsed.base();
    let previous_standalone = record.standalone_ca_certificate.clone();
    let previous_cloud = record.cloud_ca_certificate.clone();

    // Stage new bundles under the instance directory before touching the app
    // document. The previous valid files stay in place until commit.
    let standalone = prepare_ca(
        &base,
        &input.id,
        "ca.pem",
        "ca",
        input.standalone_ca_pem.as_deref(),
    )?;
    let cloud = prepare_ca(
        &base,
        &input.id,
        "cloud-ca.pem",
        "cloud-ca",
        input.cloud_ca_pem.as_deref(),
    )?;
    if standalone.staged.is_some() {
        record.standalone_ca_certificate = Some(format!("instances/{}/ca.pem", input.id));
    }
    if cloud.staged.is_some() {
        record.cloud_ca_certificate = Some(format!("instances/{}/cloud-ca.pem", input.id));
    }
    // A service never keeps the other service's CA reference.
    match parse_service(&record.service)? {
        ClientService::WhaleReinCloud => record.standalone_ca_certificate = None,
        ClientService::Standalone => record.cloud_ca_certificate = None,
    }

    write_instance_table(&mut parsed.doc, &input.id, &record);
    parsed.instances.insert(input.id.clone(), record);

    // The materialized instance file is written before the app document.
    if let Err(error) = materialize(&parsed, &input.id) {
        standalone.discard();
        cloud.discard();
        return Err(error);
    }
    if let Err(error) = commit_document(&parsed, &write_document) {
        standalone.discard();
        cloud.discard();
        return Err(error);
    }
    standalone.finish();
    cloud.finish();

    // A service change or explicit removal drops the old instance-owned bundle
    // after the document reference is gone.
    if record_ca_is_none(&parsed, &input.id, false) {
        if let Some(previous) = previous_standalone {
            let _ = fs::remove_file(resolve_reference(&base, &previous));
        }
    }
    if record_ca_is_none(&parsed, &input.id, true) {
        if let Some(previous) = previous_cloud {
            let _ = fs::remove_file(resolve_reference(&base, &previous));
        }
    }
    read_document(path).map(|parsed| parsed.view())
}

/// One staged CA bundle plus its committed target.
struct PreparedCa {
    staged: Option<PathBuf>,
    target: PathBuf,
    created: bool,
}

impl PreparedCa {
    fn discard(&self) {
        if let Some(staged) = &self.staged {
            let _ = fs::remove_file(staged);
        }
        if self.created {
            let _ = fs::remove_file(&self.target);
        }
    }

    fn finish(&self) {
        if let Some(staged) = &self.staged {
            if self.created {
                let _ = fs::remove_file(staged);
            } else if fs::rename(staged, &self.target).is_err() {
                let _ = fs::remove_file(staged);
            }
        }
    }
}

fn prepare_ca(
    base: &Path,
    id: &str,
    file_name: &str,
    prefix: &str,
    pem: Option<&str>,
) -> Result<PreparedCa, MobileError> {
    let target = instance_directory(base, id).join(file_name);
    let Some(pem) = pem else {
        return Ok(PreparedCa {
            staged: None,
            target,
            created: false,
        });
    };
    let staged = stage_ca(base, id, pem, prefix)?;
    if target.exists() {
        return Ok(PreparedCa {
            staged: Some(staged),
            target,
            created: false,
        });
    }
    // First import: install the validated bundle before materializing because
    // the materialized TOML must open on its own. This is not the commit point.
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|_| MobileError::internal())?;
    }
    if let Err(error) = fs::copy(&staged, &target) {
        let _ = fs::remove_file(&staged);
        let _ = error;
        return Err(MobileError::internal());
    }
    Ok(PreparedCa {
        staged: Some(staged),
        target,
        created: true,
    })
}

fn record_ca_is_none(parsed: &ParsedDocument, id: &str, cloud: bool) -> bool {
    parsed.instances.get(id).is_some_and(|record| {
        if cloud {
            record.cloud_ca_certificate.is_none()
        } else {
            record.standalone_ca_certificate.is_none()
        }
    })
}

fn set_active(path: &Path, id: &str) -> Result<AppConfigView, MobileError> {
    set_active_with(path, id, &write_document)
}

fn set_active_with(
    path: &Path,
    id: &str,
    writer: DocumentWriter<'_>,
) -> Result<AppConfigView, MobileError> {
    let mut parsed = read_document(path)?;
    validate_id(id)?;
    if !parsed.instances.contains_key(id) {
        return Err(MobileError::not_found());
    }
    // Prepare the destination before the selection commits.
    materialize(&parsed, id)?;
    parsed.active_instance = id.to_owned();
    app_table_mut(&mut parsed.doc)?["active_instance"] = value(id);
    commit_document(&parsed, writer)?;
    read_document(path).map(|parsed| parsed.view())
}

fn commit_document(parsed: &ParsedDocument, writer: DocumentWriter<'_>) -> Result<(), MobileError> {
    writer(&parsed.path, parsed.doc.to_string().as_bytes())
}

fn candidate_record(
    parsed: &ParsedDocument,
    input: &InstanceInput,
) -> Result<InstanceRecord, MobileError> {
    validate_id(&input.id)?;
    let service = parse_service(&input.service)?;
    let environment = match input.environment.as_deref() {
        None => None,
        Some(environment) => Some(environment_name(parse_environment(environment)?).to_owned()),
    };
    if service == ClientService::WhaleReinCloud {
        if environment.is_none() {
            return Err(MobileError::invalid_input());
        }
    } else if environment.is_some() {
        return Err(MobileError::invalid_input());
    }
    if input.data_dir.is_empty() || input.data_dir.len() > MAX_PATH_BYTES {
        return Err(MobileError::invalid_input());
    }
    if input.standalone_ca_pem.is_some() && service != ClientService::Standalone {
        return Err(MobileError::invalid_input());
    }
    if input.cloud_ca_pem.is_some()
        && (service != ClientService::WhaleReinCloud
            || environment.as_deref() != Some(LOCAL_ENVIRONMENT))
    {
        return Err(MobileError::invalid_input());
    }
    let existing = parsed.instances.get(&input.id);
    Ok(InstanceRecord {
        service: input.service.clone(),
        data_dir: input.data_dir.clone(),
        cloud_environment: environment,
        standalone_ca_certificate: match service {
            ClientService::Standalone => {
                existing.and_then(|record| record.standalone_ca_certificate.clone())
            }
            ClientService::WhaleReinCloud => None,
        },
        cloud_ca_certificate: match service {
            ClientService::WhaleReinCloud => {
                existing.and_then(|record| record.cloud_ca_certificate.clone())
            }
            ClientService::Standalone => None,
        },
        transport: existing.and_then(|record| record.transport.clone()),
    })
}

fn validate_candidate(
    parsed: &ParsedDocument,
    id: &str,
    record: &InstanceRecord,
    standalone_ca_pem: Option<&str>,
    cloud_ca_pem: Option<&str>,
) -> Result<(), MobileError> {
    let instance_dir = instance_directory(&parsed.base(), id);
    let config_path = instance_dir.join("instance.toml");
    fs::create_dir_all(&instance_dir).map_err(|_| MobileError::internal())?;
    let mut temporary = Vec::new();
    let stage = |pem: &str, suffix: &str| -> Result<PathBuf, MobileError> {
        if pem.len() > MAX_PEM_BYTES {
            return Err(MobileError::invalid_input());
        }
        let staged = instance_dir.join(format!(
            ".ca-validate-{}-{}",
            suffix,
            Core::new_command_id()
        ));
        fs::write(&staged, pem.as_bytes()).map_err(|_| MobileError::internal())?;
        Ok(staged)
    };
    let standalone_override = match standalone_ca_pem {
        Some(pem) => {
            let staged = stage(pem, "standalone")?;
            temporary.push(staged.clone());
            Some(staged)
        }
        None => None,
    };
    let cloud_override = match cloud_ca_pem {
        Some(pem) => {
            let staged = stage(pem, "cloud")?;
            temporary.push(staged.clone());
            Some(staged)
        }
        None => None,
    };
    let result = client_config(
        record,
        &parsed.base(),
        &instance_dir,
        standalone_override.as_deref(),
        cloud_override.as_deref(),
    )
    .and_then(|config| {
        Core::validate_client_config(&config_path, &config)
            .map_err(|_| MobileError::invalid_input())
    });
    for staged in temporary {
        let _ = fs::remove_file(staged);
    }
    result
}

fn stage_ca(base: &Path, id: &str, pem: &str, prefix: &str) -> Result<PathBuf, MobileError> {
    if pem.is_empty() || pem.len() > MAX_PEM_BYTES {
        return Err(MobileError::invalid_input());
    }
    let directory = instance_directory(base, id);
    fs::create_dir_all(&directory).map_err(|_| MobileError::internal())?;
    let staged = directory.join(format!(".{prefix}-stage-{}", Core::new_command_id()));
    fs::write(&staged, pem.as_bytes()).map_err(|_| MobileError::internal())?;
    Ok(staged)
}

fn materialize(parsed: &ParsedDocument, id: &str) -> Result<(), MobileError> {
    let record = parsed
        .instances
        .get(id)
        .ok_or_else(MobileError::not_found)?;
    let instance_dir = instance_directory(&parsed.base(), id);
    let config_path = instance_dir.join("instance.toml");
    let config = client_config(record, &parsed.base(), &instance_dir, None, None)?;
    Core::validate_client_config(&config_path, &config)
        .map_err(|_| MobileError::invalid_input())?;
    let text = toml_edit::ser::to_string(&config).map_err(|_| MobileError::internal())?;
    Core::write_config_atomically(&config_path, text.as_bytes())
        .map_err(|_| MobileError::internal())
}

/// Build a typed Core config whose relative paths resolve from the
/// materialized instance file's directory.
fn client_config(
    record: &InstanceRecord,
    base: &Path,
    instance_dir: &Path,
    ca_override: Option<&Path>,
    cloud_ca_override: Option<&Path>,
) -> Result<ClientConfig, MobileError> {
    let service = parse_service(&record.service)?;
    let environment = match record.cloud_environment.as_deref() {
        None => None,
        Some(environment) => Some(parse_environment(environment)?),
    };
    let data_dir = relative_to_instance(base, &record.data_dir, instance_dir);
    let standalone_ca_certificate = match ca_override {
        Some(staged) => Some(relative_or_absolute(staged, instance_dir)),
        None => record
            .standalone_ca_certificate
            .as_deref()
            .map(|reference| {
                relative_or_absolute(&resolve_reference_path(base, reference), instance_dir)
            }),
    };
    let cloud_ca_certificate = match cloud_ca_override {
        Some(staged) => Some(relative_or_absolute(staged, instance_dir)),
        None => record.cloud_ca_certificate.as_deref().map(|reference| {
            relative_or_absolute(&resolve_reference_path(base, reference), instance_dir)
        }),
    };
    let transport = match &record.transport {
        None => None,
        Some(item) => Some(
            toml_edit::de::from_str::<TransportSettings>(&item.to_string())
                .map_err(|_| MobileError::invalid_input())?,
        ),
    };
    Ok(ClientConfig {
        version: INSTANCE_VERSION,
        service,
        data_dir,
        cloud_environment: environment,
        standalone_ca_certificate,
        cloud_ca_certificate,
        transport,
    })
}

/// `data_dir` is stored relative to the app document. The materialized file
/// lives under `instances/<id>/`, so express the same target relative to that
/// directory when possible.
fn relative_to_instance(base: &Path, reference: &str, instance_dir: &Path) -> PathBuf {
    let absolute = resolve_reference_path(base, reference);
    relative_or_absolute(&absolute, instance_dir)
}

fn relative_or_absolute(path: &Path, directory: &Path) -> PathBuf {
    let resolved_path = absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let resolved_directory = absolute(directory).unwrap_or_else(|_| directory.to_path_buf());
    resolved_path
        .strip_prefix(&resolved_directory)
        .map(Path::to_path_buf)
        .unwrap_or(resolved_path)
}

fn resolve_reference(base: &Path, reference: &str) -> PathBuf {
    resolve_reference_path(base, reference)
}

fn resolve_reference_path(base: &Path, reference: &str) -> PathBuf {
    let path = Path::new(reference);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

// ---------------------------------------------------------------------------
// Views and document editing
// ---------------------------------------------------------------------------

fn instance_view(base: &Path, id: &str, record: &InstanceRecord) -> InstanceView {
    let ca_path = record
        .standalone_ca_certificate
        .as_deref()
        .map(|reference| resolve_reference(base, reference));
    let cloud_ca_path = record
        .cloud_ca_certificate
        .as_deref()
        .map(|reference| resolve_reference(base, reference));
    InstanceView {
        id: id.to_owned(),
        service: record.service.clone(),
        environment: record.cloud_environment.clone(),
        data_dir: resolve_reference(base, &record.data_dir)
            .to_string_lossy()
            .into_owned(),
        config_path: instance_directory(base, id)
            .join("instance.toml")
            .to_string_lossy()
            .into_owned(),
        has_standalone_ca: ca_path.as_ref().is_some_and(|path| path.is_file()),
        standalone_ca_path: ca_path.map(|path| path.to_string_lossy().into_owned()),
        has_cloud_ca: cloud_ca_path.as_ref().is_some_and(|path| path.is_file()),
        cloud_ca_path: cloud_ca_path.map(|path| path.to_string_lossy().into_owned()),
    }
}

fn write_instance_table(doc: &mut DocumentMut, id: &str, record: &InstanceRecord) {
    let instances = doc
        .entry("instances")
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(instances) = instances.as_table_mut() else {
        return;
    };
    let table = instances
        .entry(id)
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(table) = table.as_table_mut() else {
        return;
    };
    table["version"] = value(i64::from(INSTANCE_VERSION));
    table["service"] = value(record.service.as_str());
    table["data_dir"] = value(record.data_dir.as_str());
    match record.cloud_environment.as_deref() {
        Some(environment) => table["cloud_environment"] = value(environment),
        None => {
            table.remove("cloud_environment");
        }
    }
    match record.standalone_ca_certificate.as_deref() {
        Some(reference) => table["standalone_ca_certificate"] = value(reference),
        None => {
            table.remove("standalone_ca_certificate");
        }
    }
    match record.cloud_ca_certificate.as_deref() {
        Some(reference) => table["cloud_ca_certificate"] = value(reference),
        None => {
            table.remove("cloud_ca_certificate");
        }
    }
    // `transport` and any future supported key are preserved verbatim.
}

fn remove_instance_table(doc: &mut DocumentMut, id: &str) {
    if let Some(instances) = doc.get_mut("instances").and_then(Item::as_table_mut) {
        instances.remove(id);
    }
}

fn app_table_mut(doc: &mut DocumentMut) -> Result<&mut Table, MobileError> {
    doc.get_mut("app")
        .and_then(Item::as_table_mut)
        .ok_or_else(MobileError::invalid_input)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn validate_path(path: &Path) -> Result<(), MobileError> {
    let text = path.to_string_lossy();
    if text.is_empty() || text.len() > MAX_PATH_BYTES {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), MobileError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn parse_service(value: &str) -> Result<ClientService, MobileError> {
    match value {
        "whalerein_cloud" => Ok(ClientService::WhaleReinCloud),
        "standalone" => Ok(ClientService::Standalone),
        _ => Err(MobileError::invalid_input()),
    }
}

fn environment_name(value: Environment) -> &'static str {
    match value {
        Environment::Production => "production",
        Environment::Staging => "staging",
        Environment::Local => "local",
        _ => "unknown",
    }
}

fn parse_environment(value: &str) -> Result<Environment, MobileError> {
    match value {
        "production" => Ok(Environment::Production),
        "staging" => Ok(Environment::Staging),
        "local" => Ok(Environment::Local),
        _ => Err(MobileError::invalid_input()),
    }
}

fn app_base(path: &Path) -> PathBuf {
    absolute(path)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn instance_directory(base: &Path, id: &str) -> PathBuf {
    base.join("instances").join(id)
}

fn absolute(path: &Path) -> Result<PathBuf, MobileError> {
    if path.as_os_str().is_empty() {
        return Err(MobileError::invalid_input());
    }
    std::path::absolute(path).map_err(|_| MobileError::invalid_input())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const VALID_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBazCCARKgAwIBAgIUCYEJf6AWvrWHzQnqALYWDNrwYfgwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRV2hhbGVSZWluIFRlc3QgQ0EwHhcNMjYxMTE0MDQzMDA2WhcN
MzYxMTExMDQzMDA2WjAcMRowGAYDVQQDDBFXaGFsZVJlaW4gVGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABIDzEtmehsRaR0Vx65NWD4y1kflMTDFblTA8
64kOENsLgnPx+O1z8BV4JzL5+wqRd/gcxvR5WULQOuxjCoDpm4ejMjAwMB0GA1Ud
DgQWBBQ0nEJTr/TR1J9+vLqmx9euDviHNzAPBgNVHRMBAf8EBTADAQH/MAoGCCqG
SM49BAMCA0cAMEQCIEyFy7p0NegnWrzl41pLP89cCp9itTXSeJ8I13iKpm28AiAR
kg3kmwrRgJT2uxIVgusk+cBIKzWjxMwkC7uvy8RsrQ==
-----END CERTIFICATE-----
";

    fn document(root: &Path) -> String {
        root.join("client/app.toml").to_string_lossy().into_owned()
    }

    fn bootstrap(root: &Path) -> AppConfigView {
        app_config_bootstrap(document(root)).unwrap()
    }

    fn save(root: &Path, input: InstanceInput) -> Result<AppConfigView, MobileError> {
        app_config_save_instance(document(root), input)
    }

    fn standalone_input(ca: Option<&str>) -> InstanceInput {
        InstanceInput {
            id: "standalone".into(),
            service: "standalone".into(),
            environment: None,
            data_dir: "instances/standalone/data".into(),
            standalone_ca_pem: ca.map(str::to_owned),
            cloud_ca_pem: None,
        }
    }

    #[test]
    fn bootstrap_creates_both_instances_only_once() {
        let root = tempfile::tempdir().unwrap();
        let view = bootstrap(root.path());
        assert_eq!(view.app.active_instance_id, "cloud-production");
        assert_eq!(
            view.instances
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["cloud-production", "standalone"]
        );
        let standalone = view
            .instances
            .iter()
            .find(|instance| instance.id == "standalone")
            .unwrap();
        assert!(
            standalone
                .data_dir
                .ends_with("client/instances/standalone/data"),
            "{:?}",
            standalone
        );
        assert!(standalone
            .config_path
            .ends_with("client/instances/standalone/instance.toml"));
        assert!(Path::new(&view.path).is_file());
        let active = view.active_instance().unwrap();
        assert!(Path::new(&active.config_path).is_file());

        // An existing document is never overwritten.
        let path = Path::new(&view.path);
        let mut text = fs::read_to_string(path).unwrap();
        text = text.replace("reasoning_visible = false", "reasoning_visible = true");
        fs::write(path, &text).unwrap();
        let again = bootstrap(root.path());
        assert!(again.app.reasoning_visible);
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn malformed_document_is_an_error_and_is_left_untouched() {
        let root = tempfile::tempdir().unwrap();
        let document_path = document(root.path());
        let path = Path::new(&document_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "version = 1\n[app\n").unwrap();
        let before = fs::read(path).unwrap();
        assert!(app_config_read(document(root.path())).is_err());
        assert!(app_config_bootstrap(document(root.path())).is_err());
        assert_eq!(fs::read(path).unwrap(), before);

        fs::write(
            path,
            "version = 1\n[app]\nactive_instance = 'cloud-production'\n[instances]\n",
        )
        .unwrap();
        assert!(app_config_read(document(root.path())).is_err());
    }

    #[test]
    fn editing_one_field_preserves_transport_and_other_instances() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let document_path = document(root.path());
        let path = Path::new(&document_path);
        let mut text = fs::read_to_string(path).unwrap();
        text = text.replace(
            "data_dir = \"instances/cloud-production/data\"",
            "data_dir = \"instances/cloud-production/data\"\n[instances.cloud-production.transport]\nprofile = \"compact\"\n",
        );
        fs::write(path, &text).unwrap();
        let before = fs::read_to_string(path).unwrap();

        let view = save(root.path(), standalone_input(None)).unwrap();
        let after = fs::read_to_string(path).unwrap();
        assert!(after.contains("profile = \"compact\""));
        assert!(after.contains("cloud-production"));
        assert!(after.contains("instances/cloud-production/data"));
        assert_eq!(
            view.instances
                .iter()
                .find(|instance| instance.id == "cloud-production")
                .unwrap()
                .service,
            "whalerein_cloud"
        );
        // The standalone edit did not rewrite the cloud transport block.
        let cloud = after
            .split("[instances.cloud-production]")
            .nth(1)
            .unwrap()
            .split("[instances.standalone]")
            .next()
            .unwrap();
        assert!(cloud.contains("profile = \"compact\""));
        assert!(before.contains("profile = \"compact\""));
    }

    #[test]
    fn invalid_pem_keeps_the_prior_document_and_ca() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let document_path = document(root.path());
        let path = Path::new(&document_path);
        let before = fs::read(path).unwrap();
        let error = save(root.path(), standalone_input(Some("not a certificate"))).unwrap_err();
        assert_eq!(error.code(), "invalid_input");
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(!root
            .path()
            .join("client/instances/standalone/ca.pem")
            .exists());

        save(root.path(), standalone_input(Some(VALID_PEM))).unwrap();
        let with_ca = fs::read(path).unwrap();
        let error = save(root.path(), standalone_input(Some("not a certificate"))).unwrap_err();
        assert_eq!(error.code(), "invalid_input");
        assert_eq!(fs::read(path).unwrap(), with_ca);
        assert!(root
            .path()
            .join("client/instances/standalone/ca.pem")
            .is_file());
    }

    #[test]
    fn valid_pem_is_staged_materialized_and_removable() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let view = save(root.path(), standalone_input(Some(VALID_PEM))).unwrap();
        let standalone = view
            .instances
            .iter()
            .find(|instance| instance.id == "standalone")
            .unwrap();
        assert!(standalone.has_standalone_ca);
        let ca_path = standalone.standalone_ca_path.as_deref().unwrap();
        assert!(ca_path.ends_with("client/instances/standalone/ca.pem"));
        assert_eq!(fs::read_to_string(ca_path).unwrap(), VALID_PEM);
        let materialized = fs::read_to_string(
            root.path()
                .join("client/instances/standalone/instance.toml"),
        )
        .unwrap();
        assert!(materialized.contains("standalone_ca_certificate"));

        // The staged file is gone after commit.
        let leftovers = fs::read_dir(root.path().join("client/instances/standalone"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ca-"))
            .count();
        assert_eq!(leftovers, 0);

        let removed = app_config_remove_ca(document(root.path()), "standalone".into()).unwrap();
        let standalone_after = removed
            .instances
            .iter()
            .find(|instance| instance.id == "standalone")
            .unwrap();
        assert!(!standalone_after.has_standalone_ca);
        assert!(!Path::new(ca_path).exists());
        let materialized = fs::read_to_string(
            root.path()
                .join("client/instances/standalone/instance.toml"),
        )
        .unwrap();
        assert!(!materialized.contains("standalone_ca_certificate"));
    }

    #[test]
    fn activation_materializes_before_the_document_and_keeps_old_selection_on_write_failure() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let document_path = document(root.path());
        let path = Path::new(&document_path);

        let log: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let writer = |target: &Path, bytes: &[u8]| -> Result<(), MobileError> {
            if target == path {
                let materialized = root
                    .path()
                    .join("client/instances/standalone/instance.toml");
                if !materialized.is_file() {
                    return Err(MobileError::internal());
                }
                log.borrow_mut().push("document".into());
            }
            Core::write_config_atomically(target, bytes).map_err(|_| MobileError::internal())
        };
        let view = set_active_with(path, "standalone", &writer).unwrap();
        assert_eq!(view.app.active_instance_id, "standalone");
        assert_eq!(log.borrow().as_slice(), ["document"]);

        let before = fs::read(path).unwrap();
        let failing = |_target: &Path, _bytes: &[u8]| Err::<(), _>(MobileError::internal());
        let error = set_active_with(path, "cloud-production", &failing).unwrap_err();
        assert_eq!(error.code(), "internal");
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(
            app_config_read(document(root.path()))
                .unwrap()
                .app
                .active_instance_id,
            "standalone"
        );
    }

    #[test]
    fn activation_rejects_unknown_and_invalid_targets() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        assert_eq!(
            app_config_set_active(document(root.path()), "missing".into())
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            app_config_set_active(document(root.path()), "Bad Id".into())
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            app_config_save_instance(
                document(root.path()),
                InstanceInput {
                    id: "cloud-production".into(),
                    service: "whalerein_cloud".into(),
                    environment: None,
                    data_dir: "instances/cloud-production/data".into(),
                    standalone_ca_pem: None,
                    cloud_ca_pem: None,
                },
            )
            .unwrap_err()
            .code(),
            "invalid_input"
        );
    }

    fn local_input(ca: Option<&str>, environment: &str) -> InstanceInput {
        InstanceInput {
            id: "cloud-local".into(),
            service: "whalerein_cloud".into(),
            environment: Some(environment.into()),
            data_dir: "instances/cloud-local/data".into(),
            standalone_ca_pem: None,
            cloud_ca_pem: ca.map(str::to_owned),
        }
    }

    #[test]
    fn local_cloud_ca_is_validated_materialized_and_removable() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());

        // A non-PEM bundle never becomes a local instance.
        let invalid = save(root.path(), local_input(Some("not a certificate"), "local"));
        assert_eq!(invalid.unwrap_err().code(), "invalid_input");

        let view = save(root.path(), local_input(Some(VALID_PEM), "local")).unwrap();
        let local = view
            .instances
            .iter()
            .find(|instance| instance.id == "cloud-local")
            .unwrap();
        assert!(local.has_cloud_ca);
        assert!(local
            .cloud_ca_path
            .as_deref()
            .is_some_and(|path| Path::new(path).is_file()));
        let materialized = fs::read_to_string(
            instance_directory(&root.path().join("client"), "cloud-local").join("instance.toml"),
        )
        .unwrap();
        assert!(materialized.contains("cloud_ca_certificate = \"cloud-ca.pem\""));

        // The override is restricted to the local environment and to Cloud.
        assert_eq!(
            save(root.path(), local_input(Some(VALID_PEM), "staging"))
                .unwrap_err()
                .code(),
            "invalid_input"
        );

        let cleared =
            app_config_remove_cloud_ca(document(root.path()), "cloud-local".into()).unwrap();
        assert!(
            !cleared
                .instances
                .iter()
                .find(|instance| instance.id == "cloud-local")
                .unwrap()
                .has_cloud_ca
        );
        assert!(!root
            .path()
            .join("client/instances/cloud-local/cloud-ca.pem")
            .exists());
    }

    #[test]
    fn legacy_state_is_never_read_or_mutated() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("preferences.json");
        fs::write(&legacy, br#"{"paired":true}"#).unwrap();
        fs::create_dir(root.path().join("core")).unwrap();
        fs::write(root.path().join("core/identity"), b"legacy").unwrap();

        bootstrap(root.path());
        save(root.path(), standalone_input(None)).unwrap();
        app_config_set_active(document(root.path()), "standalone".into()).unwrap();

        assert_eq!(fs::read(&legacy).unwrap(), br#"{"paired":true}"#);
        assert_eq!(
            fs::read(root.path().join("core/identity")).unwrap(),
            b"legacy"
        );
        assert!(!root.path().join("core/config.json").exists());
    }

    #[test]
    fn reasoning_visibility_edits_only_the_app_flag() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let document_path = document(root.path());
        let path = Path::new(&document_path);
        save(root.path(), standalone_input(None)).unwrap();
        let before = fs::read_to_string(path).unwrap();
        let view = app_config_set_reasoning_visible(document(root.path()), true).unwrap();
        assert!(view.app.reasoning_visible);
        let after = fs::read_to_string(path).unwrap();
        assert!(after.contains("reasoning_visible = true"));
        assert!(after.contains("instances/standalone/data"));
        assert_ne!(before, after);
        assert_eq!(
            app_config_remove_instance(document(root.path()), "cloud-production".into())
                .unwrap_err()
                .code(),
            "conflict"
        );
    }

    #[test]
    fn absolute_document_path_and_relative_references_resolve_consistently() {
        let root = tempfile::tempdir().unwrap();
        bootstrap(root.path());
        let view = app_config_read(document(root.path())).unwrap();
        assert!(Path::new(&view.path).is_absolute());
        assert!(view
            .instances
            .iter()
            .all(|instance| Path::new(&instance.data_dir).is_absolute()));
    }

    #[test]
    fn missing_document_read_reports_not_found() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            app_config_read(document(root.path())).unwrap_err().code(),
            "not_found"
        );
    }
}

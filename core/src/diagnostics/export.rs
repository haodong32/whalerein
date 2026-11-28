//! Safe local writes for one captured diagnostic snapshot.
//!
//! A new destination is created exclusively with private permissions. An
//! existing regular file is replaced only when the caller explicitly selected
//! [`DiagnosticExportMode::ReplaceExisting`], using a staged write in the same
//! directory so a failed write preserves the prior contents. Symbolic links,
//! directories and other non-regular destinations are refused before any
//! write; the target is never followed.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::api::config;
use crate::api::{
    CoreError, CoreResult, DiagnosticExportMode, DiagnosticExportTarget, DiagnosticsSnapshot,
    ExportManifest,
};

fn unsafe_target(message: &str) -> CoreError {
    CoreError::InvalidInput(message.into())
}

fn was_not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn already_exists(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists
}

/// Classify a destination without following a symbolic link.
///
/// `New` and `ExistingRegularFile` are the only safe states. A symbolic link,
/// directory, device, socket or other special file is refused with a stable
/// invalid-input error before any content is written.
pub fn inspect_target(path: &Path) -> CoreResult<DiagnosticExportTarget> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                Err(unsafe_target(
                    "diagnostic export target must not be a symbolic link",
                ))
            } else if file_type.is_dir() {
                Err(unsafe_target(
                    "diagnostic export target must be a file, not a directory",
                ))
            } else if file_type.is_file() {
                Ok(DiagnosticExportTarget::ExistingRegularFile)
            } else {
                Err(unsafe_target(
                    "diagnostic export target must be a regular file",
                ))
            }
        }
        Err(error) if was_not_found(&error) => Ok(DiagnosticExportTarget::New),
        Err(_) => Err(CoreError::Internal(
            "diagnostic export target could not be inspected".into(),
        )),
    }
}

/// Write the captured bytes according to `mode`.
pub fn write_snapshot(
    snapshot: &DiagnosticsSnapshot,
    path: &Path,
    mode: DiagnosticExportMode,
) -> CoreResult<ExportManifest> {
    if snapshot.bytes.is_empty() || snapshot.text.as_bytes() != snapshot.bytes.as_slice() {
        return Err(CoreError::Internal(
            "diagnostic capture is not a shareable document".into(),
        ));
    }
    if let Some(parent) = parent_directory(path) {
        std::fs::create_dir_all(parent).map_err(|_| {
            CoreError::Internal("diagnostic export directory could not be created".into())
        })?;
    }
    match mode {
        DiagnosticExportMode::CreateNew => create_new(path, &snapshot.bytes)?,
        DiagnosticExportMode::ReplaceExisting => replace_existing(path, &snapshot.bytes)?,
    }
    Ok(ExportManifest {
        schema: snapshot.schema.clone(),
        path: path.display().to_string(),
        fields: crate::api::ExportManifest::fields(),
        redactions: snapshot.redaction_fields.clone(),
        bytes: snapshot.bytes.len() as u64,
        capture_id: snapshot.capture_id.clone(),
        capture_time_unix_ms: snapshot.capture_time_unix_ms,
    })
}

fn parent_directory(path: &Path) -> Option<&Path> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Some(parent),
        Some(_) => Some(Path::new(".")),
        None => None,
    }
}

fn private_new_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn create_error(path: &Path, error: std::io::Error) -> CoreError {
    if already_exists(&error) {
        // A file that appeared after the new-target check survives unless the
        // caller explicitly authorized replacement.
        return match inspect_target(path) {
            Ok(_) => CoreError::Conflict(
                "diagnostic export target appeared during the write and was preserved".into(),
            ),
            Err(unsafe_target) => unsafe_target,
        };
    }
    CoreError::Internal("diagnostic bundle could not be written".into())
}

fn create_new(path: &Path, bytes: &[u8]) -> CoreResult<()> {
    let mut file = private_new_file(path).map_err(|error| create_error(path, error))?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        // The file is ours because creation was exclusive; a partial new file
        // must not be left behind as if it were a completed capture.
        let _ = std::fs::remove_file(path);
        return Err(CoreError::Internal(
            "diagnostic bundle could not be written".into(),
        ));
    }
    drop(file);
    config::set_private_permissions(path)?;
    sync_parent(path);
    Ok(())
}

fn replace_existing(path: &Path, bytes: &[u8]) -> CoreResult<()> {
    match inspect_target(path)? {
        DiagnosticExportTarget::New => return create_new(path, bytes),
        DiagnosticExportTarget::ExistingRegularFile => {}
    }
    let parent = parent_directory(path).unwrap_or_else(|| Path::new("."));
    let staged: PathBuf = parent.join(format!(
        ".whalerein-diagnostics-{}.tmp",
        crate::util::random_id(8)
    ));
    let result: CoreResult<()> = (|| {
        let mut file = private_new_file(&staged)
            .map_err(|_| CoreError::Internal("diagnostic bundle could not be written".into()))?;
        if file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .is_err()
        {
            return Err(CoreError::Internal(
                "diagnostic bundle could not be written".into(),
            ));
        }
        drop(file);
        config::set_private_permissions(&staged)?;
        // Re-check after consent and staging: a target that became a link,
        // directory or special file is an unsafe change and is refused.
        match inspect_target(path)? {
            DiagnosticExportTarget::New | DiagnosticExportTarget::ExistingRegularFile => {}
        }
        std::fs::rename(&staged, path).map_err(|_| {
            CoreError::Internal(
                "diagnostic bundle could not be replaced; the existing file was preserved".into(),
            )
        })?;
        config::set_private_permissions(path)?;
        sync_parent(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = parent_directory(path) {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{DiagnosticExportTarget, DiagnosticsSnapshot};

    fn snapshot() -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            schema: "whalerein/v1".into(),
            capture_id: "c".repeat(32),
            capture_time_unix_ms: 1,
            bytes: br#"{"schema":"whalerein/v1"}"#.to_vec(),
            text: r#"{"schema":"whalerein/v1"}"#.into(),
            summary: "summary".into(),
            redaction_fields: vec!["private keys".into()],
            last_failure_code: None,
            hosts: vec![],
        }
    }

    #[test]
    fn new_target_is_created_exclusively_and_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        assert_eq!(inspect_target(&path).unwrap(), DiagnosticExportTarget::New);
        let manifest = write_snapshot(&snapshot(), &path, DiagnosticExportMode::CreateNew).unwrap();
        assert_eq!(manifest.bytes, snapshot().bytes.len() as u64);
        assert_eq!(manifest.capture_id, snapshot().capture_id);
        assert_eq!(std::fs::read(&path).unwrap(), snapshot().bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_racing_file_survives_exclusive_creation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        assert_eq!(inspect_target(&path).unwrap(), DiagnosticExportTarget::New);
        std::fs::write(&path, b"racing writer").unwrap();
        let error =
            write_snapshot(&snapshot(), &path, DiagnosticExportMode::CreateNew).unwrap_err();
        assert_eq!(error.code(), "conflict");
        assert_eq!(std::fs::read(&path).unwrap(), b"racing writer");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_directory_targets_are_refused_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let link = dir.path().join("report.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(inspect_target(&link).is_err());
        assert!(write_snapshot(&snapshot(), &link, DiagnosticExportMode::CreateNew).is_err());
        assert!(write_snapshot(&snapshot(), &link, DiagnosticExportMode::ReplaceExisting).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());

        let directory = dir.path().join("reports");
        std::fs::create_dir(&directory).unwrap();
        assert!(inspect_target(&directory).is_err());
        assert!(write_snapshot(
            &snapshot(),
            &directory,
            DiagnosticExportMode::ReplaceExisting
        )
        .is_err());
        assert!(std::fs::metadata(&directory).unwrap().is_dir());
    }
}

//! Build-time provenance for the mobile binding.
//!
//! Records the workspace commit, whether the working tree was dirty, and the
//! Core package version. The value is compiled into the library and also
//! written as `WhaleReinMobile.revision` so the XCFramework carries it.
//! The build never fails when git or the repository metadata is unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let workspace_root = manifest_dir.join("../..");

    for path in [
        manifest_dir.join("src"),
        manifest_dir.join("Cargo.toml"),
        workspace_root.join("Cargo.toml"),
        workspace_root.join("core/Cargo.toml"),
        workspace_root.join(".git/HEAD"),
        workspace_root.join(".git/index"),
    ] {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let version = workspace_version(&workspace_root.join("Cargo.toml"))
        .or_else(|| core_package_version(&workspace_root.join("core/Cargo.toml")))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rerun-if-env-changed=WHALEREIN_MOBILE_SOURCE");
    let revision = revision(&workspace_root, &version);
    println!("cargo:rustc-env=WHALERIN_MOBILE_REVISION={revision}");
    println!("cargo:rustc-env=WHALERIN_CORE_VERSION={version}");

    let out_dir = std::env::var_os("OUT_DIR").map(PathBuf::from);
    if let Some(out_dir) = out_dir.as_deref() {
        // The resource is informational; a write failure must not fail the build.
        let _ = std::fs::write(
            out_dir.join("WhaleReinMobile.revision"),
            format!("{revision}\n"),
        );
    }
}

/// `<git shorthand>[-dirty] core:<version>`, with `unknown` fallbacks.
fn revision(workspace_root: &Path, version: &str) -> String {
    if let Ok(source) = std::env::var("WHALEREIN_MOBILE_SOURCE") {
        return format!("source:{source} core:{version}");
    }
    let head = git(workspace_root, &["rev-parse", "HEAD"])
        .map(|sha| sha.chars().take(12).collect::<String>())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let dirty = git(workspace_root, &["status", "--porcelain"])
        .is_some_and(|status| !status.trim().is_empty());
    let dirty = if dirty { "-dirty" } else { "" };
    format!("git:{head}{dirty} core:{version}")
}

fn git(workspace_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Read `version` from the `[workspace.package]` table.
fn workspace_version(manifest: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(manifest).ok()?;
    section_value(&manifest, "workspace.package", "version")
}

/// Fall back to a Core manifest that carries a literal version.
fn core_package_version(manifest: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(manifest).ok()?;
    section_value(&manifest, "package", "version")
}

/// Minimal TOML table lookup: only `key = "value"` lines inside `[section]`.
fn section_value(manifest: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches('"');
        if value.is_empty() {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

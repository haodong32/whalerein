//! Minimal, payload-free identities retained after a pairing is revoked, plus
//! the durable profile marker for a service-denial observation or revocation
//! intent awaiting confirmation.
use crate::api::{CoreError, CoreResult, PendingCommand};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    request_id: String,
    salt: String,
    binding: String,
    device_binding: String,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn valid_pending(value: &str) -> bool {
    value
        .strip_prefix("pending:revoke:")
        .or_else(|| value.strip_prefix("pending:observe:"))
        .is_some_and(valid_id)
}
fn confirmed_parts(value: &str) -> Option<(&str, &str)> {
    let mut parts = value.split(':');
    if parts.next()? != "confirmed" {
        return None;
    }
    let id = parts.next()?;
    let phase = parts.next()?;
    if parts.next().is_some()
        || !valid_id(id)
        || !matches!(phase, "check" | "waiting" | "saved" | "lost" | "conflict")
    {
        return None;
    }
    Some((id, phase))
}
pub(super) fn confirmed_phase(value: &str) -> Option<&str> {
    confirmed_parts(value).map(|(_, phase)| phase)
}
fn dir(path: &Path) -> PathBuf {
    path.join("unconfirmed-commands")
}
fn failure() -> CoreError {
    CoreError::Internal("unconfirmed command identity storage failed".into())
}

#[cfg(any(unix, windows))]
pub(crate) fn marker(path: &Path) -> CoreResult<Option<String>> {
    let directory = anchored::profile(path)?;
    marker_in(&directory)
}

#[cfg(any(unix, windows))]
pub(super) fn marker_with_retention_warning(path: &Path) -> CoreResult<(Option<String>, bool)> {
    let directory = anchored::profile(path)?;
    Ok((marker_in(&directory)?, retention_warning_in(&directory)))
}

#[cfg(any(unix, windows))]
pub(super) fn retention_warning_in(directory: &std::fs::File) -> bool {
    !matches!(
        anchored::open(directory, "revocation-retention-warning", false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(any(unix, windows))]
pub(crate) fn marker_in(directory: &std::fs::File) -> CoreResult<Option<String>> {
    let opened = match anchored::open(directory, "admission-revoked", false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure()),
    };
    let metadata = opened.metadata().map_err(|_| failure())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure());
        }
    }
    if !metadata.is_file() || metadata.len() > 64 {
        return Err(failure());
    }
    let mut data = String::new();
    opened
        .take(65)
        .read_to_string(&mut data)
        .map_err(|_| failure())?;
    if data.len() > 64 {
        return Err(failure());
    }
    if confirmed_parts(&data).is_some() || valid_pending(&data) {
        Ok(Some(data))
    } else {
        Err(failure())
    }
}
#[cfg(all(test, any(unix, windows)))]
pub(super) fn clear_owned_pending(path: &Path, expected: &str) -> CoreResult<bool> {
    let directory = anchored::profile(path)?;
    clear_owned_pending_in(&directory, expected)
}
#[cfg(any(unix, windows))]
pub(super) fn clear_owned_pending_in(
    directory: &std::fs::File,
    expected: &str,
) -> CoreResult<bool> {
    if !valid_pending(expected) {
        return Err(failure());
    }
    if marker_in(directory)?.as_deref() != Some(expected) {
        return Ok(false);
    }
    remove_marker_in(directory)?;
    Ok(true)
}
#[cfg(any(unix, windows))]
pub(super) fn remove_profile_entry(directory: &std::fs::File, name: &str) -> CoreResult<()> {
    anchored::remove_if_exists(directory, name)
}

#[cfg(any(unix, windows))]
pub(super) fn remove_marker_in(directory: &std::fs::File) -> CoreResult<()> {
    anchored::remove_checked(directory, "admission-revoked")?;
    #[cfg(unix)]
    directory.sync_all().map_err(|_| failure())?;
    Ok(())
}

/// Durable profile generation. It is never removed when the transient marker
/// is cleared, preventing an old pairing from observing a marker ABA.
#[cfg(any(unix, windows))]
pub(super) fn pairing_epoch_in(directory: &std::fs::File) -> CoreResult<Option<String>> {
    let mut file = match anchored::open(directory, "pairing-epoch", false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure()),
    };
    if !file.metadata().map_err(|_| failure())?.is_file() {
        return Err(failure());
    }
    let mut value = String::new();
    std::io::Read::by_ref(&mut file)
        .take(33)
        .read_to_string(&mut value)
        .map_err(|_| failure())?;
    if !valid_id(&value) {
        return Err(failure());
    }
    Ok(Some(value))
}

#[cfg(any(unix, windows))]
fn write_pairing_epoch_in(directory: &std::fs::File, id: &str) -> CoreResult<()> {
    if !valid_id(id) {
        return Err(failure());
    }
    if pairing_epoch_in(directory)?.as_deref() == Some(id) {
        return Ok(());
    }
    let temp = format!(".pairing-epoch-{}.tmp", crate::util::random_id(16));
    let mut file = anchored::open(directory, &temp, true).map_err(|_| failure())?;
    let result = (|| {
        file.write_all(id.as_bytes()).map_err(|_| failure())?;
        file.sync_all().map_err(|_| failure())?;
        drop(file);
        anchored::replace(directory, &temp, "pairing-epoch")?;
        #[cfg(unix)]
        directory.sync_all().map_err(|_| failure())?;
        Ok(())
    })();
    if result.is_err() {
        anchored::remove(directory, &temp)
    }
    result
}

/// Upgrade a confirmed pre-epoch marker before admitting or resuming pairing.
#[cfg(any(unix, windows))]
pub(super) fn ensure_pairing_epoch_in(directory: &std::fs::File) -> CoreResult<Option<String>> {
    if let Some(marker) = marker_in(directory)? {
        if let Some((id, _)) = confirmed_parts(&marker) {
            write_pairing_epoch_in(directory, id)?;
        }
    }
    pairing_epoch_in(directory)
}

#[cfg(all(test, any(unix, windows)))]
pub(super) fn write_marker(path: &Path, state: &str) -> CoreResult<()> {
    let directory = anchored::profile(path)?;
    write_marker_in(&directory, state)
}
#[cfg(any(unix, windows))]
pub(super) fn write_marker_in(directory: &std::fs::File, state: &str) -> CoreResult<()> {
    if !matches!(
        state,
        "confirmed"
            | "confirmed-waiting"
            | "confirmed-saved"
            | "confirmed-lost"
            | "confirmed-conflict"
    ) && !valid_pending(state)
    {
        return Err(failure());
    }
    let old = marker_in(directory)?;
    if old
        .as_ref()
        .is_some_and(|value| confirmed_parts(value).is_some())
        && valid_pending(state)
    {
        return Err(CoreError::Revoked);
    }
    let encoded = if valid_pending(state) {
        state.to_owned()
    } else {
        let id = old
            .as_deref()
            .and_then(|value| {
                confirmed_parts(value)
                    .map(|(id, _)| id)
                    .or_else(|| value.rsplit(':').next().filter(|id| valid_id(id)))
            })
            .map(str::to_owned)
            .unwrap_or_else(|| crate::util::random_id(16));
        // Publish the durable generation before a confirmed marker can later
        // be cleared by a new pairing. Both files are written under admission.
        write_pairing_epoch_in(directory, &id)?;
        let phase = if old.as_deref().and_then(confirmed_phase) == Some("conflict")
            || state == "confirmed-conflict"
        {
            "conflict"
        } else if old.as_deref().and_then(confirmed_phase) == Some("lost") {
            "lost"
        } else {
            match state {
                "confirmed" => "check",
                "confirmed-waiting" => "waiting",
                "confirmed-saved" => "saved",
                "confirmed-lost" => "lost",
                "confirmed-conflict" => "conflict",
                _ => return Err(failure()),
            }
        };
        format!("confirmed:{id}:{phase}")
    };
    let state = encoded.as_str();
    let temp = format!(".revocation-{}.tmp", crate::util::random_id(16));
    let mut file = anchored::open(directory, &temp, true).map_err(|_| failure())?;
    let result = (|| {
        file.write_all(state.as_bytes()).map_err(|_| failure())?;
        file.sync_all().map_err(|_| failure())?;
        drop(file);
        anchored::replace(directory, &temp, "admission-revoked")?;
        #[cfg(unix)]
        directory.sync_all().map_err(|_| failure())?;
        Ok(())
    })();
    if result.is_err() {
        anchored::remove(directory, &temp);
    }
    result
}
#[cfg(unix)]
mod anchored {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    extern "C" {
        fn openat(fd: i32, path: *const std::os::raw::c_char, flags: i32, ...) -> i32;
        fn linkat(
            oldfd: i32,
            old: *const std::os::raw::c_char,
            newfd: i32,
            new: *const std::os::raw::c_char,
            flags: i32,
        ) -> i32;
        fn unlinkat(fd: i32, path: *const std::os::raw::c_char, flags: i32) -> i32;
    }
    const NOFOLLOW: i32 = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    const CREATE: i32 = libc::O_CREAT | libc::O_EXCL;
    pub fn profile(path: &Path) -> CoreResult<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(NOFOLLOW);
        let file = options.open(path).map_err(|_| failure())?;
        if !file.metadata().map_err(|_| failure())?.is_dir() {
            return Err(failure());
        }
        Ok(file)
    }
    pub fn replace(directory: &std::fs::File, temp: &str, target: &str) -> CoreResult<()> {
        let temp = CString::new(temp).map_err(|_| failure())?;
        let target = CString::new(target).map_err(|_| failure())?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temp.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(failure());
        }
        Ok(())
    }
    pub fn remove_checked(directory: &std::fs::File, name: &str) -> CoreResult<()> {
        let name = CString::new(name).map_err(|_| failure())?;
        if unsafe { unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(failure());
        }
        Ok(())
    }
    pub fn remove_if_exists(directory: &std::fs::File, name: &str) -> CoreResult<()> {
        let name = CString::new(name).map_err(|_| failure())?;
        if unsafe { unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(failure())
        }
    }
    pub fn directory(path: &Path) -> CoreResult<std::fs::File> {
        directory_in(&profile(path)?, false)
    }
    pub fn directory_in(profile: &std::fs::File, create: bool) -> CoreResult<std::fs::File> {
        let name = CString::new("unconfirmed-commands").expect("literal");
        if create {
            let status = unsafe { libc::mkdirat(profile.as_raw_fd(), name.as_ptr(), 0o700) };
            if status != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(failure());
            }
        }
        let fd = unsafe {
            libc::openat(
                profile.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(failure());
        }
        let directory = unsafe { std::fs::File::from_raw_fd(fd) };
        if !directory.metadata().map_err(|_| failure())?.is_dir() {
            return Err(failure());
        }
        Ok(directory)
    }
    pub fn open(
        directory: &std::fs::File,
        name: &str,
        create: bool,
    ) -> std::io::Result<std::fs::File> {
        let name = CString::new(name)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let flags = NOFOLLOW | if create { 1 | CREATE } else { 0 };
        // Directory is pinned by its fd; no component supplied here contains a slash.
        let fd = unsafe { openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600u32) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    pub fn install(directory: &std::fs::File, temp: &str, target: &str) -> CoreResult<()> {
        let temp = CString::new(temp).map_err(|_| failure())?;
        let target = CString::new(target).map_err(|_| failure())?;
        if unsafe {
            linkat(
                directory.as_raw_fd(),
                temp.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(failure());
        }
        if unsafe { unlinkat(directory.as_raw_fd(), temp.as_ptr(), 0) } != 0 {
            return Err(failure());
        }
        Ok(())
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux",
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    pub fn names(directory: &std::fs::File) -> CoreResult<Vec<String>> {
        let fd = unsafe { libc::dup(directory.as_raw_fd()) };
        if fd < 0 {
            return Err(failure());
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(failure());
        }
        let result = (|| {
            let mut result = Vec::new();
            loop {
                #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                let errno = unsafe { libc::__error() };
                #[cfg(target_os = "linux")]
                let errno = unsafe { libc::__errno_location() };
                #[cfg(any(target_os = "android", target_os = "openbsd", target_os = "netbsd"))]
                let errno = unsafe { libc::__errno() };
                unsafe {
                    *errno = 0;
                }
                let entry = unsafe { libc::readdir(stream) };
                if entry.is_null() {
                    if unsafe { *errno } != 0 {
                        return Err(failure());
                    }
                    break;
                }
                let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
                result.push(name.to_str().map_err(|_| failure())?.to_owned());
            }
            Ok(result)
        })();
        if unsafe { libc::closedir(stream) } != 0 {
            return Err(failure());
        }
        result
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "linux",
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    pub fn names(_directory: &std::fs::File) -> CoreResult<Vec<String>> {
        Err(failure())
    }

    pub fn remove(directory: &std::fs::File, name: &str) {
        if let Ok(name) = CString::new(name) {
            unsafe {
                unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }
}

#[cfg(windows)]
mod anchored {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            handle: *mut std::ffi::c_void,
            path: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
    }
    fn path(directory: &std::fs::File) -> CoreResult<PathBuf> {
        let mut buf = vec![0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                directory.as_raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                0,
            )
        } as usize;
        if length == 0 || length >= buf.len() {
            return Err(failure());
        }
        buf.truncate(length);
        Ok(PathBuf::from(OsString::from_wide(&buf)))
    }
    pub fn profile(path: &Path) -> CoreResult<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000);
        let file = options.open(path).map_err(|_| failure())?;
        let metadata = file.metadata().map_err(|_| failure())?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(failure());
        }
        Ok(file)
    }
    pub fn replace(directory: &std::fs::File, temp: &str, target: &str) -> CoreResult<()> {
        let parent = path(directory)?;
        std::fs::rename(parent.join(temp), parent.join(target)).map_err(|_| failure())
    }
    pub fn remove_checked(directory: &std::fs::File, name: &str) -> CoreResult<()> {
        std::fs::remove_file(path(directory)?.join(name)).map_err(|_| failure())
    }
    pub fn remove_if_exists(directory: &std::fs::File, name: &str) -> CoreResult<()> {
        match std::fs::remove_file(path(directory)?.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure()),
        }
    }
    pub fn directory(path: &Path) -> CoreResult<std::fs::File> {
        directory_in(&profile(path)?, false)
    }
    pub fn directory_in(profile: &std::fs::File, create: bool) -> CoreResult<std::fs::File> {
        let target = path(profile)?.join("unconfirmed-commands");
        if create {
            match std::fs::create_dir(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(failure()),
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000);
        let file = options.open(target).map_err(|_| failure())?;
        let metadata = file.metadata().map_err(|_| failure())?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(failure());
        }
        Ok(file)
    }
    pub fn open(
        directory: &std::fs::File,
        name: &str,
        create: bool,
    ) -> std::io::Result<std::fs::File> {
        let parent =
            path(directory).map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))?;
        if !create {
            let metadata = std::fs::symlink_metadata(parent.join(name))?;
            if metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink() {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.share_mode(3).custom_flags(0x00200000);
        if create {
            options.write(true).create_new(true);
        } else {
            options.read(true);
        }
        let file = options.open(parent.join(name))?;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(file)
    }
    pub fn install(directory: &std::fs::File, temp: &str, target: &str) -> CoreResult<()> {
        let parent = path(directory)?;
        std::fs::hard_link(parent.join(temp), parent.join(target)).map_err(|_| failure())?;
        std::fs::remove_file(parent.join(temp)).map_err(|_| failure())
    }
    pub fn names(directory: &std::fs::File) -> CoreResult<Vec<String>> {
        std::fs::read_dir(path(directory)?)
            .map_err(|_| failure())?
            .map(|entry| {
                let entry = entry.map_err(|_| failure())?;
                entry.file_name().into_string().map_err(|_| failure())
            })
            .collect()
    }

    pub fn remove(directory: &std::fs::File, name: &str) {
        if let Ok(parent) = path(directory) {
            let _ = std::fs::remove_file(parent.join(name));
        }
    }
}

#[cfg(any(unix, windows))]
fn read_identity(directory: &std::fs::File, id: &str) -> CoreResult<Identity> {
    let file = anchored::open(directory, &format!("{id}.json"), false).map_err(|_| failure())?;
    let metadata = file.metadata().map_err(|_| failure())?;
    if !metadata.is_file() || metadata.len() > 2048 {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(2049)
        .read_to_end(&mut bytes)
        .map_err(|_| failure())?;
    if bytes.len() > 2048 {
        return Err(failure());
    }
    let item: Identity = serde_json::from_slice(&bytes).map_err(|_| failure())?;
    if item.request_id != id
        || !valid_id(&item.salt)
        || item.binding.len() != 64
        || item.device_binding.len() != 64
        || !item
            .device_binding
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !item
            .binding
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(failure());
    }
    Ok(item)
}

fn device_binding(salt: &str, device: &str) -> String {
    crate::util::sha256_hex(format!("{}:{}:{}", salt, device.len(), device).as_bytes())
}

fn binding(salt: &str, p: &PendingCommand) -> String {
    let fields = [
        &p.request_id,
        &p.command_id,
        &p.host_id,
        &p.device_id,
        &p.access_url,
        &p.host_fingerprint,
    ];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(salt.as_bytes());
    for value in fields {
        bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }
    crate::util::sha256_hex(&bytes)
}

#[cfg(all(test, any(unix, windows)))]
pub(super) fn preserve(path: &Path, pending: &[PendingCommand]) -> CoreResult<()> {
    let profile = anchored::profile(path)?;
    preserve_in(&profile, pending)
}
#[cfg(any(unix, windows))]
pub(super) fn preserve_in(profile: &std::fs::File, pending: &[PendingCommand]) -> CoreResult<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let directory = anchored::directory_in(profile, true)?;
    #[cfg(unix)]
    profile.sync_all().map_err(|_| failure())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        directory
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|_| failure())?;
    }

    for item in pending {
        if !valid_id(&item.request_id) {
            return Err(failure());
        }
        let target = format!("{}.json", item.request_id);
        match anchored::open(&directory, &target, false) {
            Ok(_) => {
                let existing = read_identity(&directory, &item.request_id)?;
                if existing.binding != binding(&existing.salt, item)
                    || existing.device_binding != device_binding(&existing.salt, &item.device_id)
                {
                    return Err(failure());
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(failure()),
        }
        let salt = crate::util::random_id(16);
        let record = Identity {
            request_id: item.request_id.clone(),
            binding: binding(&salt, item),
            device_binding: device_binding(&salt, &item.device_id),
            salt,
        };
        let temporary = format!(".{}.tmp", crate::util::random_id(16));
        let mut file = anchored::open(&directory, &temporary, true).map_err(|_| failure())?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(&record).map_err(|_| failure())?)
                .map_err(|_| failure())?;
            file.sync_all().map_err(|_| failure())?;
            drop(file);
            anchored::install(&directory, &temporary, &target)
        })();
        if result.is_err() {
            anchored::remove(&directory, &temporary);
        }
        result?;
    }
    #[cfg(unix)]
    directory.sync_all().map_err(|_| failure())?;
    Ok(())
}

#[cfg(any(unix, windows))]
pub(super) fn list(path: &Path, device_id: &str) -> CoreResult<Vec<String>> {
    match std::fs::symlink_metadata(dir(path)) {
        Ok(metadata) if !metadata.file_type().is_dir() => return Err(failure()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(failure()),
        Ok(_) => {}
    }
    #[cfg(any(unix, windows))]
    let directory = anchored::directory(path)?;

    let entries = anchored::names(&directory)?;
    let mut result = Vec::new();
    for name in entries {
        if let Some(id) = name.strip_suffix(".json") {
            if !valid_id(id) {
                return Err(failure());
            }
            let identity = read_identity(&directory, id)?;
            if identity.device_binding == device_binding(&identity.salt, device_id) {
                result.push(id.to_owned());
            }
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn marker(path: &Path) -> CoreResult<Option<String>> {
    match std::fs::symlink_metadata(path.join("admission-revoked")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(failure()),
    }
}
#[cfg(not(any(unix, windows)))]
pub(super) fn write_marker(_path: &Path, _state: &str) -> CoreResult<()> {
    Err(failure())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn preserve(_path: &Path, pending: &[PendingCommand]) -> CoreResult<()> {
    if pending.is_empty() {
        Ok(())
    } else {
        Err(failure())
    }
}
#[cfg(not(any(unix, windows)))]
pub(super) fn list(path: &Path, _device_id: &str) -> CoreResult<Vec<String>> {
    match std::fs::symlink_metadata(dir(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        _ => Err(failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::OnlineStatus;

    fn pending() -> PendingCommand {
        PendingCommand {
            request_id: "a".repeat(32),
            command_id: "private command".into(),
            host_id: "host".into(),
            device_id: "device".into(),
            access_url: "https://fixture/path".into(),
            host_fingerprint: "fingerprint".into(),
            status: OnlineStatus::Unknown,
        }
    }

    #[test]
    fn pending_owner_cannot_clear_newer_or_confirmed_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let old = format!("pending:observe:{}", "a".repeat(32));
        let new = format!("pending:revoke:{}", "b".repeat(32));
        write_marker(dir.path(), &old).unwrap();
        write_marker(dir.path(), &new).unwrap();
        assert!(!clear_owned_pending(dir.path(), &old).unwrap());
        write_marker(dir.path(), "confirmed").unwrap();
        assert!(!clear_owned_pending(dir.path(), &new).unwrap());
        assert!(write_marker(dir.path(), &old).is_err());
        assert_eq!(
            marker(dir.path())
                .unwrap()
                .as_deref()
                .and_then(confirmed_phase),
            Some("check")
        );
    }

    #[test]
    fn pairing_epoch_survives_marker_removal_and_advances_per_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
        let first = format!("pending:revoke:{}", "a".repeat(32));
        write_marker_in(guard.directory(), &first).unwrap();
        write_marker_in(guard.directory(), "confirmed-saved").unwrap();
        assert_eq!(
            pairing_epoch_in(guard.directory()).unwrap().as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        remove_marker_in(guard.directory()).unwrap();
        assert_eq!(
            pairing_epoch_in(guard.directory()).unwrap().as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let second = format!("pending:revoke:{}", "b".repeat(32));
        write_marker_in(guard.directory(), &second).unwrap();
        assert_eq!(
            pairing_epoch_in(guard.directory()).unwrap().as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        write_marker_in(guard.directory(), "confirmed-saved").unwrap();
        assert_eq!(
            pairing_epoch_in(guard.directory()).unwrap().as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[cfg(unix)]
    #[test]
    fn pairing_epoch_rejects_symlink_without_modifying_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let target = external.path().join("outside");
        std::fs::write(&target, b"unchanged").unwrap();
        symlink(&target, dir.path().join("pairing-epoch")).unwrap();
        let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
        assert!(pairing_epoch_in(guard.directory()).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }

    #[test]
    fn duplicates_and_corrupt_records_are_not_silently_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let item = pending();
        preserve(dir.path(), std::slice::from_ref(&item)).unwrap();
        preserve(dir.path(), std::slice::from_ref(&item)).unwrap();
        assert_eq!(
            list(dir.path(), "device").unwrap(),
            vec![item.request_id.clone()]
        );
        assert!(list(dir.path(), "another-device").unwrap().is_empty());
        let mut different = item.clone();
        different.command_id = "different".into();
        assert!(preserve(dir.path(), &[different]).is_err());
        std::fs::write(
            dir.path()
                .join("unconfirmed-commands")
                .join(format!("{}.json", item.request_id)),
            b"{}",
        )
        .unwrap();
        assert!(list(dir.path(), "device").is_err());
        assert!(preserve(dir.path(), &[item]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn retention_warning_stays_bound_to_pinned_profile_after_path_replacement() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("profile");
        let moved = parent.path().join("moved");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&original).unwrap();
        let guard = crate::security::device::ProfileLock::acquire_admission(&original).unwrap();
        assert!(!retention_warning_in(guard.directory()));
        std::fs::write(original.join("revocation-retention-warning"), b"").unwrap();
        std::fs::rename(&original, &moved).unwrap();
        symlink(outside.path(), &original).unwrap();
        assert!(retention_warning_in(guard.directory()));
        assert!(!outside.path().join("revocation-retention-warning").exists());
    }

    #[cfg(unix)]
    #[test]
    fn admission_lock_and_marker_share_profile_after_path_replacement() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("profile");
        let moved = parent.path().join("moved");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&original).unwrap();
        let guard = crate::security::device::ProfileLock::acquire_admission(&original).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        symlink(outside.path(), &original).unwrap();
        let pending = format!("pending:observe:{}", "a".repeat(32));
        write_marker_in(guard.directory(), &pending).unwrap();
        assert_eq!(
            marker_in(guard.directory()).unwrap().as_deref(),
            Some(pending.as_str())
        );
        assert!(moved.join("admission-revoked").exists());
        assert!(!outside.path().join("admission-revoked").exists());
        preserve_in(guard.directory(), &[super::tests::pending()]).unwrap();
        assert!(moved.join("unconfirmed-commands").is_dir());
        assert!(!outside.path().join("unconfirmed-commands").exists());
        assert!(clear_owned_pending_in(guard.directory(), &pending).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn marker_operations_remain_anchored_after_profile_path_replacement() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("profile");
        let moved = parent.path().join("moved");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&original).unwrap();
        let handle = anchored::profile(&original).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        symlink(outside.path(), &original).unwrap();
        let mut temp = anchored::open(&handle, "temp", true).unwrap();
        temp.write_all(format!("pending:observe:{}", "a".repeat(32)).as_bytes())
            .unwrap();
        temp.sync_all().unwrap();
        drop(temp);
        anchored::replace(&handle, "temp", "admission-revoked").unwrap();
        assert!(marker_in(&handle).unwrap().is_some());
        assert!(moved.join("admission-revoked").exists());
        assert!(!outside.path().join("admission-revoked").exists());
        anchored::remove_checked(&handle, "admission-revoked").unwrap();
        assert!(!moved.join("admission-revoked").exists());
    }

    #[cfg(unix)]
    #[test]
    fn opened_directory_remains_anchored_after_path_replacement() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir(root.path())).unwrap();
        let directory = anchored::directory(root.path()).unwrap();
        std::fs::rename(dir(root.path()), root.path().join("moved")).unwrap();
        symlink(outside.path(), dir(root.path())).unwrap();
        let mut file = anchored::open(&directory, "temporary", true).unwrap();
        file.write_all(b"fixture").unwrap();
        anchored::install(&directory, "temporary", "identity").unwrap();
        assert!(root.path().join("moved/identity").exists());
        assert!(!outside.path().join("identity").exists());
        assert!(anchored::names(&directory)
            .unwrap()
            .contains(&"identity".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_marker_and_record_are_rejected_without_touching_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("untouched");
        std::fs::write(&target, b"sentinel").unwrap();
        symlink(&target, dir.path().join("admission-revoked")).unwrap();
        assert!(write_marker(dir.path(), "confirmed").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
        std::fs::remove_file(dir.path().join("admission-revoked")).unwrap();
        symlink(outside.path(), dir.path().join("unconfirmed-commands")).unwrap();
        assert!(preserve(dir.path(), &[pending()]).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_handle_blocks_renaming_until_released() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir(root.path())).unwrap();
        let handle = anchored::directory(root.path()).unwrap();
        let moved = root.path().join("moved");
        assert!(std::fs::rename(dir(root.path()), &moved).is_err());
        drop(handle);
        std::fs::rename(dir(root.path()), &moved).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_points_are_rejected_without_touching_target() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("untouched");
        std::fs::write(&target, b"sentinel").unwrap();
        symlink_file(&target, root.path().join("admission-revoked"))
            .expect("Windows symlink privilege or Developer Mode is required for this test");
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        let path = root.path().join("admission-revoked");
        let link_flags = std::fs::symlink_metadata(&path)
            .map(|value| value.file_attributes())
            .map_err(|error| error.raw_os_error());
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(0x00200000);
        let opened_flags = options
            .open(&path)
            .and_then(|file| file.metadata())
            .map(|value| value.file_attributes())
            .map_err(|error| error.raw_os_error());
        let names = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();
        let rejected = write_marker(root.path(), "confirmed").is_err();
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
        assert!(
            rejected,
            "marker accepted: path_flags={link_flags:?}, opened_flags={opened_flags:?}, entries={names:?}"
        );
        std::fs::remove_file(root.path().join("admission-revoked")).unwrap();
        symlink_dir(outside.path(), dir(root.path()))
            .expect("Windows symlink privilege or Developer Mode is required for this test");
        assert!(preserve(root.path(), &[pending()]).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
    }
}

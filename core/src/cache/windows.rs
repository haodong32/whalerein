//! Native file identity checks for Windows cache recovery.
use super::{CacheError, CacheProvenance};
use rusqlite::Connection;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    volume: u64,
    index: [u8; 16],
}

#[repr(C)]
struct FileInformation {
    attributes: u32,
    _creation: [u32; 2],
    _access: [u32; 2],
    _write: [u32; 2],
    _volume: u32,
    _size_high: u32,
    _size_low: u32,
    links: u32,
    _index_high: u32,
    _index_low: u32,
}

#[repr(C)]
struct FileIdInformation {
    volume: u64,
    index: [u8; 16],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut FileInformation)
        -> i32;
    fn GetFileInformationByHandleEx(
        handle: *mut std::ffi::c_void,
        class: i32,
        info: *mut std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn GetFinalPathNameByHandleW(
        handle: *mut std::ffi::c_void,
        path: *mut u16,
        size: u32,
        flags: u32,
    ) -> u32;
}

fn information(handle: *mut std::ffi::c_void) -> Result<FileInformation, CacheError> {
    let mut info = std::mem::MaybeUninit::uninit();
    if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
        return Err(CacheError::Database);
    }
    let info: FileInformation = unsafe { info.assume_init() };
    if info.attributes & 0x400 != 0 {
        return Err(CacheError::Corrupt);
    }
    Ok(info)
}

fn identity(handle: *mut std::ffi::c_void) -> Result<Identity, CacheError> {
    // FileIdInfo includes the volume and full 128-bit identifier (also on ReFS).
    let mut info = std::mem::MaybeUninit::<FileIdInformation>::uninit();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            18,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FileIdInformation>() as u32,
        )
    } == 0
    {
        return Err(CacheError::Database);
    }
    let info = unsafe { info.assume_init() };
    Ok(Identity {
        volume: info.volume,
        index: info.index,
    })
}

pub(super) fn directory_identity(directory: &File) -> Result<Identity, CacheError> {
    let info = information(directory.as_raw_handle())?;
    if info.attributes & 0x10 == 0 {
        return Err(CacheError::Corrupt);
    }
    identity(directory.as_raw_handle())
}

pub(super) fn directory_path(directory: &File) -> Result<PathBuf, CacheError> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            directory.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(CacheError::Database);
    }
    buffer.truncate(length);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

pub(super) fn prove(conn: &Connection, profile: &Path) -> Result<CacheProvenance, CacheError> {
    // Neither pin permits deletion/renaming; SQLite's native handle must name
    // the same regular, singly linked file before any business rows are read.
    let directory = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x02000000 | 0x00200000)
        .open(profile)
        .map_err(|_| CacheError::Database)?;
    let profile = directory_identity(&directory)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x00200000)
        .open(directory_path(&directory)?.join("cache.sqlite3"))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CacheError::Missing
            } else {
                CacheError::Database
            }
        })?;
    let info = information(file.as_raw_handle())?;
    if info.attributes & 0x10 != 0 || info.links != 1 {
        return Err(CacheError::Corrupt);
    }
    let mut handle: *mut std::ffi::c_void = std::ptr::null_mut();
    let status = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            std::ptr::null(),
            rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
            (&mut handle as *mut *mut std::ffi::c_void).cast(),
        )
    };
    if status != rusqlite::ffi::SQLITE_OK {
        return Err(CacheError::Database);
    }
    let database = identity(file.as_raw_handle())?;
    if identity(handle)? != database {
        return Err(CacheError::Corrupt);
    }
    Ok(CacheProvenance { profile, database })
}

pub(super) fn no_sidecars(directory: &File) -> Result<(), CacheError> {
    let path = directory_path(directory)?;
    for name in [
        "cache.sqlite3-journal",
        "cache.sqlite3-wal",
        "cache.sqlite3-shm",
    ] {
        match std::fs::symlink_metadata(path.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) if name.ends_with("-journal") => return Err(CacheError::Busy),
            _ => return Err(CacheError::Corrupt),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::OnlineStatus;
    use crate::cache::Cache;
    use crate::security::device::ProfileLock;

    #[test]
    fn dormant_snapshot_preserves_rows_without_creating_a_missing_database() {
        let profile = tempfile::tempdir().unwrap();
        let guard = ProfileLock::acquire_admission(profile.path()).unwrap();
        let path = profile.path().join("cache.sqlite3");
        assert!(Cache::open_existing_verified(&path, [1; 32], guard.directory()).is_err());
        assert!(!path.exists());
        let cache = Cache::open(&path, [1; 32]).unwrap();
        cache
            .put_command("request", "command", OnlineStatus::Unknown, None)
            .unwrap();
        drop(cache);
        let cache = Cache::open_existing_verified(&path, [1; 32], guard.directory()).unwrap();
        assert!(cache.is_revocation_only());
        assert_eq!(
            cache
                .pending_commands_verified(guard.directory(), profile.path())
                .unwrap()
                .len(),
            1
        );
        cache.clear().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn releasing_an_obsolete_cache_closes_its_handle_but_keeps_the_durable_journal() {
        let profile = tempfile::tempdir().unwrap();
        let guard = ProfileLock::acquire_admission(profile.path()).unwrap();
        let path = profile.path().join("cache.sqlite3");
        let cache = Cache::open(&path, [1; 32]).unwrap();
        cache
            .put_command("request", "command", OnlineStatus::Unknown, None)
            .unwrap();
        cache.release_for_revocation().unwrap();
        assert!(cache.pending_commands().unwrap().is_empty());
        let recovered = Cache::open_existing_verified(&path, [1; 32], guard.directory()).unwrap();
        assert_eq!(
            recovered
                .pending_commands_verified(guard.directory(), profile.path())
                .unwrap()
                .len(),
            1
        );
        recovered.clear().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn snapshots_reject_other_profiles_hardlinks_and_sidecar_files() {
        let profile = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let guard = ProfileLock::acquire_admission(profile.path()).unwrap();
        let other = ProfileLock::acquire_admission(outside.path()).unwrap();
        let path = profile.path().join("cache.sqlite3");
        let cache = Cache::open(&path, [1; 32]).unwrap();
        assert!(cache.matches_profile(guard.directory(), profile.path()));
        assert!(!cache.matches_profile(other.directory(), profile.path()));
        assert!(cache
            .pending_commands_verified(other.directory(), profile.path())
            .is_err());
        assert!(Cache::open_existing_verified(&path, [1; 32], other.directory()).is_err());
        let alias = outside.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(cache
            .pending_commands_verified(guard.directory(), profile.path())
            .is_err());
        std::fs::remove_file(alias).unwrap();
        for name in ["cache.sqlite3-journal", "cache.sqlite3-wal"] {
            let sidecar = profile.path().join(name);
            std::fs::write(&sidecar, b"synthetic").unwrap();
            assert!(cache
                .pending_commands_verified(guard.directory(), profile.path())
                .is_err());
            std::fs::remove_file(sidecar).unwrap();
        }
        let memory = Cache::open_in_memory([1; 32]).unwrap();
        assert!(!memory.matches_profile(guard.directory(), profile.path()));
        assert!(memory
            .pending_commands_verified(guard.directory(), profile.path())
            .is_err());
    }

    #[test]
    fn profile_reparse_points_are_not_trusted_as_cache_provenance() {
        use std::os::windows::fs::symlink_dir;
        let profile = tempfile::tempdir().unwrap();
        let alias_root = tempfile::tempdir().unwrap();
        let cache = Cache::open(&profile.path().join("cache.sqlite3"), [1; 32]).unwrap();
        let alias = alias_root.path().join("profile-link");
        symlink_dir(profile.path(), &alias).unwrap();
        let connection = cache.conn.lock().unwrap();
        assert!(prove(&connection, &alias).is_err());
    }
}

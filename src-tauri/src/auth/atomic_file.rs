//! Staged atomic file replacement with exact snapshot checks.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tempfile::{NamedTempFile, TempPath};

const RECOVERY_SUFFIX: &str = ".codex-switcher.recovery";

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum FileSnapshot {
    Missing,
    Present(Vec<u8>),
}

impl FileSnapshot {
    pub(crate) fn present(bytes: Vec<u8>) -> Self {
        Self::Present(bytes)
    }

    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present(bytes) => Some(bytes),
        }
    }
}

pub(crate) struct StagedFileChange {
    path: PathBuf,
    expected: FileSnapshot,
    desired: FileSnapshot,
    staged_path: Option<TempPath>,
    label: &'static str,
}

pub(crate) fn read_snapshot(path: &Path) -> Result<FileSnapshot> {
    match fs::read(path) {
        Ok(bytes) => Ok(FileSnapshot::Present(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileSnapshot::Missing),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Clean up a recovery sidecar left by an interrupted externally mutable file transaction.
///
/// In-process publication failures restore through the still-open recovery handle. After a
/// process interruption, the account catalog remains the durable credential source. Restoring a
/// fixed sidecar into a missing live path would be unsafe because the live file may have been
/// deliberately removed by a later sign-out.
pub(crate) fn recover_externally_mutable_file(path: &Path) -> Result<()> {
    let recovery = recovery_path(path);
    ensure_not_reparse_point(path)?;
    ensure_not_reparse_point(&recovery)?;
    if !recovery.exists() {
        return Ok(());
    }

    remove_file_if_present(&recovery).with_context(|| {
        format!(
            "Failed to remove stale recovery sidecar for {}",
            path.display()
        )
    })
}

pub(crate) fn stage_file_change(
    path: &Path,
    expected: FileSnapshot,
    desired: FileSnapshot,
    label: &'static str,
) -> Result<StagedFileChange> {
    test_fail_point(match label {
        "auth" => "stage_auth",
        "accounts" => "stage_accounts",
        "rollback" => "stage_rollback",
        _ => "stage_file",
    })?;

    let staged_path = match &desired {
        FileSnapshot::Missing => None,
        FileSnapshot::Present(bytes) => Some(stage_bytes(path, bytes)?),
    };

    Ok(StagedFileChange {
        path: path.to_path_buf(),
        expected,
        desired,
        staged_path,
        label,
    })
}

impl StagedFileChange {
    pub(crate) fn commit(mut self) -> Result<FileSnapshot> {
        if matches!(self.label, "auth" | "rollback") {
            self.commit_externally_mutable()?;
        } else {
            self.commit_locked_file()?;
        }

        Ok(self.desired.clone())
    }

    fn commit_locked_file(&mut self) -> Result<()> {
        if read_snapshot(&self.path)? != self.expected {
            anyhow::bail!(
                "{} changed while the operation was in progress",
                display_name(self.label)
            );
        }

        self.check_publication_fail_point()?;
        match &self.desired {
            FileSnapshot::Missing => remove_file_if_present(&self.path),
            FileSnapshot::Present(_) => {
                let staged_path = self
                    .staged_path
                    .take()
                    .context("Staged file was unavailable for publication")?;
                replace_file(staged_path.as_ref(), &self.path)
            }
        }
    }

    fn commit_externally_mutable(&mut self) -> Result<()> {
        recover_externally_mutable_file(&self.path)?;
        let recovery = recovery_path(&self.path);
        if recovery.exists() {
            anyhow::bail!(
                "A previous {} transaction is still awaiting recovery at {}",
                display_name(self.label),
                recovery.display()
            );
        }

        self.check_publication_fail_point()?;
        match &self.expected {
            FileSnapshot::Missing => self.commit_when_expected_missing(),
            FileSnapshot::Present(expected_bytes) => {
                self.commit_when_expected_present(expected_bytes.clone())
            }
        }
    }

    fn commit_when_expected_missing(&mut self) -> Result<()> {
        test_external_replacement(&self.path)?;
        match &self.desired {
            FileSnapshot::Missing => {
                if read_snapshot(&self.path)? != FileSnapshot::Missing {
                    anyhow::bail!(
                        "{} changed while the operation was in progress",
                        display_name(self.label)
                    );
                }
                Ok(())
            }
            FileSnapshot::Present(_) => {
                let staged_path = self
                    .staged_path
                    .take()
                    .context("Staged file was unavailable for publication")?;
                move_file_no_replace(staged_path.as_ref(), &self.path).with_context(|| {
                    format!(
                        "{} changed while the operation was in progress",
                        display_name(self.label)
                    )
                })
            }
        }
    }

    #[cfg(windows)]
    fn commit_when_expected_present(&mut self, expected_bytes: Vec<u8>) -> Result<()> {
        test_external_replacement(&self.path)?;
        let recovery = recovery_path(&self.path);
        let mut guarded = open_guarded_file(&self.path).with_context(|| {
            format!(
                "{} is busy or changed while the operation was in progress",
                display_name(self.label)
            )
        })?;
        let mut actual = Vec::new();
        guarded
            .read_to_end(&mut actual)
            .with_context(|| format!("Failed to verify guarded {}", display_name(self.label)))?;
        if actual != expected_bytes {
            anyhow::bail!(
                "{} changed while the operation was in progress",
                display_name(self.label)
            );
        }

        if matches!(self.desired, FileSnapshot::Present(_)) {
            let staged_path = self
                .staged_path
                .take()
                .context("Staged file was unavailable for publication")?;
            replace_file_with_backup(staged_path.as_ref(), &self.path, &recovery).with_context(
                || format!("Failed to atomically publish {}", display_name(self.label)),
            )?;

            // ReplaceFileW never exposes a missing destination. If sidecar cleanup fails, a later
            // missing destination means an external sign-out happened after publication, so startup
            // recovery can safely discard the stale backup instead of resurrecting credentials.
            let _ = dispose_recovery_handle(&guarded);
            return Ok(());
        }

        rename_open_file(&guarded, &recovery).with_context(|| {
            format!(
                "Failed to prepare recovery for {}",
                display_name(self.label)
            )
        })?;
        let publication = self.publish_after_recovery_rename();
        match publication {
            Ok(()) => {
                if let Err(cleanup_error) = dispose_recovery_handle(&guarded) {
                    return self.restore_after_failed_publication(&guarded, cleanup_error);
                }
                Ok(())
            }
            Err(publication_error) => {
                self.restore_after_failed_publication(&guarded, publication_error)
            }
        }
    }

    #[cfg(not(windows))]
    fn commit_when_expected_present(&mut self, expected_bytes: Vec<u8>) -> Result<()> {
        let recovery = recovery_path(&self.path);
        move_file_to_recovery(&self.path, &recovery).with_context(|| {
            format!(
                "{} changed while the operation was in progress",
                display_name(self.label)
            )
        })?;

        let recovered_bytes = fs::read(&recovery).with_context(|| {
            format!(
                "Failed to verify recovery copy of {}",
                display_name(self.label)
            )
        })?;
        if recovered_bytes != expected_bytes {
            restore_recovery_path(&recovery, &self.path, self.label)?;
            anyhow::bail!(
                "{} changed while the operation was in progress",
                display_name(self.label)
            );
        }

        test_external_replacement(&self.path)?;
        let publication = self.publish_after_recovery_rename();
        match publication {
            Ok(()) => {
                if matches!(self.desired, FileSnapshot::Missing) {
                    if let Err(cleanup_error) = remove_recovery_path(&recovery) {
                        return self
                            .restore_path_after_failed_publication(&recovery, cleanup_error);
                    }
                } else {
                    let _ = remove_recovery_path(&recovery);
                }
                Ok(())
            }
            Err(publication_error) => {
                self.restore_path_after_failed_publication(&recovery, publication_error)
            }
        }
    }

    fn publish_after_recovery_rename(&mut self) -> Result<()> {
        match &self.desired {
            FileSnapshot::Missing => {
                if read_snapshot(&self.path)? == FileSnapshot::Missing {
                    Ok(())
                } else {
                    anyhow::bail!(
                        "{} changed while the operation was in progress",
                        display_name(self.label)
                    )
                }
            }
            FileSnapshot::Present(_) => {
                let staged_path = self
                    .staged_path
                    .take()
                    .context("Staged file was unavailable for publication")?;
                move_file_no_replace(staged_path.as_ref(), &self.path).with_context(|| {
                    format!(
                        "{} changed while the operation was in progress",
                        display_name(self.label)
                    )
                })
            }
        }
    }

    #[cfg(windows)]
    fn restore_after_failed_publication(
        &self,
        guarded_recovery: &fs::File,
        publication_error: anyhow::Error,
    ) -> Result<()> {
        if self.path.exists() {
            let _ = dispose_recovery_handle(guarded_recovery);
            return Err(publication_error);
        }

        match rename_open_file(guarded_recovery, &self.path) {
            Ok(()) => Err(publication_error).context(format!(
                "{} publication was not applied; the previous file was restored",
                display_name(self.label)
            )),
            Err(restore_error) if self.path.exists() => {
                let _ = dispose_recovery_handle(guarded_recovery);
                Err(publication_error).context(format!(
                    "{} changed while recovery was in progress: {restore_error}",
                    display_name(self.label)
                ))
            }
            Err(restore_error) => Err(publication_error).context(format!(
                "{} publication failed and the previous file remains at {}: {restore_error}",
                display_name(self.label),
                recovery_path(&self.path).display()
            )),
        }
    }

    #[cfg(not(windows))]
    fn restore_path_after_failed_publication(
        &self,
        recovery: &Path,
        publication_error: anyhow::Error,
    ) -> Result<()> {
        if self.path.exists() {
            let _ = remove_recovery_path(recovery);
            return Err(publication_error);
        }

        match move_file_no_replace(recovery, &self.path) {
            Ok(()) => Err(publication_error).context(format!(
                "{} publication was not applied; the previous file was restored",
                display_name(self.label)
            )),
            Err(restore_error) if self.path.exists() => {
                let _ = remove_recovery_path(recovery);
                Err(publication_error).context(format!(
                    "{} changed while recovery was in progress: {restore_error}",
                    display_name(self.label)
                ))
            }
            Err(restore_error) => Err(publication_error).context(format!(
                "{} publication failed and the previous file remains at {}: {restore_error}",
                display_name(self.label),
                recovery.display()
            )),
        }
    }

    fn check_publication_fail_point(&self) -> Result<()> {
        test_fail_point(match self.label {
            "auth" => "publish_auth",
            "accounts" => "publish_accounts",
            "rollback" => "publish_rollback",
            _ => "publish_file",
        })
    }
}

pub(crate) fn restore_if_matches(
    path: &Path,
    expected: FileSnapshot,
    desired: FileSnapshot,
) -> Result<()> {
    stage_file_change(path, expected, desired, "rollback")?
        .commit()
        .map(|_| ())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], label: &'static str) -> Result<()> {
    let expected = read_snapshot(path)?;
    let desired = FileSnapshot::Present(bytes.to_vec());
    stage_file_change(path, expected, desired, label)?
        .commit()
        .map(|_| ())
}

fn stage_bytes(path: &Path, bytes: &[u8]) -> Result<TempPath> {
    let parent = path
        .parent()
        .context("Atomic write path did not have a parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory: {}", parent.display()))?;

    let mut temp_file = NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temp file in {}", parent.display()))?;
    temp_file
        .write_all(bytes)
        .with_context(|| format!("Failed to write staged file for {}", path.display()))?;
    temp_file
        .flush()
        .with_context(|| format!("Failed to flush staged file for {}", path.display()))?;
    temp_file
        .as_file()
        .sync_all()
        .with_context(|| format!("Failed to sync staged file for {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Failed to set staged permissions for {}", path.display()))?;
    }

    Ok(temp_file.into_temp_path())
}

fn recovery_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(RECOVERY_SUFFIX);
    PathBuf::from(name)
}

#[cfg(not(windows))]
fn restore_recovery_path(recovery: &Path, destination: &Path, label: &str) -> Result<()> {
    match move_file_no_replace(recovery, destination) {
        Ok(()) => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "{} changed and the previous file remains at {}",
                display_name(label),
                recovery.display()
            )
        }),
    }
}

#[cfg(not(windows))]
fn remove_recovery_path(path: &Path) -> Result<()> {
    test_fail_point("cleanup_recovery")?;
    remove_file_if_present(path)
}

fn ensure_not_reparse_point(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}", path.display()));
        }
    };

    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "Refusing to replace symbolic link or reparse point at {}",
            path.display()
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            anyhow::bail!(
                "Refusing to replace symbolic link or reparse point at {}",
                path.display()
            );
        }
    }

    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination).with_context(|| {
        format!(
            "Failed to atomically replace {} with {}",
            destination.display(),
            source.display()
        )
    })
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    move_file_windows(
        source,
        destination,
        windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING,
    )
    .with_context(|| {
        format!(
            "Failed to atomically replace {} with {}",
            destination.display(),
            source.display()
        )
    })
}

#[cfg(windows)]
fn replace_file_with_backup(
    source: &Path,
    destination: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let backup_wide: Vec<u16> = backup
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            source_wide.as_ptr(),
            backup_wide.as_ptr(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn move_file_to_recovery(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(windows))]
fn move_file_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination)?;
    fs::remove_file(source)
}

#[cfg(windows)]
fn move_file_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    move_file_windows(source, destination, 0)
}

#[cfg(windows)]
fn move_file_windows(source: &Path, destination: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let moved = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            flags | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn open_guarded_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ,
    };

    let file = fs::OpenOptions::new()
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to replace a symbolic link or reparse point",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn rename_open_file(file: &fs::File, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    let destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    let file_name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let byte_len = file_name_offset + destination_wide.len() * std::mem::size_of::<u16>();
    let mut buffer = vec![0usize; byte_len.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();

    let renamed = unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = (destination_wide.len() * std::mem::size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            destination_wide.as_ptr(),
            (info.cast::<u8>().add(file_name_offset)).cast::<u16>(),
            destination_wide.len(),
        );
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileRenameInfo,
            info.cast(),
            byte_len as u32,
        )
    };
    if renamed == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn dispose_recovery_handle(file: &fs::File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfoEx, SetFileInformationByHandle, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO_EX,
    };

    test_fail_point("cleanup_recovery")?;
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    let disposed = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfoEx,
            (&raw const info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if disposed == 0 {
        return Err(std::io::Error::last_os_error())
            .context("Failed to remove completed auth recovery sidecar");
    }
    Ok(())
}

fn display_name(label: &str) -> &str {
    match label {
        "auth" | "rollback" => "Codex auth.json",
        "accounts" => "the account catalog",
        _ => "the file",
    }
}

#[cfg(test)]
fn test_fail_point(name: &str) -> Result<()> {
    let should_fail = std::env::var("CODEX_SWITCHER_TEST_ATOMIC_FAIL")
        .ok()
        .is_some_and(|configured| configured.split(',').any(|value| value.trim() == name));
    if should_fail {
        anyhow::bail!("Injected atomic file failure at {name}");
    }
    Ok(())
}

#[cfg(not(test))]
fn test_fail_point(_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn test_external_replacement(path: &Path) -> Result<()> {
    let Ok(contents) = std::env::var("CODEX_SWITCHER_TEST_REPLACE_AFTER_QUARANTINE") else {
        return Ok(());
    };
    fs::write(path, contents).context("Failed to inject external replacement after recovery rename")
}

#[cfg(not(test))]
fn test_external_replacement(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_recovery_does_not_restore_a_login() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("auth.json");
        let recovery = recovery_path(&path);
        fs::write(&recovery, b"previous").expect("write recovery");

        recover_externally_mutable_file(&path).expect("clean interrupted recovery");

        assert!(!path.exists());
        assert!(!recovery.exists());
    }

    #[test]
    fn live_file_wins_over_a_stale_recovery_sidecar() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("auth.json");
        let recovery = recovery_path(&path);
        fs::write(&path, b"live").expect("write live file");
        fs::write(&recovery, b"previous").expect("write recovery");

        recover_externally_mutable_file(&path).expect("recover file");

        assert!(fs::read(path).expect("read live file") == b"live");
    }

    #[test]
    fn cleanup_failure_after_publication_cannot_resurrect_auth_after_sign_out() {
        let _guard = crate::test_support::env_lock();
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("auth.json");
        fs::write(&path, b"previous").expect("write previous");
        std::env::set_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL", "cleanup_recovery");

        let result = stage_file_change(
            &path,
            FileSnapshot::present(b"previous".to_vec()),
            FileSnapshot::present(b"desired".to_vec()),
            "auth",
        )
        .expect("stage change")
        .commit();

        std::env::remove_var("CODEX_SWITCHER_TEST_ATOMIC_FAIL");
        assert!(result.is_ok());
        assert!(fs::read(&path).expect("read published file") == b"desired");
        fs::remove_file(&path).expect("simulate sign-out");
        recover_externally_mutable_file(&path).expect("clean stale recovery");
        assert!(!path.exists());
        assert!(!recovery_path(&path).exists());
    }

    #[cfg(windows)]
    #[test]
    fn writable_handle_blocks_publication_without_losing_its_write() {
        use std::io::{Seek, SeekFrom};

        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("auth.json");
        fs::write(&path, b"previous").expect("write previous");
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open external writer");
        let staged = stage_file_change(
            &path,
            FileSnapshot::present(b"previous".to_vec()),
            FileSnapshot::present(b"desired".to_vec()),
            "auth",
        )
        .expect("stage change");

        let result = staged.commit();
        assert!(result.is_err());

        writer.set_len(0).expect("truncate through existing handle");
        writer.seek(SeekFrom::Start(0)).expect("rewind writer");
        writer.write_all(b"external").expect("write externally");
        writer.sync_all().expect("sync external write");
        assert!(fs::read(path).expect("read external write") == b"external");
    }

    #[cfg(windows)]
    #[test]
    fn symbolic_link_auth_is_rejected_without_moving_its_target() {
        use std::os::windows::fs::symlink_file;

        let directory = tempfile::tempdir().expect("temp directory");
        let target = directory.path().join("target.json");
        let path = directory.path().join("auth.json");
        fs::write(&target, b"previous").expect("write target");
        if let Err(error) = symlink_file(&target, &path) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("create symbolic link: {error}");
        }

        let result = stage_file_change(
            &path,
            FileSnapshot::present(b"previous".to_vec()),
            FileSnapshot::present(b"desired".to_vec()),
            "auth",
        )
        .expect("stage change")
        .commit();

        assert!(result.is_err());
        assert!(fs::read(&target).expect("read target") == b"previous");
        assert!(fs::symlink_metadata(&path)
            .expect("read link metadata")
            .file_type()
            .is_symlink());
        assert!(!recovery_path(&path).exists());
    }
}

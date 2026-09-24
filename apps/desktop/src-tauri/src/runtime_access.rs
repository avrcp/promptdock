//! Windows file-share gate for the local Desktop runtime data directory.
//!
//! Runtime owners retain a compatible shared handle for their entire access
//! lifetime. The reset tool opens the same file with `FileShare.None`, so it
//! cannot race a GUI or Hook helper into deleting live state.

use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::error::AppError;

const GUARD_FILE_NAME: &str = ".runtime-access.guard";
const RESET_PENDING_FILE_NAME: &str = ".reset-pending";

/// Holds the compatible runtime access handle until the caller drops it.
#[derive(Debug)]
pub struct RuntimeAccessGuard {
    _file: File,
}

impl RuntimeAccessGuard {
    /// Acquires the product runtime side of the Windows share-mode protocol.
    ///
    /// The directory is created only for a real runtime startup. Reset preview
    /// deliberately does not call this API.
    pub fn acquire(dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|error| {
            AppError::new(
                "RUNTIME_ACCESS_UNAVAILABLE",
                format!("无法准备本地运行数据访问目录：{error}"),
            )
        })?;
        let path = dir.join(GUARD_FILE_NAME);
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            if !metadata.file_type().is_file() || is_reparse_point(&metadata) {
                return Err(AppError::new(
                    "RUNTIME_ACCESS_UNAVAILABLE",
                    "本地运行数据维护门闩不是普通文件",
                ));
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }

        let file = options.open(&path).map_err(|error| {
            AppError::new(
                "RUNTIME_ACCESS_UNAVAILABLE",
                format!("无法取得本地运行数据访问权限：{error}"),
            )
        })?;
        // Keep this check inside the compatible share handle. Reset writes the
        // marker only while holding FileShare.None, so it cannot slip between
        // a successful check and this runtime entering its data directory.
        let pending = dir.join(RESET_PENDING_FILE_NAME);
        match std::fs::symlink_metadata(&pending) {
            Ok(_) => {
                drop(file);
                return Err(AppError::new(
                    "RUNTIME_RESET_INCOMPLETE",
                    "本地运行数据清空尚未完成；请重新运行清空工具后再启动",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::store(error.to_string())),
        }
        Ok(Self { _file: file })
    }

    pub const fn file_name() -> &'static str {
        GUARD_FILE_NAME
    }

    pub const fn reset_pending_file_name() -> &'static str {
        RESET_PENDING_FILE_NAME
    }
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_guard_uses_a_fixed_file_under_the_requested_directory() {
        let directory = tempfile::tempdir().unwrap();
        let _guard = RuntimeAccessGuard::acquire(directory.path()).unwrap();
        assert!(directory.path().join(GUARD_FILE_NAME).is_file());
    }

    #[test]
    fn runtime_guard_refuses_to_start_while_a_reset_is_pending() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(RESET_PENDING_FILE_NAME), b"pending").unwrap();

        let error = RuntimeAccessGuard::acquire(directory.path()).unwrap_err();

        assert_eq!(error.code, "RUNTIME_RESET_INCOMPLETE");
    }
}

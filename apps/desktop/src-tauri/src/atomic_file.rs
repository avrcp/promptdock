use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::AppError;

pub(crate) fn write(
    path: &Path,
    bytes: &[u8],
    code: &'static str,
    message: &'static str,
) -> Result<(), AppError> {
    write_stream(path, code, message, |writer| {
        writer
            .write_all(bytes)
            .map_err(|error| AppError::internal(code, message, error.to_string()))?;
        Ok(())
    })
}

pub(crate) fn write_stream<T>(
    path: &Path,
    code: &'static str,
    message: &'static str,
    operation: impl FnOnce(&mut dyn Write) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let parent = path.parent().ok_or_else(|| AppError::new(code, message))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| AppError::internal(code, message, error.to_string()))?;
    let temp_path = sibling_path(path, &format!(".promptdock.tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let temp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|error| AppError::internal(code, message, error.to_string()))?;
        let mut writer = BufWriter::new(temp);
        let outcome = operation(&mut writer)?;
        writer
            .flush()
            .map_err(|error| AppError::internal(code, message, error.to_string()))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| AppError::internal(code, message, error.to_string()))?;
        drop(writer);
        replace(&temp_path, path)
            .and_then(|()| sync_parent(parent))
            .map_err(|error| AppError::internal(code, message, error.to_string()))?;
        Ok(outcome)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn sync_parent(_parent: &Path) -> std::io::Result<()> {
    // MoveFileExW with MOVEFILE_WRITE_THROUGH above flushes the rename metadata.
    Ok(())
}

#[cfg(not(windows))]
fn sync_parent(parent: &Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("value.json");
        std::fs::write(&path, b"old").expect("seed file");
        write(&path, b"new", "WRITE_FAILED", "write failed").expect("replace file");
        assert_eq!(std::fs::read(path).expect("read file"), b"new");
    }

    #[test]
    fn failed_stream_keeps_destination_and_removes_temporary_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("value.json");
        std::fs::write(&path, b"old").expect("seed file");

        let result = write_stream(&path, "WRITE_FAILED", "write failed", |writer| {
            writer.write_all(b"partial").unwrap();
            Err::<(), _>(AppError::new("SIMULATED_FAILURE", "simulated failure"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).expect("read file"), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

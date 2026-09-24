use fs2::FileExt;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_LOCK_TIMEOUT: Duration = Duration::from_millis(400);
const MAINTENANCE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const ERROR_LOG_MAX_BYTES: u64 = 1024 * 1024;
const ERROR_LOG_ARCHIVES: u8 = 3;

#[derive(Debug, Clone)]
pub(crate) struct InboxPaths {
    inbox: PathBuf,
    inbox_lock: PathBuf,
    errors: PathBuf,
    errors_lock: PathBuf,
    maintenance_journal: PathBuf,
}

impl InboxPaths {
    pub(crate) fn new(inbox: impl Into<PathBuf>) -> Self {
        let inbox = inbox.into();
        let inbox_lock = sibling_path(&inbox, ".lock");
        let errors = sibling_path(&inbox, ".errors.log");
        let errors_lock = sibling_path(&errors, ".lock");
        let maintenance_journal = sibling_path(&inbox, ".maintenance.json");
        Self {
            inbox,
            inbox_lock,
            errors,
            errors_lock,
            maintenance_journal,
        }
    }

    pub(crate) fn inbox(&self) -> &Path {
        &self.inbox
    }

    #[cfg(test)]
    pub(crate) fn errors(&self) -> &Path {
        &self.errors
    }

    pub(crate) fn maintenance_journal(&self) -> &Path {
        &self.maintenance_journal
    }

    pub(crate) fn is_owned_quarantine(&self, candidate: &Path) -> bool {
        if candidate.parent() != self.inbox.parent() {
            return false;
        }
        let Some(inbox_name) = self.inbox.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let Some(candidate_name) = candidate.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let Some(generation) = candidate_name.strip_prefix(&format!("{inbox_name}.purge-")) else {
            return false;
        };
        uuid::Uuid::parse_str(generation).is_ok()
    }

    pub(crate) fn lock_inbox(&self) -> io::Result<FileLock> {
        FileLock::acquire(&self.inbox_lock, Some(MAINTENANCE_LOCK_TIMEOUT))
    }

    fn lock_errors(&self) -> io::Result<FileLock> {
        FileLock::acquire(&self.errors_lock, Some(CAPTURE_LOCK_TIMEOUT))
    }
}

pub(crate) struct FileLock {
    file: File,
}

impl FileLock {
    fn acquire(path: &Path, timeout: Option<Duration>) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        lock_exclusive(&file, timeout)?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(%error, "failed to release inbox filesystem lock");
        }
    }
}

/// Appends one already-serialized batch while holding the capture lock once.
///
/// Callers that need a multi-record fact boundary must construct the complete
/// batch first so it is written and synced as one operation. This deliberately
/// imposes no record format; inbox adapters own their serialization contract.
pub(crate) fn append_batch(inbox_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let paths = InboxPaths::new(inbox_path);
    if let Some(parent) = inbox_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = FileLock::acquire(&paths.inbox_lock, Some(CAPTURE_LOCK_TIMEOUT))?;
    terminate_partial_tail(inbox_path)?;
    if inbox_path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0)
        .saturating_add(bytes.len() as u64)
        > 32 * 1024 * 1024
    {
        return Err(io::Error::other("inbox capacity reached"));
    }
    append_and_sync(inbox_path, bytes)
}

/// Prevents a helper that died mid-record from consuming the first subsequent
/// valid record. The existing incomplete suffix becomes one rejected record;
/// the next append starts a distinct JSONL record. This runs under the same
/// append lock as the write, so two helpers cannot insert a record between the
/// inspection and the boundary.
fn terminate_partial_tail(path: &Path) -> io::Result<()> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
        file.sync_data()?;
    }
    Ok(())
}

pub(crate) fn append_error(inbox_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let paths = InboxPaths::new(inbox_path);
    if let Some(parent) = paths.errors.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = paths.lock_errors()?;
    rotate_errors_if_needed(&paths, bytes.len() as u64)?;
    append_and_sync(&paths.errors, bytes)
}

fn append_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_data()
}

fn rotate_errors_if_needed(paths: &InboxPaths, incoming: u64) -> io::Result<()> {
    let current = paths
        .errors
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if current.saturating_add(incoming) <= ERROR_LOG_MAX_BYTES {
        return Ok(());
    }
    for index in (1..=ERROR_LOG_ARCHIVES).rev() {
        let source = if index == 1 {
            paths.errors.clone()
        } else {
            error_archive(paths, index - 1)
        };
        if !source.exists() {
            continue;
        }
        let destination = error_archive(paths, index);
        if destination.exists() {
            std::fs::remove_file(&destination)?;
        }
        std::fs::rename(source, destination)?;
    }
    Ok(())
}

fn error_archive(paths: &InboxPaths, index: u8) -> PathBuf {
    sibling_path(&paths.errors, &format!(".{index}"))
}

fn lock_exclusive(file: &File, timeout: Option<Duration>) -> io::Result<()> {
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        match FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error) if is_lock_contended(&error) => {
                let Some(deadline) = deadline else {
                    thread::sleep(LOCK_RETRY_INTERVAL);
                    continue;
                };
                let now = Instant::now();
                if now >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "inbox lock wait timed out",
                    ));
                }
                thread::sleep(LOCK_RETRY_INTERVAL.min(deadline - now));
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_lock_contended(error: &io::Error) -> bool {
    let expected = fs2::lock_contended_error();
    match (error.raw_os_error(), expected.raw_os_error()) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => error.kind() == expected.kind(),
    }
}

pub(crate) fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_error_log_rotates_to_three_archives() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("hook-inbox.jsonl");
        let paths = InboxPaths::new(&inbox);
        for generation in 0..5_u8 {
            std::fs::write(
                paths.errors(),
                vec![b'a' + generation; ERROR_LOG_MAX_BYTES as usize],
            )
            .unwrap();
            append_error(&inbox, b"next\n").unwrap();
        }

        assert!(paths.errors().metadata().unwrap().len() <= 5);
        for index in 1..=ERROR_LOG_ARCHIVES {
            assert!(error_archive(&paths, index).exists());
        }
        assert!(!error_archive(&paths, ERROR_LOG_ARCHIVES + 1).exists());
    }

    #[test]
    fn append_terminates_a_partial_tail_before_the_next_record() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("agent-events.jsonl");
        std::fs::write(&inbox, b"pdenc1:partial").unwrap();

        append_batch(&inbox, b"pdenc1:complete\n").unwrap();

        assert_eq!(
            std::fs::read(&inbox).unwrap(),
            b"pdenc1:partial\npdenc1:complete\n"
        );
    }
}

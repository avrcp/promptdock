use super::*;

pub(super) const COMPACTION_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONSUMED_SEGMENT_AGE: Duration = Duration::from_secs(60);

pub(super) enum RuntimeCommand {
    Wake,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MaintenanceJournal {
    generation: String,
    quarantine: PathBuf,
    new_file_identity: Option<u128>,
    old_file_identity: Option<u128>,
    old_eof_offset: u64,
}

struct RotatedSegment {
    journal: MaintenanceJournal,
    cursor: InboxCursor,
}

pub(super) fn compact_at_eof_for_source(
    db: &Db,
    paths: &InboxPaths,
    cursor: &mut InboxCursor,
    threshold: u64,
    source: &SourceDescriptor,
) -> Result<(), AppError> {
    compact_at_eof_at(
        db,
        paths,
        cursor,
        threshold,
        source,
        std::time::SystemTime::now(),
    )
}

fn compact_at_eof_at(
    db: &Db,
    paths: &InboxPaths,
    cursor: &mut InboxCursor,
    threshold: u64,
    source: &SourceDescriptor,
    now: std::time::SystemTime,
) -> Result<(), AppError> {
    let Some(rotated) = rotate_segment(paths, cursor, threshold, now)? else {
        return Ok(());
    };
    *cursor = rotated.cursor.clone();
    let (status, last_error_code) = db
        .source_health(source)?
        .map(|value| (value.status, value.last_error_code))
        .unwrap_or_else(|| ("configured".into(), None));
    let checkpoint =
        checkpoint_for_status_for_source(&rotated.cursor, &status, last_error_code, source)?;
    db.checkpoint(&checkpoint)?;
    if let Err(error) = finish_maintenance(paths, &rotated.journal) {
        tracing::warn!(%error, "compaction committed; deferred inbox cleanup to recovery");
    }
    Ok(())
}

fn rotate_segment(
    paths: &InboxPaths,
    cursor: &InboxCursor,
    threshold: u64,
    now: std::time::SystemTime,
) -> Result<Option<RotatedSegment>, AppError> {
    let _lock = paths
        .lock_inbox()
        .map_err(|error| maintenance_error("INBOX_LOCK_FAILED", error))?;
    if let Some(parent) = paths.inbox().parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.inbox())
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    let identity = file_identity(&file, &metadata);
    if metadata.len() != cursor.offset
        || matches!((cursor.file_identity, identity), (Some(expected), Some(actual)) if expected != actual)
    {
        return Err(AppError::new(
            "INBOX_NOT_AT_EOF",
            "采集文件在维护开始前发生了变化，请重试",
        ));
    }
    // Even low-volume spools must release already-checkpointed encrypted body
    // copies. Creation time is fixed for this segment, unlike last-write time
    // which ongoing small appends could keep refreshing indefinitely.
    let aged = metadata
        .created()
        .or_else(|_| metadata.modified())
        .ok()
        .and_then(|created| now.duration_since(created).ok())
        .is_some_and(|age| age >= MAX_CONSUMED_SEGMENT_AGE);
    if metadata.len() == 0 || (metadata.len() <= threshold && !aged) {
        return Ok(None);
    }
    drop(file);

    let generation = uuid::Uuid::new_v4().to_string();
    let mut journal = MaintenanceJournal {
        generation: generation.clone(),
        quarantine: crate::inbox::sibling_path(paths.inbox(), &format!(".purge-{generation}")),
        new_file_identity: None,
        old_file_identity: identity,
        old_eof_offset: cursor.offset,
    };
    write_journal(paths, &journal)?;
    validate_journal(paths, &journal)?;
    std::fs::rename(paths.inbox(), &journal.quarantine)
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    create_new_inbox(paths)?;
    journal.new_file_identity = current_file_identity(paths.inbox())?;
    write_journal(paths, &journal)?;
    Ok(Some(RotatedSegment {
        cursor: InboxCursor {
            generation: Some(generation),
            offset: 0,
            file_identity: journal.new_file_identity,
        },
        journal,
    }))
}

pub(super) fn recover_maintenance_for_source(
    db: &Db,
    paths: &InboxPaths,
    source: &SourceDescriptor,
) -> Result<(), AppError> {
    let journal = {
        let _lock = paths
            .lock_inbox()
            .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
        resume_rotation_locked(paths)?
    };
    let Some(journal) = journal else {
        return Ok(());
    };
    let cursor = InboxCursor {
        generation: Some(journal.generation.clone()),
        offset: 0,
        file_identity: journal.new_file_identity,
    };
    if load_cursor_for_source(db, source)?.generation.as_deref()
        != Some(journal.generation.as_str())
    {
        let (status, last_error_code) = db
            .source_health(source)?
            .map(|value| (value.status, value.last_error_code))
            .unwrap_or_else(|| ("configured".into(), None));
        db.checkpoint(&checkpoint_for_status_for_source(
            &cursor,
            &status,
            last_error_code,
            source,
        )?)?;
    }
    finish_maintenance(paths, &journal)
}

fn resume_rotation_locked(paths: &InboxPaths) -> Result<Option<MaintenanceJournal>, AppError> {
    let Some(mut journal) = read_journal(paths)? else {
        return Ok(None);
    };
    validate_journal(paths, &journal)?;
    if journal.new_file_identity.is_none() {
        if !journal.quarantine.exists() && paths.inbox().exists() {
            let file = File::open(paths.inbox())
                .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
            let metadata = file
                .metadata()
                .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
            let identity = file_identity(&file, &metadata);
            if metadata.len() < journal.old_eof_offset
                || matches!((journal.old_file_identity, identity), (Some(expected), Some(actual)) if expected != actual)
            {
                return Err(AppError::new(
                    "INBOX_MAINTENANCE_RECOVERY_CONFLICT",
                    "采集文件维护前的原始文件身份或偏移发生变化；已保留文件等待人工处理",
                ));
            }
            if metadata.len() > journal.old_eof_offset {
                // The process died after journaling but before rename. A helper
                // appended a valid suffix after the durable checkpoint. Keep
                // the original segment and let the normal consumer advance its
                // old cursor before a later compaction attempt.
                std::fs::remove_file(paths.maintenance_journal())
                    .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
                tracing::warn!(
                    old_eof_offset = journal.old_eof_offset,
                    observed_len = metadata.len(),
                    "abandoned incomplete inbox rotation to preserve appended records"
                );
                return Ok(None);
            }
            std::fs::rename(paths.inbox(), &journal.quarantine)
                .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
        }
        if !paths.inbox().exists() {
            create_new_inbox(paths)?;
        }
        journal.new_file_identity = current_file_identity(paths.inbox())?;
        write_journal(paths, &journal)?;
    } else if !paths.inbox().exists() {
        create_new_inbox(paths)?;
        journal.new_file_identity = current_file_identity(paths.inbox())?;
        write_journal(paths, &journal)?;
    } else if current_file_identity(paths.inbox())? != journal.new_file_identity {
        if !journal.quarantine.exists() {
            std::fs::rename(paths.inbox(), &journal.quarantine)
                .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
            create_new_inbox(paths)?;
        }
        journal.new_file_identity = current_file_identity(paths.inbox())?;
        write_journal(paths, &journal)?;
    }
    Ok(Some(journal))
}

fn create_new_inbox(paths: &InboxPaths) -> Result<(), AppError> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(paths.inbox())
        .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
    file.sync_all()
        .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))
}

fn current_file_identity(path: &Path) -> Result<Option<u128>, AppError> {
    let file =
        File::open(path).map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| maintenance_error("INBOX_RECOVERY_FAILED", error))?;
    Ok(file_identity(&file, &metadata))
}

fn finish_maintenance(paths: &InboxPaths, expected: &MaintenanceJournal) -> Result<(), AppError> {
    let _lock = paths
        .lock_inbox()
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    let Some(current) = read_journal(paths)? else {
        return Ok(());
    };
    if current.generation != expected.generation {
        return Err(AppError::new(
            "INBOX_GENERATION_CONFLICT",
            "采集文件维护代次发生冲突",
        ));
    }
    validate_journal(paths, &current)?;
    if current.quarantine.exists() {
        ensure_regular_non_reparse(&current.quarantine)?;
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&current.quarantine)
            .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
        file.sync_all()
            .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
        drop(file);
        std::fs::remove_file(&current.quarantine)
            .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    }
    std::fs::remove_file(paths.maintenance_journal())
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))
}

fn write_journal(paths: &InboxPaths, journal: &MaintenanceJournal) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(journal).map_err(|error| {
        AppError::internal(
            "INBOX_MAINTENANCE_FAILED",
            "采集文件维护日志写入失败",
            error.to_string(),
        )
    })?;
    crate::atomic_file::write(
        paths.maintenance_journal(),
        &bytes,
        "INBOX_MAINTENANCE_FAILED",
        "采集文件维护日志写入失败",
    )
}

fn read_journal(paths: &InboxPaths) -> Result<Option<MaintenanceJournal>, AppError> {
    let bytes = match std::fs::read(paths.maintenance_journal()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(maintenance_error("INBOX_RECOVERY_FAILED", error)),
    };
    let journal = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::internal(
            "INBOX_RECOVERY_FAILED",
            "采集文件维护日志损坏",
            error.to_string(),
        )
    })?;
    validate_journal(paths, &journal)?;
    Ok(Some(journal))
}

fn validate_journal(paths: &InboxPaths, journal: &MaintenanceJournal) -> Result<(), AppError> {
    let expected_quarantine =
        crate::inbox::sibling_path(paths.inbox(), &format!(".purge-{}", journal.generation));
    if uuid::Uuid::parse_str(&journal.generation).is_err()
        || !paths.is_owned_quarantine(&journal.quarantine)
        || journal.quarantine != expected_quarantine
    {
        return Err(AppError::new(
            "INBOX_MAINTENANCE_JOURNAL_INVALID",
            "采集文件维护日志包含无效的隔离文件目标",
        ));
    }
    let Some(parent) = paths.inbox().parent() else {
        return Err(AppError::new(
            "INBOX_MAINTENANCE_JOURNAL_INVALID",
            "采集文件维护目录无效",
        ));
    };
    ensure_directory_non_reparse(parent)
}

fn ensure_directory_non_reparse(path: &Path) -> Result<(), AppError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(AppError::new(
            "INBOX_MAINTENANCE_JOURNAL_INVALID",
            "采集文件维护目录不是普通目录",
        ));
    }
    Ok(())
}

fn ensure_regular_non_reparse(path: &Path) -> Result<(), AppError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| maintenance_error("INBOX_MAINTENANCE_FAILED", error))?;
    if !metadata.file_type().is_file() || is_reparse_point(&metadata) {
        return Err(AppError::new(
            "INBOX_MAINTENANCE_JOURNAL_INVALID",
            "采集文件维护隔离目标不是普通文件",
        ));
    }
    Ok(())
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

    #[cfg(windows)]
    #[test]
    fn production_source_loop_compacts_a_small_aged_checkpointed_spool() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::FILETIME, Storage::FileSystem::SetFileTime};
        struct NoEvents;
        impl RuntimeEventEmitter for NoEvents {
            fn capture_health_changed(&self, _: &SourceCheckpointUpdate) {}
        }
        struct NoWork;
        impl AgentEventProcessor for NoWork {
            fn process(
                &self,
                _: AgentEventEnvelopeV2,
                _: &SourceCheckpointUpdate,
            ) -> Result<AgentEventProcessingOutcome, AppError> {
                panic!("checkpointed prefix may not be replayed")
            }
            fn checkpoint_rejection(
                &self,
                _: &'static str,
                _: &SourceCheckpointUpdate,
            ) -> Result<(), AppError> {
                panic!("checkpointed prefix may not be replayed")
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let paths = InboxPaths::new(directory.path().join("agent-events.jsonl"));
        let db = Arc::new(Db::open_in_memory().unwrap());
        let source = crate::adapters::codex::hooks::source_descriptor();
        crate::inbox::append_batch(paths.inbox(), b"checkpointed protected content\n").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.inbox())
            .unwrap();
        let age = std::time::SystemTime::now() - MAX_CONSUMED_SEGMENT_AGE - Duration::from_secs(1);
        let ticks = (age.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 11_644_473_600)
            * 10_000_000;
        let created = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        assert_ne!(
            unsafe {
                SetFileTime(
                    file.as_raw_handle(),
                    &created,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let metadata = file.metadata().unwrap();
        let cursor = InboxCursor {
            generation: None,
            offset: metadata.len(),
            file_identity: file_identity(&file, &metadata),
        };
        drop(file);
        db.checkpoint(&checkpoint_for_status_for_source(&cursor, "active", None, &source).unwrap())
            .unwrap();
        let (done, received) = mpsc::channel();
        let runtime = DurableEventRuntime::start_with_emitter_and_caught_up(
            db,
            Arc::new(NoWork),
            Arc::new(NoEvents),
            paths.inbox().to_owned(),
            source,
            Arc::new(move || {
                let _ = done.send(());
            }),
        );
        let caught_up = received.recv_timeout(Duration::from_secs(2));
        let report = runtime.shutdown();
        assert!(caught_up.is_ok());
        assert!(report.completed_cleanly());
        assert_eq!(std::fs::metadata(paths.inbox()).unwrap().len(), 0);
        assert!(!paths.maintenance_journal().exists());
    }

    #[test]
    fn a_small_checkpointed_segment_is_collected_by_age_and_next_append_survives() {
        let directory = tempfile::tempdir().unwrap();
        let paths = InboxPaths::new(directory.path().join("agent-events.jsonl"));
        let db = Db::open_in_memory().unwrap();
        let source = crate::adapters::codex::hooks::source_descriptor();
        crate::inbox::append_batch(paths.inbox(), b"checkpointed protected content\n").unwrap();
        let file = File::open(paths.inbox()).unwrap();
        let metadata = file.metadata().unwrap();
        let mut cursor = InboxCursor {
            generation: None,
            offset: metadata.len(),
            file_identity: file_identity(&file, &metadata),
        };
        let created = metadata
            .created()
            .unwrap_or_else(|_| metadata.modified().unwrap());
        drop(file);
        db.checkpoint(&checkpoint_for_status_for_source(&cursor, "active", None, &source).unwrap())
            .unwrap();
        compact_at_eof_at(
            &db,
            &paths,
            &mut cursor,
            COMPACTION_THRESHOLD_BYTES,
            &source,
            created + MAX_CONSUMED_SEGMENT_AGE,
        )
        .unwrap();
        assert_eq!(std::fs::metadata(paths.inbox()).unwrap().len(), 0);
        assert!(!paths.maintenance_journal().exists());
        assert_eq!(load_cursor_for_source(&db, &source).unwrap().offset, 0);
        crate::inbox::append_batch(paths.inbox(), b"new protected content\n").unwrap();
        assert_eq!(
            std::fs::read(paths.inbox()).unwrap(),
            b"new protected content\n"
        );
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".purge-"))
                .count(),
            0
        );
    }

    #[test]
    fn journal_rejects_a_quarantine_path_outside_the_inbox_directory() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let paths = InboxPaths::new(directory.path().join("agent-events.jsonl"));
        std::fs::write(directory.path().join("agent-events.jsonl"), b"").unwrap();
        let journal = MaintenanceJournal {
            generation: uuid::Uuid::new_v4().to_string(),
            quarantine: outside.path().join("unrelated.db"),
            new_file_identity: None,
            old_file_identity: None,
            old_eof_offset: 0,
        };
        std::fs::write(
            paths.maintenance_journal(),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let error = read_journal(&paths).unwrap_err();

        assert_eq!(error.code, "INBOX_MAINTENANCE_JOURNAL_INVALID");
    }

    #[test]
    fn recovery_preserves_a_helper_append_after_journal_before_rename() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("agent-events.jsonl");
        std::fs::write(&inbox, b"checkpointed\n").unwrap();
        let paths = InboxPaths::new(&inbox);
        let old_eof_offset = std::fs::metadata(&inbox).unwrap().len();
        let generation = uuid::Uuid::new_v4().to_string();
        let journal = MaintenanceJournal {
            quarantine: crate::inbox::sibling_path(&inbox, &format!(".purge-{generation}")),
            generation,
            new_file_identity: None,
            old_file_identity: current_file_identity(&inbox).unwrap(),
            old_eof_offset,
        };
        write_journal(&paths, &journal).unwrap();
        crate::inbox::append_batch(&inbox, b"arrived-after-journal\n").unwrap();

        let recovered = resume_rotation_locked(&paths).unwrap();

        assert!(recovered.is_none());
        assert_eq!(
            std::fs::read(&inbox).unwrap(),
            b"checkpointed\narrived-after-journal\n"
        );
        assert!(!paths.maintenance_journal().exists());
        assert!(!journal.quarantine.exists());
    }
}
